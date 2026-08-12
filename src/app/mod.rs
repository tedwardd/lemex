pub mod actions;
pub mod repository;
pub mod state;

use std::sync::Arc;

pub use actions::{ApiResult, AppAction, ProfileCommand, ProfileDraft};
pub use repository::{CachedRead, Repository};
pub use state::{AppState, DraftStore, RenderModel, Status, View};
use crate::{
    api::{FeedQuery, LemmyApi, MutationResult},
    cache::{CacheStore, Draft},
    domain::{CreateCommentRequest, Mutation, Profile, ProfileContext, ProfileId},
    error::Result,
    input::{Command, Mode},
    profiles::CredentialStore,
};

pub struct App {
    pub state: AppState,
    pub repository: Repository,
    quit: bool,
}

impl App {
    pub fn new(
        api: Arc<dyn LemmyApi>,
        cache: Arc<dyn CacheStore>,
        active: ProfileContext,
        credentials: Arc<dyn CredentialStore>,
    ) -> Self {
        let state = AppState::new(active, cache.clone());
        Self { state, repository: Repository::new(api, cache, credentials), quit: false }
    }

    pub fn render_model(&self) -> RenderModel { self.state.render_model() }
    pub fn is_quit(&self) -> bool { self.quit }

    pub async fn dispatch(&mut self, action: AppAction) -> Result<()> {
        match action {
            AppAction::Input(command) => self.dispatch_command(command).await,
            AppAction::Profile(command) => self.dispatch_profile(command).await,
            AppAction::SubmitDraft(id) => self.submit_draft(id).await,
            AppAction::OpenSelected => self.open_selected().await,
            AppAction::Back => { self.state.view.detail = None; self.state.mode = Mode::Normal; Ok(()) }
            AppAction::DeletePost(id) => self.delete_post(id).await,
            AppAction::Confirm => Ok(()),
            AppAction::ApiResult(result) => { self.apply_api_result(result); Ok(()) }
            AppAction::Tick => Ok(()),
            AppAction::Quit => { self.quit = true; Ok(()) }
        }
    }

    async fn dispatch_command(&mut self, command: Command) -> Result<()> {
        match command {
            Command::Open => self.open_selected().await,
            Command::Back => { self.state.view.detail = None; self.state.mode = Mode::Normal; Ok(()) }
            Command::Quit => { self.quit = true; Ok(()) }
            Command::Refresh => self.refresh_feed().await,
            Command::MoveDown { count } => { self.move_selection(count as isize); Ok(()) }
            Command::MoveUp { count } => { self.move_selection(-(count as isize)); Ok(()) }
            Command::EnterInsert => { self.state.mode = Mode::Insert; Ok(()) }
            Command::EnterVisual => { self.state.mode = Mode::Visual; Ok(()) }
            Command::EnterCommand => { self.state.mode = Mode::Command; Ok(()) }
            Command::EnterSearch { backward } => { self.state.mode = if backward { Mode::SearchBackward } else { Mode::SearchForward }; Ok(()) }
            Command::Text(text) => { self.state.view.compose.push_str(&text); Ok(()) }
            Command::SubmitLine(_) | Command::MoveLeft { .. } | Command::MoveRight { .. } | Command::Noop => Ok(()),
        }
    }

    async fn dispatch_profile(&mut self, command: ProfileCommand) -> Result<()> {
        match command {
            ProfileCommand::Switch(id) => self.switch_profile(id).await,
            ProfileCommand::Logout => {
                let id = self.state.active.profile.id.clone();
                if let Err(error) = self.repository.credentials.delete_session(&id).await { self.state.status.failure(error.to_string()); } else { self.state.active.session = None; self.state.status.success("logged out"); }
                Ok(())
            }
            ProfileCommand::WhoAmI => { self.state.status.success(self.state.active.session.as_ref().map(|session| format!("user {}", session.user_id.0)).unwrap_or_else(|| "anonymous".into())); Ok(()) }
            ProfileCommand::List => { self.state.status.success(self.state.active.profile.id.to_string()); Ok(()) }
            ProfileCommand::New(draft) => {
                let context = ProfileContext { profile: Profile { id: draft.id, instance_url: draft.instance_url, account_label: draft.account_label }, session: None };
                self.state.switch_context(context);
                Ok(())
            }
            ProfileCommand::Login | ProfileCommand::Delete(_) => { self.state.status.failure("profile operation requires interactive profile service"); Ok(()) }
        }
    }

    async fn switch_profile(&mut self, id: ProfileId) -> Result<()> {
        let session = match self.repository.session(&id).await { Ok(session) => session, Err(error) => { self.state.status.failure(error.to_string()); return Ok(()); } };
        let mut profile = self.state.active.profile.clone();
        profile.id = id;
        let context = ProfileContext { profile, session };
        self.state.switch_context(context);
        if let Ok(Some(read)) = self.repository.cached_feed(&self.state.active, &FeedQuery::home()) {
            self.state.view.posts = read.value.items;
            self.state.view.stale = read.stale;
            self.state.status.stale = read.stale;
            self.state.status.message = if read.stale { "stale cache loaded".into() } else { "cache loaded".into() };
        }
        Ok(())
    }

    async fn refresh_feed(&mut self) -> Result<()> {
        let context = self.state.active.clone();
        self.state.status.pending = true;
        match self.repository.feed(&context, FeedQuery::home()).await {
            Ok(read) => {
                self.state.view.posts = read.value.items;
                self.state.view.stale = read.stale;
                self.state.status.stale = read.stale;
                if let Some(error) = read.refresh_error { self.state.status.failure(format!("stale cache: {error}")); self.state.status.stale = true; } else { self.state.status.success("feed refreshed"); }
            }
            Err(error) => self.state.status.failure(error.to_string()),
        }
        Ok(())
    }

    async fn open_selected(&mut self) -> Result<()> {
        let Some(id) = self.state.selected_post() else { return Ok(()); };
        let profile = self.state.active.profile.id.clone();
        match self.repository.post(&self.state.active, id).await {
            Ok(detail) => self.apply_api_result(ApiResult::Post { profile, result: Ok(detail) }),
            Err(error) => self.apply_api_result(ApiResult::Post { profile, result: Err(error) }),
        }
        Ok(())
    }

    async fn delete_post(&mut self, id: crate::PostId) -> Result<()> {
        let profile = self.state.active.profile.id.clone();
        let mutation = Mutation::DeletePost(id);
        let result = self.repository.mutate(&self.state.active, mutation.clone()).await;
        self.apply_api_result(ApiResult::Mutation { profile, draft: None, mutation, result });
        Ok(())
    }

    async fn submit_draft(&mut self, id: crate::cache::DraftId) -> Result<()> {
        let Some(draft) = self.state.draft(id.clone()) else { return Ok(()); };
        let mutation = mutation_for_draft(&draft, self.state.selected_post());
        let Some(mutation) = mutation else { self.state.status.failure("unsupported draft operation"); return Ok(()); };
        let profile = self.state.active.profile.id.clone();
        let result = self.repository.mutate(&self.state.active, mutation.clone()).await;
        self.apply_api_result(ApiResult::Mutation { profile, draft: Some(id), mutation, result });
        Ok(())
    }

    fn move_selection(&mut self, delta: isize) {
        if self.state.view.posts.is_empty() { return; }
        let current = self.state.view.selected.unwrap_or(0) as isize;
        let max = self.state.view.posts.len().saturating_sub(1) as isize;
        self.state.view.selected = Some((current + delta).clamp(0, max) as usize);
    }

    fn apply_api_result(&mut self, result: ApiResult) {
        let profile = match &result { ApiResult::Feed { profile, .. } | ApiResult::Post { profile, .. } | ApiResult::Mutation { profile, .. } | ApiResult::Comments { profile, .. } => profile };
        if profile != &self.state.active.profile.id { return; }
        match result {
            ApiResult::Feed { result, stale, .. } => match result { Ok(page) => { self.state.view.posts = page.items; self.state.view.stale = stale; self.state.status.stale = stale; self.state.status.success("feed loaded"); }, Err(error) => self.state.status.failure(error.to_string()) },
            ApiResult::Post { result, .. } => match result { Ok(detail) => { self.state.view.detail = Some(detail); self.state.mode = Mode::Normal; self.state.status.success("post loaded"); }, Err(error) => self.state.status.failure(error.to_string()) },
            ApiResult::Mutation { result, draft, .. } => match result { Ok(MutationResult { success: true, post, .. }) => { if let Some(post) = post { if let Some(existing) = self.state.view.posts.iter_mut().find(|candidate| candidate.id == post.id) { *existing = post; } } if let Some(id) = draft { self.state.drafts.mark_completed(id); } self.state.status.success("saved"); }, Ok(_) => self.state.status.failure("mutation was not confirmed"), Err(error) => self.state.status.failure(error.to_string()) },
            ApiResult::Comments { result, .. } => match result { Ok(comments) => { if let Some(detail) = &mut self.state.view.detail { detail.comments = comments; } self.state.status.success("comments loaded"); }, Err(error) => self.state.status.failure(error.to_string()) },
        }
    }
}

fn mutation_for_draft(draft: &Draft, selected: Option<crate::PostId>) -> Option<Mutation> {
    match draft.operation.as_str() {
        "create_comment" => Some(Mutation::CreateComment(CreateCommentRequest { post: selected.unwrap_or(crate::PostId(0)), content: draft.content.clone() })),
        _ => None,
    }
}
