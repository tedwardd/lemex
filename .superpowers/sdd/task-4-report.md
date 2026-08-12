# Task 4 report

## Status
Complete.

## Changed files
- `Cargo.toml`
- `Cargo.lock`
- `src/domain/mod.rs`
- `src/domain/profile.rs`
- `src/lib.rs`
- `src/profiles/mod.rs`
- `src/profiles/credentials.rs`
- `tests/config_profiles.rs`

Implemented profile-scoped `Session` and `ProfileContext` domain values, a redacted `SecretString` wrapper, the async `CredentialStore` interface, an in-memory `MemoryCredentialStore`, and a `KeyringCredentialStore` backed by the native Linux keyring feature. Session records are keyed by `ProfileId`; token and user ID data are encoded only into the OS credential-store value, while unavailable/malformed keyring state returns actionable `AppError::Storage` errors.

## Commit
- `63aee3d feat: isolate profile sessions in credential storage`

## Test evidence
- RED: `cargo test --test config_profiles sessions_are_keyed_by_profile_id` failed before implementation with unresolved imports for `lemmy::Session` and `lemmy::profiles::MemoryCredentialStore` (exit code 101).
- GREEN focused: `cargo test --test config_profiles sessions_are_keyed_by_profile_id` => `cargo test: 1 passed (1 suite, 6 filtered, 0.00s)` (exit code 0).
- GREEN suite: `cargo test --test config_profiles` => `cargo test: 7 passed (1 suite, 0.00s)` (exit code 0).

## Self-review findings
- `SecretString` redacts both `Debug` and `Display`; `Session`/`ProfileContext` formatting therefore cannot include the token.
- Memory operations use the complete `ProfileId` as the map key, and keyring operations use the complete profile ID as the credential username.
- Keyring `NoEntry` is treated as an absent session (and idempotent deletion); other backend failures are classified as `AppError::Storage` without a plaintext fallback.
- Existing profile/config exports and non-secret metadata interfaces remain available; no secret fields were added to TOML models.

## Concerns
- `target/` is an untracked local build directory and was intentionally not included in the commit.
- No formatters, linters, or project-wide test suites were run, per contract.


## Portability/security correction

### Changed files
- `src/profiles/credentials.rs`
- `tests/config_profiles.rs`
- `.superpowers/sdd/task-4-report.md`

### Fix details
- Restricted `keyring::Entry` construction and all native keyring calls to Linux, preserving the configured `linux-native-sync-persistent` backend there.
- Added an explicit non-Linux `AppError::Storage` refusal for read, write, and delete operations. The refusal does not construct a keyring entry, invoke keyring's platform-independent mock, or inspect/encode the supplied session token.
- Added a non-Linux-target regression test that requires all operations to refuse with an actionable storage error and checks that the supplied token is absent from the error.

### Test evidence
- Command: `cargo test --test config_profiles`
- Output: `cargo test: 7 passed (1 suite, 0.00s)` (exit code 0)
- The non-Linux regression test is guarded with `#[cfg(not(target_os = "linux"))]`, so it compiles and runs only on unsupported targets; no non-Linux target is installed in this environment, so that test was not exercised here. Linux exercises only the native implementation path.