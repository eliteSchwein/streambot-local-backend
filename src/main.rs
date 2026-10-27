mod internal;
mod cache;
pub mod abstracts;
pub mod config;
mod server;
mod twitch;
mod category_library;
mod integrations;
mod system;

use crate::{config::Config, internal::{log, state}};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

// Used by the existing REST and WebSocket status endpoints.
pub fn status() -> serde_json::Value { state::status() }

#[tokio::main]
async fn main() -> crate::abstracts::cache::CacheResult<()> {
    log::success(&format!("Starting Streamding Backend V{}...", VERSION));
    let config = Config::load()?;
    initialize(&config).await
}

async fn initialize(config: &Config) -> crate::abstracts::cache::CacheResult<()> {
    state::reset();
    state::set_stage("loading_cache");
    cache::init(config).await?;

    state::set_stage("loading_config");
    // Keep the watcher alive until all services have stopped.
    let _configuration = config::init().await?;

    state::set_stage("loading_category_library");
    category_library::init().map_err(std::io::Error::other)?;

    state::set_stage("loading_web_content");
    let mut servers = server::init(config)?;

    state::set_stage("loading_system_components");
    let system_updater = system::init().await?;

    state::set_stage("starting_twitch_bot");
    let mut twitch = match twitch::init(config) {
        Ok(handle) => handle,
        Err(error) => {
            servers.shutdown().await?;
            return Err(error.into());
        }
    };

    state::set_stage("loading_integrations");
    let mut integrations = integrations::init(config).map_err(std::io::Error::other)?;

    state::set_stage("ready");
    state::set_ready(true);
    log::success("Backend initialized");

    tokio::select! {
        signal = tokio::signal::ctrl_c() => {
            signal?;
            log::notice("Shutdown requested");
        }
        exited = servers.wait_for_exit() => {
            match exited {
                Some((name, Ok(()))) => log::warn(&format!("{name} server stopped unexpectedly")),
                Some((name, Err(error))) => log::error(&format!("{name} server failed: {error}")),
                None => log::warn("Server threads stopped"),
            }
        }
        exited = twitch.wait_for_exit() => {
            log::warn(&format!("Twitch worker stopped unexpectedly: {exited:?}"));
        }
    }

    state::set_ready(false);
    state::set_stage("stopping");
    // Stop workers before dropping config/cache resources.
    system_updater.abort();
    integrations.shutdown().await?;
    twitch.shutdown().await?;
    servers.shutdown().await?;
    state::set_stage("stopped");
    Ok(())
}
