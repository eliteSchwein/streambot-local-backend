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

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub restport: u16,
    pub wsport: u16,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self { restport: 8105, wsport: 8100 }
    }
}

#[derive(Debug, Clone, Default)]
pub struct TwitchConfig {
    pub test_mode: bool,
    pub client_id: String,
    pub client_secret: String, // server-side only, never returned by /config.json
    pub channels: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ApiWebsiteConfig {
    pub url: String,
    pub api_slug: String,
    pub token: String, // server-side only
}

#[derive(Debug, Clone)]
pub struct CloudConfig { pub url: String }
impl Default for CloudConfig {
    fn default() -> Self { Self { url: "https://cloud.streamding.dev".into() } }
}

#[derive(Debug, Clone, Default)]
pub struct Config {
    pub cache: CacheConfig,
    pub server: ServerConfig,
    pub twitch: TwitchConfig,
    pub api_website: ApiWebsiteConfig,
    pub cloud: CloudConfig,
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
            if section == "server" {
                let parsed_port = || -> io::Result<u16> {
                    let port = value.parse::<u16>().map_err(|_| {
                        io::Error::new(io::ErrorKind::InvalidData,
                            format!("Invalid server port on line {}: {value}", index + 1))
                    })?;
                    if port == 0 {
                        return Err(io::Error::new(io::ErrorKind::InvalidData,
                            format!("Server port must be greater than zero on line {}", index + 1)));
                    }
                    Ok(port)
                };
                match key.as_str() {
                    "restport" => result.server.restport = parsed_port()?,
                    "wsport" => result.server.wsport = parsed_port()?,
                    _ => {}
                }
            }
            match section.as_str() {
                "twitch" => match key.as_str() {
                    "test_mode" => result.twitch.test_mode = match value.to_ascii_lowercase().as_str() {
                        "true" | "yes" | "1" | "on" => true,
                        "false" | "no" | "0" | "off" => false,
                        _ => return Err(io::Error::new(io::ErrorKind::InvalidData, format!("Invalid twitch.test_mode on line {}", index + 1))),
                    },
                    "client_id" => result.twitch.client_id = value.into(),
                    "client_secret" => result.twitch.client_secret = value.into(),
                    "channels" => result.twitch.channels = value.trim_matches(|c| c == '[' || c == ']')
                        .split(',').map(|c| c.trim().trim_matches('"').trim_matches('\'').to_string())
                        .filter(|c| !c.is_empty()).collect(),
                    _ => {}
                },
                "cloud" => match key.as_str() {
                    "url" => result.cloud.url = value.trim_end_matches('/').to_owned(),
                    _ => {}
                },
                "api website" => match key.as_str() {
                    "url" => result.api_website.url = value.into(),
                    "api_slug" => result.api_website.api_slug = value.into(),
                    "token" => result.api_website.token = value.into(),
                    _ => {}
                },
                _ => {}
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
    fn cloud_config() {
        assert_eq!(Config::parse("").unwrap().cloud.url, "https://cloud.streamding.dev");
        assert_eq!(Config::parse("[cloud]\nurl: 'http://localhost:8080/'\n").unwrap().cloud.url, "http://localhost:8080");
    }

    #[test]
    fn server_defaults() {
        let c = Config::parse("").unwrap();
        assert_eq!(c.server.restport, 8105);
        assert_eq!(c.server.wsport, 8100);
    }

    #[test]
    fn server_overrides() {
        let c = Config::parse("[server]\nrestport: 9000\nwsport: 9001\n").unwrap();
        assert_eq!(c.server.restport, 9000);
        assert_eq!(c.server.wsport, 9001);
    }

    #[test]
    fn rejects_invalid_port() {
        assert!(Config::parse("[server]\nrestport: abc\n").is_err());
        assert!(Config::parse("[server]\nwsport: 0\n").is_err());
    }

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
