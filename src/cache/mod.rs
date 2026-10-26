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
