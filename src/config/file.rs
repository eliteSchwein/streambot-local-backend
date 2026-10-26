use crate::abstracts::cache::CacheResult;
use crate::cache::get_cache;
use serde_json::Value;
use std::{ffi::OsStr, path::{Component, Path, PathBuf}, sync::{Arc, OnceLock, atomic::{AtomicU64, Ordering}}};
use tokio::{fs, sync::Mutex};

pub const SKIPPED_DIRECTORIES: &[&str] = &["assets", "piper", "compressed_assets", "streambot-customization"];
static MANAGER: OnceLock<Arc<ConfigFiles>> = OnceLock::new();
static TMP_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Clone)]
pub struct ConfigFiles {
    root: PathBuf,
    // One writer at a time; avoids lost updates and partial read/modify/write races.
    lock: Arc<Mutex<()>>,
}

pub fn config_root() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config/streambot")
}

pub fn init_config_files() -> Arc<ConfigFiles> {
    MANAGER.get_or_init(|| Arc::new(ConfigFiles::new(config_root()))).clone()
}

pub fn get_config_files() -> Arc<ConfigFiles> {
    init_config_files()
}

fn invalid(message: impl Into<String>) -> Box<dyn std::error::Error + Send + Sync> {
    std::io::Error::new(std::io::ErrorKind::InvalidInput, message.into()).into()
}

fn meaningful(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::String(s) => !s.trim().is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
        _ => true,
    }
}

fn merge(base: &mut Value, changes: Value) {
    if let (Value::Object(existing), Value::Object(patch)) = (&mut *base, &changes) {
        for (key, value) in patch {
            if let Some(old) = existing.get_mut(key) {
                merge(old, value.clone());
            } else {
                existing.insert(key.clone(), value.clone());
            }
        }
    } else {
        *base = changes;
    }
}

impl ConfigFiles {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into(), lock: Arc::new(Mutex::new(())) }
    }

    pub fn root(&self) -> &Path { &self.root }

    fn resolve(&self, input: impl AsRef<Path>) -> CacheResult<PathBuf> {
        let input = input.as_ref();
        if input.is_absolute() { return Err(invalid("absolute paths are not allowed; use a path relative to ~/.config/streambot")); }
        for component in input.components() {
            if !matches!(component, Component::Normal(_)) {
                return Err(invalid("invalid config path (expected only normal path components)"));
            }
        }
        let first = input.components().next().and_then(|c| match c { Component::Normal(n) => n.to_str(), _ => None });
        if first.is_some_and(|n| SKIPPED_DIRECTORIES.contains(&n)) {
            return Err(invalid("directory is excluded from config management"));
        }
        Ok(self.root.join(input))
    }

    pub fn cache_key(&self, relative: impl AsRef<Path>) -> CacheResult<String> {
        let relative = relative.as_ref();
        self.resolve(relative)?;
        let name = relative.file_stem().and_then(OsStr::to_str).ok_or_else(|| invalid("invalid filename"))?;
        let name = name.strip_prefix("streambot-").unwrap_or(name);
        if name.is_empty() { return Err(invalid("empty config name")); }
        let folder = relative.parent().filter(|p| !p.as_os_str().is_empty())
            .and_then(|p| p.to_str()).unwrap_or("config").replace(std::path::MAIN_SEPARATOR, ":");
        Ok(format!("{folder}:{name}"))
    }

    fn format(path: &Path) -> CacheResult<&'static str> {
        match path.extension().and_then(OsStr::to_str).map(|s| s.to_ascii_lowercase()).as_deref() {
            Some("json") => Ok("json"),
            Some("yaml" | "yml") => Ok("yaml"),
            _ => Err(invalid("only .json, .yaml and .yml are supported")),
        }
    }

    async fn read_disk(&self, relative: &Path) -> CacheResult<Value> {
        let path = self.resolve(relative)?;
        let raw = fs::read_to_string(&path).await?;
        let value = if Self::format(&path)? == "json" {
            serde_json::from_str(&raw)?
        } else {
            serde_saphyr::from_str(&raw)?
        };
        Ok(value)
    }

    pub async fn read_config(&self, file: impl AsRef<Path>) -> CacheResult<Value> {
        let file = file.as_ref();
        let key = self.cache_key(file)?;
        let cache = get_cache();
        if let Some(value) = cache.get(&key).await? { return Ok(value); }
        let value = self.read_disk(file).await?;
        cache.set(&key, value.clone()).await?;
        Ok(value)
    }

    pub async fn list_configs(&self, folder: impl AsRef<Path>) -> CacheResult<Vec<PathBuf>> {
        let folder = folder.as_ref();
        // Empty path means root.
        let dir = if folder.as_os_str().is_empty() { self.root.clone() } else { self.resolve(folder)? };
        let mut entries = fs::read_dir(dir).await?;
        let mut files = Vec::new();
        while let Some(entry) = entries.next_entry().await? {
            let typ = entry.file_type().await?;
            if !typ.is_file() { continue; }
            let path = entry.path();
            if Self::format(&path).is_ok() {
                files.push(path.strip_prefix(&self.root)?.to_path_buf());
            }
        }
        files.sort();
        Ok(files)
    }

    pub async fn read_configs(&self, folder: impl AsRef<Path>) -> CacheResult<std::collections::BTreeMap<String, Value>> {
        let files = self.list_configs(folder).await?;
        let mut result = std::collections::BTreeMap::new();
        for file in files {
            let key = self.cache_key(&file)?;
            result.insert(key, self.read_config(&file).await?);
        }
        Ok(result)
    }

    /// Discover and cache every JSON/YAML config in the Streambot root and its
    /// non-excluded directories. Returns the number of loaded files.
    pub async fn read_all_configs(&self) -> CacheResult<usize> {
        let mut pending = vec![PathBuf::new()];
        let mut count = 0;

        while let Some(folder) = pending.pop() {
            let directory = if folder.as_os_str().is_empty() {
                self.root.clone()
            } else {
                self.resolve(&folder)?
            };

            let mut entries = match fs::read_dir(&directory).await {
                Ok(entries) => entries,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            };

            while let Some(entry) = entries.next_entry().await? {
                let name = entry.file_name();
                let relative = folder.join(&name);
                let kind = entry.file_type().await?;

                if kind.is_dir() {
                    // Excluded directories (including their descendants) never get scanned.
                    if folder.as_os_str().is_empty()
                        && name.to_str().is_some_and(|n| SKIPPED_DIRECTORIES.contains(&n))
                    {
                        continue;
                    }
                    pending.push(relative);
                } else if kind.is_file() && Self::format(&relative).is_ok() {
                    self.read_config(&relative).await?;
                    count += 1;
                }
            }
        }

        Ok(count)
    }

    async fn write_disk(&self, file: &Path, value: &Value) -> CacheResult<()> {
        let path = self.resolve(file)?;
        let bytes = if Self::format(&path)? == "json" {
            serde_json::to_vec_pretty(value)?
        } else {
            serde_saphyr::to_string(value)?.into_bytes()
        };
        if let Some(parent) = path.parent() { fs::create_dir_all(parent).await?; }
        let n = TMP_ID.fetch_add(1, Ordering::Relaxed);
        let temp = path.with_extension(format!("{}.{}.{}.tmp", path.extension().and_then(OsStr::to_str).unwrap_or("cfg"), std::process::id(), n));
        fs::write(&temp, bytes).await?;
        if let Err(err) = fs::rename(&temp, &path).await {
            let _ = fs::remove_file(&temp).await;
            return Err(err.into());
        }
        Ok(())
    }

    /// Returns false when data is empty, leaving both disk and cache unchanged.
    pub async fn write_config(&self, file: impl AsRef<Path>, value: Value) -> CacheResult<bool> {
        if !meaningful(&value) { return Ok(false); }
        let file = file.as_ref();
        let key = self.cache_key(file)?;
        let _guard = self.lock.lock().await;
        self.write_disk(file, &value).await?;
        let cache = get_cache();
        cache.delete(&key).await?;
        cache.set(&key, value).await?;
        Ok(true)
    }

    /// Recursively merges objects, retains untouched fields; arrays/scalars replace their target.
    /// Returns false for an empty patch or if there are no actual changes.
    pub async fn update_config(&self, file: impl AsRef<Path>, patch: Value) -> CacheResult<bool> {
        if !meaningful(&patch) { return Ok(false); }
        let file = file.as_ref();
        let key = self.cache_key(file)?;
        let _guard = self.lock.lock().await;
        // Prefer disk for writes to avoid stale cached versions after external edits.
        let mut current = match self.read_disk(file).await {
            Ok(v) => v,
            Err(e) => {
                if self.resolve(file)?.exists() { return Err(e); }
                Value::Object(serde_json::Map::new())
            }
        };
        let before = current.clone();
        merge(&mut current, patch);
        if current == before || !meaningful(&current) { return Ok(false); }
        self.write_disk(file, &current).await?;
        let cache = get_cache();
        cache.delete(&key).await?;
        cache.set(&key, current).await?;
        Ok(true)
    }

    pub async fn delete_config(&self, file: impl AsRef<Path>) -> CacheResult<bool> {
        let file = file.as_ref();
        let path = self.resolve(file)?;
        Self::format(&path)?;
        let key = self.cache_key(file)?;
        let _guard = self.lock.lock().await;
        let removed = match fs::remove_file(&path).await {
            Ok(()) => true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(e) => return Err(e.into()),
        };
        get_cache().delete(&key).await?;
        Ok(removed)
    }

    pub async fn invalidate(&self, file: impl AsRef<Path>) -> CacheResult<()> {
        let key = self.cache_key(file)?;
        get_cache().delete(&key).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn key_generation() {
        let config = ConfigFiles::new("/tmp/fake-streambot");
        assert_eq!(config.cache_key("streambot-settings.json").unwrap(), "config:settings");
        assert_eq!(config.cache_key("macros/streambot-test.yaml").unwrap(), "macros:test");
        assert_eq!(config.cache_key("commands/hello.yml").unwrap(), "commands:hello");
        assert!(config.cache_key("assets/bad.json").is_err());
    }
    #[test]
    fn merge_keeps_fields() {
        let mut value = json!({"a":1,"nested":{"b":2,"c":3}});
        merge(&mut value, json!({"nested":{"b":4}}));
        assert_eq!(value, json!({"a":1,"nested":{"b":4,"c":3}}));
    }
}
