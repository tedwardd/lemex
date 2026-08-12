mod credentials;
mod store;

pub use credentials::{CredentialStore, KeyringCredentialStore, MemoryCredentialStore};
pub use store::{default_store, load, save, ProfileStore};

use url::Url;

use crate::{
    api::{LemmyApi, LoginRequest},
    domain::{ProfileId, Session},
    error::{AppError, Result},
};

/// Validate that an instance URL is usable as a login target: an http(s)
/// scheme, a resolvable host, and no embedded credentials. The same rules the
/// configuration parser enforces, applied before any network activity.
pub fn validate_instance(url: &Url) -> Result<()> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err(AppError::Configuration(format!(
            "instance URL must use http or https; got {}",
            url.scheme()
        )));
    }
    if url.host_str().is_none() {
        return Err(AppError::Configuration(format!(
            "instance URL must include a host: {url}"
        )));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(AppError::Configuration(
            "instance URL must not contain credentials".to_owned(),
        ));
    }
    Ok(())
}

/// Log in through the Lemmy API and persist the session in the OS credential
/// store. The session is stored only after the API call succeeds; any
/// failure leaves the credential store untouched and the caller
/// unauthenticated. The plaintext password never leaves `LoginRequest`.
pub async fn login(
    api: &dyn LemmyApi,
    credentials: &dyn CredentialStore,
    request: LoginRequest,
) -> Result<Session> {
    validate_instance(&request.instance_url)?;
    let profile = request.profile.clone();
    let session = api.login(request).await?;
    credentials.put_session(&profile, &session).await?;
    Ok(session)
}

/// Log out: remove only the session for `profile` from the credential store.
/// Non-secret profile metadata lives in `profiles` and is deliberately
/// untouched — logout is destructive to the session alone.
pub async fn logout(
    _profiles: &ProfileStore,
    credentials: &dyn CredentialStore,
    profile: &ProfileId,
) -> Result<()> {
    credentials.delete_session(profile).await
}
