use url::Url;

/// Stable identifier for a configured profile.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ProfileId(pub String);

impl From<String> for ProfileId {
    fn from(value: String) -> Self { Self(value) }
}

impl From<&str> for ProfileId {
    fn from(value: &str) -> Self { Self(value.to_owned()) }
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

/// A profile together with the current authentication state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActiveProfile {
    pub profile: Profile,
    pub authenticated: bool,
}
