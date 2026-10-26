use async_trait::async_trait;
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};
use tokio::{fs, sync::Mutex};
use crate::abstracts::cache::{Cache, CacheResult};

pub const FILE_CACHE_NAME: &str = "streamding-cache.json";

/// The default cache file in the current operating system's temporary directory.
pub fn file_cache_path() -> PathBuf {
    std::env::temp_dir().join(FILE_CACHE_NAME)
}

pub struct FileCache {
    path: PathBuf,
    values: Mutex<Map<String, Value>>,
}

impl FileCache {
    pub async fn new() -> CacheResult<Self> {
        Self::open(file_cache_path()).await
    }

    pub async fn open(path: impl AsRef<Path>) -> CacheResult<Self> {
        let path = path.as_ref().to_owned();
        let values = match fs::read_to_string(&path).await {
            Ok(json) => serde_json::from_str::<Map<String, Value>>(&json)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Map::new(),
            Err(error) => return Err(error.into()),
        };
        Ok(Self { path, values: Mutex::new(values) })
    }

    async fn persist(&self, values: &Map<String, Value>) -> CacheResult<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).await?;
        }
        let temp = self.path.with_extension(format!("{}.tmp", std::process::id()));
        fs::write(&temp, serde_json::to_vec_pretty(values)?).await?;
        fs::rename(&temp, &self.path).await?;
        Ok(())
    }
}

#[async_trait]
impl Cache for FileCache {
    async fn get(&self, key: &str) -> CacheResult<Option<Value>> {
        Ok(self.values.lock().await.get(key).cloned())
    }

    async fn set(&self, key: &str, value: Value) -> CacheResult<()> {
        let mut values = self.values.lock().await;
        let mut updated = values.clone();
        updated.insert(key.to_owned(), value);
        self.persist(&updated).await?;
        *values = updated;
        Ok(())
    }

    async fn delete(&self, key: &str) -> CacheResult<bool> {
        let mut values = self.values.lock().await;
        if !values.contains_key(key) {
            return Ok(false);
        }
        let mut updated = values.clone();
        updated.remove(key);
        self.persist(&updated).await?;
        *values = updated;
        Ok(true)
    }
}