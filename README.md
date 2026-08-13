# levim — a Vim-like terminal client for Lemmy

A Linux-first, Vim-like terminal client for [Lemmy](https://join-lemmy.org/).
Browse feeds, open posts and threads, manage multiple instance/account
profiles, interact with your account, and view or download media — all from a
ratatui terminal shell.

## Features

- **Modal Vim-like interaction** — normal / insert / visual / command /
  search modes with `hjkl` navigation, counted motions, and a command line.
- **Multiple profiles** — one profile per instance/account pair; switch with
  `:profile <id>`. Non-secret profile metadata lives in your config file;
  sessions live only in the OS credential store.
- **Authenticated interaction** — `:login` (password masked while typing),
  `:logout`, `:whoami`, voting, saving, subscribing, replying, editing, and
  deleting — each destructive action requires confirmation.
- **Cache and drafts** — profile-scoped SQLite cache and drafts survive
  restarts; failed submissions keep your draft.
- **Media** — mailcap (or a configured handler) opens media externally;
  `:download-media` fetches asynchronously with per-session download
  history, reusing already-downloaded files within the session.
- **Resilience** — bounded retries on transient network failures, stale-cache
  reads while refreshing, and clean terminal restoration on every exit path.

## Building and running

Requires a stable Rust toolchain (see `rust-toolchain.toml`).

```sh
cargo build --release
./target/release/levim
```

`levim --help` prints usage and the command index and exits without starting
the TUI. `levim --clean-temp` sweeps downloaded temp media files.

## Quick start

1. On the first run the client creates a starter config at
   `~/.config/levim/config.toml` with one profile (a general instance);
   edit it to point at your instance, or add profiles once running with
   `:profile-new <id> <instance-url>`. See
   [Configuration](docs/configuration.md) for details.
2. Launch `levim`.
3. `:feed` loads the home feed; `j`/`k` move, `n`/`p` flip pages, `Enter`
   opens a post (j/k then scroll the thread), `Esc` closes it.
4. `:login <username> <password>` signs in (the password is masked while
   typing); the session is stored in the OS credential store.
5. `:help` (or `:help <topic>`) shows the searchable command index.
6. `q` quits and restores the terminal.

## Documentation

- [Configuration](docs/configuration.md) — config file, profiles, cache,
  keymaps, media settings, logging, troubleshooting.
- [Keybindings](docs/keybindings.md) — modes, keys, commands, and remapping.
- [Media](docs/media.md) — handler precedence, scratch files, downloads, and
  current-session history.

## Command summary

| Command | Purpose |
| --- | --- |
| `:feed`, `:subscribed`, `:community [<id>]`, `:search <query>` | navigate content |
| `j`/`k`, `Enter`, `Esc`, `r` | move, open, back, refresh |
| `:profile`, `:profile <id>`, `:profile-new`, `:profile-delete` | manage profiles |
| `:login`, `:logout`, `:whoami` | authentication |
| `:vote <up\|down\|clear>`, `:save`, `:subscribe`, `:reply`, `:edit`, `:delete` | interact |
| `y` / `Esc`, `:confirm`, `:yes`, `:cancel` | confirm / cancel destructive actions |
| `o`, `:media`, `:download-media`, `:downloads` | view and download media |
| `:close`, `:set ...`, `:help`, `:quit` | pane, configuration, help, exit |

Run `levim --help` or `:help` inside the client for the complete list.

## Development

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
cargo test --test smoke -- --test-threads=1   # end-to-end smoke scenarios
```

The smoke suite drives the real application through its public seams against
fixture-backed HTTP servers, temporary XDG config/cache directories, and the
compiled binary (see `tests/support/mod.rs`). The `script(1)`-based launch
scenario requires a Linux system with util-linux.
