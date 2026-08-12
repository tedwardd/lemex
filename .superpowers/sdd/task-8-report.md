# Task 8 Report

## Changed files

- `src/main.rs`: Constructs the configured application, initializes ratatui, invokes `run_terminal`, and restores the terminal on every returned path.
- `src/app/mod.rs`: Exposes rendering/help modules and adds a Tokio-driven terminal loop with a dedicated crossterm input thread, redraw ticks, and non-blocking application action tasks.
- `src/app/state.rs`: Includes the current selection in the read-only `RenderModel` projection.
- `src/app/render.rs`: Adds accessible, read-only ratatui rendering for session identity, primary content, detail/thread, compose buffer, status/error/network/stale/pending indicators, mode, and contextual help.
- `src/app/help.rs`: Defines mode-specific help entries and textual mode labels.
- `tests/smoke.rs`: Adds the required active-profile/instance render-model test and fixture app.

## Commits

- `ffa8bb0 feat: add ratatui shell and terminal lifecycle`

## Red/green test evidence

- RED: `cargo test --test smoke render_model_always_contains_active_profile_and_instance` failed before implementation because the fixture API attempted to spawn without a Tokio reactor (`there is no reactor running, must be called from the context of a Tokio 1.x runtime`); exit 101.
- GREEN focused: `cargo test --test smoke render_model_always_contains_active_profile_and_instance` — `cargo test: 1 passed (1 suite, 1 filtered, 0.00s)`.
- GREEN required suite: `cargo test --test smoke` — `cargo test: 2 passed (1 suite, 0.00s)`.
- Binary check: `cargo check --bin lemmy` — `OK`.

## Self-review findings

- Rendering only reads the cloned `RenderModel`; local ratatui list state is rebuilt per frame.
- Focus, mode, stale data, pending activity, and failures have explicit text markers/labels in addition to styling, so color is not the sole signal.
- Contextual help changes with the current mode and is displayed in the command/status panel.
- Input polling occurs on a dedicated thread; Tokio ticks and application actions are selected asynchronously, so slow network actions do not hold the drawing loop.
- Terminal restoration is owned by `main` immediately after `run_terminal` returns, including recoverable application and drawing errors.

## Concerns

- The binary intentionally reports a configuration error when no profile exists; it does not invent an instance or profile.
- The focused red run exposed that the initial fixture helper needed a Tokio runtime; the helper was corrected before green verification. No application behavior was changed to hide that failure.

## Review integration fixes

### Changed files

- `src/app/mod.rs`: queues semantic key commands and resize redraw events while an async action owns the application, then dispatches queued commands in order after completion; Escape/Back and compose text are no longer discarded. Action startup now publishes a read-only render snapshot with pending refresh/confirmation status before spawning the task.
- `src/app/mod.rs`: adds deterministic regressions for queued text/Escape delivery and pending refresh snapshot visibility.
- `.superpowers/sdd/task-8-report.md`: records these fixes and verification output.

### Verification

- `cargo test --lib app::tests` — `cargo test: 2 passed (1 suite, 0.00s)`.
- `cargo test --test smoke` — `cargo test: 2 passed (1 suite, 0.00s)`.

### Fix details and concerns

- Input handling remains on the event-loop task; only serialized `AppAction` values are queued, so no concurrent mutable access to `App` is introduced. A normal-mode `q` still aborts the in-flight task and exits; other semantic commands, including Escape/Back, are retained and dispatched in FIFO order.
- The render model is refreshed synchronously before the action `JoinHandle` is spawned, after setting the pending marker for refresh and confirmation actions. Rendering still receives only the cloned read-only `RenderModel`.
- Confirmation and network pending states are now distinct in the render model; delete staging no longer appears as network activity.

## Residual pending-state follow-up

### Changed files

- `src/app/state.rs`: distinguishes network `pending` from `confirmation_pending` in `Status`, preserving the distinction in read-only `RenderModel` snapshots.
- `src/app/mod.rs`: marks selected post opens pending before their async request, clears confirmation state when confirming, and records delete confirmation without presenting it as network activity; regressions cover open, refresh, and delete-confirmation snapshots.
- `src/app/render.rs`: renders separate confirmation and network pending indicators.
- `.superpowers/sdd/task-8-report.md`: records the follow-up and verification.

### Verification

- `cargo test --lib app::tests` — `cargo test: 2 passed (1 suite, 0.00s)`.
- `cargo test --test smoke` — `cargo test: 2 passed (1 suite, 0.00s)`.

### Fix details

- Delete staging now sets `confirmation_pending` without setting network `pending`; confirmation and cancellation clear the correct state, while confirmed mutation requests set network `pending` before awaiting.
- `OpenSelected` receives the same pre-spawn pending snapshot treatment as refresh, but only when a post is selected; no-selection open remains a no-op.

- Cached refreshes now retain network `pending` after returning stale data while the repository's detached background refresh is still running; fresh completion clears it through the existing tick path.
- `cargo test --lib app::tests` — `cargo test: 2 passed (1 suite, 0.00s)`.
- `cargo test --test smoke` — `cargo test: 2 passed (1 suite, 0.00s)`.
- `src/app/mod.rs`: starting Refresh or a selected Open now clears any staged destructive confirmation before publishing network-pending state; regression verifies no render snapshot shows both indicators.
- `cargo test --lib app::tests` — `cargo test: 2 passed (1 suite, 0.00s)`.
- `cargo test --test smoke` — `cargo test: 2 passed (1 suite, 0.00s)`.
- `src/app/mod.rs`: clearing staged confirmation for Refresh/Open now also clears its stale status message and error before network-pending rendering; regression asserts the network snapshot has no confirmation text.
- `cargo test --lib app::tests` — `cargo test: 2 passed (1 suite, 0.00s)`.
- `cargo test --test smoke` — `cargo test: 2 passed (1 suite, 0.00s)`.
- `src/app/repository.rs`: detached refresh completions now carry API/cache-write errors to the event loop instead of dropping them; `Tick` applies the error so pending clears and retryable status is rendered.
- `src/app/mod.rs`: adds a deterministic cached-refresh failure regression.
- `cargo test --lib app::tests` — `cargo test: 3 passed (1 suite, 0.07s)`.
- `cargo test --test smoke` — `cargo test: 2 passed (1 suite, 0.00s)`.

