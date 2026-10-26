mod internal;
mod cache;
pub mod abstracts;
pub mod config;
mod server;

use std::sync::RwLock;
use crate::internal::log;
use crate::abstracts::cache::{CacheResult, SharedCache};
use crate::config::{CacheBackend, Config};
use crate::cache::{FileCache, RedisCache, set_cache};
use std::{sync::Arc, thread};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub static STARTUP_STAGE: RwLock<&str> = RwLock::new("unknown");
pub static BACKEND_READY: RwLock<bool> = RwLock::new(false);
pub static IS_SAVING: RwLock<bool> = RwLock::new(false);

#[tokio::main]
async fn main() -> CacheResult<()> {
    log::success(&format!("Starting Streamding Backend V{}...", VERSION));

    // Optional root .env: omitted settings retain their defaults.
    let config = Config::load()?;
    initialize(false, &config).await
}

/// Starts the backend infrastructure and keeps it alive until shutdown.
/// The `reload` flag is reserved for the future reload lifecycle.
pub async fn initialize(reload: bool, config: &Config) -> CacheResult<()> {
    if reload {
        // TODO: stop/reinitialize services when reload support is implemented.
        log::notice("Backend reinitialization requested");
    }

    *IS_SAVING.write().unwrap() = false;
    *BACKEND_READY.write().unwrap() = false;
    *STARTUP_STAGE.write().unwrap() = "loading_cache";

    // Cache must be ready before the config-file manager populates it.
    let cache = init_cache(config).await?;
    set_cache(cache).map_err(std::io::Error::other)?;
    log::success("Cache initialized");

    let configs = crate::config::file::init_config_files();
    let _settings_watcher = crate::config::watcher::watch_settings(configs.clone())?;
    let count = configs.read_all_configs().await?;
    log::success(&format!("Loaded {count} configuration files"));

    *STARTUP_STAGE.write().unwrap() = "loading_web_content";

    // Dedicated OS thread + Tokio runtime for each server.
    let (server_exit_tx, mut server_exit_rx) =
        tokio::sync::mpsc::unbounded_channel::<(&'static str, std::io::Result<()>)>();
    let (rest_shutdown_tx, rest_shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let (ws_shutdown_tx, ws_shutdown_rx) = tokio::sync::oneshot::channel::<()>();

    let rest_config = config.server.clone();
    let rest_exits = server_exit_tx.clone();
    let rest_thread = thread::Builder::new()
        .name("streambot-rest".into())
        .spawn(move || {
            let result = run_server_thread(server::rest::run(rest_config), rest_shutdown_rx);
            let _ = rest_exits.send(("REST", result));
        })?;

    let ws_config = config.server.clone();
    let ws_thread = match thread::Builder::new()
        .name("streambot-websocket".into())
        .spawn(move || {
            let result = run_server_thread(server::websocket::run(ws_config), ws_shutdown_rx);
            let _ = server_exit_tx.send(("WebSocket", result));
        }) {
        Ok(thread) => thread,
        Err(error) => {
            let _ = rest_shutdown_tx.send(());
            let _ = tokio::task::spawn_blocking(move || rest_thread.join()).await;
            return Err(error.into());
        }
    };

    *STARTUP_STAGE.write().unwrap() = "ready";
    *BACKEND_READY.write().unwrap() = true;
    log::success("Backend initialized");

    tokio::select! {
        signal = tokio::signal::ctrl_c() => {
            signal?;
            log::notice("Shutdown requested");
        }
        exited = server_exit_rx.recv() => {
            match exited {
                Some((name, Ok(()))) => log::warn(&format!("{name} server stopped unexpectedly")),
                Some((name, Err(error))) => log::error(&format!("{name} server failed: {error}")),
                None => log::warn("Server threads stopped"),
            }
        }
    }

    *BACKEND_READY.write().unwrap() = false;
    *STARTUP_STAGE.write().unwrap() = "stopping";
    let _ = rest_shutdown_tx.send(());
    let _ = ws_shutdown_tx.send(());
    tokio::task::spawn_blocking(move || {
        let _ = rest_thread.join();
        let _ = ws_thread.join();
    }).await?;

    *STARTUP_STAGE.write().unwrap() = "stopped";
    Ok(())
}

async fn init_cache(config: &Config) -> CacheResult<SharedCache> {
    match config.cache.backend {
        CacheBackend::Redis => {
            log::info("Cache backend: Redis");
            match RedisCache::connect(&config.cache.redis_url, &config.cache.redis_prefix).await {
                Ok(redis) => Ok(Arc::new(redis)),
                Err(error) => {
                    log::warn(&format!("Redis unavailable: {error}"));
                    log::warn(&format!(
                        "Falling back to file cache: {}",
                        config.cache.file_path.display()
                    ));
                    let file = FileCache::open(&config.cache.file_path).await?;
                    log::success("File cache initialized (Redis fallback)");
                    Ok(Arc::new(file))
                }
            }
        }
        CacheBackend::File => {
            log::info("Cache backend: File");
            Ok(Arc::new(FileCache::open(&config.cache.file_path).await?))
        }
    }
}

// Shared status payload for REST and WebSocket servers.
pub fn status() -> serde_json::Value {
    serde_json::json!({
        "version": VERSION,
        "bootup_stage": *STARTUP_STAGE.read().unwrap(),
        "ready": *BACKEND_READY.read().unwrap(),
    })
}

// Run one long-lived server future on its own thread/runtime.
fn run_server_thread<F>(server: F, mut shutdown: tokio::sync::oneshot::Receiver<()>) -> std::io::Result<()>
where
    F: std::future::Future<Output = std::io::Result<()>>,
{
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        tokio::select! {
            result = server => result,
            _ = &mut shutdown => Ok(()),
        }
    })
}
