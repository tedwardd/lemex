pub mod actions;
pub mod help;
pub mod render;
pub mod repository;
pub mod state;

use std::{collections::{HashMap, VecDeque}, ffi::OsStr, io::Write, path::{Path, PathBuf}, process::Stdio, sync::Arc, thread, time::Duration};

use crossterm::event::{self, Event};
use ratatui::DefaultTerminal;
use tokio::sync::mpsc;


pub use actions::{ApiResult, AppAction, DownloadsAction, ProfileCommand, ProfileDraft, RequestIdentity, RequestToken};
pub use repository::{CachedRead, Repository};
pub use state::{AppState, DraftStore, DownloadsPanel, DownloadsRender, RenderModel, Status, View};
use crate::{
    api::{FeedQuery, LemmyApi, MutationResult},
    cache::Draft,
    config::MediaConfig,
    domain::{CreateCommentRequest, CreatePostRequest, DownloadStatus, EditCommentRequest, EditPostRequest, MediaRef, Mutation, Profile, ProfileContext, ProfileId},
    error::{AppError, Result},
    input::{Command, Mode},
    media::{kitty, build_argv, filename_for, CollisionPolicy, DownloadEvent, DownloadManager, DownloadRequest, MediaHandler, MediaPolicyConfig, TerminalCapabilities},
    profiles::{default_store, CredentialStore, ProfileStore},
};

pub fn run_terminal(app: App, terminal: DefaultTerminal) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| crate::error::AppError::Terminal(format!("could not start Tokio runtime: {error}")))?;
    runtime.block_on(run_terminal_async(app, terminal))
}

async fn run_terminal_async(app: App, mut terminal: DefaultTerminal) -> Result<()> {
    let (input_tx, mut input_rx) = mpsc::unbounded_channel::<Result<Event>>();
    let (stop_tx, stop_rx) = std::sync::mpsc::channel();
    let input_thread = thread::spawn(move || {
        loop {
            if stop_rx.try_recv().is_ok() { break; }
            match event::poll(Duration::from_millis(100)) {
                Ok(true) => match event::read() {
                    Ok(event) => {
                        if input_tx.send(Ok(event)).is_err() { break; }
                    }
                    Err(error) => {
                        let _ = input_tx.send(Err(crate::error::AppError::Terminal(error.to_string())));
                        break;
                    }
                },
                Ok(false) => {}
                Err(error) => {
                    let _ = input_tx.send(Err(crate::error::AppError::Terminal(error.to_string())));
                    break;
                }
            }
        }
    });

    let result = async {
        let mut input = crate::input::InputEngine::new();
        let mut ticks = tokio::time::interval(Duration::from_millis(100));
        let mut app = Some(app);
        let mut model = app.as_ref().expect("application is present").render_model();
        let mut action_task: Option<tokio::task::JoinHandle<(App, Result<()>)>> = None;
        let mut queued_actions = VecDeque::new();
        let mut redraw = true;
        let mut quit = false;

        while !quit {
            if redraw {
                terminal
                    .draw(|frame| render::render(frame, &model))
                    .map_err(|error| crate::error::AppError::Terminal(error.to_string()))?;
                redraw = false;
            }

            if action_task.is_none() {
                if let Some(action) = queued_actions.pop_front() {
                    start_action(&mut app, &mut model, &mut action_task, action, &mut redraw);
                    continue;
                }
            }

            if let Some(task) = action_task.as_mut() {
                tokio::select! {
                    completed = task => {
                        action_task = None;
                        let (finished, result) = completed.map_err(|error| crate::error::AppError::Terminal(format!("application task failed: {error}")))?;
                        app = Some(finished);
                        result?;
                        model = app.as_ref().expect("application is present").render_model();
                        redraw = true;
                    }
                    _ = ticks.tick() => redraw = true,
                    event = input_rx.recv() => match event {
                        Some(Ok(event)) => {
                            if queue_terminal_event(event, &mut input, &mut queued_actions, &mut redraw) {
                                if let Some(task) = action_task.take() { task.abort(); }
                                quit = true;
                            }
                        }
                        Some(Err(error)) => return Err(error),
                        None => quit = true,
                    }
                }
                continue;
            }

            if app.as_ref().is_some_and(App::is_quit) { break; }
            tokio::select! {
                _ = ticks.tick() => {
                    start_action(&mut app, &mut model, &mut action_task, AppAction::Tick, &mut redraw);
                }
                event = input_rx.recv() => match event {
                    Some(Ok(event)) => {
                        if queue_terminal_event(event, &mut input, &mut queued_actions, &mut redraw) {
                            quit = true;
                        }
                    }
                    Some(Err(error)) => return Err(error),
                    None => quit = true,
                }
            }
        }
        Ok::<(), crate::error::AppError>(())
    }
    .await;

    let _ = stop_tx.send(());
    let _ = input_thread.join();
    result
}

fn queue_terminal_event(
    event: Event,
    input: &mut crate::input::InputEngine,
    queued_actions: &mut VecDeque<AppAction>,
    redraw: &mut bool,
) -> bool {
    match event {
        Event::Key(key) => {
            let command = input.handle(key);
            if command == crate::input::Command::Quit { return true; }
            if command != crate::input::Command::Noop {
                queued_actions.push_back(command.into());
            }
        }
        Event::Resize(_, _) => *redraw = true,
        _ => {}
    }
    false
}

fn start_action(
    app: &mut Option<App>,
    model: &mut RenderModel,
    action_task: &mut Option<tokio::task::JoinHandle<(App, Result<()>)>>,
    action: AppAction,
    redraw: &mut bool,
) {
    let mut owned = app.take().expect("application is present");
    owned.prepare_action(&action);
    *model = owned.render_model();
    *redraw = true;
    *action_task = Some(tokio::spawn(async move {
        let mut app = owned;
        let result = app.dispatch(action).await;
        (app, result)
    }));
}

pub struct App {
    pub state: AppState,
    pub repository: Repository,
    profile_store: ProfileStore,
    requests: HashMap<RequestIdentity, RequestToken>,
    next_generation: u64,
    downloads: DownloadManager,
    media_policy: MediaPolicyConfig,
    terminal_capabilities: TerminalCapabilities,
    collision_policy: CollisionPolicy,
    quit: bool,
}

impl App {
    pub fn new(
        api: Arc<dyn LemmyApi>,
        cache: Arc<dyn crate::cache::CacheStore>,
        active: ProfileContext,
        credentials: Arc<dyn CredentialStore>,
    ) -> Self {
        Self::with_media(api, cache, active, credentials, MediaConfig::default())
    }

    pub fn with_profile_store(
        api: Arc<dyn LemmyApi>,
        cache: Arc<dyn crate::cache::CacheStore>,
        active: ProfileContext,
        credentials: Arc<dyn CredentialStore>,
        profile_store: ProfileStore,
    ) -> Self {
        Self::with_media_and_profile_store(api, cache, active, credentials, MediaConfig::default(), profile_store)
    }

    pub fn with_media(
        api: Arc<dyn LemmyApi>,
        cache: Arc<dyn crate::cache::CacheStore>,
        active: ProfileContext,
        credentials: Arc<dyn CredentialStore>,
        media: MediaConfig,
    ) -> Self {
        Self::with_media_and_profile_store(api, cache, active, credentials, media, default_store())
    }

    fn with_media_and_profile_store(
        api: Arc<dyn LemmyApi>,
        cache: Arc<dyn crate::cache::CacheStore>,
        active: ProfileContext,
        credentials: Arc<dyn CredentialStore>,
        media: MediaConfig,
        profile_store: ProfileStore,
    ) -> Self {
        let state = AppState::new(active, cache.clone());
        let downloads_directory = media.download_directory.clone().unwrap_or_else(|| crate::config::cache_dir().join("downloads"));
        let collision_policy = CollisionPolicy::from_config(&media.collision_policy);
        let media_policy = MediaPolicyConfig::from_config(&media);
        let terminal_capabilities = TerminalCapabilities { kitty: kitty::detect_support() };
        Self {
            state,
            repository: Repository::new(api, cache, credentials),
            profile_store,
            requests: HashMap::new(),
            next_generation: 0,
            downloads: DownloadManager::new(downloads_directory),
            media_policy,
            terminal_capabilities,
            collision_policy,
            quit: false,
        }
    }

    pub fn begin_request(&mut self, identity: RequestIdentity) -> RequestToken {
        self.next_generation = self.next_generation.wrapping_add(1);
        let token = RequestToken { generation: self.next_generation, identity: identity.clone() };
        self.requests.insert(identity, token.clone());
        token
    }

    pub fn render_model(&self) -> RenderModel {
        let mut model = self.state.render_model();
        model.downloads = self.state.view.downloads.as_ref().map(|panel| DownloadsRender {
            query: panel.query.clone(),
            selected: panel.selected,
            records: self.downloads.history().filtered(&panel.query),
        });
        model
    }
    pub fn is_quit(&self) -> bool { self.quit }

    fn prepare_action(&mut self, action: &AppAction) {
        let is_confirm = matches!(action, AppAction::Confirm) && self.state.pending.is_some();
        let is_network = matches!(action, AppAction::Input(Command::Refresh) | AppAction::OpenCommunity(_) | AppAction::LoadMore | AppAction::Mutate(_) | AppAction::Media | AppAction::DownloadMedia)
            || is_confirm
            || (matches!(action, AppAction::OpenSelected) && self.state.selected_post().is_some());
        if !is_network { return; }
        if !is_confirm {
            self.state.pending = None;
            self.state.status.confirmation_pending = false;
            self.state.status.message.clear();
            self.state.status.error = None;
        }
        self.state.status.pending = true;
    }

    pub async fn dispatch(&mut self, action: AppAction) -> Result<()> {
        match action {
            AppAction::Input(command) => self.dispatch_command(command).await,
            AppAction::Profile(command) => self.dispatch_profile(command).await,
            AppAction::SubmitDraft(id) => self.submit_draft(id).await,
            AppAction::DiscardDraft(id) => { self.state.drafts.mark_completed(id); self.state.status.success("draft discarded"); Ok(()) }
            AppAction::OpenSelected => self.open_selected().await,
            AppAction::OpenCommunity(id) => self.open_community(id).await,
            AppAction::LoadMore => self.load_more().await,
            AppAction::Back => {
                if self.state.view.downloads_active() {
                    self.state.view.close_downloads_panel();
                    return Ok(());
                }
                self.invalidate_content_requests(); self.state.view.detail = None; self.state.mode = Mode::Normal; self.cancel_pending(); Ok(())
            }
            AppAction::DeletePost(id) => self.delete_post(id).await,
            AppAction::Mutate(mutation) => self.start_mutation(mutation, None).await,
            AppAction::Confirm => self.confirm_pending().await,
            AppAction::Cancel => { self.cancel_pending(); Ok(()) }
            AppAction::ApiResult(result) => { self.apply_api_result(result); Ok(()) }
            AppAction::Media => self.open_media_selected().await,
            AppAction::DownloadMedia => self.download_media_selected().await,
            AppAction::ShowDownloads => { self.toggle_downloads_panel(); Ok(()) }
            AppAction::Downloads(action) => self.downloads_action(action).await,
            AppAction::Tick => { self.poll_feed_refresh(); self.poll_downloads(); Ok(()) }
            AppAction::Quit => { self.downloads.shutdown(); self.quit = true; Ok(()) }
        }
    }

    async fn dispatch_command(&mut self, command: Command) -> Result<()> {
        match command {
            Command::Open => {
                if self.state.view.downloads_active() {
                    return self.downloads_action(DownloadsAction::Reopen).await;
                }
                self.open_selected().await
            }
            Command::Back => {
                if self.state.view.downloads_active() {
                    self.state.view.close_downloads_panel();
                    return Ok(());
                }
                self.invalidate_content_requests(); self.state.view.detail = None; self.state.mode = Mode::Normal; self.cancel_pending(); Ok(())
            }
            Command::Quit => { self.quit = true; Ok(()) }
            Command::Refresh => {
                if self.state.view.downloads_active() {
                    return self.downloads_action(DownloadsAction::Retry).await;
                }
                self.refresh_feed().await
            }
            Command::MoveDown { count } => { self.move_selection(count as isize); Ok(()) }
            Command::MoveUp { count } => { self.move_selection(-(count as isize)); Ok(()) }
            Command::EnterInsert => { self.state.mode = Mode::Insert; Ok(()) }
            Command::EnterVisual => { self.state.mode = Mode::Visual; Ok(()) }
            Command::EnterCommand => { self.state.mode = Mode::Command; self.state.view.compose.clear(); Ok(()) }
            Command::EnterSearch { backward } => { self.state.mode = if backward { Mode::SearchBackward } else { Mode::SearchForward }; self.state.view.compose.clear(); Ok(()) }
            Command::Text(text) => { self.state.view.compose.push_str(&text); Ok(()) }
            Command::SubmitLine(line) => self.submit_line(line).await,
            Command::MoveLeft { .. } | Command::MoveRight { .. } | Command::Noop => Ok(()),
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
        let query = self.state.view.feed_query.clone();
        let request = self.begin_request(RequestIdentity::Feed);
        match self.repository.feed_with_generation(&context, query, request.generation).await {
            Ok(read) => self.apply_api_result(ApiResult::Feed { profile, request, result: Ok(read.value), stale: read.stale }),
            Err(error) => self.apply_api_result(ApiResult::Feed { profile, request, result: Err(error), stale: false }),
        }
        Ok(())
    }

    async fn submit_line(&mut self, line: String) -> Result<()> {
        if matches!(self.state.mode, Mode::SearchForward | Mode::SearchBackward) {
            let search = line.trim().to_owned();
            self.state.view.search = search.clone();
            self.state.view.feed_query = FeedQuery { search: (!search.is_empty()).then_some(search), ..FeedQuery::home() };
            self.state.mode = Mode::Normal;
            return self.refresh_feed().await;
        }
        self.state.mode = Mode::Normal;
        let trimmed = line.trim();
        if trimmed.is_empty() { return Ok(()); }
        match trimmed {
            "media" => self.open_media_selected().await,
            "download-media" | "download_media" => self.download_media_selected().await,
            "downloads" => {
                self.toggle_downloads_panel();
                Ok(())
            }
            _ if self.state.view.downloads_active() => self.submit_downloads_command(trimmed).await,
            other => {
                self.state.status.failure(format!("unknown command: {other}"));
                Ok(())
            }
        }
    }

    async fn submit_downloads_command(&mut self, line: &str) -> Result<()> {
        if let Some(query) = line.strip_prefix("search ") {
            return self.downloads_action(DownloadsAction::Search(query.trim().to_owned())).await;
        }
        let action = match line {
            "reopen" => DownloadsAction::Reopen,
            "reveal" => DownloadsAction::Reveal,
            "copy" => DownloadsAction::CopyPath,
            "retry" => DownloadsAction::Retry,
            "cancel" => DownloadsAction::Cancel,
            "delete" => DownloadsAction::Delete,
            "overwrite" => DownloadsAction::ResolveCollision { overwrite: true },
            "keep" => DownloadsAction::ResolveCollision { overwrite: false },
            "close" => DownloadsAction::Close,
            other => {
                self.state.status.failure(format!("unknown download command: {other}"));
                return Ok(());
            }
        };
        self.downloads_action(action).await
    }

    async fn open_community(&mut self, community: crate::domain::CommunityId) -> Result<()> {
        self.state.view.feed_query = FeedQuery { community: Some(community), ..FeedQuery::home() };
        self.state.view.search.clear();
        self.state.view.next_page = None;
        self.refresh_feed().await
    }

    async fn load_more(&mut self) -> Result<()> {
        let Some(page) = self.state.view.next_page else {
            self.state.status.success("no more posts to load");
            return Ok(());
        };
        let mut query = self.state.view.feed_query.clone();
        query.page = Some(page);
        let result = self.repository.api.feed(&self.state.active, query).await;
        match result {
            Ok(next) => {
                let known = self.state.view.posts.iter().map(|post| post.id).collect::<std::collections::HashSet<_>>();
                self.state.view.posts.extend(next.items.into_iter().filter(|post| !known.contains(&post.id)));
                self.state.view.next_page = next.next_page;
                self.state.status.success("more posts loaded");
            }
            Err(error) => self.state.status.failure(error.to_string()),
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

    fn selected_media(&self) -> Option<MediaRef> {
        self.state
            .selected_post()
            .and_then(|id| self.state.view.posts.iter().find(|post| post.id == id))
            .and_then(|post| post.url.clone())
            .map(MediaRef::new)
    }

    async fn open_media_selected(&mut self) -> Result<()> {
        let Some(media) = self.selected_media() else {
            self.state.status.failure("selected post has no media URL");
            return Ok(());
        };
        self.open_media(media, None).await
    }

    async fn download_media_selected(&mut self) -> Result<()> {
        let Some(media) = self.selected_media() else {
            self.state.status.failure("selected post has no media URL");
            return Ok(());
        };
        let directory = self.downloads.directory().to_path_buf();
        let destination = directory.join(filename_for(&media));
        let request = DownloadRequest {
            media,
            profile: self.state.active.profile.id.clone(),
            instance_url: self.state.active.profile.instance_url.clone(),
            destination,
            collision: self.collision_policy,
        };
        match self.downloads.start(request).await {
            Ok(id) => {
                let prompting = self.downloads.history().get(id).is_some_and(|record| record.status == DownloadStatus::Prompting);
                if prompting {
                    self.state.view.open_downloads_panel();
                    if let Some(panel) = &mut self.state.view.downloads { panel.selected = Some(id); }
                    self.state.status.message = "file already exists; choose :overwrite or :keep in the downloads panel".into();
                    self.state.status.pending = false;
                    self.state.status.error = None;
                } else {
                    self.state.status.success(format!("download #{} started", id.0));
                }
                Ok(())
            }
            Err(error) => {
                self.state.status.failure(error.to_string());
                Ok(())
            }
        }
    }

    /// Open media through the selected handler. `local` is the downloaded file
    /// when reopening a record; otherwise the source URL is passed.
    async fn open_media(&mut self, media: MediaRef, local: Option<PathBuf>) -> Result<()> {
        let handler = self.media_policy.select(&media, &self.terminal_capabilities);
        match handler {
            MediaHandler::KittyInline => self.render_kitty(media, local).await,
            MediaHandler::Mailcap { command } | MediaHandler::External { command } => {
                if !media.url.username().is_empty() || media.url.password().is_some() {
                    self.state.status.failure("refusing to open a media URL containing credentials");
                    return Ok(());
                }
                let mime = crate::media::resolve_mime(&media, None).unwrap_or_default();
                let source = match local {
                    Some(path) => path.into_os_string(),
                    None => OsStr::new(media.url.as_str()).to_os_string(),
                };
                match spawn_detached(&command, &source, &mime) {
                    Ok(()) => self.state.status.success("opened media with external handler"),
                    Err(error) => self.state.status.failure(error.to_string()),
                }
                Ok(())
            }
            MediaHandler::MetadataOnly => {
                let mime = crate::media::resolve_mime(&media, None).unwrap_or_else(|| "unknown".into());
                self.state.status.success(format!("no media handler for {mime}; metadata only"));
                Ok(())
            }
        }
    }

    async fn render_kitty(&mut self, media: MediaRef, local: Option<PathBuf>) -> Result<()> {
        let path = match local {
            Some(path) if path.exists() => path,
            _ => {
                let scratch = std::env::temp_dir().join(filename_for(&media));
                let request = DownloadRequest {
                    media: media.clone(),
                    profile: self.state.active.profile.id.clone(),
                    instance_url: self.state.active.profile.instance_url.clone(),
                    destination: scratch,
                    collision: CollisionPolicy::UniqueName,
                };
                match self.downloads.start(request).await {
                    Ok(id) => {
                        let outcome = tokio::time::timeout(Duration::from_secs(30), self.downloads.wait_for(id)).await;
                        match outcome {
                            Ok(DownloadStatus::Completed) => match self.downloads.history().get(id) {
                                Some(record) => record.local_path,
                                None => {
                                    self.state.status.failure("kitty render download vanished");
                                    return Ok(());
                                }
                            },
                            Ok(status) => {
                                self.state.status.failure(format!("kitty render download did not complete ({status})"));
                                return Ok(());
                            }
                            Err(_) => {
                                self.state.status.failure("kitty render download timed out");
                                return Ok(());
                            }
                        }
                    }
                    Err(error) => {
                        self.state.status.failure(error.to_string());
                        return Ok(());
                    }
                }
            }
        };
        match kitty::render_file(&path) {
            Ok(bytes) => {
                let mut stdout = std::io::stdout();
                if stdout.write_all(&bytes).is_ok() {
                    let _ = stdout.flush();
                    self.state.status.success("rendered media via kitty graphics protocol");
                } else {
                    self.state.status.failure("could not write kitty escape sequence to terminal");
                }
            }
            Err(error) => self.state.status.failure(error.to_string()),
        }
        Ok(())
    }

    fn toggle_downloads_panel(&mut self) {
        if self.state.view.downloads_active() {
            self.state.view.close_downloads_panel();
            return;
        }
        self.state.view.open_downloads_panel();
        let query = self.state.view.downloads.as_ref().map(|panel| panel.query.clone()).unwrap_or_default();
        let first = self.downloads.history().filtered(&query).first().map(|record| record.id);
        if let Some(panel) = &mut self.state.view.downloads {
            panel.selected = first;
        }
    }

    async fn downloads_action(&mut self, action: DownloadsAction) -> Result<()> {
        match action {
            DownloadsAction::Search(query) => {
                let mut panel = self.state.view.downloads.clone().unwrap_or_default();
                panel.query = query;
                panel.selected = self.downloads.history().filtered(&panel.query).first().map(|record| record.id);
                self.state.view.downloads = Some(panel);
                Ok(())
            }
            DownloadsAction::Close => {
                self.state.view.close_downloads_panel();
                Ok(())
            }
            DownloadsAction::Reopen
            | DownloadsAction::Reveal
            | DownloadsAction::CopyPath
            | DownloadsAction::Retry
            | DownloadsAction::Cancel
            | DownloadsAction::Delete
            | DownloadsAction::ResolveCollision { .. } => {
                let Some(id) = self.state.view.selected_download() else {
                    self.state.status.failure("no download selected");
                    return Ok(());
                };
                match action {
                    DownloadsAction::Reopen => {
                        let Some(record) = self.downloads.history().get(id) else {
                            self.state.status.failure("download not found");
                            return Ok(());
                        };
                        if record.local_file_deleted || !record.local_path.exists() {
                            self.state.status.failure("local file is missing; retry the download");
                            return Ok(());
                        }
                        self.open_media(record.media.clone(), Some(record.local_path.clone())).await
                    }
                    DownloadsAction::Reveal => {
                        let Some(record) = self.downloads.history().get(id) else {
                            self.state.status.failure("download not found");
                            return Ok(());
                        };
                        let directory = record.local_path.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
                        reveal_directory(&directory);
                        self.state.status.success(format!("revealed {}", directory.display()));
                        Ok(())
                    }
                    DownloadsAction::CopyPath => {
                        let Some(record) = self.downloads.history().get(id) else {
                            self.state.status.failure("download not found");
                            return Ok(());
                        };
                        if copy_to_clipboard(&record.local_path.to_string_lossy()) {
                            self.state.status.success("download path copied to clipboard");
                        } else {
                            self.state.status.success(format!("path: {}", record.local_path.display()));
                        }
                        Ok(())
                    }
                    DownloadsAction::Retry => match self.downloads.retry(id).await {
                        Ok(()) => {
                            self.state.status.success(format!("retrying download #{id}"));
                            Ok(())
                        }
                        Err(error) => {
                            self.state.status.failure(error.to_string());
                            Ok(())
                        }
                    },
                    DownloadsAction::Cancel => match self.downloads.cancel(id).await {
                        Ok(()) => {
                            self.state.status.success(format!("download #{id} cancelled"));
                            Ok(())
                        }
                        Err(error) => {
                            self.state.status.failure(error.to_string());
                            Ok(())
                        }
                    },
                    DownloadsAction::Delete => {
                        let Some(record) = self.downloads.history().get(id) else {
                            self.state.status.failure("download not found");
                            return Ok(());
                        };
                        if record.local_file_deleted {
                            self.state.status.failure("local file was already deleted");
                            return Ok(());
                        }
                        self.state.pending = Some(crate::app::actions::PendingAction::DeleteDownload { id, path: record.local_path.clone() });
                        self.state.status.pending = false;
                        self.state.status.confirmation_pending = true;
                        self.state.status.message = format!("confirm deletion of {}", record.local_path.display());
                        self.state.status.error = None;
                        Ok(())
                    }
                    DownloadsAction::ResolveCollision { overwrite } => match self.downloads.resolve_collision(id, overwrite).await {
                        Ok(()) => {
                            self.state.status.success(if overwrite { "overwriting existing file" } else { "kept existing file" });
                            Ok(())
                        }
                        Err(error) => {
                            self.state.status.failure(error.to_string());
                            Ok(())
                        }
                    },
                    _ => unreachable!("other download actions handled above"),
                }
            }
        }
    }

    fn poll_downloads(&mut self) {
        for event in self.downloads.take_events() {
            match event {
                DownloadEvent::Completed(id) => {
                    let message = self
                        .downloads
                        .history()
                        .get(id)
                        .map(|record| format!("download #{} complete: {}", id.0, record.local_path.display()))
                        .unwrap_or_else(|| format!("download #{} complete", id.0));
                    self.state.status.success(message);
                }
                DownloadEvent::Failed(id, error) => {
                    self.state.status.failure(format!("download #{} failed: {error}", id.0));
                }
            }
        }
    }

    async fn delete_post(&mut self, id: crate::PostId) -> Result<()> {
        self.state.pending = Some(crate::app::actions::PendingAction::DeletePost { profile: self.state.active.profile.id.clone(), id });
        self.state.status.pending = false;
        self.state.status.confirmation_pending = true;
        self.state.status.message = format!("confirm deletion of post {} on {}", id.0, self.state.status.instance_url);
        self.state.status.error = None;
        Ok(())
    }

    async fn start_mutation(&mut self, mutation: Mutation, draft: Option<crate::cache::DraftId>) -> Result<()> {
        if matches!(mutation, Mutation::CreatePost(_) | Mutation::DeletePost(_) | Mutation::DeleteComment(_)) {
            self.state.pending = Some(crate::app::actions::PendingAction::Mutation { profile: self.state.active.profile.id.clone(), mutation, draft });
            self.state.status.pending = false;
            self.state.status.confirmation_pending = true;
            self.state.status.message = format!("confirm destructive action on {}", self.state.status.instance_url);
            return Ok(())
        }
        let profile = self.state.active.profile.id.clone();
        let request = self.begin_request(RequestIdentity::Mutation(mutation.clone()));
        let result = self.repository.mutate(&self.state.active, mutation.clone()).await;
        self.apply_api_result(ApiResult::Mutation { profile, request, draft, mutation, result });
        Ok(())
    }

    async fn confirm_pending(&mut self) -> Result<()> {
        let pending = self.state.pending.take();
        match pending {
            Some(crate::app::actions::PendingAction::DeleteDownload { id, path }) => {
                self.state.status.confirmation_pending = false;
                if std::fs::remove_file(&path).is_ok() {
                    self.downloads.history().mark_file_deleted(id);
                    self.state.status.success("local file deleted");
                } else {
                    self.state.status.failure(format!("could not delete {}", path.display()));
                }
                Ok(())
            }
            Some(crate::app::actions::PendingAction::DeletePost { profile, id }) => self.confirm_mutation(profile, Mutation::DeletePost(id), None).await,
            Some(crate::app::actions::PendingAction::Mutation { profile, mutation, draft }) => self.confirm_mutation(profile, mutation, draft).await,
            None => Ok(()),
        }
    }

    async fn confirm_mutation(&mut self, profile: ProfileId, mutation: Mutation, draft: Option<crate::cache::DraftId>) -> Result<()> {
        if profile != self.state.active.profile.id { self.cancel_pending(); return Ok(()); }
        self.state.status.confirmation_pending = false;
        self.state.status.pending = true;
        let request = self.begin_request(RequestIdentity::Mutation(mutation.clone()));
        let result = self.repository.mutate(&self.state.active, mutation.clone()).await;
        self.apply_api_result(ApiResult::Mutation { profile, request, draft, mutation, result });
        Ok(())
    }

    fn invalidate_content_requests(&mut self) {
        self.requests.retain(|identity, _| !matches!(identity, RequestIdentity::Post(_) | RequestIdentity::Comments(_)));
    }
    fn cancel_pending(&mut self) {
        let had_confirmation = self.state.pending.is_some() || self.state.status.confirmation_pending;
        self.state.pending = None;
        self.state.status.confirmation_pending = false;
        if self.state.status.pending || had_confirmation { self.state.status.success("cancelled"); }
    }
    fn poll_feed_refresh(&mut self) {
        let Some(request) = self.requests.get(&RequestIdentity::Feed).cloned() else { return; };
        let context = self.state.active.clone();
        let query = self.state.view.feed_query.clone();
        if let Ok(Some((generation, result))) = self.repository.take_completed_feed(&context, &query) {
            if generation == request.generation {
                self.apply_api_result(ApiResult::Feed { profile: context.profile.id, request, result: result.map(|read| read.value), stale: false });
            }
        }
    }

    async fn submit_draft(&mut self, id: crate::cache::DraftId) -> Result<()> {
        let Some(draft) = self.state.draft(id.clone()) else { return Ok(()); };
        let selected_post = self.state.view.selected.and_then(|index| self.state.view.posts.get(index));
        let selected = selected_post.map(|post| post.id);
        let community = selected_post.map(|post| post.community_id);
        if matches!(draft.operation.as_str(), "create_comment" | "reply") && selected.is_none() {
            self.state.status.failure("select a post before submitting a comment");
            return Ok(());
        }
        if draft.operation.as_str() == "create_post" && community.is_none() {
            self.state.status.failure("select a post in the target community before creating a post");
            return Ok(());
        }
        if let Err(error) = self.state.drafts.validate(&draft) { self.state.status.failure(error.to_string()); return Ok(()); }
        let Some(mutation) = mutation_for_draft(&draft, selected, community) else { self.state.status.failure("unsupported draft operation"); return Ok(()); };
        self.start_mutation(mutation, Some(id)).await
    }



    fn move_selection(&mut self, delta: isize) {
        if self.state.view.downloads_active() {
            let query = self.state.view.downloads.as_ref().map(|panel| panel.query.clone()).unwrap_or_default();
            let ids = self.downloads.history().filtered(&query).into_iter().map(|record| record.id).collect::<Vec<_>>();
            self.state.view.move_download_selection(delta, &ids);
            return;
        }
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
                Ok(page) => {
                    let selected_id = self.state.selected_post();
                    let selected_index = self.state.view.selected.unwrap_or_default();
                    self.state.view.posts = page.items;
                    self.state.view.next_page = page.next_page;
                    self.state.view.selected = selected_id.and_then(|id| self.state.view.posts.iter().position(|post| post.id == id)).or_else(|| (!self.state.view.posts.is_empty()).then_some(selected_index.min(self.state.view.posts.len().saturating_sub(1))));
                    self.state.view.stale = stale;
                    self.state.status.stale = stale;
                    if stale {
                        self.state.status.message = "stale feed loaded; refreshing".into();
                        self.state.status.error = None;
                        self.state.status.retryable = false;
                        self.state.status.pending = true;
                    } else {
                        self.state.status.success("feed loaded");
                    }
                }
                Err(error) => self.state.status.failure(error.to_string()),
            },
            ApiResult::Post { request, result, .. } => match result {
                Ok(detail) if matches!(request.identity, RequestIdentity::Post(id) if id == detail.post.id) && self.state.selected_post() == Some(detail.post.id) => { self.state.view.detail = Some(detail); self.state.mode = Mode::Normal; self.state.status.success("post loaded"); }
                Ok(_) => {}
                Err(error) if matches!(request.identity, RequestIdentity::Post(id) if self.state.selected_post() == Some(id)) => self.state.status.failure(error.to_string()),
                Err(_) => {},
            },
            ApiResult::Mutation { request, mutation, result, draft, .. } => if request.identity == RequestIdentity::Mutation(mutation.clone()) { match result {
                Ok(MutationResult { success: true, post, comment, .. }) => {
                    match mutation {
                        Mutation::DeletePost(id) => {
                            self.state.view.posts.retain(|candidate| candidate.id != id);
                            if self.state.view.detail.as_ref().is_some_and(|detail| detail.post.id == id) { self.state.view.detail = None; self.state.mode = Mode::Normal; }
                            self.state.view.selected = self.state.view.selected.and_then(|selected| if self.state.view.posts.is_empty() { None } else { Some(selected.min(self.state.view.posts.len() - 1)) });
                        }
                        Mutation::DeleteComment(id) => if let Some(detail) = &mut self.state.view.detail { detail.comments.retain(|comment| comment.id != id); },
                        _ => if let Some(post) = post {
                            if let Some(existing) = self.state.view.posts.iter_mut().find(|candidate| candidate.id == post.id) { *existing = post.clone(); }
                            else if matches!(mutation, Mutation::CreatePost(_)) { self.state.view.posts.push(post.clone()); }
                            if let Some(detail) = &mut self.state.view.detail { if detail.post.id == post.id { detail.post = post; } }
                        } else if let Some(comment) = comment {
                            if let Some(detail) = &mut self.state.view.detail { if let Some(existing) = detail.comments.iter_mut().find(|item| item.id == comment.id) { *existing = comment.clone(); } else { detail.comments.push(comment); } }
                        },
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
/// Run a media handler with a safely constructed argv: no shell, no inherited
/// stdin/stdout (children must not corrupt the TUI), no authorization headers.
fn spawn_detached(template: &str, source: &OsStr, mime: &str) -> Result<()> {
    let argv = build_argv(template, source, mime);
    let Some(program) = argv.first() else {
        return Err(AppError::Media("empty media handler command".into()));
    };
    std::process::Command::new(program)
        .args(&argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|error| AppError::Media(format!("could not start media handler: {error}")))
}

/// Best-effort reveal of a directory through the system file manager.
fn reveal_directory(directory: &Path) {
    let _ = std::process::Command::new("xdg-open")
        .arg(directory)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

/// Copy a value to the system clipboard through available tools.
fn copy_to_clipboard(value: &str) -> bool {
    const TOOLS: [&[&str]; 3] = [
        &["wl-copy"],
        &["xclip", "-selection", "clipboard"],
        &["xsel", "--clipboard", "--input"],
    ];
    for tool in TOOLS {
        let Ok(mut child) = std::process::Command::new(tool[0])
            .args(&tool[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        else {
            continue;
        };
        let written = child
            .stdin
            .take()
            .is_some_and(|mut stdin| stdin.write_all(value.as_bytes()).is_ok());
        if !written {
            continue;
        }
        if child.wait().is_ok_and(|status| status.success()) {
            return true;
        }
    }
    false
}

fn mutation_for_draft(draft: &Draft, selected: Option<crate::PostId>, community: Option<crate::CommunityId>) -> Option<Mutation> {
    let mut lines = draft.content.lines();
    match draft.operation.as_str() {
        "create_comment" | "reply" => Some(Mutation::CreateComment(CreateCommentRequest { post: selected?, content: draft.content.clone() })),
        "create_post" => {
            let community = community?;
            let name = lines.next()?.trim().to_owned();
            if name.is_empty() { return None; }
            // Draft layout: title, optional link line, then body. A link line
            // is consumed only when it parses as a URL.
            let mut body = Vec::new();
            let mut url = None;
            if let Some(candidate) = lines.next() {
                match url::Url::parse(candidate.trim()) {
                    Ok(parsed) => url = Some(parsed),
                    Err(_) => body.push(candidate),
                }
            }
            body.extend(lines);
            let body = body.join("\n");
            Some(Mutation::CreatePost(CreatePostRequest { community, name, body: (!body.is_empty()).then_some(body), url }))
        }
        "edit_post" => {
            let id = lines.next()?.trim().parse().ok().map(crate::PostId)?;
            let name = lines.next().map(str::to_owned);
            let body = lines.collect::<Vec<_>>().join("\n");
            Some(Mutation::EditPost(EditPostRequest { id, name, body: (!body.is_empty()).then_some(body), url: None }))
        }
        "edit_comment" => {
            let id = lines.next()?.trim().parse().ok().map(crate::CommentId)?;
            let content = lines.collect::<Vec<_>>().join("\n");
            Some(Mutation::EditComment(EditCommentRequest { id, content }))
        }
        _ => None,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::{CacheStore, CachedFeed, FeedKey, MemoryCache};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use serde_json::json;
    use std::sync::Arc;
    use url::Url;

    #[test]
    fn queues_text_and_escape_while_action_is_in_flight() {
        let mut input = crate::input::InputEngine::new();
        let mut queued = VecDeque::new();
        let mut redraw = false;
        assert!(!queue_terminal_event(Event::Key(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE)), &mut input, &mut queued, &mut redraw));
        assert!(!queue_terminal_event(Event::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)), &mut input, &mut queued, &mut redraw));
        assert!(!queue_terminal_event(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)), &mut input, &mut queued, &mut redraw));
        assert!(matches!(queued.pop_front(), Some(AppAction::Input(Command::EnterInsert))));
        assert!(matches!(queued.pop_front(), Some(AppAction::Input(Command::Text(text))) if text == "x"));
        assert!(matches!(queued.pop_front(), Some(AppAction::Input(Command::Back))));
        assert!(queued.is_empty());
    }

    #[tokio::test]
    async fn pending_refresh_snapshot_is_visible_before_action_completes() {
        let context = ProfileContext {
            profile: Profile { id: ProfileId::from("fixture"), instance_url: Url::parse("http://127.0.0.1/").unwrap(), account_label: Some("fixture".into()) },
            session: None,
        };
        let app = App::new(
            Arc::new(crate::api::fixtures::fixture_api("feed.json")),
            Arc::new(crate::cache::MemoryCache::default()),
            context,
            Arc::new(crate::profiles::MemoryCredentialStore::default()),
        );
        let action = AppAction::Input(Command::Refresh);
        let mut model = app.render_model();
        let mut app = app;
        app.prepare_action(&action);
        model = app.render_model();
        assert!(model.status.pending);
        app.state.view.posts.push(crate::api::PostView {
            id: crate::PostId(1),
            title: "selected".into(),
            body: None,
            url: None,
            community_id: crate::CommunityId(1),
            creator_id: crate::UserId(1),
            score: 0,
            comments: 0,
            published: None,
        });
        app.state.view.selected = Some(0);
        app.state.status.pending = false;
        app.prepare_action(&AppAction::OpenSelected);
        assert!(app.render_model().status.pending);

        app.dispatch(AppAction::DeletePost(crate::PostId(1))).await.unwrap();
        let confirmation = app.render_model();
        assert!(confirmation.status.confirmation_pending);
        app.prepare_action(&AppAction::Input(Command::Refresh));
        let network = app.render_model();
        assert!(app.state.pending.is_none());
        assert!(!network.status.confirmation_pending);
        assert!(network.status.pending);
        assert!(network.status.message.is_empty());
        assert!(!confirmation.status.pending);

        let request = app.begin_request(RequestIdentity::Feed);
        app.state.status.pending = false;
        app.apply_api_result(ApiResult::Feed {
            profile: ProfileId::from("fixture"),
            request,
            result: Ok(crate::api::Page { items: Vec::new(), next_page: None }),
            stale: true,
        });
        assert!(app.render_model().status.pending);
    }
    #[tokio::test]
    async fn empty_feed_page_leaves_selection_none() {
        let context = ProfileContext {
            profile: Profile { id: ProfileId::from("fixture"), instance_url: Url::parse("http://127.0.0.1/").unwrap(), account_label: Some("fixture".into()) },
            session: None,
        };
        let mut app = App::new(
            Arc::new(crate::api::fixtures::fixture_api("feed.json")),
            Arc::new(crate::cache::MemoryCache::default()),
            context,
            Arc::new(crate::profiles::MemoryCredentialStore::default()),
        );
        app.state.view.posts = vec![
            crate::api::PostView { id: crate::PostId(1), title: "one".into(), body: None, url: None, community_id: crate::CommunityId(1), creator_id: crate::UserId(1), score: 0, comments: 0, published: None },
            crate::api::PostView { id: crate::PostId(2), title: "two".into(), body: None, url: None, community_id: crate::CommunityId(1), creator_id: crate::UserId(1), score: 0, comments: 0, published: None },
        ];
        app.state.view.selected = Some(1);
        let request = app.begin_request(RequestIdentity::Feed);
        app.apply_api_result(ApiResult::Feed {
            profile: ProfileId::from("fixture"),
            request,
            result: Ok(crate::api::Page { items: Vec::new(), next_page: None }),
            stale: false,
        });
        assert!(app.state.view.posts.is_empty());
        assert!(app.state.view.selected.is_none());
    }
    #[tokio::test]
    async fn detached_refresh_error_clears_pending_and_is_retryable() {
        let context = ProfileContext {
            profile: Profile { id: ProfileId::from("fixture"), instance_url: Url::parse("http://127.0.0.1/").unwrap(), account_label: Some("fixture".into()) },
            session: None,
        };
        let cache = Arc::new(MemoryCache::default());
        cache.write_feed(&context.profile.id, &FeedKey::from("home"), &CachedFeed::new(json!({ "items": [], "next_page": null }), 1, false)).unwrap();
        let mut app = App::new(
            Arc::new(crate::api::fixtures::fixture_api_with_status("/api/v3/post/list", 500)),
            cache,
            context,
            Arc::new(crate::profiles::MemoryCredentialStore::default()),
        );
        app.dispatch(AppAction::Input(Command::Refresh)).await.unwrap();
        assert!(app.state.status.pending);
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                app.dispatch(AppAction::Tick).await.unwrap();
                if !app.state.status.pending { break; }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        }).await.unwrap();
        assert!(app.state.status.error.is_some());
        assert!(app.state.status.retryable);
    }
}
