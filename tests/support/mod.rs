//! Shared harness for the end-to-end smoke suite.
//!
//! `FixtureApp` wires the real application together the way the binary does —
//! a fixture-backed HTTP adapter, a SQLite cache/draft store, temporary XDG
//! config/cache/downloads directories, and an in-memory credential store —
//! so the smoke scenarios exercise integration seams rather than unit
//! boundaries. The smoke suite runs serially (`--test-threads=1`) because
//! `FixtureApp` briefly redirects the process XDG environment while it
//! constructs the application.

use std::{
    io::{Read, Write},
    net::TcpListener,
    path::PathBuf,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use lemmy::{
    api::{
        LemmyApi,
        fixtures::{anonymous_context, fixture_api},
    },
    app::{App, AppAction, RenderModel},
    cache::{CacheStore, SqliteCacheStore},
    config::MediaConfig,
    domain::{CommunityId, PostId, Profile},
    error::Result,
    input::{Command, InputEngine},
    profiles::{MemoryCredentialStore, ProfileStore},
};
use url::Url;

/// A disposable scratch directory that removes itself on drop.
#[derive(Debug)]
pub struct ScratchDir {
    pub path: PathBuf,
}

impl ScratchDir {
    pub fn new(label: &str) -> Self {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "lemmy-smoke-{label}-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("create smoke scratch directory");
        Self { path }
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Fixture-backed application harness: a real Tokio runtime, the fixture API,
/// an in-memory credential store, and a temporary XDG config/cache/downloads
/// layout that mirrors production paths.
pub struct FixtureApp {
    pub runtime: tokio::runtime::Runtime,
    pub app: App,
    pub credentials: MemoryCredentialStore,
    pub profile_store: ProfileStore,
    pub config_path: PathBuf,
    pub cache_dir: PathBuf,
    pub download_dir: PathBuf,
    /// Owned scratch directory kept alive for cleanup; `None` when the test
    /// owns it (`in_scratch`). Held only for its drop side effect.
    _scratch: Option<ScratchDir>,
}

impl FixtureApp {
    /// Default harness: the fixture feed API, an anonymous fixture profile,
    /// and the default media policy.
    pub fn new(label: &str) -> Self {
        let runtime = runtime();
        let api = runtime.block_on(async { fixture_api("feed.json") });
        Self::with_runtime(
            runtime,
            label,
            api,
            anonymous_context(),
            MediaConfig::default(),
            &[],
        )
    }

    /// Build a harness with a caller-built runtime and fixture API. The
    /// runtime must be created first — the fixture server starts inside it
    /// (its task is spawned on the ambient runtime) and stays alive for the
    /// whole scenario. Use `support::runtime()` and `support::api()`.
    pub fn with_runtime(
        runtime: tokio::runtime::Runtime,
        label: &str,
        api: impl LemmyApi + 'static,
        context: lemmy::domain::ProfileContext,
        media: MediaConfig,
        profiles: &[Profile],
    ) -> Self {
        let scratch = ScratchDir::new(label);
        Self::build(
            runtime,
            scratch.path.clone(),
            Some(scratch),
            api,
            context,
            media,
            profiles,
        )
    }

    /// Build a harness reusing a test-owned scratch directory so two harness
    /// instances can share one on-disk cache (draft/restart scenarios). The
    /// caller keeps ownership of `scratch` for the whole scenario.
    pub fn in_scratch(
        runtime: tokio::runtime::Runtime,
        scratch: &ScratchDir,
        api: impl LemmyApi + 'static,
        context: lemmy::domain::ProfileContext,
        media: MediaConfig,
        profiles: &[Profile],
    ) -> Self {
        Self::build(
            runtime,
            scratch.path.clone(),
            None,
            api,
            context,
            media,
            profiles,
        )
    }

    fn build(
        runtime: tokio::runtime::Runtime,
        root: PathBuf,
        scratch: Option<ScratchDir>,
        api: impl LemmyApi + 'static,
        context: lemmy::domain::ProfileContext,
        media: MediaConfig,
        profiles: &[Profile],
    ) -> Self {
        let config_home = root.join("config");
        let cache_home = root.join("cache");
        let config_path = config_home.join("lemmy").join("config.toml");
        let cache_dir = cache_home.join("lemmy");
        let download_dir = root.join("downloads");
        std::fs::create_dir_all(config_home.join("lemmy")).expect("create config directory");
        std::fs::create_dir_all(&cache_dir).expect("create cache directory");
        let profile_store = ProfileStore::new(&config_path);
        profile_store.save(profiles).expect("seed profiles");

        // Redirect the XDG locations while the application resolves its
        // default profile store and cache, so `App` never touches the real
        // user configuration. The smoke suite runs single-threaded, and the
        // environment is restored immediately after construction (paths are
        // captured at that point).
        let previous_config = std::env::var("XDG_CONFIG_HOME").ok();
        let previous_cache = std::env::var("XDG_CACHE_HOME").ok();
        // SAFETY: single-threaded test process; the previous values are
        // restored immediately below.
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", &config_home);
            std::env::set_var("XDG_CACHE_HOME", &cache_home);
        }

        let cache: Arc<dyn CacheStore> = Arc::new(
            SqliteCacheStore::open_with_size_limit(cache_dir.join("cache.sqlite3"), None)
                .expect("open sqlite cache"),
        );
        let credentials = MemoryCredentialStore::default();
        let app = App::with_media(
            Arc::new(api),
            cache,
            context,
            Arc::new(credentials.clone()),
            MediaConfig {
                download_directory: Some(download_dir.clone()),
                ..media
            },
        );

        // SAFETY: restoring values that were read above; see the comment on
        // the matching set_var calls.
        unsafe {
            match previous_config {
                Some(value) => std::env::set_var("XDG_CONFIG_HOME", value),
                None => std::env::remove_var("XDG_CONFIG_HOME"),
            }
            match previous_cache {
                Some(value) => std::env::set_var("XDG_CACHE_HOME", value),
                None => std::env::remove_var("XDG_CACHE_HOME"),
            }
        }

        Self {
            runtime,
            app,
            credentials,
            profile_store,
            config_path,
            cache_dir,
            download_dir,
            _scratch: scratch,
        }
    }

    /// Dispatch an action on the harness runtime, mirroring the terminal
    /// event loop's dispatch boundary.
    pub fn dispatch(&mut self, action: AppAction) -> Result<()> {
        self.runtime.block_on(self.app.dispatch(action))
    }

    pub fn model(&self) -> RenderModel {
        self.app.render_model()
    }

    /// Feed a key through the real input engine and dispatch the resulting
    /// command, mirroring how the terminal event loop queues input.
    pub fn press(&mut self, engine: &mut InputEngine, key: KeyEvent) -> Result<()> {
        let command = engine.handle(key);
        if command == Command::Noop {
            return Ok(());
        }
        self.dispatch(AppAction::Input(command))
    }

    /// Drive the full command-mode path: `:` then the line then Enter.
    pub fn command(&mut self, engine: &mut InputEngine, line: &str) -> Result<()> {
        self.press(
            engine,
            KeyEvent::new(KeyCode::Char(':'), KeyModifiers::NONE),
        )?;
        for character in line.chars() {
            self.press(
                engine,
                KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
            )?;
        }
        self.press(engine, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
    }

    /// Poll the application tick until `check` passes or the timeout elapses,
    /// returning whether the condition was observed.
    pub fn poll_until(
        &mut self,
        timeout: std::time::Duration,
        check: impl Fn(&RenderModel) -> bool,
    ) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            if check(&self.model()) {
                return true;
            }
            self.dispatch(AppAction::Tick).expect("tick dispatch");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        check(&self.model())
    }
}

/// A multi-threaded Tokio runtime for one smoke scenario. Create it before
/// building any fixture API: fixture servers spawn their accept loop on the
/// ambient runtime.
pub fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("build smoke runtime")
}

/// Construct a fixture API inside `runtime` so its server task is spawned on
/// that runtime and stays alive for the scenario. Fixture helpers may return
/// additional handles (request counters, instance URLs) alongside the API.
pub fn api<T>(runtime: &tokio::runtime::Runtime, make: impl FnOnce() -> T) -> T {
    runtime.block_on(async move { make() })
}

/// A `PostView` for seeding a feed without a network call.
pub fn post_view(id: i64, title: &str, url: Option<Url>) -> lemmy::PostView {
    lemmy::PostView {
        id: PostId(id),
        title: title.to_owned(),
        body: None,
        url,
        community_id: CommunityId(1),
        creator_id: lemmy::domain::UserId(1),
        score: 0,
        comments: 0,
        published: None,
    }
}

/// A key event for a printable character.
pub fn key(character: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE)
}

/// A tiny HTTP server that answers one request per connection with a fixed
/// body; used so media downloads never leave the loopback interface.
pub fn spawn_http_server(body: Vec<u8>, content_type: &'static str) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback media server");
    let port = listener.local_addr().expect("media server address").port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let mut buffer = [0u8; 4096];
            let _ = stream.read(&mut buffer);
            let headers = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(headers.as_bytes());
            let _ = stream.write_all(&body);
            let _ = stream.flush();
        }
    });
    port
}
