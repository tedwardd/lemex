# Task 11 Report: Wire authentication, profile switching, help, and configuration commands

## Status

Complete. All focused tests green; commit `b91f1d1` on `feature/lemmy-client` in the isolated worktree.

## Changed files

- `src/api/mod.rs` — `LoginRequest` now carries `profile: ProfileId` so the credential store keys the stored session by profile.
- `src/profiles/mod.rs` — profile service: `validate_instance`, `login` (session stored only after API success), `logout` (destructive to the session only).
- `src/profiles/store.rs` — `ProfileStore::create` (upsert, atomic) and `ProfileStore::get` (metadata lookup).
- `src/profiles/credentials.rs` — `MemoryCredentialStore::all()` (in-memory session snapshot for tests/inventory).
- `src/app/help.rs` — searchable `HelpIndex` with profile, navigation, media, download-history, mutation, config, and session entries.
- `src/app/state.rs` — `View.help` + `RenderModel.help` (active help filter), cleared on profile switch and Back.
- `src/app/render.rs` — renders the searchable help index (left pane: matching entries; right pane: groups) when active.
- `src/app/mod.rs` — `pub async fn execute_profile_command(&mut self, ProfileCommand) -> Result<()>` wiring login/logout/whoami/new/delete/switch; full `:` command parser in `submit_line` (`:profile`, `:profile <id>`, `:profile-new`, `:profile-delete`, `:login`, `:logout`, `:whoami`, `:help [topic]`, `:set ...`, `:feed`, `:search`, `:open`, `:refresh`, `:delete`, `:quit`); `config_command` validates → `save_config` (atomic) → `apply_runtime_config` (media policy, collision policy, download directory live).
- `src/config/model.rs` — `LogConfig` (opt-in, redacting); validated setters: `set_keymap`, `set_kitty`, `set_mailcap`, `set_download_directory`, `set_collision_policy`, `set_cache_directory`, `set_cache_size`, `set_logging`.
- `src/config/mod.rs` — export `LogConfig`.
- `src/main.rs` — `build_app` is async: loads profiles at startup, restores the credential-store session when available (secrets never touch the config file); opt-in tracing subscriber from `logging` config; main drives a single runtime.
- `src/media/download.rs` — `DownloadManager::set_directory` (future downloads), `directory()` returns `PathBuf` behind a mutex.
- `tests/application.rs` — required `help_lists_profile_and_media_commands` plus wiring tests: `login_wires_session_into_active_context_after_api_success`, `deleting_a_profile_removes_metadata_and_session_but_keeps_active`, `help_command_opens_searchable_help_and_back_closes_it`, `set_command_validates_writes_atomically_and_rejects_bad_values`.
- `tests/config_profiles.rs` — required `login_stores_session_only_after_api_success` and `logout_removes_session_and_keeps_non_secret_profile_metadata` with `FailOnceLoginApi` and helpers.
- `tests/api_adapter.rs` — `LoginRequest` construction updated for the new `profile` field.

## Commits

- `b91f1d1` `feat: wire profile authentication and command help`

## Red/green test evidence

Red (before implementation), `cargo test --test application --test config_profiles`:

```
error[E0432]: unresolved import `lemmy::app::help::HelpIndex`  --> tests/application.rs:7:91
error[E0560]: struct `LoginRequest` has no field named `profile`  --> tests/config_profiles.rs:154:9
error[E0599]: no method named `all` found for struct `MemoryCredentialStore`
error[E0599]: no method named `create` found for struct `ProfileStore`
error[E0599]: no method named `get` found for struct `ProfileStore`
error[E0432]: unresolved import `lemmy::profiles::{login, logout}`
error: could not compile `lemmy` (test "config_profiles") due to 5 previous errors; 1 warning emitted
error: could not compile `lemmy` (test "application") due to 1 previous error
```

Green (after implementation), same command:

```
running 40 tests
test result: ok. 40 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.11s
     Running tests/config_profiles.rs
running 9 tests
test result: ok. 9 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

The three specified acceptance tests pass: `login_stores_session_only_after_api_success`, `logout_removes_session_and_keeps_non_secret_profile_metadata`, `help_lists_profile_and_media_commands`.

Regression sweep of directly affected suites: `api_adapter` 13 passed, `media` 17 passed, `smoke` 2 passed, `input_engine` 10 passed, `cache` 7 passed; `cargo check --bin lemmy` clean (only pre-existing `secrecy::ExposeSecret` unused-import warning in `tests/config_profiles.rs`, which predates this task).

## Self-review findings

- Login is profile-scoped end to end: `LoginRequest.profile` → `profiles::login` → `put_session(profile, session)`; failure paths never touch the credential store (tested).
- Logout deletes only the session; profile metadata is untouched by construction (tested); the App additionally invalidates repository per-profile context and clears transient view state.
- Switch remains a hard context transition: requests cleared, `switch_context` clears posts/detail/selection/compose/search/next_page/help/pending/status and rebinds drafts to the destination profile; destination session restored from the credential store; cache rehydrated under the destination profile; stale in-flight results are rejected by profile-id + request-token guards.
- Delete refuses the active profile, removes metadata atomically after invalidating context and removing the session, and reports per-step failures.
- Config updates validate first, persist atomically (`write_atomic`), then apply: media policy, collision policy, and download directory take effect live; keymaps, cache location, and the logging subscriber take effect on the next launch (documented in help entries).
- Secrets: passwords arrive via the compose buffer, are consumed in memory, never persisted to TOML, never logged, and are absent from all status messages; sessions live only in the OS credential store; `LoginRequest` Debug redacts the password.

## Concerns

- `:login <username> <password>` reads the password from the on-screen compose buffer (echoed). A hidden-password prompt would be safer; the spec's no-echo requirement (2FA codes) is not covered by this task's scope.
- Help documents the full spec command vocabulary, including `:reply`/`:edit`/`:vote`/`:save`/`:subscribe`; those remain draft-flow entry points rather than dispatchable `:` commands (the delete/open/feed/search/refresh commands are wired).
- Keymap, cache-directory, and logging-subscriber changes persist atomically but apply on the next launch; only media policy, collision policy, and download directory apply live.
- `main.rs` now builds a short-lived current-thread runtime for startup session restoration before `run_terminal` builds the multi-thread runtime; harmless but two runtimes exist briefly.

---

# Task 11 re-review fixes (2026-08-12)

## Status

All Task 11 review findings fixed; all four required suites green plus every directly affected suite; committed on `feature/lemmy-client` in the isolated worktree.

## Changed files

- `src/app/mod.rs` — downloads-panel command routing restored ahead of the top-level post/search arms; documented `:downloads <sub>` forms dispatched; `:community [<id>]`, `:post`, `:reply <text>`, `:edit <title>`, `:vote <score>`, `:save`, `:subscribe` wired to existing actions; compose buffer cleared on login completion and after every command-line submission; `run_terminal` now takes the shared runtime and the startup keymaps; two panel-routing regressions added to the unit tests.
- `src/app/help.rs` — every help entry now describes a dispatchable command; descriptions updated for the newly wired commands; `:set cache-size` annotated "(next launch)".
- `src/input/command.rs` — `Command::by_name` registry mapping documented command names to commands for `[keymaps]`.
- `src/input/engine.rs` — `InputEngine::with_keymaps` applies persisted keymaps at startup, replacing the default binding for the named command; unknown names skipped with a warning.
- `src/input/mapping.rs` — `MappingTable::remove_command` so a rebinding replaces rather than shadows the default sequence.
- `src/cache/store.rs` — `SqliteCacheStore::open_with_size_limit`; `CacheConfig.max_size_bytes` enforced by evicting the oldest feed entries (by `synchronized_at`) after every feed write; drafts never evicted.
- `src/main.rs` — one multi-thread Tokio runtime drives startup and `run_terminal`; absent/unsupported keyring at startup becomes `Ok(None)` (anonymous launch) with a warning instead of aborting; startup passes `config.keymaps` to the input engine and `config.cache.max_size_bytes` to the cache store.
- `tests/application.rs` — regressions: `login_clears_compose_buffer_so_password_does_not_persist` (success and failure paths) and `documented_content_commands_are_dispatchable` (every documented mutation/navigation command dispatches and reaches the API).
- `tests/cache.rs` — regression: `sqlite_cache_size_limit_evicts_oldest_entries`.
- `tests/input_engine.rs` — regression: `persisted_keymaps_bind_documented_commands_at_startup` (rebind replaces default, multi-key prefix works, unknown names skipped).

## Fix details per review finding

- **Important — downloads-panel command routing**: `submit_line` matched the top-level `"search"`/`"delete"` arms before the `downloads_active()` fallback, so with the panel open `:delete` deleted the hidden feed selection's post and `:search` searched the feed instead of filtering panel history. The parser now routes `:search`/`:delete` (and `:open`/`:refresh`) to `DownloadsAction::Search`/`Delete`/`Reopen`/`Retry` when the panel is open, refuses other content commands with "close the downloads panel before using content commands", and keeps the Task 10 fallback `_ if downloads_active() => submit_downloads_command` for the bare panel commands. `:downloads` with no args still toggles; the documented `:downloads search <query>|reopen|reveal|copy|retry|cancel|delete|overwrite|keep|close` forms now dispatch their `DownloadsAction` (with `:downloads search <query>` opening the panel when closed). Regressions: `downloads_panel_routes_delete_and_search_before_feed_arms`, `downloads_subcommands_dispatch_panel_actions_and_bare_toggles`.
- **Help documents only dispatchable commands**: `:community [<id>]` (selected post's community or explicit id), `:post` (`open_selected`), `:reply <text>` (`CreateComment` on the selected post), `:edit <title>` (`EditPost` title), `:vote <score>` (`VotePost`, i8), `:save` (`SavePost`), `:subscribe` (`Subscribe` to the selected post's community) are all wired through `start_mutation`/`open_community`; descriptions were made exact (no comment targeting, since the UI has no comment selection). Regression: `documented_content_commands_are_dispatchable`.
- **Compose buffer cleared on login completion**: `perform_login` clears `state.view.compose` (the on-screen buffer, which renders unconditionally) on both success and failure; `submit_line` also clears it after every submitted command line and after search submission, so typed secrets can never linger on screen or in state. Regression: `login_clears_compose_buffer_so_password_does_not_persist`.
- **Stored-but-inert settings**: `config.keymaps` is fed into the input engine at startup (`InputEngine::new().with_keymaps(...)`; documented command names resolved via `Command::by_name`, unknown names warned and skipped; a rebinding replaces the default sequence so multi-key bindings are not shadowed) and `CacheConfig.max_size_bytes` is enforced by `SqliteCacheStore` (oldest-first eviction after each feed write; drafts exempt). Help annotates `:set cache-size <bytes>` with "(next launch)" since the cap is applied when the store is opened, matching `:set cache-dir`; `:set keymap`'s "applied on next launch" is now accurate. Regressions: `persisted_keymaps_bind_documented_commands_at_startup`, `sqlite_cache_size_limit_evicts_oldest_entries`.
- **One Tokio runtime + keyring-tolerant startup**: `main` builds a single multi-thread runtime and passes `&Runtime` into `run_terminal` (signature: `run_terminal(app, terminal, runtime, keymaps)`); startup session restoration and the terminal loop share it, so App state never crosses runtimes. `build_app` treats a `get_session` failure (no secret service, unsupported target, unavailable keyring) as `Ok(None)` with a warning — anonymous launch works without a keyring; `:login` still surfaces credential-store failures at sign-in time.

## Test evidence

### `cargo test --test application` (42 passed, 0 failed)

```
test result: ok. 42 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.12s
```

Includes `login_clears_compose_buffer_so_password_does_not_persist` and `documented_content_commands_are_dispatchable` (new).

### `cargo test --test config_profiles` (9 passed, 0 failed)

```
test result: ok. 9 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

### `cargo test --test media` (17 passed, 0 failed)

```
test result: ok. 17 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s
```

### `cargo test --lib` (10 passed, 0 failed)

```
test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.07s
```

Includes `downloads_panel_routes_delete_and_search_before_feed_arms` and `downloads_subcommands_dispatch_panel_actions_and_bare_toggles` (new).

### Directly affected suites (regression sweep)

```
tests/api_adapter.rs  — ok. 13 passed; 0 failed
tests/cache.rs        — ok. 8 passed; 0 failed   (incl. sqlite_cache_size_limit_evicts_oldest_entries)
tests/input_engine.rs — ok. 11 passed; 0 failed  (incl. persisted_keymaps_bind_documented_commands_at_startup)
tests/smoke.rs        — ok. 2 passed; 0 failed
```

`cargo check --bin lemmy` and `cargo build --bin lemmy` clean (only pre-existing warnings: unused `secrecy::ExposeSecret` import in `tests/config_profiles.rs`, and an `unused_assignments` warning in the pre-existing `pending_refresh_snapshot_is_visible_before_action_completes` unit test — neither introduced here).

## Notes

- Panel-open content commands (`:feed`, `:media`, `:download-media`, `:community`, `:post`, `:reply`, `:edit`, `:vote`, `:save`, `:subscribe`) are refused while the downloads panel is open so they never act on the hidden feed selection; `:search`/`:delete`/`:open`/`:refresh` route to their panel equivalents, matching the `Command::Open`/`Command::Refresh`/`Command::Back` guards already present in `dispatch_command`.
- `:vote`/`:save`/`:subscribe`/`:reply`/`:edit` target the selected post only; the UI has no comment-selection concept, so the help text says "post" rather than "post or comment".
- Cache-size eviction is best-effort: an eviction failure logs a warning and keeps the (successful) write, since the cap is a hygiene limit rather than a durability contract.
- `run_terminal`'s signature changed (`&Runtime`, `&HashMap<String, String>`); its only caller is `main.rs`.
