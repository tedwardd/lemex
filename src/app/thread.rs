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
            parsed.push((comment.id, parse_path(comment.path.as_deref().unwrap_or_default(), comment.id)));
        }
        let mut parent = HashMap::new();
        let mut children: HashMap<CommentId, Vec<usize>> = HashMap::new();
        let mut roots: Vec<usize> = Vec::new();
        for position in 0..parsed.len() {
            let (id, (path_parent, _)) = parsed[position];
            match path_parent {
                Some(ancestor) if index.contains_key(&ancestor) => {
                    parent.insert(id, ancestor);
                    children.entry(ancestor).or_default().push(position);
                }
                // Missing parent (or no path): promoted to a top-level
                // root; the walk assigns roots depth 0.
                _ => roots.push(position),
            }
        }
        let mut rows = Vec::with_capacity(comments.len());
        walk(&parsed, &children, &roots, &mut rows);
        // The input-position map above only served the parent-presence
        // check; rows are pre-order, which orphan promotion reorders, so
        // rebuild the stored index from the pre-order rows.
        let index: HashMap<CommentId, usize> = rows
            .iter()
            .enumerate()
            .map(|(position, row)| (row.id, position))
            .collect();
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
        if !self.is_hidden(id, collapsed) {
            return id;
        }
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
/// Depths are re-derived from the tree (roots at 0, children at
/// parent + 1) rather than taken from the ltree string, so promoted
/// orphans and their subtrees render with contiguous depths.
fn walk(
    parsed: &[(CommentId, (Option<CommentId>, u8))],
    children: &HashMap<CommentId, Vec<usize>>,
    roots: &[usize],
    rows: &mut Vec<CommentRow>,
) {
    fn visit(
        position: usize,
        depth: u8,
        parsed: &[(CommentId, (Option<CommentId>, u8))],
        children: &HashMap<CommentId, Vec<usize>>,
        rows: &mut Vec<CommentRow>,
    ) -> usize {
        let (id, _) = parsed[position];
        let row_index = rows.len();
        rows.push(CommentRow {
            id,
            depth,
            reply_count: 0,
        });
        let mut count = 0usize;
        for &kid in children.get(&id).into_iter().flatten() {
            count += 1 + visit(kid, depth + 1, parsed, children, rows);
        }
        rows[row_index].reply_count = count;
        count
    }
    for &root in roots {
        visit(root, 0, parsed, children, rows);
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
            comment(4, Some("0.1.2.4")),
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
        assert_eq!(tree.row_index(CommentId(1)), Some(0));
        assert_eq!(tree.row_index(CommentId(4)), Some(3));
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
        // row_index must answer in pre-order position, not input position.
        assert_eq!(tree.row_index(CommentId(3)), Some(1));
        assert_eq!(tree.row_index(CommentId(2)), Some(2));
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
