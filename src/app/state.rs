use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use super::actions::PendingAction;
use crate::{
    api::{CommentView, PostView},
    cache::{CacheStore, Draft, DraftId},
    domain::{DownloadId, DownloadRecord, PostId, ProfileContext, ProfileId},
    error::Result,
    input::Mode,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct View {
    pub posts: Vec<PostView>,
    pub selected: Option<usize>,
    pub compose: String,
    pub stale: bool,
    pub next_page: Option<String>,
    /// Cursors of the pages behind the current one, oldest first; `None`
    /// marks the first page. `<` pops the most recent entry to go back.
    pub page_history: Vec<Option<String>>,
    pub feed_query: crate::api::FeedQuery,
    pub search: String,
    /// The sort applied to every feed load this session (`Active` by
    /// default); `:sort <name>` changes it and it sticks until the next
    /// `:sort`.
    pub feed_sort: String,
    /// Open modals, bottom (index 0) to top (last). The last one has focus;
    /// `Esc` pops one level. Threads, the community picker, and help all
    /// live here instead of competing for a split pane.
    pub modals: Vec<Modal>,
    pub downloads: Option<DownloadsPanel>,
}

/// Selection and search state for the session downloads panel.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DownloadsPanel {
    pub query: String,
    pub selected: Option<DownloadId>,
}

/// Open community-list modal (centered over the primary content). The modal
/// fetches `community/list` for its listing on open and whenever `:sort`
/// switches the listing.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CommunitiesModal {
    pub communities: Vec<crate::api::CommunityView>,
    pub listing: crate::api::FeedListing,
    pub selected: Option<usize>,
}

/// The thread view: a post and its comments, opened from the feed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ThreadModal {
    pub post: crate::api::PostDetail,
    /// Scroll offset (in lines) of the thread's content.
    pub scroll: usize,
}

impl ThreadModal {
    pub fn new(post: crate::api::PostDetail) -> Self {
        Self { post, scroll: 0 }
    }

    /// Placeholder shown while the post fetch is in flight; the result
    /// replaces it.
    pub fn empty() -> Self {
        Self::new(crate::api::PostDetail {
            post: crate::api::PostView {
                id: crate::PostId(0),
                title: String::new(),
                body: None,
                url: None,
                community_id: crate::CommunityId(0),
                creator_id: crate::UserId(0),
                score: 0,
                comments: 0,
                published: None,
            },
            comments: Vec::new(),
        })
    }
}

/// The searchable help index, shown as a scrollable wrapped list.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct HelpModal {
    /// Query filtering the index (empty = all commands).
    pub query: String,
    /// Scroll offset (in lines) of the help content.
    pub scroll: usize,
}

impl HelpModal {
    pub fn new(query: String) -> Self {
        Self { query, scroll: 0 }
    }
}

/// A transient overlay drawn over the primary content. Modals are stacked
/// bottom (index 0) to top (last); the last one has focus and `Esc` pops one
/// level. Opening beyond [`MAX_MODALS`] evicts the oldest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Modal {
    /// The thread view, opened from the feed. Contextual: any command that
    /// replaces the feed contents dismisses it.
    Thread(ThreadModal),
    /// The community-list picker.
    Communities(CommunitiesModal),
    /// Searchable command help (the query filters the index).
    Help(HelpModal),
}

/// Depth cap for the modal stack; deeper nesting is evicted oldest-first.
pub const MAX_MODALS: usize = 3;

impl CommunitiesModal {
    pub fn new(listing: crate::api::FeedListing) -> Self {
        Self {
            communities: Vec::new(),
            listing,
            selected: None,
        }
    }

    pub fn selected_community(&self) -> Option<crate::domain::CommunityId> {
        self.selected
            .and_then(|index| self.communities.get(index))
            .map(|community| community.id)
    }

    pub fn move_selection(&mut self, delta: isize) {
        let Some(max) = self.communities.len().checked_sub(1) else {
            self.selected = None;
            return;
        };
        let current = self.selected.unwrap_or_default() as isize;
        self.selected = Some((current + delta).clamp(0, max as isize) as usize);
    }

    pub fn goto_first(&mut self, count: usize) {
        let Some(last) = self.communities.len().checked_sub(1) else {
            self.selected = None;
            return;
        };
        self.selected = Some(count.saturating_sub(1).min(last));
    }

    pub fn goto_last(&mut self, count: usize) {
        let Some(last) = self.communities.len().checked_sub(1) else {
            self.selected = None;
            return;
        };
        self.selected = Some(if count <= 1 {
            last
        } else {
            count.saturating_sub(1).min(last)
        });
    }
}

impl Default for View {
    fn default() -> Self {
        Self {
            posts: Vec::new(),
            selected: None,
            compose: String::new(),
            stale: false,
            next_page: None,
            page_history: Vec::new(),
            feed_query: crate::api::FeedQuery::home(),
            search: String::new(),
            feed_sort: "Active".into(),
            modals: Vec::new(),
            downloads: None,
        }
    }
}

impl View {
    pub fn selected_post(&self) -> Option<PostId> {
        self.selected
            .and_then(|index| self.posts.get(index))
            .map(|post| post.id)
    }

    pub fn clear_profile_transient(&mut self) {
        self.posts.clear();
        self.selected = None;
        self.compose.clear();
        self.stale = false;
        self.next_page = None;
        self.page_history.clear();
        self.feed_query = crate::api::FeedQuery::home();
        // The chosen sort is a session preference, not profile data: keep it
        // across profile switches so `:sort New` then switching instances
        // still loads the new instance's feed sorted by New.
        self.feed_query.sort = self.feed_sort.clone();
        self.search.clear();
        self.modals.clear();
    }

    pub fn has_modals(&self) -> bool {
        !self.modals.is_empty()
    }

    /// The focused modal: the last entry in the stack.
    pub fn top_modal(&self) -> Option<&Modal> {
        self.modals.last()
    }

    pub fn top_modal_mut(&mut self) -> Option<&mut Modal> {
        self.modals.last_mut()
    }

    /// Pop the focused modal, returning it.
    pub fn pop_modal(&mut self) -> Option<Modal> {
        self.modals.pop()
    }

    /// Push a modal on top, evicting the oldest when the stack is full.
    pub fn push_modal(&mut self, modal: Modal) {
        if self.modals.len() >= MAX_MODALS {
            self.modals.remove(0);
        }
        self.modals.push(modal);
    }

    /// Dismiss every thread modal. Feed-navigation commands call this: a
    /// thread belongs to the post that was selected in the feed, so changing
    /// the feed must never leave the old thread visible.
    pub fn dismiss_threads(&mut self) {
        self.modals
            .retain(|modal| !matches!(modal, Modal::Thread(_)));
    }

    /// The focused thread's comments, when a thread modal is on top.
    pub fn selected_comments(&self) -> &[CommentView] {
        match self.top_modal() {
            Some(Modal::Thread(thread)) => thread.post.comments.as_slice(),
            _ => &[],
        }
    }

    pub fn downloads_active(&self) -> bool {
        self.downloads.is_some()
    }

    pub fn open_downloads_panel(&mut self) {
        self.downloads.get_or_insert_with(DownloadsPanel::default);
    }

    pub fn close_downloads_panel(&mut self) {
        self.downloads = None;
    }

    pub fn selected_download(&self) -> Option<DownloadId> {
        self.downloads.as_ref().and_then(|panel| panel.selected)
    }

    pub fn move_download_selection(&mut self, delta: isize, ids: &[DownloadId]) {
        let Some(panel) = &mut self.downloads else {
            return;
        };
        if ids.is_empty() {
            panel.selected = None;
            return;
        }
        let current = panel
            .selected
            .and_then(|id| ids.iter().position(|candidate| *candidate == id))
            .unwrap_or(0) as isize;
        let max = ids.len().saturating_sub(1) as isize;
        panel.selected = Some(ids[(current + delta).clamp(0, max) as usize]);
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Status {
    pub message: String,
    pub error: Option<String>,
    pub retryable: bool,
    pub stale: bool,
    pub pending: bool,
    pub confirmation_pending: bool,
    pub profile_name: String,
    pub instance_url: String,
}

impl Status {
    pub fn ready(context: &ProfileContext) -> Self {
        Self {
            message: String::new(),
            error: None,
            retryable: false,
            stale: false,
            pending: false,
            confirmation_pending: false,
            profile_name: context
                .profile
                .account_label
                .clone()
                .unwrap_or_else(|| context.profile.id.to_string()),
            instance_url: context.profile.instance_url.to_string(),
        }
    }

    pub fn is_retryable(&self) -> bool {
        self.retryable
    }

    pub fn set_context(&mut self, context: &ProfileContext) {
        self.profile_name = context
            .profile
            .account_label
            .clone()
            .unwrap_or_else(|| context.profile.id.to_string());
        self.instance_url = context.profile.instance_url.to_string();
    }

    pub fn success(&mut self, message: impl Into<String>) {
        self.message = message.into();
        self.error = None;
        self.retryable = false;
        self.pending = false;
        self.confirmation_pending = false;
    }

    pub fn failure(&mut self, error: impl Into<String>) {
        self.message.clear();
        self.error = Some(error.into());
        self.retryable = true;
        self.pending = false;
        self.confirmation_pending = false;
    }
}

#[derive(Clone)]
pub struct DraftStore {
    backend: Arc<dyn CacheStore>,
    profile: ProfileId,
    sequence: Arc<AtomicU64>,
    completed: Arc<std::sync::Mutex<HashMap<ProfileId, HashSet<DraftId>>>>,
}

impl DraftStore {
    pub fn new(backend: Arc<dyn CacheStore>, profile: ProfileId) -> Self {
        Self {
            backend,
            profile,
            sequence: Arc::new(AtomicU64::new(1)),
            completed: Arc::new(std::sync::Mutex::new(HashMap::new())),
        }
    }

    pub fn profile(&self) -> &ProfileId {
        &self.profile
    }

    pub fn set_profile(&mut self, profile: ProfileId) {
        self.profile = profile;
    }
    pub fn begin_comment_draft(&self) -> Draft {
        self.begin_draft("create_comment", "comment".into())
    }

    pub fn begin_post_draft(&self) -> Draft {
        self.begin_draft("create_post", "Untitled".into())
    }

    pub fn begin_edit_post_draft(&self, id: PostId) -> Draft {
        self.begin_draft("edit_post", id.0.to_string())
    }

    pub fn begin_edit_comment_draft(&self, id: crate::domain::CommentId) -> Draft {
        self.begin_draft("edit_comment", id.0.to_string())
    }

    fn begin_draft(&self, operation: &str, content: String) -> Draft {
        let id = DraftId::new(format!(
            "{}-{}",
            operation,
            self.sequence.fetch_add(1, Ordering::Relaxed)
        ));
        let draft = Draft::new(id, self.profile.clone(), operation, content);
        let _ = self.backend.save_draft(draft.clone());
        draft
    }

    pub fn validate(&self, draft: &Draft) -> Result<()> {
        if draft.profile != self.profile {
            return Err(crate::error::AppError::Authorization(
                "draft belongs to another profile".into(),
            ));
        }
        match draft.operation.as_str() {
            "create_post" => {
                if draft
                    .content
                    .lines()
                    .next()
                    .is_none_or(|title| title.trim().is_empty())
                {
                    Err(crate::error::AppError::InvalidCommand(
                        "post title is required".into(),
                    ))
                } else {
                    Ok(())
                }
            }
            "create_comment" | "reply" => {
                if draft.content.trim().is_empty() {
                    Err(crate::error::AppError::InvalidCommand(
                        "comment content is required".into(),
                    ))
                } else {
                    Ok(())
                }
            }
            "edit_comment" => {
                // The first line is the object id; the extracted content is the rest.
                let mut lines = draft.content.lines();
                let _ = lines.next();
                let content = lines.collect::<Vec<_>>().join("\n");
                if content.trim().is_empty() {
                    Err(crate::error::AppError::InvalidCommand(
                        "comment content is required".into(),
                    ))
                } else {
                    Ok(())
                }
            }
            "edit_post" => {
                // The first line is the object id; the extracted title is the next line.
                let mut lines = draft.content.lines();
                let _ = lines.next();
                if lines.next().is_none_or(|title| title.trim().is_empty()) {
                    Err(crate::error::AppError::InvalidCommand(
                        "post title is required".into(),
                    ))
                } else {
                    Ok(())
                }
            }
            _ => Err(crate::error::AppError::InvalidCommand(format!(
                "unsupported draft operation: {}",
                draft.operation
            ))),
        }
    }

    pub fn save(&self, draft: Draft) -> Result<()> {
        self.backend.save_draft(draft)
    }

    pub fn update(&self, id: &DraftId, content: impl Into<String>) -> Result<()> {
        let mut draft = self
            .draft(id)
            .ok_or_else(|| crate::error::AppError::Storage("draft not found".into()))?;
        draft.content = content.into();
        self.save(draft)
    }

    pub fn draft(&self, id: &DraftId) -> Option<Draft> {
        if self.completed.lock().ok().is_some_and(|completed| {
            completed
                .get(&self.profile)
                .is_some_and(|ids| ids.contains(id))
        }) {
            return None;
        }
        self.backend
            .load_drafts(&self.profile)
            .ok()?
            .into_iter()
            .find(|draft| &draft.id == id)
    }

    pub fn all(&self) -> Vec<Draft> {
        self.backend
            .load_drafts(&self.profile)
            .unwrap_or_default()
            .into_iter()
            .filter(|draft| {
                !self.completed.lock().ok().is_some_and(|completed| {
                    completed
                        .get(&self.profile)
                        .is_some_and(|ids| ids.contains(&draft.id))
                })
            })
            .collect()
    }

    pub fn mark_completed(&self, id: DraftId) {
        if let Ok(mut completed) = self.completed.lock() {
            completed
                .entry(self.profile.clone())
                .or_default()
                .insert(id);
        }
    }
}

pub struct AppState {
    pub mode: Mode,
    pub active: ProfileContext,
    pub view: View,
    pub status: Status,
    pub drafts: DraftStore,
    pub pending: Option<PendingAction>,
}

impl AppState {
    pub fn new(active: ProfileContext, cache: Arc<dyn CacheStore>) -> Self {
        let drafts = DraftStore::new(cache, active.profile.id.clone());
        Self {
            mode: Mode::Normal,
            status: Status::ready(&active),
            active,
            view: View::default(),
            drafts,
            pending: None,
        }
    }

    pub fn select(&mut self, post: PostId) {
        self.view.selected = self
            .view
            .posts
            .iter()
            .position(|candidate| candidate.id == post);
        if self.view.posts.is_empty() {
            self.view.selected = None;
        }
    }

    pub fn select_index(&mut self, index: usize) {
        self.view.selected = (index < self.view.posts.len()).then_some(index);
    }
    pub fn selected_index(&self) -> usize {
        self.view.selected.unwrap_or_default()
    }
    pub fn selected_post(&self) -> Option<PostId> {
        self.view.selected_post()
    }
    pub fn selected_comments(&self) -> &[CommentView] {
        self.view.selected_comments()
    }
    pub fn begin_comment_draft(&self) -> Draft {
        self.drafts.begin_comment_draft()
    }
    pub fn begin_post_draft(&self) -> Draft {
        self.drafts.begin_post_draft()
    }
    pub fn begin_edit_post_draft(&self, id: PostId) -> Draft {
        self.drafts.begin_edit_post_draft(id)
    }
    pub fn begin_edit_comment_draft(&self, id: crate::domain::CommentId) -> Draft {
        self.drafts.begin_edit_comment_draft(id)
    }
    pub fn draft(&self, id: DraftId) -> Option<Draft> {
        self.drafts.draft(&id)
    }

    pub fn update_draft(&self, id: &DraftId, content: impl Into<String>) -> Result<()> {
        self.drafts.update(id, content)
    }

    pub fn switch_context(&mut self, context: ProfileContext) {
        self.active = context;
        self.view.clear_profile_transient();
        self.drafts.set_profile(self.active.profile.id.clone());
        self.pending = None;
        self.status = Status::ready(&self.active);
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RenderModel {
    pub mode: Mode,
    pub posts: Vec<PostView>,
    pub selected: Option<usize>,
    pub compose: String,
    pub search: String,
    pub has_more: bool,
    pub status: Status,
    pub downloads: Option<DownloadsRender>,
    /// Open modals, bottom to top; the last is focused.
    pub modals: Vec<Modal>,
}

/// Render snapshot of the downloads panel, populated by the application layer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DownloadsRender {
    pub query: String,
    pub selected: Option<DownloadId>,
    pub records: Vec<DownloadRecord>,
}

impl AppState {
    pub fn render_model(&self) -> RenderModel {
        RenderModel {
            mode: self.mode,
            posts: self.view.posts.clone(),
            selected: self.view.selected,
            compose: self.view.compose.clone(),
            search: self.view.search.clone(),
            has_more: self.view.next_page.is_some(),
            status: self.status.clone(),
            downloads: None,
            modals: self.view.modals.clone(),
        }
    }
}
