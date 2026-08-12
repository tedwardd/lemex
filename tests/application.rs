use std::{sync::{atomic::{AtomicUsize, Ordering}, Arc}, time::Duration};

use async_trait::async_trait;
use lemmy::{
    api::{fixtures::{fixture_api, fixture_api_with_status_count, timeout_fixture_api}, CommentView, FeedQuery, LemmyApi, MutationResult, Page, PostDetail, PostView, SiteInfo},
    app::{actions::{ApiResult, AppAction, ProfileCommand, RequestIdentity}, App, Repository},
    cache::{CacheStore, CachedFeed, FeedKey, MemoryCache},
    domain::{ActiveProfile, Mutation, PostId, Profile, ProfileContext, ProfileId},
    error::{AppError, Result},
    profiles::{MemoryCredentialStore, ProfileStore},
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
    let path = std::env::temp_dir().join(format!("lemmy-application-switch-{}.toml", std::process::id()));
    let store = ProfileStore::new(&path);
    store.save(&[Profile { id: ProfileId::from("other"), instance_url: Url::parse("https://other.example/").unwrap(), account_label: Some("other".into()) }]).unwrap();
    App::with_profile_store(Arc::new(fixture_api("feed.json")), Arc::new(MemoryCache::default()), fixture_context(), Arc::new(MemoryCredentialStore::default()), store)
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

#[tokio::test]
async fn profile_switch_changes_request_context_and_clears_selection() {
    let mut app = configured_fixture_app();
    app.state.select(lemmy::PostId(1));
    app.dispatch(AppAction::Profile(ProfileCommand::Switch(ProfileId::from("other"))))
        .await
        .unwrap();
    assert_eq!(app.state.active.profile.id, ProfileId::from("other"));
    assert!(app.state.selected_post().is_none());
}

#[tokio::test]
async fn failed_mutation_keeps_draft_and_sets_retryable_status() {
    let mut app = failing_mutation_app();
    let draft = app.state.begin_comment_draft();
    app.dispatch(AppAction::SubmitDraft(draft.id.clone())).await.unwrap();
    assert!(app.state.draft(draft.id).is_some());
    assert!(app.state.status.is_retryable());
}

#[tokio::test]
async fn delete_is_staged_until_confirmed_and_cancelled_once() {
    let (api, requests) = fixture_api_with_status_count(200);
    let mut app = App::new(Arc::new(api), Arc::new(MemoryCache::default()), fixture_context(), Arc::new(MemoryCredentialStore::default()));
    app.dispatch(AppAction::DeletePost(PostId(1))).await.unwrap();
    assert_eq!(requests.load(Ordering::SeqCst), 0);
    assert!(app.state.pending.is_some());
    app.dispatch(AppAction::Cancel).await.unwrap();
    assert!(app.state.pending.is_none());
    app.dispatch(AppAction::Confirm).await.unwrap();
    assert_eq!(requests.load(Ordering::SeqCst), 0);

    app.dispatch(AppAction::DeletePost(PostId(1))).await.unwrap();
    app.dispatch(AppAction::Confirm).await.unwrap();
    assert_eq!(requests.load(Ordering::SeqCst), 1);
    app.dispatch(AppAction::Confirm).await.unwrap();
    assert_eq!(requests.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn stale_same_profile_post_result_is_rejected_by_request_token() {
    let mut app = fixture_app();
    let old = app.begin_request(RequestIdentity::Post(PostId(1)));
    let current = app.begin_request(RequestIdentity::Post(PostId(1)));
    let detail = |title| PostDetail { post: PostView { id: PostId(1), title, body: None, url: None, community_id: lemmy::CommunityId(1), creator_id: lemmy::UserId(1), score: 0, comments: 0, published: None }, comments: Vec::new() };
    app.dispatch(AppAction::ApiResult(ApiResult::Post { profile: ProfileId::from("fixture"), request: current, result: Ok(detail("current".into())) })).await.unwrap();
    app.dispatch(AppAction::ApiResult(ApiResult::Post { profile: ProfileId::from("fixture"), request: old, result: Ok(detail("old".into())) })).await.unwrap();
    assert_eq!(app.state.view.detail.unwrap().post.title, "current");
}

#[tokio::test]
async fn stale_comments_for_old_post_do_not_overwrite_active_detail() {
    let mut app = fixture_app();
    app.state.view.detail = Some(PostDetail { post: PostView { id: PostId(2), title: "active".into(), body: None, url: None, community_id: lemmy::CommunityId(1), creator_id: lemmy::UserId(1), score: 0, comments: 0, published: None }, comments: Vec::new() });
    let old = app.begin_request(RequestIdentity::Comments(PostId(1)));
    let comment = CommentView { id: lemmy::CommentId(1), post_id: PostId(1), content: "stale".into(), creator_id: lemmy::UserId(1), score: 0 };
    app.dispatch(AppAction::ApiResult(ApiResult::Comments { profile: ProfileId::from("fixture"), request: old, post: PostId(1), result: Ok(vec![comment]) })).await.unwrap();
    assert!(app.state.selected_comments().is_empty());
}

#[tokio::test]
async fn switch_profile_uses_destination_store_metadata() {
    let path = std::env::temp_dir().join(format!("lemmy-application-{}.toml", std::process::id()));
    let store = ProfileStore::new(&path);
    store.save(&[Profile { id: ProfileId::from("destination"), instance_url: Url::parse("https://remote.example/lemmy").unwrap(), account_label: Some("remote account".into()) }]).unwrap();
    let mut app = App::with_profile_store(Arc::new(fixture_api("feed.json")), Arc::new(MemoryCache::default()), fixture_context(), Arc::new(MemoryCredentialStore::default()), store);
    app.dispatch(AppAction::Profile(ProfileCommand::Switch(ProfileId::from("destination")))).await.unwrap();
    assert_eq!(app.state.active.profile.instance_url, Url::parse("https://remote.example/lemmy").unwrap());
    assert_eq!(app.state.active.profile.account_label.as_deref(), Some("remote account"));
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn feed_returns_cached_content_before_slow_refresh_and_marks_stale_on_failure() {
    let cache = Arc::new(MemoryCache::default());
    let context = fixture_context();
    let cached = CachedFeed::new(json!({ "items": [{ "id": 1, "title": "cached", "body": null, "url": null, "community_id": 1, "creator_id": 1, "score": 1, "comments": 0, "published": null }], "next_page": null }), 1, false);
    cache.write_feed(&context.profile.id, &FeedKey::from("home"), &cached).unwrap();
    let repository = Repository::new(Arc::new(timeout_fixture_api()), cache.clone(), Arc::new(MemoryCredentialStore::default()));
    let read = tokio::time::timeout(Duration::from_millis(100), repository.feed(&context, FeedQuery::home())).await.unwrap().unwrap();
    assert_eq!(read.value.items[0].title, "cached");
    assert!(read.stale);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(cache.read_feed(&context.profile.id, &FeedKey::from("home")).unwrap().unwrap().stale);
}

#[tokio::test]
async fn unsuccessful_mutation_does_not_update_cached_post() {
    let cache = Arc::new(MemoryCache::default());
    let context = fixture_context();
    let before = CachedFeed::new(json!({ "items": [{ "id": 1, "title": "original", "body": null, "url": null, "community_id": 1, "creator_id": 1, "score": 1, "comments": 0, "published": null }], "next_page": null }), 1, false);
    cache.write_feed(&context.profile.id, &FeedKey::from("home"), &before).unwrap();
    let repository = Repository::new(Arc::new(UnconfirmedApi), cache.clone(), Arc::new(MemoryCredentialStore::default()));
    let result = repository.mutate(&context, Mutation::DeletePost(PostId(1))).await.unwrap();
    assert!(!result.success);
    assert_eq!(cache.read_feed(&context.profile.id, &FeedKey::from("home")).unwrap().unwrap(), before);
}

fn fixture_context() -> ProfileContext {
    ProfileContext { profile: Profile { id: ProfileId::from("fixture"), instance_url: Url::parse("http://127.0.0.1/").unwrap(), account_label: Some("fixture".into()) }, session: None }
}

struct UnconfirmedApi;

#[async_trait]
impl LemmyApi for UnconfirmedApi {
    async fn site(&self, _: &ProfileContext) -> Result<SiteInfo> { Err(AppError::Network("unused".into())) }
    async fn feed(&self, _: &ProfileContext, _: FeedQuery) -> Result<Page<PostView>> { Err(AppError::Network("unused".into())) }
    async fn post(&self, _: &ProfileContext, _: PostId) -> Result<PostDetail> { Err(AppError::Network("unused".into())) }
    async fn login(&self, _: lemmy::api::LoginRequest) -> Result<lemmy::Session> { Err(AppError::Network("unused".into())) }
    async fn mutate(&self, _: &ProfileContext, _: Mutation) -> Result<MutationResult> { Ok(MutationResult { success: false, post: Some(PostView { id: PostId(1), title: "unconfirmed".into(), body: None, url: None, community_id: lemmy::CommunityId(1), creator_id: lemmy::UserId(1), score: 99, comments: 0, published: None }), comment: None, message: Some("not confirmed".into()) }) }
}