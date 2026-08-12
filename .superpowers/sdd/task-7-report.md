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