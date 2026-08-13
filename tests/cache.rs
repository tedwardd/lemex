use levim::ProfileId;
use levim::cache::{
    CacheStore, CachedFeed, Draft, DraftId, FeedKey, MemoryCache, MemoryDraftStore,
    SqliteCacheStore,
};
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

    assert!(
        cache
            .read_feed(&ProfileId::from("b"), &feed_key("home"))
            .unwrap()
            .is_none()
    );
}

#[test]
fn malformed_cache_entry_is_ignored() {
    let cache = MemoryCache::with_raw_entry("a", "home", b"not-json");

    assert!(
        cache
            .read_feed(&ProfileId::from("a"), &feed_key("home"))
            .unwrap()
            .is_none()
    );
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
fn memory_drafts_are_keyed_by_profile_and_id() {
    let drafts = MemoryDraftStore::default();
    let profile_a = ProfileId::from("a");
    let profile_b = ProfileId::from("b");
    let draft_a = draft_for(&profile_a);
    let draft_b = draft_for(&profile_b);

    drafts.save_draft(draft_a.clone()).unwrap();
    drafts.save_draft(draft_b.clone()).unwrap();

    assert_eq!(drafts.load_drafts(&profile_a).unwrap(), vec![draft_a]);
    assert_eq!(drafts.load_drafts(&profile_b).unwrap(), vec![draft_b]);
}

#[test]
fn sqlite_store_round_trips_profile_scoped_cache_and_drafts() {
    let cache = SqliteCacheStore::in_memory().unwrap();
    let profile_a = ProfileId::from("a");
    let profile_b = ProfileId::from("b");
    let key = feed_key("home");
    let expected_feed = feed("a");

    cache.write_feed(&profile_a, &key, &expected_feed).unwrap();
    assert_eq!(
        cache.read_feed(&profile_a, &key).unwrap(),
        Some(expected_feed)
    );
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

    assert!(
        cache
            .read_feed(&profile, &feed_key("home"))
            .unwrap()
            .is_none()
    );
    assert_eq!(cache.load_drafts(&profile).unwrap(), vec![draft]);
}

#[test]
fn cached_feed_exposes_stale_metadata() {
    let cache = MemoryCache::default();
    let key = feed_key("home");
    let mut stale = feed("a");
    stale.stale = true;
    cache
        .write_feed(&ProfileId::from("a"), &key, &stale)
        .unwrap();

    assert!(
        cache
            .read_feed(&ProfileId::from("a"), &key)
            .unwrap()
            .unwrap()
            .stale
    );
}

#[test]
fn sqlite_cache_size_limit_evicts_oldest_entries() {
    let cache = SqliteCacheStore::open_with_size_limit(
        std::env::temp_dir().join(format!("levim-cache-size-{}.sqlite3", std::process::id())),
        Some(120),
    )
    .unwrap();
    let profile = ProfileId::from("a");
    // Three payloads padded well above 40 bytes each; with a 120-byte cap at
    // most two survive. The oldest (by synchronized_at) must be evicted
    // first.
    let mut first = feed("first");
    first.synchronized_at = 1_000;
    let mut second = feed("second");
    second.synchronized_at = 2_000;
    let mut third = feed("third");
    third.synchronized_at = 3_000;
    let pad = |mut cached: CachedFeed| {
        cached.entity["pad"] = json!("0123456789abcdef0123456789abcdef");
        cached
    };
    first = pad(first);
    second = pad(second);
    third = pad(third);
    cache
        .write_feed(&profile, &feed_key("first"), &first)
        .unwrap();
    cache
        .write_feed(&profile, &feed_key("second"), &second)
        .unwrap();
    cache
        .write_feed(&profile, &feed_key("third"), &third)
        .unwrap();

    assert!(
        cache
            .read_feed(&profile, &feed_key("first"))
            .unwrap()
            .is_none(),
        "oldest entry must be evicted first"
    );
    assert!(
        cache
            .read_feed(&profile, &feed_key("second"))
            .unwrap()
            .is_some()
    );
    assert!(
        cache
            .read_feed(&profile, &feed_key("third"))
            .unwrap()
            .is_some()
    );

    // A fresh write still fits and evicts the next-oldest.
    let mut fourth = feed("fourth");
    fourth.synchronized_at = 4_000;
    fourth = pad(fourth);
    cache
        .write_feed(&profile, &feed_key("fourth"), &fourth)
        .unwrap();
    assert!(
        cache
            .read_feed(&profile, &feed_key("second"))
            .unwrap()
            .is_none()
    );
    assert!(
        cache
            .read_feed(&profile, &feed_key("third"))
            .unwrap()
            .is_some()
    );
    assert!(
        cache
            .read_feed(&profile, &feed_key("fourth"))
            .unwrap()
            .is_some()
    );

    // Drafts are never evicted by the size cap.
    let draft = draft_for(&profile);
    cache.save_draft(draft.clone()).unwrap();
    assert_eq!(cache.load_drafts(&profile).unwrap(), vec![draft]);
    let _ = std::fs::remove_file(
        std::env::temp_dir().join(format!("levim-cache-size-{}.sqlite3", std::process::id())),
    );
}
