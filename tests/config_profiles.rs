use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use lemex::profiles::KeyringCredentialStore;
use lemex::{
    AppConfig, AppError, ColorsConfig, HttpConfig, ProfileId, SecretString, Session, UserId,
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
    let path = std::env::temp_dir().join(format!("lemex-config-profiles-{unique}"));
    fs::create_dir(&path).unwrap();
    path
}

#[test]
fn log_path_resides_in_the_cache_directory() {
    let path = lemex::config::log_path();
    assert!(
        path.starts_with(lemex::config::cache_dir()),
        "the log file must live in the cache directory, got {}",
        path.display()
    );
    assert_eq!(
        path.file_name().and_then(|name| name.to_str()),
        Some("lemex.log")
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
    // Every content view the client opens interactively is a valid start
    // page, `:subscribed` included.
    let subscribed = "startup = 'subscribed'\n[[profiles]]\nid = 'main'\ninstance_url = 'https://example.test'\n";
    assert_eq!(
        AppConfig::from_toml(subscribed).unwrap().startup,
        "subscribed"
    );
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
fn http_instance_urls_require_explicit_opt_in() {
    // Credentials must not travel in cleartext by default: http:// instance
    // URLs are rejected unless the user explicitly sets allow_insecure_http.
    let rejected = "[[profiles]]\nid = 'main'\ninstance_url = 'http://example.test'\n";
    let error = AppConfig::from_toml(rejected)
        .expect_err("http instance URLs must be rejected without allow_insecure_http");
    assert!(
        error.to_string().contains("allow_insecure_http"),
        "the error must name the opt-in key, got {error}"
    );

    let accepted = "allow_insecure_http = true\n[[profiles]]\nid = 'main'\ninstance_url = 'http://example.test'\n";
    let config = AppConfig::from_toml(accepted).unwrap();
    assert!(config.allow_insecure_http);

    // Saving an http profile without the opt-in fails the same way; the flag
    // round-trips through to_toml.
    let encoded = config.to_toml().unwrap();
    assert!(encoded.contains("allow_insecure_http"));
    assert_eq!(AppConfig::from_toml(&encoded).unwrap(), config);
}

#[test]
fn default_collapsed_threads_round_trips_and_defaults_to_expanded() {
    let absent = AppConfig::from_toml(
        "[[profiles]]\nid = 'main'\ninstance_url = 'https://example.test'\n",
    )
    .unwrap();
    assert!(
        !absent.default_collapsed_threads,
        "threads open expanded unless the option says otherwise"
    );

    let source = "default_collapsed_threads = true\n[[profiles]]\nid = 'main'\ninstance_url = 'https://example.test'\n";
    let config = AppConfig::from_toml(source).unwrap();
    assert!(config.default_collapsed_threads);

    let encoded = config.to_toml().unwrap();
    assert!(encoded.contains("default_collapsed_threads"));
    assert_eq!(AppConfig::from_toml(&encoded).unwrap(), config);
}

#[test]
fn cache_size_defaults_to_a_bounded_cap() {
    // The feed cache must not grow without bound: the default config applies
    // a byte cap (drafts are never evicted; the cap only bounds cached feeds).
    let source = "[[profiles]]\nid = 'main'\ninstance_url = 'https://example.test'\n";
    let config = AppConfig::from_toml(source).unwrap();
    assert!(
        config.cache.max_size_bytes.is_some(),
        "the default cache must be bounded"
    );
    // An explicit value is preserved, and an explicit unlimited config stays
    // unlimited.
    let capped = "[[profiles]]\nid = 'main'\ninstance_url = 'https://example.test'\n[cache]\nmax_size_bytes = 1024\n";
    assert_eq!(
        AppConfig::from_toml(capped).unwrap().cache.max_size_bytes,
        Some(1024)
    );
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
    async fn site(&self, _: &ProfileContext) -> lemex::Result<SiteInfo> {
        Err(AppError::Network("unused".into()))
    }
    async fn feed(&self, _: &ProfileContext, _: FeedQuery) -> lemex::Result<Page<PostView>> {
        Err(AppError::Network("unused".into()))
    }
    async fn post(&self, _: &ProfileContext, _: PostId) -> lemex::Result<PostDetail> {
        Err(AppError::Network("unused".into()))
    }
    async fn comments(&self, _: &ProfileContext, _: PostId) -> lemex::Result<Vec<CommentView>> {
        Ok(Vec::new())
    }
    async fn login(&self, _: LoginRequest) -> lemex::Result<Session> {
        if self.failed.swap(false, Ordering::SeqCst) {
            Err(AppError::Authentication("invalid credentials".into()))
        } else {
            Ok(session("ok"))
        }
    }
    async fn mutate(&self, _: &ProfileContext, _: Mutation) -> lemex::Result<MutationResult> {
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
    let path = std::env::temp_dir().join(format!("lemex-logout-{}.toml", std::process::id()));
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

#[test]
fn colors_section_parses_round_trips_and_rejects_bad_values() {
    // Absent [colors]: the standard palette applies.
    let source = "[[profiles]]\nid = 'main'\ninstance_url = 'https://example.test'\n";
    let config = AppConfig::from_toml(source).unwrap();
    assert_eq!(config.colors, ColorsConfig::default());

    // Custom named and hex colors parse and round-trip.
    let custom = "[colors]\naccent = 'lightblue'\nsurface = '#1c1c1c'\ntext = 'lightcyan'\n[[profiles]]\nid = 'main'\ninstance_url = 'https://example.test'\n";
    let config = AppConfig::from_toml(custom).unwrap();
    assert_eq!(config.colors.accent, "lightblue");
    assert_eq!(config.colors.surface, "#1c1c1c");
    let encoded = config.to_toml().unwrap();
    assert_eq!(
        AppConfig::from_toml(&encoded).unwrap().colors,
        config.colors
    );

    // Resolving the palette yields the ratatui colors.
    let app_colors = lemex::AppColors::from_config(&config.colors);
    assert_eq!(app_colors.accent, ratatui::style::Color::LightBlue);
    assert_eq!(
        app_colors.surface,
        ratatui::style::Color::Rgb(0x1c, 0x1c, 0x1c)
    );
    assert_eq!(app_colors.text, ratatui::style::Color::LightCyan);

    // A typo is a configuration error, not a silent fallback.
    for bad in ["blurple", "#12345", "#gggggg"] {
        let source = format!(
            "[colors]\naccent = '{bad}'\n[[profiles]]\nid = 'main'\ninstance_url = 'https://example.test'\n"
        );
        assert!(
            matches!(
                AppConfig::from_toml(&source),
                Err(AppError::Configuration(_))
            ),
            "color {bad:?} must be rejected"
        );
    }
}

#[test]
fn http_section_parses_clamps_and_round_trips() {
    // Absent [http]: the 5/10/15 second defaults apply.
    let source = "[[profiles]]\nid = 'main'\ninstance_url = 'https://example.test'\n";
    let config = AppConfig::from_toml(source).unwrap();
    assert_eq!(config.http, HttpConfig::default());
    assert_eq!(config.http.connect_timeout, Duration::from_secs(5));
    assert_eq!(config.http.request_timeout, Duration::from_secs(10));
    assert_eq!(config.http.total_timeout, Duration::from_secs(15));

    // Explicit values parse and survive a save/load round trip (equality now
    // includes AppConfig.http).
    let custom = "[http]\nconnect_timeout_secs = 3\nrequest_timeout_secs = 8\ntotal_timeout_secs = 20\n[[profiles]]\nid = 'main'\ninstance_url = 'https://example.test'\n";
    let config = AppConfig::from_toml(custom).unwrap();
    assert_eq!(config.http.connect_timeout, Duration::from_secs(3));
    assert_eq!(config.http.request_timeout, Duration::from_secs(8));
    assert_eq!(config.http.total_timeout, Duration::from_secs(20));
    let encoded = config.to_toml().unwrap();
    assert_eq!(AppConfig::from_toml(&encoded).unwrap(), config);

    // Inverted orderings clamp into the invariant
    // connect <= request <= total; a value can only shrink.
    let inverted = "[http]\nconnect_timeout_secs = 99\nrequest_timeout_secs = 5\ntotal_timeout_secs = 15\n[[profiles]]\nid = 'main'\ninstance_url = 'https://example.test'\n";
    let config = AppConfig::from_toml(inverted).unwrap();
    assert_eq!(config.http.connect_timeout, Duration::from_secs(5));
    assert_eq!(config.http.request_timeout, Duration::from_secs(5));
    assert_eq!(config.http.total_timeout, Duration::from_secs(15));

    // A request deadline above the total clamps down to the total.
    let inverted_total = "[http]\nconnect_timeout_secs = 5\nrequest_timeout_secs = 99\ntotal_timeout_secs = 5\n[[profiles]]\nid = 'main'\ninstance_url = 'https://example.test'\n";
    let config = AppConfig::from_toml(inverted_total).unwrap();
    assert_eq!(config.http.connect_timeout, Duration::from_secs(5));
    assert_eq!(config.http.request_timeout, Duration::from_secs(5));
    assert_eq!(config.http.total_timeout, Duration::from_secs(5));
}

#[test]
fn http_zero_timeout_is_rejected() {
    // A zero deadline would make every request fail instantly; reject it
    // loudly instead of tolerating it.
    for key in [
        "connect_timeout_secs",
        "request_timeout_secs",
        "total_timeout_secs",
    ] {
        let source = format!(
            "[http]\n{key} = 0\n[[profiles]]\nid = 'main'\ninstance_url = 'https://example.test'\n"
        );
        assert!(
            matches!(
                AppConfig::from_toml(&source),
                Err(AppError::Configuration(_))
            ),
            "[http] {key} = 0 must be rejected"
        );
    }
    // Unknown keys are rejected (the raw layer is strict about unknown fields).
    let unknown = "[http]\nconnect_timeout_secs = 5\nconnect_timeout = 3\n[[profiles]]\nid = 'main'\ninstance_url = 'https://example.test'\n";
    assert!(
        matches!(
            AppConfig::from_toml(unknown),
            Err(AppError::Configuration(_))
        ),
        "an unknown [http] key must be rejected"
    );
}
