# Task 5 Report: Scoped Cache and Draft Storage

## Changed files

- `src/cache/mod.rs`: Added `FeedKey`, `CachedFeed`, `DraftId`, `Draft`, `CacheStore`, and profile-scoped `DraftStore` interfaces.
- `src/cache/store.rs`: Added disposable SQLite cache/draft storage, profile-scoped in-memory stores, corruption-tolerant cache reads, and draft-preservation behavior.
- `src/lib.rs`: Exported the cache module.
- `tests/cache.rs`: Added the required profile isolation, malformed-entry, and draft-survival tests plus SQLite round-trip/corruption and stale metadata coverage.

## Commits

- `32b6a46 feat: add scoped cache and draft storage`

## Red/green evidence

- RED: `cargo test --test cache` failed before implementation with `unresolved import lemmy::cache` (exit 101).
- GREEN: `cargo test --test cache` — `cargo test: 6 passed (1 suite, 0.00s)`.

## Self-review findings

- Every cache and draft table row carries `profile_id`; cache keys and draft primary keys are profile-scoped.
- Malformed cache JSON is logged and skipped as `Ok(None)`, allowing a fresh network request; it does not affect drafts.
- SQLite and memory stores preserve drafts independently of cache read failures.
- No ratatui or transport dependencies were added; the existing `rusqlite` dependency is used.

## Concerns

- None identified within the Task 5 contract. The SQLite cache is intentionally disposable; callers should treat malformed entries as cache misses.
