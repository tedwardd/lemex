//! End-to-end smoke scenarios.
//!
//! These tests drive the real application through its public seams — input
//! engine, command parsing, dispatch, fixture-backed HTTP adapter, SQLite
//! cache, credential store, and download manager — plus the compiled binary
//! itself for the non-interactive `--help` path and a PTY-backed launch/quit
//! cycle. `FixtureApp` redirects the process XDG environment while
//! constructing each app; that window is serialized process-wide by a static
//! mutex in the harness, so the suite is safe to run in parallel (CI runs it
//! serially for deterministic ordering).

mod support;

use std::{
    collections::HashMap,
    io::Write,
    process::{Command as ProcessCommand, Stdio},
    sync::atomic::Ordering,
    time::Duration,
};

use lemmy::{
    api::fixtures::{
        anonymous_context, authenticated_context, fixture_api_requiring_auth,
        fixture_api_with_body, fixture_api_with_pages, fixture_api_with_status,
        fixture_api_with_transient_failures, login_fixture_api,
    },
    app::AppAction,
    config::MediaConfig,
    domain::{DownloadStatus, MediaRef, PostId, Profile, ProfileContext, ProfileId},
    input::{Command, InputEngine},
    media::{MediaHandler, MediaPolicyConfig},
};
use url::Url;

use support::{FixtureApp, ScratchDir, key, post_view, spawn_http_server};

fn profile_context(instance_url: Url) -> ProfileContext {
    ProfileContext {
        profile: Profile {
            id: ProfileId::from("fixture"),
            instance_url,
            account_label: None,
        },
        session: None,
    }
}

/// A fixture body that serves both a post and one thread comment.
const POST_THREAD_BODY: &str = r#"{"post_view":{"post":{"id":1,"name":"Fixture post","body":"Fixture body","url":"https://example.com/fixture","community_id":1,"creator_id":1,"published":"2026-01-01T00:00:00Z","score":3},"counts":{"score":3,"comments":1}},"comments":[{"comment":{"id":1,"post_id":1,"content":"Fixture comment","creator_id":1},"creator":{"id":1,"name":"alice"},"counts":{"score":1}}]}"#;

/// A fixture body that reports a successful post mutation.
const MUTATION_BODY: &str = r#"{"post_view":{"post":{"id":1,"name":"Voted post","body":null,"community_id":1,"creator_id":1,"score":5},"counts":{"score":5,"comments":0}}}"#;

fn feed_fixture_body() -> String {
    std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/fixtures/lemmy/feed.json"
    ))
    .expect("feed fixture exists")
}

/// Launch path: `lemmy --help` reports commands and exits 0 without touching
/// the TUI (no config, no runtime, no alternate screen).
#[test]
fn help_flag_reports_commands_and_exits_without_entering_tui() {
    let binary = env!("CARGO_BIN_EXE_lemmy");
    for flag in ["--help", "-h"] {
        let output = ProcessCommand::new(binary)
            .arg(flag)
            .output()
            .expect("run lemmy --help");
        assert!(
            output.status.success(),
            "`lemmy {flag}` must exit successfully, got {:?}",
            output.status
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("Usage"), "`lemmy {flag}` must print usage");
        assert!(
            stdout.contains(":help"),
            "`lemmy {flag}` must report commands"
        );
        assert!(
            stdout.contains(":profile"),
            "`lemmy {flag}` must report profile commands"
        );
    }
}

/// Launch and terminal restoration: the compiled binary starts the TUI under
/// a real PTY (via `script(1)`), renders the session header, quits on `q`,
/// and exits 0. `timeout` bounds a hung process.
#[cfg(target_os = "linux")]
#[test]
fn binary_launches_tui_and_quits_cleanly_restoring_terminal() {
    let binary = env!("CARGO_BIN_EXE_lemmy");
    let scratch = ScratchDir::new("tui");
    let config_home = scratch.path.join("config");
    let cache_home = scratch.path.join("cache");
    let config_dir = config_home.join("lemmy");
    std::fs::create_dir_all(&config_dir).expect("create config directory");
    std::fs::write(
        config_dir.join("config.toml"),
        "[[profiles]]\nid = 'smoke'\ninstance_url = 'http://127.0.0.1/'\n",
    )
    .expect("write smoke config");

    let typescript = scratch.path.join("typescript.log");
    let command = format!(
        "stty rows 24 cols 80; XDG_CONFIG_HOME={} XDG_CACHE_HOME={} timeout 25 {} ",
        config_home.display(),
        cache_home.display(),
        binary
    );
    let mut child = ProcessCommand::new("script")
        .args(["-qec", &command])
        .arg(&typescript)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap_or_else(|error| panic!("spawn script(1) for the TUI smoke: {error}"));

    // Give the binary time to parse config, open the cache, probe the keyring
    // and enter the alternate screen. Sending `q` plus a carriage return also
    // covers the case where the key lands before raw mode: the cooked line
    // discipline delivers `q` and Enter together, which still quits.
    std::thread::sleep(Duration::from_millis(2000));
    let stdin = child.stdin.as_mut().expect("script stdin");
    stdin.write_all(b"q\r").expect("write quit key");
    stdin.flush().expect("flush quit key");

    let status = child.wait().expect("wait for the TUI process");
    assert!(
        status.success(),
        "the TUI must exit successfully after `q` (got {status:?}); the terminal was not restored cleanly"
    );

    let output = std::fs::read(&typescript).unwrap_or_default();
    let output = String::from_utf8_lossy(&output);
    assert!(
        output.contains("Profile:") || output.contains("NORMAL"),
        "the TUI must render the session header before quitting; script log was empty of UI text"
    );
}

/// Launch, feed load, and Vim feed navigation: `:feed` populates the list,
/// `j`/`k` move the selection, and counted motions are honored.
#[test]
fn feed_loads_through_fixture_and_vim_keys_navigate() {
    let mut app = FixtureApp::new("feed");
    let mut engine = InputEngine::new();

    app.command(&mut engine, "feed").expect("load the feed");
    assert_eq!(
        app.app.state.view.posts.len(),
        2,
        "fixture feed has two posts"
    );
    assert_eq!(
        app.app.state.view.selected,
        Some(0),
        "feed selects the first post"
    );
    assert!(app.app.state.status.message.contains("feed loaded"));

    app.press(&mut engine, key('j')).expect("move down");
    assert_eq!(app.app.state.selected_index(), 1);
    app.press(&mut engine, key('k')).expect("move up");
    assert_eq!(app.app.state.selected_index(), 0);

    // Counted motion: `2j` clamps to the last post.
    app.press(&mut engine, key('2')).expect("count prefix");
    app.press(&mut engine, key('j')).expect("counted move down");
    assert_eq!(app.app.state.selected_index(), 1);

    // The engine returns to normal mode after commands.
    assert_eq!(engine.mode(), lemmy::input::Mode::Normal);
}

/// Pagination: the first page carries an opaque `next_page` cursor, `>`
/// sends it back as `page_cursor` to replace the list with the following
/// page, and `<` restores the previous page (the Lemmy 0.19+ cursor
/// protocol).
#[test]
fn page_flips_replace_the_list_and_go_back() {
    let runtime = support::runtime();
    let first = r#"{"posts":[{"post":{"id":1,"name":"First page","body":null,"community_id":1,"creator_id":1,"published":"2026-01-01T00:00:00Z"},"counts":{"score":1,"comments":0}}],"next_page":"P2"}"#;
    let next = r#"{"posts":[{"post":{"id":2,"name":"Second page","body":null,"community_id":1,"creator_id":1,"published":"2026-01-02T00:00:00Z"},"counts":{"score":1,"comments":0}}],"next_page":null}"#;
    let api = support::api(&runtime, || fixture_api_with_pages(first, next));
    let mut app = FixtureApp::with_runtime(
        runtime,
        "page",
        api,
        anonymous_context(),
        MediaConfig::default(),
        &[],
    );
    let mut engine = InputEngine::new();

    app.command(&mut engine, "feed")
        .expect("load the first page");
    assert_eq!(app.app.state.view.posts.len(), 1);
    assert_eq!(
        app.app.state.view.next_page.as_deref(),
        Some("P2"),
        "the opaque next_page cursor must survive normalization"
    );

    app.press(&mut engine, key('>')).expect("flip forward");
    assert_eq!(
        app.app.state.view.posts.len(),
        1,
        "the next page replaces the current list"
    );
    assert_eq!(app.app.state.view.posts[0].id, PostId(2));
    assert!(
        app.app.state.view.next_page.is_none(),
        "the last page carries no further cursor"
    );
    assert!(app.app.state.status.message.contains("next page loaded"));

    app.press(&mut engine, key('<')).expect("flip backward");
    assert_eq!(
        app.app.state.view.posts.len(),
        1,
        "the previous page replaces the current list"
    );
    assert_eq!(app.app.state.view.posts[0].id, PostId(1));
    assert_eq!(
        app.app.state.view.next_page.as_deref(),
        Some("P2"),
        "back on the first page the forward cursor is available again"
    );
    assert!(
        app.app
            .state
            .status
            .message
            .contains("previous page loaded")
    );
}

/// Post/thread opening: opening a selected post fetches the post detail and
/// then the thread comments through the fixture adapter, and `Esc` (back)
/// returns to the intact feed.
#[test]
fn opening_post_shows_thread_and_back_preserves_feed_position() {
    let runtime = support::runtime();
    let api = support::api(&runtime, || fixture_api_with_body(POST_THREAD_BODY));
    let mut app = FixtureApp::with_runtime(
        runtime,
        "post",
        api,
        anonymous_context(),
        MediaConfig::default(),
        &[],
    );
    app.app.state.view.posts = vec![
        post_view(1, "Fixture post", None),
        post_view(2, "Second post", None),
    ];
    app.app.state.view.selected = Some(0);

    app.dispatch(AppAction::OpenSelected)
        .expect("open selected post");
    let detail = app
        .app
        .state
        .view
        .detail
        .clone()
        .expect("post detail loads");
    assert_eq!(detail.post.id, PostId(1));
    assert_eq!(
        detail.comments.len(),
        1,
        "thread comment arrives via the comments fetch"
    );
    assert_eq!(detail.comments[0].content, "Fixture comment");
    assert!(app.app.state.status.message.contains("comments loaded"));

    app.dispatch(AppAction::Back).expect("back to the feed");
    assert!(
        app.app.state.view.detail.is_none(),
        "back closes the thread"
    );
    assert_eq!(
        app.app.state.view.posts.len(),
        2,
        "feed survives back navigation"
    );
    assert_eq!(
        app.app.state.selected_index(),
        0,
        "feed position is preserved"
    );
}

/// Draft preservation: drafts persist to the on-disk cache, survive a
/// validation failure, and remain after the application is torn down and
/// restarted against the same cache.
#[test]
fn drafts_persist_across_restart_and_validation_failure_keeps_draft() {
    let scratch = ScratchDir::new("drafts");

    let first_runtime = support::runtime();
    let first_api = support::api(&first_runtime, || fixture_api_with_body("{}"));
    let mut first = FixtureApp::in_scratch(
        first_runtime,
        &scratch,
        first_api,
        anonymous_context(),
        MediaConfig::default(),
        &[],
    );
    let draft = first.app.state.begin_post_draft();
    first
        .app
        .state
        .update_draft(&draft.id, "My title\nMy body")
        .expect("update draft");

    // A validation failure must not destroy the draft.
    let edit = first.app.state.begin_edit_post_draft(PostId(5));
    first
        .app
        .state
        .update_draft(&edit.id, "5\n")
        .expect("update edit draft");
    first
        .dispatch(AppAction::SubmitDraft(edit.id.clone()))
        .expect("submit invalid edit draft");
    assert_eq!(
        first.app.state.status.error.as_deref(),
        Some("invalid command: post title is required")
    );
    assert!(
        first.app.state.draft(edit.id).is_some(),
        "a failed submission keeps the draft"
    );
    assert!(
        first.cache_dir.join("cache.sqlite3").is_file(),
        "the harness keeps drafts in a real on-disk cache"
    );
    drop(first);

    let second_runtime = support::runtime();
    let second_api = support::api(&second_runtime, || fixture_api_with_body("{}"));
    let second = FixtureApp::in_scratch(
        second_runtime,
        &scratch,
        second_api,
        anonymous_context(),
        MediaConfig::default(),
        &[],
    );
    let restored = second.app.state.drafts.all();
    assert!(
        restored
            .iter()
            .any(|candidate| candidate.id == draft.id && candidate.content == "My title\nMy body"),
        "the draft must survive an application restart on the same cache"
    );
}

/// Profile switching: listing marks the active profile, switching is a hard
/// context transition that clears transient view state, and whoami reports
/// the anonymous session after a switch without one.
#[test]
fn profile_list_switch_and_whoami() {
    let profiles = vec![
        Profile {
            id: ProfileId::from("fixture"),
            instance_url: Url::parse("http://127.0.0.1/").unwrap(),
            account_label: Some("fixture".into()),
        },
        Profile {
            id: ProfileId::from("other"),
            instance_url: Url::parse("https://other.example/").unwrap(),
            account_label: None,
        },
    ];
    let runtime = support::runtime();
    let api = support::api(&runtime, || fixture_api_with_body("{}"));
    let mut app = FixtureApp::with_runtime(
        runtime,
        "profiles",
        api,
        anonymous_context(),
        MediaConfig::default(),
        &profiles,
    );
    let mut engine = InputEngine::new();

    app.command(&mut engine, "profile").expect("list profiles");
    assert!(
        app.app.state.status.message.contains("fixture (active)"),
        "listing marks the active profile: {}",
        app.app.state.status.message
    );
    assert!(app.app.state.status.message.contains("other"));

    app.command(&mut engine, "profile other")
        .expect("switch profile");
    assert_eq!(app.app.state.active.profile.id, ProfileId::from("other"));
    assert!(
        app.app.state.view.posts.is_empty(),
        "switching clears stale transient feed state"
    );
    assert!(app.app.state.view.compose.is_empty());

    app.command(&mut engine, "whoami").expect("whoami");
    assert!(
        app.app.state.status.message.contains("anonymous"),
        "a profile without a stored session reports anonymous"
    );

    app.command(&mut engine, "profile").expect("list again");
    assert!(
        app.app.state.status.message.contains("other (active)"),
        "listing after a switch marks the new active profile"
    );
}

/// Authentication: `:login` stores the session in the credential store only
/// after success, clears the compose buffer, and never lets the password
/// appear in application state or status output.
#[test]
fn login_stores_session_only_after_success_and_redacts_password() {
    let runtime = support::runtime();
    let api = support::api(&runtime, || login_fixture_api("").0);
    let mut app = FixtureApp::with_runtime(
        runtime,
        "login",
        api,
        profile_context(Url::parse("http://127.0.0.1/").unwrap()),
        MediaConfig::default(),
        &[],
    );
    let mut engine = InputEngine::new();

    app.command(&mut engine, "login alice hunter2")
        .expect("log in");
    assert!(
        app.config_path.is_file(),
        "the harness config file exists on disk"
    );
    let stored = app
        .credentials
        .all()
        .get(&ProfileId::from("fixture"))
        .cloned()
        .expect("session stored in the credential store");
    assert_eq!(stored.token.expose_secret(), "fixture-jwt");
    assert!(app.app.state.active.session.is_some());
    assert!(
        app.app.state.view.compose.is_empty(),
        "password never lingers in the compose buffer"
    );
    assert!(app.app.state.status.message.contains("logged in as user 1"));

    let rendered = format!("{:?}", app.app.state.active);
    assert!(
        !rendered.contains("hunter2"),
        "password must not appear in state"
    );
    assert!(
        !rendered.contains("fixture-jwt"),
        "session token must not appear in state"
    );

    // A failed login must not store or overwrite a session.
    let failing_runtime = support::runtime();
    let failing_api = support::api(&failing_runtime, || {
        fixture_api_with_status("/api/v3/user/login", 401)
    });
    let mut failing = FixtureApp::with_runtime(
        failing_runtime,
        "login-fail",
        failing_api,
        anonymous_context(),
        MediaConfig::default(),
        &[],
    );
    let mut failing_engine = InputEngine::new();
    failing
        .command(&mut failing_engine, "login alice wrongpass")
        .expect("attempt failed login");
    assert!(failing.app.state.status.error.is_some());
    assert!(
        failing.credentials.all().is_empty(),
        "no session may be stored after a failed login"
    );
    assert!(failing.app.state.view.compose.is_empty());
    assert!(
        !format!("{:?}", failing.app.state.status).contains("wrongpass"),
        "the password must not be echoed in status"
    );
}

/// Authenticated mutation: with a session the fixture server accepts the
/// bearer-authenticated mutation and the response updates the view; without
/// one the server rejects the request and the app surfaces the authentication
/// error instead of applying a result.
#[test]
fn authenticated_mutation_succeeds_only_with_session() {
    let runtime = support::runtime();
    let (api, requests) = support::api(&runtime, || fixture_api_requiring_auth(MUTATION_BODY));
    let mut authenticated = FixtureApp::with_runtime(
        runtime,
        "mutation-auth",
        api,
        authenticated_context(),
        MediaConfig::default(),
        &[],
    );
    authenticated.app.state.view.posts = vec![post_view(1, "Target post", None)];
    authenticated.app.state.view.selected = Some(0);
    authenticated
        .dispatch(AppAction::Input(Command::SubmitLine("vote 1".into())))
        .expect("vote with a session");
    assert_eq!(
        requests.load(Ordering::SeqCst),
        1,
        "exactly one authenticated request"
    );
    assert!(authenticated.app.state.status.message.contains("saved"));
    assert_eq!(
        authenticated.app.state.view.posts[0].score, 5,
        "the mutation response must update the view"
    );

    let anonymous_runtime = support::runtime();
    let (anonymous_api, anonymous_requests) = support::api(&anonymous_runtime, || {
        fixture_api_requiring_auth(MUTATION_BODY)
    });
    let mut anonymous = FixtureApp::with_runtime(
        anonymous_runtime,
        "mutation-anon",
        anonymous_api,
        anonymous_context(),
        MediaConfig::default(),
        &[],
    );
    anonymous.app.state.view.posts = vec![post_view(1, "Target post", None)];
    anonymous.app.state.view.selected = Some(0);
    anonymous
        .dispatch(AppAction::Input(Command::SubmitLine("vote 1".into())))
        .expect("vote without a session");
    assert_eq!(
        anonymous_requests.load(Ordering::SeqCst),
        1,
        "the unauthenticated request is rejected"
    );
    assert!(
        anonymous
            .app
            .state
            .status
            .error
            .as_deref()
            .is_some_and(|error| error.contains("authentication")),
        "an anonymous mutation surfaces an authentication error, got {:?}",
        anonymous.app.state.status.error
    );
}

/// Handler selection: mailcap is the default, explicit handlers win over
/// mailcap, and disabling mailcap in the app degrades media opening to
/// metadata-only. There is no inline terminal graphics path.
#[test]
fn mailcap_is_default_and_handlers_are_explicit() {
    let media = MediaRef::new(Url::parse("https://example.com/photo.png").unwrap());

    let policy = MediaPolicyConfig::default();
    assert!(
        matches!(policy.select(&media), MediaHandler::Mailcap { .. }),
        "mailcap is the default handler"
    );

    let handlers = MediaPolicyConfig {
        handlers: HashMap::from([("image/png".to_owned(), "viewer %s".to_owned())]),
        ..Default::default()
    };
    assert_eq!(
        handlers.select(&media),
        MediaHandler::External {
            command: "viewer %s".to_owned()
        }
    );

    // App-level: `:set media mailcap off` persists and degrades `:media` to
    // metadata-only without spawning anything.
    let mut app = FixtureApp::new("mailcap");
    let mut engine = InputEngine::new();
    app.app.state.view.posts = vec![post_view(
        1,
        "Media post",
        Some(Url::parse("https://example.com/photo.png").unwrap()),
    )];
    app.app.state.view.selected = Some(0);
    app.command(&mut engine, "set media mailcap off")
        .expect("disable mailcap");
    assert!(
        !app.profile_store
            .load_config()
            .expect("reload config")
            .media
            .mailcap_enabled,
        "the mailcap toggle must persist to the config file"
    );
    // Pin the binding to the harness's scratch config: the toggle must land
    // in the file the harness resolved (never the real user config), which
    // also guards against any future env-resolution regression.
    let persisted = std::fs::read_to_string(&app.config_path).expect("read harness config");
    assert!(
        persisted.contains("mailcap_enabled = false"),
        "the mailcap toggle must persist to the scratch config, got:\n{persisted}"
    );
    app.command(&mut engine, "media")
        .expect("open media with mailcap disabled");
    assert!(
        app.app.state.status.message.contains("metadata only"),
        "disabling mailcap degrades to metadata-only, got {:?}",
        app.app.state.status.message
    );
}

/// MIME probing: Lemmy media URLs are often extension-less (`image_proxy`
/// rewrites), so the open path must resolve the MIME from the resource's
/// Content-Type header before choosing a handler. A probed image served by
/// the loopback server must reach the handler decision instead of degrading
/// to an "unknown" metadata-only result.
#[test]
fn extensionless_media_url_is_probed_for_its_mime_type() {
    let port = spawn_http_server(b"fixture media bytes".to_vec(), "image/png");
    let media_url = Url::parse(&format!("http://127.0.0.1:{port}/noext")).unwrap();
    let mut app = FixtureApp::new("media-probe");
    let mut engine = InputEngine::new();
    app.app.state.view.posts = vec![post_view(1, "Media post", Some(media_url))];
    app.app.state.view.selected = Some(0);
    // Mailcap stays off so the handler decision is observable without
    // spawning an external viewer.
    app.command(&mut engine, "set media mailcap off")
        .expect("disable mailcap");
    app.command(&mut engine, "media").expect("open media");
    assert!(
        app.app.state.status.message.contains("image/png"),
        "the extension-less URL must be probed to image/png, got {:?}",
        app.app.state.status.message
    );
    assert!(
        app.app.state.status.message.contains("metadata only"),
        "with mailcap off the probed media is still metadata-only, got {:?}",
        app.app.state.status.message
    );
}

/// Media handlers receive a local file, never the remote URL: `:media` on a
/// loopback-served image must download it to a scratch file first, then
/// spawn the handler with that path (imv/feh/zathura cannot fetch URLs, and
/// a URL argument leaves imv with a blank window).
#[test]
fn media_handlers_receive_a_local_file() {
    let port = spawn_http_server(b"fixture media bytes".to_vec(), "image/png");
    let media_url = Url::parse(&format!("http://127.0.0.1:{port}/pic.png")).unwrap();
    let destination =
        std::env::temp_dir().join(format!("lemmy-handler-copy-{}.png", std::process::id()));
    let _ = std::fs::remove_file(&destination);
    let media = MediaConfig {
        handlers: HashMap::from([(
            "image/png".to_owned(),
            format!("cp %s {}", destination.display()),
        )]),
        ..Default::default()
    };
    let runtime = support::runtime();
    let api = support::api(&runtime, || fixture_api_with_body("{}"));
    let mut app =
        FixtureApp::with_runtime(runtime, "media-local", api, anonymous_context(), media, &[]);
    let mut engine = InputEngine::new();
    app.app.state.view.posts = vec![post_view(1, "Media post", Some(media_url))];
    app.app.state.view.selected = Some(0);

    app.command(&mut engine, "media").expect("open media");
    assert!(
        app.app.state.status.message.contains("external handler"),
        "the handler runs after the download, got {:?}",
        app.app.state.status.message
    );
    // The handler is spawned detached, so poll for the copy.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        match std::fs::read(&destination) {
            Ok(bytes) if bytes == b"fixture media bytes" => break,
            _ if std::time::Instant::now() >= deadline => {
                panic!(
                    "the handler never copied the downloaded file to {}",
                    destination.display()
                );
            }
            _ => std::thread::sleep(std::time::Duration::from_millis(20)),
        }
    }

    // The scratch download (in the temp directory) survives while the
    // session is live, then is removed when the client quits.
    app.dispatch(AppAction::ShowDownloads)
        .expect("open the downloads panel");
    let scratch = app
        .model()
        .downloads
        .expect("the downloads panel renders")
        .records
        .iter()
        .find(|record| record.local_path.starts_with(std::env::temp_dir()))
        .expect("the handler download is recorded")
        .local_path
        .clone();
    assert!(
        scratch.exists(),
        "the scratch file exists while the session is live: {}",
        scratch.display()
    );
    app.dispatch(AppAction::Quit).expect("quit");
    assert!(
        !scratch.exists(),
        "quit must remove scratch media files: {}",
        scratch.display()
    );

    let _ = std::fs::remove_file(&destination);
}

/// Media download and history inspection: `:download-media` fetches through a
/// loopback server, the session downloads panel shows the completed record
/// and filters it, a confirmed delete removes the local file, and quitting
/// clears the current-session history.
#[test]
fn media_download_completes_and_history_is_inspectable() {
    let port = spawn_http_server(b"fixture media bytes".to_vec(), "image/png");
    let media_url = Url::parse(&format!("http://127.0.0.1:{port}/pic.png")).unwrap();
    let media = MediaConfig {
        collision_policy: "overwrite".to_owned(),
        ..Default::default()
    };
    let runtime = support::runtime();
    let api = support::api(&runtime, || fixture_api_with_body("{}"));
    let mut app = FixtureApp::with_runtime(runtime, "media", api, anonymous_context(), media, &[]);
    let mut engine = InputEngine::new();

    app.app.state.view.posts = vec![post_view(1, "Media post", Some(media_url))];
    app.app.state.view.selected = Some(0);

    app.dispatch(AppAction::DownloadMedia)
        .expect("start media download");
    assert!(
        app.app.state.status.message.contains("download #1 started"),
        "download starts with a status, got {:?}",
        app.app.state.status.message
    );

    app.dispatch(AppAction::ShowDownloads)
        .expect("open the downloads panel");
    assert!(app.model().downloads.is_some(), "the downloads panel opens");

    assert!(
        app.poll_until(Duration::from_secs(10), |model| {
            model.downloads.as_ref().is_some_and(|panel| {
                panel
                    .records
                    .iter()
                    .any(|record| record.status == DownloadStatus::Completed)
            }) && model.status.message.contains("download #1 complete")
        }),
        "the download completes, appears in the session history, and the status line reports it"
    );

    let completed = app
        .model()
        .downloads
        .expect("panel still open")
        .records
        .into_iter()
        .find(|record| record.status == DownloadStatus::Completed)
        .expect("completed record");
    assert!(
        completed.local_path.exists(),
        "the downloaded file exists at {}",
        completed.local_path.display()
    );
    assert!(
        completed.local_path.starts_with(&app.download_dir),
        "the download lands in the configured download directory"
    );
    assert_eq!(
        std::fs::read(&completed.local_path).expect("read download"),
        b"fixture media bytes"
    );
    assert_eq!(completed.filename, "pic.png");

    // History inspection: the panel filters by query.
    app.command(&mut engine, "downloads search pic")
        .expect("filter history");
    let panel = app.model().downloads.expect("panel stays open");
    assert_eq!(panel.query, "pic");
    assert!(
        !panel.records.is_empty(),
        "the search filter matches the record"
    );
    assert!(
        panel
            .records
            .iter()
            .all(|record| record.filename.contains("pic")),
        "only matching records remain after filtering"
    );

    // Confirmed delete removes the local file.
    app.dispatch(AppAction::Downloads(lemmy::app::DownloadsAction::Delete))
        .expect("stage download deletion");
    assert!(
        app.app.state.status.confirmation_pending,
        "deletion requires confirmation"
    );
    app.dispatch(AppAction::Confirm)
        .expect("confirm download deletion");
    assert!(
        !completed.local_path.exists(),
        "the confirmed delete removes the local file"
    );
    assert!(app.app.state.status.message.contains("local file deleted"));

    // Clean exit clears the current-session history.
    app.dispatch(AppAction::Quit).expect("quit");
    assert!(app.app.is_quit(), "the quit action ends the session");
    assert!(
        app.model()
            .downloads
            .expect("panel remains")
            .records
            .is_empty(),
        "session download history is cleared on quit"
    );
}

/// Transient network recovery: a feed refresh that exhausts the bounded retry
/// window surfaces a retryable error without dropping cached content, and a
/// later refresh succeeds once the transient window closes.
#[test]
fn transient_network_failure_is_retryable_and_recovers() {
    let runtime = support::runtime();
    let (api, remaining) = support::api(&runtime, || {
        fixture_api_with_transient_failures(&feed_fixture_body(), 3)
    });
    let mut app = FixtureApp::with_runtime(
        runtime,
        "transient",
        api,
        anonymous_context(),
        MediaConfig::default(),
        &[],
    );
    let mut engine = InputEngine::new();

    app.command(&mut engine, "feed").expect("first refresh");
    assert_eq!(
        remaining.load(Ordering::SeqCst),
        0,
        "the bounded retry loop consumes exactly the transient window"
    );
    assert!(
        app.app.state.status.error.is_some(),
        "exhausted failures surface an error"
    );
    assert!(app.app.state.status.retryable, "the error is retryable");
    assert!(app.app.state.view.posts.is_empty());

    app.command(&mut engine, "feed").expect("recovered refresh");
    assert_eq!(
        app.app.state.view.posts.len(),
        2,
        "a later refresh recovers once the window closes"
    );
    assert!(
        app.app.state.status.error.is_none(),
        "the recovered refresh clears the error"
    );
}

/// Kept from the scaffold: the library exposes the result alias the binary
/// and every test target rely on.
#[test]
fn library_exposes_error_result_alias() {
    let result: lemmy::Result<()> = Ok(());
    assert!(result.is_ok());
}

#[test]
fn render_model_always_contains_active_profile_and_instance() {
    let app = FixtureApp::new("model");
    let model = app.app.state.render_model();
    assert!(!model.status.profile_name.is_empty());
    assert!(!model.status.instance_url.is_empty());
}
