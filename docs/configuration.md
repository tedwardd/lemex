# Configuration

The client reads a TOML configuration file and keeps its runtime data in
XDG-standard locations. On the first run (no config file yet) the client
creates a starter config with one profile so it launches immediately; the
status bar reports the created path. Edit that file to point at your
instance, and add further profiles from inside the TUI with
`:profile-new <id> <instance-url>`. A config file that exists but contains
no profiles is still rejected at launch.

## Locations

| Item | Default | Override |
| --- | --- | --- |
| Config file | `~/.config/levim/config.toml` | `$XDG_CONFIG_HOME/levim/config.toml` |
| Cache directory | `~/.cache/levim/` | `$XDG_CACHE_HOME/levim/` (or `cache.directory`) |
| Download directory | `<cache directory>/downloads` | `media.download_directory` |
| Cache database | `<cache directory>/cache.sqlite3` | — |

The cache holds profile-scoped feed pages, community lists, post details,
comment threads, and your in-progress drafts. All cached content is capped
by `cache.max_size_bytes` (a 64 MiB default applies when the key is absent;
oldest entries are evicted first); drafts are never evicted.

Instance URLs must use `https`. An `http://` URL is rejected unless the
config explicitly opts in with a top-level `allow_insecure_http = true`
(credentials would otherwise travel in cleartext).

Comment threads open fully expanded. To start every thread collapsed —
handy on comment-heavy posts — set the top-level
`default_collapsed_threads = true`. Threads still toggle individually with
`z` (collapse/expand the focused thread) and `Z` (collapse all) per
session, and a refresh keeps your current toggles; the default applies
only when a thread first opens.

## Profiles — never store secrets here

A profile is one instance/account pairing. Secrets — passwords and session
tokens — are never written to the config file. Sessions are stored in the
platform's native OS credential store (Linux Secret Service, macOS Keychain,
or Windows Credential Manager via `keyring`) and only after a successful
`:login`; `:logout` deletes the stored session.

```toml
[[profiles]]
id = "main"
instance_url = "https://lemmy.example.com"
account_label = "My main account"

[[profiles]]
id = "work"
instance_url = "https://lemmy.work.example"
```

- `id` — unique short name used by `:profile <id>`.
- `instance_url` — `https` URL of the instance. It must include a host and
  must not contain embedded credentials. An `http://` URL is rejected unless
  the top-level `allow_insecure_http = true` opt-in is set (credentials
  would otherwise travel in cleartext).
- `account_label` — optional display name for the status bar.

Configuration is strict: unknown keys, duplicate ids, and credentials embedded
in URLs are rejected at load time. If the OS credential store is unavailable
(no secret service in a headless session), the client starts anonymous;
`:login` will then surface the credential-store error.

## Startup action

`startup` runs one command automatically once the client opens. It is
optional and unset by default, which keeps the launch view empty (cache-only)
until you act. A leading `:` is optional.

```toml
startup = "feed"            # show the home feed on launch
# startup = "subscribed"    # show your subscribed communities' feed (needs login)
# startup = "search rust"   # run a search on launch
# startup = "community 123" # open a community on launch
```

Every content view the client opens interactively is a valid start page:
`feed`, `subscribed`, `search <query>`, and `community <id>`. Anything else
is rejected at load time so a typo never launches into an unexpected view.
`subscribed` needs a session (like `:subscribed`); without one it refuses
with the same "login first" message.

Because TOML assigns every bare `key = value` to the most recent `[section]`
header, `startup` must appear **before the first `[` header** in the file
(above `[keymaps]`); placing it after a section such as `[logging]` parses it
as `logging.startup` and fails with an "unknown field" error.

## Keymaps

```toml
[keymaps]
down = "jk"
refresh = "r"
```

Each entry maps a documented command name to a key sequence; the sequence
replaces the command's default binding and multi-key sequences participate in
prefix matching. Entries whose name is not a documented command are skipped
with a warning. Keymaps take effect on the next launch. See
[Keybindings](keybindings.md) for the default bindings and command names.

## Media

```toml
[media]
mailcap_enabled = true       # mailcap is the default media handler
download_directory = "/data/levim-downloads"
collision_policy = "prompt"  # prompt | overwrite | unique-name

[media.handlers]
"image/png" = "feh %s"
```

A legacy `kitty_enabled` key in `[media]` is still accepted for backward
compatibility with older configs, but inline kitty rendering was removed and
the key has no effect.

Handler precedence and the download behavior are documented in
[Media](media.md). Changes made with `:set` inside the client are written to
this file atomically and apply immediately when possible; keymaps, the cache
directory, and the cache size take effect on the next launch.

## Cache

```toml
[cache]
directory = "/var/cache/levim"
max_size_bytes = 268435456
```

`directory` relocates the cache (including drafts). `max_size_bytes` caps the
total cached payload; a single entry larger than the cap is evicted too, and
drafts are exempt. When the key is absent a 64 MiB default applies, so the
cache is bounded out of the box.

Cached content is always shown instantly: a load never blocks the interface.
When the cache has an entry, it paints immediately (marked stale while a
background refresh revalidates it); when it does not, the pane shows a
loading row until the fetch lands. `Esc` cancels an in-flight load, and
navigation, scrolling, and other keys stay responsive throughout.

## Colors

The UI palette is customizable with a `[colors]` section. Every key accepts
a color name (`black`, `red`, `green`, `yellow`, `blue`, `magenta`, `cyan`,
`gray`, `darkgray`, `lightred`, `lightgreen`, `lightyellow`, `lightblue`,
`lightmagenta`, `lightcyan`, `white`, `reset` — case-insensitive) or
`#rrggbb` hex, and falls back to the standard palette when absent. Unknown
values are rejected at load time so a typo never silently renders with the
wrong color.

```toml
[colors]
accent = "cyan"        # modal borders, titles, picker selection
surface = "darkgray"   # modal interior background
text = "white"         # modal interior text
error = "red"          # status bar
pending = "yellow"     # status bar
ready = "green"        # status bar
```

The palette is part of the interface, so the client always renders it: the
`NO_COLOR` environment variable (no-color.org) is deliberately overridden
at startup. To run without colors, set every key to `"reset"`.

A ready-made palette matching the Catppuccin Mocha desktop theme (blue
accent) ships in `examples/catppuccin-mocha.toml`.

## Logging

Logging is off by default and redacts credentials, tokens, private content,
and sensitive profile values. When enabled, events are appended to
`<cache directory>/levim.log` (for example `~/.cache/levim/levim.log`) —
logs go to that file, never stdout, so the TUI screen stays intact. The
status bar reports the file path on launch.

```toml
[logging]
enabled = true
level = "debug"  # trace | debug | info | warn | error
```

## HTTP

The HTTP client's timeout budget is configurable. Three levels split the
single request deadline so a dead instance (a connect that never answers)
fails in seconds per attempt instead of burning the full request budget,
while a slow-but-alive server still gets a full per-attempt deadline, and
read retries can never multiply the worst case beyond the total.

```toml
[http]
connect_timeout_secs = 5     # TCP/TLS connect per attempt (default 5)
request_timeout_secs = 10    # per-attempt deadline covering connect, response, body (default 10)
total_timeout_secs = 15      # whole read including retries (default 15)
```

- Values are seconds; each key defaults when absent. A `0` value is a
  configuration error, never a silently instant timeout.
- Inverted orderings are clamped into the invariant
  `connect_timeout_secs <= request_timeout_secs <= total_timeout_secs`;
  a value can only shrink toward a smaller one, so a config that gets the
  ordering wrong fails safe instead of misbehaving silently.
- The total deadline applies to reads (feeds, posts, comments, site);
  mutations are a single attempt bounded by `request_timeout_secs`, so a
  cancelled write is never misreported as confirmed.
- Timeouts take effect on the next launch: the HTTP client is built at
  startup.

## Runtime configuration (`:set`)

| Command | Effect |
| --- | --- |
| `:set keymap <name> <keys>` | configure a key mapping (next launch) |
| `:set media mailcap on\|off` | toggle mailcap handler use |
| `:set download-dir <path>` (or `download-directory`) | set the download directory |
| `:set collision-policy <prompt\|overwrite\|unique-name>` | set collision policy |
| `:set cache-dir <path>` | set the cache directory (next launch) |
| `:set cache-size <bytes>` | set the cache size limit (next launch) |
| `:set logging on\|off [level]` | toggle opt-in logging |

Invalid values are rejected and nothing is written; valid changes are
persisted atomically before being applied.

## Troubleshooting

- **No profiles configured** — the config file exists but has no
  `[[profiles]]` entry (first runs get a starter profile automatically).
  Add one to the config file and launch again; once running,
  `:profile-new <id> <instance-url>` adds further profiles.
- **Login fails with a credential-store error** — the OS credential store is
  unavailable or locked. The client refuses keyring's in-memory fallback
  store, so sessions are never silently stored in memory. On Linux, start a
  Secret Service provider (for example `gnome-keyring` or `keepassxc` with
  Secret Service integration) and unlock it; on macOS, unlock the Keychain —
  then try again.
- **Media does not open** — confirm the MIME type resolves (see
  [Media](media.md)); with mailcap disabled and no explicit handler, media is
  reported as metadata-only by design, and executable/script media
  (`.desktop`, `.sh`, `.jar`, …) is refused unless an explicit
  `[media.handlers]` entry covers its MIME type.
- **Downloads fail** — check the download directory is writable and the
  source URL is reachable; failures are recorded in the downloads panel and
  can be retried with `:downloads retry`.
- **Cache looks stale** — a cached feed is shown immediately while a refresh
  runs in the background; transient failures surface as retryable errors and
  `r`/`:refresh` retries.
