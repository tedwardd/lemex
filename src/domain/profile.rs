use std::fmt;

use secrecy::ExposeSecret;
use url::Url;

/// A secret string that is explicitly exposed only through `expose_secret`.
pub struct SecretString(secrecy::SecretString);

impl SecretString {
    pub fn expose_secret(&self) -> &str {
        self.0.expose_secret()
    }
}

impl From<String> for SecretString {
    fn from(value: String) -> Self {
        Self(secrecy::SecretString::from(value))
    }
}

impl From<&str> for SecretString {
    fn from(value: &str) -> Self {
        Self(secrecy::SecretString::from(value))
    }
}

impl Clone for SecretString {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl PartialEq for SecretString {
    fn eq(&self, other: &Self) -> bool {
        self.expose_secret() == other.expose_secret()
    }
}

impl Eq for SecretString {}

impl fmt::Debug for SecretString {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[REDACTED]")
    }
}

impl fmt::Display for SecretString {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[REDACTED]")
    }
}

impl ExposeSecret<str> for SecretString {
    fn expose_secret(&self) -> &str {
        self.expose_secret()
    }
}
/// Stable identifier for a configured profile.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ProfileId(pub String);

impl From<String> for ProfileId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for ProfileId {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

impl std::fmt::Display for ProfileId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Non-secret metadata describing a Lemmy instance/account pairing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Profile {
    pub id: ProfileId,
    pub instance_url: Url,
    pub account_label: Option<String>,
}

/// Authentication state kept separately from non-secret profile metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Session {
    pub token: SecretString,
    pub user_id: crate::domain::UserId,
}

/// A profile together with the optional session used for its requests.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProfileContext {
    pub profile: Profile,
    pub session: Option<Session>,
}

/// A profile together with the current authentication state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActiveProfile {
    pub profile: Profile,
    pub authenticated: bool,
}
