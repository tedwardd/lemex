# Media

Media handling is safe by design: mailcap is the default external handler,
Kitty graphics rendering is an explicit opt-in, downloads are asynchronous
and cancellable, and every handler command runs without a shell.

## Handler selection precedence

For a selected post's media URL, the client resolves the MIME type (server
metadata, then the HTTP `Content-Type` header, then the URL filename) and
picks the first handler that applies:

1. **Kitty inline rendering** — only when `media.kitty_enabled = true` *and*
   the terminal advertises Kitty graphics support (`TERM` contains `kitty` or
   `KITTY_WINDOW_ID` is set). Without the capability check the opt-in falls
   through.
2. **Explicit handler** — an entry in `[media.handlers]` keyed by MIME type
   (for example `"image/png" = "feh %s"`). Explicit configuration always
   wins over mailcap.
3. **Mailcap** — when `media.mailcap_enabled = true` (the default). The
   matching entry is used, or the mailcap default opener `xdg-open %s` when
   no entry matches.
4. **Metadata only** — no handler applies; the client reports the MIME type
   and does not open anything.

## Mailcap

Entries are loaded from `$MAILCAPS` (a `:`-separated list) or, when unset,
from `~/.mailcap` and `/etc/mailcap`. Parsing never evaluates shell logic:
`test=` predicates are ignored, and the command template is tokenized with
shell-like quote handling but executed directly with a safely constructed
argv — no shell is involved. `%s` is substituted with the media URL (or the
local file when reopening a download), `%t` with the MIME type, and `%%`
with a literal `%`; a template without `%s` gets the file appended, matching
mailcap convention.

Media URLs containing embedded credentials are refused. Child processes run
detached with stdin/stdout/stderr disconnected so they cannot corrupt the
TUI, and media downloads never send authorization headers.

## Kitty graphics (opt-in)

Enable with `media.kitty_enabled = true` or `:set media kitty on`. When the
terminal capability check also passes, images are transmitted through the
Kitty graphics protocol and rendered inline. This is strictly opt-in: the
default configuration uses mailcap even when Kitty support is available.

## Downloads

- `:download-media` downloads the selected post's media asynchronously and
  reports the assigned id.
- The destination file name is derived from the URL (with an extension added
  from the resolved MIME type when the name has none) inside the download
  directory — see [Configuration](configuration.md).
- `media.collision_policy` (or `:set collision-policy`) controls what happens
  when the target already exists:
  - `prompt` (default) — the download waits in the panel for
    `:downloads overwrite` or `:downloads keep`;
  - `overwrite` — replace the existing file;
  - `unique-name` — pick a non-conflicting name with a numeric suffix.
- Downloads can be cancelled (`:downloads cancel`) and retried
  (`:downloads retry`); failures record the reason in the panel.

### Current-session history

In-memory download history (no files, no API) tracks every attempt in the
current session: filename, source URL, MIME type, profile and instance,
timestamp, local path, and status. `:downloads` opens the panel,
`:downloads search` filters it, `:downloads reopen` reopens the file,
`:downloads reveal` opens the directory, and `:downloads copy` copies the
path.

The history is **current-session only**: it is held in memory and cleared
when the client exits. It is not persisted across launches. Deleting a
download's local file (`:downloads delete`, confirmed) is restricted to
completed downloads; pending, in-flight, cancelled, or prompting records
never own their local path and are refused.

## Troubleshooting

- **"no media handler for …; metadata only"** — mailcap is disabled and no
  explicit handler covers the resolved MIME type. Re-enable mailcap
  (`:set media mailcap on`) or add a `[media.handlers]` entry.
- **Downloads fail** — check the download directory is writable and the
  source is reachable; the failure reason is shown in the panel and the
  record can be retried. Collision prompts require an explicit
  `overwrite`/`keep` decision.
- **Kitty rendering does not happen** — the terminal does not advertise
  Kitty support, or `media.kitty_enabled` is off. The client falls back to
  mailcap, which is the intended default.
