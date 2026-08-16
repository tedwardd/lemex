//! Async access layer over the synchronous [`CacheStore`] trait.
//!
//! The store trait stays synchronous (tests/cache.rs calls it directly from
//! plain sync tests), but every call site that can run on an async runtime
//! goes through these thin wrappers. SQLite in the store runs on rusqlite
//! connections guarded by a `std::sync::Mutex`, so blocking it inline would
//! stall the single-flight UI action task whenever the disk wedges. Each
//! wrapper offloads the blocking call to the tokio blocking pool; only
//! scheduling noise (microseconds) is added, and pathological disk stalls
//! can no longer freeze the action task.

use std::sync::Arc;

use crate::cache::{CacheStore, CachedFeed, Draft, FeedKey};
use crate::domain::ProfileId;
use crate::{AppError, Result};

/// Run a blocking cache operation on the tokio blocking pool.
async fn blocking<T>(operation: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T>
where
    T: Send + 'static,
{
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|error| AppError::Storage(format!("cache task failed: {error}")))?
}

/// Load a feed row for `key` in `context`, or `None` when absent.
pub async fn read_feed(
    store: &Arc<dyn CacheStore>,
    context: &ProfileId,
    key: &FeedKey,
) -> Result<Option<CachedFeed>> {
    let store = Arc::clone(store);
    let context = context.clone();
    let key = key.clone();
    blocking(move || store.read_feed(&context, &key)).await
}

/// Store (or overwrite) a feed row for `key` in `context`.
pub async fn write_feed(
    store: &Arc<dyn CacheStore>,
    context: &ProfileId,
    key: &FeedKey,
    feed: &CachedFeed,
) -> Result<()> {
    let store = Arc::clone(store);
    let context = context.clone();
    let key = key.clone();
    let feed = feed.clone();
    blocking(move || store.write_feed(&context, &key, &feed)).await
}

/// Persist a draft in the store associated with its profile.
pub async fn save_draft(store: &Arc<dyn CacheStore>, draft: Draft) -> Result<()> {
    let store = Arc::clone(store);
    blocking(move || store.save_draft(draft)).await
}

/// Load every draft persisted for `context`.
pub async fn load_drafts(store: &Arc<dyn CacheStore>, context: &ProfileId) -> Result<Vec<Draft>> {
    let store = Arc::clone(store);
    let context = context.clone();
    blocking(move || store.load_drafts(&context)).await
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::cache::SqliteCacheStore;

    #[tokio::test]
    async fn cache_ops_round_trip_through_blocking_pool() {
        let store: Arc<dyn CacheStore> = Arc::new(SqliteCacheStore::in_memory().unwrap());
        let profile = ProfileId::from("alice");
        let key = FeedKey::from("home:page1");

        // A missing key reads back as Ok(None) — no error, no phantom row.
        assert!(read_feed(&store, &profile, &key).await.unwrap().is_none());

        let feed = CachedFeed::new(
            json!({"posts": [{"id": 1}], "next_page": 2}),
            1_700_000_000,
            false,
        );
        write_feed(&store, &profile, &key, &feed).await.unwrap();

        // Round trip through the blocking pool preserves the row exactly.
        let read = read_feed(&store, &profile, &key).await.unwrap();
        assert_eq!(read, Some(feed.clone()));

        // An unrelated key in the same profile is still absent.
        assert!(
            read_feed(&store, &profile, &FeedKey::from("home:page2"))
                .await
                .unwrap()
                .is_none()
        );

        // Drafts round trip and stay scoped to their profile.
        let draft = Draft::new("d1", profile.clone(), "CreatePost", "hello world");
        save_draft(&store, draft.clone()).await.unwrap();
        assert_eq!(load_drafts(&store, &profile).await.unwrap(), vec![draft]);
        assert!(
            load_drafts(&store, &ProfileId::from("bob"))
                .await
                .unwrap()
                .is_empty()
        );
    }
}
