# Media

Media handling is safe by design: mailcap is the default external handler,
downloads are asynchronous and cancellable, and every handler command runs
without a shell.

## Handler selection precedence

For a selected post's media URL, the client resolves the MIME type (server
metadata, then the HTTP `Content-Type` header, then the URL filename) and
picks the first handler that applies:

1. **Explicit handler** — an entry in `[media.handlers]` keyed by MIME type
   (for example `"image/png" = "feh %s"`). Explicit configuration always
   wins over mailcap.
2. **Refused (executable/script media)** — media that could carry
   executable or script content — MIME types such as `application/x-desktop`
   or `text/x-shellscript`, or filenames ending in `.desktop`/`.sh`/`.jar`/
   `.exe`/… — is **refused** rather than handed to a generic opener
   (`xdg-open` or a wildcard mailcap entry): the media host controls both
   the `Content-Type` and the URL name, so opening such content with one
   keystroke would be code execution in your session. An explicit
   `[media.handlers]` entry for the exact MIME type is treated as consent
   and still opens (step 1 applies first).
3. **Mailcap** — when `media.mailcap_enabled = true` (the default). The
   matching entry is used, or the mailcap default opener `xdg-open %s` when
   no entry matches.
4. **Metadata only** — no handler applies; the client reports the MIME type
   and does not open anything.

## Mailcap

Entries are loaded from `$MAILCAPS` (a `:`-separated list) or, when unset,
from `~/.mailcap` and `/etc/mailcap`. Ready-made examples live in
`examples/mailcap.linux` (imv/mpv/zathura/libreoffice) and
`examples/mailcap.macos` (the `open` command); copy the one for your
platform to `~/.mailcap`. Parsing never evaluates shell logic: `test=`
predicates are ignored, and the command template is tokenized with
shell-like quote handling but executed directly with a safely constructed
argv — no shell is involved. `%s` is substituted with the local media file,
`%t` with the MIME type, and `%%` with a literal `%`; a template without
`%s` gets the file appended, matching mailcap convention.

Media URLs containing embedded credentials are refused. Child processes run
detached with stdin/stdout/stderr disconnected so they cannot corrupt the
TUI, and media downloads never send authorization headers.

## Scratch files

Handlers open local files, not URLs (imv/feh/zathura cannot fetch remote
URLs), so before invoking a handler the client downloads the media to a
scratch file under `${TMPDIR}/lemex-client/` (honoring `$TMPDIR` per POSIX,
falling back to `/tmp`). Within a session, opening the same media again
reuses the already-downloaded file instead of re-fetching it (the status
line notes `(reused cached file)`). Scratch files are removed when the
client exits; `lemex --clean-temp` sweeps the whole directory — crash
leftovers, stale files — without any per-file tracking. The scratch
downloads also appear in the downloads panel, where they can be deleted
individually.

The scratch directory is protected against other local users: a pre-planted
symlink at `${TMPDIR}/lemex-client` is refused (never followed), and the
directory is created 0700. Downloads are capped at 2 GiB per file, so a
malicious media host cannot fill the disk with one endless stream.

## tmux and SSH

The client runs the media handlers **on the host where it executes**, not on
your local machine:

- **External handlers (`xdg-open`, mailcap, `[media.handlers]`)** open on
  that host's display. Over SSH:
  - With X11 forwarding (`ssh -X`/`-Y`), `$DISPLAY` is set and the handler
    opens locally through the tunnel — tmux does not interfere.
  - Without forwarding (typical headless server), there is no display: the
    client refuses to spawn the handler and reports
    `no display on this host; … use :download-media and view the file
    locally` instead of silently doing nothing.
  - When the client detects an SSH session (any of `$SSH_CONNECTION`,
    `$SSH_CLIENT`, or `$SSH_TTY` set), the success status adds a note that
    the handler runs on the host running lemex, not on your local terminal.
- **The reliable tmux/SSH path is `:download-media`** to the remote disk,
  then view the file locally (scp/sftp, a synced directory, or
  `:downloads copy` to grab the path). `:downloads reopen` still needs a
  display on the remote host.

Custom handlers that do not need a display (for example copying the file to
a shared location) can be run by setting a dummy `DISPLAY` in the client's
environment; the guard only checks for a non-empty `$DISPLAY` or
`$WAYLAND_DISPLAY`.

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

- **"refusing to open executable media type …"** — the media resolves to an
  executable/script MIME or filename (`.desktop`, `.sh`, `.jar`, …) and no
  explicit `[media.handlers]` entry covers the MIME type. Add one (for
  example `"application/x-desktop" = "my-opener %s"`) to open such files
  deliberately.
- **"no media handler for …; metadata only"** — mailcap is disabled and no
  explicit handler covers the resolved MIME type. Re-enable mailcap
  (`:set media mailcap on`) or add a `[media.handlers]` entry.
- **The handler opens but shows nothing** — handlers receive a local file,
  not the URL; if the media download failed the handler is not spawned and
  the status shows the download error. Check the scratch directory
  (`${TMPDIR}/lemex-client/`) is writable.
- **Downloads fail** — check the download directory is writable and the
  source is reachable; the failure reason is shown in the panel and the
  record can be retried. Collision prompts require an explicit
  `overwrite`/`keep` decision.
- **"no display on this host; …"** — the client is running on a Linux
  machine without `$DISPLAY`/`$WAYLAND_DISPLAY` (headless SSH, no X11
  forwarding). Use `:download-media` and view the file locally, or forward
  X11 so `xdg-open` can reach your display. On macOS the guard always
  passes: `open` needs no display variable.
