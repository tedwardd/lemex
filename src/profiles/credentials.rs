use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};

use async_trait::async_trait;
use keyring::Entry;
use keyring::mock::MockCredential;

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

/// OS credential-store backed session storage: the platform's native secure
/// store (Linux Secret Service, macOS Keychain, Windows Credential Manager).
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

    fn entry(&self, profile: &ProfileId) -> Result<Entry> {
        let entry = Entry::new(&self.service, &profile.to_string())
            .map_err(|error| storage_error(profile, "open", error))?;
        // keyring silently falls back to its in-memory mock store whenever
        // no platform store is available (no Secret Service/keyutils on
        // Linux, an unavailable Keychain on macOS). Refuse it: a session
        // that only lives in memory is not secure storage and would vanish
        // at exit without warning.
        if entry.get_credential().is::<MockCredential>() {
            return Err(AppError::Storage(format!(
                "the OS credential store is unavailable for profile {profile}: keyring fell back to its in-memory mock store, which levim refuses. On Linux start a Secret Service provider (gnome-keyring or keepassxc); on macOS unlock the Keychain."
            )));
        }
        Ok(entry)
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
        let store = self.clone();
        let profile = profile.clone();
        tokio::task::spawn_blocking(move || {
            let entry = store.entry(&profile)?;
            let encoded = match entry.get_password() {
                Ok(value) => value,
                Err(keyring::Error::NoEntry) => return Ok(None),
                Err(error) => return Err(storage_error(&profile, "read", error)),
            };

            decode_session(&encoded).map(Some).ok_or_else(|| {
                AppError::Storage(format!(
                    "keyring credential for profile {profile} is malformed; sign in again"
                ))
            })
        })
        .await
        .map_err(|error| AppError::Storage(format!("keyring task failed: {error}")))?
    }

    async fn put_session(&self, profile: &ProfileId, session: &Session) -> Result<()> {
        let store = self.clone();
        let profile = profile.clone();
        let session = session.clone();
        tokio::task::spawn_blocking(move || {
            let entry = store.entry(&profile)?;
            let encoded = format!("{}:{}", session.user_id.0, session.token.expose_secret());
            entry
                .set_password(&encoded)
                .map_err(|error| storage_error(&profile, "write", error))
        })
        .await
        .map_err(|error| AppError::Storage(format!("keyring task failed: {error}")))?
    }

    async fn delete_session(&self, profile: &ProfileId) -> Result<()> {
        let store = self.clone();
        let profile = profile.clone();
        tokio::task::spawn_blocking(move || {
            let entry = store.entry(&profile)?;
            match entry.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
                Err(error) => Err(storage_error(&profile, "delete", error)),
            }
        })
        .await
        .map_err(|error| AppError::Storage(format!("keyring task failed: {error}")))?
    }
}

fn decode_session(encoded: &str) -> Option<Session> {
    let (user_id, token) = encoded.split_once(':')?;
    Some(Session {
        user_id: UserId(user_id.parse().ok()?),
        token: token.into(),
    })
}

fn storage_error(profile: &ProfileId, operation: &str, error: keyring::Error) -> AppError {
    AppError::Storage(format!(
        "unable to {operation} credentials for profile {profile} in the OS credential store: {error}"
    ))
}
