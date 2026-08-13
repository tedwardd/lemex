use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use levim::profiles::KeyringCredentialStore;
use levim::{
    AppConfig, AppError, ProfileId, SecretString, Session, UserId,
    api::{
        CommentView, FeedQuery, LemmyApi, LoginRequest, MutationResult, Page, PostDetail, PostView,
        SiteInfo,
    },
    domain::{Mutation, PostId, Profile, ProfileContext},
    profiles::{CredentialStore, MemoryCredentialStore, ProfileStore, login, logout},
};
use url::Url;

fn session(token: &str) -> Session {
    Session {
        token: SecretString::from(token),
        user_id: UserId(1),
    }
}

#[tokio::test]
async fn sessions_are_keyed_by_profile_id() {
    let store = MemoryCredentialStore::default();
    store
        .put_session(&ProfileId::from("one"), &session("token-one"))
        .await
        .unwrap();
    store
        .put_session(&ProfileId::from("two"), &session("token-two"))
        .await
        .unwrap();

    assert_eq!(
        store
            .get_session(&ProfileId::from("one"))
            .await
            .unwrap()
            .unwrap()
            .token
            .expose_secret(),
        "token-one"
    );
    assert_eq!(
        store
            .get_session(&ProfileId::from("two"))
            .await
            .unwrap()
            .unwrap()
            .token
            .expose_secret(),
        "token-two"
    );
}

#[tokio::test]
async fn keyring_store_refuses_the_in_memory_mock_backend() {
    // keyring falls back to its mock store when no platform store applies;
    // the client must refuse it so sessions are never silently stored in
    // memory. Force the mock builder to exercise the refusal deterministically.
    keyring::set_default_credential_builder(keyring::mock::default_credential_builder());
    let store = KeyringCredentialStore::default();
    let error = store
        .get_session(&ProfileId::from("mock-target"))
        .await
        .unwrap_err();
    let message = format!("{error}");
    assert!(
        message.contains("mock"),
        "the mock store must be refused, got: {message}"
    );
    assert!(
        message.contains("credential store is unavailable"),
        "the refusal must explain the store is unavailable, got: {message}"
    );
}

#[test]
fn session_debug_output_does_not_include_token() {
    let value = format!("{:?}", session("do-not-log").token);
    assert!(!value.contains("do-not-log"));
    let display = format!("{}", session("do-not-log").token);
    assert!(!display.contains("do-not-log"));
}

struct CurrentDirGuard(PathBuf);

impl CurrentDirGuard {
    fn enter(path: &Path) -> Self {
        let previous = std::env::current_dir().unwrap();
        std::env::set_current_dir(path).unwrap();
        Self(previous)
    }
}

impl Drop for CurrentDirGuard {
    fn drop(&mut self) {
        std::env::set_current_dir(&self.0).unwrap();
    }
}

fn temporary_directory() -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!("levim-config-profiles-{unique}"));
    fs::create_dir(&path).unwrap();
    path
}

#[test]
fn log_path_resides_in_the_cache_directory() {
    let path = levim::config::log_path();
    assert!(
        path.starts_with(levim::config::cache_dir()),
        "the log file must live in the cache directory, got {}",
        path.display()
    );
    assert_eq!(
        path.file_name().and_then(|name| name.to_str()),
        Some("levim.log")
    );
}

#[test]
fn starter_config_has_one_valid_profile_and_round_trips() {
    let starter = AppConfig::starter();
    assert_eq!(starter.profiles.len(), 1);
    let profile = &starter.profiles[0];
    assert_eq!(profile.id, ProfileId::from("main"));
    assert!(matches!(profile.instance_url.scheme(), "https"));
    assert!(profile.instance_url.host_str().is_some());
    assert!(profile.account_label.is_none());

    // The starter must serialize to a file the loader accepts unchanged, so
    // the first-run write can never produce a config the client rejects.
    let directory = temporary_directory();
    let path = directory.join("config.toml");
    starter.write_atomic(&path).unwrap();
    assert_eq!(AppConfig::load(&path).unwrap(), starter);
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn startup_action_round_trips_and_normalizes_colon() {
    let source =
        "startup = ':feed'\n[[profiles]]\nid = 'main'\ninstance_url = 'https://example.test'\n";
    let config = AppConfig::from_toml(source).unwrap();
    assert_eq!(config.startup, "feed");
    let encoded = config.to_toml().unwrap();
    assert_eq!(AppConfig::from_toml(&encoded).unwrap().startup, "feed");
    let search = "startup = 'search rust'\n[[profiles]]\nid = 'main'\ninstance_url = 'https://example.test'\n";
    assert_eq!(AppConfig::from_toml(search).unwrap().startup, "search rust");
}

#[test]
fn empty_startup_action_is_allowed_and_invalid_ones_are_rejected() {
    let empty = "startup = ''\n[[profiles]]\nid = 'main'\ninstance_url = 'https://example.test'\n";
    assert_eq!(AppConfig::from_toml(empty).unwrap().startup, "");
    for bad in ["bogus", "feed extra", "search", "community"] {
        let source = format!(
            "startup = '{bad}'\n[[profiles]]\nid = 'main'\ninstance_url = 'https://example.test'\n"
        );
        assert!(
            matches!(
                AppConfig::from_toml(&source),
                Err(AppError::Configuration(_))
            ),
            "startup {bad:?} must be rejected"
        );
    }
}

#[test]
fn config_round_trips_non_secret_profile_metadata() {
    let source = "[[profiles]]\nid = 'main'\ninstance_url = 'https://example.test'\naccount_label = 'primary'\n";
    let config = AppConfig::from_toml(source).unwrap();
    let encoded = config.to_toml().unwrap();
    assert_eq!(AppConfig::from_toml(&encoded).unwrap(), config);
}

#[test]
fn duplicate_profile_ids_are_rejected() {
    let source = "[[profiles]]\nid = 'main'\ninstance_url = 'https://one.test'\n[[profiles]]\nid = 'main'\ninstance_url = 'https://two.test'\n";
    assert!(matches!(
        AppConfig::from_toml(source),
        Err(AppError::Configuration(_))
    ));
}

#[test]
fn credential_like_fields_are_rejected() {
    let source =
        "[[profiles]]\nid = 'main'\ninstance_url = 'https://example.test'\npassword = 'secret'\n";
    assert!(matches!(
        AppConfig::from_toml(source),
        Err(AppError::Configuration(_))
    ));
}

#[test]
fn profile_only_config_preserves_media_defaults() {
    let source = "[[profiles]]\nid = 'main'\ninstance_url = 'https://example.test'\n";
    let config = AppConfig::from_toml(source).unwrap();

    assert!(config.media.mailcap_enabled);
    assert_eq!(config.media.collision_policy, "prompt");
}

#[test]
fn deprecated_kitty_enabled_key_is_accepted_and_ignored() {
    // Configs written before inline kitty rendering was removed still carry
    // `kitty_enabled`; they must keep parsing (the raw layer is strict about
    // unknown fields) while the value has no effect on the domain config.
    let source = "[media]\nkitty_enabled = true\n[[profiles]]\nid = 'main'\ninstance_url = 'https://example.test'\n";
    let config = AppConfig::from_toml(source).unwrap();
    assert!(config.media.mailcap_enabled, "media defaults still apply");
    let encoded = config.to_toml().unwrap();
    assert!(
        !encoded.contains("kitty_enabled"),
        "the deprecated key must not be re-emitted"
    );
    assert_eq!(AppConfig::from_toml(&encoded).unwrap(), config);
}

#[test]
fn relative_config_path_writes_and_reloads() {
    let directory = temporary_directory();
    {
        let _current_directory = CurrentDirGuard::enter(&directory);
        let path = Path::new("config.toml");
        let config = AppConfig::from_toml(
            "[[profiles]]\nid = 'main'\ninstance_url = 'https://example.test'\n",
        )
        .unwrap();

        config.write_atomic(path).unwrap();

        assert_eq!(AppConfig::load(path).unwrap(), config);
    }
    fs::remove_dir_all(directory).unwrap();
}

fn profile(id: &str) -> Profile {
    Profile {
        id: ProfileId::from(id),
        instance_url: Url::parse("https://example.test/").unwrap(),
        account_label: Some(id.into()),
    }
}

fn login_request() -> LoginRequest {
    LoginRequest {
        profile: ProfileId::from("main"),
        instance_url: Url::parse("https://example.test/").unwrap(),
        username: "alice".into(),
        password: SecretString::from("hunter2"),
    }
}

#[derive(Default)]
struct FailOnceLoginApi {
    failed: AtomicBool,
}

impl FailOnceLoginApi {
    fn fail_login_once(&self) {
        self.failed.store(true, Ordering::SeqCst);
    }
}

#[async_trait]
impl LemmyApi for FailOnceLoginApi {
    async fn site(&self, _: &ProfileContext) -> levim::Result<SiteInfo> {
        Err(AppError::Network("unused".into()))
    }
    async fn feed(&self, _: &ProfileContext, _: FeedQuery) -> levim::Result<Page<PostView>> {
        Err(AppError::Network("unused".into()))
    }
    async fn post(&self, _: &ProfileContext, _: PostId) -> levim::Result<PostDetail> {
        Err(AppError::Network("unused".into()))
    }
    async fn comments(&self, _: &ProfileContext, _: PostId) -> levim::Result<Vec<CommentView>> {
        Ok(Vec::new())
    }
    async fn login(&self, _: LoginRequest) -> levim::Result<Session> {
        if self.failed.swap(false, Ordering::SeqCst) {
            Err(AppError::Authentication("invalid credentials".into()))
        } else {
            Ok(session("ok"))
        }
    }
    async fn mutate(&self, _: &ProfileContext, _: Mutation) -> levim::Result<MutationResult> {
        Err(AppError::Network("unused".into()))
    }
}

fn login_test_dependencies() -> (FailOnceLoginApi, MemoryCredentialStore) {
    (
        FailOnceLoginApi::default(),
        MemoryCredentialStore::default(),
    )
}

#[tokio::test]
async fn login_stores_session_only_after_api_success() {
    let (api, credentials) = login_test_dependencies();
    api.fail_login_once();
    let result = login(&api, &credentials, login_request()).await;
    assert!(result.is_err());
    assert!(credentials.all().is_empty());
}

fn profile_test_dependencies() -> (ProfileStore, MemoryCredentialStore) {
    let path = std::env::temp_dir().join(format!("levim-logout-{}.toml", std::process::id()));
    let _ = std::fs::remove_file(&path);
    (ProfileStore::new(path), MemoryCredentialStore::default())
}

#[tokio::test]
async fn logout_removes_session_and_keeps_non_secret_profile_metadata() {
    let (profiles, credentials) = profile_test_dependencies();
    profiles.create(profile("main")).unwrap();
    credentials
        .put_session(&ProfileId::from("main"), &session("secret"))
        .await
        .unwrap();
    logout(&profiles, &credentials, &ProfileId::from("main"))
        .await
        .unwrap();
    assert!(
        credentials
            .get_session(&ProfileId::from("main"))
            .await
            .unwrap()
            .is_none()
    );
    assert!(profiles.get(&ProfileId::from("main")).is_ok());
}
