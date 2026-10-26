use async_trait::async_trait;
use redis::{aio::ConnectionManager, AsyncCommands};
use serde_json::Value;
use crate::abstracts::cache::{Cache, CacheResult};

pub struct RedisCache {
    connection: ConnectionManager,
    prefix: String,
}

impl RedisCache {
    /// Connects to Redis. Uses the local server and streamding: prefix by default.
    pub async fn new() -> CacheResult<Self> {
        Self::connect("redis://127.0.0.1:6379/", "streamding:").await
    }

    pub async fn connect(url: &str, prefix: &str) -> CacheResult<Self> {
        let client = redis::Client::open(url)?;
        let connection = ConnectionManager::new(client).await?;
        Ok(Self { connection, prefix: prefix.into() })
    }

    fn key(&self, key: &str) -> String {
        format!("{}{}", self.prefix, key)
    }
}

#[async_trait]
impl Cache for RedisCache {
    async fn get(&self, key: &str) -> CacheResult<Option<Value>> {
        let mut connection = self.connection.clone();
        let raw: Option<String> = connection.get(self.key(key)).await?;
        raw.map(|json| serde_json::from_str::<Value>(&json).map_err(Into::into))
            .transpose()
    }

    async fn set(&self, key: &str, value: Value) -> CacheResult<()> {
        let mut connection = self.connection.clone();
        let json = serde_json::to_string(&value)?;
        let _: () = connection.set(self.key(key), json).await?;
        Ok(())
    }

    async fn delete(&self, key: &str) -> CacheResult<bool> {
        let mut connection = self.connection.clone();
        let removed: usize = connection.del(self.key(key)).await?;
        Ok(removed > 0)
    }
}
