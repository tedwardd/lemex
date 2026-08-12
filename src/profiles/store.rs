use std::path::{Path, PathBuf};

use crate::{AppConfig, Result};
use crate::domain::Profile;

/// Persistence boundary for non-secret profile metadata.
#[derive(Clone, Debug)]
pub struct ProfileStore {
    path: PathBuf,
}

impl ProfileStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path { &self.path }

    pub fn load(&self) -> Result<Vec<Profile>> {
        load(&self.path)
    }

    pub fn save(&self, profiles: &[Profile]) -> Result<()> {
        save(&self.path, profiles)
    }

    pub fn load_config(&self) -> Result<AppConfig> {
        if !self.path.exists() {
            return Ok(AppConfig::default());
        }
        AppConfig::load(&self.path)
    }

    pub fn save_config(&self, config: &AppConfig) -> Result<()> {
        config.write_atomic(&self.path)
    }
}

pub fn load(path: &Path) -> Result<Vec<Profile>> {
    Ok(AppConfig::load(path)?.profiles)
}

pub fn save(path: &Path, profiles: &[Profile]) -> Result<()> {
    let config = AppConfig {
        profiles: profiles.to_vec(),
        ..AppConfig::default()
    };
    config.write_atomic(path)
}

pub fn default_store() -> ProfileStore {
    ProfileStore::new(crate::config::config_path())
}
