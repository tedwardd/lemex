use std::path::{Path, PathBuf};

use crate::{AppConfig, Result};
use crate::domain::{Profile, ProfileId};

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

    /// Create or replace a profile's non-secret metadata, atomically. A same-id
    /// replacement overwrites the existing entry; secrets are never handled
    /// here (the caller owns credential-store lifecycle).
    pub fn create(&self, profile: Profile) -> Result<()> {
        let mut config = self.load_config()?;
        if let Some(existing) = config.profiles.iter_mut().find(|existing| existing.id == profile.id) {
            *existing = profile;
        } else {
            config.profiles.push(profile);
        }
        self.save_config(&config)
    }

    /// Fetch a configured profile's non-secret metadata by id.
    pub fn get(&self, id: &ProfileId) -> Result<Profile> {
        self.load()?
            .into_iter()
            .find(|profile| profile.id == *id)
            .ok_or_else(|| crate::error::AppError::Configuration(format!("profile {id} is not configured")))
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
