use std::{env, fs::{self, File, OpenOptions}, io::{self, Write}, path::{Path, PathBuf}};

use crate::{AppError, Result};

/// Resolved Linux XDG locations used by the client.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct XdgPaths {
    pub config_dir: PathBuf,
    pub config_file: PathBuf,
    pub cache_dir: PathBuf,
}

/// Backwards-compatible descriptive alias for callers that prefer ConfigPaths.
pub type ConfigPaths = XdgPaths;

impl XdgPaths {
    pub fn from_env() -> Self {
        let config_home = xdg_home("XDG_CONFIG_HOME", ".config");
        let cache_home = xdg_home("XDG_CACHE_HOME", ".cache");
        let config_dir = config_home.join("lemmy");
        let cache_dir = cache_home.join("lemmy");
        let config_file = config_dir.join("config.toml");
        Self { config_dir, config_file, cache_dir }
    }

    pub fn config_path(&self) -> &Path { &self.config_file }
    pub fn cache_path(&self) -> &Path { &self.cache_dir }
}

pub fn paths() -> XdgPaths { XdgPaths::from_env() }
pub fn config_dir() -> PathBuf { XdgPaths::from_env().config_dir }
pub fn config_path() -> PathBuf { XdgPaths::from_env().config_file }
pub fn cache_dir() -> PathBuf { XdgPaths::from_env().cache_dir }
pub fn cache_path() -> PathBuf { cache_dir() }

fn xdg_home(variable: &str, fallback: &str) -> PathBuf {
    if let Ok(value) = env::var(variable) {
        let candidate = PathBuf::from(value);
        if candidate.is_absolute() {
            return candidate;
        }
    }
    env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(fallback)
}

/// Write bytes using a sibling temporary file and an atomic rename.
pub(crate) fn write_atomic(path: &Path, contents: &[u8]) -> Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|error| storage_error(path, error))?;

    let mut temporary = None;
    let process = std::process::id();
    for attempt in 0..100u32 {
        let candidate = parent.join(format!(".{}.tmp-{}-{}", path.file_name().and_then(|name| name.to_str()).unwrap_or("config"), process, attempt));
        match open_restrictive(&candidate) {
            Ok(file) => {
                temporary = Some((candidate, file));
                break;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(storage_error(path, error)),
        }
    }
    let (temporary_path, mut file) = temporary.ok_or_else(|| {
        AppError::Storage(format!("cannot create temporary file beside {}", path.display()))
    })?;

    let result = (|| {
        file.write_all(contents).map_err(|error| storage_error(path, error))?;
        file.flush().map_err(|error| storage_error(path, error))?;
        file.sync_all().map_err(|error| storage_error(path, error))?;
        fs::rename(&temporary_path, path).map_err(|error| storage_error(path, error))?;
        sync_directory(parent).map_err(|error| storage_error(path, error))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary_path);
    }
    result
}

fn open_restrictive(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(file)
}

fn sync_directory(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        File::open(path)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

fn storage_error(path: &Path, error: io::Error) -> AppError {
    AppError::Storage(format!("cannot write {}: {error}", path.display()))
}
