# Task 7 Report

## Changed files

- `src/app/actions.rs`: Added `ProfileCommand`, `AppAction`, `ApiResult`, and profile draft boundary types.
- `src/app/state.rs`: Added profile-scoped `AppState`, view/status/render model, selection handling, and draft orchestration.
- `src/app/repository.rs`: Added injected Lemmy API/cache/credential repository, cache-first feed reads, stale refresh results, profile-keyed cache writes, and confirmed mutation cache updates.
- `src/app/mod.rs`: Added `App` dispatch orchestration, semantic input translation, profile context transitions, stale-result isolation, draft mutation handling, and read-only render-model projection.
- `src/lib.rs`: Exported the application module and stable application types.
- `tests/application.rs`: Added fixture-backed profile-switch and failed-mutation draft/status tests.

## Commits

- `10fe35e feat: add application state and repository orchestration`

## Red/green test evidence

- RED: `cargo test --test application` failed with unresolved `lemmy::app` imports before application implementation (exit 101).
- GREEN: `cargo test --test application` — `cargo test: 2 passed (1 suite, 0.05s)`.

## Self-review findings

- Profile switches load destination credentials, clear profile-scoped view selection/detail/compose state, change the active profile identity, and rehydrate destination cache when present.
- `ApiResult` applies results only when their originating profile matches the active profile; stale-context results are discarded.
- Failed draft mutations are converted into retryable status while leaving the draft stored; confirmed mutations mark drafts complete and update cached posts.
- Feed reads refresh the API after checking the profile-keyed cache and return cached content with explicit stale metadata when refresh fails.
- Rendering is a pure clone/projection from application state.

## Concerns

- `ProfileCommand::Login` and `Delete` retain an explicit actionable status because the existing profile boundary does not provide interactive login/delete request data; no credentials or profiles are deleted implicitly.


## Task 7 review correction pass

### Changed files

- `src/app/actions.rs`: Added profile-scoped request identities/tokens, pending destructive-action state, and explicit cancellation.
- `src/app/state.rs`: Added pending-action storage and clear-on-context-switch behavior.
- `src/app/mod.rs`: Made DeletePost stage-only; Confirm consumes staged deletion once; Cancel/Back clear it; validated profile, request generation/identity, post target, and comment target before applying results; resolved switched profiles from `ProfileStore` metadata.
- `src/app/repository.rs`: Returned cached feeds immediately, persisted stale metadata before background refresh, and only updated mutation cache entries for confirmed success.
- `src/domain/lemmy.rs`: Added hashability for mutation request identity.
- `tests/application.rs`: Added regressions for confirmation/cancellation, stale same-profile post/comment results, destination profile metadata, cache-first stale refresh, and unsuccessful mutation cache protection.
- `.superpowers/sdd/task-7-report.md`: Appended this correction-pass evidence.

### Fix details

- Delete requests now create a pending destructive action without contacting the adapter. Confirm takes ownership of that action before invoking the adapter, so repeated confirmation cannot repeat the call. Cancel and Back clear pending state.
- Every application request receives a monotonically increasing generation plus operation identity. Results are ignored unless both profile and current token match; post and comment results additionally must match the requested/active post context.
- Profile switches load the destination `Profile` from the configured `ProfileStore`; instance URL and account label are never inherited from the current profile.
- Repository mutation cache writes are gated on `MutationResult.success`; unsuccessful results leave cached posts and drafts untouched.
- Feed reads return a cached page immediately, mark it stale durably, and refresh asynchronously. Successful refresh replaces the stale row; failed refresh leaves stale metadata persisted.

### Focused verification

Command:

```text
cargo test --test application
```

Output:

```text
cargo test: 8 passed (1 suite, 0.10s)
```

## Task 7 re-review correctness fixes

### Changed files

- `src/app/mod.rs`: Back navigation now invalidates all active post request tokens before clearing detail; confirmed DeletePost removes the target from the in-memory feed and detail state; comments results require both the active post identity and current request identity for success and error handling.
- `src/app/repository.rs`: Confirmed DeletePost removes the target post ID from the cached home feed, regardless of an optional returned post; non-delete mutation replacement behavior is preserved.
- `tests/application.rs`: Added focused regressions for stale post results after Back, confirmed deletion from feed and cache, and stale comments errors from an inactive post.
- `.superpowers/sdd/task-7-report.md`: Appended this re-review fix evidence.

### Fix details

- Back and `Command::Back` remove `RequestIdentity::Post` entries from the request-token map before clearing detail, so a late result cannot reopen stale detail.
- A successful `Mutation::DeletePost(id)` now retains every cached and in-memory feed item except `id`; matching open detail and out-of-range selection are cleared or adjusted. Returned post payloads no longer reinsert deleted posts.
- Comments results are ignored unless their request token is current, its identity matches the reported post, and that post is still active in detail. This applies to both successful comments and error results.

### Focused verification

Command:

```text
cargo test --test application
```

Output:

```text
cargo test: 11 passed (1 suite, 0.10s)
```

## Task 7 final state and context fixes

### Changed files

- `src/app/mod.rs`: Required post results (including errors) to match the currently selected post; invalidated post and comments requests on Back; polled completed asynchronous feed refreshes into `AppState`; invalidated all requests on Logout, New, and profile switches; persisted New profile metadata through `ProfileStore`/`AppConfig`; and rejected comment/reply drafts without a selected post before creating a mutation.
- `src/app/repository.rs`: Added profile/post deletion tombstones shared with asynchronous refresh tasks, filtering confirmed deletions before refresh cache writes so a late feed response cannot reinsert a deleted post.
- `tests/application.rs`: Added regressions for selected-post result guards, Back/comments invalidation, refresh/delete races plus AppState refresh application, Logout/New stale-result isolation, New profile persistence, and comment validation without a selected post; updated the existing post-token regression to establish active selection.
- `.superpowers/sdd/task-7-report.md`: Appended this final-fix evidence.

### Fix details

- A matching request token is insufficient for post results: success and failure handling now requires the request's post ID to remain selected. This prevents opening another post or changing selection from allowing an older result to replace detail or change status.
- Back removes both `Post` and `Comments` request identities. Logout, New, and profile switching clear the entire request map before changing authentication/profile context.
- New profile metadata is upserted into the loaded `AppConfig` and atomically saved through `ProfileStore` before activating the new context.
- Confirmed deletion records a profile-scoped tombstone. Refresh tasks hold the tombstone set while filtering and writing fresh feed data, serializing deletion and refresh cache updates. `Tick` applies a completed fresh cache read to the active `AppState`.
- Comment and reply drafts with no selected post now report `select a post before submitting a comment`, preserve the draft, and do not invoke the adapter or construct `PostId(0)`.

### Focused verification

Command:

```text
cargo test --test application
```

Output:

```text
cargo test: 17 passed (1 suite, 0.10s)
```

## Task 7 lifecycle final fixes

### Changed files

- `src/app/repository.rs`: Tagged feed refreshes with monotonic request generations, rejected late refresh cache writes, exposed completed refresh generations for `Tick`, and serialized mutation/refresh cache writes.
- `src/app/mod.rs`: Passed feed request generations into repository refreshes, applied completed refreshes to `AppState` only when the active feed token still matched, and deleted credentials before replacing an existing profile ID.
- `src/app/state.rs`: Preserved completed draft IDs in profile-scoped memory across profile switches.
- `tests/application.rs`: Added regressions for concurrent refresh ordering, successful draft switch-away/back behavior, and profile replacement credential invalidation.
- `.superpowers/sdd/task-7-report.md`: Appended this lifecycle-fix evidence.

### Fix details

- Each feed refresh registers its request generation per profile/feed key. A background response writes cache data only if its generation is still current, so an older concurrent response cannot overwrite a newer cache result. Completed generations are consumed by `Tick` and must match the active request token before updating `AppState`.
- Confirmed-success drafts are tracked by profile ID rather than one global completion set. Switching away and back therefore continues to suppress the submitted draft instead of reloading it from persistence.
- `ProfileCommand::New` detects replacement by profile ID and removes the old credential-store session before saving replacement metadata or activating the new anonymous context.

### Focused verification

Command:

```text
cargo test --test application
```

Output:

```text
cargo test: 20 passed (1 suite, 0.11s)
```
## Task 7 lifecycle review integrity fixes

### Changed files

- `src/app/repository.rs`: Reconciled background feed snapshots with confirmed post updates and profile-scoped deletion tombstones while holding the shared cache-write lock; applied tombstones and confirmed updates during cached-feed rehydration; added profile context epochs that reject refresh writes from replaced instances.
- `src/app/mod.rs`: Invalidated repository refresh state and context epochs when `ProfileCommand::New` replaces an existing profile ID.
- `tests/application.rs`: Added deterministic regressions for a refresh racing a confirmed post update, tombstone filtering during profile-switch rehydration, and an in-flight refresh from an old same-ID profile instance.
- `.superpowers/sdd/task-7-report.md`: Appended this lifecycle review fix evidence.

### Fix details

- Confirmed mutation records and tombstones are updated under the same write synchronization used by refresh cache writes. A refresh takes that lock before reconciling its API snapshot, so either ordering preserves the confirmed mutation and deletion outcome.
- `cached_feed` now reconciles and persists tombstone-filtered/confirmed content before returning it, so profile switches cannot resurrect deleted posts from persistent cache.
- Repository context epochs are incremented on same-ID profile replacement and captured by each refresh. Late refreshes from the old instance fail the epoch check and cannot write into the replacement profile's cache.

### Focused verification

Command:

```text
cargo test --test application
```

Output:

```text
cargo test: 23 passed (1 suite, 0.11s)
```

## Task 7 active unpersisted same-ID replacement fix

### Changed files

- `src/app/mod.rs`: Treats an active profile context with the replacement ID as a replacement even when `ProfileStore`/config has no matching profile, then invalidates repository refresh context epochs before activating the new profile.
- `tests/application.rs`: Added an `App::new` regression with an active unpersisted profile, an in-flight refresh, and same-ID `ProfileCommand::New`; the old refresh response must not overwrite replacement cache data.
- `.superpowers/sdd/task-7-report.md`: Appended this focused fix evidence.

### Fix details

- `ProfileCommand::New` now computes replacement as `active_profile.id == new_profile.id || configured_profile.id == new_profile.id`. This covers active profiles created outside persisted configuration while preserving configured replacement behavior.
- The existing repository context invalidation runs for both replacement cases before credentials are removed and the new context is activated, so refreshes captured from the old instance cannot write to the replacement cache.

### Focused verification

Command:

```text
cargo test --test application
```

Output:

```text
cargo test: 24 passed (1 suite, 0.11s)
```