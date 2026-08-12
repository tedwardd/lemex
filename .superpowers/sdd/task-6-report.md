# Task 6 Report: Fixture-backed Lemmy HTTP adapter

## Changed files

- `src/api/mod.rs`: Added the stable `LemmyApi` trait, feed/page/post/login/mutation result types, and site capabilities.
- `src/api/http.rs`: Added the shared rustls `reqwest` adapter, Lemmy v3 endpoint mapping, auth attachment, JSON normalization, classified server errors, capability-aware unsupported endpoint errors, and bounded transient retries for reads only.
- `src/api/fixtures.rs`: Added an in-process TCP fixture server plus `fixture_api`, `fixture_api_with_status`, `timeout_fixture_api`, `anonymous_context`, and `authenticated_context` helpers.
- `src/lib.rs`: Exported the adapter module and stable API types.
- `Cargo.toml`: Enabled Tokio networking, timers, and async I/O needed by the fixture server.
- `fixtures/lemmy/{login,site,community,post,comment,feed}.json`: Added representative Lemmy v3 response fixtures.
- `tests/api_adapter.rs`: Added the required normalization, authentication classification, and uncertain mutation-timeout tests.

## Commits

- `ec71482 feat: add fixture-backed Lemmy HTTP adapter`
- `docs: record Task 6 report` (this report)

## Red/green evidence

- RED: `cargo test --test api_adapter` failed with unresolved imports for `lemmy::api` before the adapter implementation (exit 101).
- GREEN: `cargo test --test api_adapter` — `cargo test: 3 passed (1 suite, 0.05s)`.

## Self-review findings

- Read requests retry at most three attempts and only for timeouts/transient HTTP statuses; mutations and login are sent once.
- Authentication is attached only when a `ProfileContext` has a session; secret values are not included in `Debug` output.
- HTTP 401/403 and unsupported 404 responses preserve server detail in classified errors, while mutation transport failures are marked as outcome-uncertain.
- Fixture server ownership is retained by the adapter so fixture endpoints remain available for the request lifetime.

## Concerns

- None identified within the Task 6 contract. Capability flags describe the core v3 endpoint family after a successful site response; endpoint-specific unsupported responses remain actionable authorization errors.


## Task 6 review fix pass

### Changed files

- `src/api/http.rs`: Propagated reqwest response body-read failures, classified mutation transport/body/parse failures as outcome-uncertain, rejected empty successful bodies, preserved signed counts, derived capabilities from recognized Lemmy v3 site metadata, used shared base-path URL construction for login, restricted retries to explicit transient statuses, and normalized mutation comment post IDs from the response.
- `src/api/fixtures.rs`: Added deterministic truncated/empty response, request-count, custom-body, and path-preserving login fixtures.
- `tests/api_adapter.rs`: Added regression coverage for body-read and empty-body errors, negative scores/comments, mutation comment post IDs, login subpaths, unknown/malformed site metadata, unsupported endpoints, and non-transient retry behavior.

### Fix details

- `Response::text()` errors now become `AppError::Network`; mutation failures include `outcome uncertain`. A successful response with an empty body is an error rather than an empty JSON object.
- Counts use the observed `counts` value when present, including negative values, and otherwise fall back to the post/comment value.
- Mutation `comment_view` normalization reads `comment.post_id` instead of injecting `PostId(0)`.
- Login uses the same `/api/v3/{path}` endpoint builder as other calls, preserving instance URL subpaths and fixture base overrides.
- Read retries are bounded to 408, 429, 500, 502, 503, and 504; 501/505 and other statuses are not retried, and mutations remain single-attempt.
- Site capability flags require valid site metadata, a recognized 0.18/0.19 Lemmy version, and the observed `local_site` v3 shape; unknown versions report no claimed capabilities. 404 responses retain actionable unsupported-capability text.

### Verification

Exact command and output:

```text
$ cargo test --test api_adapter
cargo test: 13 passed (1 suite, 0.06s)
```

### Concerns

- Capability detection intentionally reports no capabilities for unknown software versions or incomplete site metadata; endpoint-specific 404 responses remain actionable authorization errors.

### Commit

- Focused commit: `fix: address Lemmy adapter review findings`