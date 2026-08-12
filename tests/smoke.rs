use std::sync::Arc;

use lemmy::{api::fixtures::fixture_api, cache::MemoryCache, domain::{Profile, ProfileContext, ProfileId}, profiles::MemoryCredentialStore, App};
use url::Url;

fn fixture_app() -> App {
    let context = ProfileContext {
        profile: Profile { id: ProfileId::from("fixture"), instance_url: Url::parse("http://127.0.0.1/").unwrap(), account_label: Some("fixture".into()) },
        session: None,
    };
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let api = runtime.block_on(async { fixture_api("feed.json") });
    App::new(Arc::new(api), Arc::new(MemoryCache::default()), context, Arc::new(MemoryCredentialStore::default()))
}

#[test]
fn render_model_always_contains_active_profile_and_instance() {
    let model = fixture_app().state.render_model();
    assert!(!model.status.profile_name.is_empty());
    assert!(!model.status.instance_url.is_empty());
}

#[test]
fn library_exposes_error_result_alias() {
    let result: lemmy::Result<()> = Ok(());
    assert!(result.is_ok());
}
