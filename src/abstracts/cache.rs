use async_trait::async_trait;
use serde_json::Value;
use std::{error::Error, sync::Arc};

pub type CacheResult<T> = Result<T, Box<dyn Error + Send + Sync>>;
pub type SharedCache = Arc<dyn Cache>;

#[async_trait]
pub trait Cache: Send + Sync {
    async fn get(&self, key: &str) -> CacheResult<Option<Value>>;
    async fn set(&self, key: &str, value: Value) -> CacheResult<()>;
    async fn delete(&self, key: &str) -> CacheResult<bool>;

    async fn exists(&self, key: &str) -> CacheResult<bool> {
        Ok(self.get(key).await?.is_some())
    }
}
