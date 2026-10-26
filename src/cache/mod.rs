pub mod file;
pub mod redis;

pub use file::FileCache;
pub use redis::RedisCache;

use std::sync::OnceLock;
use crate::abstracts::cache::SharedCache;

static CACHE: OnceLock<SharedCache> = OnceLock::new();

/// Register the selected cache backend once during startup.
pub fn set_cache(cache: SharedCache) -> Result<(), &'static str> {
    CACHE.set(cache).map_err(|_| "Cache already initialized")
}

/// Get the cache, returning None if startup has not initialized it yet.
pub fn try_get_cache() -> Option<SharedCache> {
    CACHE.get().cloned()
}

/// Get the active Redis or file cache from anywhere in the backend.
/// Panics only if called before cache initialization.
pub fn get_cache() -> SharedCache {
    try_get_cache().expect("Cache not initialized")
}

/// Initialize and register the selected cache backend, with Redis -> file fallback.
pub async fn init(config: &crate::config::Config) -> crate::abstracts::cache::CacheResult<()> {
    use crate::{config::CacheBackend, internal::log};
    let selected: SharedCache = match config.cache.backend {
        CacheBackend::Redis => {
            log::info("Cache backend: Redis");
            match RedisCache::connect(&config.cache.redis_url, &config.cache.redis_prefix).await {
                Ok(redis) => std::sync::Arc::new(redis),
                Err(error) => {
                    log::warn(&format!("Redis unavailable: {error}"));
                    log::warn(&format!("Falling back to file cache: {}", config.cache.file_path.display()));
                    std::sync::Arc::new(FileCache::open(&config.cache.file_path).await?)
                }
            }
        }
        CacheBackend::File => {
            log::info("Cache backend: File");
            std::sync::Arc::new(FileCache::open(&config.cache.file_path).await?)
        }
    };
    set_cache(selected).map_err(std::io::Error::other)?;
    log::success("Cache initialized");
    Ok(())
}
