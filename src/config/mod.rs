pub mod file;
pub mod watcher;

use std::{fs, io, path::{Path, PathBuf}};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheBackend {
    Redis,
    File,
}

#[derive(Debug, Clone)]
pub struct CacheConfig {
    pub backend: CacheBackend,
    pub redis_url: String,
    pub redis_prefix: String,
    pub file_path: PathBuf,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            backend: CacheBackend::Redis,
            redis_url: "redis://127.0.0.1:6379/".into(),
            redis_prefix: "streamding:".into(),
            file_path: "/tmp/streamding-cache.json".into(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Config {
    pub cache: CacheConfig,
}

impl Config {
    /// Loads optional .env from the project root; absent file => defaults.
    pub fn load() -> io::Result<Self> {
        Self::from_path(Path::new(env!("CARGO_MANIFEST_DIR")).join(".env"))
    }

    pub fn from_path(path: impl AsRef<Path>) -> io::Result<Self> {
        let raw = match fs::read_to_string(path) {
            Ok(raw) => raw,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(err) => return Err(err),
        };
        Self::parse(&raw)
    }

    pub fn parse(raw: &str) -> io::Result<Self> {
        let mut result = Self::default();
        let mut section = String::new();

        for (index, original_line) in raw.lines().enumerate() {
            let line = original_line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                continue;
            }
            if line.starts_with('[') && line.ends_with(']') {
                section = line[1..line.len()-1].trim().to_ascii_lowercase();
                continue;
            }
            let (key, raw_value) = line.split_once(':').ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, format!("Invalid .env line {}: expected key: value", index + 1))
            })?;
            let key = key.trim().to_ascii_lowercase();
            let value = raw_value.trim();
            let value = if value.len() >= 2 && ((value.starts_with('"') && value.ends_with('"')) || (value.starts_with('\'') && value.ends_with('\''))) {
                &value[1..value.len()-1]
            } else {
                value
            };

            if section == "cache" {
                match key.as_str() {
                    "backend" => result.cache.backend = match value.to_ascii_lowercase().as_str() {
                        "redis" => CacheBackend::Redis,
                        "file" => CacheBackend::File,
                        _ => return Err(io::Error::new(io::ErrorKind::InvalidData, format!("Invalid cache backend on line {}: {value}", index + 1))),
                    },
                    "redis_url" => result.cache.redis_url = value.into(),
                    "redis_prefix" => result.cache.redis_prefix = value.into(),
                    "file_path" => result.cache.file_path = value.into(),
                    _ => {} // Other cache settings can be added later.
                }
            }
            // Unknown sections belong to future components.
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_redis() {
        assert_eq!(Config::parse("").unwrap().cache.backend, CacheBackend::Redis);
    }

    #[test]
    fn overrides_file_backend() {
        let cfg = Config::parse("[cache]\nbackend: file\nfile_path: /tmp/custom.json\n").unwrap();
        assert_eq!(cfg.cache.backend, CacheBackend::File);
        assert_eq!(cfg.cache.file_path, PathBuf::from("/tmp/custom.json"));
    }

    #[test]
    fn supports_url_containing_colons() {
        let cfg = Config::parse("[cache]\nredis_url: redis://localhost:6379/\n").unwrap();
        assert_eq!(cfg.cache.redis_url, "redis://localhost:6379/");
    }

    #[test]
    fn ignores_unknown_sections() {
        assert!(Config::parse("[twitch]\nclient_id: abc\n").is_ok());
    }
}
