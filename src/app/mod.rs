pub mod actions;
pub mod help;
pub mod render;
pub mod repository;
pub mod state;
pub mod thread;

/// Lines the detail/thread pane scrolls per Ctrl-d / Ctrl-u press.
const DETAIL_SCROLL_STEP: usize = 10;

use std::{
    collections::{HashMap, HashSet, VecDeque},
    ffi::OsStr,
    io::Write,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};

use crossterm::event::{self, Event};
use ratatui::DefaultTerminal;
use tokio::sync::mpsc;
use url::Url;

use crate::{
    api::{FeedQuery, LemmyApi, LoginRequest, MutationResult, Page, PostView},
    cache::Draft,
    config::{AppConfig, MediaConfig},
    domain::{
        CreateCommentRequest, CreatePostRequest, DownloadStatus, EditCommentRequest,
        EditPostRequest, MediaRef, Mutation, Profile, ProfileContext, ProfileId, SecretString,
    },
    error::{AppError, Result},
    input::{Command, Mode},
    media::{
        CollisionPolicy, DownloadEvent, DownloadManager, DownloadRequest, MediaHandler,
        MediaPolicyConfig, build_argv, filename_for,
    },
    profiles::{CredentialStore, ProfileStore, default_store},
};
pub use actions::{
    ApiResult, AppAction, DownloadsAction, PageDirection, ProfileCommand, ProfileDraft,
    RequestIdentity, RequestToken,
};
pub use repository::{CachedRead, Repository};
pub use state::{
    AppColors, AppState, CommunitiesModal, DownloadsPanel, DownloadsRender, DraftStore, Modal,
    RenderModel, Status, ThreadModal, View,
};

pub fn run_terminal(
    app: App,
    terminal: DefaultTerminal,
    runtime: &tokio::runtime::Runtime,
    keymaps: &HashMap<String, String>,
    startup: &str,
) -> Result<()> {
    runtime.block_on(run_terminal_async(app, terminal, keymaps, startup))
}

async fn run_terminal_async(
    app: App,
    mut terminal: DefaultTerminal,
    keymaps: &HashMap<String, String>,
    startup: &str,
) -> Result<()> {
    let (input_tx, mut input_rx) = mpsc::unbounded_channel::<Result<Event>>();
    let (stop_tx, stop_rx) = std::sync::mpsc::channel();
    let input_thread = std::thread::spawn(move || {
        loop {
            if stop_rx.try_recv().is_ok() {
                break;
            }
            match event::poll(Duration::from_millis(100)) {
                Ok(true) => match event::read() {
                    Ok(event) => {
                        if input_tx.send(Ok(event)).is_err() {
                            break;
                        }
                    }
                    Err(error) => {
                        let _ =
                            input_tx.send(Err(crate::error::AppError::Terminal(error.to_string())));
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
        let mut input = crate::input::InputEngine::new()
            .with_keymaps(keymaps)
            .with_completions(help::HelpIndex::default().completion_names());
        let mut ticks = tokio::time::interval(Duration::from_millis(100));
        let mut app = Some(app);
        // Feed pages are sized to the primary pane, so the app must know the
        // terminal height before any launch fetch runs.
        let (initial_width, initial_height) = terminal
            .size()
            .map(|size| (size.width, size.height))
            .unwrap_or((80, 24));
        app.as_mut()
            .expect("application is present")
            .dispatch(AppAction::Resize {
                width: initial_width,
                height: initial_height,
            })
            .await?;
        let mut model = app.as_ref().expect("application is present").render_model();
        let mut action_task: Option<tokio::task::JoinHandle<(App, Result<()>)>> = None;
        let mut queued_actions = VecDeque::new();
        let mut redraw = true;
        let mut quit = false;
        // Run the configured startup action (for example `feed`) through the
        // normal queue instead of awaiting it: the first frame draws
        // immediately (with the pending/loading state) and the feed lands
        // asynchronously, so a slow instance never shows a blank terminal
        // for the whole load.
        if !startup.is_empty() {
            queued_actions.push_back(AppAction::Input(Command::SubmitLine(startup.to_owned())));
        }

        while !quit {
            if redraw {
                terminal
                    .draw(|frame| render::render(frame, &model))
                    .map_err(|error| crate::error::AppError::Terminal(error.to_string()))?;
                redraw = false;
            }

            if action_task.is_none() && let Some(action) = queued_actions.pop_front() {
                start_action(&mut app, &mut model, &mut action_task, action, &mut redraw);
                continue;
            }

            if let Some(task) = action_task.as_mut() {
                tokio::select! {
                    completed = task => {
                        action_task = None;
                        let (finished, result) = completed.map_err(|error| crate::error::AppError::Terminal(format!("application task failed: {error}")))?;
                        app = Some(finished);
                        result?;
                        // Redraw only when the finished action actually
                        // changed the view (idle ticks change nothing).
                        let fresh = app.as_ref().expect("application is present").render_model();
                        if fresh != model {
                            model = fresh;
                            redraw = true;
                        }
                    }
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
            if command == crate::input::Command::Quit {
                // Route the quit key through `AppAction::Quit` so the
                // download manager is shut down and session history cleared
                // on interactive exit. The loop then ends once the dispatched
                // action sets the app's quit flag.
                queued_actions.push_back(AppAction::Quit);
                return false;
            }
            if command != crate::input::Command::Noop {
                queued_actions.push_back(command.into());
            }
        }
        Event::Resize(width, height) => {
            queued_actions.push_back(AppAction::Resize { width, height });
            *redraw = true;
        }
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
    // Only redraw when the pre-action snapshot differs from what is on
    // screen. The idle Tick fires ten times a second just to poll for
    // completed work; forcing a full render each time would burn ~30% CPU.
    let before = model.clone();
    *model = owned.render_model();
    *redraw = *model != before;
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
    /// Completed detached reads awaiting application on the next Tick.
    /// Spawned read tasks push here; the Tick arm drains into
    /// `apply_api_result` (generation-guarded).
    completed: Arc<Mutex<VecDeque<ApiResult>>>,
    /// Handles of detached reads in flight, keyed by identity so
    /// supersession and Esc-cancel can abort them.
    in_flight: HashMap<RequestIdentity, tokio::task::JoinHandle<()>>,
    downloads: DownloadManager,
    /// Download ids created to serve external media handlers; their files
    /// live in the temp directory and are removed when the client exits.
    scratch_downloads: HashSet<crate::domain::DownloadId>,
    media_policy: MediaPolicyConfig,
    collision_policy: CollisionPolicy,
    /// Resolved `[colors]` palette applied at render time.
    colors: state::AppColors,
    /// Latest terminal size: height sizes feed pages to the primary pane.
    /// Zero means unknown (no Resize seen yet); feeds then fall back to the
    /// fixed default limit.
    terminal_width: u16,
    terminal_height: u16,
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
        Self::with_media_and_profile_store(
            api,
            cache,
            active,
            credentials,
            MediaConfig::default(),
            profile_store,
        )
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
        let downloads_directory = media
            .download_directory
            .clone()
            .unwrap_or_else(|| crate::config::cache_dir().join("downloads"));
        let collision_policy = CollisionPolicy::from_config(&media.collision_policy);
        let media_policy = MediaPolicyConfig::from_config(&media);
        Self {
            state,
            repository: Repository::new(api, cache, credentials),
            profile_store,
            requests: HashMap::new(),
            next_generation: 0,
            completed: Arc::new(Mutex::new(VecDeque::new())),
            in_flight: HashMap::new(),
            downloads: DownloadManager::new(downloads_directory),
            scratch_downloads: HashSet::new(),
            media_policy,
            collision_policy,
            colors: state::AppColors::default(),
            terminal_width: 0,
            terminal_height: 0,
            quit: false,
        }
    }

    /// Apply the `[colors]` palette from configuration.
    pub fn with_colors(mut self, colors: state::AppColors) -> Self {
        self.colors = colors;
        self
    }

    /// Best-effort removal of completed scratch files created for external
    /// media handlers. In-flight downloads keep their `.part` handling in
    /// `DownloadManager::shutdown`. The handler may still be showing the
    /// file, but the session is ending, so the temp file's purpose is over.
    fn remove_scratch_files(&mut self) {
        let paths = self
            .scratch_downloads
            .iter()
            .filter_map(|id| self.downloads.history().get(*id))
            .filter(|record| {
                record.status == DownloadStatus::Completed && !record.local_file_deleted
            })
            .map(|record| record.local_path.clone())
            .collect::<Vec<_>>();
        for path in paths {
            let _ = std::fs::remove_file(path);
        }
    }

    /// Quit the client: abort in-flight reads, remove scratch media files,
    /// shut down the download manager, and set the quit flag. Every quit
    /// path funnels through here.
    fn quit(&mut self) {
        self.cancel_in_flight();
        self.remove_scratch_files();
        self.downloads.shutdown();
        self.quit = true;
    }

    /// The home feed query with the session's chosen sort applied.
    fn home_query(&self) -> FeedQuery {
        let mut query = FeedQuery::home();
        query.sort = self.state.view.feed_sort.clone();
        query
    }

    /// Page size for the current terminal: exactly what fits the primary
    /// pane when the height is known, otherwise the fixed default.
    fn feed_limit(&self) -> u32 {
        if self.terminal_height == 0 {
            FeedQuery::DEFAULT_LIMIT
        } else {
            render::feed_limit_for_height(self.terminal_height) as u32
        }
    }

    pub fn begin_request(&mut self, identity: RequestIdentity) -> RequestToken {
        // A newer request for the same identity supersedes the in-flight
        // one: abort its task so its result cannot land late.
        if let Some(handle) = self.in_flight.remove(&identity) {
            handle.abort();
        }
        self.next_generation = self.next_generation.wrapping_add(1);
        let token = RequestToken {
            generation: self.next_generation,
            identity: identity.clone(),
        };
        self.requests.insert(identity, token.clone());
        token
    }

    pub fn render_model(&self) -> RenderModel {
        let mut model = self.state.render_model();
        model.colors = self.colors.clone();
        model.downloads = self
            .state
            .view
            .downloads
            .as_ref()
            .map(|panel| DownloadsRender {
                query: panel.query.clone(),
                selected: panel.selected,
                records: self.downloads.history().filtered(&panel.query),
            });
        model
    }

    /// Abort every detached read in flight and invalidate every request
    /// token, so no straggler result can land afterwards (aborted tasks
    /// never push; late mailbox items fail the token guard).
    fn cancel_in_flight(&mut self) {
        for (_, handle) in self.in_flight.drain() {
            handle.abort();
        }
        self.requests.clear();
    }

    /// Spawn a detached read whose `ApiResult` is applied on the next Tick.
    /// `begin_request` must already have registered the token; the task
    /// owns only clones and never touches `App` itself.
    fn detach<F>(&mut self, identity: RequestIdentity, future: F)
    where
        F: std::future::Future<Output = ApiResult> + Send + 'static,
    {
        let completed = self.completed.clone();
        let handle = tokio::spawn(async move {
            let result = future.await;
            if let Ok(mut queue) = completed.lock() {
                queue.push_back(result);
            }
        });
        self.in_flight.insert(identity, handle);
    }

    /// Apply every completed detached read, retiring its in-flight handle.
    fn poll_completed(&mut self) {
        let drained = {
            let mut queue = self.completed.lock().expect("completed mailbox poisoned");
            queue.drain(..).collect::<Vec<_>>()
        };
        for result in drained {
            self.in_flight.remove(&result.identity());
            self.apply_api_result(result);
        }
    }

    /// Whether any detached read is still in flight or awaiting
    /// application. Tests use this to drain the pipeline deterministically;
    /// the event loop itself is driven by the Tick action. Production code
    /// does not call it.
    pub fn has_pending_work(&self) -> bool {
        !self.in_flight.is_empty()
            || !self
                .completed
                .lock()
                .expect("completed mailbox poisoned")
                .is_empty()
            || self.state.status.pending
            || self.state.view.loading
    }
    pub fn is_quit(&self) -> bool {
        self.quit
    }

    fn prepare_action(&mut self, action: &AppAction) {
        let is_confirm = matches!(action, AppAction::Confirm) && self.state.pending.is_some();
        let is_network = matches!(
            action,
            AppAction::Input(Command::Refresh)
                | AppAction::Input(Command::NextPage)
                | AppAction::Input(Command::PreviousPage)
                | AppAction::OpenCommunity(_)
                | AppAction::LoadMore
                | AppAction::Mutate(_)
                | AppAction::Media
                | AppAction::DownloadMedia
                | AppAction::Profile(ProfileCommand::Login | ProfileCommand::Logout,)
        ) || is_confirm
            || (matches!(action, AppAction::OpenSelected) && self.state.selected_post().is_some());
        if !is_network {
            return;
        }
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
            AppAction::Resize { width, height } => {
                self.terminal_width = width;
                self.terminal_height = height;
                Ok(())
            }
            AppAction::Profile(command) => self.dispatch_profile(command).await,
            AppAction::SubmitDraft(id) => self.submit_draft(id).await,
            AppAction::DiscardDraft(id) => {
                self.state.drafts.mark_completed(id);
                self.state.status.success("draft discarded");
                Ok(())
            }
            AppAction::OpenSelected => self.open_selected().await,
            AppAction::OpenCommunity(id) => self.open_community(id).await,
            AppAction::LoadMore => self.next_page().await,
            AppAction::Back => {
                // Same semantics as Esc: pop the focused modal, then fall
                // back to closing the downloads panel.
                if self.close_top_modal().is_some() {
                    self.cancel_pending();
                    return Ok(());
                }
                self.close_detail_pane().await
            }
            AppAction::DeletePost(id) => self.delete_post(id).await,
            AppAction::Mutate(mutation) => self.start_mutation(mutation, None).await,
            AppAction::Confirm => self.confirm_pending().await,
            AppAction::Cancel => {
                self.cancel_pending();
                Ok(())
            }
            AppAction::ApiResult(result) => {
                self.apply_api_result(*result);
                Ok(())
            }
            AppAction::Media => self.open_media_selected().await,
            AppAction::DownloadMedia => self.download_media_selected().await,
            AppAction::ShowDownloads => {
                self.toggle_downloads_panel();
                Ok(())
            }
            AppAction::Downloads(action) => self.downloads_action(action).await,
            AppAction::Tick => {
                self.poll_feed_refresh().await;
                self.poll_completed();
                self.poll_downloads();
                Ok(())
            }
            AppAction::Quit => {
                self.quit();
                Ok(())
            }
        }
    }

    async fn dispatch_command(&mut self, command: Command) -> Result<()> {
        match command {
            Command::Open => match self.state.view.top_modal() {
                Some(Modal::Communities(_)) => {
                    // Enter in the community picker opens the selected
                    // community's feed: pop the picker, dismiss any thread
                    // underneath (its post belongs to the old feed), and load
                    // the community.
                    let Some(id) = self
                        .state
                        .view
                        .top_modal_mut()
                        .and_then(|modal| match modal {
                            Modal::Communities(communities) => communities.selected_community(),
                            _ => None,
                        })
                    else {
                        self.state.status.failure("no community selected");
                        return Ok(());
                    };
                    self.state.view.pop_modal();
                    self.state.view.dismiss_threads();
                    return self.open_community(id).await;
                }
                Some(_) => Ok(()), // Enter on a thread/help modal: nothing to open
                None => {
                    if self.state.view.downloads_active() {
                        return self.downloads_action(DownloadsAction::Reopen).await;
                    }
                    self.open_selected().await
                }
            },
            Command::OpenMedia => {
                if self.state.view.has_modals() {
                    // Media acts on the feed selection, which a modal hides.
                    return Ok(());
                }
                if self.state.view.downloads_active() {
                    return self.downloads_action(DownloadsAction::Reopen).await;
                }
                self.open_media_selected().await
            }
            Command::Communities => self.communities_command().await,
            Command::CompleteLine(line) => {
                // The engine already holds the completed line; mirror it into
                // the visible compose buffer.
                self.state.view.compose = line;
                Ok(())
            }
            Command::Back => {
                // Esc pops the focused modal; it also cancels a staged
                // confirmation, matching the historical meaning of Esc.
                if self.close_top_modal().is_some() {
                    self.cancel_pending();
                    return Ok(());
                }
                self.close_detail_pane().await
            }
            Command::ClosePane => {
                if self.close_top_modal().is_some() {
                    return Ok(());
                }
                self.close_detail_pane().await
            }
            Command::Quit => {
                self.quit();
                Ok(())
            }
            Command::Confirm => {
                // User-reachable confirmation for staged destructive actions
                // (default key `y`); a no-op unless a confirmation is pending.
                if self.state.status.confirmation_pending {
                    self.confirm_pending().await
                } else {
                    Ok(())
                }
            }
            Command::Cancel => {
                // User-reachable cancellation (Esc or `:cancel`); acts only on
                // a staged confirmation, so the key never swallows unrelated
                // input when nothing is pending.
                if self.state.status.confirmation_pending {
                    self.cancel_pending();
                }
                Ok(())
            }
            Command::Refresh => {
                match self.state.view.top_modal() {
                    Some(Modal::Communities(_)) => return self.refresh_communities().await,
                    Some(Modal::Thread(thread)) => {
                        return self.refresh_thread_comments(thread.post.post.id).await;
                    }
                    Some(Modal::Help(_)) => return Ok(()),
                    None => {}
                }
                if self.state.view.downloads_active() {
                    return self.downloads_action(DownloadsAction::Retry).await;
                }
                self.refresh_feed().await
            }
            Command::NextPage => {
                // Feed pagination belongs to the feed: inert while any modal
                // is open or the downloads panel replaces the content.
                if self.state.view.has_modals() || self.state.view.downloads_active() {
                    return Ok(());
                }
                self.next_page().await
            }
            Command::PreviousPage => {
                if self.state.view.has_modals() || self.state.view.downloads_active() {
                    return Ok(());
                }
                self.previous_page().await
            }
            Command::MoveDown { count } => {
                match self.state.view.top_modal_mut() {
                    Some(Modal::Thread(thread)) => {
                        thread.scroll = thread.scroll.saturating_add(count as usize);
                        return Ok(());
                    }
                    Some(Modal::Communities(communities)) => {
                        communities.move_selection(count as isize);
                        return Ok(());
                    }
                    Some(Modal::Help(help)) => {
                        help.scroll = help.scroll.saturating_add(count as usize);
                        return Ok(());
                    }
                    None => {}
                }
                self.move_selection(count as isize);
                Ok(())
            }
            Command::MoveUp { count } => {
                match self.state.view.top_modal_mut() {
                    Some(Modal::Thread(thread)) => {
                        thread.scroll = thread.scroll.saturating_sub(count as usize);
                        return Ok(());
                    }
                    Some(Modal::Communities(communities)) => {
                        communities.move_selection(-(count as isize));
                        return Ok(());
                    }
                    Some(Modal::Help(help)) => {
                        help.scroll = help.scroll.saturating_sub(count as usize);
                        return Ok(());
                    }
                    None => {}
                }
                self.move_selection(-(count as isize));
                Ok(())
            }
            Command::GoToFirst { count } => {
                if let Some(Modal::Communities(communities)) = self.state.view.top_modal_mut() {
                    communities.goto_first(count as usize);
                    return Ok(());
                }
                // `gg` (or `N gg`) jumps to the Nth row; the default count
                // of one lands on the first row. An empty feed clears the
                // selection instead of pointing nowhere.
                let last = self.state.view.posts.len().checked_sub(1);
                self.state.view.selected =
                    last.map(|last| (count.saturating_sub(1) as usize).min(last));
                Ok(())
            }
            Command::GoToLast { count } => {
                if let Some(Modal::Communities(communities)) = self.state.view.top_modal_mut() {
                    communities.goto_last(count as usize);
                    return Ok(());
                }
                // `G` jumps to the last row; `N G` to the Nth row (clamped).
                let last = self.state.view.posts.len().checked_sub(1);
                self.state.view.selected = last.map(|last| {
                    if count <= 1 {
                        last
                    } else {
                        (count as usize).saturating_sub(1).min(last)
                    }
                });
                Ok(())
            }
            Command::ScrollDetailDown { count } => {
                match self.state.view.top_modal_mut() {
                    Some(Modal::Thread(thread)) => {
                        thread.scroll = thread
                            .scroll
                            .saturating_add(DETAIL_SCROLL_STEP * count as usize);
                    }
                    Some(Modal::Help(help)) => {
                        help.scroll = help
                            .scroll
                            .saturating_add(DETAIL_SCROLL_STEP * count as usize);
                    }
                    _ => {}
                }
                Ok(())
            }
            Command::ScrollDetailUp { count } => {
                match self.state.view.top_modal_mut() {
                    Some(Modal::Thread(thread)) => {
                        thread.scroll = thread
                            .scroll
                            .saturating_sub(DETAIL_SCROLL_STEP * count as usize);
                    }
                    Some(Modal::Help(help)) => {
                        help.scroll = help
                            .scroll
                            .saturating_sub(DETAIL_SCROLL_STEP * count as usize);
                    }
                    _ => {}
                }
                Ok(())
            }
            // Placeholders for Task 5's new commands: the real handlers land
            // in Task 6 (toggle/collapse/expand comment threads).
            Command::ToggleCommentThread
            | Command::CollapseAllCommentThreads
            | Command::ExpandAllCommentThreads => Ok(()),
            Command::EnterInsert => {
                self.state.mode = Mode::Insert;
                Ok(())
            }
            Command::EnterVisual => {
                self.state.mode = Mode::Visual;
                Ok(())
            }
            Command::EnterCommand => {
                self.state.mode = Mode::Command;
                self.state.view.compose.clear();
                Ok(())
            }
            Command::EnterSearch { backward } => {
                self.state.mode = if backward {
                    Mode::SearchBackward
                } else {
                    Mode::SearchForward
                };
                self.state.view.compose.clear();
                Ok(())
            }
            Command::Text(text) => {
                self.state.view.compose.push_str(&text);
                Ok(())
            }
            Command::Backspace => {
                // The compose buffer mirrors the command line and the insert
                // draft; deleting the engine's line must delete the visible
                // text too, or backspace looks dead.
                self.state.view.compose.pop();
                Ok(())
            }
            Command::CancelLine => {
                // Abandoning a command/search line clears its visible text
                // and returns to Normal without touching the open view.
                self.state.mode = Mode::Normal;
                self.state.view.compose.clear();
                Ok(())
            }
            Command::SubmitLine(line) => self.submit_line(line).await,
            Command::MoveLeft { .. } | Command::MoveRight { .. } | Command::Noop => Ok(()),
        }
    }

    /// Execute a profile command: switch, list, create, login, logout,
    /// whoami, or delete. Every variant funnels through the shared profile
    /// service so the terminal and any other caller share one session
    /// lifecycle. Login stores a session only after the API call succeeds;
    /// logout is destructive only to the session; switching is a hard context
    /// transition that clears stale transient state before any request can
    /// observe the old context.
    pub async fn execute_profile_command(&mut self, command: ProfileCommand) -> Result<()> {
        match command {
            ProfileCommand::Switch(id) => self.switch_profile(id).await,
            ProfileCommand::List => self.list_profiles(),
            ProfileCommand::New(draft) => self.create_profile(draft).await,
            ProfileCommand::Login => self.login_from_compose().await,
            ProfileCommand::Logout => self.logout().await,
            ProfileCommand::WhoAmI => {
                self.state.status.success(
                    self.state
                        .active
                        .session
                        .as_ref()
                        .map(|session| format!("user {}", session.user_id.0))
                        .unwrap_or_else(|| "anonymous".into()),
                );
                Ok(())
            }
            ProfileCommand::Delete(id) => self.delete_profile(id).await,
        }
    }

    async fn dispatch_profile(&mut self, command: ProfileCommand) -> Result<()> {
        self.execute_profile_command(command).await
    }

    fn list_profiles(&mut self) -> Result<()> {
        match self.profile_store.load() {
            Ok(profiles) if !profiles.is_empty() => {
                let list = profiles
                    .iter()
                    .map(|profile| {
                        if profile.id == self.state.active.profile.id {
                            format!("{} (active)", profile.id)
                        } else {
                            profile.id.to_string()
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                self.state.status.success(format!("profiles: {list}"));
            }
            Ok(_) => self.state.status.success("no profiles configured"),
            Err(error) => self.state.status.failure(error.to_string()),
        }
        Ok(())
    }

    async fn create_profile(&mut self, draft: ProfileDraft) -> Result<()> {
        self.cancel_in_flight();
        let profile = Profile {
            id: draft.id,
            instance_url: draft.instance_url,
            account_label: draft.account_label,
        };
        if let Err(error) = crate::profiles::validate_instance(&profile.instance_url) {
            self.state.status.failure(error.to_string());
            return Ok(());
        }
        let mut config = match self.profile_store.load_config() {
            Ok(config) => config,
            Err(error) => {
                self.state.status.failure(error.to_string());
                return Ok(());
            }
        };
        let replacing = self.state.active.profile.id == profile.id
            || config
                .profiles
                .iter()
                .any(|existing| existing.id == profile.id);
        if replacing {
            self.repository
                .invalidate_profile_context(&profile.id)
                .await;
            if let Err(error) = self
                .repository
                .credentials
                .delete_session(&profile.id)
                .await
            {
                self.state.status.failure(error.to_string());
                return Ok(());
            }
        }
        if let Some(existing) = config
            .profiles
            .iter_mut()
            .find(|existing| existing.id == profile.id)
        {
            *existing = profile.clone();
        } else {
            config.profiles.push(profile.clone());
        }
        if let Err(error) = self.profile_store.save_config(&config) {
            self.state.status.failure(error.to_string());
            return Ok(());
        }
        self.state.switch_context(ProfileContext {
            profile,
            session: None,
        });
        self.state.status.success("profile created");
        Ok(())
    }

    /// `:login <username> <password>` — credentials come from the compose
    /// buffer. The password is consumed in memory only: it is never written
    /// to config, logged, or echoed in status.
    /// `:sort <name>` — set the sort order of the feed list (for example
    /// `New`, `Hot`, `TopWeek`). The choice applies to the current view and
    /// sticks for the session, so `:subscribed` after `:sort New` shows your
    /// subscribed feed sorted by New, exactly like the web UI's
    /// `listingType=Subscribed&sort=New`.
    async fn sort_command(&mut self, args: &[&str]) -> Result<()> {
        let [name] = args else {
            self.state.status.failure(format!(
                "usage: sort <{}>",
                crate::api::FEED_SORTS.join("|")
            ));
            return Ok(());
        };
        let Some(canonical) = crate::api::FEED_SORTS
            .iter()
            .find(|candidate| candidate.eq_ignore_ascii_case(name))
        else {
            self.state.status.failure(format!(
                "unknown sort {name:?}: use one of {}",
                crate::api::FEED_SORTS.join(", ")
            ));
            return Ok(());
        };
        self.state.view.feed_sort = (*canonical).to_owned();
        self.state.view.feed_query.sort = (*canonical).to_owned();
        self.state.view.next_page = None;
        self.refresh_feed().await?;
        // The refresh already reported the load; add the sort change on top
        // so the user knows the list re-sorted. Failures keep their error.
        if self.state.status.error.is_none() {
            self.state.status.success(format!("sorted by {canonical}"));
        }
        Ok(())
    }

    /// `:communities` (or `C`) — push the community-list modal (toggles
    /// closed when it is already the focused modal). Defaults to the
    /// subscribed communities when logged in, otherwise the local ones.
    async fn communities_command(&mut self) -> Result<()> {
        if matches!(self.state.view.top_modal(), Some(Modal::Communities(_))) {
            self.state.view.pop_modal();
            return Ok(());
        }
        let listing = if self.state.active.session.is_some() {
            crate::api::FeedListing::Subscribed
        } else {
            crate::api::FeedListing::Local
        };
        self.state
            .view
            .push_modal(Modal::Communities(state::CommunitiesModal::new(listing)));
        self.refresh_communities().await
    }

    /// Fetch the community list for the picker's current listing.
    async fn refresh_communities(&mut self) -> Result<()> {
        let Some(Modal::Communities(modal)) = self.state.view.top_modal() else {
            return Ok(());
        };
        let context = self.state.active.clone();
        let profile = context.profile.id.clone();
        let query = crate::api::CommunityQuery {
            listing: modal.listing,
            limit: Some(crate::api::CommunityQuery::DEFAULT_LIMIT),
        };
        let request = self.begin_request(RequestIdentity::Communities);
        if let Some(read) = self.repository.cached_communities(&context, &query).await? {
            self.apply_api_result(ApiResult::Communities {
                profile: profile.clone(),
                request: request.clone(),
                result: Ok(read.value),
                stale: read.stale,
            });
        } else {
            self.state.view.loading = true;
        }
        let repository = self.repository.clone();
        self.detach(RequestIdentity::Communities, async move {
            let result = repository.api.communities(&context, query.clone()).await;
            if let Ok(page) = &result {
                let _ = repository
                    .record_fresh_communities(&context, &query, page)
                    .await;
            }
            ApiResult::Communities {
                profile,
                request,
                result,
                stale: false,
            }
        });
        Ok(())
    }

    /// `:sort <subscribed|local|all>` — switch which community list the
    /// picker shows (the feed sort doesn't apply inside the modal).
    async fn sort_communities_command(&mut self, args: &[&str]) -> Result<()> {
        let [name] = args else {
            self.state
                .status
                .failure("usage: sort <subscribed|local|all> (communities list)");
            return Ok(());
        };
        let listing = match name.to_ascii_lowercase().as_str() {
            "subscribed" => crate::api::FeedListing::Subscribed,
            "local" => crate::api::FeedListing::Local,
            "all" => crate::api::FeedListing::All,
            other => {
                self.state.status.failure(format!(
                    "unknown community list {other:?}: use subscribed, local, or all"
                ));
                return Ok(());
            }
        };
        if let Some(Modal::Communities(modal)) = self.state.view.top_modal_mut() {
            modal.listing = listing;
        }
        self.refresh_communities().await?;
        if self.state.status.error.is_none() {
            self.state.status.success(format!(
                "showing {} communities",
                Self::listing_label(listing)
            ));
        }
        Ok(())
    }

    /// Re-fetch the comments of the focused thread modal.
    async fn refresh_thread_comments(&mut self, id: crate::PostId) -> Result<()> {
        let context = self.state.active.clone();
        let profile = context.profile.id.clone();
        let request = self.begin_request(RequestIdentity::Comments(id));
        if let Some(read) = self.repository.cached_comments(&context, id).await? {
            self.apply_api_result(ApiResult::Comments {
                profile: profile.clone(),
                request: request.clone(),
                post: id,
                result: Ok(read.value),
                stale: read.stale,
            });
        }
        let repository = self.repository.clone();
        self.detach(RequestIdentity::Comments(id), async move {
            let result = repository.api.comments(&context, id).await;
            if let Ok(comments) = &result {
                let _ = repository
                    .record_fresh_comments(&context, id, comments)
                    .await;
            }
            ApiResult::Comments {
                profile,
                request,
                post: id,
                result,
                stale: false,
            }
        });
        Ok(())
    }

    /// Lowercase label for a community listing, for status messages.
    fn listing_label(listing: crate::api::FeedListing) -> &'static str {
        match listing {
            crate::api::FeedListing::All => "all",
            crate::api::FeedListing::Local => "local",
            crate::api::FeedListing::Subscribed => "subscribed",
        }
    }

    /// Pop the focused modal. Closing a thread also aborts its in-flight
    /// post/comment requests, so a stale result can never land in a later
    /// thread for the same post.
    fn close_top_modal(&mut self) -> Option<Modal> {
        let post = match self.state.view.top_modal() {
            Some(Modal::Thread(thread)) => Some(thread.post.post.id),
            _ => None,
        };
        let popped = self.state.view.pop_modal();
        if let Some(post) = post {
            self.requests.retain(|identity, _| {
                !matches!(
                    identity,
                    RequestIdentity::Post(id) | RequestIdentity::Comments(id) if *id == post
                )
            });
            for identity in [RequestIdentity::Post(post), RequestIdentity::Comments(post)] {
                if let Some(handle) = self.in_flight.remove(&identity) {
                    handle.abort();
                }
            }
        }
        // Closing the community picker retires its in-flight load too.
        if matches!(popped, Some(Modal::Communities(_))) {
            if let Some(handle) = self.in_flight.remove(&RequestIdentity::Communities) {
                handle.abort();
            }
            self.requests
                .retain(|identity, _| *identity != RequestIdentity::Communities);
        }
        popped
    }

    async fn login_from_compose(&mut self) -> Result<()> {
        let mut tokens = self.state.view.compose.split_whitespace();
        let first = tokens.next().unwrap_or_default().trim_start_matches(':');
        let username = if first == "login" {
            tokens.next()
        } else {
            Some(first)
        };
        let password = tokens.next();
        let (Some(username), Some(password)) = (username, password) else {
            self.state
                .status
                .failure("usage: login <username> <password>");
            return Ok(());
        };
        if tokens.next().is_some() {
            self.state
                .status
                .failure("usage: login <username> <password>");
            return Ok(());
        }
        self.perform_login(LoginRequest {
            profile: self.state.active.profile.id.clone(),
            instance_url: self.state.active.profile.instance_url.clone(),
            username: username.to_owned(),
            password: SecretString::from(password),
        })
        .await
    }

    async fn perform_login(&mut self, request: LoginRequest) -> Result<()> {
        self.cancel_in_flight();
        // The password arrived through the on-screen compose buffer; it must
        // not persist on screen or in state after the attempt, whether the
        // login succeeds or fails.
        self.state.view.compose.clear();
        // The credential-store write can block (a locked or prompting macOS
        // Keychain, an unavailable Secret Service); without a deadline a
        // hung store would leave the login silently in flight with no
        // feedback and no session. Bound the whole attempt and say so.
        match tokio::time::timeout(
            Duration::from_secs(30),
            crate::profiles::login(
                self.repository.api.as_ref(),
                self.repository.credentials.as_ref(),
                request,
            ),
        )
        .await
        {
            Ok(Ok(session)) => {
                let user = session.user_id.0;
                self.state.active.session = Some(session);
                self.state
                    .status
                    .success(format!("logged in as user {user}"));
            }
            Ok(Err(error)) => self.state.status.failure(error.to_string()),
            Err(_) => self.state.status.failure(
                "login timed out after 30 seconds — check the connection and that the OS credential store (macOS Keychain) is available and unlocked",
            ),
        }
        Ok(())
    }

    async fn logout(&mut self) -> Result<()> {
        self.cancel_in_flight();
        let id = self.state.active.profile.id.clone();
        if let Err(error) = crate::profiles::logout(
            &self.profile_store,
            self.repository.credentials.as_ref(),
            &id,
        )
        .await
        {
            self.state.status.failure(error.to_string());
        } else {
            self.repository.invalidate_profile_context(&id).await;
            let mut context = self.state.active.clone();
            context.session = None;
            self.state.switch_context(context);
            self.state.status.success("logged out");
        }
        Ok(())
    }

    async fn delete_profile(&mut self, id: ProfileId) -> Result<()> {
        if id == self.state.active.profile.id {
            self.state
                .status
                .failure("cannot delete the active profile; switch to another profile first");
            return Ok(());
        }
        let mut config = match self.profile_store.load_config() {
            Ok(config) => config,
            Err(error) => {
                self.state.status.failure(error.to_string());
                return Ok(());
            }
        };
        if !config.profiles.iter().any(|profile| profile.id == id) {
            self.state
                .status
                .failure(format!("profile {id} is not configured"));
            return Ok(());
        }
        self.repository.invalidate_profile_context(&id).await;
        if let Err(error) = self.repository.credentials.delete_session(&id).await {
            self.state.status.failure(error.to_string());
            return Ok(());
        }
        config.profiles.retain(|profile| profile.id != id);
        if let Err(error) = self.profile_store.save_config(&config) {
            self.state.status.failure(error.to_string());
            return Ok(());
        }
        self.state.status.success(format!("profile {id} deleted"));
        Ok(())
    }

    async fn switch_profile(&mut self, id: ProfileId) -> Result<()> {
        self.cancel_in_flight();
        // A profile-store read failure must never terminate the TUI: surface
        // it in the status line and stay in the current profile.
        let profiles = match self.profile_store.load() {
            Ok(profiles) => profiles,
            Err(error) => {
                self.state.status.failure(error.to_string());
                return Ok(());
            }
        };
        let profile = match profiles.into_iter().find(|profile| profile.id == id) {
            Some(profile) => profile,
            None => {
                self.state
                    .status
                    .failure(format!("profile {id} is not configured"));
                return Ok(());
            }
        };
        let session = match self.repository.session(&id).await {
            Ok(session) => session,
            Err(error) => {
                self.state.status.failure(error.to_string());
                return Ok(());
            }
        };
        self.cancel_in_flight();
        self.state
            .switch_context(ProfileContext { profile, session });
        let context = self.state.active.clone();
        if let Ok(Some(read)) = self
            .repository
            .cached_feed(&context, &FeedQuery::home())
            .await
        {
            self.state.view.posts = read.value.items;
            self.state.view.stale = read.stale;
            self.state.status.stale = read.stale;
            self.state.status.message = if read.stale {
                "stale cache loaded".into()
            } else {
                "cache loaded".into()
            };
        }
        Ok(())
    }

    async fn refresh_feed(&mut self) -> Result<()> {
        let context = self.state.active.clone();
        self.state.status.pending = true;
        let profile = context.profile.id.clone();
        // Pull exactly what the primary pane can show at the current size.
        self.state.view.feed_query.limit = Some(self.feed_limit());
        let query = self.state.view.feed_query.clone();
        let request = self.begin_request(RequestIdentity::Feed);
        // A true miss has nothing to paint: spawn the fetch detached and
        // show the loading state; the fresh page lands through
        // `poll_feed_refresh` once the background refresh completes.
        // (The cache-hit path returns instantly with the stale page and
        // spawns its own background refresh, as before.)
        if self
            .repository
            .cached_feed(&context, &query)
            .await?
            .is_none()
        {
            self.state.view.loading = true;
            self.repository
                .spawn_feed_miss(&context, query, request.generation)
                .await?;
            return Ok(());
        }
        match self
            .repository
            .feed_with_generation(&context, query, request.generation)
            .await
        {
            Ok(read) => self.apply_api_result(ApiResult::Feed {
                profile,
                request,
                result: Ok(read.value),
                stale: read.stale,
            }),
            Err(error) => self.apply_api_result(ApiResult::Feed {
                profile,
                request,
                result: Err(error),
                stale: false,
            }),
        }
        Ok(())
    }

    async fn submit_line(&mut self, line: String) -> Result<()> {
        if matches!(self.state.mode, Mode::SearchForward | Mode::SearchBackward) {
            let search = line.trim().to_owned();
            self.state.view.search = search.clone();
            self.state.view.feed_query = FeedQuery {
                search: (!search.is_empty()).then_some(search),
                sort: self.state.view.feed_sort.clone(),
                ..FeedQuery::home()
            };
            // A new search starts a fresh pagination cursor; the previous
            // feed's `next_page` is stale and must not be reused by LoadMore
            // (a failed search would otherwise mix pages from the old query).
            self.state.view.next_page = None;
            self.state.mode = Mode::Normal;
            self.state.view.compose.clear();
            return self.refresh_feed().await;
        }
        self.state.mode = Mode::Normal;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return Ok(());
        }
        // The command line commonly starts with the `:` that entered command
        // mode, so accept both `:profile` and `profile` spellings.
        let trimmed = trimmed.strip_prefix(':').unwrap_or(trimmed);
        let mut parts = trimmed.split_whitespace();
        let command = parts.next().unwrap_or_default();
        let args: Vec<&str> = parts.collect();
        // While a modal is open, only commands that belong to it (or that
        // deliberately replace the feed context) run; everything else would
        // act on the hidden feed underneath. Navigation dismisses the thread
        // view — its post belongs to the old feed context — while overlay
        // modals (help, communities) stay.
        if self.state.view.has_modals() {
            match command {
                // Modal lifecycle: these always work.
                "communities" | "help" | "close" | "quit" => {}
                // Navigation replaces the feed contents; the thread view goes
                // with the old context.
                "feed" | "subscribed" | "community" | "search" | "sort" => {
                    self.state.view.dismiss_threads();
                }
                // Feed-post actions only make sense while the thread modal is
                // on screen (its post is what you are looking at).
                "post" | "open" | "media" | "download-media" | "download_media" | "reply"
                | "edit" | "vote" | "save" | "subscribe" | "delete"
                    if matches!(self.state.view.top_modal(), Some(Modal::Thread(_))) => {}
                // Confirmations and account/profile commands never touch the
                // hidden feed.
                "confirm" | "yes" | "cancel" | "profile" | "profile-new" | "profile-delete"
                | "login" | "logout" | "whoami" => {}
                _ => {
                    self.state
                        .status
                        .failure("close the open view (Esc) before using content commands");
                    return Ok(());
                }
            }
        }
        let result = match command {
            "profile" => match args.as_slice() {
                [] => self.execute_profile_command(ProfileCommand::List).await,
                [id] => {
                    self.execute_profile_command(ProfileCommand::Switch(ProfileId::from(*id)))
                        .await
                }
                _ => {
                    self.state.status.failure("usage: profile [<id>]");
                    Ok(())
                }
            },
            "profile-new" => match args.as_slice() {
                [id, instance, label @ ..] if label.len() <= 1 => match Url::parse(instance) {
                    Ok(instance_url) => {
                        let account_label = label.first().map(|label| (*label).to_owned());
                        self.execute_profile_command(ProfileCommand::New(ProfileDraft {
                            id: ProfileId::from(*id),
                            instance_url,
                            account_label,
                        }))
                        .await
                    }
                    Err(error) => {
                        self.state
                            .status
                            .failure(format!("invalid instance URL: {error}"));
                        Ok(())
                    }
                },
                _ => {
                    self.state
                        .status
                        .failure("usage: profile-new <id> <instance-url> [account label]");
                    Ok(())
                }
            },
            "profile-delete" | "delete-profile" => match args.as_slice() {
                [id] => {
                    self.execute_profile_command(ProfileCommand::Delete(ProfileId::from(*id)))
                        .await
                }
                _ => {
                    self.state.status.failure("usage: profile-delete <id>");
                    Ok(())
                }
            },
            "login" => self.execute_profile_command(ProfileCommand::Login).await,
            "logout" | "profile-logout" => {
                self.execute_profile_command(ProfileCommand::Logout).await
            }
            "whoami" => self.execute_profile_command(ProfileCommand::WhoAmI).await,
            "help" => {
                self.show_help(if args.is_empty() {
                    None
                } else {
                    Some(args.join(" "))
                });
                Ok(())
            }
            "close" => {
                self.close_top_modal();
                self.close_detail_pane().await
            }
            "set" => self.config_command(&args).await,
            "quit" => {
                self.quit();
                Ok(())
            }
            "feed" if !self.state.view.downloads_active() => {
                self.state.view.feed_query = self.home_query();
                self.state.view.search.clear();
                self.state.view.next_page = None;
                self.refresh_feed().await
            }
            "subscribed" if !self.state.view.downloads_active() => {
                // The subscribed listing needs a session: without one the
                // server has no subscriptions to show. Refuse before any
                // network activity so the user gets a clear reason.
                if self.state.active.session.is_none() {
                    self.state
                        .status
                        .failure("login first to view your subscribed feed");
                    return Ok(());
                }
                let mut query = FeedQuery::subscribed();
                query.sort = self.state.view.feed_sort.clone();
                self.state.view.feed_query = query;
                self.state.view.search.clear();
                self.state.view.next_page = None;
                self.refresh_feed().await
            }
            "search" if !self.state.view.downloads_active() => {
                let query = args.join(" ").trim().to_owned();
                self.state.view.search = query.clone();
                self.state.view.feed_query = FeedQuery {
                    search: (!query.is_empty()).then_some(query),
                    sort: self.state.view.feed_sort.clone(),
                    ..FeedQuery::home()
                };
                self.state.view.next_page = None;
                self.refresh_feed().await
            }
            // Downloads panel routing (Task 10 guards): with the panel open,
            // `:search` filters the download history and `:delete` acts on the
            // selected download, ahead of the top-level post/search arms.
            "search" => {
                self.downloads_action(DownloadsAction::Search(args.join(" ").trim().to_owned()))
                    .await
            }
            "open" if !self.state.view.downloads_active() => self.open_selected().await,
            "open" => self.downloads_action(DownloadsAction::Reopen).await,
            "refresh" if !self.state.view.downloads_active() => self.refresh_feed().await,
            "refresh" => self.downloads_action(DownloadsAction::Retry).await,
            "delete" if !self.state.view.downloads_active() => match self.state.selected_post() {
                Some(id) => self.delete_post(id).await,
                None => {
                    self.state.status.failure("no post selected");
                    Ok(())
                }
            },
            "delete" => self.downloads_action(DownloadsAction::Delete).await,
            // Content commands have no download-panel equivalent; refuse them
            // while the panel is open so they never act on the hidden feed
            // selection.
            "feed" | "media" | "download-media" | "download_media" | "community" | "post"
            | "reply" | "edit" | "vote" | "save" | "subscribe" | "subscribed" | "sort"
            | "communities"
                if self.state.view.downloads_active() =>
            {
                self.state
                    .status
                    .failure("close the downloads panel before using content commands");
                Ok(())
            }
            "communities" if !self.state.view.downloads_active() => {
                self.communities_command().await
            }
            "sort" => {
                if matches!(self.state.view.top_modal(), Some(Modal::Communities(_))) {
                    self.sort_communities_command(&args).await
                } else {
                    self.sort_command(&args).await
                }
            }
            "media" => self.open_media_selected().await,
            "download-media" | "download_media" => self.download_media_selected().await,
            "community" => self.community_command(&args).await,
            "post" => self.open_selected().await,
            "reply" => self.reply_command(&args).await,
            "edit" => self.edit_command(&args).await,
            "vote" => self.vote_command(&args).await,
            "save" => self.save_command().await,
            "subscribe" => self.subscribe_command().await,
            "downloads" => match args.as_slice() {
                [] => {
                    self.toggle_downloads_panel();
                    Ok(())
                }
                ["search", rest @ ..] => {
                    self.downloads_action(DownloadsAction::Search(rest.join(" ").trim().to_owned()))
                        .await
                }
                ["reopen"] => self.downloads_action(DownloadsAction::Reopen).await,
                ["reveal"] => self.downloads_action(DownloadsAction::Reveal).await,
                ["copy"] => self.downloads_action(DownloadsAction::CopyPath).await,
                ["retry"] => self.downloads_action(DownloadsAction::Retry).await,
                ["cancel"] => self.downloads_action(DownloadsAction::Cancel).await,
                ["delete"] => self.downloads_action(DownloadsAction::Delete).await,
                ["overwrite"] => {
                    self.downloads_action(DownloadsAction::ResolveCollision { overwrite: true })
                        .await
                }
                ["keep"] => {
                    self.downloads_action(DownloadsAction::ResolveCollision { overwrite: false })
                        .await
                }
                ["close"] => self.downloads_action(DownloadsAction::Close).await,
                _ => {
                    self.state.status.failure("usage: downloads [search <query>|reopen|reveal|copy|retry|cancel|delete|overwrite|keep|close]");
                    Ok(())
                }
            },
            // Confirmation commands resolve a staged destructive action. They
            // run before the downloads-panel catch-all so confirming a staged
            // download deletion works with the panel open; with no
            // confirmation pending, `:cancel` falls through to the panel's
            // download-cancel when the panel is open.
            "confirm" | "yes" if self.state.status.confirmation_pending => {
                self.confirm_pending().await
            }
            "confirm" | "yes" => {
                self.state.status.failure("nothing to confirm");
                Ok(())
            }
            "cancel" if self.state.status.confirmation_pending || self.state.pending.is_some() => {
                self.cancel_pending();
                Ok(())
            }
            _ if self.state.view.downloads_active() => self.submit_downloads_command(trimmed).await,
            other => {
                self.state
                    .status
                    .failure(format!("unknown command: {other}"));
                Ok(())
            }
        };
        // The compose buffer is transient command input; never leave what was
        // typed on screen after it has been submitted (secrets such as
        // `:login <user> <password>` must not persist).
        self.state.view.compose.clear();
        result
    }

    async fn config_command(&mut self, args: &[&str]) -> Result<()> {
        let mut config = match self.profile_store.load_config() {
            Ok(config) => config,
            Err(error) => {
                self.state.status.failure(error.to_string());
                return Ok(());
            }
        };
        let update = match args {
            ["keymap", name, sequence] => {
                config.set_keymap((*name).to_owned(), (*sequence).to_owned())
            }
            ["media", "mailcap", "on"] => config.set_mailcap(true),
            ["media", "mailcap", "off"] => config.set_mailcap(false),
            ["download-dir", directory] | ["download-directory", directory] => {
                config.set_download_directory(Some(PathBuf::from(*directory)))
            }
            ["collision-policy", policy] => config.set_collision_policy((*policy).to_owned()),
            ["cache-dir", directory] => config.set_cache_directory(Some(PathBuf::from(*directory))),
            ["cache-size", bytes] => match bytes.parse::<u64>() {
                Ok(size) => config.set_cache_size(Some(size)),
                Err(_) => Err(AppError::Configuration(format!(
                    "cache size must be a byte count; got {bytes}"
                ))),
            },
            ["logging", "on"] => config.set_logging(true, None),
            ["logging", "off"] => config.set_logging(false, None),
            ["logging", "on", level] => config.set_logging(true, Some((*level).to_owned())),
            ["logging", "off", level] => config.set_logging(false, Some((*level).to_owned())),
            _ => {
                self.state.status.failure("usage: set keymap <name> <keys> | set media mailcap <on|off> | set download-dir <path> | set collision-policy <prompt|overwrite|unique-name> | set cache-dir <path> | set cache-size <bytes> | set logging <on|off> [level]");
                return Ok(());
            }
        };
        match update {
            Ok(()) => {
                // Validate first (above), then persist atomically, then apply.
                if let Err(error) = self.profile_store.save_config(&config) {
                    self.state.status.failure(error.to_string());
                    return Ok(());
                }
                self.apply_runtime_config(&config);
                self.state.status.success("configuration updated");
            }
            Err(error) => self.state.status.failure(error.to_string()),
        }
        Ok(())
    }

    /// Apply configuration changes that can take effect live; the durable
    /// config was already written atomically. Keymaps, the cache directory
    /// and size limit, and the logging subscriber take effect on the next
    /// launch (the input engine and cache store are opened at startup).
    fn apply_runtime_config(&mut self, config: &AppConfig) {
        self.media_policy = MediaPolicyConfig::from_config(&config.media);
        self.collision_policy = CollisionPolicy::from_config(&config.media.collision_policy);
        if let Some(directory) = &config.media.download_directory {
            self.downloads.set_directory(directory.clone());
        }
    }

    fn show_help(&mut self, query: Option<String>) {
        // Re-help replaces the help modal instead of stacking help on help.
        if matches!(self.state.view.top_modal(), Some(Modal::Help(_))) {
            self.state.view.pop_modal();
        }
        self.state
            .view
            .push_modal(Modal::Help(state::HelpModal::new(
                query.unwrap_or_default(),
            )));
        self.state
            .status
            .success("help: type :help <topic> to filter; Esc closes");
    }

    async fn submit_downloads_command(&mut self, line: &str) -> Result<()> {
        if let Some(query) = line.strip_prefix("search ") {
            return self
                .downloads_action(DownloadsAction::Search(query.trim().to_owned()))
                .await;
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
                self.state
                    .status
                    .failure(format!("unknown download command: {other}"));
                return Ok(());
            }
        };
        self.downloads_action(action).await
    }

    /// `:community [<id>]` — open a community feed; without an id, the
    /// selected post's community is used.
    async fn community_command(&mut self, args: &[&str]) -> Result<()> {
        match args {
            [] => {
                let community = self.state.selected_post().and_then(|id| {
                    self.state
                        .view
                        .posts
                        .iter()
                        .find(|post| post.id == id)
                        .map(|post| post.community_id)
                });
                match community {
                    Some(community) => self.open_community(community).await,
                    None => {
                        self.state
                            .status
                            .failure("no post selected; specify a community id");
                        Ok(())
                    }
                }
            }
            [id] => match id.parse::<i64>() {
                Ok(id) => self.open_community(crate::domain::CommunityId(id)).await,
                Err(_) => {
                    self.state.status.failure("community id must be a number");
                    Ok(())
                }
            },
            _ => {
                self.state.status.failure("usage: community [<id>]");
                Ok(())
            }
        }
    }

    /// `:reply <text>` — comment on the selected post.
    async fn reply_command(&mut self, args: &[&str]) -> Result<()> {
        let content = args.join(" ").trim().to_owned();
        if content.is_empty() {
            self.state.status.failure("usage: reply <text>");
            return Ok(());
        }
        let Some(post) = self.state.selected_post() else {
            self.state.status.failure("no post selected");
            return Ok(());
        };
        self.start_mutation(
            Mutation::CreateComment(CreateCommentRequest { post, content }),
            None,
        )
        .await
    }

    /// `:edit <title>` — retitle the selected post.
    async fn edit_command(&mut self, args: &[&str]) -> Result<()> {
        let title = args.join(" ").trim().to_owned();
        if title.is_empty() {
            self.state.status.failure("usage: edit <title>");
            return Ok(());
        }
        let Some(id) = self.state.selected_post() else {
            self.state.status.failure("no post selected");
            return Ok(());
        };
        self.start_mutation(
            Mutation::EditPost(EditPostRequest {
                id,
                name: Some(title),
                body: None,
                url: None,
            }),
            None,
        )
        .await
    }

    /// `:vote <up|down|clear>` — vote on the selected post. Lemmy only has
    /// up and down votes; the numeric spellings (-1, 0, 1) remain as
    /// aliases.
    async fn vote_command(&mut self, args: &[&str]) -> Result<()> {
        let [score] = args else {
            self.state.status.failure("usage: vote <up|down|clear>");
            return Ok(());
        };
        let score: i8 = match *score {
            "up" | "u" | "1" => 1,
            "down" | "d" | "-1" => -1,
            "clear" | "none" | "remove" | "0" => 0,
            other => {
                self.state
                    .status
                    .failure(format!("invalid vote {other:?}: use up, down, or clear"));
                return Ok(());
            }
        };
        let Some(id) = self.state.selected_post() else {
            self.state.status.failure("no post selected");
            return Ok(());
        };
        self.start_mutation(Mutation::VotePost { id, score }, None)
            .await
    }

    /// `:save` — save the selected post.
    async fn save_command(&mut self) -> Result<()> {
        let Some(id) = self.state.selected_post() else {
            self.state.status.failure("no post selected");
            return Ok(());
        };
        self.start_mutation(Mutation::SavePost { id, saved: true }, None)
            .await
    }

    /// `:subscribe` — subscribe to the selected post's community.
    async fn subscribe_command(&mut self) -> Result<()> {
        let Some(id) = self.state.selected_post() else {
            self.state.status.failure("no post selected");
            return Ok(());
        };
        let Some(community) = self
            .state
            .view
            .posts
            .iter()
            .find(|post| post.id == id)
            .map(|post| post.community_id)
        else {
            self.state.status.failure("no post selected");
            return Ok(());
        };
        self.start_mutation(
            Mutation::Subscribe {
                community,
                subscribed: true,
            },
            None,
        )
        .await
    }

    async fn open_community(&mut self, community: crate::domain::CommunityId) -> Result<()> {
        self.state.view.feed_query = FeedQuery {
            community: Some(community),
            sort: self.state.view.feed_sort.clone(),
            ..FeedQuery::home()
        };
        self.state.view.search.clear();
        self.state.view.next_page = None;
        self.refresh_feed().await
    }

    /// Flip to the next feed page, replacing the current list. The cursor of
    /// the page we leave is remembered so `<` can come back.
    async fn next_page(&mut self) -> Result<()> {
        let Some(cursor) = self.state.view.next_page.clone() else {
            self.state.status.success("no more posts to load");
            return Ok(());
        };
        let context = self.state.active.clone();
        let profile = context.profile.id.clone();
        let previous = self.state.view.feed_query.page.clone();
        self.state
            .view
            .page_history
            .push(self.state.view.feed_query.page.clone());
        let mut query = self.state.view.feed_query.clone();
        query.page = Some(cursor.clone());
        query.limit = Some(self.feed_limit());
        let request = self.begin_request(RequestIdentity::Feed);
        // A cached page paints instantly (with its staleness); the detached
        // fetch below then refreshes it and lands the fresh page.
        if let Some(read) = self.repository.cached_feed(&context, &query).await? {
            self.apply_api_result(ApiResult::FeedPage {
                profile: profile.clone(),
                request: request.clone(),
                result: Ok(read.value),
                stale: read.stale,
                cursor: Some(cursor.clone()),
                previous: previous.clone(),
                direction: PageDirection::Next,
            });
        } else {
            self.state.view.loading = true;
        }
        let repository = self.repository.clone();
        self.detach(RequestIdentity::Feed, async move {
            let result = repository.api.feed(&context, query.clone()).await;
            if let Ok(page) = &result {
                let _ = repository
                    .record_fresh_feed_page(&context, query.clone(), page)
                    .await;
            }
            ApiResult::FeedPage {
                profile,
                request,
                result,
                stale: false,
                cursor: Some(cursor),
                previous,
                direction: PageDirection::Next,
            }
        });
        Ok(())
    }

    /// Flip back to the previous feed page, replacing the current list.
    async fn previous_page(&mut self) -> Result<()> {
        let Some(previous) = self.state.view.page_history.pop() else {
            self.state.status.success("already on the first page");
            return Ok(());
        };
        let context = self.state.active.clone();
        let profile = context.profile.id.clone();
        let mut query = self.state.view.feed_query.clone();
        query.page = previous.clone();
        query.limit = Some(self.feed_limit());
        let request = self.begin_request(RequestIdentity::Feed);
        if let Some(read) = self.repository.cached_feed(&context, &query).await? {
            self.apply_api_result(ApiResult::FeedPage {
                profile: profile.clone(),
                request: request.clone(),
                result: Ok(read.value),
                stale: read.stale,
                cursor: previous.clone(),
                previous: previous.clone(),
                direction: PageDirection::Previous,
            });
        } else {
            self.state.view.loading = true;
        }
        let repository = self.repository.clone();
        self.detach(RequestIdentity::Feed, async move {
            let result = repository.api.feed(&context, query.clone()).await;
            if let Ok(page) = &result {
                let _ = repository
                    .record_fresh_feed_page(&context, query.clone(), page)
                    .await;
            }
            ApiResult::FeedPage {
                profile,
                request,
                result,
                stale: false,
                cursor: previous.clone(),
                previous,
                direction: PageDirection::Previous,
            }
        });
        Ok(())
    }

    /// Replace the visible feed with a fetched page, keeping the selection
    /// on the same post when it is still present (otherwise the first row).
    fn apply_page(&mut self, page: Page<PostView>) {
        let selected_id = self.state.selected_post();
        self.state.view.posts = page.items;
        self.state.view.next_page = page.next_page;
        self.state.view.selected = selected_id
            .and_then(|id| self.state.view.posts.iter().position(|post| post.id == id))
            .or_else(|| (!self.state.view.posts.is_empty()).then_some(0));
    }

    /// Collapse the downloads panel (`Back` with no modal open keeps its
    /// meaning there). Modals are popped by `Back`/`:close` directly.
    async fn close_detail_pane(&mut self) -> Result<()> {
        if self.state.view.downloads_active() {
            self.state.view.close_downloads_panel();
            return Ok(());
        }
        self.cancel_pending();
        Ok(())
    }

    async fn open_selected(&mut self) -> Result<()> {
        let Some(id) = self.state.selected_post() else {
            return Ok(());
        };
        // Push the thread modal up front so the fetch is visible; the post
        // result replaces the placeholder, comments fill it in. Esc pops it
        // at any point.
        self.state
            .view
            .push_modal(Modal::Thread(state::ThreadModal::for_post(id)));
        let context = self.state.active.clone();
        let profile = context.profile.id.clone();
        // The post detail and its thread fetch independently and
        // concurrently; either may land first into the placeholder thread.
        // The post arm keeps any comments that already landed.
        let request = self.begin_request(RequestIdentity::Post(id));
        if let Some(read) = self.repository.cached_post(&context, id).await? {
            self.apply_api_result(ApiResult::Post {
                profile: profile.clone(),
                request: request.clone(),
                result: Ok(read.value),
                stale: read.stale,
            });
        } else {
            self.state.view.loading = true;
        }
        let repository = self.repository.clone();
        let post_context = context.clone();
        let post_profile = profile.clone();
        self.detach(RequestIdentity::Post(id), async move {
            let result = repository.api.post(&post_context, id).await;
            if let Ok(detail) = &result {
                let _ = repository
                    .record_fresh_post(&post_context, id, detail)
                    .await;
            }
            ApiResult::Post {
                profile: post_profile,
                request,
                result,
                stale: false,
            }
        });
        // The thread lives on `comment/list`, not the post detail response;
        // fetch it so the thread modal can render the full thread.
        if self.state.selected_post() == Some(id) {
            let request = self.begin_request(RequestIdentity::Comments(id));
            if let Some(read) = self.repository.cached_comments(&context, id).await? {
                self.apply_api_result(ApiResult::Comments {
                    profile: profile.clone(),
                    request: request.clone(),
                    post: id,
                    result: Ok(read.value),
                    stale: read.stale,
                });
            } else {
                self.state.view.loading = true;
            }
            let repository = self.repository.clone();
            self.detach(RequestIdentity::Comments(id), async move {
                let result = repository.api.comments(&context, id).await;
                if let Ok(comments) = &result {
                    let _ = repository
                        .record_fresh_comments(&context, id, comments)
                        .await;
                }
                ApiResult::Comments {
                    profile,
                    request,
                    post: id,
                    result,
                    stale: false,
                }
            });
        }
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
        // Media opens in an external handler (mailcap or a configured
        // command), so the detail/thread pane must not appear.
        self.open_media(media, None).await
    }

    /// Fetch the media to a scratch file for an external handler, which
    /// cannot open remote URLs. Reuses the session download manager, so the
    /// transfer is cancellable and shows up in the downloads panel; on
    /// failure a status message is set and `None` is returned. Returns
    /// `(path, reused)` where `reused` is true when an existing completed
    /// download of the same URL was found instead of re-fetching it.
    async fn download_for_handler(&mut self, media: &MediaRef) -> Option<(PathBuf, bool)> {
        // The same media is often opened repeatedly while browsing; reuse a
        // completed, not-deleted local copy of this URL (either a handler
        // scratch file or a user download) instead of fetching it again.
        if let Some(existing) = self.downloads.history().all().iter().find(|record| {
            record.media.url == media.url
                && record.status == DownloadStatus::Completed
                && !record.local_file_deleted
                && record.local_path.exists()
        }) {
            return Some((existing.local_path.clone(), true));
        }
        let scratch_directory = match crate::media::ensure_scratch_dir() {
            Ok(directory) => directory,
            Err(error) => {
                self.state.status.failure(error.to_string());
                return None;
            }
        };
        let scratch = scratch_directory.join(filename_for(media));
        let request = DownloadRequest {
            media: media.clone(),
            profile: self.state.active.profile.id.clone(),
            instance_url: self.state.active.profile.instance_url.clone(),
            destination: scratch,
            collision: CollisionPolicy::UniqueName,
        };
        let id = match self.downloads.start(request).await {
            Ok(id) => id,
            Err(error) => {
                self.state.status.failure(error.to_string());
                return None;
            }
        };
        // The file is session-scratch: it is removed when the client exits
        // (and on failure paths via `Drop`), not left in the temp directory.
        self.scratch_downloads.insert(id);
        match tokio::time::timeout(Duration::from_secs(30), self.downloads.wait_for(id)).await {
            Ok(DownloadStatus::Completed) => match self.downloads.history().get(id) {
                Some(record) => Some((record.local_path, false)),
                None => {
                    self.state.status.failure("media download vanished");
                    None
                }
            },
            Ok(status) => {
                self.state
                    .status
                    .failure(format!("media download did not complete ({status})"));
                None
            }
            Err(_) => {
                self.state.status.failure("media download timed out");
                None
            }
        }
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
                let prompting = self
                    .downloads
                    .history()
                    .get(id)
                    .is_some_and(|record| record.status == DownloadStatus::Prompting);
                if prompting {
                    self.state.view.open_downloads_panel();
                    if let Some(panel) = &mut self.state.view.downloads {
                        panel.selected = Some(id);
                    }
                    self.state.status.message =
                        "file already exists; choose :overwrite or :keep in the downloads panel"
                            .into();
                    self.state.status.pending = false;
                    self.state.status.error = None;
                } else {
                    self.state
                        .status
                        .success(format!("download #{} started", id.0));
                }
                Ok(())
            }
            Err(error) => {
                self.state.status.failure(error.to_string());
                Ok(())
            }
        }
    }

    /// Open media through the selected handler. `local` is the downloaded
    /// file when reopening a record; otherwise the media is downloaded to a
    /// scratch file so the handler always receives a local path.
    async fn open_media(&mut self, media: MediaRef, local: Option<PathBuf>) -> Result<()> {
        // Lemmy media URLs often carry no file extension (`image_proxy`
        // rewrites), so the MIME type must come from the resource itself.
        // Probe the Content-Type header, exactly like the download path
        // does, before choosing a handler.
        let mut media = media;
        if media.mime_type.is_none() {
            match crate::media::probe_content_type(&media.url).await {
                Ok(Some(mime)) => media.mime_type = Some(mime),
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(%error, "media MIME probe failed; falling back to filename");
                }
            }
        }
        let handler = self.media_policy.select(&media);
        match handler {
            MediaHandler::Mailcap { command } | MediaHandler::External { command } => {
                if !media.url.username().is_empty() || media.url.password().is_some() {
                    self.state
                        .status
                        .failure("refusing to open a media URL containing credentials");
                    return Ok(());
                }
                // Over plain SSH without X11 forwarding there is no display
                // on the host running the client; a spawned `xdg-open` would
                // fail invisibly. Say so instead of reporting success, and
                // when the session is SSH tell the user the handler runs on
                // the remote host either way.
                let is_ssh = crate::media::environment_is_ssh(
                    std::env::var("SSH_CONNECTION").ok().as_deref(),
                    std::env::var("SSH_CLIENT").ok().as_deref(),
                    std::env::var("SSH_TTY").ok().as_deref(),
                );
                let has_display = crate::media::environment_has_display(
                    std::env::var("DISPLAY").ok().as_deref(),
                    std::env::var("WAYLAND_DISPLAY").ok().as_deref(),
                );
                if !has_display {
                    let hint = if is_ssh {
                        "SSH session without X11 forwarding: no display on this host — external media handlers cannot open windows here; use :download-media and view the file locally"
                    } else {
                        "no display on this host; external media handlers cannot open windows here — use :download-media and view the file locally"
                    };
                    self.state.status.failure(hint);
                    return Ok(());
                }
                let mime = crate::media::resolve_mime(&media, None).unwrap_or_default();
                // Handlers open local files, not URLs (imv/feh/zathura
                // cannot fetch); download the media to a scratch file first
                // unless a local path was already supplied.
                let (source, reused) = match local {
                    Some(path) => (path, false),
                    None => match self.download_for_handler(&media).await {
                        Some((path, reused)) => (path, reused),
                        None => return Ok(()),
                    },
                };
                let cached_note = if reused { " (reused cached file)" } else { "" };
                match spawn_detached(&command, source.as_os_str(), &mime) {
                    Ok(()) if is_ssh => self.state.status.success(format!(
                        "opened media with external handler on this host (SSH session — the handler runs where levim is, not on your local terminal){cached_note}"
                    )),
                    Ok(()) => self
                        .state
                        .status
                        .success(format!("opened media with external handler{cached_note}")),
                    Err(error) => self.state.status.failure(error.to_string()),
                }
                Ok(())
            }
            MediaHandler::Refused { mime } => {
                self.state.status.failure(format!(
                    "refusing to open executable media type {mime} — configure an explicit [media] handlers entry to allow it"
                ));
                Ok(())
            }
            MediaHandler::MetadataOnly => {
                let mime =
                    crate::media::resolve_mime(&media, None).unwrap_or_else(|| "unknown".into());
                self.state
                    .status
                    .success(format!("no media handler for {mime}; metadata only"));
                Ok(())
            }
        }
    }

    fn toggle_downloads_panel(&mut self) {
        if self.state.view.downloads_active() {
            self.state.view.close_downloads_panel();
            return;
        }
        self.state.view.open_downloads_panel();
        let query = self
            .state
            .view
            .downloads
            .as_ref()
            .map(|panel| panel.query.clone())
            .unwrap_or_default();
        let first = self
            .downloads
            .history()
            .filtered(&query)
            .first()
            .map(|record| record.id);
        if let Some(panel) = &mut self.state.view.downloads {
            panel.selected = first;
        }
    }

    async fn downloads_action(&mut self, action: DownloadsAction) -> Result<()> {
        match action {
            DownloadsAction::Search(query) => {
                let mut panel = self.state.view.downloads.clone().unwrap_or_default();
                panel.query = query;
                panel.selected = self
                    .downloads
                    .history()
                    .filtered(&panel.query)
                    .first()
                    .map(|record| record.id);
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
                            self.state
                                .status
                                .failure("local file is missing; retry the download");
                            return Ok(());
                        }
                        self.open_media(record.media.clone(), Some(record.local_path.clone()))
                            .await
                    }
                    DownloadsAction::Reveal => {
                        let Some(record) = self.downloads.history().get(id) else {
                            self.state.status.failure("download not found");
                            return Ok(());
                        };
                        let directory = record
                            .local_path
                            .parent()
                            .map(Path::to_path_buf)
                            .unwrap_or_else(|| PathBuf::from("."));
                        reveal_directory(&directory);
                        self.state
                            .status
                            .success(format!("revealed {}", directory.display()));
                        Ok(())
                    }
                    DownloadsAction::CopyPath => {
                        let Some(record) = self.downloads.history().get(id) else {
                            self.state.status.failure("download not found");
                            return Ok(());
                        };
                        if copy_to_clipboard(&record.local_path.to_string_lossy()) {
                            self.state
                                .status
                                .success("download path copied to clipboard");
                        } else {
                            self.state
                                .status
                                .success(format!("path: {}", record.local_path.display()));
                        }
                        Ok(())
                    }
                    DownloadsAction::Retry => match self.downloads.retry(id).await {
                        Ok(()) => {
                            self.state
                                .status
                                .success(format!("retrying download #{id}"));
                            Ok(())
                        }
                        Err(error) => {
                            self.state.status.failure(error.to_string());
                            Ok(())
                        }
                    },
                    DownloadsAction::Cancel => match self.downloads.cancel(id).await {
                        Ok(()) => {
                            self.state
                                .status
                                .success(format!("download #{id} cancelled"));
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
                        if record.status != DownloadStatus::Completed {
                            // Only completed downloads own their local path.
                            // Prompting records point at the pre-existing
                            // collision target, and cancelled (including
                            // prompt-keep), failed, pending, or downloading
                            // records may point at files the download never
                            // created; deleting any of those could destroy a
                            // user file.
                            self.state
                                .status
                                .failure("only completed downloads may be deleted");
                            return Ok(());
                        }
                        self.state.pending =
                            Some(crate::app::actions::PendingAction::DeleteDownload {
                                id,
                                path: record.local_path.clone(),
                            });
                        self.state.status.pending = false;
                        self.state.status.confirmation_pending = true;
                        self.state.status.message =
                            format!("confirm deletion of {}", record.local_path.display());
                        self.state.status.error = None;
                        Ok(())
                    }
                    DownloadsAction::ResolveCollision { overwrite } => {
                        match self.downloads.resolve_collision(id, overwrite).await {
                            Ok(()) => {
                                self.state.status.success(if overwrite {
                                    "overwriting existing file"
                                } else {
                                    "kept existing file"
                                });
                                Ok(())
                            }
                            Err(error) => {
                                self.state.status.failure(error.to_string());
                                Ok(())
                            }
                        }
                    }
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
                        .map(|record| {
                            format!(
                                "download #{} complete: {}",
                                id.0,
                                record.local_path.display()
                            )
                        })
                        .unwrap_or_else(|| format!("download #{} complete", id.0));
                    self.state.status.success(message);
                }
                DownloadEvent::Failed(id, error) => {
                    self.state
                        .status
                        .failure(format!("download #{} failed: {error}", id.0));
                }
            }
        }
    }

    async fn delete_post(&mut self, id: crate::PostId) -> Result<()> {
        self.state.pending = Some(crate::app::actions::PendingAction::DeletePost {
            profile: self.state.active.profile.id.clone(),
            id,
        });
        self.state.status.pending = false;
        self.state.status.confirmation_pending = true;
        self.state.status.message = format!(
            "confirm deletion of post {} on {}",
            id.0, self.state.status.instance_url
        );
        self.state.status.error = None;
        Ok(())
    }

    async fn start_mutation(
        &mut self,
        mutation: Mutation,
        draft: Option<crate::cache::DraftId>,
    ) -> Result<()> {
        if matches!(
            mutation,
            Mutation::CreatePost(_) | Mutation::DeletePost(_) | Mutation::DeleteComment(_)
        ) {
            self.state.pending = Some(crate::app::actions::PendingAction::Mutation {
                profile: self.state.active.profile.id.clone(),
                mutation,
                draft,
            });
            self.state.status.pending = false;
            self.state.status.confirmation_pending = true;
            self.state.status.message = format!(
                "confirm destructive action on {}",
                self.state.status.instance_url
            );
            return Ok(());
        }
        let profile = self.state.active.profile.id.clone();
        let request = self.begin_request(RequestIdentity::Mutation(mutation.clone()));
        let result = self
            .repository
            .mutate(&self.state.active, mutation.clone())
            .await;
        self.apply_api_result(ApiResult::Mutation {
            profile,
            request,
            draft,
            mutation,
            result: Box::new(result),
        });
        Ok(())
    }

    async fn confirm_pending(&mut self) -> Result<()> {
        let pending = self.state.pending.take();
        match pending {
            Some(crate::app::actions::PendingAction::DeleteDownload { id, path }) => {
                self.state.status.confirmation_pending = false;
                // Re-check ownership at confirmation time: only a Completed
                // record owns its local path. A record parked in Prompting
                // (or cancelled prompt-keep, failed, pending, downloading)
                // does not own its local path, which may be a pre-existing
                // user file the download has not touched.
                if self
                    .downloads
                    .history()
                    .get(id)
                    .is_some_and(|record| record.status != DownloadStatus::Completed)
                {
                    self.state
                        .status
                        .failure("refusing to delete a file the download does not own");
                    return Ok(());
                }
                if std::fs::remove_file(&path).is_ok() {
                    self.downloads.history().mark_file_deleted(id);
                    self.state.status.success("local file deleted");
                } else {
                    self.state
                        .status
                        .failure(format!("could not delete {}", path.display()));
                }
                Ok(())
            }
            Some(crate::app::actions::PendingAction::DeletePost { profile, id }) => {
                self.confirm_mutation(profile, Mutation::DeletePost(id), None)
                    .await
            }
            Some(crate::app::actions::PendingAction::Mutation {
                profile,
                mutation,
                draft,
            }) => self.confirm_mutation(profile, mutation, draft).await,
            None => Ok(()),
        }
    }

    async fn confirm_mutation(
        &mut self,
        profile: ProfileId,
        mutation: Mutation,
        draft: Option<crate::cache::DraftId>,
    ) -> Result<()> {
        if profile != self.state.active.profile.id {
            self.cancel_pending();
            return Ok(());
        }
        self.state.status.confirmation_pending = false;
        self.state.status.pending = true;
        let request = self.begin_request(RequestIdentity::Mutation(mutation.clone()));
        let result = self
            .repository
            .mutate(&self.state.active, mutation.clone())
            .await;
        self.apply_api_result(ApiResult::Mutation {
            profile,
            request,
            draft,
            mutation,
            result: Box::new(result),
        });
        Ok(())
    }

    fn cancel_pending(&mut self) {
        let had_confirmation =
            self.state.pending.is_some() || self.state.status.confirmation_pending;
        self.state.pending = None;
        self.state.status.confirmation_pending = false;
        // Esc during a detached load really cancels it: abort the task and
        // invalidate its token so a straggler result cannot land.
        self.cancel_in_flight();
        self.state.view.loading = false;
        if self.state.status.pending || had_confirmation {
            self.state.status.success("cancelled");
        }
    }
    async fn poll_feed_refresh(&mut self) {
        let Some(request) = self.requests.get(&RequestIdentity::Feed).cloned() else {
            return;
        };
        let context = self.state.active.clone();
        let query = self.state.view.feed_query.clone();
        if let Ok(Some((generation, result))) =
            self.repository.take_completed_feed(&context, &query).await
            && generation == request.generation
        {
            self.apply_api_result(ApiResult::Feed {
                profile: context.profile.id,
                request,
                result: result.map(|read| read.value),
                stale: false,
            });
        }
    }

    async fn submit_draft(&mut self, id: crate::cache::DraftId) -> Result<()> {
        let Some(draft) = self.state.draft(id.clone()).await else {
            return Ok(());
        };
        let selected_post = self
            .state
            .view
            .selected
            .and_then(|index| self.state.view.posts.get(index));
        let selected = selected_post.map(|post| post.id);
        let community = selected_post.map(|post| post.community_id);
        if matches!(draft.operation.as_str(), "create_comment" | "reply") && selected.is_none() {
            self.state
                .status
                .failure("select a post before submitting a comment");
            return Ok(());
        }
        if draft.operation.as_str() == "create_post" && community.is_none() {
            self.state
                .status
                .failure("select a post in the target community before creating a post");
            return Ok(());
        }
        if let Err(error) = self.state.drafts.validate(&draft) {
            self.state.status.failure(error.to_string());
            return Ok(());
        }
        let Some(mutation) = mutation_for_draft(&draft, selected, community) else {
            self.state.status.failure("unsupported draft operation");
            return Ok(());
        };
        self.start_mutation(mutation, Some(id)).await
    }

    fn move_selection(&mut self, delta: isize) {
        if self.state.view.downloads_active() {
            let query = self
                .state
                .view
                .downloads
                .as_ref()
                .map(|panel| panel.query.clone())
                .unwrap_or_default();
            let ids = self
                .downloads
                .history()
                .filtered(&query)
                .into_iter()
                .map(|record| record.id)
                .collect::<Vec<_>>();
            self.state.view.move_download_selection(delta, &ids);
            return;
        }
        if self.state.view.posts.is_empty() {
            return;
        }
        let current = self.state.view.selected.unwrap_or(0) as isize;
        let max = self.state.view.posts.len().saturating_sub(1) as isize;
        self.state.view.selected = Some((current + delta).clamp(0, max) as usize);
    }

    fn apply_api_result(&mut self, result: ApiResult) {
        let (profile, request) = match &result {
            ApiResult::Feed {
                profile, request, ..
            }
            | ApiResult::FeedPage {
                profile, request, ..
            }
            | ApiResult::Post {
                profile, request, ..
            }
            | ApiResult::Mutation {
                profile, request, ..
            }
            | ApiResult::Comments {
                profile, request, ..
            }
            | ApiResult::Communities {
                profile, request, ..
            } => (profile, request),
        };
        if profile != &self.state.active.profile.id
            || self.requests.get(&request.identity) != Some(request)
        {
            return;
        }
        match result {
            ApiResult::Feed { result, stale, .. } => {
                self.state.view.loading = false;
                match result {
                    Ok(page) => {
                        let selected_id = self.state.selected_post();
                        let selected_index = self.state.view.selected.unwrap_or_default();
                        self.state.view.posts = page.items;
                        self.state.view.next_page = page.next_page;
                        self.state.view.selected = selected_id
                            .and_then(|id| {
                                self.state.view.posts.iter().position(|post| post.id == id)
                            })
                            .or_else(|| {
                                (!self.state.view.posts.is_empty()).then_some(
                                    selected_index
                                        .min(self.state.view.posts.len().saturating_sub(1)),
                                )
                            });
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
                }
            }
            ApiResult::FeedPage {
                result,
                stale,
                cursor,
                previous,
                direction,
                ..
            } => {
                self.state.view.loading = false;
                match result {
                    Ok(page) => {
                        // The flip landed: the view's cursor moves to the
                        // fetched page (for a Next flip it was already set
                        // at dispatch; for a Previous flip this is the
                        // older page that was popped off the history).
                        self.state.view.feed_query.page = cursor;
                        self.apply_page(page);
                        self.state.view.stale = stale;
                        self.state.status.stale = stale;
                        if stale {
                            self.state.status.message = "stale feed loaded; refreshing".into();
                            self.state.status.error = None;
                            self.state.status.retryable = false;
                            self.state.status.pending = true;
                        } else {
                            let message = match direction {
                                PageDirection::Next => "next page loaded",
                                PageDirection::Previous => "previous page loaded",
                            };
                            self.state.status.success(message);
                        }
                    }
                    Err(error) => {
                        // The failed flip never happened: restore the cursor
                        // and history state from before the flip.
                        match direction {
                            PageDirection::Next => {
                                self.state.view.page_history.pop();
                                self.state.view.feed_query.page = previous;
                            }
                            PageDirection::Previous => {
                                self.state.view.page_history.push(previous);
                            }
                        }
                        self.state.status.failure(error.to_string());
                    }
                }
            }
            ApiResult::Post {
                request,
                result,
                stale,
                ..
            } => {
                self.state.view.loading = false;
                match result {
                    Ok(detail)
                        if matches!(request.identity, RequestIdentity::Post(id) if id == detail.post.id)
                            && self.state.selected_post() == Some(detail.post.id) =>
                    {
                        // The post result fills the thread modal: it replaces
                        // the placeholder pushed by `open_selected` (or an
                        // older thread when a new post was opened on top of
                        // it). Comments may have landed first (the two
                        // fetches are concurrent): keep them instead of
                        // replacing the thread with the post response's
                        // usually-empty comment list.
                        let post_id = detail.post.id;
                        match self.state.view.top_modal_mut() {
                            Some(Modal::Thread(thread)) => {
                                let existing_comments = thread.post.comments.clone();
                                thread.post = detail;
                                if !existing_comments.is_empty() {
                                    thread.post.comments = existing_comments;
                                }
                                thread.scroll = 0;
                            }
                            _ => {
                                self.state
                                    .view
                                    .push_modal(Modal::Thread(state::ThreadModal::new(detail)));
                            }
                        }
                        self.state.mode = Mode::Normal;
                        self.state.status.stale = stale;
                        // The post and comments fetches are concurrent. When
                        // the comments landed first, their "comments loaded"
                        // message is the thread's completion message; keep it
                        // instead of letting the (second-arriving) post
                        // overwrite it. When comments are still in flight,
                        // report the post load and let comments finish.
                        let comments_still_in_flight = self
                            .in_flight
                            .contains_key(&RequestIdentity::Comments(post_id));
                        if comments_still_in_flight {
                            self.state.status.success("post loaded");
                        }
                    }
                    Ok(_) => {}
                    Err(error) if matches!(request.identity, RequestIdentity::Post(id) if self.state.selected_post() == Some(id)) => {
                        self.state.status.failure(error.to_string())
                    }
                    Err(_) => {}
                }
            }
            ApiResult::Mutation {
                request,
                mutation,
                result,
                draft,
                ..
            } => {
                if request.identity == RequestIdentity::Mutation(mutation.clone()) {
                    match *result {
                        Ok(MutationResult {
                            success: true,
                            post,
                            comment,
                            ..
                        }) => {
                            match mutation {
                                Mutation::DeletePost(id) => {
                                    self.state.view.posts.retain(|candidate| candidate.id != id);
                                    // Deleting the post dismisses its thread.
                                    self.state.view.modals.retain(|modal| {
                                        !matches!(modal, Modal::Thread(thread) if thread.post.post.id == id)
                                    });
                                    self.requests.retain(|identity, _| {
                                        !matches!(
                                            identity,
                                            RequestIdentity::Post(dropped)
                                                | RequestIdentity::Comments(dropped)
                                                if *dropped == id
                                        )
                                    });
                                    self.state.mode = Mode::Normal;
                                    self.state.view.selected =
                                        self.state.view.selected.and_then(|selected| {
                                            if self.state.view.posts.is_empty() {
                                                None
                                            } else {
                                                Some(selected.min(self.state.view.posts.len() - 1))
                                            }
                                        });
                                }
                                Mutation::DeleteComment(id) => {
                                    if let Some(Modal::Thread(thread)) =
                                        self.state.view.top_modal_mut()
                                    {
                                        thread.post.comments.retain(|comment| comment.id != id);
                                    }
                                }
                                _ => {
                                    if let Some(post) = post {
                                        if let Some(existing) = self
                                            .state
                                            .view
                                            .posts
                                            .iter_mut()
                                            .find(|candidate| candidate.id == post.id)
                                        {
                                            *existing = post.clone();
                                        } else if matches!(mutation, Mutation::CreatePost(_)) {
                                            self.state.view.posts.push(post.clone());
                                        }
                                        if let Some(Modal::Thread(thread)) =
                                            self.state.view.top_modal_mut()
                                            && thread.post.post.id == post.id
                                        {
                                            thread.post.post = post;
                                        }
                                    } else if let Some(comment) = comment {
                                        if let Some(Modal::Thread(thread)) =
                                            self.state.view.top_modal_mut()
                                            && let Some(existing) = thread
                                                .post
                                                .comments
                                                .iter_mut()
                                                .find(|item| item.id == comment.id)
                                        {
                                            *existing = comment.clone();
                                        } else if let Some(Modal::Thread(thread)) =
                                            self.state.view.top_modal_mut()
                                        {
                                            thread.post.comments.push(comment);
                                        }
                                    }
                                }
                            }
                            if let Some(id) = draft {
                                self.state.drafts.mark_completed(id);
                            }
                            self.state.status.success("saved");
                        }
                        Ok(_) => self.state.status.failure("mutation was not confirmed"),
                        Err(error) => self.state.status.failure(error.to_string()),
                    }
                }
            }
            ApiResult::Comments {
                request,
                post,
                result,
                stale,
                ..
            } => {
                self.state.view.loading = false;
                if request.identity == RequestIdentity::Comments(post)
                    && matches!(
                        self.state.view.top_modal(),
                        Some(Modal::Thread(thread)) if thread.post.post.id == post
                    )
                {
                    match result {
                        Ok(comments) => {
                            if let Some(Modal::Thread(thread)) = self.state.view.top_modal_mut() {
                                thread.post.comments = comments;
                            }
                            self.state.status.stale = stale;
                            self.state.status.success("comments loaded");
                        }
                        Err(error) => self.state.status.failure(error.to_string()),
                    }
                }
            }
            ApiResult::Communities { result, stale, .. } => {
                self.state.view.loading = false;
                if !matches!(self.state.view.top_modal(), Some(Modal::Communities(_))) {
                    // The picker was closed while the fetch was in flight;
                    // keep the status clean.
                    return;
                }
                match result {
                    Ok(page) => {
                        let modal = self
                            .state
                            .view
                            .top_modal_mut()
                            .and_then(|modal| match modal {
                                Modal::Communities(communities) => Some(communities),
                                _ => None,
                            })
                            .expect("picker presence checked above");
                        modal.communities = page.items;
                        modal.selected = (!modal.communities.is_empty()).then_some(0);
                        let (count, listing) = (modal.communities.len(), modal.listing);
                        self.state.status.stale = stale;
                        self.state.status.success(format!(
                            "{} communities ({count})",
                            Self::listing_label(listing)
                        ));
                    }
                    Err(error) => self.state.status.failure(error.to_string()),
                }
            }
        }
    }
}

impl Drop for App {
    fn drop(&mut self) {
        // Guarantee in-flight downloads are aborted and their temp files
        // removed on every exit path (quit action, error return, or an
        // action task aborted mid-dispatch), not only on the explicit
        // `AppAction::Quit` path. Scratch media files are removed too; a
        // second removal after `quit()` is a harmless no-op (files are
        // already gone and the history was cleared).
        self.remove_scratch_files();
        self.downloads.shutdown();
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

fn mutation_for_draft(
    draft: &Draft,
    selected: Option<crate::PostId>,
    community: Option<crate::CommunityId>,
) -> Option<Mutation> {
    let mut lines = draft.content.lines();
    match draft.operation.as_str() {
        "create_comment" | "reply" => Some(Mutation::CreateComment(CreateCommentRequest {
            post: selected?,
            content: draft.content.clone(),
        })),
        "create_post" => {
            let community = community?;
            let name = lines.next()?.trim().to_owned();
            if name.is_empty() {
                return None;
            }
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
            Some(Mutation::CreatePost(CreatePostRequest {
                community,
                name,
                body: (!body.is_empty()).then_some(body),
                url,
            }))
        }
        "edit_post" => {
            let id = lines.next()?.trim().parse().ok().map(crate::PostId)?;
            let name = lines.next().map(str::to_owned);
            let body = lines.collect::<Vec<_>>().join("\n");
            Some(Mutation::EditPost(EditPostRequest {
                id,
                name,
                body: (!body.is_empty()).then_some(body),
                url: None,
            }))
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
    fn resize_events_are_queued_as_actions() {
        let mut input = crate::input::InputEngine::new();
        let mut queued = VecDeque::new();
        let mut redraw = false;
        assert!(!queue_terminal_event(
            Event::Resize(120, 40),
            &mut input,
            &mut queued,
            &mut redraw
        ));
        assert!(redraw, "a resize must trigger a redraw");
        assert!(
            matches!(
                queued.pop_front(),
                Some(AppAction::Resize {
                    width: 120,
                    height: 40
                })
            ),
            "the resize is queued so the app can re-size feed pages"
        );
    }

    #[test]
    fn queues_text_and_escape_while_action_is_in_flight() {
        let mut input = crate::input::InputEngine::new();
        let mut queued = VecDeque::new();
        let mut redraw = false;
        assert!(!queue_terminal_event(
            Event::Key(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE)),
            &mut input,
            &mut queued,
            &mut redraw
        ));
        assert!(!queue_terminal_event(
            Event::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)),
            &mut input,
            &mut queued,
            &mut redraw
        ));
        assert!(!queue_terminal_event(
            Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            &mut input,
            &mut queued,
            &mut redraw
        ));
        assert!(matches!(
            queued.pop_front(),
            Some(AppAction::Input(Command::EnterInsert))
        ));
        assert!(
            matches!(queued.pop_front(), Some(AppAction::Input(Command::Text(text))) if text == "x")
        );
        assert!(matches!(
            queued.pop_front(),
            Some(AppAction::Input(Command::Back))
        ));
        assert!(queued.is_empty());
    }

    #[tokio::test]
    async fn pending_refresh_snapshot_is_visible_before_action_completes() {
        let context = ProfileContext {
            profile: Profile {
                id: ProfileId::from("fixture"),
                instance_url: Url::parse("http://127.0.0.1/").unwrap(),
                account_label: Some("fixture".into()),
            },
            session: None,
        };
        let app = App::new(
            Arc::new(crate::api::fixtures::fixture_api("feed.json")),
            Arc::new(crate::cache::MemoryCache::default()),
            context,
            Arc::new(crate::profiles::MemoryCredentialStore::default()),
        );
        let action = AppAction::Input(Command::Refresh);
        let mut app = app;
        app.prepare_action(&action);
        let model = app.render_model();
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

        app.dispatch(AppAction::DeletePost(crate::PostId(1)))
            .await
            .unwrap();
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
            result: Ok(crate::api::Page {
                items: Vec::new(),
                next_page: None,
            }),
            stale: true,
        });
        assert!(app.render_model().status.pending);
    }
    #[tokio::test]
    async fn empty_feed_page_leaves_selection_none() {
        let context = ProfileContext {
            profile: Profile {
                id: ProfileId::from("fixture"),
                instance_url: Url::parse("http://127.0.0.1/").unwrap(),
                account_label: Some("fixture".into()),
            },
            session: None,
        };
        let mut app = App::new(
            Arc::new(crate::api::fixtures::fixture_api("feed.json")),
            Arc::new(crate::cache::MemoryCache::default()),
            context,
            Arc::new(crate::profiles::MemoryCredentialStore::default()),
        );
        app.state.view.posts = vec![
            crate::api::PostView {
                id: crate::PostId(1),
                title: "one".into(),
                body: None,
                url: None,
                community_id: crate::CommunityId(1),
                creator_id: crate::UserId(1),
                score: 0,
                comments: 0,
                published: None,
            },
            crate::api::PostView {
                id: crate::PostId(2),
                title: "two".into(),
                body: None,
                url: None,
                community_id: crate::CommunityId(1),
                creator_id: crate::UserId(1),
                score: 0,
                comments: 0,
                published: None,
            },
        ];
        app.state.view.selected = Some(1);
        let request = app.begin_request(RequestIdentity::Feed);
        app.apply_api_result(ApiResult::Feed {
            profile: ProfileId::from("fixture"),
            request,
            result: Ok(crate::api::Page {
                items: Vec::new(),
                next_page: None,
            }),
            stale: false,
        });
        assert!(app.state.view.posts.is_empty());
        assert!(app.state.view.selected.is_none());
    }
    #[tokio::test]
    async fn detached_refresh_error_clears_pending_and_is_retryable() {
        let context = ProfileContext {
            profile: Profile {
                id: ProfileId::from("fixture"),
                instance_url: Url::parse("http://127.0.0.1/").unwrap(),
                account_label: Some("fixture".into()),
            },
            session: None,
        };
        let cache = Arc::new(MemoryCache::default());
        cache
            .write_feed(
                &context.profile.id,
                &FeedKey::from("home"),
                &CachedFeed::new(json!({ "items": [], "next_page": null }), 1, false),
            )
            .unwrap();
        let mut app = App::new(
            Arc::new(crate::api::fixtures::fixture_api_with_status(
                "/api/v3/post/list",
                500,
            )),
            cache,
            context,
            Arc::new(crate::profiles::MemoryCredentialStore::default()),
        );
        app.dispatch(AppAction::Input(Command::Refresh))
            .await
            .unwrap();
        assert!(app.state.status.pending);
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                app.dispatch(AppAction::Tick).await.unwrap();
                if !app.state.status.pending {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        assert!(app.state.status.error.is_some());
        assert!(app.state.status.retryable);
    }
    #[test]
    fn quit_key_routes_through_quit_action() {
        let mut input = crate::input::InputEngine::new();
        let mut queued = VecDeque::new();
        let mut redraw = false;
        // The `q` key must be queued as `AppAction::Quit` (so dispatch runs
        // DownloadManager::shutdown and clears session history) instead of
        // exiting the loop before the action is ever dispatched.
        let quit = queue_terminal_event(
            Event::Key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE)),
            &mut input,
            &mut queued,
            &mut redraw,
        );
        assert!(
            !quit,
            "quit key must not short-circuit before AppAction::Quit is dispatched"
        );
        assert!(matches!(queued.pop_front(), Some(AppAction::Quit)));
        assert!(queued.is_empty());
    }
    #[tokio::test]
    async fn quit_action_shuts_down_downloads_and_clears_history() {
        let context = ProfileContext {
            profile: Profile {
                id: ProfileId::from("fixture"),
                instance_url: Url::parse("http://127.0.0.1/").unwrap(),
                account_label: Some("fixture".into()),
            },
            session: None,
        };
        let mut app = App::new(
            Arc::new(crate::api::fixtures::fixture_api("feed.json")),
            Arc::new(crate::cache::MemoryCache::default()),
            context,
            Arc::new(crate::profiles::MemoryCredentialStore::default()),
        );
        app.downloads
            .history()
            .insert(crate::domain::DownloadRecord::new(
                crate::domain::DownloadId(7),
                MediaRef::new(Url::parse("https://example.com/photo.png").unwrap()),
                "photo.png",
                ProfileId::from("fixture"),
                Url::parse("http://127.0.0.1/").unwrap(),
                1,
                PathBuf::from("/tmp/levim-quit-test-photo.png"),
            ));
        assert!(!app.downloads.history().is_empty());

        app.dispatch(AppAction::Quit).await.unwrap();
        assert!(app.is_quit());
        assert!(
            app.downloads.history().is_empty(),
            "quit must clear the in-memory session history"
        );
    }
    fn record_with_status(
        id: crate::domain::DownloadId,
        destination: PathBuf,
        status: DownloadStatus,
    ) -> crate::domain::DownloadRecord {
        let mut record = crate::domain::DownloadRecord::new(
            id,
            MediaRef::new(Url::parse("https://example.com/notes.txt").unwrap()),
            "notes.txt",
            ProfileId::from("fixture"),
            Url::parse("http://127.0.0.1/").unwrap(),
            1,
            destination,
        );
        record.status = status;
        record
    }

    #[tokio::test]
    async fn delete_is_refused_while_collision_prompt_is_pending() {
        let context = ProfileContext {
            profile: Profile {
                id: ProfileId::from("fixture"),
                instance_url: Url::parse("http://127.0.0.1/").unwrap(),
                account_label: Some("fixture".into()),
            },
            session: None,
        };
        let mut app = App::new(
            Arc::new(crate::api::fixtures::fixture_api("feed.json")),
            Arc::new(crate::cache::MemoryCache::default()),
            context,
            Arc::new(crate::profiles::MemoryCredentialStore::default()),
        );
        let directory = std::env::temp_dir().join(format!(
            "levim-delete-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::create_dir_all(&directory);
        let destination = directory.join("notes.txt");
        std::fs::write(&destination, b"pre-existing user file").unwrap();

        let id = crate::domain::DownloadId(7);
        app.downloads
            .history()
            .insert(crate::domain::DownloadRecord::new(
                id,
                MediaRef::new(Url::parse("https://example.com/notes.txt").unwrap()),
                "notes.txt",
                ProfileId::from("fixture"),
                Url::parse("http://127.0.0.1/").unwrap(),
                1,
                destination.clone(),
            ));
        app.downloads
            .history()
            .transition(id, |_| DownloadStatus::Prompting);
        app.state.view.downloads = Some(DownloadsPanel {
            query: String::new(),
            selected: Some(id),
        });

        // `:delete` on a Prompting record must not stage a confirmation: the
        // local path is the pre-existing collision target the download does
        // not own.
        app.dispatch(AppAction::Downloads(DownloadsAction::Delete))
            .await
            .unwrap();
        assert!(
            app.state.pending.is_none(),
            "no deletion may be staged for a prompting download"
        );
        assert!(
            app.state.status.error.is_some(),
            "refusal must be surfaced to the user"
        );
        assert!(
            destination.exists(),
            "the pre-existing collision target must survive"
        );

        // A Completed record may be staged...
        app.downloads
            .history()
            .transition(id, |_| DownloadStatus::Completed);
        app.dispatch(AppAction::Downloads(DownloadsAction::Delete))
            .await
            .unwrap();
        assert!(
            app.state.pending.is_some(),
            "a completed download may be staged for deletion"
        );

        // ...but the confirmation-time re-check is the second line of
        // defense: a staged deletion is refused when the record is not
        // Completed at confirm time. `transition` is write-once for terminal
        // statuses, so re-insert the record in Prompting (the state a retry
        // legitimately parks it in when the collision policy prompts).
        app.downloads.history().insert(record_with_status(
            id,
            destination.clone(),
            DownloadStatus::Prompting,
        ));
        app.dispatch(AppAction::Confirm).await.unwrap();
        assert!(
            destination.exists(),
            "confirm must refuse to remove the pre-existing collision target"
        );
    }

    #[tokio::test]
    async fn delete_is_refused_for_cancelled_prompt_keep_record() {
        let context = ProfileContext {
            profile: Profile {
                id: ProfileId::from("fixture"),
                instance_url: Url::parse("http://127.0.0.1/").unwrap(),
                account_label: Some("fixture".into()),
            },
            session: None,
        };
        let mut app = App::new(
            Arc::new(crate::api::fixtures::fixture_api("feed.json")),
            Arc::new(crate::cache::MemoryCache::default()),
            context,
            Arc::new(crate::profiles::MemoryCredentialStore::default()),
        );
        let directory = std::env::temp_dir().join(format!(
            "levim-delete-keep-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::create_dir_all(&directory);
        let destination = directory.join("notes.txt");
        std::fs::write(&destination, b"pre-existing user file").unwrap();

        let id = crate::domain::DownloadId(7);
        app.downloads
            .history()
            .insert(crate::domain::DownloadRecord::new(
                id,
                MediaRef::new(Url::parse("https://example.com/notes.txt").unwrap()),
                "notes.txt",
                ProfileId::from("fixture"),
                Url::parse("http://127.0.0.1/").unwrap(),
                1,
                destination.clone(),
            ));
        // A prompt-keep decision parks the record in `Cancelled`; its local
        // path is the pre-existing collision target the download never
        // created, so `:delete` must refuse to touch it.
        app.downloads
            .history()
            .transition(id, |_| DownloadStatus::Cancelled);
        app.state.view.downloads = Some(DownloadsPanel {
            query: String::new(),
            selected: Some(id),
        });

        app.dispatch(AppAction::Downloads(DownloadsAction::Delete))
            .await
            .unwrap();
        assert!(
            app.state.pending.is_none(),
            "no deletion may be staged for a cancelled prompt-keep record"
        );
        assert!(
            app.state.status.error.is_some(),
            "refusal must be surfaced to the user"
        );
        assert!(
            destination.exists(),
            "the pre-existing collision target must survive"
        );

        // The confirmation-time re-check covers Cancelled too: re-insert as
        // Completed, stage the deletion, then park the record back in
        // Cancelled before confirming; the file must survive.
        app.downloads.history().insert(record_with_status(
            id,
            destination.clone(),
            DownloadStatus::Completed,
        ));
        app.dispatch(AppAction::Downloads(DownloadsAction::Delete))
            .await
            .unwrap();
        assert!(
            app.state.pending.is_some(),
            "a completed download may be staged for deletion"
        );
        app.downloads.history().insert(record_with_status(
            id,
            destination.clone(),
            DownloadStatus::Cancelled,
        ));
        app.dispatch(AppAction::Confirm).await.unwrap();
        assert!(
            destination.exists(),
            "confirm must refuse to remove the pre-existing collision target"
        );
    }

    fn completed_download_app() -> App {
        let context = ProfileContext {
            profile: Profile {
                id: ProfileId::from("fixture"),
                instance_url: Url::parse("http://127.0.0.1/").unwrap(),
                account_label: Some("fixture".into()),
            },
            session: None,
        };
        App::new(
            Arc::new(crate::api::fixtures::fixture_api("feed.json")),
            Arc::new(crate::cache::MemoryCache::default()),
            context,
            Arc::new(crate::profiles::MemoryCredentialStore::default()),
        )
    }

    #[tokio::test]
    async fn downloads_panel_routes_delete_and_search_before_feed_arms() {
        let mut app = completed_download_app();
        let id = crate::domain::DownloadId(7);
        app.downloads
            .history()
            .insert(crate::domain::DownloadRecord::new(
                id,
                MediaRef::new(Url::parse("https://example.com/notes.txt").unwrap()),
                "notes.txt",
                ProfileId::from("fixture"),
                Url::parse("http://127.0.0.1/").unwrap(),
                1,
                PathBuf::from("/tmp/levim-routing-notes.txt"),
            ));
        app.downloads
            .history()
            .transition(id, |_| DownloadStatus::Completed);
        app.state.view.downloads = Some(DownloadsPanel {
            query: String::new(),
            selected: Some(id),
        });
        // A selected post so the top-level `:search`/`:delete` arms would have
        // a feed target if they were (wrongly) reached.
        app.state.view.posts = vec![crate::api::PostView {
            id: crate::PostId(1),
            title: "one".into(),
            body: None,
            url: None,
            community_id: crate::CommunityId(1),
            creator_id: crate::UserId(1),
            score: 0,
            comments: 0,
            published: None,
        }];
        app.state.view.selected = Some(0);
        app.state.view.search = "feed-search".into();

        // `:search` must filter the download history, not the feed.
        app.dispatch(AppAction::Input(Command::SubmitLine("search notes".into())))
            .await
            .unwrap();
        assert_eq!(
            app.state
                .view
                .downloads
                .as_ref()
                .map(|panel| panel.query.as_str()),
            Some("notes"),
            "panel search must filter the download history"
        );
        assert_eq!(
            app.state.view.search, "feed-search",
            "panel search must not touch the feed search"
        );
        assert!(
            app.state.view.feed_query.search.is_none(),
            "panel search must not change the feed query"
        );

        // `:delete` must stage a download deletion, not a post deletion.
        app.state.pending = None;
        app.state.view.downloads = Some(DownloadsPanel {
            query: String::new(),
            selected: Some(id),
        });
        app.dispatch(AppAction::Input(Command::SubmitLine("delete".into())))
            .await
            .unwrap();
        assert!(
            matches!(&app.state.pending, Some(crate::app::actions::PendingAction::DeleteDownload { id: staged, .. }) if *staged == id),
            "panel delete must target the selected download, got {:?}",
            app.state.pending
        );
    }

    #[tokio::test]
    async fn downloads_subcommands_dispatch_panel_actions_and_bare_toggles() {
        let mut app = completed_download_app();
        let id = crate::domain::DownloadId(7);
        app.downloads
            .history()
            .insert(crate::domain::DownloadRecord::new(
                id,
                MediaRef::new(Url::parse("https://example.com/notes.txt").unwrap()),
                "notes.txt",
                ProfileId::from("fixture"),
                Url::parse("http://127.0.0.1/").unwrap(),
                1,
                PathBuf::from("/tmp/levim-subcommand-notes.txt"),
            ));
        app.downloads
            .history()
            .transition(id, |_| DownloadStatus::Completed);

        // `:downloads search <query>` opens the panel with the filter applied.
        assert!(!app.state.view.downloads_active());
        app.dispatch(AppAction::Input(Command::SubmitLine(
            "downloads search notes".into(),
        )))
        .await
        .unwrap();
        assert!(
            app.state.view.downloads_active(),
            "documented `:downloads search <query>` must open the panel"
        );
        assert_eq!(
            app.state
                .view
                .downloads
                .as_ref()
                .map(|panel| panel.query.as_str()),
            Some("notes")
        );

        // `:downloads delete` acts on the selected completed record.
        app.state.view.downloads.as_mut().unwrap().selected = Some(id);
        app.dispatch(AppAction::Input(Command::SubmitLine(
            "downloads delete".into(),
        )))
        .await
        .unwrap();
        assert!(
            matches!(&app.state.pending, Some(crate::app::actions::PendingAction::DeleteDownload { id: staged, .. }) if *staged == id),
            "`:downloads delete` must stage the selected download deletion"
        );

        // Bare `:downloads` toggles the panel closed.
        app.dispatch(AppAction::Input(Command::SubmitLine("downloads".into())))
            .await
            .unwrap();
        assert!(
            !app.state.view.downloads_active(),
            "bare `:downloads` must toggle the panel closed"
        );
    }
}
