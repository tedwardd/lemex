use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};

use async_trait::async_trait;
#[cfg(target_os = "linux")]
use keyring::Entry;

use crate::{
    AppError, Result,
    domain::{ProfileId, Session, UserId},
};

/// Secure storage boundary for authentication sessions.
#[async_trait]
pub trait CredentialStore: Send + Sync {
    async fn get_session(&self, profile: &ProfileId) -> Result<Option<Session>>;
    async fn put_session(&self, profile: &ProfileId, session: &Session) -> Result<()>;
    async fn delete_session(&self, profile: &ProfileId) -> Result<()>;
}

/// In-memory credential store for tests and ephemeral application state.
#[derive(Clone, Default)]
pub struct MemoryCredentialStore {
    sessions: Arc<RwLock<HashMap<ProfileId, Session>>>,
}

#[async_trait]
impl CredentialStore for MemoryCredentialStore {
    async fn get_session(&self, profile: &ProfileId) -> Result<Option<Session>> {
        self.sessions
            .read()
            .map_err(|_| AppError::Storage("in-memory credential store lock poisoned".to_owned()))
            .map(|sessions| sessions.get(profile).cloned())
    }

    async fn put_session(&self, profile: &ProfileId, session: &Session) -> Result<()> {
        self.sessions
            .write()
            .map_err(|_| AppError::Storage("in-memory credential store lock poisoned".to_owned()))?
            .insert(profile.clone(), session.clone());
        Ok(())
    }

    async fn delete_session(&self, profile: &ProfileId) -> Result<()> {
        self.sessions
            .write()
            .map_err(|_| AppError::Storage("in-memory credential store lock poisoned".to_owned()))?
            .remove(profile);
        Ok(())
    }
}

impl MemoryCredentialStore {
    /// Snapshot every stored session keyed by profile id. Used by tests and by
    /// session inventory surfaces; never serialized.
    pub fn all(&self) -> HashMap<ProfileId, Session> {
        self.sessions
            .read()
            .map(|sessions| sessions.clone())
            .unwrap_or_default()
    }
}

/// OS credential-store backed session storage.
#[derive(Clone, Debug)]
pub struct KeyringCredentialStore {
    service: String,
}

impl KeyringCredentialStore {
    pub fn new(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
        }
    }

    #[cfg(target_os = "linux")]
    fn entry(&self, profile: &ProfileId) -> Result<Entry> {
        Entry::new(&self.service, &profile.to_string())
            .map_err(|error| storage_error(profile, "open", error))
    }
}

impl Default for KeyringCredentialStore {
    fn default() -> Self {
        Self::new("levim-client")
    }
}

#[async_trait]
impl CredentialStore for KeyringCredentialStore {
    async fn get_session(&self, profile: &ProfileId) -> Result<Option<Session>> {
        #[cfg(target_os = "linux")]
        {
            let entry = self.entry(profile)?;
            let encoded = match entry.get_password() {
                Ok(value) => value,
                Err(keyring::Error::NoEntry) => return Ok(None),
                Err(error) => return Err(storage_error(profile, "read", error)),
            };

            return decode_session(&encoded).map(Some).ok_or_else(|| {
                AppError::Storage(format!(
                    "keyring credential for profile {profile} is malformed; sign in again"
                ))
            });
        }

        #[cfg(not(target_os = "linux"))]
        {
            Err(unsupported_target_error("read", profile))
        }
    }

    async fn put_session(&self, profile: &ProfileId, session: &Session) -> Result<()> {
        #[cfg(target_os = "linux")]
        {
            let entry = self.entry(profile)?;
            let encoded = format!("{}:{}", session.user_id.0, session.token.expose_secret());
            return entry
                .set_password(&encoded)
                .map_err(|error| storage_error(profile, "write", error));
        }

        #[cfg(not(target_os = "linux"))]
        {
            let _ = session;
            Err(unsupported_target_error("write", profile))
        }
    }

    async fn delete_session(&self, profile: &ProfileId) -> Result<()> {
        #[cfg(target_os = "linux")]
        {
            let entry = self.entry(profile)?;
            return match entry.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
                Err(error) => Err(storage_error(profile, "delete", error)),
            };
        }

        #[cfg(not(target_os = "linux"))]
        {
            Err(unsupported_target_error("delete", profile))
        }
    }
}

#[cfg(target_os = "linux")]
fn decode_session(encoded: &str) -> Option<Session> {
    let (user_id, token) = encoded.split_once(':')?;
    Some(Session {
        user_id: UserId(user_id.parse().ok()?),
        token: token.into(),
    })
}

#[cfg(target_os = "linux")]
fn storage_error(profile: &ProfileId, operation: &str, error: keyring::Error) -> AppError {
    AppError::Storage(format!(
        "unable to {operation} credentials for profile {profile} in the OS credential store: {error}"
    ))
}

#[cfg(not(target_os = "linux"))]
fn unsupported_target_error(operation: &str, profile: &ProfileId) -> AppError {
    AppError::Storage(format!(
        "unable to {operation} credentials for profile {profile}: OS credential storage is unavailable on this unsupported target; refusing keyring's in-memory backend"
    ))
}
