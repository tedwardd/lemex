use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

use parking_lot::Mutex;
use reqwest::header::CONTENT_TYPE;
use tokio::sync::{oneshot, watch};

use crate::{
    domain::{DownloadId, DownloadRecord, DownloadStatus, MediaRef, ProfileId},
    error::{AppError, Result},
};

use super::{
    mailcap::{is_temporary_name, remove_temporary, temporary_path},
    mime::{extension_for_mime, resolve_mime},
};

/// How to handle a download whose target file already exists.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CollisionPolicy {
    /// Ask the user before overwriting; the download waits in `Prompting`.
    #[default]
    Prompt,
    /// Replace the existing file.
    Overwrite,
    /// Pick a non-conflicting name by appending a numeric suffix.
    UniqueName,
}

impl CollisionPolicy {
    pub fn from_config(value: &str) -> Self {
        match value.trim() {
            "overwrite" => CollisionPolicy::Overwrite,
            "unique-name" | "unique_name" | "uniquename" => CollisionPolicy::UniqueName,
            _ => CollisionPolicy::Prompt,
        }
    }
}

/// A single download request. The client never sends authorization headers
/// when fetching media, and downloaded content is treated as untrusted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DownloadRequest {
    pub media: MediaRef,
    pub profile: ProfileId,
    pub instance_url: url::Url,
    /// Requested final path (before collision policy is applied).
    pub destination: PathBuf,
    pub collision: CollisionPolicy,
}

/// Terminal state transitions surfaced to the application layer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DownloadEvent {
    Completed(DownloadId),
    Failed(DownloadId, String),
}

/// Current-session download history. In-memory only; cleared on exit.
/// Every mutation bumps a watch channel so `wait_for` and the UI can observe
/// progress without polling.
#[derive(Clone)]
pub struct SessionDownloadHistory {
    inner: Arc<HistoryShared>,
}

struct HistoryShared {
    state: Mutex<HistoryInner>,
    version: Mutex<u64>,
    watchers: watch::Sender<u64>,
}

#[derive(Debug, Default)]
struct HistoryInner {
    records: HashMap<DownloadId, DownloadRecord>,
    order: Vec<DownloadId>,
}

impl Default for SessionDownloadHistory {
    fn default() -> Self {
        let (watchers, _) = watch::channel(0u64);
        Self {
            inner: Arc::new(HistoryShared {
                state: Mutex::new(HistoryInner::default()),
                version: Mutex::new(0),
                watchers,
            }),
        }
    }
}

impl SessionDownloadHistory {
    fn notify(&self) {
        let mut version = self.inner.version.lock();
        *version = version.saturating_add(1);
        let _ = self.inner.watchers.send(*version);
    }

    pub(crate) fn subscribe(&self) -> watch::Receiver<u64> {
        self.inner.watchers.subscribe()
    }

    pub fn get(&self, id: DownloadId) -> Option<DownloadRecord> {
        self.inner.state.lock().records.get(&id).cloned()
    }

    /// All records in insertion order.
    pub fn all(&self) -> Vec<DownloadRecord> {
        let inner = self.inner.state.lock();
        inner.order.iter().filter_map(|id| inner.records.get(id).cloned()).collect()
    }

    /// Records matching a query, newest first. Empty query returns everything.
    pub fn filtered(&self, query: &str) -> Vec<DownloadRecord> {
        let query = query.trim().to_lowercase();
        let inner = self.inner.state.lock();
        let mut records: Vec<DownloadRecord> = inner
            .order
            .iter()
            .rev()
            .filter_map(|id| inner.records.get(id).cloned())
            .collect();
        if !query.is_empty() {
            records.retain(|record| record_matches(record, &query));
        }
        records
    }

    pub fn len(&self) -> usize {
        self.inner.state.lock().records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.state.lock().records.is_empty()
    }

    pub fn clear(&self) {
        self.inner.state.lock().records.clear();
        self.inner.state.lock().order.clear();
        self.notify();
    }

    pub(crate) fn insert(&self, record: DownloadRecord) {
        let mut inner = self.inner.state.lock();
        inner.order.push(record.id);
        inner.records.insert(record.id, record);
        drop(inner);
        self.notify();
    }

    /// Apply a status update unless the record is already in a terminal state.
    pub(crate) fn transition(
        &self,
        id: DownloadId,
        next: impl FnOnce(&DownloadRecord) -> DownloadStatus,
    ) -> bool {
        let mut inner = self.inner.state.lock();
        let Some(record) = inner.records.get_mut(&id) else { return false; };
        if record.status.is_terminal() {
            return false;
        }
        record.status = next(record);
        drop(inner);
        self.notify();
        true
    }

    pub(crate) fn set_mime(&self, id: DownloadId, mime: Option<String>) {
        let mut changed = false;
        if let Some(record) = self.inner.state.lock().records.get_mut(&id) {
            if record.mime_type != mime {
                record.mime_type = mime;
                changed = true;
            }
        }
        if changed {
            self.notify();
        }
    }

    pub(crate) fn set_path(&self, id: DownloadId, path: PathBuf) {
        let mut changed = false;
        if let Some(record) = self.inner.state.lock().records.get_mut(&id) {
            if record.local_path != path {
                record.local_path = path;
                changed = true;
            }
        }
        if changed {
            self.notify();
        }
    }

    pub(crate) fn reset(&self, id: DownloadId, request: &DownloadRequest) {
        let mut inner = self.inner.state.lock();
        let Some(record) = inner.records.get_mut(&id) else { return; };
        record.media = request.media.clone();
        record.filename = filename_for(&request.media);
        record.mime_type = resolve_mime(&request.media, None);
        record.profile = request.profile.clone();
        record.instance_url = request.instance_url.clone();
        record.requested_at = unix_now();
        record.local_path = request.destination.clone();
        record.status = DownloadStatus::Pending;
        record.local_file_deleted = false;
        drop(inner);
        self.notify();
    }

    pub fn mark_file_deleted(&self, id: DownloadId) {
        let mut changed = false;
        if let Some(record) = self.inner.state.lock().records.get_mut(&id) {
            if !record.local_file_deleted {
                record.local_file_deleted = true;
                changed = true;
            }
        }
        if changed {
            self.notify();
        }
    }
}

fn record_matches(record: &DownloadRecord, query: &str) -> bool {
    record.filename.to_lowercase().contains(query)
        || record.media.url.as_str().to_lowercase().contains(query)
        || record.mime_type.as_deref().unwrap_or_default().to_lowercase().contains(query)
        || record.status.to_string().contains(query)
        || record.profile.0.to_lowercase().contains(query)
        || record.instance_url.as_str().to_lowercase().contains(query)
}

#[derive(Clone)]
pub struct DownloadManager {
    inner: Arc<DownloadManagerInner>,
}

struct DownloadManagerInner {
    directory: Mutex<PathBuf>,
    client: reqwest::Client,
    history: SessionDownloadHistory,
    requests: Mutex<HashMap<DownloadId, DownloadRequest>>,
    cancel_flags: Mutex<HashMap<DownloadId, Arc<AtomicBool>>>,
    tasks: Mutex<HashMap<DownloadId, tokio::task::JoinHandle<()>>>,
    prompts: Mutex<HashMap<DownloadId, oneshot::Sender<bool>>>,
    events: Mutex<Vec<DownloadEvent>>,
    next_id: AtomicU64,
}

impl DownloadManager {
    /// Create a manager rooted at `directory`, cleaning up stale temporary
    /// files left by interrupted sessions.
    pub fn new(directory: PathBuf) -> Self {
        let _ = fs::create_dir_all(&directory);
        cleanup_stale_temporaries(&directory);
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            inner: Arc::new(DownloadManagerInner {
                directory: Mutex::new(directory),
                client,
                history: SessionDownloadHistory::default(),
                requests: Mutex::new(HashMap::new()),
                cancel_flags: Mutex::new(HashMap::new()),
                tasks: Mutex::new(HashMap::new()),
                prompts: Mutex::new(HashMap::new()),
                events: Mutex::new(Vec::new()),
                next_id: AtomicU64::new(1),
            }),
        }
    }

    pub fn directory(&self) -> PathBuf {
        self.inner.directory.lock().clone()
    }

    /// Point future downloads at a new root directory. In-flight downloads
    /// keep their already-resolved destinations; the directory is created if
    /// missing.
    pub fn set_directory(&self, directory: PathBuf) {
        let _ = fs::create_dir_all(&directory);
        *self.inner.directory.lock() = directory;
    }

    pub fn history(&self) -> &SessionDownloadHistory {
        &self.inner.history
    }

    /// Start an asynchronous download and return its id immediately.
    pub async fn start(&self, request: DownloadRequest) -> Result<DownloadId> {
        validate_request(&request)?;
        let id = DownloadId(self.inner.next_id.fetch_add(1, Ordering::Relaxed));
        let mut needs_prompt = false;
        let target = match request.collision {
            CollisionPolicy::Overwrite => request.destination.clone(),
            CollisionPolicy::UniqueName => uniquify(&request.destination),
            CollisionPolicy::Prompt => {
                if request.destination.exists() {
                    needs_prompt = true;
                }
                request.destination.clone()
            }
        };
        let record = DownloadRecord::new(
            id,
            request.media.clone(),
            filename_for(&request.media),
            request.profile.clone(),
            request.instance_url.clone(),
            unix_now(),
            target.clone(),
        );
        let prompt_receiver = if needs_prompt {
            let (sender, receiver) = oneshot::channel();
            self.inner.prompts.lock().insert(id, sender);
            Some(receiver)
        } else {
            None
        };
        self.inner.history.insert(DownloadRecord {
            status: if needs_prompt { DownloadStatus::Prompting } else { DownloadStatus::Pending },
            ..record
        });
        self.inner.requests.lock().insert(id, request.clone());
        let flag = Arc::new(AtomicBool::new(false));
        self.inner.cancel_flags.lock().insert(id, flag.clone());
        self.spawn_task(id, request, target, needs_prompt, flag, prompt_receiver);
        Ok(id)
    }

    /// Cancel an in-flight download. Completed/failed downloads reject the call.
    pub async fn cancel(&self, id: DownloadId) -> Result<()> {
        let record = self.history().get(id).ok_or_else(|| AppError::Media(format!("download {id} not found")))?;
        if record.status == DownloadStatus::Completed || matches!(record.status, DownloadStatus::Failed(_)) {
            return Err(AppError::Media(format!("download {id} is already {status}", status = record.status)));
        }
        if let Some(flag) = self.inner.cancel_flags.lock().get(&id) {
            flag.store(true, Ordering::SeqCst);
        }
        let _ = self.inner.prompts.lock().remove(&id);
        if let Some(handle) = self.inner.tasks.lock().get(&id) {
            handle.abort();
        }
        self.history().transition(id, |_| DownloadStatus::Cancelled);
        remove_temporary(&record.local_path, id);
        Ok(())
    }

    /// Resolve a pending collision prompt (policy "prompt") with a decision.
    pub async fn resolve_collision(&self, id: DownloadId, overwrite: bool) -> Result<()> {
        let sender = self
            .inner
            .prompts
            .lock()
            .remove(&id)
            .ok_or_else(|| AppError::Media(format!("download {id} has no pending collision prompt")))?;
        let _ = sender.send(overwrite);
        Ok(())
    }

    /// Re-run a previous download using its stored request, reusing the id so
    /// the history position is stable.
    pub async fn retry(&self, id: DownloadId) -> Result<()> {
        let request = self
            .inner
            .requests
            .lock()
            .get(&id)
            .cloned()
            .ok_or_else(|| AppError::Media(format!("download {id} not found")))?;
        if let Some(handle) = self.inner.tasks.lock().remove(&id) {
            handle.abort();
        }
        if let Some(flag) = self.inner.cancel_flags.lock().remove(&id) {
            flag.store(true, Ordering::SeqCst);
        }
        // Aborting the previous attempt drops its task without running the
        // cleanup inside `run_download`, so a `.part-{id}` temp file may
        // still sit at the (target, id) path this retry will reuse. Remove it
        // before spawning or `open_restrictive` (create_new) fails with EEXIST.
        let previous_path = self.history().get(id).map(|record| record.local_path);
        self.history().reset(id, &request);
        let flag = Arc::new(AtomicBool::new(false));
        self.inner.cancel_flags.lock().insert(id, flag.clone());
        let mut needs_prompt = false;
        let target = match request.collision {
            CollisionPolicy::Overwrite => request.destination.clone(),
            CollisionPolicy::UniqueName => uniquify(&request.destination),
            CollisionPolicy::Prompt => {
                if request.destination.exists() {
                    needs_prompt = true;
                }
                request.destination.clone()
            }
        };
        if let Some(previous_path) = previous_path {
            remove_temporary(&previous_path, id);
        }
        remove_temporary(&target, id);
        // Mirror `start`: a collision prompt must be observable as `Prompting`
        // synchronously, before the retried task is spawned.
        if needs_prompt {
            self.history().transition(id, |_| DownloadStatus::Prompting);
        }
        let prompt_receiver = if needs_prompt {
            let (sender, receiver) = oneshot::channel();
            self.inner.prompts.lock().insert(id, sender);
            Some(receiver)
        } else {
            None
        };
        self.spawn_task(id, request, target, needs_prompt, flag, prompt_receiver);
        Ok(())
    }

    /// Wait until the download reaches a terminal status.
    pub async fn wait_for(&self, id: DownloadId) -> DownloadStatus {
        let mut receiver = self.history().subscribe();
        loop {
            let status = self.history().get(id).map(|record| record.status).unwrap_or(DownloadStatus::Cancelled);
            if status.is_terminal() {
                return status;
            }
            if receiver.changed().await.is_err() {
                return status;
            }
        }
    }

    /// Drain events that completed or failed since the last poll.
    pub fn take_events(&self) -> Vec<DownloadEvent> {
        std::mem::take(&mut *self.inner.events.lock())
    }

    /// Abort every in-flight download and remove its temporary file.
    pub fn shutdown(&self) {
        let tasks = std::mem::take(&mut *self.inner.tasks.lock());
        for (_, handle) in tasks {
            handle.abort();
        }
        let flags = std::mem::take(&mut *self.inner.cancel_flags.lock());
        for (_, flag) in flags {
            flag.store(true, Ordering::SeqCst);
        }
        let _ = std::mem::take(&mut *self.inner.prompts.lock());
        let records = self.history().all();
        for record in records {
            if !record.status.is_terminal() {
                remove_temporary(&record.local_path, record.id);
            }
        }
        self.history().clear();
    }

    fn spawn_task(
        &self,
        id: DownloadId,
        request: DownloadRequest,
        target: PathBuf,
        needs_prompt: bool,
        flag: Arc<AtomicBool>,
        prompt_receiver: Option<oneshot::Receiver<bool>>,
    ) {
        let inner = self.inner.clone();
        let handle = tokio::spawn(async move {
            run_download(inner, id, request, target, needs_prompt, flag, prompt_receiver).await;
        });
        self.inner.tasks.lock().insert(id, handle);
    }

}

fn validate_request(request: &DownloadRequest) -> Result<()> {
    let url = &request.media.url;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(AppError::Media(format!("unsupported download scheme: {}", url.scheme())));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(AppError::Media("refusing to download a URL containing embedded credentials".into()));
    }
    if url.host_str().is_none() {
        return Err(AppError::Media("download URL must include a host".into()));
    }
    Ok(())
}

/// Derive a safe requested filename from the media URL, adding an extension
/// from the resolved MIME type when the name has none.
pub fn filename_for(media: &MediaRef) -> String {
    let candidate = media
        .url
        .path_segments()
        .and_then(|segments| segments.last())
        .filter(|segment| !segment.is_empty() && *segment != "." && *segment != "..");
    let mut name = candidate
        .map(|segment| sanitize_name(segment))
        .unwrap_or_else(|| "download".to_owned());
    if !name.contains('.') {
        if let Some(mime) = resolve_mime(media, None) {
            if let Some(extension) = extension_for_mime(&mime) {
                name.push('.');
                name.push_str(extension);
            }
        }
    }
    name
}

fn sanitize_name(name: &str) -> String {
    name.chars()
        .filter(|character| !matches!(character, '/' | '\\' | '\0'))
        .take(200)
        .collect()
}

fn uniquify(path: &Path) -> PathBuf {
    if !path.exists() {
        return path.to_path_buf();
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let stem = path.file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_else(|| "download".into());
    let extension = path.extension().map(|extension| format!(".{}", extension.to_string_lossy())).unwrap_or_default();
    for index in 1..1000u32 {
        let candidate = parent.join(format!("{stem}-{index}{extension}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    parent.join(format!("{stem}-{}{extension}", unix_now()))
}

fn cleanup_stale_temporaries(directory: &Path) {
    let Ok(entries) = fs::read_dir(directory) else { return };
    for entry in entries.flatten() {
        // Only the exact `.{name}.part-{numeric id}` pattern is a stale
        // temporary; completed downloads and user files containing ".part-"
        // as a bare substring must never be removed.
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if is_temporary_name(name) {
            let _ = fs::remove_file(entry.path());
        }
    }
}

async fn run_download(
    inner: Arc<DownloadManagerInner>,
    id: DownloadId,
    request: DownloadRequest,
    target: PathBuf,
    needs_prompt: bool,
    flag: Arc<AtomicBool>,
    prompt_receiver: Option<oneshot::Receiver<bool>>,
) {
    let history = &inner.history;
    if needs_prompt {
        let overwrite = match prompt_receiver {
            Some(receiver) => tokio::select! {
                decision = receiver => decision.unwrap_or(false),
                _ = wait_for_cancel(&flag) => false,
            },
            None => false,
        };
        if !overwrite {
            history.transition(id, |_| DownloadStatus::Cancelled);
            return;
        }
    }
    if flag.load(Ordering::SeqCst) {
        history.transition(id, |_| DownloadStatus::Cancelled);
        return;
    }
    history.set_path(id, target.clone());
    history.transition(id, |_| DownloadStatus::Downloading { received: 0, total: None });

    let mut response = match inner.client.get(request.media.url.clone()).send().await {
        Ok(response) => response,
        Err(error) => {
            history.transition(id, |_| DownloadStatus::Failed(error.to_string()));
            inner.events.lock().push(DownloadEvent::Failed(id, error.to_string()));
            return;
        }
    };
    let status = response.status();
    if !status.is_success() {
        let message = format!("server returned {status}");
        history.transition(id, |_| DownloadStatus::Failed(message.clone()));
        inner.events.lock().push(DownloadEvent::Failed(id, message));
        return;
    }
    let header_mime = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let mime = resolve_mime(&request.media, header_mime.as_deref());
    history.set_mime(id, mime);

    let parent = target
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    if let Err(error) = fs::create_dir_all(parent) {
        let message = format!("cannot create {}: {error}", parent.display());
        history.transition(id, |_| DownloadStatus::Failed(message.clone()));
        inner.events.lock().push(DownloadEvent::Failed(id, message));
        return;
    }
    let temporary = temporary_path(&target, id);
    let mut file = match open_restrictive(&temporary) {
        Ok(file) => file,
        Err(error) => {
            let message = format!("cannot create temporary file: {error}");
            history.transition(id, |_| DownloadStatus::Failed(message.clone()));
            inner.events.lock().push(DownloadEvent::Failed(id, message));
            return;
        }
    };

    let total = response.content_length();
    let mut received = 0u64;
    loop {
        if flag.load(Ordering::SeqCst) {
            drop(file);
            let _ = fs::remove_file(&temporary);
            history.transition(id, |_| DownloadStatus::Cancelled);
            return;
        }
        match response.chunk().await {
            Ok(Some(chunk)) => {
                if let Err(error) = file.write_all(&chunk) {
                    drop(file);
                    let _ = fs::remove_file(&temporary);
                    let message = format!("write failed: {error}");
                    history.transition(id, |_| DownloadStatus::Failed(message.clone()));
                    inner.events.lock().push(DownloadEvent::Failed(id, message));
                    return;
                }
                received = received.saturating_add(chunk.len() as u64);
                history.transition(id, |_| DownloadStatus::Downloading { received, total });
            }
            Ok(None) => break,
            Err(error) => {
                drop(file);
                let _ = fs::remove_file(&temporary);
                let message = format!("stream failed: {error}");
                history.transition(id, |_| DownloadStatus::Failed(message.clone()));
                inner.events.lock().push(DownloadEvent::Failed(id, message));
                return;
            }
        }
    }
    if flag.load(Ordering::SeqCst) {
        drop(file);
        let _ = fs::remove_file(&temporary);
        history.transition(id, |_| DownloadStatus::Cancelled);
        return;
    }
    let _ = file.sync_all();
    drop(file);
    if let Err(error) = fs::rename(&temporary, &target) {
        let _ = fs::remove_file(&temporary);
        let message = format!("cannot move download into place: {error}");
        history.transition(id, |_| DownloadStatus::Failed(message.clone()));
        inner.events.lock().push(DownloadEvent::Failed(id, message));
        return;
    }
    history.transition(id, |_| DownloadStatus::Completed);
    inner.events.lock().push(DownloadEvent::Completed(id));
}

/// Polls the cancellation flag at a slow cadence. The prompt wait parks on
/// this timer instead of busy-spinning a CPU core; cancellation is also
/// delivered by aborting the task, so this is a defensive backstop, not a
/// hot path.
async fn wait_for_cancel(flag: &AtomicBool) {
    while !flag.load(Ordering::SeqCst) {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn open_restrictive(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default()
}
