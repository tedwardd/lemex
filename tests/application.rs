use std::sync::Arc;

use lemmy::{
    api::fixtures::{fixture_api, timeout_fixture_api},
    app::{actions::{AppAction, ProfileCommand}, App},
    cache::MemoryCache,
    domain::{ActiveProfile, Profile, ProfileContext, ProfileId},
    profiles::MemoryCredentialStore,
};
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
    let mut app = fixture_app();
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
