pub mod ops;
mod store;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ProfileId;
use crate::Result;

pub use ops::{load_drafts, read_feed, save_draft, write_feed};
pub use store::{MemoryCache, MemoryDraftStore, SqliteCacheStore};

/// Stable key for a feed within one profile's cache namespace.
#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct FeedKey(pub String);

impl FeedKey {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for FeedKey {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for FeedKey {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

impl std::fmt::Display for FeedKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

/// A feed entity and the metadata needed to decide whether it is stale.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CachedFeed {
    pub entity: Value,
    pub synchronized_at: i64,
    pub stale: bool,
}

impl CachedFeed {
    pub fn new(entity: Value, synchronized_at: i64, stale: bool) -> Self {
        Self {
            entity,
            synchronized_at,
            stale,
        }
    }
}

/// Stable identifier for a draft in a profile's in-session draft store.
#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct DraftId(pub String);

impl DraftId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

impl From<String> for DraftId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for DraftId {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

impl std::fmt::Display for DraftId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

/// User-authored content queued for an operation under one profile.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Draft {
    pub id: DraftId,
    pub profile: ProfileId,
    pub operation: String,
    pub content: String,
}

impl Draft {
    pub fn new(
        id: impl Into<DraftId>,
        profile: ProfileId,
        operation: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            profile,
            operation: operation.into(),
            content: content.into(),
        }
    }
}

/// Profile-scoped draft persistence independent of the cache backend.
pub trait DraftStore: Send + Sync {
    fn save_draft(&self, draft: Draft) -> Result<()>;
    fn load_drafts(&self, context: &ProfileId) -> Result<Vec<Draft>>;
}

/// Cache and profile-scoped draft persistence.
pub trait CacheStore: Send + Sync {
    fn read_feed(&self, context: &ProfileId, key: &FeedKey) -> Result<Option<CachedFeed>>;
    fn write_feed(&self, context: &ProfileId, key: &FeedKey, feed: &CachedFeed) -> Result<()>;
    fn save_draft(&self, draft: Draft) -> Result<()>;
    fn load_drafts(&self, context: &ProfileId) -> Result<Vec<Draft>>;
}
