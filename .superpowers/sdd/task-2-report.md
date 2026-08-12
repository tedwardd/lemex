# Task 2 Report: Vim Input Engine

## Status

DONE

## Changed files

- `src/input/mod.rs` — input module boundary and public re-exports.
- `src/input/mode.rs` — modal state enum.
- `src/input/command.rs` — input command enum.
- `src/input/mapping.rs` — sequence mapping table with longest-complete resolution and prefix classification.
- `src/input/engine.rs` — standalone modal input state machine.
- `src/lib.rs` — exports `input` while preserving the existing root `lemmy::Result<T>` alias.
- `tests/input_engine.rs` — focused normal, modal, line-buffer, mapping, and unknown-key behavior tests.

The input engine has no UI, network, filesystem, API, or credential dependencies; it only consumes crossterm key events and returns input commands.

## Commits

- `54b6053` — `feat: add modal Vim input engine`

## TDD evidence

### Red

Command:

```text
cargo test --test input_engine
```

Output:

```text
   Compiling lemmy v0.1.0 (/home/elw/git/lemmy/.worktrees/lemmy-client)
error[E0432]: unresolved import `lemmy::input`
 --> tests/input_engine.rs:2:12
  |
2 | use lemmy::input::{Command, InputEngine, Mode};
  |            ^^^^^ could not find `input` in `lemmy`

For more information about this error, try `rustc --explain E0432`.
error: could not compile `lemmy` (test "input_engine") due to 1 previous error
warning: build failed, waiting for other jobs to finish...
```

### Green

Command:

```text
cargo test --test input_engine
```

Output:

```text
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.23s
     Running tests/input_engine.rs (target/debug/deps/input_engine-dda62c0d27c93f2a)

running 9 tests
test colon_enters_command_mode_and_enter_submits_line ... ok
test insert_emits_printable_text ... ok
test mappings_choose_longest_complete_sequence_without_waiting ... ok
test normal_decimal_prefix_applies_to_motion ... ok
test normal_j_moves_down_once ... ok
test unknown_key_is_noop ... ok
test normal_maps_motions_open_refresh_search_and_quit ... ok
test escape_leaves_insert_and_visual_modes ... ok
test search_submits_and_backspace_updates_line ... ok

test result: ok. 9 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

## Self-review findings

- Normal mode supports the required motions, decimal count prefixes with a one-count default, mode-entry keys, Escape, Enter, and Quit; `r` is also mapped to `Refresh`.
- Insert mode emits printable characters as `Text` and Escape returns to Normal with `Back`.
- Command and both Search modes maintain a line buffer, emit printable `Text`, support Backspace, and emit `SubmitLine` on Enter.
- Mapping sequences are owned by a standalone `MappingTable`; complete mappings are resolved by longest matching sequence, while invalid pending sequences return `Noop` and are cleared.
- Count accumulation saturates at `u32::MAX` rather than overflowing.

## Concerns

None. The focused test run created an untracked local `target/` build directory; it was not included in the commit.

## Task 2 review follow-up

### Changed files

- `src/input/engine.rs` — decimal count accumulation now applies to both Normal and Visual modal modes before mapped motions; line-mode behavior and existing mappings are unchanged.
- `tests/input_engine.rs` — added `visual_decimal_prefix_applies_to_motion`, which enters Visual mode, feeds `2`, then `j`, and asserts `MoveDown { count: 2 }`.
- `.superpowers/sdd/task-2-report.md` — appended this follow-up evidence.

### Verification

Command:

```text
cargo test --test input_engine
```

Output:

```text
cargo test: 10 passed (1 suite, 0.00s)
```

### Fix details

Removed the Normal-only guard around decimal count accumulation in `InputEngine::handle_mapped`; counts are now accumulated before motions in Visual mode as required, while line modes continue to route digits through their existing line buffer handling.
