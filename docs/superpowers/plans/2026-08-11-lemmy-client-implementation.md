# Lemmy Client Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a Linux-first Rust terminal Lemmy client with a focused Vim-like interaction model, multiple instance/account profiles, authenticated personal interaction, safe multimedia handling, downloads, and current-session download history.

**Architecture:** A ratatui terminal shell feeds a standalone Vim input engine. Semantic commands update application state and call repositories/services. Lemmy HTTP, profile/credential storage, cache/drafts, and media handlers sit behind explicit interfaces so UI behavior is independent of API-version and terminal-handler details.

**Tech Stack:** Rust stable; ratatui 0.30; crossterm; Tokio; reqwest; serde; TOML; SQLite via rusqlite; OS credential-store integration; mailcap; optional Kitty graphics protocol.

## Global Constraints

- Linux terminal behavior, mailcap integration, and Kitty support are first-class.
- The core domain, application, and HTTP layers remain portable enough for later macOS support.
- The client uses a focused modal Vim core, not full Vim or a complete Ex implementation.
- A profile represents one instance/account pair; multiple profiles may share a base URL.
- Passwords, tokens, and session secrets are stored only in the OS credential store.
- Profile-scoped cache, drafts, requests, and credentials must remain isolated.
- The application is online-first; cached reads and drafts improve resilience, but mutations are not queued offline.
- Reads may retry bounded transient failures; non-idempotent mutations are never blindly retried.
- Mailcap is the default media handler; Kitty rendering is opt-in and capability-gated.
- Media downloads are asynchronous, cancellable, and recorded in current-session history only.
- Destructive actions require confirmation.
- Logs are opt-in and redact credentials, private content, and sensitive profile values.
- Every change is test-driven and ends with a focused test command before its commit.
- Do not add moderation, site administration, persistent download history, offline mutation queueing, or an embedded browser engine.

---

## File map

The implementation starts as a greenfield Rust workspace with one binary crate and focused modules:

```text
Cargo.toml
Cargo.lock
rust-toolchain.toml
src/
  main.rs                    # CLI entry point and terminal lifecycle
  lib.rs                     # Public module wiring for integration tests
  error.rs                   # Stable application error categories
  domain/
    mod.rs                   # Domain module exports
    profile.rs               # Profile and active-context types
    lemmy.rs                 # Posts, comments, communities, users, pagination
    media.rs                 # Media references, downloads, handler decisions
  input/
    mod.rs                   # Input engine exports
    mode.rs                  # Normal/Insert/Visual/Command/Search modes
    command.rs               # Semantic command enum and command parser
    engine.rs                # Key sequence state machine
    mapping.rs               # Configurable key mappings
  config/
    mod.rs                   # Config loading and atomic persistence
    model.rs                 # TOML-backed non-secret configuration types
    paths.rs                 # XDG paths and restrictive file permissions
  profiles/
    mod.rs                   # Profile service interface
    store.rs                 # Profile metadata persistence
    credentials.rs            # OS credential-store interface
  cache/
    mod.rs                   # Cache/draft repository interfaces
    store.rs                 # Local cache implementation
  api/
    mod.rs                   # Lemmy adapter interface
    http.rs                  # HTTP transport and API version handling
    fixtures.rs              # Deterministic test server/fixture helpers
  media/
    mod.rs                   # Media service interface
    mime.rs                  # MIME resolution
    mailcap.rs               # Mailcap lookup and command construction
    kitty.rs                 # Kitty capability detection and rendering
    download.rs              # Async download manager
  app/
    mod.rs                   # Application state and event loop boundary
    state.rs                 # Navigation, buffers, status, pending work
    actions.rs               # Semantic command execution
    repository.rs            # Domain repository orchestration
    render.rs                # ratatui views and widgets
    help.rs                  # Contextual help buffer

tests/
  input_engine.rs
  config_profiles.rs
  cache.rs
  api_adapter.rs
  application.rs
  media.rs
  smoke.rs
fixtures/
  lemmy/
    login.json
    site.json
    community.json
    post.json
    comment.json
    feed.json
```

Each module exposes traits where an adjacent layer needs substitution in tests. Concrete implementations are constructed in `main.rs`; tests inject deterministic fakes or fixture-backed implementations.

---

## Task 1: Scaffold the Rust project and executable boundary

**Files:**
- Create: `Cargo.toml`
- Create: `Cargo.lock` (generated by Cargo)
- Create: `rust-toolchain.toml`
- Create: `src/main.rs`
- Create: `src/lib.rs`
- Create: `src/error.rs`
- Create: `tests/smoke.rs`

**Interfaces:**
- Produces binary `lemmy`.
- Produces `lemmy::error::AppError` and `lemmy::Result<T>` for every later module.
- `main()` returns `Result<(), AppError>` and restores the terminal before returning.

- [ ] **Step 1: Write the failing executable smoke test**

```rust
#[test]
fn library_exposes_error_result_alias() {
    let result: lemmy::Result<()> = Ok(());
    assert!(result.is_ok());
}
```

- [ ] **Step 2: Run the focused test and verify failure**

Run: `cargo test --test smoke library_exposes_error_result_alias`
Expected: FAIL because the crate and result alias do not exist.

- [ ] **Step 3: Add the manifest and module boundary**

Use `ratatui = "0.30"`, `crossterm`, `tokio` with runtime and sync features, `reqwest` with JSON and rustls features, `serde` with derive, `serde_json`, `toml`, `url`, `thiserror`, `async-trait`, `secrecy`, `rusqlite` with bundled SQLite, `keyring`, and `tracing`/`tracing-subscriber`. Pin the Rust toolchain to the current stable channel in `rust-toolchain.toml`.

Implement:

```rust
// src/error.rs
pub type Result<T> = std::result::Result<T, AppError>;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("configuration error: {0}")]
    Configuration(String),
    #[error("authentication error: {0}")]
    Authentication(String),
    #[error("authorization error: {0}")]
    Authorization(String),
    #[error("network error: {0}")]
    Network(String),
    #[error("storage error: {0}")]
    Storage(String),
    #[error("media error: {0}")]
    Media(String),
    #[error("terminal error: {0}")]
    Terminal(String),
    #[error("invalid command: {0}")]
    InvalidCommand(String),
}
```

`src/lib.rs` exports `pub mod error;` and `pub use error::{AppError, Result};`. `src/main.rs` parses no arguments yet, returns an explicit terminal-boundary error, and contains no business logic.

- [ ] **Step 4: Run the focused test and verify success**

Run: `cargo test --test smoke library_exposes_error_result_alias`
Expected: PASS.

- [ ] **Step 5: Commit the scaffold**

```bash
git add Cargo.toml Cargo.lock rust-toolchain.toml src tests/smoke.rs
git commit -m "build: scaffold Rust Lemmy client"
```

---

## Task 2: Implement the Vim input engine

**Files:**
- Create: `src/input/mod.rs`
- Create: `src/input/mode.rs`
- Create: `src/input/command.rs`
- Create: `src/input/mapping.rs`
- Create: `src/input/engine.rs`
- Modify: `src/lib.rs`
- Test: `tests/input_engine.rs`

**Interfaces:**

```rust
pub enum Mode { Normal, Insert, Visual, Command, SearchForward, SearchBackward }

pub enum Command {
    MoveDown { count: u32 },
    MoveUp { count: u32 },
    MoveLeft { count: u32 },
    MoveRight { count: u32 },
    Open,
    Refresh,
    EnterInsert,
    EnterVisual,
    EnterCommand,
    EnterSearch { backward: bool },
    Back,
    Quit,
    Text(String),
    SubmitLine(String),
    Noop,
}

pub struct InputEngine { mode: Mode, count: u32, line: String, mappings: MappingTable }
impl InputEngine {
    pub fn handle(&mut self, key: crossterm::event::KeyEvent) -> Command;
    pub fn mode(&self) -> Mode;
}
```

- [ ] **Step 1: Write tests for mode transitions, counts, and command-line submission**

```rust
#[test]
fn normal_j_emits_one_down_move() { assert_eq!(InputEngine::default().handle(key('j')), Command::MoveDown { count: 1 }); }

#[test]
fn count_prefix_is_applied_to_motion() { let mut engine = InputEngine::default(); engine.handle(key('1')); engine.handle(key('2')); assert_eq!(engine.handle(key('j')), Command::MoveDown { count: 12 }); }

#[test]
fn colon_enters_command_mode_and_enter_submits_line() { let mut engine = InputEngine::default(); engine.handle(key(':')); for character in ":profile demo".chars() { engine.handle(key(character)); } assert_eq!(engine.handle(enter()), Command::SubmitLine(":profile demo".into())); }

#[test]
fn escape_returns_insert_and_visual_to_normal() { let mut engine = InputEngine::default(); engine.handle(key('i')); assert_eq!(engine.mode(), Mode::Insert); engine.handle(escape()); assert_eq!(engine.mode(), Mode::Normal); engine.handle(key('v')); assert_eq!(engine.mode(), Mode::Visual); engine.handle(escape()); assert_eq!(engine.mode(), Mode::Normal); }
```
The test module defines `key(char) -> KeyEvent`, `enter() -> KeyEvent`, and `escape() -> KeyEvent` helpers using crossterm’s `KeyEvent::new`.

- [ ] **Step 2: Run the tests and verify they fail**

Run: `cargo test --test input_engine`
Expected: FAIL because `InputEngine` and commands are not implemented.

- [ ] **Step 3: Implement the smallest state machine**

Map `h`, `j`, `k`, `l`, `q`, `i`, `v`, `:`, `/`, `?`, `Esc`, and `Enter`. Accumulate decimal counts before a motion, defaulting to one. In Insert mode, emit printable characters as `Text`; in Command/Search mode, maintain a line buffer and emit `SubmitLine` on Enter. Unknown sequences emit `Noop` and clear only the invalid pending sequence.

- [ ] **Step 4: Add configurable mappings**

Implement `MappingTable::insert(sequence, command)` and resolve the longest complete mapping without blocking indefinitely. Keep mappings independent from ratatui and network state.

- [ ] **Step 5: Run the tests and verify they pass**

Run: `cargo test --test input_engine`
Expected: PASS.

- [ ] **Step 6: Commit the input engine**

```bash
git add src/input src/lib.rs tests/input_engine.rs
git commit -m "feat: add modal Vim input engine"
```

---

## Task 3: Add domain types and configuration/profile metadata

**Files:**
- Create: `src/domain/mod.rs`
- Create: `src/domain/profile.rs`
- Create: `src/domain/lemmy.rs`
- Create: `src/domain/media.rs`
- Create: `src/config/mod.rs`
- Create: `src/config/model.rs`
- Create: `src/config/paths.rs`
- Create: `src/profiles/mod.rs`
- Create: `src/profiles/store.rs`
- Modify: `src/lib.rs`
- Test: `tests/config_profiles.rs`

**Interfaces:**

```rust
pub struct ProfileId(pub String);
pub struct Profile { pub id: ProfileId, pub instance_url: url::Url, pub account_label: Option<String> }
pub struct ActiveProfile { pub profile: Profile, pub authenticated: bool }

pub struct AppConfig { pub profiles: Vec<Profile>, pub keymaps: HashMap<String, String>, pub media: MediaConfig, pub cache: CacheConfig }
impl AppConfig {
    pub fn from_toml(source: &str) -> Result<Self>;
    pub fn to_toml(&self) -> Result<String>;
    pub fn load(path: &Path) -> Result<Self>;
    pub fn write_atomic(&self, path: &Path) -> Result<()>;
}

pub enum Mutation {
    VotePost { id: PostId, score: i8 },
    VoteComment { id: CommentId, score: i8 },
    SavePost { id: PostId, saved: bool },
    Subscribe { community: CommunityId, subscribed: bool },
    CreatePost(CreatePostRequest),
    EditPost(EditPostRequest),
    DeletePost(PostId),
    CreateComment(CreateCommentRequest),
    EditComment(EditCommentRequest),
    DeleteComment(CommentId),
}
```

- [ ] **Step 1: Write tests for TOML round-trip, duplicate profile rejection, and secret rejection**

```rust
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
    assert!(matches!(AppConfig::from_toml(source), Err(AppError::Configuration(_))));
}
#[test]
fn credential_like_fields_are_rejected() {
    let source = "[[profiles]]\nid = 'main'\ninstance_url = 'https://example.test'\npassword = 'secret'\n";
    assert!(matches!(AppConfig::from_toml(source), Err(AppError::Configuration(_))));
}
```

- [ ] **Step 2: Run the tests and verify failure**

Run: `cargo test --test config_profiles`
Expected: FAIL because configuration and profile types do not exist.

- [ ] **Step 3: Implement domain and TOML types**

Use `serde` for non-secret config only. Define explicit `ProfileId`, `Profile`, `MediaConfig`, `CacheConfig`, and Lemmy entity types. Store instance URLs as parsed `url::Url`; reject non-HTTP(S) URLs and duplicate profile IDs. Define `PostId`, `CommentId`, `CommunityId`, `UserId`, `MediaRef`, `DownloadRecord`, `CreatePostRequest`, `EditPostRequest`, `CreateCommentRequest`, and `EditCommentRequest` as typed domain values or newtypes. `Mutation` is owned by `src/domain/lemmy.rs` so the API adapter and application actions share one definition.

- [ ] **Step 4: Implement XDG paths and atomic writes**

Resolve config/cache paths from XDG variables with Linux defaults. Write to a sibling temporary file, flush, set restrictive permissions, then rename. Never serialize credential fields.

- [ ] **Step 5: Run the tests and verify success**

Run: `cargo test --test config_profiles`
Expected: PASS.

- [ ] **Step 6: Commit domain and config**

```bash
git add src/domain src/config src/profiles src/lib.rs tests/config_profiles.rs Cargo.toml Cargo.lock
git commit -m "feat: add profiles and non-secret configuration"
```

---

## Task 4: Implement credential storage and profile context isolation

**Files:**
- Create: `src/profiles/credentials.rs`
- Modify: `src/profiles/mod.rs`
- Modify: `src/domain/profile.rs`
- Test: `tests/config_profiles.rs`

**Interfaces:**

```rust
#[async_trait::async_trait]
pub trait CredentialStore: Send + Sync {
    async fn get_session(&self, profile: &ProfileId) -> Result<Option<Session>>;
    async fn put_session(&self, profile: &ProfileId, session: &Session) -> Result<()>;
    async fn delete_session(&self, profile: &ProfileId) -> Result<()>;
}

pub struct Session { pub token: SecretString, pub user_id: UserId }
pub struct ProfileContext { pub profile: Profile, pub session: Option<Session> }
```

- [ ] **Step 1: Add an in-memory fake and tests for isolation and redaction**

```rust
#[tokio::test]
async fn sessions_are_keyed_by_profile_id() { let store = MemoryCredentialStore::default(); store.put_session(&ProfileId::from("one"), &session("token-one")).await.unwrap(); store.put_session(&ProfileId::from("two"), &session("token-two")).await.unwrap(); assert_eq!(store.get_session(&ProfileId::from("one")).await.unwrap().unwrap().token.expose_secret(), "token-one"); assert_eq!(store.get_session(&ProfileId::from("two")).await.unwrap().unwrap().token.expose_secret(), "token-two"); }

#[test]
fn session_debug_output_does_not_include_token() { let value = format!("{:?}", session("do-not-log").token); assert!(!value.contains("do-not-log")); }
```

- [ ] **Step 2: Run the tests and verify failure**

Run: `cargo test --test config_profiles sessions_are_keyed_by_profile_id`
Expected: FAIL because the credential interface is absent.

- [ ] **Step 3: Implement the interface and fake**

Use a secret wrapper with redacted `Debug` and `Display`. Profile context must carry the profile ID into every repository request. Add the production credential-store adapter behind a feature-compatible interface; use the platform keyring crate selected by a short Linux smoke check, and return an actionable `Storage` error when no credential backend is available.

- [ ] **Step 4: Run tests and verify success**

Run: `cargo test --test config_profiles`
Expected: PASS.

- [ ] **Step 5: Commit credential isolation**

```bash
git add src/profiles src/domain/profile.rs tests/config_profiles.rs Cargo.toml Cargo.lock
git commit -m "feat: isolate profile sessions in credential storage"
```

---

## Task 5: Add cache and draft repositories

**Files:**
- Create: `src/cache/mod.rs`
- Create: `src/cache/store.rs`
- Test: `tests/cache.rs`

**Interfaces:**

```rust
pub trait CacheStore: Send + Sync {
    fn read_feed(&self, context: &ProfileId, key: &FeedKey) -> Result<Option<CachedFeed>>;
    fn write_feed(&self, context: &ProfileId, key: &FeedKey, feed: &CachedFeed) -> Result<()>;
    fn save_draft(&self, draft: Draft) -> Result<()>;
    fn load_drafts(&self, context: &ProfileId) -> Result<Vec<Draft>>;
}
```

- [ ] **Step 1: Write tests for profile scoping, stale reads, malformed data, and draft survival**

```rust
#[test]
fn profile_a_cannot_read_profile_b_cache() { let cache = MemoryCache::default(); cache.write_feed(&ProfileId::from("a"), &feed_key("home"), &feed("a")).unwrap(); assert!(cache.read_feed(&ProfileId::from("b"), &feed_key("home")).unwrap().is_none()); }

#[test]
fn malformed_cache_entry_is_ignored() { let cache = MemoryCache::with_raw_entry("a", "home", b"not-json"); assert!(cache.read_feed(&ProfileId::from("a"), &feed_key("home")).unwrap().is_none()); }

#[test]
fn draft_survives_cache_failure() { let drafts = MemoryDraftStore::default(); let draft = draft_for(&ProfileId::from("a")); drafts.save_draft(draft.clone()).unwrap(); drafts.fail_next_read(); let _ = drafts.load_drafts(&ProfileId::from("a")); assert_eq!(drafts.raw_draft(&draft.id), Some(draft)); }
```

- [ ] **Step 2: Run tests and verify failure**

Run: `cargo test --test cache`
Expected: FAIL because the repository does not exist.

- [ ] **Step 3: Implement the local store**

Use SQLite through `rusqlite` with a profile ID column on every cache and draft table. Persist entity JSON, feed keys, synchronization timestamp, and draft records. Treat the cache as disposable: malformed rows are skipped and reported as stale/cache errors; they never prevent a new network request.

- [ ] **Step 4: Run tests and verify success**

Run: `cargo test --test cache`
Expected: PASS.

- [ ] **Step 5: Commit cache and drafts**

```bash
git add src/cache tests/cache.rs Cargo.toml Cargo.lock
git commit -m "feat: add scoped cache and draft storage"
```

---

## Task 6: Build the Lemmy HTTP adapter and fixture server

**Files:**
- Create: `src/api/mod.rs`
- Create: `src/api/http.rs`
- Create: `src/api/fixtures.rs`
- Create: `fixtures/lemmy/login.json`
- Create: `fixtures/lemmy/site.json`
- Create: `fixtures/lemmy/community.json`
- Create: `fixtures/lemmy/post.json`
- Create: `fixtures/lemmy/comment.json`
- Create: `fixtures/lemmy/feed.json`
- Test: `tests/api_adapter.rs`

**Interfaces:**

```rust
#[async_trait::async_trait]
pub trait LemmyApi: Send + Sync {
    async fn site(&self, ctx: &ProfileContext) -> Result<SiteInfo>;
    async fn feed(&self, ctx: &ProfileContext, query: FeedQuery) -> Result<Page<PostView>>;
    async fn post(&self, ctx: &ProfileContext, id: PostId) -> Result<PostDetail>;
    async fn login(&self, request: LoginRequest) -> Result<Session>;
    async fn mutate(&self, ctx: &ProfileContext, mutation: Mutation) -> Result<MutationResult>;
}
```

- [ ] **Step 1: Write fixture-backed tests for login, pagination, normalization, capability detection, and errors**

```rust
#[tokio::test]
async fn feed_response_normalizes_into_domain_posts() { let api = fixture_api("feed.json"); let page = api.feed(&anonymous_context(), FeedQuery::home()).await.unwrap(); assert_eq!(page.items.len(), 2); assert_eq!(page.items[0].title, "Fixture post"); }

#[tokio::test]
async fn expired_session_is_classified_as_authentication_error() { let api = fixture_api_with_status("/api/v3/post/list", 401); let result = api.feed(&anonymous_context(), FeedQuery::home()).await; assert!(matches!(result, Err(AppError::Authentication(_)))); }

#[tokio::test]
async fn mutation_timeout_is_not_reported_as_confirmed_failure() { let api = timeout_fixture_api(); let result = api.mutate(&authenticated_context(), Mutation::DeletePost(PostId(1))).await; assert!(matches!(result, Err(AppError::Network(message)) if message.contains("uncertain"))); }
```
The test module defines `fixture_api`, `fixture_api_with_status`, `timeout_fixture_api`, `anonymous_context`, and `authenticated_context` using the fixture server in `src/api/fixtures.rs`.

- [ ] **Step 2: Run tests and verify failure**

Run: `cargo test --test api_adapter`
Expected: FAIL because the adapter and fixtures are absent.

- [ ] **Step 3: Implement transport and stable adapter interfaces**

Use a shared reqwest client with timeouts and rustls. Build URLs from the active profile instance URL. Serialize the Lemmy HTTP API request envelope, attach auth only when a session exists, parse JSON into stable domain types, and preserve server error detail in a classified error. Add bounded retries only for idempotent reads and transient status codes.

- [ ] **Step 4: Add capability detection**

The `site` call records server/version capabilities. Unsupported operations return `AppError::Authorization` or a dedicated capability error with the operation name, never a silent success.

- [ ] **Step 5: Run tests and verify success**

Run: `cargo test --test api_adapter`
Expected: PASS.

- [ ] **Step 6: Commit the adapter**

```bash
git add src/api fixtures tests/api_adapter.rs Cargo.toml Cargo.lock
git commit -m "feat: add fixture-backed Lemmy HTTP adapter"
```

---

## Task 7: Add repository orchestration and application state

**Files:**
- Create: `src/app/mod.rs`
- Create: `src/app/state.rs`
- Create: `src/app/actions.rs`
- Create: `src/app/repository.rs`
- Modify: `src/lib.rs`
- Test: `tests/application.rs`

**Interfaces:**

```rust
pub enum ProfileCommand { Switch(ProfileId), List, New(ProfileDraft), Login, Logout, WhoAmI, Delete(ProfileId) }
pub enum AppAction { Input(Command), Profile(ProfileCommand), SubmitDraft(DraftId), OpenSelected, Back, DeletePost(PostId), Confirm, ApiResult(ApiResult), Tick, Quit }
pub struct AppState { pub mode: Mode, pub active: ProfileContext, pub view: View, pub status: Status, pub drafts: DraftStore }
pub struct App { pub state: AppState }
impl App {
    pub async fn dispatch(&mut self, action: AppAction) -> Result<()>;
    pub fn render_model(&self) -> RenderModel;
}
```

The test module defines `fixture_app` and `failing_mutation_app` with injected fixture API, cache, profile, and credential services. Task 11 consumes `ProfileCommand` rather than redefining it.

- [ ] **Step 1: Write tests for profile switching, selection preservation, draft safety, and error status**

```rust
#[tokio::test]
async fn profile_switch_changes_request_context_and_clears_selection() { let mut app = fixture_app(); app.state.select(PostId(1)); app.dispatch(AppAction::Profile(ProfileCommand::Switch(ProfileId::from("other")))).await.unwrap(); assert_eq!(app.state.active.profile.id, ProfileId::from("other")); assert!(app.state.selected_post().is_none()); }

#[tokio::test]
async fn failed_mutation_keeps_draft_and_sets_retryable_status() { let mut app = failing_mutation_app(); let draft = app.state.begin_comment_draft(); app.dispatch(AppAction::SubmitDraft(draft.id)).await.unwrap(); assert!(app.state.draft(draft.id).is_some()); assert!(app.state.status.is_retryable()); }
```

- [ ] **Step 2: Run tests and verify failure**

Run: `cargo test --test application`
Expected: FAIL because application state does not exist.

- [ ] **Step 3: Implement state and action dispatch**

Keep rendering read-only. `dispatch` translates input commands into application actions, invokes repository services, updates status, and preserves mode/view/drafts on recoverable errors. Model active profile identity in every pending request and discard stale results whose context no longer matches.

- [ ] **Step 4: Implement feed, post, comment, and profile repository calls**

Repositories read the cache first, return stale content with an explicit flag, then refresh through `LemmyApi`. Mutation calls write confirmed results to cache and retain drafts on all non-success outcomes.

- [ ] **Step 5: Run tests and verify success**

Run: `cargo test --test application`
Expected: PASS.

- [ ] **Step 6: Commit application orchestration**

```bash
git add src/app src/lib.rs tests/application.rs
git commit -m "feat: add application state and repository orchestration"
```

---

## Task 8: Implement ratatui rendering and terminal lifecycle

**Files:**
- Modify: `src/main.rs`
- Create: `src/app/render.rs`
- Create: `src/app/help.rs`
- Modify: `src/app/mod.rs`
- Test: `tests/smoke.rs`

**Interfaces:**

```rust
pub fn render(frame: &mut ratatui::Frame, model: &RenderModel);
pub fn run_terminal(app: App, terminal: DefaultTerminal) -> Result<()>;
```

- [ ] **Step 1: Add render-model tests for active profile, mode, stale state, and errors**

```rust
#[test]
fn render_model_always_contains_active_profile_and_instance() { let model = fixture_app().state.render_model(); assert!(!model.status.profile_name.is_empty()); assert!(!model.status.instance_url.is_empty()); }
```

- [ ] **Step 2: Run the focused test and verify failure**

Run: `cargo test --test smoke render_model_always_contains_active_profile_and_instance`
Expected: FAIL because the render model is absent.

- [ ] **Step 3: Implement read-only widgets**

Render the primary content area, optional detail/thread area, compose buffer, status line, mode indicator, active profile/instance, network state, and error message. Use accessible text labels and do not rely on color alone.

- [ ] **Step 4: Implement terminal lifecycle and async event loop**

Initialize ratatui/crossterm, poll input and Tokio events without blocking network work, redraw on state changes/ticks, and call terminal restoration in a guard/finally path on every exit.

- [ ] **Step 5: Run tests and verify success**

Run: `cargo test --test smoke`
Expected: PASS.

- [ ] **Step 6: Commit the TUI shell**

```bash
git add src/main.rs src/app/render.rs src/app/help.rs src/app/mod.rs tests/smoke.rs
git commit -m "feat: add ratatui shell and terminal lifecycle"
```

---

## Task 9: Implement browsing, search, composition, and personal mutations

**Files:**
- Modify: `src/app/actions.rs`
- Modify: `src/app/state.rs`
- Modify: `src/api/http.rs`
- Modify: `src/app/render.rs`
- Test: `tests/application.rs`
- Test: `tests/api_adapter.rs`

**Interfaces:**

Task 3 owns the `Mutation` enum and request types in `src/domain/lemmy.rs`. This task consumes those types while adding action handlers for browsing, composition, validation, confirmation, and personal mutations.

- [ ] **Step 1: Write failing tests for feed navigation, search, draft submission, and mutation confirmation**

```rust
#[tokio::test]
async fn opening_post_preserves_feed_position_for_back_navigation() { let mut app = fixture_app(); app.state.select_index(4); app.dispatch(AppAction::OpenSelected).await.unwrap(); app.dispatch(AppAction::Back).await.unwrap(); assert_eq!(app.state.selected_index(), 4); }

#[tokio::test]
async fn destructive_delete_requires_confirmation_before_api_call() { let mut app = fixture_app(); app.dispatch(AppAction::DeletePost(PostId(1))).await.unwrap(); assert_eq!(app.api.calls(), 0); app.dispatch(AppAction::Confirm).await.unwrap(); assert_eq!(app.api.calls(), 1); }

#[tokio::test]
async fn successful_post_submission_removes_draft_only_after_confirmation() { let mut app = fixture_app(); let draft = app.state.begin_post_draft(); app.dispatch(AppAction::SubmitDraft(draft.id)).await.unwrap(); assert!(app.state.draft(draft.id).is_some()); app.dispatch(AppAction::Confirm).await.unwrap(); assert!(app.state.draft(draft.id).is_none()); }
```

- [ ] **Step 2: Run tests and verify failure**

Run: `cargo test --test application`
Expected: FAIL because the action handlers are not implemented.

- [ ] **Step 3: Implement reading and search actions**

Add feed selection, pagination, refresh, community navigation, post/thread opening, back navigation, and search matching. Preserve stable object IDs and visible list position across refreshes.

- [ ] **Step 4: Implement compose/edit buffers**

Add multiline buffers, post title/body/link fields, comment/reply fields, validation, submit/cancel/discard, and profile-associated draft storage. Keep the active mode and draft on validation or network failure.

- [ ] **Step 5: Implement personal mutations**

Add vote, save, subscribe, create, edit, and delete commands. Confirm destructive operations, identify the active account/instance, avoid unsafe retries, and update cache only on confirmed success.

- [ ] **Step 6: Run tests and verify success**

Run: `cargo test --test application --test api_adapter`
Expected: PASS.

- [ ] **Step 7: Commit personal interaction**

```bash
git add src/app src/api tests/application.rs tests/api_adapter.rs
git commit -m "feat: add Lemmy browsing and personal mutations"
```

---

## Task 10: Implement MIME resolution, mailcap, Kitty, and media downloads

**Files:**
- Create: `src/media/mod.rs`
- Create: `src/media/mime.rs`
- Create: `src/media/mailcap.rs`
- Create: `src/media/kitty.rs`
- Create: `src/media/download.rs`
- Modify: `src/domain/media.rs`
- Modify: `src/app/actions.rs`
- Modify: `src/app/state.rs`
- Modify: `src/app/render.rs`
- Test: `tests/media.rs`

**Interfaces:**

```rust
pub struct TerminalCapabilities { pub kitty: bool }
pub enum MediaHandler { Mailcap { command: String }, KittyInline, External { command: String }, MetadataOnly }
pub struct MediaPolicyConfig { pub kitty_enabled: bool, pub mailcap_enabled: bool }
impl MediaPolicyConfig { pub fn select(&self, media: &MediaRef, capabilities: &TerminalCapabilities) -> MediaHandler; }
pub struct DownloadManager;
impl DownloadManager {
    pub async fn start(&self, request: DownloadRequest) -> Result<DownloadId>;
    pub async fn cancel(&self, id: DownloadId) -> Result<()>;
    pub fn history(&self) -> &SessionDownloadHistory;
}
```

The test module defines `image_media`, `test_download_manager`, and `slow_download_request` helpers locally.

- [ ] **Step 1: Write failing tests for MIME precedence, Kitty opt-in, collision policy, cancellation, and history**

```rust
#[test]
fn mailcap_is_default_even_when_kitty_is_available() { let policy = MediaPolicyConfig::default(); let handler = policy.select(&image_media(), &TerminalCapabilities { kitty: true }); assert!(matches!(handler, MediaHandler::Mailcap { .. })); }

#[test]
fn kitty_is_selected_only_when_enabled_and_supported() { let policy = MediaPolicyConfig { kitty_enabled: true, ..Default::default() }; let handler = policy.select(&image_media(), &TerminalCapabilities { kitty: true }); assert_eq!(handler, MediaHandler::KittyInline); }

#[tokio::test]
async fn cancelled_download_is_recorded_in_session_history() { let manager = test_download_manager(); let id = manager.start(slow_download_request()).await.unwrap(); manager.cancel(id).await.unwrap(); assert_eq!(manager.history().get(id).unwrap().status, DownloadStatus::Cancelled); }
```

- [ ] **Step 2: Run tests and verify failure**

Run: `cargo test --test media`
Expected: FAIL because the media service is absent.

- [ ] **Step 3: Implement MIME and handler selection**

Resolve MIME from server type, response header, and filename in that order. Parse mailcap entries without shell interpolation, construct argv safely, and select mailcap by default. Kitty is selected only when explicitly enabled and the terminal capability check succeeds. Unsupported types return metadata-only handling.

- [ ] **Step 4: Implement asynchronous downloads**

Stream downloads through Tokio, report progress, support cancellation, write to a temporary file, apply prompt/overwrite/unique-name collision policy, atomically rename on success, and clean stale temporary files on startup. Never pass authorization headers to external handlers.

- [ ] **Step 5: Implement current-session history and commands**

Record filename, source URL, MIME type, profile/instance, timestamp, local path, and status. Add `:media`, `:download-media`, and `:downloads`; render a searchable history with reopen, reveal-directory, copy-path, retry, and confirmed-delete actions. Keep history in memory only.

- [ ] **Step 6: Run tests and verify success**

Run: `cargo test --test media`
Expected: PASS.

- [ ] **Step 7: Commit media support**

```bash
git add src/media src/domain/media.rs src/app tests/media.rs
 git commit -m "feat: add media handlers downloads and session history"
```

---

## Task 11: Wire authentication, profile switching, help, and configuration commands

**Files:**
- Modify: `src/app/actions.rs`
- Modify: `src/app/help.rs`
- Modify: `src/app/render.rs`
- Modify: `src/main.rs`
- Modify: `src/config/model.rs`
- Modify: `src/profiles/mod.rs`
- Test: `tests/application.rs`
- Test: `tests/config_profiles.rs`

**Interfaces:**

Task 7 defines `ProfileCommand` and `AppAction`. This task adds the profile command executor and session lifecycle implementation.

```rust
pub async fn execute_profile_command(&mut self, command: ProfileCommand) -> Result<()>;
```

- [ ] **Step 1: Write failing tests for login/logout, profile switch, and help discoverability**

```rust
#[tokio::test]
async fn login_stores_session_only_after_api_success() { let (api, credentials) = login_test_dependencies(); api.fail_login_once(); let result = login(&api, &credentials, login_request()).await; assert!(result.is_err()); assert!(credentials.all().is_empty()); }

#[tokio::test]
async fn logout_removes_session_and_keeps_non_secret_profile_metadata() { let (profiles, credentials) = profile_test_dependencies(); profiles.create(profile("main")).unwrap(); credentials.put_session(&ProfileId::from("main"), &session("secret")).await.unwrap(); logout(&profiles, &credentials, &ProfileId::from("main")).await.unwrap(); assert!(credentials.get_session(&ProfileId::from("main")).await.unwrap().is_none()); assert!(profiles.get(&ProfileId::from("main")).is_ok()); }

#[test]
fn help_lists_profile_and_media_commands() { let help = HelpIndex::default(); assert!(help.contains(":profile")); assert!(help.contains(":downloads")); }
```

- [ ] **Step 2: Run tests and verify failure**

Run: `cargo test --test application --test config_profiles`
Expected: FAIL because command wiring is incomplete.

- [ ] **Step 3: Implement profile commands and session restoration**

Load profiles at startup, restore a credential-store session when available, expose login/logout/whoami, validate the target instance before login, and make profile switching clear stale transient state. Ensure every API call receives the current profile context.

- [ ] **Step 4: Implement help and configuration commands**

Add searchable contextual help and configuration updates for keymaps, media policy, download directory, collision policy, cache settings, and logging. Validate and atomically write changed config before applying it.

- [ ] **Step 5: Run tests and verify success**

Run: `cargo test --test application --test config_profiles`
Expected: PASS.

- [ ] **Step 6: Commit profile and command wiring**

```bash
git add src/app src/main.rs src/config src/profiles tests/application.rs tests/config_profiles.rs
git commit -m "feat: wire profile authentication and command help"
```

---

## Task 12: Add end-to-end fixture smoke tests and Linux packaging checks

**Files:**
- Modify: `tests/smoke.rs`
- Create: `tests/support/mod.rs`
- Create: `.github/workflows/ci.yml`
- Create: `README.md`
- Create: `docs/configuration.md`
- Create: `docs/keybindings.md`
- Create: `docs/media.md`
- Modify: `Cargo.toml`

**Interfaces:**
- `tests/support::FixtureApp` starts the fixture-backed adapter, fake credential store, temporary config/cache, and application state.
- CI runs formatting, linting, unit tests, integration tests, and a non-interactive binary smoke check.

- [ ] **Step 1: Write the complete smoke scenarios**

Cover launch, feed navigation, post/thread opening, draft preservation, profile switching, authenticated mutation, mailcap selection, download/history inspection, transient network recovery, and terminal lifecycle restoration.

- [ ] **Step 2: Run the smoke tests and verify any failures**

Run: `cargo test --test smoke -- --test-threads=1`
Expected: failures identify missing end-to-end wiring rather than incomplete code paths.

- [ ] **Step 3: Fix only integration defects exposed by the scenarios**

Keep feature behavior in the owning module. Do not weaken assertions or suppress terminal cleanup errors. Every failure must produce either a focused implementation fix or a documented, approved platform limitation.

- [ ] **Step 4: Add CI and packaging checks**

CI commands:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
cargo run -- --help
```

The binary smoke check must not enter the TUI. Add a minimal `--help` path that reports commands and exits successfully while preserving the interactive default.

- [ ] **Step 5: Document configuration and media behavior**

Document profile configuration without secrets, OS credential storage, keymaps and modes, mailcap precedence, Kitty opt-in, downloads, current-session history, cache locations, and troubleshooting. Do not document unsupported moderation or persistent download history as available features.

- [ ] **Step 6: Run the complete verification set**

Run:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
cargo run -- --help
cargo test --test smoke -- --test-threads=1
```

Expected: all commands exit successfully; smoke tests cover every release acceptance criterion listed in the specification.

- [ ] **Step 7: Commit release hardening**

```bash
git add .github Cargo.toml tests README.md docs
 git commit -m "test: add end-to-end smoke checks and documentation"
```

---

## Verification matrix

| Specification area | Plan coverage | Primary proof |
| --- | --- | --- |
| Modal Vim interaction | Task 2, Task 8 | `tests/input_engine.rs`, render smoke test |
| Multiple profiles and switching | Tasks 3, 4, 7, 11 | profile isolation and application tests |
| Secure authentication | Tasks 4, 6, 11 | credential-store fake, API fixtures, login tests |
| Browsing and search | Tasks 6, 7, 9 | fixture-backed adapter and application tests |
| Posting/commenting/editing/deleting | Tasks 6, 9 | mutation fixture tests and confirmation tests |
| Cache and drafts | Task 5, Task 7 | scoped-cache and failed-mutation tests |
| Mailcap and Kitty media | Task 10 | handler precedence and capability tests |
| Downloads and session history | Task 10 | cancellation, collision, and history tests |
| Error safety and terminal restoration | Tasks 7, 8, 12 | classified errors and end-to-end smoke tests |
| Linux documentation and packaging | Task 12 | CI and `cargo run -- --help` |

## Plan self-review

- **Spec coverage:** Every goal, functional requirement, privacy rule, roadmap phase, testing category, and release acceptance criterion has a named task and verification command.
- **Completeness scan:** The plan contains no unresolved stand-ins or unspecified implementation step. The local store is explicitly SQLite through `rusqlite`; each named helper is assigned to the same task's test module before its first use.
- **Type consistency:** `ProfileId`, `ProfileContext`, `Session`, `LemmyApi`, `Mutation`, `MediaHandler`, `DownloadManager`, `AppAction`, `AppState`, and `App` are introduced before later tasks consume them.
- **Scope:** The plan excludes moderation, persistent download history, offline mutation queueing, and embedded browsing as required by the approved specification.
- **Verification:** Each task has a failing test, a focused implementation, a passing command, and a commit boundary. The final smoke suite exercises the full release contract.
