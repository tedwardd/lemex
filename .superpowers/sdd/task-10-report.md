# Task 10 report

## Changed files

- `src/media/mod.rs` (new): media module root; re-exports handler/policy, mailcap, kitty, and download APIs plus domain media values.
- `src/media/mime.rs` (new): `TerminalCapabilities`, `MediaHandler`, `MediaPolicyConfig` (default: mailcap enabled, kitty disabled) with `select`; MIME resolution chain (server metadata → HTTP `Content-Type` header → filename extension), `is_image`, extension mapping, `DEFAULT_MAILCAP_COMMAND` (`xdg-open %s` fallback).
- `src/media/mailcap.rs` (new): mailcap parser (backslash continuations, comments, quote-aware field splitting; `test=`/flags never evaluated), exact + `type/*` lookup, `$MAILCAPS`/`~/.mailcap`/`/etc/mailcap` loading, shell-free tokenizer and `build_argv` (`%s`/`%t`/`%%` substitution; file appended when `%s` absent), deterministic temp-path helpers.
- `src/media/kitty.rs` (new): conservative capability detection (TERM/`KITTY_WINDOW_ID`, no terminal writes), dependency-free base64, chunked `a=T` transmission + `a=p` placement escape generation with format codes for PNG/JPEG/GIF.
- `src/media/download.rs` (new): `CollisionPolicy` (prompt/overwrite/unique-name + config parsing), `DownloadRequest`, `DownloadEvent`, `SessionDownloadHistory` (in-memory, mutation-driven watch notifications, search, clear), `DownloadManager` (`start`/`cancel`/`resolve_collision`/`retry`/`wait_for`/`take_events`/`shutdown`/`history`), unauthenticated streaming download task, restrictive temp files, atomic rename, progress reporting, cancellation flags, stale-temp cleanup on startup, URL credential/scheme validation, `filename_for` sanitization.
- `src/domain/media.rs` (modified): added `DownloadId`, `DownloadStatus` (pending/downloading/prompting/completed/cancelled/failed, `is_terminal`, Display); `DownloadRecord` extended with id, filename, MIME, profile, instance URL, timestamp, status, and `local_file_deleted`.
- `src/domain/mod.rs`, `src/lib.rs` (modified): exports for the new types and `pub mod media`.
- `src/app/actions.rs` (modified): `AppAction::{Media, DownloadMedia, ShowDownloads, Downloads(DownloadsAction)}`, `DownloadsAction` (search/reopen/reveal/copy/retry/cancel/delete/resolve-collision/close), `PendingAction::DeleteDownload`.
- `src/app/state.rs` (modified): `View.downloads` panel with query/selection, panel open/close/move helpers, `DownloadsRender` snapshot, `RenderModel.downloads`.
- `src/app/render.rs` (modified): downloads panel list (id, filename, status, deleted marker) + selected-record detail; content view extracted to `render_content`.
- `src/app/mod.rs` (modified): App media fields and constructors (`new`/`with_profile_store` keep signatures; `with_media` wired from config), `render_model` fills the downloads snapshot, `:media`/`:download-media`/`:downloads` command parsing, panel-aware Open/Back/Refresh/j-k, handler selection and opening (kitty render path, mailcap/external spawn with detached stdio and credential-free argv, metadata-only fallback), download start with prompt auto-selection, downloads panel actions (reopen/reveal/copy/retry/cancel/delete with confirmation/resolve collision/close/search), tick event polling, confirmed local-file deletion, quit shutdown + history clear, clipboard/reveal/spawn helpers.
- `src/main.rs` (modified): passes `config.media` into `App::with_media`.
- `Cargo.toml`, `Cargo.lock` (modified): `parking_lot = "0.12"` added as a direct dependency (already present transitively at 0.12.5 in the lockfile; lockfile change is only the dependency list entry).
- `tests/media.rs` (new): the three required acceptance tests plus ten supporting tests.

## Commits

- `c6f6dec` — `feat: add media handlers downloads and session history` (16 files, +2248 −74)

## Test evidence

### Red (before implementation)

Command: `cargo test --test media`

```
error[E0432]: unresolved import `lemmy::domain::DownloadStatus`
error[E0433]: could not find `media` in `lemmy`
error: could not compile `lemmy` (test "media") due to 2 previous errors
```

The three required tests failed to compile because the media service and download status types did not exist.

### Green (after implementation)

Command: `cargo test --test media`

```
running 13 tests
test cancelled_download_is_recorded_in_session_history ... ok
test explicit_handler_configuration_wins_over_mailcap ... ok
test download_completes_and_renames_atomically ... ok
test kitty_is_selected_only_when_enabled_and_supported ... ok
test kitty_requires_terminal_support_even_when_enabled ... ok
test mailcap_is_default_even_when_kitty_is_available ... ok
test mailcap_parses_and_builds_safe_argv ... ok
test mime_resolution_prefers_metadata_then_header_then_filename ... ok
test download_records_profile_instance_and_timestamp ... ok
test unsupported_types_return_metadata_only ... ok
test history_search_filters_by_filename_and_url ... ok
test prompt_collision_waits_for_resolution ... ok
test unique_name_collision_appends_suffix ... ok

test result: ok. 13 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s
```

Regression check across every other focused target (`cargo test --tests --lib`): 4 lib + 13 api_adapter + 35 application + 7 cache + 7 config_profiles + 10 input_engine + 13 media + 2 smoke = 91 passed, 0 failed. Only pre-existing warnings (`unused_assignments` on `model` in `app::tests`, unused `ExposeSecret` import in `config_profiles`).

## Self-review findings

- MIME precedence is exactly metadata → response header → filename; the header is resolved inside the download task from the unauthenticated response, and the record's MIME is updated before completion.
- Mailcap stays the default handler: with no matching entry the policy returns `Mailcap { command: "xdg-open %s" }` rather than silently degrading, which also keeps the acceptance test deterministic. Unknown MIME types (unresolvable) return `MetadataOnly` even with mailcap enabled.
- Kitty is strictly opt-in: selected only when `kitty_enabled` AND capability detection succeeds; otherwise the image falls through to mailcap/handler config.
- Downloads never send authorization headers (fresh unauthenticated client), reject URLs with embedded userinfo, and handlers are spawned with a shell-free argv, nulled stdio, and no credential material.
- Collision handling: overwrite and unique-name are resolved in `start`/`retry`; prompt parks the record in `Prompting` (set synchronously before spawn so it is observable immediately) and waits on a oneshot resolved by `:overwrite`/`:keep` or cancellation.
- Cancellation is race-safe: the cancel flag is registered before the task spawns, terminal statuses are write-once via a guarded transition, and `cancel()` aborts the task without awaiting it so a stuck peer never blocks the TUI.
- History is in-memory only, searchable (filename, URL, MIME, status, profile, instance), and cleared on quit; in-flight tasks are aborted and their temp files removed on quit, with `.part-` files reclaimed on next startup.
- Prompt-keep does not emit a spurious `Failed` event; only genuine transfer failures/completions surface in the status bar via `Tick`.

## Concerns

- Kitty inline rendering is implemented as a capability path and escape-sequence emission, not pixel-perfect embedding in the ratatui frame: the image is placed at the cursor and the next redraw may paint over it. Selection/capability behavior is fully tested; visual output depends on terminal and frame timing.
- Aborted download tasks may leave a `.part-` temp file when the abort lands between the task's own cleanup and the rename; these are reclaimed by the next startup's stale-temp sweep by design.
- `:media`/`:download-media` act on the selected post's link URL (the only media field the current `PostView` carries); thumbnails/embed fields are not yet modeled.
- `copy path` shells out to `wl-copy`/`xclip`/`xsel` with a fallback to showing the path in the status line; no clipboard dependency was added.

---

# Task 10 review fixes (2026-08-12)

## Changed files (review-fix commit)

- `src/media/download.rs`: `retry()` now removes the aborted attempt's `.part-{id}` temp file (old resolved target and new target) before reusing the same DownloadId/temp path, and transitions the record to `Prompting` synchronously when the collision policy prompts; `cleanup_stale_temporaries` now matches the exact `.{name}.part-{numeric id}` pattern via the new shared matcher; `wait_for_cancel` parks on a 10 ms timer instead of a `yield_now()` busy-spin.
- `src/media/mailcap.rs`: new `pub(crate) fn is_temporary_name(name: &str)` — the exact temp naming pattern (leading dot, file name, `.part-`, numeric id) shared with `temporary_path`.
- `src/app/mod.rs`: the `q` key now queues `AppAction::Quit` (dispatch runs `DownloadManager::shutdown()` + history clear) instead of short-circuiting the event loop; `dispatch_command`'s `Command::Quit` arm also shuts down the manager; added `impl Drop for App` calling `downloads.shutdown()` so cleanup runs on every exit path (error return, aborted action task, input-thread death); `:delete` refuses to stage or confirm deletion while a record is `Prompting` (the local path is the pre-existing collision target the download does not own); three new unit tests.
- `tests/media.rs`: four new regression tests (retry stale-temp removal, exact-pattern stale sweep, retry parks in `Prompting`, parked prompt wait under paused virtual time).
- `tests/application.rs`: `detail.unwrap()` → `detail.clone().unwrap()` (one line) — the new `Drop` impl makes moving fields out of `App` illegal.
- `Cargo.toml`: dev-only `tokio` `test-util` feature so the parked-wait regression can run under `start_paused = true`.

## Fix details per review finding

1. **Important — retry EEXIST**: an aborted task is dropped without running the cleanup inside `run_download`, leaving `.part-{id}` at the (target, id) path; `retry()` reuses the same id/temp path, so the new attempt's `open_restrictive` (`create_new`) failed with EEXIST. `retry()` now captures the previous resolved target before `reset`, and removes `temporary_path` for both the previous and the new target before spawning. Regression: `retry_removes_stale_temp_before_reusing_temp_path`.
2. **Important — stale-temp sweep overmatching**: `cleanup_stale_temporaries` used a bare `.part-` substring, deleting completed downloads/user files like `notes.part-1.txt` or `.my.part-notes.txt`. It now requires the exact `.{name}.part-{numeric}` pattern via `mailcap::is_temporary_name`, colocated with `temporary_path` so the two cannot drift. Regression: `stale_temp_cleanup_matches_exact_pattern_only` (buggy version deletes 2 of the 4 keeper files; fixed version keeps all 4).
3. **Important — quit key bypassed shutdown**: the `q` key returned `true` from `queue_terminal_event`, exiting the loop before `AppAction::Quit` (the only path calling `downloads.shutdown()`/history clear) was ever dispatched. The key now queues `AppAction::Quit`; the loop ends via the app's quit flag after dispatch. `Command::Quit` in `dispatch_command` also shuts the manager down, and a `Drop` impl guarantees cleanup on abort/error exit paths. Regressions: `quit_key_routes_through_quit_action`, `quit_action_shuts_down_downloads_and_clears_history`.
4. **Important — retry never re-entered Prompting**: `reset()` always set `Pending`; `retry()` now calls `history().transition(.., Prompting)` synchronously before spawn when the collision policy prompts, mirroring `start()`. Regression: `retry_prompt_collision_parks_record_in_prompting`.
5. **Minor — busy-spin while prompting**: `wait_for_cancel` looped on `yield_now()`, spinning a CPU core for the whole prompt wait. It now sleeps 10 ms between flag polls (a parked timer, not a spin); cancellation is still also delivered by task abort. Regression: `collision_prompt_wait_parks_instead_of_spinning` (paused-time: prompt stays `Prompting` across many poll intervals, timers stay responsive, resolution completes the download).
6. **Minor — `:delete` destroyed pre-existing collision targets**: a `Prompting` record's local path is the file that existed before the download; `:delete` + confirm removed it. Deletion is now refused at staging and re-checked at confirmation while the record is `Prompting`. Regression: `delete_is_refused_while_collision_prompt_is_pending`.

## Test evidence

### `cargo test --test media` (17 passed, 0 failed)

```
running 17 tests
test cancelled_download_is_recorded_in_session_history ... ok
test explicit_handler_configuration_wins_over_mailcap ... ok
test collision_prompt_wait_parks_instead_of_spinning ... ok
test kitty_is_selected_only_when_enabled_and_supported ... ok
test kitty_requires_terminal_support_even_when_enabled ... ok
test download_completes_and_renames_atomically ... ok
test history_search_filters_by_filename_and_url ... ok
test mailcap_is_default_even_when_kitty_is_available ... ok
test mailcap_parses_and_builds_safe_argv ... ok
test mime_resolution_prefers_metadata_then_header_then_filename ... ok
test download_records_profile_instance_and_timestamp ... ok
test stale_temp_cleanup_matches_exact_pattern_only ... ok
test prompt_collision_waits_for_resolution ... ok
test retry_prompt_collision_parks_record_in_prompting ... ok
test unsupported_types_return_metadata_only ... ok
test unique_name_collision_appends_suffix ... ok
test retry_removes_stale_temp_before_reusing_temp_path ... ok

test result: ok. 17 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

### `cargo test --lib` (7 passed, 0 failed)

```
running 7 tests
test app::tests::pending_refresh_snapshot_is_visible_before_action_completes ... ok
test app::tests::delete_is_refused_while_collision_prompt_is_pending ... ok
test app::tests::queues_text_and_escape_while_action_is_in_flight ... ok
test app::tests::empty_feed_page_leaves_selection_none ... ok
test app::tests::quit_key_routes_through_quit_action ... ok
test app::tests::quit_action_shuts_down_downloads_and_clears_history ... ok
test app::tests::detached_refresh_error_clears_pending_and_is_retryable ... ok

test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

(pre-existing warning only: `unused_assignments` on `model` in `app::tests::pending_refresh_snapshot_is_visible_before_action_completes`)

### `cargo test --test application` (35 passed, 0 failed)

```
test result: ok. 35 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

### `cargo test --test api_adapter` (13 passed, 0 failed)

```
test result: ok. 13 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

Total across the four focused targets: 72 passed, 0 failed.

## Notes

- The parked-wait regression uses `#[tokio::test(start_paused = true)]` (dev-only `test-util`); hyper/reqwest drives request lifecycles on real timers, so the test restores real time with `tokio::time::resume()` before the download performs its network request.
- `App` now implements `Drop` (calls `downloads.shutdown()`); this made moving fields out of `App` illegal, which broke one existing application test line (`detail.unwrap()` → `detail.clone().unwrap()`).
