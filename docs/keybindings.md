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
| `h` / `l` | (bound, inert) | bound to `left`/`right`; no horizontal panes use them today, so they are no-ops |
| `j` / `k` | move / move cursor | move the feed selection; with a thread modal open, move the comment cursor (collapsed threads are skipped) |
| `gg` / `G` | jump | jump to the top / bottom of the feed (`N gg` / `N G` jump to row N) |
| `Enter` | open | open the selected post (or download) |
| `o` | open media | open the selected post's media through the configured handler |
| `C` | communities | open the community list (shortcut for `:communities`) |
| `Ctrl-d` / `Ctrl-u` | scroll detail | scroll the open thread down/up (10 lines, counts apply) |
| `z` | toggle thread | collapse or expand the focused comment's reply thread |
| `Z` | collapse all threads | collapse every comment thread in the open thread |
| `n` | next page | flip to the next feed page (inert while a modal is open) |
| `p` | previous page | flip back to the previous feed page |

Feed pages are sized to the primary content pane: each fetch (first page,
`n`, `p`, refresh, search, `:subscribed`) requests exactly as many posts as
fit your current terminal height (capped at 50 — the Lemmy server maximum),
and a resize re-sizes subsequent pages. With no terminal size known yet, the
fixed 20-post default is used.
| `r` | refresh | refresh the current view |
| `q` | quit | quit the client |
| `y` | confirm | confirm the pending destructive action |
| `Esc` | back / cancel | pop the focused modal, cancel the pending action, cancel an in-flight load, or back out |
| `i` | insert | enter Insert mode |
| `v` | visual | enter Visual mode |
| `:` | command | enter Command mode |
| `/` | search | search forward |
| `?` | search | search backward |

Motions accept numeric counts: `3j` moves down three positions (clamped to
the list). A digit that begins a registered keymap sequence is part of that
mapping, not a count (for example with a `2r` mapping, `2r` fires the mapped
command instead of counting two refreshes). Page loads never block the
interface: fetching runs in the background, cached content paints instantly,
and keys apply immediately while a refresh is in flight — `[PENDING]` (and
`[STALE]` for cached content being revalidated) marks the load, and `Esc`
cancels it.

## Command line

Commands are entered with `:` and submitted with `Enter`. A leading `:` on
the line is optional. `Tab` completes the command from the documented
command index: a unique match fills in the whole command, several matches
extend to their longest common prefix (press `Tab` again to reach it).
`Backspace` deletes the previous character of the line (or of an insert-mode
draft), and `Esc` abandons the line without submitting it — leaving an open
thread untouched. The compose buffer is cleared after every submission so
secrets typed for `:login` never linger on screen, and while a `:login` line
is being typed the password (the third token) is echoed as asterisks.

| Command | Purpose |
| --- | --- |
| `:profile` | list configured profiles (active marked) |
| `:profile <id>` | switch to a profile; a hard context transition |
| `:profile-new <id> <instance-url> [label]` | create or replace a profile |
| `:profile-delete <id>` | delete a profile and its stored session |
| `:login <username> <password>` | log in (password masked while typing); session stored in the OS credential store only after success |
| `:logout` | log out; keeps non-secret profile metadata |
| `:whoami` | show the active session user or anonymous |
| `:feed` | show the home feed |
| `:subscribed` | show your subscribed communities' feed (requires login) |
| `:communities` (or `C`) | open the community list as a centered modal — subscribed communities when logged in, local otherwise; rows show name and subscriber count (a `◉` glyph marks subscribed communities on the all/local lists); `j`/`k` move, `Enter` opens a community, `Esc` closes, `:sort <subscribed\|local\|all>` switches the list |
| `:sort <name>` | set the feed sort — `Active` (default), `Hot`, `New`, `Old`, `TopDay`, `TopWeek`, `TopMonth`, `TopYear`, `TopAll`, `TopHour`, `TopSixHour`, `TopTwelveHour`, `MostComments`, `NewComments`; sticks for the session, so `:subscribed` after `:sort New` matches the web UI's `sort=New`. Inside the communities modal it instead switches the list: `:sort subscribed`, `:sort local`, `:sort all` |
| `:community [<id>]` | open a community feed (defaults to the selected post's community) |
| `:post` | open the selected post |
| `:search <query>` | search posts (filters download history with the panel open) |
| `:open` | open the selected post |
| `:close` | pop the focused modal (thread, communities, or help) |
| `:expand-all-threads` | expand every collapsed comment thread (no default key) |
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

## Modals

Threads, the community list, and help are **floating centered modals** drawn
over the primary content: none ever fills the window — margins on every
side show the feed (or downloads panel) around the box, so they read as
overlays above the content. Each modal carries its own colors — cyan
borders/titles on a dark surface with light text — a self-contained palette
that stands apart from the content behind it on any terminal theme. They stack
bottom-to-top (at most three deep; the depth shows in the title as
`(N/3)` when stacked), and the focused modal — the top of the stack — takes
`j`/`k`, `gg`/`G`, `Enter`, and `Esc`:

- `Enter` on a post opens the **thread modal** (the post and its comments;
  `j`/`k` or `Ctrl-d`/`Ctrl-u` scroll it).
- `C` / `:communities` opens the **community picker**; `Enter` opens the
  selected community's feed and pops the picker.
- `:help [topic]` opens **help** as a single wrapped column
  (command + description per line, `j`/`k` or `Ctrl-d`/`Ctrl-u` scrolls;
  re-help replaces the help modal instead of stacking a second one).
- `Esc` (`:close`) pops one level. `:help` above `C` above a thread is fine:
  each `Esc` unwinds one of them.
- **Navigation replaces the feed and dismisses the thread modal** — its post
  belongs to the old feed context, so `:feed`, `:subscribed`, `:community`,
  `:search`, `:sort`, and page flips never leave a stale thread on screen.
  Overlay modals (help, the community picker) survive navigation.
- While a modal is open, commands that would act on the hidden feed are
  refused with `close the open view (Esc) before using content commands`;
  actions on the post visible inside a thread modal (`:vote`, `:reply`,
  `:save`, ...) keep working.

## Remapping

Configure `[keymaps]` in the config file (see
[Configuration](configuration.md)) or use `:set keymap <name> <keys>`, where
`<name>` is a documented command name: `down`, `up`, `left`, `right`,
`go-to-first` (or `top`), `go-to-last` (or `bottom`), `open`, `media` (or
`open-media`), `refresh`, `next-page` (or `load-more`), `previous-page`,
`close-pane` (or `close`), `insert`, `visual`, `command`, `backspace`,
`cancel-line`, `search` (or `search-forward`), `search-backward`, `back`,
`quit`, `scroll-detail-down`, `scroll-detail-up`, `confirm`, or `cancel`.
The new sequence replaces the command's default binding, and multi-key
sequences (for example `jk`) participate in prefix matching. Keymaps take
effect on the next launch.
