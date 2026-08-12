use std::{
    ffi::OsString,
    path::PathBuf,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::Notify;

use async_trait::async_trait;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use lemmy::{
    api::{
        CommentView, FeedQuery, LemmyApi, MutationResult, Page, PostDetail, PostView, SiteInfo,
        fixtures::{fixture_api, fixture_api_with_status_count, timeout_fixture_api},
    },
    app::{
        App, Repository,
        actions::{ApiResult, AppAction, ProfileCommand, ProfileDraft, RequestIdentity},
        help::HelpIndex,
    },
    cache::{CacheStore, CachedFeed, FeedKey, MemoryCache},
    domain::{Mutation, PostId, Profile, ProfileContext, ProfileId},
    error::{AppError, Result},
    input::{Command, InputEngine},
    profiles::{CredentialStore, MemoryCredentialStore, ProfileStore},
};
use serde_json::json;
use url::Url;

fn fixture_app() -> App {
    let context = ProfileContext {
        profile: Profile {
            id: ProfileId::from("fixture"),
            instance_url: Url::parse("http://127.0.0.1/").unwrap(),
            account_label: Some("fixture".into()),
        },
        session: None,
    };
    App::new(
        Arc::new(fixture_api("feed.json")),
        Arc::new(MemoryCache::default()),
        context,
        Arc::new(MemoryCredentialStore::default()),
    )
}
fn configured_fixture_app() -> App {
    let path = std::env::temp_dir().join(format!(
        "lemmy-application-switch-{}.toml",
        std::process::id()
    ));
    let store = ProfileStore::new(&path);
    store
        .save(&[Profile {
            id: ProfileId::from("other"),
            instance_url: Url::parse("https://other.example/").unwrap(),
            account_label: Some("other".into()),
        }])
        .unwrap();
    App::with_profile_store(
        Arc::new(fixture_api("feed.json")),
        Arc::new(MemoryCache::default()),
        fixture_context(),
        Arc::new(MemoryCredentialStore::default()),
        store,
    )
}

fn failing_mutation_app() -> App {
    let context = ProfileContext {
        profile: Profile {
            id: ProfileId::from("fixture"),
            instance_url: Url::parse("http://127.0.0.1/").unwrap(),
            account_label: Some("fixture".into()),
        },
        session: None,
    };
    App::new(
        Arc::new(timeout_fixture_api()),
        Arc::new(MemoryCache::default()),
        context,
        Arc::new(MemoryCredentialStore::default()),
    )
}

/// A disposable scratch XDG root that removes itself on drop; mirrors
/// `tests/support::ScratchDir`.
struct XdgScratch {
    root: PathBuf,
}

impl XdgScratch {
    fn new(label: &str) -> Self {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "lemmy-application-{label}-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(root.join("config").join("lemmy"))
            .expect("create scratch config directory");
        std::fs::create_dir_all(root.join("cache").join("lemmy"))
            .expect("create scratch cache directory");
        Self { root }
    }
}

impl Drop for XdgScratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Serializes the whole-body XDG redirect in `XdgRedirect`.
///
/// `std::env::set_var` is not thread-safe. Parallel tests in this binary
/// resolve (but never mutate) the XDG environment while constructing apps,
/// so the redirect window only serializes against itself.
static XDG_ENV_LOCK: Mutex<()> = Mutex::new(());

/// Redirects `XDG_CONFIG_HOME` and `XDG_CACHE_HOME` to a per-test scratch
/// directory for the whole test body, restoring the previous values on drop.
///
/// Mirrors the redirect window in `tests/support::FixtureApp::build`, but
/// spans the entire body: tests that resolve the default profile store
/// (`lemmy::profiles::default_store()`, `App::new`) or the default cache
/// location after construction never read or materialize the real
/// `~/.config/lemmy/config.toml`. The window is serialized process-wide by
/// `XDG_ENV_LOCK` and the environment is restored before the scratch
/// directory is removed.
struct XdgRedirect {
    _lock: MutexGuard<'static, ()>,
    previous_config: Option<OsString>,
    previous_cache: Option<OsString>,
    /// Owned scratch directory kept alive until the environment is
    /// restored; held for its drop side effect.
    _scratch: XdgScratch,
}

impl XdgRedirect {
    fn new(label: &str) -> Self {
        let scratch = XdgScratch::new(label);
        let _lock = XDG_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous_config = std::env::var_os("XDG_CONFIG_HOME");
        let previous_cache = std::env::var_os("XDG_CACHE_HOME");
        // SAFETY: the whole redirect window is serialized by `XDG_ENV_LOCK`,
        // so no other thread reads or mutates these variables while they are
        // redirected; the previous values are restored in `Drop` before the
        // guard is released.
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", scratch.root.join("config"));
            std::env::set_var("XDG_CACHE_HOME", scratch.root.join("cache"));
        }
        Self {
            _lock,
            previous_config,
            previous_cache,
            _scratch: scratch,
        }
    }
}

impl Drop for XdgRedirect {
    fn drop(&mut self) {
        // SAFETY: still holding `XDG_ENV_LOCK`; the previous values were
        // read under the same lock above. See the comment on the matching
        // set_var calls.
        unsafe {
            match &self.previous_config {
                Some(value) => std::env::set_var("XDG_CONFIG_HOME", value),
                None => std::env::remove_var("XDG_CONFIG_HOME"),
            }
            match &self.previous_cache {
                Some(value) => std::env::set_var("XDG_CACHE_HOME", value),
                None => std::env::remove_var("XDG_CACHE_HOME"),
            }
        }
    }
}

#[test]
fn help_lists_profile_and_media_commands() {
    let help = HelpIndex::default();
    assert!(help.contains(":profile"));
    assert!(help.contains(":downloads"));
}

#[tokio::test]
async fn opening_post_preserves_feed_position_for_back_navigation() {
    let mut app = fixture_app();
    app.state.view.posts = (1..=5).map(|id| post_view(id, "post")).collect();
    app.state.select_index(4);
    app.dispatch(AppAction::OpenSelected).await.unwrap();
    app.dispatch(AppAction::Back).await.unwrap();
    assert_eq!(app.state.selected_index(), 4);
}

#[tokio::test]
async fn destructive_delete_requires_confirmation_before_api_call() {
    let (api, requests) = fixture_api_with_status_count(200);
    let mut app = App::new(
        Arc::new(api),
        Arc::new(MemoryCache::default()),
        fixture_context(),
        Arc::new(MemoryCredentialStore::default()),
    );
    app.dispatch(AppAction::DeletePost(PostId(1)))
        .await
        .unwrap();
    assert_eq!(requests.load(Ordering::SeqCst), 0);
    app.dispatch(AppAction::Confirm).await.unwrap();
    assert_eq!(requests.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn successful_post_submission_removes_draft_only_after_confirmation() {
    let mut app = fixture_app();
    app.state.view.posts = vec![post_view(1, "target")];
    app.state.select(PostId(1));
    let draft = app.state.begin_post_draft();
    app.dispatch(AppAction::SubmitDraft(draft.id.clone()))
        .await
        .unwrap();
    assert!(app.state.draft(draft.id.clone()).is_some());
    app.dispatch(AppAction::Confirm).await.unwrap();
    assert!(app.state.draft(draft.id).is_none());
}

#[tokio::test]
async fn untouched_edit_drafts_fail_local_validation() {
    let mut app = fixture_app();
    let post_draft = app.state.begin_edit_post_draft(PostId(5));
    app.dispatch(AppAction::SubmitDraft(post_draft.id.clone()))
        .await
        .unwrap();
    assert_eq!(
        app.state.status.error.as_deref(),
        Some("invalid command: post title is required")
    );
    assert!(app.state.draft(post_draft.id).is_some());
    let comment_draft = app.state.begin_edit_comment_draft(lemmy::CommentId(6));
    app.dispatch(AppAction::SubmitDraft(comment_draft.id.clone()))
        .await
        .unwrap();
    assert_eq!(
        app.state.status.error.as_deref(),
        Some("invalid command: comment content is required")
    );
    assert!(app.state.draft(comment_draft.id).is_some());
}

#[tokio::test]
async fn valid_edit_post_draft_strips_id_line_and_submits() {
    let (api, requests) = fixture_api_with_status_count(200);
    let mut app = App::new(
        Arc::new(api),
        Arc::new(MemoryCache::default()),
        fixture_context(),
        Arc::new(MemoryCredentialStore::default()),
    );
    let draft = app.state.begin_edit_post_draft(PostId(5));
    app.state
        .update_draft(&draft.id, "5\nEdited title\nEdited body")
        .unwrap();
    app.dispatch(AppAction::SubmitDraft(draft.id))
        .await
        .unwrap();
    assert_eq!(requests.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn search_submission_uses_engine_line_not_stale_compose() {
    let mut app = fixture_app();
    app.state.mode = lemmy::input::Mode::SearchForward;
    app.state.view.compose = "stale buffer".into();
    app.dispatch(AppAction::Input(lemmy::input::Command::SubmitLine(
        String::new(),
    )))
    .await
    .unwrap();
    assert_eq!(app.state.view.search, "");
    assert!(app.state.view.feed_query.search.is_none());

    app.state.mode = lemmy::input::Mode::SearchForward;
    app.state.view.compose = "stale buffer".into();
    app.dispatch(AppAction::Input(lemmy::input::Command::SubmitLine(
        "fresh query".into(),
    )))
    .await
    .unwrap();
    assert_eq!(app.state.view.search, "fresh query");
    assert_eq!(
        app.state.view.feed_query.search.as_deref(),
        Some("fresh query")
    );
}

#[tokio::test]
async fn load_more_without_next_page_clears_pending_status() {
    let mut app = fixture_app();
    app.state.view.next_page = None;
    app.state.status.pending = true;
    app.dispatch(AppAction::LoadMore).await.unwrap();
    assert!(!app.state.status.pending);
    assert_eq!(app.state.status.message, "no more posts to load");
}

#[tokio::test]
async fn open_community_clears_search_label_state() {
    let mut app = fixture_app();
    app.state.view.search = "rust".into();
    app.dispatch(AppAction::OpenCommunity(lemmy::CommunityId(3)))
        .await
        .unwrap();
    assert!(app.state.view.search.is_empty());
    assert_eq!(
        app.state.view.feed_query.community,
        Some(lemmy::CommunityId(3))
    );
}

#[tokio::test]
async fn create_post_draft_wires_title_link_and_body() {
    let api = Arc::new(CapturingPostApi::default());
    let mut app = App::new(
        api.clone(),
        Arc::new(MemoryCache::default()),
        fixture_context(),
        Arc::new(MemoryCredentialStore::default()),
    );
    let mut post = post_view(7, "target");
    post.community_id = lemmy::CommunityId(3);
    app.state.view.posts = vec![post];
    app.state.select(PostId(7));
    let draft = app.state.begin_post_draft();
    app.state
        .update_draft(
            &draft.id,
            "A post title\nhttps://example.com/article\nSome body",
        )
        .unwrap();
    app.dispatch(AppAction::SubmitDraft(draft.id.clone()))
        .await
        .unwrap();
    app.dispatch(AppAction::Confirm).await.unwrap();
    let captured = api
        .captured
        .lock()
        .unwrap()
        .clone()
        .expect("create post mutation captured");
    match captured {
        Mutation::CreatePost(request) => {
            assert_eq!(request.name, "A post title");
            assert_eq!(
                request.url,
                Some(Url::parse("https://example.com/article").unwrap())
            );
            assert_eq!(request.body.as_deref(), Some("Some body"));
            assert_eq!(request.community, lemmy::CommunityId(3));
        }
        other => panic!("expected CreatePost, got {other:?}"),
    }
}

#[tokio::test]
async fn create_post_without_community_target_fails_before_request() {
    let (api, requests) = fixture_api_with_status_count(200);
    let mut app = App::new(
        Arc::new(api),
        Arc::new(MemoryCache::default()),
        fixture_context(),
        Arc::new(MemoryCredentialStore::default()),
    );
    let draft = app.state.begin_post_draft();
    app.dispatch(AppAction::SubmitDraft(draft.id))
        .await
        .unwrap();
    assert_eq!(requests.load(Ordering::SeqCst), 0);
    assert_eq!(
        app.state.status.error.as_deref(),
        Some("select a post in the target community before creating a post")
    );
}

#[tokio::test]
async fn profile_switch_changes_request_context_and_clears_selection() {
    let mut app = configured_fixture_app();
    app.state.select(lemmy::PostId(1));
    app.dispatch(AppAction::Profile(ProfileCommand::Switch(ProfileId::from(
        "other",
    ))))
    .await
    .unwrap();
    assert_eq!(app.state.active.profile.id, ProfileId::from("other"));
    assert!(app.state.selected_post().is_none());
}

#[tokio::test]
async fn failed_mutation_keeps_draft_and_sets_retryable_status() {
    let mut app = failing_mutation_app();
    let draft = app.state.begin_comment_draft();
    app.dispatch(AppAction::SubmitDraft(draft.id.clone()))
        .await
        .unwrap();
    assert!(app.state.draft(draft.id).is_some());
    assert!(app.state.status.is_retryable());
}

#[tokio::test]
async fn delete_is_staged_until_confirmed_and_cancelled_once() {
    let (api, requests) = fixture_api_with_status_count(200);
    let mut app = App::new(
        Arc::new(api),
        Arc::new(MemoryCache::default()),
        fixture_context(),
        Arc::new(MemoryCredentialStore::default()),
    );
    app.dispatch(AppAction::DeletePost(PostId(1)))
        .await
        .unwrap();
    assert_eq!(requests.load(Ordering::SeqCst), 0);
    assert!(app.state.pending.is_some());
    app.dispatch(AppAction::Cancel).await.unwrap();
    assert!(app.state.pending.is_none());
    app.dispatch(AppAction::Confirm).await.unwrap();
    assert_eq!(requests.load(Ordering::SeqCst), 0);

    app.dispatch(AppAction::DeletePost(PostId(1)))
        .await
        .unwrap();
    app.dispatch(AppAction::Confirm).await.unwrap();
    assert_eq!(requests.load(Ordering::SeqCst), 1);
    app.dispatch(AppAction::Confirm).await.unwrap();
    assert_eq!(requests.load(Ordering::SeqCst), 1);
}

/// P0 regression: the confirmation gate is user-reachable. Pressing `y`
/// through the real input engine confirms a staged destructive action
/// (dispatching the mutation), and `n` cancels it without any API call.
#[tokio::test]
async fn confirm_and_cancel_keys_drive_staged_destructive_action() {
    let (api, requests) = fixture_api_with_status_count(200);
    let mut app = App::new(
        Arc::new(api),
        Arc::new(MemoryCache::default()),
        fixture_context(),
        Arc::new(MemoryCredentialStore::default()),
    );

    // Confirm path: stage a delete, press `y` through the real engine.
    app.dispatch(AppAction::DeletePost(PostId(1)))
        .await
        .unwrap();
    assert!(app.state.status.confirmation_pending);
    assert_eq!(requests.load(Ordering::SeqCst), 0);
    let mut engine = InputEngine::new();
    let command = engine.handle(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
    assert_eq!(command, Command::Confirm);
    app.dispatch(AppAction::Input(command)).await.unwrap();
    assert_eq!(
        requests.load(Ordering::SeqCst),
        1,
        "confirming with y must dispatch the staged mutation"
    );
    assert!(!app.state.status.confirmation_pending);
    assert!(app.state.pending.is_none());

    // Cancel path: stage another delete, press `n` through the real engine.
    app.dispatch(AppAction::DeletePost(PostId(1)))
        .await
        .unwrap();
    assert!(app.state.status.confirmation_pending);
    let mut engine = InputEngine::new();
    let command = engine.handle(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));
    assert_eq!(command, Command::Cancel);
    app.dispatch(AppAction::Input(command)).await.unwrap();
    assert!(!app.state.status.confirmation_pending);
    assert!(app.state.pending.is_none());
    assert_eq!(
        requests.load(Ordering::SeqCst),
        1,
        "cancelling with n must never dispatch the mutation"
    );

    // With nothing pending, y/n are no-ops (no API call, no status churn).
    let mut engine = InputEngine::new();
    app.dispatch(AppAction::Input(
        engine.handle(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE)),
    ))
    .await
    .unwrap();
    app.dispatch(AppAction::Input(
        engine.handle(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE)),
    ))
    .await
    .unwrap();
    assert_eq!(requests.load(Ordering::SeqCst), 1);
}

/// The `:confirm`/`:yes`/`:cancel` command arms are the command-line path to
/// the same confirmation gate.
#[tokio::test]
async fn confirm_and_cancel_commands_resolve_staged_destructive_action() {
    let (api, requests) = fixture_api_with_status_count(200);
    let mut app = App::new(
        Arc::new(api),
        Arc::new(MemoryCache::default()),
        fixture_context(),
        Arc::new(MemoryCredentialStore::default()),
    );

    app.dispatch(AppAction::DeletePost(PostId(1)))
        .await
        .unwrap();
    assert!(app.state.status.confirmation_pending);
    app.dispatch(AppAction::Input(Command::SubmitLine("yes".into())))
        .await
        .unwrap();
    assert_eq!(requests.load(Ordering::SeqCst), 1);
    assert!(!app.state.status.confirmation_pending);

    // `:confirm` with nothing staged is a failure, not a network call.
    app.dispatch(AppAction::Input(Command::SubmitLine("confirm".into())))
        .await
        .unwrap();
    assert_eq!(
        app.state.status.error.as_deref(),
        Some("nothing to confirm")
    );
    assert_eq!(requests.load(Ordering::SeqCst), 1);

    app.dispatch(AppAction::DeletePost(PostId(1)))
        .await
        .unwrap();
    app.dispatch(AppAction::Input(Command::SubmitLine("cancel".into())))
        .await
        .unwrap();
    assert!(app.state.pending.is_none());
    assert!(!app.state.status.confirmation_pending);
    assert_eq!(requests.load(Ordering::SeqCst), 1);
}

/// Submitting a `/` or `?` search resets the pagination cursor: after a
/// failed search, LoadMore must be refused instead of reusing the previous
/// feed's stale `next_page`.
#[tokio::test]
async fn failed_search_resets_stale_cursor_so_load_more_is_refused() {
    let (api, requests) = fixture_api_with_status_count(500);
    let mut app = App::new(
        Arc::new(api),
        Arc::new(MemoryCache::default()),
        fixture_context(),
        Arc::new(MemoryCredentialStore::default()),
    );
    app.state.view.posts = vec![post_view(1, "old feed")];
    app.state.view.next_page = Some("2".to_owned());

    // Drive `/rust` through the real input engine: `/`, text, Enter.
    let mut engine = InputEngine::new();
    let slash = engine.handle(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
    assert_eq!(slash, Command::EnterSearch { backward: false });
    app.dispatch(AppAction::Input(slash)).await.unwrap();
    for character in "rust".chars() {
        app.dispatch(AppAction::Input(
            engine.handle(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE)),
        ))
        .await
        .unwrap();
    }
    let enter = engine.handle(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(enter, Command::SubmitLine("rust".into()));
    app.dispatch(AppAction::Input(enter)).await.unwrap();

    // The search itself failed (500); the stale cursor must be gone and
    // LoadMore must be refused without touching the network.
    assert!(app.state.status.error.is_some());
    assert_eq!(
        app.state.view.next_page, None,
        "a new search must reset the previous feed's next_page"
    );
    let calls_before = requests.load(Ordering::SeqCst);
    app.dispatch(AppAction::LoadMore).await.unwrap();
    assert_eq!(app.state.status.message, "no more posts to load");
    assert_eq!(
        requests.load(Ordering::SeqCst),
        calls_before,
        "LoadMore after a failed search must not reuse the stale cursor"
    );
}

/// A profile-store read failure during `:profile <id>` must surface in the
/// status line instead of terminating the TUI.
#[tokio::test]
async fn switch_profile_surfaces_store_read_failure_in_status() {
    let path = std::env::temp_dir().join(format!(
        "lemmy-application-broken-switch-{}.toml",
        std::process::id()
    ));
    std::fs::write(&path, "this is not [valid toml").unwrap();
    let store = ProfileStore::new(&path);
    let mut app = App::with_profile_store(
        Arc::new(fixture_api("feed.json")),
        Arc::new(MemoryCache::default()),
        fixture_context(),
        Arc::new(MemoryCredentialStore::default()),
        store,
    );

    let result = app
        .dispatch(AppAction::Profile(ProfileCommand::Switch(ProfileId::from(
            "other",
        ))))
        .await;
    assert!(
        result.is_ok(),
        "a profile-store read failure must not terminate the TUI"
    );
    assert!(
        app.state.status.error.is_some(),
        "the read failure must be surfaced via status.failure"
    );
    assert_eq!(
        app.state.active.profile.id,
        ProfileId::from("fixture"),
        "the active profile must be unchanged"
    );
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn stale_same_profile_post_result_is_rejected_by_request_token() {
    let mut app = fixture_app();
    app.state.view.posts = vec![post_view(1, "selected")];
    app.state.select(PostId(1));
    let old = app.begin_request(RequestIdentity::Post(PostId(1)));
    let current = app.begin_request(RequestIdentity::Post(PostId(1)));
    let detail = |title| PostDetail {
        post: PostView {
            id: PostId(1),
            title,
            body: None,
            url: None,
            community_id: lemmy::CommunityId(1),
            creator_id: lemmy::UserId(1),
            score: 0,
            comments: 0,
            published: None,
        },
        comments: Vec::new(),
    };
    app.dispatch(AppAction::ApiResult(Box::new(ApiResult::Post {
        profile: ProfileId::from("fixture"),
        request: current,
        result: Ok(detail("current".into())),
    })))
    .await
    .unwrap();
    app.dispatch(AppAction::ApiResult(Box::new(ApiResult::Post {
        profile: ProfileId::from("fixture"),
        request: old,
        result: Ok(detail("old".into())),
    })))
    .await
    .unwrap();
    assert_eq!(app.state.view.detail.clone().unwrap().post.title, "current");
}

#[tokio::test]
async fn stale_comments_for_old_post_do_not_overwrite_active_detail() {
    let mut app = fixture_app();
    app.state.view.detail = Some(PostDetail {
        post: PostView {
            id: PostId(2),
            title: "active".into(),
            body: None,
            url: None,
            community_id: lemmy::CommunityId(1),
            creator_id: lemmy::UserId(1),
            score: 0,
            comments: 0,
            published: None,
        },
        comments: Vec::new(),
    });
    let old = app.begin_request(RequestIdentity::Comments(PostId(1)));
    let comment = CommentView {
        id: lemmy::CommentId(1),
        post_id: PostId(1),
        content: "stale".into(),
        creator_name: "alice".into(),
        creator_id: lemmy::UserId(1),
        score: 0,
    };
    app.dispatch(AppAction::ApiResult(Box::new(ApiResult::Comments {
        profile: ProfileId::from("fixture"),
        request: old,
        post: PostId(1),
        result: Ok(vec![comment]),
    })))
    .await
    .unwrap();
    assert!(app.state.selected_comments().is_empty());
}

#[tokio::test]
async fn back_invalidates_inflight_post_result() {
    let mut app = fixture_app();
    let request = app.begin_request(RequestIdentity::Post(PostId(1)));
    app.dispatch(AppAction::Back).await.unwrap();
    let detail = PostDetail {
        post: post_view(1, "stale"),
        comments: Vec::new(),
    };
    app.dispatch(AppAction::ApiResult(Box::new(ApiResult::Post {
        profile: ProfileId::from("fixture"),
        request,
        result: Ok(detail),
    })))
    .await
    .unwrap();
    assert!(app.state.view.detail.is_none());
}

#[tokio::test]
async fn post_result_requires_current_selected_post_context() {
    let mut app = fixture_app();
    app.state.view.posts = vec![post_view(1, "one"), post_view(2, "two")];
    app.state.select(PostId(1));
    let request = app.begin_request(RequestIdentity::Post(PostId(1)));
    app.state.select(PostId(2));
    app.dispatch(AppAction::ApiResult(Box::new(ApiResult::Post {
        profile: ProfileId::from("fixture"),
        request,
        result: Ok(PostDetail {
            post: post_view(1, "stale"),
            comments: Vec::new(),
        }),
    })))
    .await
    .unwrap();
    assert!(app.state.view.detail.is_none());
}

#[tokio::test]
async fn back_invalidates_inflight_comments_result() {
    let mut app = fixture_app();
    app.state.view.posts = vec![post_view(1, "one")];
    app.state.select(PostId(1));
    app.state.view.detail = Some(PostDetail {
        post: post_view(1, "one"),
        comments: Vec::new(),
    });
    let request = app.begin_request(RequestIdentity::Comments(PostId(1)));
    app.dispatch(AppAction::Back).await.unwrap();
    app.state.view.detail = Some(PostDetail {
        post: post_view(1, "reopened"),
        comments: Vec::new(),
    });
    let comment = CommentView {
        id: lemmy::CommentId(1),
        post_id: PostId(1),
        content: "stale".into(),
        creator_name: "alice".into(),
        creator_id: lemmy::UserId(1),
        score: 0,
    };
    app.dispatch(AppAction::ApiResult(Box::new(ApiResult::Comments {
        profile: ProfileId::from("fixture"),
        request,
        post: PostId(1),
        result: Ok(vec![comment]),
    })))
    .await
    .unwrap();
    assert!(app.state.selected_comments().is_empty());
}

#[tokio::test]
async fn async_feed_refresh_updates_state_without_reinserting_confirmed_delete() {
    let cache = Arc::new(MemoryCache::default());
    let context = fixture_context();
    let cached = CachedFeed::new(
        json!({ "items": [{ "id": 1, "title": "target", "body": null, "url": null, "community_id": 1, "creator_id": 1, "score": 1, "comments": 0, "published": null }], "next_page": null }),
        1,
        false,
    );
    cache
        .write_feed(&context.profile.id, &FeedKey::from("home"), &cached)
        .unwrap();
    let api = Arc::new(RefreshRaceApi::default());
    let mut app = App::new(
        api.clone(),
        cache,
        context,
        Arc::new(MemoryCredentialStore::default()),
    );
    app.state.view.posts = vec![post_view(1, "target")];
    app.dispatch(AppAction::Input(lemmy::input::Command::Refresh))
        .await
        .unwrap();
    api.started.notified().await;
    app.dispatch(AppAction::DeletePost(PostId(1)))
        .await
        .unwrap();
    app.dispatch(AppAction::Confirm).await.unwrap();
    api.release.notify_one();
    tokio::time::sleep(Duration::from_millis(20)).await;
    app.dispatch(AppAction::Tick).await.unwrap();
    assert_eq!(
        app.state
            .view
            .posts
            .iter()
            .map(|post| post.id)
            .collect::<Vec<_>>(),
        vec![PostId(2)]
    );
    assert_eq!(app.state.view.posts[0].title, "refreshed");
}

#[tokio::test]
async fn logout_invalidates_pending_results() {
    let mut app = fixture_app();
    let request = app.begin_request(RequestIdentity::Feed);
    app.dispatch(AppAction::Profile(ProfileCommand::Logout))
        .await
        .unwrap();
    app.dispatch(AppAction::ApiResult(Box::new(ApiResult::Feed {
        profile: ProfileId::from("fixture"),
        request,
        result: Ok(Page {
            items: vec![post_view(1, "stale")],
            next_page: None,
        }),
        stale: false,
    })))
    .await
    .unwrap();
    assert!(app.state.view.posts.is_empty());
}
#[tokio::test]
async fn logout_invalidates_inflight_authenticated_refresh_before_cache_write() {
    let cache = Arc::new(MemoryCache::default());
    let context = ProfileContext {
        profile: fixture_context().profile,
        session: Some(lemmy::Session {
            token: lemmy::SecretString::from("authenticated"),
            user_id: lemmy::UserId(7),
        }),
    };
    let id = context.profile.id.clone();
    cache
        .write_feed(
            &id,
            &FeedKey::from("home"),
            &CachedFeed::new(
                json!({ "items": [post_json(1, "cached")], "next_page": null }),
                1,
                false,
            ),
        )
        .unwrap();
    let api = Arc::new(ProfileReplacementRaceApi::default());
    let credentials = Arc::new(MemoryCredentialStore::default());
    credentials
        .put_session(&id, context.session.as_ref().unwrap())
        .await
        .unwrap();
    let mut app = App::new(api.clone(), cache.clone(), context, credentials);

    app.dispatch(AppAction::Input(lemmy::input::Command::Refresh))
        .await
        .unwrap();
    api.started.notified().await;
    app.dispatch(AppAction::Profile(ProfileCommand::Logout))
        .await
        .unwrap();
    assert!(app.state.active.session.is_none());

    api.release.notify_one();
    api.finished.notified().await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    let cached = cache
        .read_feed(&id, &FeedKey::from("home"))
        .unwrap()
        .unwrap();
    assert_eq!(cached.entity["items"][0]["title"], "cached");
}

#[tokio::test]
async fn new_profile_invalidates_pending_results_and_persists_metadata() {
    let path =
        std::env::temp_dir().join(format!("lemmy-application-new-{}.toml", std::process::id()));
    let store = ProfileStore::new(&path);
    let mut app = App::with_profile_store(
        Arc::new(fixture_api("feed.json")),
        Arc::new(MemoryCache::default()),
        fixture_context(),
        Arc::new(MemoryCredentialStore::default()),
        store.clone(),
    );
    let request = app.begin_request(RequestIdentity::Feed);
    let draft = ProfileDraft {
        id: ProfileId::from("new-profile"),
        instance_url: Url::parse("https://new.example/lemmy").unwrap(),
        account_label: Some("New account".into()),
    };
    app.dispatch(AppAction::Profile(ProfileCommand::New(draft)))
        .await
        .unwrap();
    app.dispatch(AppAction::ApiResult(Box::new(ApiResult::Feed {
        profile: ProfileId::from("fixture"),
        request,
        result: Ok(Page {
            items: vec![post_view(1, "stale")],
            next_page: None,
        }),
        stale: false,
    })))
    .await
    .unwrap();
    let profiles = store.load().unwrap();
    assert_eq!(
        profiles,
        vec![Profile {
            id: ProfileId::from("new-profile"),
            instance_url: Url::parse("https://new.example/lemmy").unwrap(),
            account_label: Some("New account".into())
        }]
    );
    assert!(app.state.view.posts.is_empty());
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn comment_submission_without_selected_post_fails_before_request() {
    let (api, requests) = fixture_api_with_status_count(200);
    let mut app = App::new(
        Arc::new(api),
        Arc::new(MemoryCache::default()),
        fixture_context(),
        Arc::new(MemoryCredentialStore::default()),
    );
    let draft = app.state.begin_comment_draft();
    app.dispatch(AppAction::SubmitDraft(draft.id))
        .await
        .unwrap();
    assert_eq!(requests.load(Ordering::SeqCst), 0);
    assert_eq!(
        app.state.status.error.as_deref(),
        Some("select a post before submitting a comment")
    );
}

#[derive(Default)]
struct RefreshRaceApi {
    started: Arc<Notify>,
    release: Arc<Notify>,
}

#[async_trait]
impl LemmyApi for RefreshRaceApi {
    async fn site(&self, _: &ProfileContext) -> Result<SiteInfo> {
        Err(AppError::Network("unused".into()))
    }
    async fn feed(&self, _: &ProfileContext, _: FeedQuery) -> Result<Page<PostView>> {
        self.started.notify_one();
        self.release.notified().await;
        Ok(Page {
            items: vec![
                post_view(1, "deleted from refresh"),
                post_view(2, "refreshed"),
            ],
            next_page: None,
        })
    }
    async fn post(&self, _: &ProfileContext, _: PostId) -> Result<PostDetail> {
        Err(AppError::Network("unused".into()))
    }
    async fn comments(&self, _: &ProfileContext, _: PostId) -> Result<Vec<CommentView>> {
        Ok(Vec::new())
    }
    async fn login(&self, _: lemmy::api::LoginRequest) -> Result<lemmy::Session> {
        Err(AppError::Network("unused".into()))
    }
    async fn mutate(&self, _: &ProfileContext, mutation: Mutation) -> Result<MutationResult> {
        match mutation {
            Mutation::DeletePost(id) => Ok(MutationResult {
                success: true,
                post: Some(post_view(id.0, "returned")),
                comment: None,
                message: None,
            }),
            _ => Err(AppError::Network("unexpected mutation".into())),
        }
    }
}

#[derive(Default)]
struct GenerationRaceApi {
    calls: Arc<AtomicUsize>,
    first_release: Arc<Notify>,
    second_release: Arc<Notify>,
}

#[async_trait]
impl LemmyApi for GenerationRaceApi {
    async fn site(&self, _: &ProfileContext) -> Result<SiteInfo> {
        Err(AppError::Network("unused".into()))
    }
    async fn feed(&self, _: &ProfileContext, _: FeedQuery) -> Result<Page<PostView>> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if call == 0 {
            self.first_release.notified().await;
            Ok(Page {
                items: vec![post_view(1, "older")],
                next_page: None,
            })
        } else {
            self.second_release.notified().await;
            Ok(Page {
                items: vec![post_view(1, "newer")],
                next_page: None,
            })
        }
    }
    async fn post(&self, _: &ProfileContext, _: PostId) -> Result<PostDetail> {
        Err(AppError::Network("unused".into()))
    }
    async fn comments(&self, _: &ProfileContext, _: PostId) -> Result<Vec<CommentView>> {
        Ok(Vec::new())
    }
    async fn login(&self, _: lemmy::api::LoginRequest) -> Result<lemmy::Session> {
        Err(AppError::Network("unused".into()))
    }
    async fn mutate(&self, _: &ProfileContext, _: Mutation) -> Result<MutationResult> {
        Err(AppError::Network("unused".into()))
    }
}

struct SuccessfulCommentApi;

#[async_trait]
impl LemmyApi for SuccessfulCommentApi {
    async fn site(&self, _: &ProfileContext) -> Result<SiteInfo> {
        Err(AppError::Network("unused".into()))
    }
    async fn feed(&self, _: &ProfileContext, _: FeedQuery) -> Result<Page<PostView>> {
        Err(AppError::Network("unused".into()))
    }
    async fn post(&self, _: &ProfileContext, _: PostId) -> Result<PostDetail> {
        Err(AppError::Network("unused".into()))
    }
    async fn comments(&self, _: &ProfileContext, _: PostId) -> Result<Vec<CommentView>> {
        Ok(Vec::new())
    }
    async fn login(&self, _: lemmy::api::LoginRequest) -> Result<lemmy::Session> {
        Err(AppError::Network("unused".into()))
    }
    async fn mutate(&self, _: &ProfileContext, _: Mutation) -> Result<MutationResult> {
        Ok(MutationResult {
            success: true,
            post: None,
            comment: None,
            message: None,
        })
    }
}

#[derive(Clone, Default)]
struct CapturingPostApi {
    captured: Arc<std::sync::Mutex<Option<Mutation>>>,
}

#[async_trait]
impl LemmyApi for CapturingPostApi {
    async fn site(&self, _: &ProfileContext) -> Result<SiteInfo> {
        Err(AppError::Network("unused".into()))
    }
    async fn feed(&self, _: &ProfileContext, _: FeedQuery) -> Result<Page<PostView>> {
        Err(AppError::Network("unused".into()))
    }
    async fn post(&self, _: &ProfileContext, _: PostId) -> Result<PostDetail> {
        Err(AppError::Network("unused".into()))
    }
    async fn comments(&self, _: &ProfileContext, _: PostId) -> Result<Vec<CommentView>> {
        Ok(Vec::new())
    }
    async fn login(&self, _: lemmy::api::LoginRequest) -> Result<lemmy::Session> {
        Err(AppError::Network("unused".into()))
    }
    async fn mutate(&self, _: &ProfileContext, mutation: Mutation) -> Result<MutationResult> {
        *self.captured.lock().unwrap() = Some(mutation);
        Ok(MutationResult {
            success: true,
            post: None,
            comment: None,
            message: None,
        })
    }
}

#[derive(Clone, Default)]
struct ThreadApi {
    post_calls: Arc<AtomicUsize>,
    comments_calls: Arc<AtomicUsize>,
}

#[async_trait]
impl LemmyApi for ThreadApi {
    async fn site(&self, _: &ProfileContext) -> Result<SiteInfo> {
        Err(AppError::Network("unused".into()))
    }
    async fn feed(&self, _: &ProfileContext, _: FeedQuery) -> Result<Page<PostView>> {
        Err(AppError::Network("unused".into()))
    }
    async fn post(&self, _: &ProfileContext, id: PostId) -> Result<PostDetail> {
        self.post_calls.fetch_add(1, Ordering::SeqCst);
        Ok(PostDetail {
            post: PostView {
                id,
                title: "Threaded post".into(),
                body: Some("The full post body".into()),
                url: None,
                community_id: lemmy::CommunityId(1),
                creator_id: lemmy::UserId(1),
                score: 5,
                comments: 2,
                published: None,
            },
            comments: Vec::new(),
        })
    }
    async fn comments(&self, _: &ProfileContext, id: PostId) -> Result<Vec<CommentView>> {
        self.comments_calls.fetch_add(1, Ordering::SeqCst);
        Ok(vec![CommentView {
            id: lemmy::CommentId(10),
            post_id: id,
            content: "A real comment".into(),
            creator_name: "alice".into(),
            creator_id: lemmy::UserId(2),
            score: 3,
        }])
    }
    async fn login(&self, _: lemmy::api::LoginRequest) -> Result<lemmy::Session> {
        Err(AppError::Network("unused".into()))
    }
    async fn mutate(&self, _: &ProfileContext, _: Mutation) -> Result<MutationResult> {
        Err(AppError::Network("unused".into()))
    }
}

#[tokio::test]
async fn opening_post_fetches_detail_and_thread_comments() {
    let api = Arc::new(ThreadApi::default());
    let mut app = App::new(
        api.clone(),
        Arc::new(MemoryCache::default()),
        fixture_context(),
        Arc::new(MemoryCredentialStore::default()),
    );
    app.state.view.posts = vec![post_view(1, "Threaded post")];
    app.state.view.selected = Some(0);
    app.dispatch(AppAction::OpenSelected).await.unwrap();
    let detail = app.state.view.detail.clone().expect("detail loads");
    assert_eq!(
        detail.post.body.as_deref(),
        Some("The full post body"),
        "the post body must be part of the opened detail"
    );
    assert_eq!(
        detail.comments.len(),
        1,
        "opening a post must fetch the thread comments"
    );
    assert_eq!(detail.comments[0].content, "A real comment");
    assert!(app.state.status.message.contains("comments loaded"));
    assert_eq!(api.post_calls.load(Ordering::SeqCst), 1);
    assert_eq!(api.comments_calls.load(Ordering::SeqCst), 1);
}

#[derive(Clone, Default)]
struct PagedFeedApi {
    second_page_calls: Arc<AtomicUsize>,
}

#[async_trait]
impl LemmyApi for PagedFeedApi {
    async fn site(&self, _: &ProfileContext) -> Result<SiteInfo> {
        Err(AppError::Network("unused".into()))
    }
    async fn feed(&self, _: &ProfileContext, query: FeedQuery) -> Result<Page<PostView>> {
        if query.page.as_deref() == Some("2") {
            self.second_page_calls.fetch_add(1, Ordering::SeqCst);
            Ok(Page {
                items: vec![post_view(2, "second page post")],
                next_page: None,
            })
        } else {
            Ok(Page {
                items: vec![post_view(1, "first page post")],
                next_page: Some("2".to_owned()),
            })
        }
    }
    async fn post(&self, _: &ProfileContext, _: PostId) -> Result<PostDetail> {
        Err(AppError::Network("unused".into()))
    }
    async fn comments(&self, _: &ProfileContext, _: PostId) -> Result<Vec<CommentView>> {
        Ok(Vec::new())
    }
    async fn login(&self, _: lemmy::api::LoginRequest) -> Result<lemmy::Session> {
        Err(AppError::Network("unused".into()))
    }
    async fn mutate(&self, _: &ProfileContext, _: Mutation) -> Result<MutationResult> {
        Err(AppError::Network("unused".into()))
    }
}

#[tokio::test]
async fn next_page_command_appends_the_following_feed_page() {
    let api = Arc::new(PagedFeedApi::default());
    let mut app = App::new(
        api.clone(),
        Arc::new(MemoryCache::default()),
        fixture_context(),
        Arc::new(MemoryCredentialStore::default()),
    );
    app.state.view.posts = vec![post_view(1, "first page post")];
    app.state.view.next_page = Some("2".to_owned());
    app.dispatch(AppAction::Input(Command::NextPage))
        .await
        .unwrap();
    assert_eq!(
        app.state.view.posts.len(),
        2,
        "the next page is appended to the feed"
    );
    assert_eq!(app.state.view.posts[1].id, PostId(2));
    assert!(app.state.view.next_page.is_none());
    assert!(app.state.status.message.contains("more posts loaded"));
    assert_eq!(api.second_page_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn startup_feed_command_loads_the_home_feed() {
    let api = Arc::new(lemmy::api::fixtures::fixture_api("feed.json"));
    let mut app = App::new(
        api,
        Arc::new(MemoryCache::default()),
        fixture_context(),
        Arc::new(MemoryCredentialStore::default()),
    );
    // This is the exact dispatch the configured `startup` action performs
    // before the event loop draws its first frame.
    app.dispatch(AppAction::Input(Command::SubmitLine("feed".into())))
        .await
        .unwrap();
    assert_eq!(
        app.state.view.posts.len(),
        2,
        "the startup feed action loads the home feed"
    );
    assert!(app.state.status.message.contains("feed loaded"));
}

#[tokio::test]
async fn detail_scroll_commands_move_and_clamp_the_offset() {
    let api = Arc::new(ThreadApi::default());
    let mut app = App::new(
        api.clone(),
        Arc::new(MemoryCache::default()),
        fixture_context(),
        Arc::new(MemoryCredentialStore::default()),
    );
    app.state.view.posts = vec![post_view(1, "Threaded post")];
    app.state.view.selected = Some(0);
    app.dispatch(AppAction::OpenSelected).await.unwrap();
    assert_eq!(app.state.view.detail_scroll, 0, "opening resets the scroll");

    app.dispatch(AppAction::Input(Command::ScrollDetailDown { count: 2 }))
        .await
        .unwrap();
    assert_eq!(app.state.view.detail_scroll, 20);
    app.dispatch(AppAction::Input(Command::ScrollDetailUp { count: 1 }))
        .await
        .unwrap();
    assert_eq!(app.state.view.detail_scroll, 10);
    app.dispatch(AppAction::Input(Command::ScrollDetailUp { count: 9 }))
        .await
        .unwrap();
    assert_eq!(
        app.state.view.detail_scroll, 0,
        "scrolling up clamps at zero"
    );

    // Without an open detail the scroll commands are inert.
    app.dispatch(AppAction::Back).await.unwrap();
    app.dispatch(AppAction::Input(Command::ScrollDetailDown { count: 1 }))
        .await
        .unwrap();
    assert_eq!(app.state.view.detail_scroll, 0);
}

#[tokio::test]
async fn confirmed_delete_removes_target_from_feed_and_cache() {
    let cache = Arc::new(MemoryCache::default());
    let context = fixture_context();
    let before = CachedFeed::new(
        json!({ "items": [
        { "id": 1, "title": "target", "body": null, "url": null, "community_id": 1, "creator_id": 1, "score": 1, "comments": 0, "published": null },
        { "id": 2, "title": "survivor", "body": null, "url": null, "community_id": 1, "creator_id": 1, "score": 1, "comments": 0, "published": null }
    ], "next_page": null }),
        1,
        false,
    );
    cache
        .write_feed(&context.profile.id, &FeedKey::from("home"), &before)
        .unwrap();
    let mut app = App::new(
        Arc::new(ConfirmedDeleteApi),
        cache.clone(),
        context,
        Arc::new(MemoryCredentialStore::default()),
    );
    app.state.view.posts = vec![post_view(1, "target"), post_view(2, "survivor")];
    app.dispatch(AppAction::DeletePost(PostId(1)))
        .await
        .unwrap();
    app.dispatch(AppAction::Confirm).await.unwrap();
    assert_eq!(
        app.state
            .view
            .posts
            .iter()
            .map(|post| post.id)
            .collect::<Vec<_>>(),
        vec![PostId(2)]
    );
    let cached = cache
        .read_feed(&ProfileId::from("fixture"), &FeedKey::from("home"))
        .unwrap()
        .unwrap();
    assert_eq!(
        cached.entity["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|post| post["id"].as_i64().unwrap())
            .collect::<Vec<_>>(),
        vec![2]
    );
}

#[tokio::test]
async fn stale_comments_error_for_inactive_post_does_not_change_status() {
    let mut app = fixture_app();
    app.state.view.detail = Some(PostDetail {
        post: post_view(2, "active"),
        comments: Vec::new(),
    });
    app.state.status.success("active detail");
    let old = app.begin_request(RequestIdentity::Comments(PostId(1)));
    let _current = app.begin_request(RequestIdentity::Comments(PostId(2)));
    app.dispatch(AppAction::ApiResult(Box::new(ApiResult::Comments {
        profile: ProfileId::from("fixture"),
        request: old,
        post: PostId(1),
        result: Err(AppError::Network("stale comments".into())),
    })))
    .await
    .unwrap();
    assert_eq!(app.state.status.message, "active detail");
    assert!(app.state.status.error.is_none());
}

#[tokio::test]
async fn switch_profile_uses_destination_store_metadata() {
    let path = std::env::temp_dir().join(format!("lemmy-application-{}.toml", std::process::id()));
    let store = ProfileStore::new(&path);
    store
        .save(&[Profile {
            id: ProfileId::from("destination"),
            instance_url: Url::parse("https://remote.example/lemmy").unwrap(),
            account_label: Some("remote account".into()),
        }])
        .unwrap();
    let mut app = App::with_profile_store(
        Arc::new(fixture_api("feed.json")),
        Arc::new(MemoryCache::default()),
        fixture_context(),
        Arc::new(MemoryCredentialStore::default()),
        store,
    );
    app.dispatch(AppAction::Profile(ProfileCommand::Switch(ProfileId::from(
        "destination",
    ))))
    .await
    .unwrap();
    assert_eq!(
        app.state.active.profile.instance_url,
        Url::parse("https://remote.example/lemmy").unwrap()
    );
    assert_eq!(
        app.state.active.profile.account_label.as_deref(),
        Some("remote account")
    );
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn feed_returns_cached_content_before_slow_refresh_and_marks_stale_on_failure() {
    let cache = Arc::new(MemoryCache::default());
    let context = fixture_context();
    let cached = CachedFeed::new(
        json!({ "items": [{ "id": 1, "title": "cached", "body": null, "url": null, "community_id": 1, "creator_id": 1, "score": 1, "comments": 0, "published": null }], "next_page": null }),
        1,
        false,
    );
    cache
        .write_feed(&context.profile.id, &FeedKey::from("home"), &cached)
        .unwrap();
    let repository = Repository::new(
        Arc::new(timeout_fixture_api()),
        cache.clone(),
        Arc::new(MemoryCredentialStore::default()),
    );
    let read = tokio::time::timeout(
        Duration::from_millis(100),
        repository.feed(&context, FeedQuery::home()),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(read.value.items[0].title, "cached");
    assert!(read.stale);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        cache
            .read_feed(&context.profile.id, &FeedKey::from("home"))
            .unwrap()
            .unwrap()
            .stale
    );
}

#[tokio::test]
async fn unsuccessful_mutation_does_not_update_cached_post() {
    let cache = Arc::new(MemoryCache::default());
    let context = fixture_context();
    let before = CachedFeed::new(
        json!({ "items": [{ "id": 1, "title": "original", "body": null, "url": null, "community_id": 1, "creator_id": 1, "score": 1, "comments": 0, "published": null }], "next_page": null }),
        1,
        false,
    );
    cache
        .write_feed(&context.profile.id, &FeedKey::from("home"), &before)
        .unwrap();
    let repository = Repository::new(
        Arc::new(UnconfirmedApi),
        cache.clone(),
        Arc::new(MemoryCredentialStore::default()),
    );
    let result = repository
        .mutate(&context, Mutation::DeletePost(PostId(1)))
        .await
        .unwrap();
    assert!(!result.success);
    assert_eq!(
        cache
            .read_feed(&context.profile.id, &FeedKey::from("home"))
            .unwrap()
            .unwrap(),
        before
    );
}

#[tokio::test]
async fn background_refresh_preserves_confirmed_post_update() {
    let cache = Arc::new(MemoryCache::default());
    let context = fixture_context();
    cache
        .write_feed(
            &context.profile.id,
            &FeedKey::from("home"),
            &CachedFeed::new(
                json!({ "items": [post_json(1, "cached")], "next_page": null }),
                1,
                false,
            ),
        )
        .unwrap();
    let api = Arc::new(RefreshMutationRaceApi::default());
    let repository = Repository::new(
        api.clone(),
        cache.clone(),
        Arc::new(MemoryCredentialStore::default()),
    );
    let read = repository.feed(&context, FeedQuery::home()).await.unwrap();
    assert!(read.stale);
    api.started.notified().await;
    repository
        .mutate(
            &context,
            Mutation::VotePost {
                id: PostId(1),
                score: 1,
            },
        )
        .await
        .unwrap();
    api.release.notify_one();
    tokio::time::sleep(Duration::from_millis(20)).await;
    let cached = cache
        .read_feed(&context.profile.id, &FeedKey::from("home"))
        .unwrap()
        .unwrap();
    assert_eq!(cached.entity["items"][0]["title"], "confirmed");
}

#[tokio::test]
async fn profile_switch_rehydrates_feed_without_deleted_tombstones() {
    let path = std::env::temp_dir().join(format!(
        "lemmy-application-tombstone-switch-{}.toml",
        std::process::id()
    ));
    let store = ProfileStore::new(&path);
    let destination = Profile {
        id: ProfileId::from("destination"),
        instance_url: Url::parse("https://destination.example/").unwrap(),
        account_label: Some("destination".into()),
    };
    store.save(std::slice::from_ref(&destination)).unwrap();
    let cache = Arc::new(MemoryCache::default());
    cache.write_feed(&destination.id, &FeedKey::from("home"), &CachedFeed::new(json!({ "items": [post_json(1, "deleted"), post_json(2, "survivor")], "next_page": null }), 1, false)).unwrap();
    let mut app = App::with_profile_store(
        Arc::new(ConfirmedDeleteApi),
        cache.clone(),
        fixture_context(),
        Arc::new(MemoryCredentialStore::default()),
        store,
    );
    app.repository
        .mutate(
            &ProfileContext {
                profile: destination.clone(),
                session: None,
            },
            Mutation::DeletePost(PostId(1)),
        )
        .await
        .unwrap();
    cache.write_feed(&destination.id, &FeedKey::from("home"), &CachedFeed::new(json!({ "items": [post_json(1, "resurrected"), post_json(2, "survivor")], "next_page": null }), 1, false)).unwrap();
    app.dispatch(AppAction::Profile(ProfileCommand::Switch(destination.id)))
        .await
        .unwrap();
    assert_eq!(
        app.state
            .view
            .posts
            .iter()
            .map(|post| post.id)
            .collect::<Vec<_>>(),
        vec![PostId(2)]
    );
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn same_id_profile_replacement_rejects_old_refresh_cache_write() {
    // `App::with_profile_store` still resolves the downloads directory from
    // `XDG_CACHE_HOME`, so keep the whole body redirected to scratch.
    let _xdg = XdgRedirect::new("profile-refresh-replace");
    let path = std::env::temp_dir().join(format!(
        "lemmy-application-profile-refresh-replace-{}.toml",
        std::process::id()
    ));
    let store = ProfileStore::new(&path);
    let id = ProfileId::from("fixture");
    let old = Profile {
        id: id.clone(),
        instance_url: Url::parse("http://old.example/").unwrap(),
        account_label: Some("old".into()),
    };
    store.save(std::slice::from_ref(&old)).unwrap();
    let cache = Arc::new(MemoryCache::default());
    cache
        .write_feed(
            &id,
            &FeedKey::from("home"),
            &CachedFeed::new(
                json!({ "items": [post_json(1, "cached")], "next_page": null }),
                1,
                false,
            ),
        )
        .unwrap();
    let api = Arc::new(ProfileReplacementRaceApi::default());
    let mut app = App::with_profile_store(
        api.clone(),
        cache.clone(),
        ProfileContext {
            profile: old,
            session: None,
        },
        Arc::new(MemoryCredentialStore::default()),
        store.clone(),
    );
    app.dispatch(AppAction::Input(lemmy::input::Command::Refresh))
        .await
        .unwrap();
    api.started.notified().await;
    app.dispatch(AppAction::Profile(ProfileCommand::New(ProfileDraft {
        id: id.clone(),
        instance_url: Url::parse("https://new.example/").unwrap(),
        account_label: Some("new".into()),
    })))
    .await
    .unwrap();
    api.release.notify_one();
    tokio::time::sleep(Duration::from_millis(20)).await;
    let cached = cache
        .read_feed(&id, &FeedKey::from("home"))
        .unwrap()
        .unwrap();
    assert_eq!(cached.entity["items"][0]["title"], "cached");
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn app_new_active_unpersisted_same_id_replacement_rejects_old_refresh() {
    // `App::new` resolves the default profile store and downloads directory
    // from the XDG env; keep the whole body redirected to scratch so the
    // real `~/.config/lemmy/config.toml` is never read or materialized.
    let _xdg = XdgRedirect::new("active-unpersisted");
    let id = ProfileId::from(format!(
        "active-unpersisted-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let old = Profile {
        id: id.clone(),
        instance_url: Url::parse("http://old.example/").unwrap(),
        account_label: Some("old".into()),
    };
    let cache = Arc::new(MemoryCache::default());
    cache
        .write_feed(
            &id,
            &FeedKey::from("home"),
            &CachedFeed::new(
                json!({ "items": [post_json(1, "cached")], "next_page": null }),
                1,
                false,
            ),
        )
        .unwrap();
    let api = Arc::new(ProfileReplacementRaceApi::default());
    let mut app = App::new(
        api.clone(),
        cache.clone(),
        ProfileContext {
            profile: old,
            session: None,
        },
        Arc::new(MemoryCredentialStore::default()),
    );
    app.dispatch(AppAction::Input(lemmy::input::Command::Refresh))
        .await
        .unwrap();
    api.started.notified().await;
    app.dispatch(AppAction::Profile(ProfileCommand::New(ProfileDraft {
        id: id.clone(),
        instance_url: Url::parse("https://new.example/").unwrap(),
        account_label: Some("new".into()),
    })))
    .await
    .unwrap();
    api.release.notify_one();
    tokio::time::sleep(Duration::from_millis(20)).await;
    let cached = cache
        .read_feed(&id, &FeedKey::from("home"))
        .unwrap()
        .unwrap();
    let title = cached.entity["items"][0]["title"].clone();
    assert_eq!(title, "cached");
}

#[tokio::test]
async fn older_concurrent_feed_refresh_cannot_replace_newer_generation() {
    let cache = Arc::new(MemoryCache::default());
    let context = fixture_context();
    cache
        .write_feed(
            &context.profile.id,
            &FeedKey::from("home"),
            &CachedFeed::new(
                json!({ "items": [post_json(1, "cached")], "next_page": null }),
                1,
                false,
            ),
        )
        .unwrap();
    let api = Arc::new(GenerationRaceApi::default());
    let repository = Repository::new(
        api.clone(),
        cache.clone(),
        Arc::new(MemoryCredentialStore::default()),
    );
    let first = repository.feed_with_generation(&context, FeedQuery::home(), 10);
    let second = repository.feed_with_generation(&context, FeedQuery::home(), 20);
    let (_first, _second) = tokio::join!(first, second);
    while api.calls.load(Ordering::SeqCst) < 2 {
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    api.second_release.notify_one();
    tokio::time::sleep(Duration::from_millis(5)).await;
    api.first_release.notify_one();
    tokio::time::sleep(Duration::from_millis(20)).await;
    let cached = cache
        .read_feed(&context.profile.id, &FeedKey::from("home"))
        .unwrap()
        .unwrap();
    assert_eq!(cached.entity["items"][0]["title"], "newer");
    assert_eq!(
        repository
            .take_completed_feed(&context, &FeedQuery::home())
            .unwrap()
            .unwrap()
            .0,
        20
    );
}

#[tokio::test]
async fn successful_draft_stays_completed_after_switching_profiles() {
    let path = std::env::temp_dir().join(format!(
        "lemmy-application-draft-switch-{}.toml",
        std::process::id()
    ));
    let store = ProfileStore::new(&path);
    let fixture = Profile {
        id: ProfileId::from("fixture"),
        instance_url: Url::parse("http://127.0.0.1/").unwrap(),
        account_label: Some("fixture".into()),
    };
    let other = Profile {
        id: ProfileId::from("other"),
        instance_url: Url::parse("https://other.example/").unwrap(),
        account_label: Some("other".into()),
    };
    store.save(&[fixture.clone(), other.clone()]).unwrap();
    let mut app = App::with_profile_store(
        Arc::new(SuccessfulCommentApi),
        Arc::new(MemoryCache::default()),
        ProfileContext {
            profile: fixture,
            session: None,
        },
        Arc::new(MemoryCredentialStore::default()),
        store,
    );
    app.state.view.posts = vec![post_view(1, "selected")];
    app.state.select(PostId(1));
    let draft = app.state.begin_comment_draft();
    app.dispatch(AppAction::SubmitDraft(draft.id.clone()))
        .await
        .unwrap();
    assert!(app.state.draft(draft.id.clone()).is_none());
    app.dispatch(AppAction::Profile(ProfileCommand::Switch(ProfileId::from(
        "other",
    ))))
    .await
    .unwrap();
    app.dispatch(AppAction::Profile(ProfileCommand::Switch(ProfileId::from(
        "fixture",
    ))))
    .await
    .unwrap();
    assert!(app.state.draft(draft.id).is_none());
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn login_wires_session_into_active_context_after_api_success() {
    let api = Arc::new(LoginApi);
    let credentials = Arc::new(MemoryCredentialStore::default());
    let mut app = App::new(
        api,
        Arc::new(MemoryCache::default()),
        fixture_context(),
        credentials.clone(),
    );
    app.state.view.compose = "login alice secret".into();
    app.dispatch(AppAction::Profile(ProfileCommand::Login))
        .await
        .unwrap();
    assert!(app.state.active.session.is_some());
    assert!(
        credentials
            .get_session(&ProfileId::from("fixture"))
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn login_clears_compose_buffer_so_password_does_not_persist() {
    // Success path: the typed `:login alice secret` line must not linger in
    // the compose buffer (which is rendered on screen) or in state.
    let mut app = App::new(
        Arc::new(LoginApi),
        Arc::new(MemoryCache::default()),
        fixture_context(),
        Arc::new(MemoryCredentialStore::default()),
    );
    app.state.view.compose = "login alice secret".into();
    app.dispatch(AppAction::Profile(ProfileCommand::Login))
        .await
        .unwrap();
    assert!(app.state.active.session.is_some());
    assert!(
        app.state.view.compose.is_empty(),
        "plaintext password must not persist in the compose buffer after a successful login"
    );

    // Failure path: the password must be cleared even when the API rejects it.
    let mut failed = App::new(
        Arc::new(FailingLoginApi),
        Arc::new(MemoryCache::default()),
        fixture_context(),
        Arc::new(MemoryCredentialStore::default()),
    );
    failed.state.view.compose = "login alice secret".into();
    failed
        .dispatch(AppAction::Profile(ProfileCommand::Login))
        .await
        .unwrap();
    assert!(failed.state.active.session.is_none());
    assert!(failed.state.status.error.is_some());
    assert!(
        failed.state.view.compose.is_empty(),
        "plaintext password must not persist in the compose buffer after a failed login"
    );
}

#[tokio::test]
async fn documented_content_commands_are_dispatchable() {
    let (api, requests) = fixture_api_with_status_count(200);
    let mut app = App::new(
        Arc::new(api),
        Arc::new(MemoryCache::default()),
        fixture_context(),
        Arc::new(MemoryCredentialStore::default()),
    );
    app.state.view.posts = vec![post_view(1, "target")];
    app.state.select(PostId(1));

    // Every mutation command documented in help must dispatch (never
    // "unknown command") and reach the API.
    for line in [
        "reply hello",
        "edit New title",
        "vote 1",
        "save",
        "subscribe",
    ] {
        app.dispatch(AppAction::Input(lemmy::input::Command::SubmitLine(
            line.into(),
        )))
        .await
        .unwrap();
        let name = line.split_whitespace().next().unwrap();
        assert_ne!(
            app.state.status.error.as_deref(),
            Some(format!("unknown command: {name}")).as_deref(),
            "`:{line}` is documented in help and must dispatch"
        );
    }
    assert_eq!(
        requests.load(Ordering::SeqCst),
        5,
        "each documented mutation command must reach the API"
    );

    // Navigation commands dispatch too: `:post` opens the selection and
    // `:community` defaults to the selected post's community.
    app.dispatch(AppAction::Input(lemmy::input::Command::SubmitLine(
        "post".into(),
    )))
    .await
    .unwrap();
    assert_ne!(
        app.state.status.error.as_deref(),
        Some("unknown command: post")
    );
    app.dispatch(AppAction::Input(lemmy::input::Command::SubmitLine(
        "community".into(),
    )))
    .await
    .unwrap();
    assert_ne!(
        app.state.status.error.as_deref(),
        Some("unknown command: community")
    );
    assert_eq!(
        app.state.view.feed_query.community,
        Some(lemmy::CommunityId(1)),
        ":community defaults to the selected post's community"
    );
}

#[tokio::test]
async fn deleting_a_profile_removes_metadata_and_session_but_keeps_active() {
    let path = std::env::temp_dir().join(format!(
        "lemmy-application-delete-{}.toml",
        std::process::id()
    ));
    let store = ProfileStore::new(&path);
    let fixture = Profile {
        id: ProfileId::from("fixture"),
        instance_url: Url::parse("http://127.0.0.1/").unwrap(),
        account_label: Some("fixture".into()),
    };
    let other = Profile {
        id: ProfileId::from("other"),
        instance_url: Url::parse("https://other.example/").unwrap(),
        account_label: Some("other".into()),
    };
    store.save(&[fixture.clone(), other.clone()]).unwrap();
    let credentials = Arc::new(MemoryCredentialStore::default());
    credentials
        .put_session(
            &other.id,
            &lemmy::Session {
                token: lemmy::SecretString::from("other-token"),
                user_id: lemmy::UserId(7),
            },
        )
        .await
        .unwrap();
    let mut app = App::with_profile_store(
        Arc::new(fixture_api("feed.json")),
        Arc::new(MemoryCache::default()),
        fixture_context(),
        credentials.clone(),
        store.clone(),
    );
    app.dispatch(AppAction::Profile(ProfileCommand::Delete(ProfileId::from(
        "other",
    ))))
    .await
    .unwrap();
    assert_eq!(app.state.active.profile.id, ProfileId::from("fixture"));
    assert!(
        credentials
            .get_session(&ProfileId::from("other"))
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(store.load().unwrap(), vec![fixture]);
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn help_command_opens_searchable_help_and_back_closes_it() {
    let mut app = fixture_app();
    app.dispatch(AppAction::Input(lemmy::input::Command::SubmitLine(
        "help downloads".into(),
    )))
    .await
    .unwrap();
    assert_eq!(app.state.view.help.as_deref(), Some("downloads"));
    app.dispatch(AppAction::Back).await.unwrap();
    assert!(app.state.view.help.is_none());
}

#[tokio::test]
async fn set_command_validates_writes_atomically_and_rejects_bad_values() {
    let path =
        std::env::temp_dir().join(format!("lemmy-application-set-{}.toml", std::process::id()));
    let store = ProfileStore::new(&path);
    let mut app = App::with_profile_store(
        Arc::new(fixture_api("feed.json")),
        Arc::new(MemoryCache::default()),
        fixture_context(),
        Arc::new(MemoryCredentialStore::default()),
        store.clone(),
    );
    app.dispatch(AppAction::Input(lemmy::input::Command::SubmitLine(
        "set collision-policy overwrite".into(),
    )))
    .await
    .unwrap();
    assert_eq!(
        store.load_config().unwrap().media.collision_policy,
        "overwrite"
    );
    assert_eq!(app.state.status.message, "configuration updated");

    app.dispatch(AppAction::Input(lemmy::input::Command::SubmitLine(
        "set collision-policy bogus".into(),
    )))
    .await
    .unwrap();
    assert!(app.state.status.error.is_some());
    assert_eq!(
        store.load_config().unwrap().media.collision_policy,
        "overwrite",
        "invalid update must not be persisted"
    );
    let _ = std::fs::remove_file(path);
}

struct LoginApi;

#[async_trait]
impl LemmyApi for LoginApi {
    async fn site(&self, _: &ProfileContext) -> Result<SiteInfo> {
        Err(AppError::Network("unused".into()))
    }
    async fn feed(&self, _: &ProfileContext, _: FeedQuery) -> Result<Page<PostView>> {
        Err(AppError::Network("unused".into()))
    }
    async fn post(&self, _: &ProfileContext, _: PostId) -> Result<PostDetail> {
        Err(AppError::Network("unused".into()))
    }
    async fn comments(&self, _: &ProfileContext, _: PostId) -> Result<Vec<CommentView>> {
        Ok(Vec::new())
    }
    async fn login(&self, request: lemmy::api::LoginRequest) -> Result<lemmy::Session> {
        Ok(lemmy::Session {
            token: lemmy::SecretString::from(format!("token-{}", request.username)),
            user_id: lemmy::UserId(7),
        })
    }
    async fn mutate(&self, _: &ProfileContext, _: Mutation) -> Result<MutationResult> {
        Err(AppError::Network("unused".into()))
    }
}

struct FailingLoginApi;

#[async_trait]
impl LemmyApi for FailingLoginApi {
    async fn site(&self, _: &ProfileContext) -> Result<SiteInfo> {
        Err(AppError::Network("unused".into()))
    }
    async fn feed(&self, _: &ProfileContext, _: FeedQuery) -> Result<Page<PostView>> {
        Err(AppError::Network("unused".into()))
    }
    async fn post(&self, _: &ProfileContext, _: PostId) -> Result<PostDetail> {
        Err(AppError::Network("unused".into()))
    }
    async fn comments(&self, _: &ProfileContext, _: PostId) -> Result<Vec<CommentView>> {
        Ok(Vec::new())
    }
    async fn login(&self, _: lemmy::api::LoginRequest) -> Result<lemmy::Session> {
        Err(AppError::Authentication("invalid credentials".into()))
    }
    async fn mutate(&self, _: &ProfileContext, _: Mutation) -> Result<MutationResult> {
        Err(AppError::Network("unused".into()))
    }
}

#[tokio::test]
async fn replacing_profile_invalidates_old_credentials_before_activation() {
    let path = std::env::temp_dir().join(format!(
        "lemmy-application-profile-replace-{}.toml",
        std::process::id()
    ));
    let store = ProfileStore::new(&path);
    let id = ProfileId::from("fixture");
    let old = Profile {
        id: id.clone(),
        instance_url: Url::parse("http://old.example/").unwrap(),
        account_label: Some("old".into()),
    };
    store.save(std::slice::from_ref(&old)).unwrap();
    let credentials = Arc::new(MemoryCredentialStore::default());
    credentials
        .put_session(
            &id,
            &lemmy::Session {
                token: lemmy::SecretString::from("old-token"),
                user_id: lemmy::UserId(7),
            },
        )
        .await
        .unwrap();
    let mut app = App::with_profile_store(
        Arc::new(fixture_api("feed.json")),
        Arc::new(MemoryCache::default()),
        ProfileContext {
            profile: old,
            session: credentials.get_session(&id).await.unwrap(),
        },
        credentials.clone(),
        store.clone(),
    );
    let replacement = ProfileDraft {
        id: id.clone(),
        instance_url: Url::parse("https://new.example/lemmy").unwrap(),
        account_label: Some("new".into()),
    };
    app.dispatch(AppAction::Profile(ProfileCommand::New(replacement)))
        .await
        .unwrap();
    assert!(credentials.get_session(&id).await.unwrap().is_none());
    assert!(app.state.active.session.is_none());
    assert_eq!(
        store.load().unwrap()[0].instance_url,
        Url::parse("https://new.example/lemmy").unwrap()
    );
    let _ = std::fs::remove_file(path);
}

fn fixture_context() -> ProfileContext {
    ProfileContext {
        profile: Profile {
            id: ProfileId::from("fixture"),
            instance_url: Url::parse("http://127.0.0.1/").unwrap(),
            account_label: Some("fixture".into()),
        },
        session: None,
    }
}

fn post_view(id: i64, title: &str) -> PostView {
    PostView {
        id: PostId(id),
        title: title.into(),
        body: None,
        url: None,
        community_id: lemmy::CommunityId(1),
        creator_id: lemmy::UserId(1),
        score: 0,
        comments: 0,
        published: None,
    }
}
fn post_json(id: i64, title: &str) -> serde_json::Value {
    json!({ "id": id, "title": title, "body": null, "url": null, "community_id": 1, "creator_id": 1, "score": 1, "comments": 0, "published": null })
}

#[derive(Default)]
struct RefreshMutationRaceApi {
    started: Arc<Notify>,
    release: Arc<Notify>,
}

#[async_trait]
impl LemmyApi for RefreshMutationRaceApi {
    async fn site(&self, _: &ProfileContext) -> Result<SiteInfo> {
        Err(AppError::Network("unused".into()))
    }
    async fn feed(&self, _: &ProfileContext, _: FeedQuery) -> Result<Page<PostView>> {
        self.started.notify_one();
        self.release.notified().await;
        Ok(Page {
            items: vec![post_view(1, "old refresh")],
            next_page: None,
        })
    }
    async fn post(&self, _: &ProfileContext, _: PostId) -> Result<PostDetail> {
        Err(AppError::Network("unused".into()))
    }
    async fn comments(&self, _: &ProfileContext, _: PostId) -> Result<Vec<CommentView>> {
        Ok(Vec::new())
    }
    async fn login(&self, _: lemmy::api::LoginRequest) -> Result<lemmy::Session> {
        Err(AppError::Network("unused".into()))
    }
    async fn mutate(&self, _: &ProfileContext, mutation: Mutation) -> Result<MutationResult> {
        match mutation {
            Mutation::VotePost { id, .. } => Ok(MutationResult {
                success: true,
                post: Some(post_view(id.0, "confirmed")),
                comment: None,
                message: None,
            }),
            _ => Err(AppError::Network("unexpected mutation".into())),
        }
    }
}

#[derive(Default)]
struct ProfileReplacementRaceApi {
    started: Arc<Notify>,
    release: Arc<Notify>,
    finished: Arc<Notify>,
}

#[async_trait]
impl LemmyApi for ProfileReplacementRaceApi {
    async fn site(&self, _: &ProfileContext) -> Result<SiteInfo> {
        Err(AppError::Network("unused".into()))
    }
    async fn feed(&self, _: &ProfileContext, _: FeedQuery) -> Result<Page<PostView>> {
        self.started.notify_one();
        self.release.notified().await;
        self.finished.notify_one();
        Ok(Page {
            items: vec![post_view(1, "old refresh")],
            next_page: None,
        })
    }
    async fn post(&self, _: &ProfileContext, _: PostId) -> Result<PostDetail> {
        Err(AppError::Network("unused".into()))
    }
    async fn comments(&self, _: &ProfileContext, _: PostId) -> Result<Vec<CommentView>> {
        Ok(Vec::new())
    }
    async fn login(&self, _: lemmy::api::LoginRequest) -> Result<lemmy::Session> {
        Err(AppError::Network("unused".into()))
    }
    async fn mutate(&self, _: &ProfileContext, _: Mutation) -> Result<MutationResult> {
        Err(AppError::Network("unused".into()))
    }
}

struct ConfirmedDeleteApi;

#[async_trait]
impl LemmyApi for ConfirmedDeleteApi {
    async fn site(&self, _: &ProfileContext) -> Result<SiteInfo> {
        Err(AppError::Network("unused".into()))
    }
    async fn feed(&self, _: &ProfileContext, _: FeedQuery) -> Result<Page<PostView>> {
        Err(AppError::Network("unused".into()))
    }
    async fn post(&self, _: &ProfileContext, _: PostId) -> Result<PostDetail> {
        Err(AppError::Network("unused".into()))
    }
    async fn comments(&self, _: &ProfileContext, _: PostId) -> Result<Vec<CommentView>> {
        Ok(Vec::new())
    }
    async fn login(&self, _: lemmy::api::LoginRequest) -> Result<lemmy::Session> {
        Err(AppError::Network("unused".into()))
    }
    async fn mutate(&self, _: &ProfileContext, mutation: Mutation) -> Result<MutationResult> {
        let Mutation::DeletePost(id) = mutation else {
            return Err(AppError::Network("unexpected mutation".into()));
        };
        Ok(MutationResult {
            success: true,
            post: Some(post_view(id.0, "returned target")),
            comment: None,
            message: None,
        })
    }
}

struct UnconfirmedApi;

#[async_trait]
impl LemmyApi for UnconfirmedApi {
    async fn site(&self, _: &ProfileContext) -> Result<SiteInfo> {
        Err(AppError::Network("unused".into()))
    }
    async fn feed(&self, _: &ProfileContext, _: FeedQuery) -> Result<Page<PostView>> {
        Err(AppError::Network("unused".into()))
    }
    async fn post(&self, _: &ProfileContext, _: PostId) -> Result<PostDetail> {
        Err(AppError::Network("unused".into()))
    }
    async fn comments(&self, _: &ProfileContext, _: PostId) -> Result<Vec<CommentView>> {
        Ok(Vec::new())
    }
    async fn login(&self, _: lemmy::api::LoginRequest) -> Result<lemmy::Session> {
        Err(AppError::Network("unused".into()))
    }
    async fn mutate(&self, _: &ProfileContext, _: Mutation) -> Result<MutationResult> {
        Ok(MutationResult {
            success: false,
            post: Some(PostView {
                id: PostId(1),
                title: "unconfirmed".into(),
                body: None,
                url: None,
                community_id: lemmy::CommunityId(1),
                creator_id: lemmy::UserId(1),
                score: 99,
                comments: 0,
                published: None,
            }),
            comment: None,
            message: Some("not confirmed".into()),
        })
    }
}
