# Comment Thread Support Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Render comment replies as a nested, collapsible thread in the thread modal — carrying the tree structure from the Lemmy response through the cache, with `j`/`k` cursor movement over visible comments and `z`/`Z` thread toggles.

**Architecture:** The Lemmy `comment/list` response already contains the whole tree flattened in path order with each comment's ltree `path` (`"0.<ancestor ids>.<own id>"`). The client currently drops `path` at parse time. This plan adds `path` to `CommentView` (plus cache round-trip), builds a pure pre-order `CommentTree` (new `src/app/thread.rs`) from the flat list, and adds cursor + collapsed-set state to `ThreadModal`. Rendering walks visible rows (descendants of collapsed comments skipped), indents by depth, and marks threads with `▸`/`▾ N replies`. The flat `Vec<CommentView>` remains the source of truth; all existing mutation arms keep working unchanged.

**Tech Stack:** Rust, ratatui, tokio (existing lemex stack). No new dependencies.

## Global Constraints

- Spec: `docs/superpowers/specs/2026-08-15-comment-threads-design.md` is the contract; deviations must be flagged in the commit message.
- `CommentView` gains exactly one field: `pub path: Option<String>` — raw ltree from the server; `None` means top-level.
- Keep the flat `post.comments` list as the single source of truth — do not convert state to a tree.
- `path` is a plain string in the SQLite cache JSON; old rows without it decode to `None` (no migration).
- Depth 0 = top-level; indent = `depth * 2` spaces, capped at 16.
- Marker suffix on header lines only for comments with replies: ` ▾ N replies` (expanded) / ` ▸ N replies` (collapsed). Leaf headers keep the exact current shape `[score] name:`.
- Keys: `z` = `ToggleCommentThread`, `Z` = `CollapseAllCommentThreads`, `:expand-all-threads` = `ExpandAllCommentThreads`. `z`/`Z` are currently unbound; do not repurpose `Enter`, `Ctrl-d`, or `Ctrl-u`.
- Existing render assertions like `text.contains("[3] alice:")` and `text.contains("Thread comments: 2")` MUST keep passing.
- Every task ends with a commit. Run `cargo test` only after each task's own tests; never run the full suite mid-task.
- Rust edition/version per `rust-toolchain.toml`; `usize::div_ceil` is stable (1.73+) and may be used.

---

### Task 1: Carry `path` through the API adapter

**Files:**
- Modify: `src/api/mod.rs:99-108` (`CommentView`)
- Modify: `src/api/http.rs:445-465` (`normalize_comment`)
- Test: `tests/api_adapter.rs:58-72` (extend) and add a new test

**Interfaces:**
- Produces: `CommentView.path: Option<String>` — consumed by Task 2 (cache), Task 3 (tree), Task 7 (render).

- [ ] **Step 1: Write the failing tests**

Append to `tests/api_adapter.rs` (after `comment_list_normalizes_thread_comments`):

```rust
#[test]
fn comment_list_normalizes_paths_into_tree_positions() {
    let body = r#"{"comments":[
        {"comment":{"id":7,"post_id":1,"content":"A threaded comment","creator_id":2,"path":"0.7"},"creator":{"id":2,"name":"alice"},"counts":{"score":4}},
        {"comment":{"id":8,"post_id":1,"content":"A reply","creator_id":2,"path":"0.7.8"},"creator":{"id":2,"name":"alice"},"counts":{"score":-1}}
    ]}"#;
    let api = fixture_api_with_body(body);
    let comments = api.comments(&anonymous_context(), PostId(1)).await.unwrap();
    assert_eq!(comments.len(), 2);
    assert_eq!(comments[0].path.as_deref(), Some("0.7"));
    assert_eq!(comments[1].path.as_deref(), Some("0.7.8"));
}
```

In the existing `comment_list_normalizes_thread_comments` test (its body has no `path`), add:

```rust
    assert_eq!(comments[0].path, None, "missing path decodes as top-level");
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --test api_adapter comment_list`
Expected: FAIL — `no field path on type CommentView`.

- [ ] **Step 3: Add the field to `CommentView`**

In `src/api/mod.rs`, in `pub struct CommentView` (after `creator_name`, before `score`):

```rust
    /// The comment's tree position, as returned by the server (ltree
    /// string like "0.12.34.56", always ending with the comment's own id).
    /// `None` when the server omitted it; such comments render as
    /// top-level.
    pub path: Option<String>,
```

- [ ] **Step 4: Parse `path` in `normalize_comment`**

In `src/api/http.rs`, `normalize_comment` (currently returns `CommentView { id, post_id, content, creator_id, creator_name, score }`):

```rust
    CommentView {
        id: CommentId(number(comment, "id")),
        post_id,
        content: crate::text::clean_text(string(comment, "content").unwrap_or_default().as_str()),
        creator_id: UserId(number(comment, "creator_id")),
        creator_name: crate::text::clean_text(
            string(creator, "name")
                .unwrap_or_else(|| "unknown".to_owned())
                .as_str(),
        ),
        score: metric(
            value.get("counts").unwrap_or(&Value::Null),
            comment,
            "score",
        ),
        path: string(comment, "path"),
    }
```

- [ ] **Step 5: Fix every remaining `CommentView` literal**

Every other `CommentView { ... }` construction must add `path: None,` (or a real path). Sites: `src/app/repository.rs:1087` (test), `src/app/render.rs:1095,1103,1138`, `tests/application.rs:754,850,1228,3647`, and `src/app/repository.rs:913` (`comment_from_value`) — the cache decoder gets a placeholder `path: None` NOW so the crate compiles at this task boundary; Task 2 replaces that line with the parsed value. Run `cargo build 2>&1 | grep -n "missing field"` to confirm none remain.

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test --test api_adapter comment_list`
Expected: PASS (both tests).

- [ ] **Step 7: Commit**

```bash
git add src/api/mod.rs src/api/http.rs src/app/repository.rs src/app/render.rs tests/api_adapter.rs tests/application.rs
git commit -m "feat: carry comment path (tree position) through the API adapter"
```

---

### Task 2: Cache round-trip for `path`

**Files:**
- Modify: `src/app/repository.rs:885-887` (`comment_to_value`), `:904-914` (`comment_from_value`)
- Test: `src/app/repository.rs` tests module (end of file)

**Interfaces:**
- Consumes: `CommentView.path` (Task 1).
- Produces: cache rows that preserve `path`; legacy rows (no `path` key) decode to `None`.

- [ ] **Step 1: Write the failing test**

Append to the tests module at the end of `src/app/repository.rs`:

```rust
#[test]
fn comment_round_trip_keeps_path_and_legacy_rows_decode() {
    let comment = CommentView {
        id: crate::CommentId(9),
        post_id: crate::PostId(1),
        content: "first".into(),
        creator_id: crate::UserId(3),
        creator_name: "alice".into(),
        score: 1,
        path: Some("0.9".into()),
    };
    let value = comment_to_value(&comment);
    let round = comment_from_value(&value).expect("decode");
    assert_eq!(round.path.as_deref(), Some("0.9"));
    // Rows written before path existed must decode as top-level.
    let mut legacy = value.clone();
    legacy.as_object_mut().expect("object").remove("path");
    let legacy_round = comment_from_value(&legacy).expect("legacy decode");
    assert_eq!(legacy_round.path, None);
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --lib comment_round_trip_keeps_path`
Expected: FAIL — `path` not serialized (decode yields `None`).

- [ ] **Step 3: Implement**

In `comment_to_value`:

```rust
fn comment_to_value(comment: &CommentView) -> Value {
    json!({ "id": comment.id.0, "post_id": comment.post_id.0, "content": comment.content, "creator_id": comment.creator_id.0, "creator_name": comment.creator_name, "score": comment.score, "path": comment.path })
}
```

In `comment_from_value`, the literal already carries `path: None` from Task 1; replace that placeholder line with the real parse (keep every other field extraction verbatim):

```rust
        path: value.get("path").and_then(Value::as_str).map(str::to_owned),
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test --lib comment_round_trip_keeps_path`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/app/repository.rs
git commit -m "feat: round-trip comment path through the cache"
```

---

### Task 3: Comment tree module

**Files:**
- Create: `src/app/thread.rs`
- Modify: `src/app/mod.rs:5` (add `pub mod thread;` after `pub mod state;`)

**Interfaces:**
- Consumes: `CommentView.path` (Task 1).
- Produces:
  - `pub struct CommentRow { pub id: CommentId, pub depth: u8, pub reply_count: usize }`
  - `pub struct CommentTree` with:
    - `pub fn build(comments: &[CommentView]) -> Self`
    - `pub fn visible_indices(&self, collapsed: &HashSet<CommentId>) -> Vec<usize>`
    - `pub fn has_replies(&self, id: CommentId) -> bool`
    - `pub fn subtree_size(&self, id: CommentId) -> usize`
    - `pub fn row_index(&self, id: CommentId) -> Option<usize>`
    - `pub fn nearest_visible_ancestor(&self, id: CommentId, collapsed: &HashSet<CommentId>) -> CommentId`
    - `pub fn visible_row_start(&self, comments: &[CommentView], collapsed: &HashSet<CommentId>, id: CommentId, width: usize) -> Option<usize>`
  - Consumed by Task 4 (state), Task 6 (movement/toggle arms), Task 7 (render).

**Design note (deviation from spec §4.2, same observable behavior):** the spec lists `collapsed: bool` on `CommentRow`; since collapse state is query-time, not tree data, the field is omitted and `visible_indices(collapsed)` / `has_replies` provide the state instead. Flag this in the commit message.

- [ ] **Step 1: Write the failing tests**

Create `src/app/thread.rs` with the module doc + types and a `#[cfg(test)] mod tests` first (tests reference the not-yet-existing API):

```rust
//! Comment tree structure: pre-order rows derived from Lemmy's `path`
//! ltree strings, plus the visible-row filtering behind collapsible
//! threads. Pure logic — no I/O, no rendering — so the tree is
//! unit-testable in isolation.

use std::collections::{HashMap, HashSet};

use crate::api::CommentView;
use crate::domain::CommentId;

/// One comment in pre-order (ancestors before descendants, siblings in
/// server order). Depth 0 is a top-level comment.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommentRow {
    pub id: CommentId,
    pub depth: u8,
    /// Total descendant count (0 for leaves).
    pub reply_count: usize,
}

/// Pre-order rows for a thread, plus the parent/child links needed to
/// filter collapsed subtrees.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CommentTree {
    pub rows: Vec<CommentRow>,
    index: HashMap<CommentId, usize>,
    parent: HashMap<CommentId, CommentId>,
    children: HashMap<CommentId, Vec<usize>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn comment(id: i64, path: Option<&str>) -> CommentView {
        CommentView {
            id: CommentId(id),
            post_id: crate::PostId(1),
            content: format!("comment {id}"),
            creator_id: crate::UserId(1),
            creator_name: "alice".into(),
            score: 0,
            path: path.map(str::to_owned),
        }
    }

    #[test]
    fn build_derives_depth_parent_and_reply_counts() {
        let tree = CommentTree::build(&[
            comment(1, Some("0.1")),
            comment(2, Some("0.1.2")),
            comment(3, Some("0.1.2.3")),
            comment(4, Some("0.1.4")),
            comment(5, Some("0.5")),
        ]);
        let ids: Vec<i64> = tree.rows.iter().map(|row| row.id.0).collect();
        assert_eq!(ids, vec![1, 2, 3, 4, 5], "pre-order: ancestors before descendants");
        assert_eq!(tree.rows[0].depth, 0);
        assert_eq!(tree.rows[1].depth, 1);
        assert_eq!(tree.rows[2].depth, 2);
        assert_eq!(tree.rows[1].reply_count, 2, "comment 2 has two descendants");
        assert_eq!(tree.rows[0].reply_count, 3);
        assert_eq!(tree.rows[4].reply_count, 0);
    }

    #[test]
    fn no_path_and_missing_parent_render_top_level() {
        let tree = CommentTree::build(&[
            comment(1, None),
            comment(2, Some("0.99.2")), // parent 99 not in the list
            comment(3, Some("0.1.3")),  // parent 1 is present
        ]);
        let ids: Vec<i64> = tree.rows.iter().map(|row| row.id.0).collect();
        assert_eq!(ids, vec![1, 3, 2], "children follow their parent; orphan promoted");
        assert_eq!(tree.rows[2].depth, 0, "promoted orphan is top-level");
    }

    #[test]
    fn malformed_paths_are_top_level() {
        let tree = CommentTree::build(&[
            comment(1, Some("0.1.2")), // own segment 2 != id 1
            comment(2, Some("bogus")),
            comment(3, Some("0.1")),
        ]);
        let ids: Vec<i64> = tree.rows.iter().map(|row| row.id.0).collect();
        assert_eq!(ids, vec![1, 2, 3]);
        assert!(tree.rows.iter().all(|row| row.depth == 0));
    }

    #[test]
    fn collapsing_hides_only_the_subtree() {
        let tree = CommentTree::build(&[
            comment(1, Some("0.1")),
            comment(2, Some("0.1.2")),
            comment(3, Some("0.1.2.3")),
            comment(4, Some("0.4")),
        ]);
        let mut collapsed = HashSet::new();
        collapsed.insert(CommentId(2));
        let visible: Vec<i64> = tree
            .visible_indices(&collapsed)
            .iter()
            .map(|&i| tree.rows[i].id.0)
            .collect();
        assert_eq!(visible, vec![1, 2, 4], "collapsed root stays, descendants hidden");
        assert!(tree.is_hidden(CommentId(3), &collapsed));
        assert!(!tree.is_hidden(CommentId(2), &collapsed));
    }

    #[test]
    fn leaf_collapse_is_a_noop() {
        let tree = CommentTree::build(&[comment(1, Some("0.1")), comment(2, Some("0.1.2"))]);
        assert!(!tree.has_replies(CommentId(2)));
        assert!(tree.has_replies(CommentId(1)));
        assert_eq!(tree.subtree_size(CommentId(1)), 1);
        assert_eq!(tree.subtree_size(CommentId(2)), 0);
    }

    #[test]
    fn nearest_visible_ancestor_walks_to_the_collapsed_root() {
        let tree = CommentTree::build(&[
            comment(1, Some("0.1")),
            comment(2, Some("0.1.2")),
            comment(3, Some("0.1.2.3")),
        ]);
        let mut collapsed = HashSet::new();
        collapsed.insert(CommentId(2));
        assert_eq!(
            tree.nearest_visible_ancestor(CommentId(3), &collapsed),
            CommentId(2)
        );
        assert_eq!(
            tree.nearest_visible_ancestor(CommentId(2), &collapsed),
            CommentId(2)
        );
    }

    #[test]
    fn visible_row_start_counts_preceding_rows_and_wrapping() {
        let comments = vec![
            comment(1, Some("0.1")),
            comment(2, Some("0.1.2")),
            comment(3, Some("0.3")),
        ];
        let tree = CommentTree::build(&comments);
        let collapsed = HashSet::new();
        // comment 1 starts at line 0; its header is at line 1 (blank first).
        assert_eq!(tree.visible_row_start(&comments, &collapsed, CommentId(1), 100), Some(0));
        // comment 2: 2 lines for comment 1 (blank + header) + 1 content line.
        assert_eq!(tree.visible_row_start(&comments, &collapsed, CommentId(2), 100), Some(3));
        // A long content (60 chars) wraps to 2 lines at width 40.
        let comments = vec![
            CommentView {
                id: CommentId(1),
                post_id: crate::PostId(1),
                content: "x".repeat(60),
                creator_id: crate::UserId(1),
                creator_name: "alice".into(),
                score: 0,
                path: Some("0.1".into()),
            },
            comment(2, Some("0.1.2")),
        ];
        let tree = CommentTree::build(&comments);
        assert_eq!(tree.visible_row_start(&comments, &collapsed, CommentId(2), 40), Some(4));
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib thread::`
Expected: FAIL — module not found / functions not defined.

- [ ] **Step 3: Implement the module**

Replace the module body (keep the tests) with:

```rust
impl CommentTree {
    /// Build a pre-order tree from the flat comment list. Comments with no
    /// `path` (or a malformed one) render as top-level in list order; a
    /// reply whose path parent is missing is promoted to top-level rather
    /// than dropped.
    pub fn build(comments: &[CommentView]) -> Self {
        let mut index = HashMap::with_capacity(comments.len());
        let mut parsed: Vec<(CommentId, (Option<CommentId>, u8))> =
            Vec::with_capacity(comments.len());
        for (position, comment) in comments.iter().enumerate() {
            index.insert(comment.id, position);
            parsed.push((comment.id, parse_path(comment.path.as_deref(), comment.id)));
        }
        let mut parent = HashMap::new();
        let mut children: HashMap<CommentId, Vec<usize>> = HashMap::new();
        let mut roots: Vec<usize> = Vec::new();
        for (position, (id, (path_parent, _))) in parsed.iter().enumerate() {
            match path_parent {
                Some(ancestor) if index.contains_key(ancestor) => {
                    parent.insert(*id, *ancestor);
                    children.entry(*ancestor).or_default().push(position);
                }
                _ => roots.push(position),
            }
        }
        let mut rows = Vec::with_capacity(comments.len());
        walk(&parsed, &children, &roots, &mut rows);
        Self {
            rows,
            index,
            parent,
            children,
        }
    }

    /// Pre-order indices of the rows whose ancestors are not collapsed; a
    /// collapsed comment itself stays visible, its subtree is hidden.
    pub fn visible_indices(&self, collapsed: &HashSet<CommentId>) -> Vec<usize> {
        (0..self.rows.len())
            .filter(|&position| !self.is_hidden(self.rows[position].id, collapsed))
            .collect()
    }

    /// True when `id` has at least one descendant.
    pub fn has_replies(&self, id: CommentId) -> bool {
        self.row_index(id)
            .is_some_and(|position| self.rows[position].reply_count > 0)
    }

    /// Total descendant count of `id` (0 for leaves or unknown ids).
    pub fn subtree_size(&self, id: CommentId) -> usize {
        self.row_index(id)
            .map_or(0, |position| self.rows[position].reply_count)
    }

    /// The pre-order row index of `id`, when present.
    pub fn row_index(&self, id: CommentId) -> Option<usize> {
        self.index.get(&id).copied()
    }

    /// The nearest ancestor of `id` whose own subtree is visible (itself
    /// not under a collapsed ancestor). Used to keep the cursor on screen
    /// when a collapse-all hides the selection.
    pub fn nearest_visible_ancestor(
        &self,
        id: CommentId,
        collapsed: &HashSet<CommentId>,
    ) -> CommentId {
        let mut current = id;
        while let Some(ancestor) = self.parent.get(&current).copied() {
            if !self.is_hidden(ancestor, collapsed) {
                return ancestor;
            }
            current = ancestor;
        }
        id
    }

    /// Approximate first rendered line of a visible row, used by cursor
    /// movement to keep the focused row on screen without duplicating the
    /// renderer's wrap math. Each row occupies a blank line, a header
    /// line, and the comment body wrapped at `width` columns (body text
    /// may contain newlines — `clean_text` keeps them).
    pub fn visible_row_start(
        &self,
        comments: &[CommentView],
        collapsed: &HashSet<CommentId>,
        id: CommentId,
        width: usize,
    ) -> Option<usize> {
        let visible = self.visible_indices(collapsed);
        let position = visible.iter().position(|&p| self.rows[p].id == id)?;
        let mut start = 0usize;
        for &p in &visible[..position] {
            let row = &self.rows[p];
            let comment = comments.iter().find(|candidate| candidate.id == row.id)?;
            let indent = (row.depth as usize * 2).min(16);
            let content_width = width.saturating_sub(indent + 2).max(1);
            start += 2 + wrapped_line_count(comment.content.as_str(), content_width);
        }
        Some(start)
    }

    fn is_hidden(&self, id: CommentId, collapsed: &HashSet<CommentId>) -> bool {
        let mut current = self.parent.get(&id);
        while let Some(ancestor) = current {
            if collapsed.contains(ancestor) {
                return true;
            }
            current = self.parent.get(ancestor);
        }
        false
    }
}

/// Pre-order walk: push a row before recursing (so `rows` stays
/// pre-order), then patch the reply count once the subtree is complete.
fn walk(
    parsed: &[(CommentId, (Option<CommentId>, u8))],
    children: &HashMap<CommentId, Vec<usize>>,
    roots: &[usize],
    rows: &mut Vec<CommentRow>,
) {
    fn visit(
        position: usize,
        parsed: &[(CommentId, (Option<CommentId>, u8))],
        children: &HashMap<CommentId, Vec<usize>>,
        rows: &mut Vec<CommentRow>,
    ) -> usize {
        let (id, (_, depth)) = parsed[position];
        let row_index = rows.len();
        rows.push(CommentRow {
            id,
            depth,
            reply_count: 0,
        });
        let mut count = 0usize;
        for &kid in children.get(&id).into_iter().flatten() {
            count += 1 + visit(kid, parsed, children, rows);
        }
        rows[row_index].reply_count = count;
        count
    }
    for &root in roots {
        visit(root, parsed, children, rows);
    }
}

/// Parse an ltree `path` ("0.<ancestor ids...>.<own id>") into the parent
/// id and depth. Depth 0 is top-level. Any malformed value (missing root,
/// non-numeric segments, own-id mismatch) degrades to a top-level comment
/// rather than dropping it.
fn parse_path(path: &str, own_id: CommentId) -> (Option<CommentId>, u8) {
    let segments: Vec<&str> = path.split('.').filter(|s| !s.is_empty()).collect();
    // Root pseudo-node `0` plus at least the comment's own id.
    if segments.len() < 2 || segments[0] != "0" {
        return (None, 0);
    }
    let own_matches = matches!(
        segments.last().and_then(|s| s.parse::<i64>().ok()),
        Some(value) if CommentId(value) == own_id
    );
    if !own_matches {
        return (None, 0);
    }
    let parent = if segments.len() >= 3 {
        segments[segments.len() - 2]
            .parse::<i64>()
            .ok()
            .map(CommentId)
    } else {
        None
    };
    // "0.12" -> depth 0, "0.12.34" -> depth 1.
    let depth = segments.len().saturating_sub(2) as u8;
    (parent, depth)
}

/// Rendered line count of a body at a given column width: one line per
/// source line, wrapped when a line exceeds `width` columns.
fn wrapped_line_count(content: &str, width: usize) -> usize {
    let width = width.max(1);
    content
        .lines()
        .map(|line| line.chars().count().div_ceil(width).max(1))
        .sum()
}
```

Register the module in `src/app/mod.rs` (after `pub mod state;`):

```rust
pub mod thread;
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib thread::`
Expected: PASS (all 7 tests).

- [ ] **Step 5: Commit**

```bash
git add src/app/thread.rs src/app/mod.rs
git commit -m "feat: pre-order comment tree with collapsible visible rows (CommentRow.collapsed dropped in favor of query-time collapsed set)"
```

---

### Task 4: `ThreadModal` cursor and collapse state

**Files:**
- Modify: `src/app/state.rs:113-153` (`ThreadModal`)

**Interfaces:**
- Consumes: `CommentId` (already exported as `crate::CommentId`).
- Produces: `ThreadModal { post, scroll, selected: Option<CommentId>, collapsed: HashSet<CommentId> }` — consumed by Tasks 6 and 7.

- [ ] **Step 1: Write the failing test**

Append a tests module to `src/app/state.rs` (add at end of file):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thread_modal_starts_without_cursor_or_collapsed_threads() {
        let thread = ThreadModal::new(crate::api::PostDetail {
            post: crate::api::PostView {
                id: crate::PostId(1),
                title: "t".into(),
                body: None,
                url: None,
                community_id: crate::CommunityId(1),
                creator_id: crate::UserId(1),
                score: 0,
                comments: 0,
                published: None,
            },
            comments: Vec::new(),
        });
        assert_eq!(thread.selected, None);
        assert!(thread.collapsed.is_empty());
        assert_eq!(thread.scroll, 0);

        let placeholder = ThreadModal::for_post(crate::PostId(7));
        assert_eq!(placeholder.post.post.id, crate::PostId(7));
        assert_eq!(placeholder.selected, None);
        assert!(placeholder.collapsed.is_empty());
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --lib thread_modal_starts_without_cursor`
Expected: FAIL — `no field selected` on `ThreadModal`.

- [ ] **Step 3: Implement**

In `src/app/state.rs`, `use` block: add `CommentId` to the `crate::domain::{...}` import list. Extend the struct:

```rust
/// The thread view: a post and its comments, opened from the feed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ThreadModal {
    pub post: crate::api::PostDetail,
    /// Scroll offset (in lines) of the thread's content.
    pub scroll: usize,
    /// Focused comment (cursor), by id so collapse changes never
    /// invalidate it. `None` when the thread has no comments.
    pub selected: Option<CommentId>,
    /// Comment ids whose reply subtree is collapsed; empty = all expanded.
    pub collapsed: HashSet<CommentId>,
}
```

In both `ThreadModal::new` and `ThreadModal::for_post`:

```rust
        Self {
            post,
            scroll: 0,
            selected: None,
            collapsed: HashSet::new(),
        }
```

(`for_post` already delegates to `new` — only `new` changes.)

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test --lib thread_modal_starts_without_cursor`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/app/state.rs
git commit -m "feat: thread modal cursor and collapsed-thread state"
```

---

### Task 5: Commands, default keys, and help entries

**Files:**
- Modify: `src/input/command.rs` (enum + `by_name`)
- Modify: `src/input/engine.rs:40-45` (default keymap)
- Modify: `src/app/help.rs` (entries)
- Test: `tests/input_engine.rs`

**Interfaces:**
- Produces: `Command::ToggleCommentThread`, `Command::CollapseAllCommentThreads`, `Command::ExpandAllCommentThreads`; names `toggle-thread`, `collapse-all-threads`, `expand-all-threads`; default keys `z` and `Z`. Consumed by Task 6.

- [ ] **Step 1: Write the failing tests**

Append to `tests/input_engine.rs`:

```rust
#[test]
fn z_and_Z_toggle_comment_threads() {
    let mut engine = InputEngine::default();
    assert_eq!(engine.handle(key('z')), Command::ToggleCommentThread);
    assert_eq!(engine.handle(key('Z')), Command::CollapseAllCommentThreads);
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --test input_engine z_and_Z`
Expected: FAIL — `cannot find variant ToggleCommentThread`.

- [ ] **Step 3: Implement**

In `src/input/command.rs`, add to the `Command` enum (near `ScrollDetailUp`):

```rust
    /// Collapse or expand the focused comment's reply thread (default
    /// key: `z`).
    ToggleCommentThread,
    /// Collapse every comment thread in the open thread modal (default
    /// key: `Z`).
    CollapseAllCommentThreads,
    /// Expand every collapsed comment thread (command: `:expand-all-threads`).
    ExpandAllCommentThreads,
```

In `by_name`:

```rust
            "toggle-thread" => Some(Command::ToggleCommentThread),
            "collapse-all-threads" => Some(Command::CollapseAllCommentThreads),
            "expand-all-threads" => Some(Command::ExpandAllCommentThreads),
```

In `src/input/engine.rs`, `InputEngine::new` (after the `C` mapping):

```rust
        // Thread folds: `z` toggles the focused comment's thread, `Z`
        // collapses every thread at once (`:expand-all-threads` has no
        // default key).
        mappings.insert('z', Command::ToggleCommentThread);
        mappings.insert('Z', Command::CollapseAllCommentThreads);
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test --test input_engine z_and_Z`
Expected: PASS.

- [ ] **Step 5: Help entries**

In `src/app/help.rs`, add a `// Threads` section (after the `// Navigation` entries, before `// Media`):

```rust
    // Threads
    HelpEntry {
        command: "j / k",
        description: "move the comment cursor in the open thread (collapsed threads are skipped)",
        group: "thread",
    },
    HelpEntry {
        command: "z",
        description: "collapse or expand the focused comment's reply thread",
        group: "thread",
    },
    HelpEntry {
        command: "Z",
        description: "collapse every comment thread in the open thread",
        group: "thread",
    },
    HelpEntry {
        command: ":expand-all-threads",
        description: "expand every collapsed comment thread",
        group: "thread",
    },
```

Update the existing `j/k` navigation entry's description (currently `"move down/up the selection; with the detail/thread pane open, scroll the thread (the pane takes focus)"`) to:

```rust
        description: "move down/up the selection; with a thread modal open, move the comment cursor (the pane takes focus)",
```

- [ ] **Step 6: Verify help still renders**

Run: `cargo test --lib help`
Expected: PASS (help entries render with the new group).

- [ ] **Step 7: Commit**

```bash
git add src/input/command.rs src/input/engine.rs src/app/help.rs tests/input_engine.rs
git commit -m "feat: toggle-thread, collapse-all-threads, expand-all-threads commands and keys"
```

---

### Task 6: Cursor movement and thread-toggle handlers

**Files:**
- Modify: `src/app/mod.rs` — `MoveDown`/`MoveUp` thread arms (`:688-753`), new `dispatch_command` arms (after the `ScrollDetailUp` arm, ~`:785`), `use` imports (`:8-31`)
- Modify: `src/app/render.rs` — add `thread_inner_size` helper (near `modal_area`, ~`:212`)
- Test: `src/app/mod.rs` tests module (end of file)

**Interfaces:**
- Consumes: `CommentTree` (Task 3), `ThreadModal.selected/collapsed` (Task 4), commands (Task 5), `crate::app::render::thread_inner_size`.
- Produces: `j`/`k` move the cursor over visible comments and keep it on screen; `z` toggles the focused thread; `Z` collapses all with selection clamp; `:expand-all-threads` clears the collapsed set.

- [ ] **Step 1: Add the `thread_inner_size` helper (needed by the movement arms)**

In `src/app/render.rs`, after `modal_area`:

```rust
/// The thread modal's inner (usable) size for a terminal of the given
/// size: the 90%-of-content modal box (see `modal_area`) minus its
/// borders. The cursor-follow scroll math in `App` mirrors this so `j`/`k`
/// keep the focused comment on screen without duplicating layout.
pub fn thread_inner_size(terminal_width: u16, terminal_height: u16) -> (u16, u16) {
    let content_height = terminal_height.saturating_sub(3 + 5 + 6);
    let modal_width = (terminal_width * 9 / 10)
        .max(40)
        .min(terminal_width.saturating_sub(2));
    let modal_height = (content_height * 9 / 10)
        .max(10)
        .min(content_height.saturating_sub(2));
    (
        modal_width.saturating_sub(2),
        modal_height.saturating_sub(2),
    )
}
```

- [ ] **Step 2: Write the failing tests**

Append to the tests module at the end of `src/app/mod.rs`:

```rust
    fn thread_comment(id: i64, path: Option<&str>) -> crate::api::CommentView {
        crate::api::CommentView {
            id: crate::CommentId(id),
            post_id: crate::PostId(1),
            content: format!("comment {id}"),
            creator_id: crate::UserId(1),
            creator_name: "alice".into(),
            score: 0,
            path: path.map(str::to_owned),
        }
    }

    fn open_thread(app: &mut App, comments: Vec<crate::api::CommentView>) {
        app.state.view.modals.push(Modal::Thread(ThreadModal::new(
            crate::api::PostDetail {
                post: crate::api::PostView {
                    id: crate::PostId(1),
                    title: "Threaded post".into(),
                    body: None,
                    url: None,
                    community_id: crate::CommunityId(1),
                    creator_id: crate::UserId(1),
                    score: 0,
                    comments: comments.len() as i64,
                    published: None,
                },
                comments,
            },
        )));
    }

    fn focused_thread(app: &App) -> &ThreadModal {
        match app.state.view.top_modal().expect("thread modal open") {
            Modal::Thread(thread) => thread,
            _ => panic!("expected a thread modal"),
        }
    }

    #[tokio::test]
    async fn thread_cursor_moves_between_visible_comments_only() {
        let mut app = test_app();
        app.terminal_width = 100;
        app.terminal_height = 40;
        open_thread(
            &mut app,
            vec![
                thread_comment(1, Some("0.1")),
                thread_comment(2, Some("0.1.2")),
                thread_comment(3, Some("0.3")),
            ],
        );

        app.dispatch(AppAction::Input(Command::MoveDown { count: 1 }))
            .await
            .unwrap();
        assert_eq!(focused_thread(&app).selected, Some(crate::CommentId(1)));
        // Collapse comment 1: its reply becomes invisible and `j` skips it.
        app.dispatch(AppAction::Input(Command::ToggleCommentThread))
            .await
            .unwrap();
        app.dispatch(AppAction::Input(Command::MoveDown { count: 1 }))
            .await
            .unwrap();
        assert_eq!(
            focused_thread(&app).selected,
            Some(crate::CommentId(3)),
            "hidden replies are skipped"
        );

        // `k` moves back up over the visible rows.
        app.dispatch(AppAction::Input(Command::MoveUp { count: 1 }))
            .await
            .unwrap();
        assert_eq!(focused_thread(&app).selected, Some(crate::CommentId(1)));

        // Expand again: the reply is reachable once more.
        app.dispatch(AppAction::Input(Command::ToggleCommentThread))
            .await
            .unwrap();
        app.dispatch(AppAction::Input(Command::MoveDown { count: 1 }))
            .await
            .unwrap();
        assert_eq!(focused_thread(&app).selected, Some(crate::CommentId(2)));
    }

    #[tokio::test]
    async fn toggle_thread_collapses_and_expands_only_threads_with_replies() {
        let mut app = test_app();
        open_thread(
            &mut app,
            vec![
                thread_comment(1, Some("0.1")),
                thread_comment(2, Some("0.1.2")),
            ],
        );
        app.dispatch(AppAction::Input(Command::MoveDown { count: 1 }))
            .await
            .unwrap();
        app.dispatch(AppAction::Input(Command::ToggleCommentThread))
            .await
            .unwrap();
        assert!(focused_thread(&app).collapsed.contains(&crate::CommentId(1)));
        app.dispatch(AppAction::Input(Command::ToggleCommentThread))
            .await
            .unwrap();
        assert!(
            focused_thread(&app).collapsed.is_empty(),
            "toggling again expands the thread"
        );

        // A leaf comment has nothing to collapse.
        app.dispatch(AppAction::Input(Command::MoveDown { count: 1 }))
            .await
            .unwrap();
        app.dispatch(AppAction::Input(Command::ToggleCommentThread))
            .await
            .unwrap();
        assert!(
            focused_thread(&app).collapsed.is_empty(),
            "toggling a leaf is a noop"
        );
    }

    #[tokio::test]
    async fn collapse_all_clamps_a_hidden_cursor_to_its_visible_ancestor() {
        let mut app = test_app();
        open_thread(
            &mut app,
            vec![
                thread_comment(1, Some("0.1")),
                thread_comment(2, Some("0.1.2")),
                thread_comment(3, Some("0.1.2.3")),
                thread_comment(4, Some("0.4")),
            ],
        );
        // Move down to the deepest comment (id 3).
        for _ in 0..3 {
            app.dispatch(AppAction::Input(Command::MoveDown { count: 1 }))
                .await
                .unwrap();
        }
        assert_eq!(focused_thread(&app).selected, Some(crate::CommentId(3)));

        app.dispatch(AppAction::Input(Command::CollapseAllCommentThreads))
            .await
            .unwrap();
        let thread = focused_thread(&app);
        assert_eq!(
            thread.collapsed,
            HashSet::from([crate::CommentId(1), crate::CommentId(2)])
        );
        assert_eq!(
            thread.selected,
            Some(crate::CommentId(1)),
            "a hidden cursor clamps to the nearest visible ancestor: comment 2 is itself hidden under collapsed comment 1"
        );

        app.dispatch(AppAction::Input(Command::ExpandAllCommentThreads))
            .await
            .unwrap();
        assert!(
            focused_thread(&app).collapsed.is_empty(),
            "expand-all clears every collapsed thread"
        );
    }
```

Add the `test_app` helper next to the other test helpers in the same tests module (it mirrors the `App::new` calls already used there):

```rust
    fn test_app() -> App {
        App::new(
            Arc::new(crate::api::fixtures::fixture_api("feed.json")),
            Arc::new(crate::cache::MemoryCache::default()),
            ProfileContext {
                profile: Profile {
                    id: ProfileId::from("fixture"),
                    instance_url: Url::parse("http://127.0.0.1/").unwrap(),
                    account_label: None,
                },
                session: None,
            },
            Arc::new(crate::profiles::MemoryCredentialStore::default()),
        )
    }
```

(`App::new` and the fixture helpers are already imported/used by existing tests in this module; add `ThreadModal` and `Modal` to the tests' `use super::*` — they are re-exported from `crate::app::state`.)

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test --lib thread_cursor_moves_between_visible_comments_only`
Expected: FAIL — `MoveDown` still scrolls, so `selected` stays `None`.

- [ ] **Step 4: Implement the handlers**

Add to the `use` block at the top of `src/app/mod.rs` (import from the new module):

```rust
use crate::app::thread::CommentTree;
```

Replace the `Some(Modal::Thread(thread))` arm inside `Command::MoveDown`:

```rust
                    Some(Modal::Thread(thread)) => {
                        let (width, height) = crate::app::render::thread_inner_size(
                            self.terminal_width,
                            self.terminal_height,
                        );
                        move_thread_cursor(thread, count as usize, true, width, height);
                        return Ok(());
                    }
```

and inside `Command::MoveUp`:

```rust
                    Some(Modal::Thread(thread)) => {
                        let (width, height) = crate::app::render::thread_inner_size(
                            self.terminal_width,
                            self.terminal_height,
                        );
                        move_thread_cursor(thread, count as usize, false, width, height);
                        return Ok(());
                    }
```

Add the two private helpers as module-level functions (outside any `impl` block, next to the other top-level functions in the file). They take `&mut ThreadModal` directly instead of going through `&self`, because the caller already holds a mutable borrow of `self.state.view` (the terminal size is read before that borrow starts):

```rust
/// Move the thread modal's cursor to the next (`down`) or previous (`up`)
/// visible comment, keeping the focused row on screen. Hidden replies
/// (collapsed threads) are skipped; the cursor never leaves the visible
/// rows.
fn move_thread_cursor(
    thread: &mut ThreadModal,
    count: usize,
    down: bool,
    width: u16,
    height: u16,
) {
    let tree = CommentTree::build(&thread.post.comments);
    let visible = tree.visible_indices(&thread.collapsed);
    if visible.is_empty() {
        return;
    }
    let current = thread.selected.and_then(|id| tree.row_index(id));
    let target = match visible.iter().position(|&row| Some(row) == current) {
        // No selection yet: the first j or k lands on the first row.
        None => 0,
        Some(position) if down => (position + count).min(visible.len() - 1),
        Some(position) => position.saturating_sub(count),
    };
    let id = tree.rows[visible[target]].id;
    thread.selected = Some(id);
    keep_thread_cursor_visible(thread, &tree, id, width as usize, height as usize);
}

/// Anchor the thread's scroll offset so the focused comment's header
/// stays inside the viewport, using the same 90%-modal geometry the
/// renderer uses (`thread_inner_size`). Wrap is estimated by
/// `visible_row_start`, so deeply wrapped rows can still sit slightly
/// off-screen until the next move.
fn keep_thread_cursor_visible(
    thread: &mut ThreadModal,
    tree: &CommentTree,
    id: crate::CommentId,
    width: usize,
    height: usize,
) {
    let Some(start) = tree.visible_row_start(&thread.post.comments, &thread.collapsed, id, width)
    else {
        return;
    };
    let start = start + thread_header_lines(&thread.post, width);
    // `visible_row_start` points at the row's blank separator line; the
    // header (the highlighted `[score] name:` line) is one line later.
    let header = start.saturating_add(1);
    if header < thread.scroll {
        thread.scroll = header;
    } else if header >= thread.scroll + height {
        thread.scroll = header.saturating_add(1).saturating_sub(height);
    }
}
```

Add the three new arms to `dispatch_command`, directly after the `Command::ScrollDetailUp` arm:

```rust
            Command::ToggleCommentThread => {
                if let Some(Modal::Thread(thread)) = self.state.view.top_modal_mut()
                    && let Some(id) = thread.selected
                    && CommentTree::build(&thread.post.comments).has_replies(id)
                {
                    if !thread.collapsed.remove(&id) {
                        thread.collapsed.insert(id);
                    }
                }
                Ok(())
            }
            Command::CollapseAllCommentThreads => {
                if let Some(Modal::Thread(thread)) = self.state.view.top_modal_mut() {
                    let tree = CommentTree::build(&thread.post.comments);
                    thread.collapsed = tree
                        .rows
                        .iter()
                        .filter(|row| row.reply_count > 0)
                        .map(|row| row.id)
                        .collect();
                    if let Some(selected) = thread.selected {
                        let visible = tree.visible_indices(&thread.collapsed);
                        let hidden = !visible.contains(&tree.row_index(selected).unwrap_or(usize::MAX));
                        if hidden {
                            thread.selected =
                                Some(tree.nearest_visible_ancestor(selected, &thread.collapsed));
                        }
                    }
                }
                Ok(())
            }
            Command::ExpandAllCommentThreads => {
                if let Some(Modal::Thread(thread)) = self.state.view.top_modal_mut() {
                    thread.collapsed.clear();
                }
                Ok(())
            }
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test --lib thread_cursor_moves_between_visible_comments_only toggle_thread_collapses_and_expands_only_threads_with_replies collapse_all_clamps_a_hidden_cursor_to_its_visible_ancestor`
Expected: PASS (all three).

- [ ] **Step 6: Commit**

```bash
git add src/app/mod.rs src/app/render.rs
git commit -m "feat: j/k move the thread cursor, z/Z toggle comment threads"
```

---

### Task 7: Thread rendering

**Files:**
- Modify: `src/app/render.rs` — `render_thread` (`:243-294`), imports (`:1-12`)
- Test: `src/app/render.rs` tests module

**Interfaces:**
- Consumes: `CommentTree` (Task 3), `ThreadModal.selected/collapsed` (Task 4).
- Produces: nested rendering with `▸`/`▾ N replies` markers, cursor highlight (`BOLD | REVERSED` on the header line), `Thread comments: N (M hidden)` header, updated modal title.

- [ ] **Step 1: Write the failing tests**

Append to the tests module in `src/app/render.rs`:

```rust
    fn threaded_post() -> crate::api::PostDetail {
        crate::api::PostDetail {
            post: crate::api::PostView {
                id: crate::PostId(1),
                title: "Threaded post".into(),
                body: None,
                url: None,
                community_id: crate::CommunityId(1),
                creator_id: crate::UserId(1),
                score: 0,
                comments: 3,
                published: None,
            },
            comments: vec![
                crate::api::CommentView {
                    id: crate::CommentId(1),
                    post_id: crate::PostId(1),
                    content: "top".into(),
                    creator_id: crate::UserId(2),
                    creator_name: "alice".into(),
                    score: 3,
                    path: Some("0.1".into()),
                },
                crate::api::CommentView {
                    id: crate::CommentId(2),
                    post_id: crate::PostId(1),
                    content: "reply".into(),
                    creator_id: crate::UserId(2),
                    creator_name: "bob".into(),
                    score: -1,
                    path: Some("0.1.2".into()),
                },
                crate::api::CommentView {
                    id: crate::CommentId(3),
                    post_id: crate::PostId(1),
                    content: "nested reply".into(),
                    creator_id: crate::UserId(2),
                    creator_name: "carol".into(),
                    score: 0,
                    path: Some("0.1.2.3".into()),
                },
            ],
        }
    }

    #[test]
    fn thread_nests_replies_and_marks_threads() {
        let mut model = model(None, false);
        model
            .modals
            .push(Modal::Thread(ThreadModal::new(threaded_post())));
        let text = rendered_at(&model, 140, 48);
        assert!(
            text.contains("[3] alice: ▾ 2 replies"),
            "expanded thread marker with reply count; rendered: {text}"
        );
        assert!(
            text.contains("  [-1] bob: ▾ 1 reply"),
            "replies indent and carry their own marker; rendered: {text}"
        );
        assert!(
            text.contains("    [0] carol:"),
            "grandchildren indent deeper; rendered: {text}"
        );
        assert!(
            text.contains("Thread comments: 3"),
            "no hidden count when nothing is collapsed; rendered: {text}"
        );
    }

    #[test]
    fn collapsed_thread_hides_descendants_and_reports_hidden() {
        let mut model = model(None, false);
        let mut thread = ThreadModal::new(threaded_post());
        thread.collapsed.insert(crate::CommentId(1));
        thread.selected = Some(crate::CommentId(1));
        model.modals.push(Modal::Thread(thread));
        let text = rendered_at(&model, 140, 48);
        assert!(
            text.contains("[3] alice: ▸ 2 replies"),
            "collapsed marker; rendered: {text}"
        );
        assert!(
            !text.contains("nested reply") && !text.contains("reply"),
            "descendant content is hidden; rendered: {text}"
        );
        assert!(
            text.contains("Thread comments: 3 (2 hidden)"),
            "the header reports hidden replies; rendered: {text}"
        );
    }

    #[test]
    fn thread_cursor_header_is_highlighted() {
        let mut model = model(None, false);
        let mut thread = ThreadModal::new(crate::api::PostDetail {
            post: crate::api::PostView {
                id: crate::PostId(1),
                title: "Threaded post".into(),
                body: None,
                url: None,
                community_id: crate::CommunityId(1),
                creator_id: crate::UserId(1),
                score: 0,
                comments: 1,
                published: None,
            },
            comments: vec![crate::api::CommentView {
                id: crate::CommentId(1),
                post_id: crate::PostId(1),
                content: "body text".into(),
                creator_id: crate::UserId(2),
                creator_name: "alice".into(),
                score: 3,
                path: None,
            }],
        });
        thread.selected = Some(crate::CommentId(1));
        model.modals.push(Modal::Thread(thread));
        let width = 140u16;
        let height = 48u16;
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("test backend");
        terminal.draw(|frame| render(frame, &model)).expect("render");
        let buffer = terminal.backend().buffer();
        let content = &buffer.content;
        // Buffer cells hold one grapheme each; locate the header row by
        // scanning joined row text, then check that row for the highlight.
        let row = (0..height)
            .find(|&row| {
                let start = row as usize * width as usize;
                let end = start + width as usize;
                let text: String = content[start..end]
                    .iter()
                    .map(|cell| cell.symbol())
                    .collect();
                text.contains("[3] alice:")
            })
            .expect("comment header renders");
        let start = row as usize * width as usize;
        let end = start + width as usize;
        let highlighted = content[start..end]
            .iter()
            .any(|cell| cell.style().add_modifier.contains(Modifier::REVERSED));
        assert!(highlighted, "the focused comment header must be highlighted");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib thread_nests_replies_and_marks_threads`
Expected: FAIL — no indentation/markers in the rendered text.

- [ ] **Step 3: Implement `render_thread`**

Replace the body of `render_thread` (keep the signature) with:

```rust
    let area = modal_area(content, 9, 9);
    let (inner, surface) = modal_chrome(
        frame,
        area,
        format!("Thread{depth} — j/k: move, z: toggle thread, Ctrl-d/u: scroll, Esc to close"),
        colors,
    );

    let detail = &thread.post;
    let tree = CommentTree::build(&detail.comments);
    let visible = tree.visible_indices(&thread.collapsed);
    let by_id: std::collections::HashMap<crate::CommentId, &CommentView> =
        detail.comments.iter().map(|comment| (comment.id, comment)).collect();

    let mut lines = vec![Line::from(Span::styled(
        detail.post.title.as_str(),
        Style::default().add_modifier(Modifier::BOLD),
    ))];
    if let Some(body) = &detail.post.body
        && !body.is_empty()
    {
        lines.push(Line::from(""));
        lines.push(Line::from(body.as_str()));
    }
    let hidden = tree.rows.len().saturating_sub(visible.len());
    let header = if hidden == 0 {
        format!("Thread comments: {}", tree.rows.len())
    } else {
        format!("Thread comments: {} ({} hidden)", tree.rows.len(), hidden)
    };
    lines.push(Line::from(header));
    for &position in &visible {
        let row = &tree.rows[position];
        let comment = by_id[&row.id];
        let indent = (row.depth as usize * 2).min(16);
        let selected = Some(row.id) == thread.selected;
        let style = if selected {
            Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED)
        } else {
            Style::default()
        };
        let marker = if tree.has_replies(row.id) {
            let glyph = if thread.collapsed.contains(&row.id) { "▸" } else { "▾" };
            let plural = if row.reply_count == 1 { "" } else { "s" };
            format!(" {glyph} {} reply{plural}", row.reply_count)
        } else {
            String::new()
        };
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::raw(" ".repeat(indent).as_str()),
            Span::styled(
                format!("[{}] {}:{marker}", comment.score, comment.creator_name),
                style,
            ),
        ]));
        lines.push(Line::from(vec![
            Span::raw(" ".repeat(indent + 2).as_str()),
            Span::raw(comment.content.as_str()),
        ]));
    }
    // Clamp the scroll offset so a short thread (or a very long scroll) can
    // never leave blank space under the box; wrapped lines are longer than
    // the line count, so reaching the absolute bottom of a deeply wrapped
    // comment may need one more Ctrl-d. Cursor-following is the movement
    // arms' job (`keep_thread_cursor_visible` in `App`): this render-side
    // clamp must NOT react to the cursor, or a manual Ctrl-d scroll would
    // snap back on the next frame.
    let scroll = thread
        .scroll
        .min(lines.len().saturating_sub(inner.height as usize)) as u16;
    let paragraph = Paragraph::new(lines)
        .style(surface)
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));
    frame.render_widget(paragraph, inner);
```

Add to the imports at the top of `src/app/render.rs`:

```rust
use super::thread::CommentTree;
use crate::api::CommentView;
```

(If `CommentView` is already imported via `use super::*`-style paths elsewhere, match the file's existing import style.)

Note on scroll ownership: cursor-following lives entirely in the movement arms (`keep_thread_cursor_visible` in Task 6), and this render-side clamp only bounds the bottom of the content. That keeps a manual `Ctrl-d`/`Ctrl-u` scroll from being fought on the next frame. The pre-existing `thread_scroll_shifts_content_above_the_fold` test still passes unchanged.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib thread_nests_replies_and_marks_threads collapsed_thread_hides_descendants_and_reports_hidden thread_cursor_header_is_highlighted thread_shows_comment_scores_without_ids_and_with_spacing thread_scroll_shifts_content_above_the_fold`
Expected: PASS (all five — including the two pre-existing render tests, which must not regress).

- [ ] **Step 5: Commit**

```bash
git add src/app/render.rs
git commit -m "feat: render nested comment threads with collapsible markers and cursor highlight"
```

---

### Task 8: Keybindings doc and end-to-end smoke test

**Files:**
- Modify: `docs/keybindings.md`
- Modify: `tests/smoke.rs` (add `NESTED_THREAD_BODY` const + one test)

**Interfaces:**
- Consumes: everything from Tasks 1-7.

- [ ] **Step 1: Update `docs/keybindings.md`**

Change the `j / k` row:

```markdown
| `j` / `k` | move / move cursor | move the feed selection; with a thread modal open, move the comment cursor (collapsed threads are skipped) |
```

Add after the `Ctrl-d / Ctrl-u` row:

```markdown
| `z` | toggle thread | collapse or expand the focused comment's reply thread |
| `Z` | collapse all threads | collapse every comment thread in the open thread |
```

In the command-line table (the `| \`:\` | purpose |` section), add a row right after the `:close` row (which reads `| \`:close\` | pop the focused modal (thread, communities, or help) |`):

```markdown
| `:expand-all-threads` | expand every collapsed comment thread (no default key) |
```

- [ ] **Step 2: Write the failing smoke test**

In `tests/smoke.rs`, after `POST_THREAD_BODY`:

```rust
/// A fixture body whose thread contains one reply, so the smoke test can
/// prove nested threads arrive and collapse.
const NESTED_THREAD_BODY: &str = r#"{"post_view":{"post":{"id":1,"name":"Fixture post","body":"Fixture body","url":"https://example.com/fixture","community_id":1,"creator_id":1,"published":"2026-01-01T00:00:00Z","score":3},"counts":{"score":3,"comments":2}},"comments":[{"comment":{"id":1,"post_id":1,"content":"Top comment","creator_id":1,"path":"0.1"},"creator":{"id":1,"name":"alice"},"counts":{"score":1}},{"comment":{"id":2,"post_id":1,"content":"Nested reply","creator_id":1,"path":"0.1.2"},"creator":{"id":1,"name":"alice"},"counts":{"score":1}}]}"#;
```

Add a test after `opening_post_shows_thread_and_back_preserves_feed_position`:

```rust
/// Nested threads: the reply arrives in the same fetch, and `z` collapses
/// the focused thread's subtree.
#[test]
fn nested_thread_arrives_and_collapses_with_z() {
    let runtime = support::runtime();
    let api = support::api(&runtime, || fixture_api_with_body(NESTED_THREAD_BODY));
    let mut app = FixtureApp::with_runtime(
        runtime,
        "post",
        api,
        anonymous_context(),
        MediaConfig::default(),
        &[],
    );
    app.app.state.view.posts = vec![post_view(1, "Fixture post", None)];
    app.app.state.view.selected = Some(0);

    app.dispatch(AppAction::OpenSelected)
        .expect("open selected post");
    let lemex::app::Modal::Thread(thread) =
        app.app.state.view.top_modal().expect("thread modal opens")
    else {
        panic!("opening a post must push a thread modal");
    };
    assert_eq!(thread.post.comments.len(), 2, "the nested reply arrives in one fetch");
    assert_eq!(
        thread.post.comments[1].path.as_deref(),
        Some("0.1.2"),
        "the reply keeps its tree position"
    );

    app.dispatch(AppAction::Input(Command::MoveDown { count: 1 }))
        .expect("focus the top comment");
    app.dispatch(AppAction::Input(Command::ToggleCommentThread))
        .expect("toggle the focused thread");
    let lemex::app::Modal::Thread(thread) = app.app.state.view.top_modal().unwrap() else {
        panic!("thread modal still open");
    };
    assert!(
        thread.collapsed.contains(&lemex::CommentId(1)),
        "z collapses the focused comment's thread"
    );
    assert_eq!(
        thread.selected,
        Some(lemex::CommentId(1)),
        "the cursor stays on the collapsed root"
    );

    app.dispatch(AppAction::Input(Command::ToggleCommentThread))
        .expect("expand again");
    let lemex::app::Modal::Thread(thread) = app.app.state.view.top_modal().unwrap() else {
        panic!("thread modal still open");
    };
    assert!(thread.collapsed.is_empty(), "z expands the thread again");
}
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cargo test --test smoke nested_thread_arrives_and_collapses_with_z`
Expected: FAIL — `MoveDown` scrolls instead of selecting, so `ToggleCommentThread` is a noop and `collapsed` stays empty.

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test --test smoke nested_thread_arrives_and_collapses_with_z`
Expected: PASS.

- [ ] **Step 5: Run the focused suite for every touched area**

Run:
```bash
cargo test --lib
cargo test --test api_adapter
cargo test --test input_engine
cargo test --test smoke
cargo test --test application
```
Expected: all pass.

- [ ] **Step 6: Commit**

```bash
git add docs/keybindings.md tests/smoke.rs
git commit -m "feat: document thread keys and cover nested threads end to end"
```

---

## Self-Review Notes (from the plan author)

- Spec coverage: §4.1 (data) → Tasks 1-2; §4.2 (tree) → Task 3; §4.3 (state) → Task 4; §4.4 (interactions) → Tasks 5-6; §4.5 (rendering) → Task 7; §5 (testing) → spread across tasks + Task 8. Non-goals (reply composition, lazy fetch, moderation) are untouched.
- Known deviation: `CommentRow.collapsed` from spec §4.2 is replaced by query-time `visible_indices(collapsed)` + `has_replies` (flagged in Task 3's commit). Observable behavior identical.
- Scroll-follow is approximate by design (`visible_row_start` estimates wrap); the renderer's bottom clamp and the existing "one more Ctrl-d for deeply wrapped comments" caveat absorb the imprecision, and a manual `Ctrl-d` scroll is never fought (the cursor clamp lives only in the movement arms).
