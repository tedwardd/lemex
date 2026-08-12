use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use url::Url;

use crate::domain::{Profile, ProfileId};
use crate::{AppError, Result};

use super::paths;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MediaConfig {
    pub kitty_enabled: bool,
    pub mailcap_enabled: bool,
    pub download_directory: Option<PathBuf>,
    pub collision_policy: String,
    pub handlers: HashMap<String, String>,
}

impl Default for MediaConfig {
    fn default() -> Self {
        Self {
            kitty_enabled: false,
            mailcap_enabled: true,
            download_directory: None,
            collision_policy: "prompt".to_owned(),
            handlers: HashMap::new(),
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CacheConfig {
    pub directory: Option<PathBuf>,
    pub max_size_bytes: Option<u64>,
}

/// Opt-in diagnostic logging policy. Logs redact credentials, tokens,
/// private content, and sensitive profile values; disabled by default.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LogConfig {
    pub enabled: bool,
    pub level: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AppConfig {
    pub profiles: Vec<Profile>,
    pub keymaps: HashMap<String, String>,
    pub media: MediaConfig,
    pub cache: CacheConfig,
    pub logging: LogConfig,
}

impl AppConfig {
    pub fn from_toml(source: &str) -> Result<Self> {
        let raw: RawConfig = toml::from_str(source)
            .map_err(|error| AppError::Configuration(format!("invalid TOML: {error}")))?;

        let mut ids = HashSet::with_capacity(raw.profiles.len());
        let mut profiles = Vec::with_capacity(raw.profiles.len());
        for profile in raw.profiles {
            if profile.id.trim().is_empty() {
                return Err(AppError::Configuration(
                    "profile id must not be empty".to_owned(),
                ));
            }
            let id = ProfileId(profile.id);
            if !ids.insert(id.clone()) {
                return Err(AppError::Configuration(format!(
                    "duplicate profile id: {}",
                    id.0
                )));
            }
            let instance_url = Url::parse(&profile.instance_url).map_err(|error| {
                AppError::Configuration(format!("invalid instance URL: {error}"))
            })?;
            if !matches!(instance_url.scheme(), "http" | "https") {
                return Err(AppError::Configuration(
                    "instance URL must use http or https".to_owned(),
                ));
            }
            if instance_url.host_str().is_none() {
                return Err(AppError::Configuration(
                    "instance URL must include a host".to_owned(),
                ));
            }
            if !instance_url.username().is_empty() || instance_url.password().is_some() {
                return Err(AppError::Configuration(
                    "instance URL must not contain credentials".to_owned(),
                ));
            }
            profiles.push(Profile {
                id,
                instance_url,
                account_label: profile.account_label,
            });
        }

        Ok(Self {
            profiles,
            keymaps: raw.keymaps,
            media: raw.media.into_config(),
            cache: raw.cache.into_config(),
            logging: raw.logging.into_config(),
        })
    }

    pub fn to_toml(&self) -> Result<String> {
        let mut ids = HashSet::with_capacity(self.profiles.len());
        for profile in &self.profiles {
            if profile.id.0.trim().is_empty() {
                return Err(AppError::Configuration(
                    "profile id must not be empty".to_owned(),
                ));
            }
            if !ids.insert(profile.id.clone()) {
                return Err(AppError::Configuration(format!(
                    "duplicate profile id: {}",
                    profile.id
                )));
            }
            if !matches!(profile.instance_url.scheme(), "http" | "https") {
                return Err(AppError::Configuration(
                    "instance URL must use http or https".to_owned(),
                ));
            }
            if profile.instance_url.host_str().is_none()
                || !profile.instance_url.username().is_empty()
                || profile.instance_url.password().is_some()
            {
                return Err(AppError::Configuration(
                    "instance URL must include a host and must not contain credentials".to_owned(),
                ));
            }
        }
        let raw = RawConfig {
            profiles: self
                .profiles
                .iter()
                .map(|profile| RawProfile {
                    id: profile.id.0.clone(),
                    instance_url: profile.instance_url.to_string(),
                    account_label: profile.account_label.clone(),
                })
                .collect(),
            keymaps: self.keymaps.clone(),
            media: RawMediaConfig::from_config(&self.media),
            cache: RawCacheConfig::from_config(&self.cache),
            logging: RawLogConfig::from_config(&self.logging),
        };
        toml::to_string_pretty(&raw)
            .map_err(|error| AppError::Configuration(format!("cannot encode TOML: {error}")))
    }

    pub fn load(path: &Path) -> Result<Self> {
        let source = fs::read_to_string(path).map_err(|error| {
            AppError::Storage(format!("cannot read {}: {error}", path.display()))
        })?;
        Self::from_toml(&source)
    }

    pub fn write_atomic(&self, path: &Path) -> Result<()> {
        let encoded = self.to_toml()?;
        paths::write_atomic(path, encoded.as_bytes())
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    #[serde(default)]
    profiles: Vec<RawProfile>,
    #[serde(default)]
    keymaps: HashMap<String, String>,
    #[serde(default)]
    media: RawMediaConfig,
    #[serde(default)]
    cache: RawCacheConfig,
    #[serde(default)]
    logging: RawLogConfig,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawProfile {
    id: String,
    instance_url: String,
    #[serde(default)]
    account_label: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawMediaConfig {
    #[serde(default)]
    kitty_enabled: bool,
    #[serde(default = "default_mailcap_enabled")]
    mailcap_enabled: bool,
    #[serde(default)]
    download_directory: Option<PathBuf>,
    #[serde(default = "default_collision_policy")]
    collision_policy: String,
    #[serde(default)]
    handlers: HashMap<String, String>,
}

impl Default for RawMediaConfig {
    fn default() -> Self {
        Self {
            kitty_enabled: false,
            mailcap_enabled: default_mailcap_enabled(),
            download_directory: None,
            collision_policy: default_collision_policy(),
            handlers: HashMap::new(),
        }
    }
}

fn default_mailcap_enabled() -> bool {
    true
}
fn default_collision_policy() -> String {
    "prompt".to_owned()
}

impl RawMediaConfig {
    fn into_config(self) -> MediaConfig {
        MediaConfig {
            kitty_enabled: self.kitty_enabled,
            mailcap_enabled: self.mailcap_enabled,
            download_directory: self.download_directory,
            collision_policy: self.collision_policy,
            handlers: self.handlers,
        }
    }

    fn from_config(config: &MediaConfig) -> Self {
        Self {
            kitty_enabled: config.kitty_enabled,
            mailcap_enabled: config.mailcap_enabled,
            download_directory: config.download_directory.clone(),
            collision_policy: config.collision_policy.clone(),
            handlers: config.handlers.clone(),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawCacheConfig {
    #[serde(default)]
    directory: Option<PathBuf>,
    #[serde(default)]
    max_size_bytes: Option<u64>,
}

impl RawCacheConfig {
    fn into_config(self) -> CacheConfig {
        CacheConfig {
            directory: self.directory,
            max_size_bytes: self.max_size_bytes,
        }
    }

    fn from_config(config: &CacheConfig) -> Self {
        Self {
            directory: config.directory.clone(),
            max_size_bytes: config.max_size_bytes,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawLogConfig {
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    level: Option<String>,
}

impl RawLogConfig {
    fn into_config(self) -> LogConfig {
        LogConfig {
            enabled: self.enabled,
            level: self.level,
        }
    }

    fn from_config(config: &LogConfig) -> Self {
        Self {
            enabled: config.enabled,
            level: config.level.clone(),
        }
    }
}

impl AppConfig {
    /// Configure a key mapping. The sequence is validated (non-empty, no
    /// whitespace); the mapping applies to the input engine on the next
    /// launch.
    pub fn set_keymap(&mut self, name: String, sequence: String) -> Result<()> {
        if name.trim().is_empty() {
            return Err(AppError::Configuration(
                "keymap name must not be empty".to_owned(),
            ));
        }
        if sequence.trim().is_empty() {
            return Err(AppError::Configuration(format!(
                "keymap {name}: key sequence must not be empty"
            )));
        }
        if sequence.chars().any(char::is_whitespace) {
            return Err(AppError::Configuration(format!(
                "keymap {name}: key sequence must not contain whitespace"
            )));
        }
        self.keymaps.insert(name, sequence);
        Ok(())
    }

    pub fn set_kitty(&mut self, enabled: bool) -> Result<()> {
        self.media.kitty_enabled = enabled;
        Ok(())
    }

    pub fn set_mailcap(&mut self, enabled: bool) -> Result<()> {
        self.media.mailcap_enabled = enabled;
        Ok(())
    }

    /// Set the download directory. An existing path must be a directory; new
    /// paths are created on application.
    pub fn set_download_directory(&mut self, directory: Option<PathBuf>) -> Result<()> {
        if let Some(path) = &directory {
            if path.as_os_str().is_empty() {
                return Err(AppError::Configuration(
                    "download directory must not be empty".to_owned(),
                ));
            }
            if path.exists() && !path.is_dir() {
                return Err(AppError::Configuration(format!(
                    "download directory {} is not a directory",
                    path.display()
                )));
            }
        }
        self.media.download_directory = directory;
        Ok(())
    }

    /// Set the download collision policy. Unlike the lenient config parsing,
    /// unknown values are rejected instead of silently falling back.
    pub fn set_collision_policy(&mut self, policy: String) -> Result<()> {
        match policy.trim() {
            "prompt" | "overwrite" | "unique-name" | "unique_name" => {}
            _ => {
                return Err(AppError::Configuration(format!(
                    "collision policy must be one of prompt, overwrite, unique-name; got {policy}"
                )));
            }
        }
        self.media.collision_policy = policy;
        Ok(())
    }

    pub fn set_cache_directory(&mut self, directory: Option<PathBuf>) -> Result<()> {
        if let Some(path) = &directory
            && path.as_os_str().is_empty()
        {
            return Err(AppError::Configuration(
                "cache directory must not be empty".to_owned(),
            ));
        }
        self.cache.directory = directory;
        Ok(())
    }

    pub fn set_cache_size(&mut self, max_size_bytes: Option<u64>) -> Result<()> {
        if let Some(size) = max_size_bytes
            && size == 0
        {
            return Err(AppError::Configuration(
                "cache size must be a positive byte count".to_owned(),
            ));
        }
        self.cache.max_size_bytes = max_size_bytes;
        Ok(())
    }

    /// Set the opt-in logging policy. `level` must parse as a tracing level
    /// (trace, debug, info, warn, error). Logs always redact secrets.
    pub fn set_logging(&mut self, enabled: bool, level: Option<String>) -> Result<()> {
        if let Some(level) = &level {
            level.parse::<tracing::Level>().map_err(|_| {
                AppError::Configuration(format!(
                    "log level must be one of trace, debug, info, warn, error; got {level}"
                ))
            })?;
        }
        self.logging = LogConfig { enabled, level };
        Ok(())
    }
}
