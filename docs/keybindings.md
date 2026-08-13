# Keybindings

The client is a focused modal Vim core: a small set of modes, `hjkl`
navigation, counted motions, and a command line. It is not full Vim.

## Modes

| Mode | Enter | Purpose |
| --- | --- | --- |
| Normal | (default) | navigate and issue commands |
| Insert | `i` | edit the compose buffer as text |
| Visual | `v` | selection mode (same motion keys) |
| Command | `:` | type a `:` command |
| Search forward | `/` | type a search query |
| Search backward | `?` | type a search query (direction) |

`Esc` returns to Normal mode from every other mode and cancels the current
command/search line.

## Default bindings (Normal mode)

| Key | Command | Purpose |
| --- | --- | --- |
| `h` / `l` | (reserved) | no-op |
| `j` / `k` | move / scroll | move the selection; with the detail/thread pane open, scroll the thread instead (the pane takes focus) |
| `gg` / `G` | jump | jump to the top / bottom of the feed (`N gg` / `N G` jump to row N) |
| `Enter` | open | open the selected post (or download) |
| `o` | open media | open the selected post's media through the configured handler |
| `Ctrl-d` / `Ctrl-u` | scroll detail | scroll the open thread down/up (10 lines, counts apply) |
| `n` | next page | flip to the next feed page (replaces the list; feed pane focused) |
| `p` | previous page | flip back to the previous feed page |

Feed pages are sized to the primary content pane: each fetch (first page,
`n`, `p`, refresh, search) requests exactly as many posts as fit your
current terminal height, and a resize re-sizes subsequent pages. With no
terminal size known yet, the fixed 20-post default is used.
| `r` | refresh | refresh the current view |
| `q` | quit | quit the client |
| `y` | confirm | confirm the pending destructive action |
| `Esc` | back / cancel | close the thread/pane, cancel the pending action, or back out |
| `i` | insert | enter Insert mode |
| `v` | visual | enter Visual mode |
| `:` | command | enter Command mode |
| `/` | search | search forward |
| `?` | search | search backward |

Motions accept numeric counts: `3j` moves down three positions (clamped to
the list). A digit that begins a registered keymap sequence is part of that
mapping, not a count (for example with a `2r` mapping, `2r` fires the mapped
command instead of counting two refreshes). Keys pressed while a network
action is in flight are queued and applied when the action completes.

## Command line

Commands are entered with `:` and submitted with `Enter`. A leading `:` on
the line is optional. `Backspace` deletes the previous character of the line
(or of an insert-mode draft), and `Esc` abandons the line without submitting
it — leaving an open thread untouched. The compose buffer is cleared after
every submission so secrets typed for `:login` never linger on screen.

| Command | Purpose |
| --- | --- |
| `:profile` | list configured profiles (active marked) |
| `:profile <id>` | switch to a profile; a hard context transition |
| `:profile-new <id> <instance-url> [label]` | create or replace a profile |
| `:profile-delete <id>` | delete a profile and its stored session |
| `:login <username> <password>` | log in; session stored in the OS credential store only after success |
| `:logout` | log out; keeps non-secret profile metadata |
| `:whoami` | show the active session user or anonymous |
| `:feed` | show the home feed |
| `:community [<id>]` | open a community feed (defaults to the selected post's community) |
| `:post` | open the selected post |
| `:search <query>` | search posts (filters download history with the panel open) |
| `:open` | open the selected post |
| `:close` | close the detail/thread pane, returning to the content-only view |
| `:refresh` | refresh the current view |
| `:reply <text>` | reply to the selected post |
| `:edit <title>` | retitle the selected post |
| `:delete` | delete the selected post (download with the panel open) |
| `:confirm` / `:yes` | confirm the pending destructive action |
| `:cancel` | cancel the pending destructive action |
| `:vote <up\|down\|clear>` | upvote, downvote, or clear your vote |
| `:save` | save the selected post |
| `:subscribe` | subscribe to the selected post's community |
| `:media` | open the selected post's media through the configured handler |
| `:download-media` | download the selected post's media |
| `:downloads` | open (or close) the current-session download history |
| `:downloads search <query>` | filter download history |
| `:downloads reopen` | reopen the selected download |
| `:downloads reveal` | reveal the download directory |
| `:downloads copy` | copy the download path |
| `:downloads retry` | retry a failed download |
| `:downloads cancel` | cancel an in-flight download |
| `:downloads delete` | delete a completed download's local file |
| `:downloads overwrite\|keep` | resolve a collision prompt |
| `:set ...` | configure keymaps, media, cache, and logging (see Configuration) |
| `:help [topic]` | show searchable help; filter by topic |
| `:quit` | quit the client |

Destructive actions (`:delete`, post/comment creation, and download deletion)
require an explicit confirmation before any network or filesystem activity.
When a confirmation is pending, the status line shows
`[PENDING] Confirmation required before network activity.`; confirm with
`y` (or `:confirm`/`:yes`) and cancel with `Esc` (or `:cancel`).

## Remapping

Configure `[keymaps]` in the config file (see
[Configuration](configuration.md)) or use `:set keymap <name> <keys>`, where
`<name>` is a documented command name such as `down`, `up`, `go-to-first`
(or `top`), `go-to-last` (or `bottom`), `open`, `media` (or `open-media`),
`refresh`, `next-page`, `previous-page`, `close-pane` (or `close`), `insert`,
`visual`, `command`, `search`, `search-backward`, `back`, `quit`,
`scroll-detail-down`, `scroll-detail-up`, `confirm`, or `cancel`. The new sequence replaces the
command's default binding, and multi-key sequences (for example `jk`)
participate in prefix matching. Keymaps take effect on the next launch.
