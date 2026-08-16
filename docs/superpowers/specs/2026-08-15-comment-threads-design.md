# Comment Thread Support

## Product specification

**Status:** Approved
**Date:** 2026-08-15
**Baseline:** levim, Rust + ratatui (see `docs/superpowers/specs/2026-08-11-lemmy-client-design.md`)

## 1. Problem

The thread view opens a post and its comments, but nested replies are not
presented as a thread:

- The fetch already returns the complete nested thread. `GET /api/v3/comment/list`
  with `post_id`, `type_=All`, `sort=Top`, `max_depth=10` returns every comment
  in the post at every depth (verified in Lemmy `0.17`/`0.19` and `main`):
  - `max_depth` binds without `parent_id`; when present, pagination is ignored
    (0.17/0.19: unbounded; modern instances cap the response at 300 rows).
  - The response is the full tree flattened in path (pre-)order; the requested
    sort only breaks ties within a sibling group.
  - Ancestry is encoded in `comment.path`, an ltree string such as `0.12.34.56`
    (root pseudo-node `0` first, the comment's own id last; top-level comments
    are `0.<id>`).
- The client discards that structure at the API boundary: `normalize_comment`
  (`src/api/http.rs`) never reads `path`, `CommentView` (`src/api/mod.rs`) has
  no parent or depth field, and the cache round-trip
  (`comment_to_value`/`comment_from_value`) cannot carry it.
- `render_thread` (`src/app/render.rs`) prints the flat list with no nesting,
  no parent linkage, and no affordance for replies, so replies are
  indistinguishable from top-level comments. `ThreadModal` (`src/app/state.rs`)
  holds only `post` and `scroll` — no cursor, no collapse state.

Thread support is therefore **not implemented**: the payload contains the whole
tree, but the tree never survives parsing.

## 2. Goals

1. Preserve the tree structure from the Lemmy response through the cache and
   into the thread view.
2. Render replies visually nested under their parents with a clear indicator
   when a comment has replies.
3. Make each comment's reply thread a collapsible item: collapse hides its
   descendants, expand reveals them, with a visible affordance for both states.
4. Keep the cursor model consistent with the rest of the client (Vim-like
   `j`/`k` movement; `Ctrl-d`/`Ctrl-u` pane scrolling unchanged).
5. Keep the flat `Vec<CommentView>` as the source of truth so existing
   mutation handling (delete, edit, vote, create) keeps working without
   restructuring state.

## 3. Non-goals

- **Reply composition.** Sending a comment as a reply to a specific comment
  (a `parent_id` on `CreateCommentRequest` and the draft flow) is a separate
  feature and out of scope.
- **Lazy fetching on expand.** Expanding reveals already-fetched replies; it
  never issues a network request. Modern Lemmy instances truncate posts with
  more than 300 comments server-side; that limitation is accepted and not
  papered over.
- **Per-thread sort switching** inside the thread view.
- **Moderator actions on comments** (distinguish, remove, report).

## 4. Design

### 4.1 Data layer

`CommentView` (`src/api/mod.rs`) gains:

```rust
/// The comment's tree position, as returned by the server (ltree string
/// like "0.12.34.56", always ending with the comment's own id). `None`
/// when the server omitted it; such comments render as top-level.
pub path: Option<String>,
```

- `normalize_comment` (`src/api/http.rs:445`) reads `comment.path` as an
  optional string. Absent (fixtures, older responses) → `None`.
- `comment_to_value` / `comment_from_value` (`src/app/repository.rs`) store and
  restore `path`. Cache rows written before this change have no `path`; they
  decode to `None` and render as top-level. No cache migration.
- Mutation responses carry `comment_view.comment.path`; the existing
  `normalize_comment` path is the only parser, so edits/votes/replies keep the
  tree position without extra work.

### 4.2 Tree model — new module `src/app/thread.rs`

A pure logic module (no ratatui, no I/O) so the tree is unit-testable in
isolation:

```rust
pub struct CommentRow {
    pub id: CommentId,
    pub depth: u8,          // 0 = top-level
    pub collapsed: bool,    // this comment's subtree is hidden
    pub reply_count: usize, // descendant count (0 for leaves)
}

pub struct CommentTree {
    /// Pre-order rows: every comment, ancestors before descendants,
    /// siblings in server order.
    pub rows: Vec<CommentRow>,
}

impl CommentTree {
    pub fn build(comments: &[CommentView]) -> Self;
    /// Pre-order row indices with descendants of collapsed comments
    /// removed; computed fresh, so it always reflects the current
    /// `collapsed` set and any mutation to the comment list.
    pub fn visible_indices(&self, collapsed: &HashSet<CommentId>) -> Vec<usize>;
    pub fn visible_rows<'a>(
        &'a self,
        collapsed: &'a HashSet<CommentId>,
    ) -> impl Iterator<Item = &'a CommentRow>;
    /// True when `id` has at least one descendant.
    pub fn has_replies(&self, id: CommentId) -> bool;
    pub fn subtree_size(&self, id: CommentId) -> usize;
}
```

Build rules:

- Parse each `path` by splitting on `.`; drop the leading `0` segment. Depth =
  remaining segment count minus one (so `0.12` → depth 0, `0.12.34` → depth
  1). Parent id = the segment before the comment's own id (`None` for
  top-level).
- Children map keyed by parent id, insertion order preserved. A comment whose
  path parent is missing from the fetched list (should not happen for a full
  tree fetch) is treated as top-level in list order rather than dropped.
- Comments with `path: None` are top-level in list order.
- Pre-order traversal: top-level comments in list order, each followed by its
  descendants in insertion order, recursively.
- `reply_count` = subtree size (total descendants, not direct children).
- `visible_indices` = pre-order indices of rows whose ancestor is not in
  `collapsed`. Collapsing a comment hides exactly its subtree; a collapsed
  comment itself stays visible.

### 4.3 State — `ThreadModal` (`src/app/state.rs`)

```rust
pub struct ThreadModal {
    pub post: crate::api::PostDetail,
    pub scroll: usize,
    /// Focused comment, as a comment id so collapse changes never
    /// invalidate it. `None` when the thread has no comments.
    pub selected: Option<CommentId>,
    /// Comment ids whose reply subtree is collapsed. Empty = all expanded.
    pub collapsed: HashSet<CommentId>,
}
```

- `ThreadModal::new` and `::for_post` initialize `selected: None`,
  `collapsed: HashSet::new()`.
- The flat `post.comments` list remains the single source of truth. Every
  render builds a `CommentTree` from it; every command that needs geometry
  builds one too. ≤ 300 comments, so rebuilding per render is free.

### 4.4 Interactions

| Key | Command | Effect in a thread modal |
|---|---|---|
| `j` | `MoveDown { count }` | Move cursor to the next visible comment; scroll follows so the cursor row stays on screen |
| `k` | `MoveUp { count }` | Move cursor to the previous visible comment; scroll follows |
| `Ctrl-d` | `ScrollDetailDown` | Pane page-scroll (unchanged) |
| `Ctrl-u` | `ScrollDetailUp` | Pane page-scroll (unchanged) |
| `z` | `ToggleCommentThread` (new) | Collapse/expand the focused comment's subtree; noop when it has no replies or nothing is focused |
| `Z` | `CollapseAllCommentThreads` (new) | Collapse every comment that has replies |
| `:expand-all-threads` | `ExpandAllCommentThreads` (new) | Clear `collapsed`; no default key, rebindable |

- `MoveDown`/`MoveUp` in the thread modal currently scroll the pane by one
  line (`src/app/mod.rs:690-743`); this changes to cursor movement. `count`
  repeats the move. The cursor cannot leave the visible rows.
- Collapsing the focused comment keeps the focus on it (still visible). If
  collapse-all hides the focused comment, the cursor moves to the nearest
  visible ancestor (the collapsed root that hides it, or the last visible row
  above it).
- New commands get names `toggle-thread`, `collapse-all-threads`,
  `expand-all-threads` in `Command::by_name` (`src/input/command.rs`), default
  key bindings `z`/`Z` in `InputEngine::new` (`src/input/engine.rs`), and
  help-index entries (`src/app/help.rs`). `z` and `Z` are currently unbound.

### 4.5 Rendering (`render_thread`, `src/app/render.rs`)

Each visible comment renders as:

```
[12] alice: ▾ 2 replies     <- expanded thread, cursor row (bold + reversed)
    [3] bob: ▸ 1 reply      <- collapsed: header and content visible, replies hidden
    [5] carol:              <- leaf: no marker
[8] eve:
```

- Indent = `depth * 2` spaces before the header; the comment body indents
  `depth * 2 + 2` spaces. Indent capped at 16 columns for very deep threads.
- Marker suffix on the header line, only for comments with replies:
  `▾ N replies` when expanded, `▸ N replies` when collapsed. Leaves keep the
  existing bare `[score] name:` line so current render assertions
  (`contains("[3] alice:")`) remain valid.
- Cursor row highlighted with the feed's selected style: `BOLD | REVERSED`
  (`render.rs:189`); the highlight applies to the header line only.
- Thread header line: `Thread comments: N` when nothing is collapsed;
  `Thread comments: N (M hidden)` otherwise.
- Modal title: `Thread — j/k: move, z: toggle thread, Ctrl-d/u: scroll, Esc: close`.
- A collapsed comment shows its own header and content (the item itself is
  never hidden, only its replies).

### 4.6 Error handling

- Malformed `path` values (non-numeric segments, missing own id) degrade to
  top-level rendering; the comment itself is always shown. No error surfaces
  to the user; a malformed path never drops a comment.
- Empty comment lists render the current empty state with no cursor.

## 5. Testing

1. **`src/app/thread.rs` unit tests:** depth/parent/reply-count derivation;
   pre-order; no-path comments as top-level; missing path parent → top-level
   not dropped; `visible` under single and nested collapse; collapse of a leaf
   is a noop.
2. **`src/api` adapter test:** a `comment/list` body with `path` values
   normalizes into `CommentView.path`; a body without `path` yields `None`.
3. **`src/app/repository.rs`:** cache round-trip preserves `path`; an old
   cache row without `path` decodes to `None`.
4. **`src/app/render.rs`:** nested comments indent; markers and reply counts
   appear; collapsed threads hide descendants; cursor row highlighted;
   hidden count in the header.
5. **`src/app/mod.rs`:** `MoveDown`/`MoveUp` move across visible rows only;
   `ToggleCommentThread` collapses and expands; `CollapseAllCommentThreads`;
   selection clamp when collapse-all hides the cursor; `count` repetition.
6. **`tests/smoke.rs`:** a nested fixture thread renders threaded; dispatching
   `z` collapses the subtree.

## 6. Out of scope (explicitly deferred)

- Reply composition (`parent_id` on `CreateCommentRequest`).
- Lazy/fetch-more expansion; server-side 300-row truncation on modern
  instances is accepted.
- Thread-local sort control.
- Comment moderation actions.
