use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use serde::{Deserialize, Serialize};
use url::Url;

use crate::domain::{Profile, ProfileId};
use crate::{AppError, Result};

use super::paths;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MediaConfig {
    pub mailcap_enabled: bool,
    pub download_directory: Option<PathBuf>,
    pub collision_policy: String,
    pub handlers: HashMap<String, String>,
}

impl Default for MediaConfig {
    fn default() -> Self {
        Self {
            mailcap_enabled: true,
            download_directory: None,
            collision_policy: "prompt".to_owned(),
            handlers: HashMap::new(),
        }
    }
}

/// Default cap on the feed cache when the config does not set one: 64 MiB of
/// cached post JSON keeps browsing history bounded on disk. Drafts live in a
/// separate table and are never evicted.
pub const DEFAULT_CACHE_SIZE_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CacheConfig {
    pub directory: Option<PathBuf>,
    pub max_size_bytes: Option<u64>,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            directory: None,
            max_size_bytes: Some(DEFAULT_CACHE_SIZE_BYTES),
        }
    }
}

/// Default whole-read (connect + request + retries) deadline applied when the
/// config does not set one. A dead instance fails at the 5 s connect bound
/// per attempt; a slow-but-alive server still gets its 10 s per-attempt
/// budget; retries can never multiply the worst case beyond this total.
pub const DEFAULT_HTTP_CONNECT_TIMEOUT_SECS: u64 = 5;
pub const DEFAULT_HTTP_REQUEST_TIMEOUT_SECS: u64 = 10;
pub const DEFAULT_HTTP_TOTAL_TIMEOUT_SECS: u64 = 15;

/// HTTP client timeout budget. Values parse from `[http]`, with the
/// invariant `connect <= request <= total` enforced by clamping each value
/// downward; a `0` value is a configuration error. Takes effect on the next
/// launch (the HTTP client is built at startup).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HttpConfig {
    /// TCP/TLS connect deadline per attempt (was unbounded beyond the
    /// request deadline, so a blackholed instance burned the full budget).
    pub connect_timeout: Duration,
    /// Per-attempt deadline covering connect, response, and body.
    pub request_timeout: Duration,
    /// Whole-read deadline including retries; not applied to mutations.
    pub total_timeout: Duration,
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(DEFAULT_HTTP_CONNECT_TIMEOUT_SECS),
            request_timeout: Duration::from_secs(DEFAULT_HTTP_REQUEST_TIMEOUT_SECS),
            total_timeout: Duration::from_secs(DEFAULT_HTTP_TOTAL_TIMEOUT_SECS),
        }
    }
}

/// Opt-in diagnostic logging policy. Logs redact credentials, tokens,
/// private content, and sensitive profile values; disabled by default.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LogConfig {
    pub enabled: bool,
    pub level: Option<String>,
}

/// Customizable UI palette. Every key accepts a color name (`red`, `cyan`,
/// `darkgray`, …) or `#rrggbb` hex and defaults to the client's standard
/// palette when absent.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ColorsConfig {
    /// Modal borders and titles (and the community-picker selection).
    pub accent: String,
    /// Modal interior background.
    pub surface: String,
    /// Modal interior text.
    pub text: String,
    /// Status-bar error color.
    pub error: String,
    /// Status-bar pending color.
    pub pending: String,
    /// Status-bar ready color.
    pub ready: String,
}

impl Default for ColorsConfig {
    fn default() -> Self {
        Self {
            accent: "cyan".into(),
            surface: "darkgray".into(),
            text: "white".into(),
            error: "red".into(),
            pending: "yellow".into(),
            ready: "green".into(),
        }
    }
}

/// Parse a color specification: a named ANSI color (case-insensitive) or
/// `#rrggbb` hex. `None` for anything else so callers can attach context.
pub fn parse_color(value: &str) -> Option<ratatui::style::Color> {
    use ratatui::style::Color;
    let trimmed = value.trim();
    let color = match trimmed.to_ascii_lowercase().as_str() {
        "black" => Color::Black,
        "red" => Color::Red,
        "green" => Color::Green,
        "yellow" => Color::Yellow,
        "blue" => Color::Blue,
        "magenta" => Color::Magenta,
        "cyan" => Color::Cyan,
        "gray" => Color::Gray,
        "darkgray" => Color::DarkGray,
        "lightred" => Color::LightRed,
        "lightgreen" => Color::LightGreen,
        "lightyellow" => Color::LightYellow,
        "lightblue" => Color::LightBlue,
        "lightmagenta" => Color::LightMagenta,
        "lightcyan" => Color::LightCyan,
        "white" => Color::White,
        "reset" => Color::Reset,
        _ if trimmed.starts_with('#') => {
            let hex = trimmed.trim_start_matches('#');
            if hex.len() == 6 && hex.chars().all(|character| character.is_ascii_hexdigit()) {
                let value = u32::from_str_radix(hex, 16).ok()?;
                Color::Rgb((value >> 16) as u8, (value >> 8) as u8, value as u8)
            } else {
                return None;
            }
        }
        _ => return None,
    };
    Some(color)
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AppConfig {
    pub profiles: Vec<Profile>,
    pub keymaps: HashMap<String, String>,
    pub media: MediaConfig,
    pub cache: CacheConfig,
    pub logging: LogConfig,
    /// Action run once at launch (for example `feed`); empty means none.
    pub startup: String,
    /// UI palette; absent keys fall back to the standard colors.
    pub colors: ColorsConfig,
    /// HTTP client timeout budget; absent keys fall back to
    /// connect 5 s / request 10 s / total 15 s.
    pub http: HttpConfig,
    /// Permit `http://` instance URLs. Off by default: credentials (login
    /// password, session JWT) must not travel in cleartext unless the user
    /// explicitly opts in.
    pub allow_insecure_http: bool,
}

impl AppConfig {
    /// Starter configuration for a first run: one profile on a general
    /// instance, everything else at defaults. Written to the config path
    /// when no config file exists, so a fresh install launches instead of
    /// failing with a bare "no profiles" error. The user edits the instance
    /// or adds profiles with `:profile-new`.
    pub fn starter() -> Self {
        Self {
            profiles: vec![Profile {
                id: ProfileId::from("main"),
                instance_url: Url::parse("https://lemmy.world").expect("static instance URL"),
                account_label: None,
            }],
            ..Default::default()
        }
    }

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
            if instance_url.scheme() == "http" && !raw.allow_insecure_http {
                return Err(AppError::Configuration(format!(
                    "profile {} uses an http:// instance URL: credentials would travel in cleartext; set allow_insecure_http = true in the config to accept this deliberately",
                    id.0
                )));
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

        let startup = validate_startup(&raw.startup)?;
        let colors = raw.colors.into_config()?;
        let http = raw.http.into_config()?;
        Ok(Self {
            profiles,
            keymaps: raw.keymaps,
            media: raw.media.into_config(),
            cache: raw.cache.into_config(),
            logging: raw.logging.into_config(),
            startup,
            colors,
            http,
            allow_insecure_http: raw.allow_insecure_http,
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
            if profile.instance_url.scheme() == "http" && !self.allow_insecure_http {
                return Err(AppError::Configuration(format!(
                    "profile {} uses an http:// instance URL: set allow_insecure_http = true in the config to accept this deliberately",
                    profile.id
                )));
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
            startup: self.startup.clone(),
            colors: RawColorsConfig::from_config(&self.colors),
            http: RawHttpConfig::from_config(&self.http),
            allow_insecure_http: self.allow_insecure_http,
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
    #[serde(default)]
    startup: String,
    #[serde(default)]
    colors: RawColorsConfig,
    /// HTTP client timeout budget.
    #[serde(default)]
    http: RawHttpConfig,
    /// Opt-in to `http://` instance URLs (credentials travel in cleartext).
    #[serde(default)]
    allow_insecure_http: bool,
}

/// Startup actions the client will run once at launch. Empty means the
/// client starts with the default (cache-hydrated, empty) view.
///
/// Accepted forms (a leading `:` is optional): `feed`, `subscribed`,
/// `search <query>`, `community <id>` — the same content views the client
/// opens interactively. Anything else is a configuration error so a typo
/// never silently launches with the wrong view.
fn validate_startup(value: &str) -> Result<String> {
    let trimmed = value.trim().strip_prefix(':').unwrap_or(value.trim());
    if trimmed.is_empty() {
        return Ok(String::new());
    }
    let mut parts = trimmed.split_whitespace();
    let command = parts.next();
    let has_arg = parts.next().is_some();
    let valid = matches!(
        (command, has_arg),
        (Some("feed"), false)
            | (Some("subscribed"), false)
            | (Some("search"), true)
            | (Some("community"), true)
    );
    if valid {
        Ok(trimmed.to_owned())
    } else {
        Err(AppError::Configuration(format!(
            "invalid startup action {value:?}: expected \"feed\", \"subscribed\", \"search <query>\", or \"community <id>\""
        )))
    }
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
    /// Deprecated: accepted for backward compatibility with configs that
    /// still set it, but inline kitty graphics rendering was removed and
    /// this key no longer has any effect. Never re-emitted on save.
    #[serde(default, skip_serializing)]
    #[expect(dead_code)] // kept solely so legacy configs keep parsing
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
            mailcap_enabled: self.mailcap_enabled,
            download_directory: self.download_directory,
            collision_policy: self.collision_policy,
            handlers: self.handlers,
        }
    }

    fn from_config(config: &MediaConfig) -> Self {
        Self {
            kitty_enabled: false,
            mailcap_enabled: config.mailcap_enabled,
            download_directory: config.download_directory.clone(),
            collision_policy: config.collision_policy.clone(),
            handlers: config.handlers.clone(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawCacheConfig {
    #[serde(default)]
    directory: Option<PathBuf>,
    #[serde(default = "default_cache_size")]
    max_size_bytes: Option<u64>,
}

impl Default for RawCacheConfig {
    fn default() -> Self {
        Self {
            directory: None,
            max_size_bytes: Some(DEFAULT_CACHE_SIZE_BYTES),
        }
    }
}

fn default_cache_size() -> Option<u64> {
    Some(DEFAULT_CACHE_SIZE_BYTES)
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

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawHttpConfig {
    #[serde(default = "default_http_connect_timeout")]
    connect_timeout_secs: u64,
    #[serde(default = "default_http_request_timeout")]
    request_timeout_secs: u64,
    #[serde(default = "default_http_total_timeout")]
    total_timeout_secs: u64,
}

impl Default for RawHttpConfig {
    fn default() -> Self {
        Self {
            connect_timeout_secs: default_http_connect_timeout(),
            request_timeout_secs: default_http_request_timeout(),
            total_timeout_secs: default_http_total_timeout(),
        }
    }
}

fn default_http_connect_timeout() -> u64 {
    DEFAULT_HTTP_CONNECT_TIMEOUT_SECS
}
fn default_http_request_timeout() -> u64 {
    DEFAULT_HTTP_REQUEST_TIMEOUT_SECS
}
fn default_http_total_timeout() -> u64 {
    DEFAULT_HTTP_TOTAL_TIMEOUT_SECS
}

impl RawHttpConfig {
    fn into_config(self) -> Result<HttpConfig> {
        // A zero deadline would make every request fail instantly, so it is
        // rejected loudly instead of tolerated; inverted orderings are
        // clamped into the invariant connect <= request <= total (each value
        // can only shrink, never grow beyond a smaller one).
        fn checked(name: &str, secs: u64) -> Result<u64> {
            if secs == 0 {
                Err(AppError::Configuration(format!(
                    "[http] {name} must be at least 1"
                )))
            } else {
                Ok(secs)
            }
        }
        let connect = checked("connect_timeout_secs", self.connect_timeout_secs)?;
        let request = checked("request_timeout_secs", self.request_timeout_secs)?;
        let total = checked("total_timeout_secs", self.total_timeout_secs)?;
        Ok(HttpConfig {
            connect_timeout: Duration::from_secs(connect.min(request).min(total)),
            request_timeout: Duration::from_secs(request.min(total)),
            total_timeout: Duration::from_secs(total),
        })
    }

    fn from_config(config: &HttpConfig) -> Self {
        Self {
            connect_timeout_secs: config.connect_timeout.as_secs(),
            request_timeout_secs: config.request_timeout.as_secs(),
            total_timeout_secs: config.total_timeout.as_secs(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawColorsConfig {
    #[serde(default = "default_color_accent")]
    accent: String,
    #[serde(default = "default_color_surface")]
    surface: String,
    #[serde(default = "default_color_text")]
    text: String,
    #[serde(default = "default_color_error")]
    error: String,
    #[serde(default = "default_color_pending")]
    pending: String,
    #[serde(default = "default_color_ready")]
    ready: String,
}

impl Default for RawColorsConfig {
    fn default() -> Self {
        Self {
            accent: default_color_accent(),
            surface: default_color_surface(),
            text: default_color_text(),
            error: default_color_error(),
            pending: default_color_pending(),
            ready: default_color_ready(),
        }
    }
}

fn default_color_accent() -> String {
    "cyan".into()
}
fn default_color_surface() -> String {
    "darkgray".into()
}
fn default_color_text() -> String {
    "white".into()
}
fn default_color_error() -> String {
    "red".into()
}
fn default_color_pending() -> String {
    "yellow".into()
}
fn default_color_ready() -> String {
    "green".into()
}

impl RawColorsConfig {
    fn into_config(self) -> Result<ColorsConfig> {
        // Validate every key at load time so a typo never silently renders
        // with a fallback color.
        for (key, value) in [
            ("accent", &self.accent),
            ("surface", &self.surface),
            ("text", &self.text),
            ("error", &self.error),
            ("pending", &self.pending),
            ("ready", &self.ready),
        ] {
            if parse_color(value).is_none() {
                return Err(AppError::Configuration(format!(
                    "[colors] {key}: unknown color {value:?}; use a named color or #rrggbb"
                )));
            }
        }
        Ok(ColorsConfig {
            accent: self.accent,
            surface: self.surface,
            text: self.text,
            error: self.error,
            pending: self.pending,
            ready: self.ready,
        })
    }

    fn from_config(config: &ColorsConfig) -> Self {
        Self {
            accent: config.accent.clone(),
            surface: config.surface.clone(),
            text: config.text.clone(),
            error: config.error.clone(),
            pending: config.pending.clone(),
            ready: config.ready.clone(),
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
