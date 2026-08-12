use lemmy::cache::{
    CacheStore, CachedFeed, Draft, DraftId, FeedKey, MemoryCache, MemoryDraftStore,
    SqliteCacheStore,
};
use lemmy::ProfileId;
use serde_json::json;

fn feed_key(name: &str) -> FeedKey {
    FeedKey::from(name)
}

fn feed(owner: &str) -> CachedFeed {
    CachedFeed::new(json!({ "owner": owner }), 1_725_000_000, false)
}

fn draft_for(profile: &ProfileId) -> Draft {
    Draft::new(
        DraftId::from("draft-1"),
        profile.clone(),
        "create_comment",
        "hello",
    )
}

#[test]
fn profile_a_cannot_read_profile_b_cache() {
    let cache = MemoryCache::default();
    cache
        .write_feed(&ProfileId::from("a"), &feed_key("home"), &feed("a"))
        .unwrap();

    assert!(cache
        .read_feed(&ProfileId::from("b"), &feed_key("home"))
        .unwrap()
        .is_none());
}

#[test]
fn malformed_cache_entry_is_ignored() {
    let cache = MemoryCache::with_raw_entry("a", "home", b"not-json");

    assert!(cache
        .read_feed(&ProfileId::from("a"), &feed_key("home"))
        .unwrap()
        .is_none());
}

#[test]
fn draft_survives_cache_failure() {
    let drafts = MemoryDraftStore::default();
    let draft = draft_for(&ProfileId::from("a"));
    drafts.save_draft(draft.clone()).unwrap();
    drafts.fail_next_read();

    let _ = drafts.load_drafts(&ProfileId::from("a"));

    assert_eq!(drafts.raw_draft(&draft.id), Some(draft));
}

#[test]
fn sqlite_store_round_trips_profile_scoped_cache_and_drafts() {
    let cache = SqliteCacheStore::in_memory().unwrap();
    let profile_a = ProfileId::from("a");
    let profile_b = ProfileId::from("b");
    let key = feed_key("home");
    let expected_feed = feed("a");

    cache.write_feed(&profile_a, &key, &expected_feed).unwrap();
    assert_eq!(cache.read_feed(&profile_a, &key).unwrap(), Some(expected_feed));
    assert!(cache.read_feed(&profile_b, &key).unwrap().is_none());

    let draft = draft_for(&profile_a);
    cache.save_draft(draft.clone()).unwrap();
    assert_eq!(cache.load_drafts(&profile_a).unwrap(), vec![draft]);
    assert!(cache.load_drafts(&profile_b).unwrap().is_empty());
}

#[test]
fn sqlite_malformed_cache_row_is_ignored_without_losing_drafts() {
    let cache = SqliteCacheStore::in_memory().unwrap();
    let profile = ProfileId::from("a");
    let draft = draft_for(&profile);
    cache.save_draft(draft.clone()).unwrap();
    cache
        .insert_raw_cache_entry("a", "home", b"not-json")
        .unwrap();

    assert!(cache
        .read_feed(&profile, &feed_key("home"))
        .unwrap()
        .is_none());
    assert_eq!(cache.load_drafts(&profile).unwrap(), vec![draft]);
}

#[test]
fn cached_feed_exposes_stale_metadata() {
    let cache = MemoryCache::default();
    let key = feed_key("home");
    let mut stale = feed("a");
    stale.stale = true;
    cache.write_feed(&ProfileId::from("a"), &key, &stale).unwrap();

    assert!(cache
        .read_feed(&ProfileId::from("a"), &key)
        .unwrap()
        .unwrap()
        .stale);
}
