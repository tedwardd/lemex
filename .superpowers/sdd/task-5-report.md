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

## Follow-up: MemoryDraftStore profile isolation

### Changed files

- `src/cache/store.rs`: Changed the in-memory draft map key from `DraftId` to `(ProfileId, DraftId)`, included the profile in saves, and preserved `raw_draft(&DraftId)` by scanning stored values.
- `tests/cache.rs`: Added a regression test saving the same draft ID under profiles A and B and asserting each profile loads only its own draft.
- `.superpowers/sdd/task-5-report.md`: Appended this follow-up record.

### Fix details

The in-memory store previously overwrote profile A's draft when profile B saved the same `DraftId`. Composite keys now match SQLite semantics, while profile filtering and deterministic ID sorting remain unchanged.

### Verification

- Command: `cargo test --test cache memory_drafts_are_keyed_by_profile_and_id`
- Output: `cargo test: 1 passed (1 suite, 6 filtered, 0.00s)`
- Command: `cargo test --test cache`
- Output: `cargo test: 7 passed (1 suite, 0.00s)`

### Concerns

- None identified for this focused fix.
