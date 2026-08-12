use std::{collections::HashSet, sync::{atomic::{AtomicU64, Ordering}, Arc}};

use crate::{
    api::{CommentView, PostDetail, PostView},
    cache::{CacheStore, Draft, DraftId},
    domain::{PostId, ProfileContext, ProfileId},
    error::Result,
    input::Mode,
};
use super::actions::PendingAction;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct View {
    pub posts: Vec<PostView>,
    pub detail: Option<PostDetail>,
    pub selected: Option<usize>,
    pub compose: String,
    pub stale: bool,
}

impl View {
    pub fn selected_post(&self) -> Option<PostId> {
        self.selected
            .and_then(|index| self.posts.get(index))
            .map(|post| post.id)
    }

    pub fn clear_profile_transient(&mut self) {
        self.posts.clear();
        self.detail = None;
        self.selected = None;
        self.compose.clear();
        self.stale = false;
    }

    pub fn selected_comments(&self) -> &[CommentView] {
        self.detail.as_ref().map(|detail| detail.comments.as_slice()).unwrap_or_default()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Status {
    pub message: String,
    pub error: Option<String>,
    pub retryable: bool,
    pub stale: bool,
    pub pending: bool,
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
            profile_name: context.profile.account_label.clone().unwrap_or_else(|| context.profile.id.to_string()),
            instance_url: context.profile.instance_url.to_string(),
        }
    }

    pub fn is_retryable(&self) -> bool { self.retryable }

    pub fn set_context(&mut self, context: &ProfileContext) {
        self.profile_name = context.profile.account_label.clone().unwrap_or_else(|| context.profile.id.to_string());
        self.instance_url = context.profile.instance_url.to_string();
    }

    pub fn success(&mut self, message: impl Into<String>) {
        self.message = message.into();
        self.error = None;
        self.retryable = false;
        self.pending = false;
    }

    pub fn failure(&mut self, error: impl Into<String>) {
        self.message.clear();
        self.error = Some(error.into());
        self.retryable = true;
        self.pending = false;
    }
}

#[derive(Clone)]
pub struct DraftStore {
    backend: Arc<dyn CacheStore>,
    profile: ProfileId,
    sequence: Arc<AtomicU64>,
    completed: Arc<std::sync::Mutex<HashSet<DraftId>>>,
}

impl DraftStore {
    pub fn new(backend: Arc<dyn CacheStore>, profile: ProfileId) -> Self {
        Self { backend, profile, sequence: Arc::new(AtomicU64::new(1)), completed: Arc::new(std::sync::Mutex::new(HashSet::new())) }
    }

    pub fn profile(&self) -> &ProfileId { &self.profile }

    pub fn set_profile(&mut self, profile: ProfileId) {
        self.profile = profile;
        if let Ok(mut completed) = self.completed.lock() { completed.clear(); }
    }

    pub fn begin_comment_draft(&self) -> Draft {
        let id = DraftId::new(format!("comment-{}", self.sequence.fetch_add(1, Ordering::Relaxed)));
        let draft = Draft::new(id, self.profile.clone(), "create_comment", String::new());
        let _ = self.backend.save_draft(draft.clone());
        draft
    }

    pub fn save(&self, draft: Draft) -> Result<()> {
        self.backend.save_draft(draft)
    }

    pub fn draft(&self, id: &DraftId) -> Option<Draft> {
        if self.completed.lock().ok().is_some_and(|completed| completed.contains(id)) { return None; }
        self.backend.load_drafts(&self.profile).ok()?.into_iter().find(|draft| &draft.id == id)
    }

    pub fn all(&self) -> Vec<Draft> {
        self.backend.load_drafts(&self.profile).unwrap_or_default().into_iter().filter(|draft| !self.completed.lock().ok().is_some_and(|completed| completed.contains(&draft.id))).collect()
    }

    pub fn mark_completed(&self, id: DraftId) {
        if let Ok(mut completed) = self.completed.lock() { completed.insert(id); }
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
        Self { mode: Mode::Normal, status: Status::ready(&active), active, view: View::default(), drafts, pending: None }
    }

    pub fn select(&mut self, post: PostId) {
        self.view.selected = self.view.posts.iter().position(|candidate| candidate.id == post).or(Some(0).filter(|_| self.view.posts.is_empty()));
        if self.view.posts.is_empty() { self.view.selected = None; }
    }

    pub fn selected_post(&self) -> Option<PostId> { self.view.selected_post() }
    pub fn selected_comments(&self) -> &[CommentView] { self.view.selected_comments() }
    pub fn begin_comment_draft(&self) -> Draft { self.drafts.begin_comment_draft() }
    pub fn draft(&self, id: DraftId) -> Option<Draft> { self.drafts.draft(&id) }

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
    pub detail: Option<PostDetail>,
    pub compose: String,
    pub status: Status,
}

impl AppState {
    pub fn render_model(&self) -> RenderModel {
        RenderModel { mode: self.mode, posts: self.view.posts.clone(), detail: self.view.detail.clone(), compose: self.view.compose.clone(), status: self.status.clone() }
    }
}
