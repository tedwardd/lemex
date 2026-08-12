use std::{
    collections::HashMap,
    fmt::Display,
    path::Path,
    sync::Mutex,
};

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;

use crate::{error::AppError, ProfileId, Result};
use super::{CacheStore, CachedFeed, Draft, DraftId, DraftStore, FeedKey};

fn storage_error(error: impl Display) -> AppError {
    AppError::Storage(error.to_string())
}

/// A disposable SQLite-backed cache and draft repository.
///
/// The connection is protected by a mutex so the store can be shared between
/// the UI and repository tasks while preserving a single transaction owner.
pub struct SqliteCacheStore {
    connection: Mutex<Connection>,
}

impl SqliteCacheStore {
    /// Open (or create) a persistent SQLite database at `path`.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let connection = Connection::open(path).map_err(storage_error)?;
        Self::from_connection(connection)
    }

    /// Open a disposable in-memory SQLite database.
    pub fn in_memory() -> Result<Self> {
        let connection = Connection::open_in_memory().map_err(storage_error)?;
        Self::from_connection(connection)
    }

    /// Alias for [`Self::open`], useful to callers that construct stores by path.
    pub fn new(path: impl AsRef<Path>) -> Result<Self> {
        Self::open(path)
    }

    fn from_connection(connection: Connection) -> Result<Self> {
        connection
            .execute_batch(
                "PRAGMA foreign_keys = ON;
                 CREATE TABLE IF NOT EXISTS cache_entries (
                     profile_id TEXT NOT NULL,
                     feed_key TEXT NOT NULL,
                     entity_json TEXT NOT NULL,
                     synchronized_at INTEGER NOT NULL,
                     stale INTEGER NOT NULL,
                     PRIMARY KEY (profile_id, feed_key)
                 );
                 CREATE TABLE IF NOT EXISTS drafts (
                     profile_id TEXT NOT NULL,
                     draft_id TEXT NOT NULL,
                     operation TEXT NOT NULL,
                     content TEXT NOT NULL,
                     PRIMARY KEY (profile_id, draft_id)
                 );",
            )
            .map_err(storage_error)?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    /// Insert an uninterpreted cache payload for corruption/recovery tests.
    /// Production callers should use [`CacheStore::write_feed`].
    pub fn insert_raw_cache_entry(
        &self,
        profile: &str,
        key: &str,
        entity_json: &[u8],
    ) -> Result<()> {
        let connection = self.connection.lock().map_err(storage_error)?;
        let payload = String::from_utf8_lossy(entity_json);
        connection
            .execute(
                "INSERT INTO cache_entries
                    (profile_id, feed_key, entity_json, synchronized_at, stale)
                 VALUES (?1, ?2, ?3, 0, 0)
                 ON CONFLICT(profile_id, feed_key) DO UPDATE SET
                    entity_json = excluded.entity_json,
                    synchronized_at = excluded.synchronized_at,
                    stale = excluded.stale",
                params![profile, key, payload.as_ref()],
            )
            .map_err(storage_error)?;
        Ok(())
    }

    fn read_feed_locked(
        connection: &Connection,
        context: &ProfileId,
        key: &FeedKey,
    ) -> Result<Option<CachedFeed>> {
        let row = connection
            .query_row(
                "SELECT entity_json, synchronized_at, stale
                 FROM cache_entries WHERE profile_id = ?1 AND feed_key = ?2",
                params![context.0.as_str(), key.0.as_str()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(storage_error)?;

        let Some((entity_json, synchronized_at, stale)) = row else {
            return Ok(None);
        };
        let entity = match serde_json::from_str::<Value>(&entity_json) {
            Ok(entity) => entity,
            Err(error) => {
                tracing::warn!(
                    profile = %context,
                    feed_key = %key,
                    error = %error,
                    "ignoring malformed cache entry"
                );
                return Ok(None);
            }
        };
        Ok(Some(CachedFeed {
            entity,
            synchronized_at,
            stale: stale != 0,
        }))
    }

    fn write_feed_locked(
        connection: &Connection,
        context: &ProfileId,
        key: &FeedKey,
        feed: &CachedFeed,
    ) -> Result<()> {
        let entity_json = serde_json::to_string(&feed.entity).map_err(storage_error)?;
        connection
            .execute(
                "INSERT INTO cache_entries
                    (profile_id, feed_key, entity_json, synchronized_at, stale)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(profile_id, feed_key) DO UPDATE SET
                    entity_json = excluded.entity_json,
                    synchronized_at = excluded.synchronized_at,
                    stale = excluded.stale",
                params![
                    context.0.as_str(),
                    key.0.as_str(),
                    entity_json,
                    feed.synchronized_at,
                    i64::from(feed.stale),
                ],
            )
            .map_err(storage_error)?;
        Ok(())
    }

    fn save_draft_locked(connection: &Connection, draft: &Draft) -> Result<()> {
        connection
            .execute(
                "INSERT INTO drafts (profile_id, draft_id, operation, content)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(profile_id, draft_id) DO UPDATE SET
                    operation = excluded.operation,
                    content = excluded.content",
                params![
                    draft.profile.0.as_str(),
                    draft.id.0.as_str(),
                    draft.operation,
                    draft.content,
                ],
            )
            .map_err(storage_error)?;
        Ok(())
    }

    fn load_drafts_locked(connection: &Connection, context: &ProfileId) -> Result<Vec<Draft>> {
        let mut statement = connection
            .prepare(
                "SELECT draft_id, operation, content FROM drafts
                 WHERE profile_id = ?1 ORDER BY draft_id",
            )
            .map_err(storage_error)?;
        let rows = statement
            .query_map(params![context.0.as_str()], |row| {
                Ok(Draft {
                    id: DraftId(row.get(0)?),
                    profile: context.clone(),
                    operation: row.get(1)?,
                    content: row.get(2)?,
                })
            })
            .map_err(storage_error)?;

        let mut drafts = Vec::new();
        for row in rows {
            match row {
                Ok(draft) => drafts.push(draft),
                Err(error) => {
                    tracing::warn!(
                        profile = %context,
                        error = %error,
                        "ignoring malformed draft row"
                    );
                }
            }
        }
        Ok(drafts)
    }
}

impl CacheStore for SqliteCacheStore {
    fn read_feed(&self, context: &ProfileId, key: &FeedKey) -> Result<Option<CachedFeed>> {
        let connection = self.connection.lock().map_err(storage_error)?;
        Self::read_feed_locked(&connection, context, key)
    }

    fn write_feed(&self, context: &ProfileId, key: &FeedKey, feed: &CachedFeed) -> Result<()> {
        let connection = self.connection.lock().map_err(storage_error)?;
        Self::write_feed_locked(&connection, context, key, feed)
    }

    fn save_draft(&self, draft: Draft) -> Result<()> {
        let connection = self.connection.lock().map_err(storage_error)?;
        Self::save_draft_locked(&connection, &draft)
    }

    fn load_drafts(&self, context: &ProfileId) -> Result<Vec<Draft>> {
        let connection = self.connection.lock().map_err(storage_error)?;
        Self::load_drafts_locked(&connection, context)
    }
}

impl super::DraftStore for SqliteCacheStore {
    fn save_draft(&self, draft: Draft) -> Result<()> {
        CacheStore::save_draft(self, draft)
    }

    fn load_drafts(&self, context: &ProfileId) -> Result<Vec<Draft>> {
        CacheStore::load_drafts(self, context)
    }
}

#[derive(Default)]
struct MemoryDraftState {
    drafts: HashMap<DraftId, Draft>,
    fail_next_read: bool,
}

/// In-session profile-scoped draft storage.
#[derive(Default)]
pub struct MemoryDraftStore {
    state: Mutex<MemoryDraftState>,
}

impl MemoryDraftStore {
    /// Make the next read fail without changing any stored draft.
    pub fn fail_next_read(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.fail_next_read = true;
        }
    }

    /// Inspect a stored draft without exercising the read path.
    pub fn raw_draft(&self, id: &DraftId) -> Option<Draft> {
        self.state
            .lock()
            .ok()
            .and_then(|state| state.drafts.get(id).cloned())
    }
    pub fn save_draft(&self, draft: Draft) -> Result<()> {
        <Self as DraftStore>::save_draft(self, draft)
    }

    pub fn load_drafts(&self, context: &ProfileId) -> Result<Vec<Draft>> {
        <Self as DraftStore>::load_drafts(self, context)
    }
}

impl super::DraftStore for MemoryDraftStore {
    fn save_draft(&self, draft: Draft) -> Result<()> {
        let mut state = self.state.lock().map_err(storage_error)?;
        state.drafts.insert(draft.id.clone(), draft);
        Ok(())
    }

    fn load_drafts(&self, context: &ProfileId) -> Result<Vec<Draft>> {
        let mut state = self.state.lock().map_err(storage_error)?;
        if state.fail_next_read {
            state.fail_next_read = false;
            return Err(AppError::Storage("draft read failed".to_owned()));
        }
        let mut drafts: Vec<_> = state
            .drafts
            .values()
            .filter(|draft| &draft.profile == context)
            .cloned()
            .collect();
        drafts.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(drafts)
    }
}

/// Lightweight in-memory cache used by tests and as a disposable fallback.
#[derive(Default)]
pub struct MemoryCache {
    entries: Mutex<HashMap<(ProfileId, FeedKey), Vec<u8>>>,
    drafts: MemoryDraftStore,
}

impl MemoryCache {
    /// Seed a cache row with uninterpreted bytes to exercise corruption handling.
    pub fn with_raw_entry(profile: &str, key: &str, bytes: &[u8]) -> Self {
        let cache = Self::default();
        if let Ok(mut entries) = cache.entries.lock() {
            entries.insert(
                (ProfileId::from(profile), FeedKey::from(key)),
                bytes.to_vec(),
            );
        }
        cache
    }
}

impl CacheStore for MemoryCache {
    fn read_feed(&self, context: &ProfileId, key: &FeedKey) -> Result<Option<CachedFeed>> {
        let bytes = self
            .entries
            .lock()
            .map_err(storage_error)?
            .get(&(context.clone(), key.clone()))
            .cloned();
        let Some(bytes) = bytes else {
            return Ok(None);
        };
        match serde_json::from_slice(&bytes) {
            Ok(feed) => Ok(Some(feed)),
            Err(error) => {
                tracing::warn!(
                    profile = %context,
                    feed_key = %key,
                    error = %error,
                    "ignoring malformed cache entry"
                );
                Ok(None)
            }
        }
    }

    fn write_feed(&self, context: &ProfileId, key: &FeedKey, feed: &CachedFeed) -> Result<()> {
        let bytes = serde_json::to_vec(feed).map_err(storage_error)?;
        self.entries
            .lock()
            .map_err(storage_error)?
            .insert((context.clone(), key.clone()), bytes);
        Ok(())
    }

    fn save_draft(&self, draft: Draft) -> Result<()> {
        self.drafts.save_draft(draft)
    }

    fn load_drafts(&self, context: &ProfileId) -> Result<Vec<Draft>> {
        self.drafts.load_drafts(context)
    }
}

impl super::DraftStore for MemoryCache {
    fn save_draft(&self, draft: Draft) -> Result<()> {
        CacheStore::save_draft(self, draft)
    }

    fn load_drafts(&self, context: &ProfileId) -> Result<Vec<Draft>> {
        CacheStore::load_drafts(self, context)
    }
}
