mod internal;
mod cache;
pub mod abstracts;
pub mod config;

use std::sync::RwLock;
use crate::internal::log;
use crate::abstracts::cache::{CacheResult, SharedCache};
use crate::config::{CacheBackend, Config};
use crate::cache::{FileCache, RedisCache, set_cache};
use std::sync::Arc;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub static STARTUP_STAGE: RwLock<&str> = RwLock::new("unknown");
pub static STARTUP_MESSAGES: RwLock<&str> = RwLock::new("Backend Loading");
pub static BACKEND_READY: RwLock<bool> = RwLock::new(false);
pub static IS_SAVING: RwLock<bool> = RwLock::new(false);

#[tokio::main]
async fn main() -> CacheResult<()> {
    log::success(&format!("Starting Streamding Backend V{}...", VERSION));
    initialize(false);

    let config = Config::load()?;
    let cache = init_cache(&config).await?;
    set_cache(cache).map_err(std::io::Error::other)?;
    log::success("Cache initialized");

    let configs = crate::config::file::init_config_files();
    let _settings_watcher = crate::config::watcher::watch_settings(configs.clone())?;
    let count = configs.read_all_configs().await?;
    log::success(&format!("Loaded {count} configuration files"));

    // Keep the watcher and backend alive until the service lifecycle is implemented.
    std::future::pending::<()>().await;
    Ok(())
}

pub fn initialize(reload: bool) {
    if reload {
        // TODO: reload/shutdown stuff in here
    }
    *IS_SAVING.write().unwrap() = false;
    *BACKEND_READY.write().unwrap() = false;
    *STARTUP_STAGE.write().unwrap() = "unknown";
    *STARTUP_MESSAGES.write().unwrap() = "Backend Loading";
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
