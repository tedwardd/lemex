pub mod actions;
pub mod repository;
pub mod state;

use std::{collections::HashMap, sync::Arc};

pub use actions::{ApiResult, AppAction, ProfileCommand, ProfileDraft, RequestIdentity, RequestToken};
pub use repository::{CachedRead, Repository};
pub use state::{AppState, DraftStore, RenderModel, Status, View};
use crate::{
    api::{FeedQuery, LemmyApi, MutationResult},
    cache::Draft,
    domain::{CreateCommentRequest, Mutation, Profile, ProfileContext, ProfileId},
    error::Result,
    input::{Command, Mode},
    profiles::{default_store, CredentialStore, ProfileStore},
};

pub struct App {
    pub state: AppState,
    pub repository: Repository,
    profile_store: ProfileStore,
    requests: HashMap<RequestIdentity, RequestToken>,
    next_generation: u64,
    quit: bool,
}

impl App {
    pub fn new(
        api: Arc<dyn LemmyApi>,
        cache: Arc<dyn crate::cache::CacheStore>,
        active: ProfileContext,
        credentials: Arc<dyn CredentialStore>,
    ) -> Self {
        Self::with_profile_store(api, cache, active, credentials, default_store())
    }

    pub fn with_profile_store(
        api: Arc<dyn LemmyApi>,
        cache: Arc<dyn crate::cache::CacheStore>,
        active: ProfileContext,
        credentials: Arc<dyn CredentialStore>,
        profile_store: ProfileStore,
    ) -> Self {
        let state = AppState::new(active, cache.clone());
        Self { state, repository: Repository::new(api, cache, credentials), profile_store, requests: HashMap::new(), next_generation: 0, quit: false }
    }

    pub fn begin_request(&mut self, identity: RequestIdentity) -> RequestToken {
        self.next_generation = self.next_generation.wrapping_add(1);
        let token = RequestToken { generation: self.next_generation, identity: identity.clone() };
        self.requests.insert(identity, token.clone());
        token
    }

    pub fn render_model(&self) -> RenderModel { self.state.render_model() }
    pub fn is_quit(&self) -> bool { self.quit }

    pub async fn dispatch(&mut self, action: AppAction) -> Result<()> {
        match action {
            AppAction::Input(command) => self.dispatch_command(command).await,
            AppAction::Profile(command) => self.dispatch_profile(command).await,
            AppAction::SubmitDraft(id) => self.submit_draft(id).await,
            AppAction::OpenSelected => self.open_selected().await,
            AppAction::Back => { self.invalidate_content_requests(); self.state.view.detail = None; self.state.mode = Mode::Normal; self.cancel_pending(); Ok(()) }
            AppAction::DeletePost(id) => self.delete_post(id).await,
            AppAction::Confirm => self.confirm_pending().await,
            AppAction::Cancel => { self.cancel_pending(); Ok(()) }
            AppAction::ApiResult(result) => { self.apply_api_result(result); Ok(()) }
            AppAction::Tick => { self.poll_feed_refresh(); Ok(()) }
            AppAction::Quit => { self.quit = true; Ok(()) }
        }
    }

    async fn dispatch_command(&mut self, command: Command) -> Result<()> {
        match command {
            Command::Open => self.open_selected().await,
            Command::Back => { self.invalidate_content_requests(); self.state.view.detail = None; self.state.mode = Mode::Normal; self.cancel_pending(); Ok(()) }
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
                self.requests.clear();
                let id = self.state.active.profile.id.clone();
                if let Err(error) = self.repository.credentials.delete_session(&id).await {
                    self.state.status.failure(error.to_string());
                } else {
                    self.repository.invalidate_profile_context(&id);
                    let mut context = self.state.active.clone();
                    context.session = None;
                    self.state.switch_context(context);
                    self.state.status.success("logged out");
                }
                Ok(())
            }
            ProfileCommand::WhoAmI => { self.state.status.success(self.state.active.session.as_ref().map(|session| format!("user {}", session.user_id.0)).unwrap_or_else(|| "anonymous".into())); Ok(()) }
            ProfileCommand::List => { self.state.status.success(self.state.active.profile.id.to_string()); Ok(()) }
            ProfileCommand::New(draft) => {
                self.requests.clear();
                let profile = Profile { id: draft.id, instance_url: draft.instance_url, account_label: draft.account_label };
                let mut config = match self.profile_store.load_config() {
                    Ok(config) => config,
                    Err(error) => { self.state.status.failure(error.to_string()); return Ok(()); }
                };
                let replacing = self.state.active.profile.id == profile.id || config.profiles.iter().any(|existing| existing.id == profile.id);
                if replacing {
                    self.repository.invalidate_profile_context(&profile.id);
                }
                if replacing {
                    if let Err(error) = self.repository.credentials.delete_session(&profile.id).await {
                        self.state.status.failure(error.to_string());
                        return Ok(());
                    }
                }
                if let Some(existing) = config.profiles.iter_mut().find(|existing| existing.id == profile.id) { *existing = profile.clone(); } else { config.profiles.push(profile.clone()); }
                if let Err(error) = self.profile_store.save_config(&config) {
                    self.state.status.failure(error.to_string());
                    return Ok(());
                }
                self.state.switch_context(ProfileContext { profile, session: None });
                self.state.status.success("profile created");
                Ok(())
            }
            ProfileCommand::Login | ProfileCommand::Delete(_) => { self.state.status.failure("profile operation requires interactive profile service"); Ok(()) }
        }
    }


    async fn switch_profile(&mut self, id: ProfileId) -> Result<()> {
        self.requests.clear();
        let profile = match self.profile_store.load()?.into_iter().find(|profile| profile.id == id) {
            Some(profile) => profile,
            None => { self.state.status.failure(format!("profile {id} is not configured")); return Ok(()); }
        };
        let session = match self.repository.session(&id).await { Ok(session) => session, Err(error) => { self.state.status.failure(error.to_string()); return Ok(()); } };
        self.requests.clear();
        self.state.switch_context(ProfileContext { profile, session });
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
        let profile = context.profile.id.clone();
        let request = self.begin_request(RequestIdentity::Feed);
        match self.repository.feed_with_generation(&context, FeedQuery::home(), request.generation).await {
            Ok(read) => self.apply_api_result(ApiResult::Feed { profile, request, result: Ok(read.value), stale: read.stale }),
            Err(error) => self.apply_api_result(ApiResult::Feed { profile, request, result: Err(error), stale: false }),
        }
        Ok(())
    }

    async fn open_selected(&mut self) -> Result<()> {
        let Some(id) = self.state.selected_post() else { return Ok(()); };
        let profile = self.state.active.profile.id.clone();
        let request = self.begin_request(RequestIdentity::Post(id));
        let result = self.repository.post(&self.state.active, id).await;
        self.apply_api_result(ApiResult::Post { profile, request, result });
        Ok(())
    }

    async fn delete_post(&mut self, id: crate::PostId) -> Result<()> {
        self.state.pending = Some(crate::app::actions::PendingAction::DeletePost { profile: self.state.active.profile.id.clone(), id });
        self.state.status.pending = true;
self.state.status.message = format!("confirm deletion of post {:?}", id);
        self.state.status.error = None;
        Ok(())
    }

    async fn confirm_pending(&mut self) -> Result<()> {
        let Some(crate::app::actions::PendingAction::DeletePost { profile, id }) = self.state.pending.take() else { return Ok(()); };
        if profile != self.state.active.profile.id { self.cancel_pending(); return Ok(()); }
        self.state.status.pending = true;
        let mutation = Mutation::DeletePost(id);
        let request = self.begin_request(RequestIdentity::Mutation(mutation.clone()));
        let result = self.repository.mutate(&self.state.active, mutation.clone()).await;
        self.apply_api_result(ApiResult::Mutation { profile, request, draft: None, mutation, result });
        Ok(())
    }

    fn cancel_pending(&mut self) {
        self.state.pending = None;
        if self.state.status.pending { self.state.status.pending = false; self.state.status.success("cancelled"); }
    }

    fn invalidate_content_requests(&mut self) {
        self.requests.retain(|identity, _| !matches!(identity, RequestIdentity::Post(_) | RequestIdentity::Comments(_)));
    }

    fn poll_feed_refresh(&mut self) {
        let Some(request) = self.requests.get(&RequestIdentity::Feed).cloned() else { return; };
        let context = self.state.active.clone();
        if let Ok(Some((generation, read))) = self.repository.take_completed_feed(&context, &FeedQuery::home()) {
            if generation == request.generation {
                self.apply_api_result(ApiResult::Feed { profile: context.profile.id, request, result: Ok(read.value), stale: read.stale });
            }
        }
    }

    async fn submit_draft(&mut self, id: crate::cache::DraftId) -> Result<()> {
        let Some(draft) = self.state.draft(id.clone()) else { return Ok(()); };
        let selected = self.state.selected_post();
        if matches!(draft.operation.as_str(), "create_comment" | "reply") && selected.is_none() {
            self.state.status.failure("select a post before submitting a comment");
            return Ok(());
        }
        let Some(mutation) = mutation_for_draft(&draft, selected) else { self.state.status.failure("unsupported draft operation"); return Ok(()); };
        let profile = self.state.active.profile.id.clone();
        let request = self.begin_request(RequestIdentity::Mutation(mutation.clone()));
        let result = self.repository.mutate(&self.state.active, mutation.clone()).await;
        self.apply_api_result(ApiResult::Mutation { profile, request, draft: Some(id), mutation, result });
        Ok(())
    }


    fn move_selection(&mut self, delta: isize) {
        if self.state.view.posts.is_empty() { return; }
        let current = self.state.view.selected.unwrap_or(0) as isize;
        let max = self.state.view.posts.len().saturating_sub(1) as isize;
        self.state.view.selected = Some((current + delta).clamp(0, max) as usize);
    }

    fn apply_api_result(&mut self, result: ApiResult) {
        let (profile, request) = match &result {
            ApiResult::Feed { profile, request, .. } | ApiResult::Post { profile, request, .. } | ApiResult::Mutation { profile, request, .. } | ApiResult::Comments { profile, request, .. } => (profile, request),
        };
        if profile != &self.state.active.profile.id || self.requests.get(&request.identity) != Some(request) { return; }
        match result {
            ApiResult::Feed { result, stale, .. } => match result {
                Ok(page) => { self.state.view.posts = page.items; self.state.view.stale = stale; self.state.status.stale = stale; self.state.status.success("feed loaded"); }
                Err(error) => self.state.status.failure(error.to_string()),
            },
            ApiResult::Post { request, result, .. } => match result {
                Ok(detail) if matches!(request.identity, RequestIdentity::Post(id) if id == detail.post.id) && self.state.selected_post() == Some(detail.post.id) => { self.state.view.detail = Some(detail); self.state.mode = Mode::Normal; self.state.status.success("post loaded"); }
                Ok(_) => {}
                Err(error) if matches!(request.identity, RequestIdentity::Post(id) if self.state.selected_post() == Some(id)) => self.state.status.failure(error.to_string()),
                Err(_) => {},
            },
            ApiResult::Mutation { request, mutation, result, draft, .. } => if request.identity == RequestIdentity::Mutation(mutation.clone()) { match result {
                Ok(MutationResult { success: true, post, .. }) => {
                    if let Mutation::DeletePost(id) = mutation {
                        self.state.view.posts.retain(|candidate| candidate.id != id);
                        if self.state.view.detail.as_ref().is_some_and(|detail| detail.post.id == id) { self.state.view.detail = None; self.state.mode = Mode::Normal; }
                        self.state.view.selected = self.state.view.selected.and_then(|selected| if self.state.view.posts.is_empty() { None } else { Some(selected.min(self.state.view.posts.len() - 1)) });
                    } else if let Some(post) = post {
                        if let Some(existing) = self.state.view.posts.iter_mut().find(|candidate| candidate.id == post.id) { *existing = post; }
                    }
                    if let Some(id) = draft { self.state.drafts.mark_completed(id); }
                    self.state.status.success("saved");
                }
                Ok(_) => self.state.status.failure("mutation was not confirmed"),
                Err(error) => self.state.status.failure(error.to_string()),
            } } else {},
            ApiResult::Comments { request, post, result, .. } => if request.identity == RequestIdentity::Comments(post) && self.state.view.detail.as_ref().is_some_and(|detail| detail.post.id == post) { match result {
                Ok(comments) => { if let Some(detail) = &mut self.state.view.detail { detail.comments = comments; } self.state.status.success("comments loaded"); }
                Err(error) => self.state.status.failure(error.to_string()),
            } } else {},
        }
    }
}
fn mutation_for_draft(draft: &Draft, selected: Option<crate::PostId>) -> Option<Mutation> {
    match draft.operation.as_str() {
        "create_comment" | "reply" => Some(Mutation::CreateComment(CreateCommentRequest { post: selected?, content: draft.content.clone() })),
        _ => None,
    }
}
