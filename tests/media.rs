use std::{
    ffi::OsString,
    io::{Read, Write},
    net::TcpListener,
    path::PathBuf,
    sync::LazyLock,
    time::Duration,
};

use lemmy::{
    domain::{DownloadStatus, MediaRef, ProfileId},
    media::{
        CollisionPolicy, DownloadManager, DownloadRequest, MediaHandler, MediaPolicyConfig,
        build_argv, find_entry, parse_mailcap, resolve_mime,
    },
};
use url::Url;

fn image_media() -> MediaRef {
    MediaRef::new(Url::parse("https://example.com/photo.png").unwrap())
}

fn test_dir() -> PathBuf {
    std::env::temp_dir().join(format!(
        "lemmy-media-test-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ))
}

fn test_download_manager() -> DownloadManager {
    let dir = test_dir();
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    DownloadManager::new(dir)
}

fn download_request(url: Url, destination: PathBuf, collision: CollisionPolicy) -> DownloadRequest {
    DownloadRequest {
        media: MediaRef::new(url),
        profile: ProfileId::from("fixture"),
        instance_url: Url::parse("http://127.0.0.1/").unwrap(),
        destination,
        collision,
    }
}

/// A URL served by a listener that accepts a connection, reads the request, and
/// never responds. Cancellation is the only way to unblock the download.
fn slow_download_request() -> DownloadRequest {
    static LISTENERS: LazyLock<std::sync::Mutex<Vec<TcpListener>>> =
        LazyLock::new(|| std::sync::Mutex::new(Vec::new()));
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    LISTENERS
        .lock()
        .unwrap()
        .push(listener.try_clone().unwrap());
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buffer = [0u8; 512];
            let _ = stream.read(&mut buffer);
            std::thread::sleep(Duration::from_secs(120));
        }
    });
    download_request(
        Url::parse(&format!("http://127.0.0.1:{port}/slow")).unwrap(),
        test_dir().join("slow.bin"),
        CollisionPolicy::Overwrite,
    )
}

/// A tiny HTTP server that answers one request per connection with a fixed body.
fn spawn_http_server(body: &'static [u8], content_type: &'static str) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let mut buffer = [0u8; 4096];
            let _ = stream.read(&mut buffer);
            let headers = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(headers.as_bytes());
            let _ = stream.write_all(body);
            let _ = stream.flush();
        }
    });
    port
}

#[test]
fn mailcap_is_the_default_handler() {
    let policy = MediaPolicyConfig::default();
    let handler = policy.select(&image_media());
    assert!(matches!(handler, MediaHandler::Mailcap { .. }));
}

#[test]
fn scratch_dir_is_nested_under_the_system_temp_dir() {
    let directory = lemmy::media::scratch_dir();
    assert!(
        directory.starts_with(std::env::temp_dir()),
        "the scratch dir must live under the system temp dir, got {}",
        directory.display()
    );
    assert_eq!(
        directory.file_name().and_then(|name| name.to_str()),
        Some("lemmy-client"),
        "the scratch dir is an exclusively-owned subdirectory"
    );
}

#[test]
fn clean_scratch_dir_removes_the_whole_subtree() {
    let directory = lemmy::media::scratch_dir();
    std::fs::create_dir_all(directory.join("nested")).unwrap();
    std::fs::write(directory.join("stale.bin"), b"stale").unwrap();
    std::fs::write(directory.join("nested/other.bin"), b"stale").unwrap();
    lemmy::media::clean_scratch_dir().unwrap();
    assert!(
        !directory.exists(),
        "the whole scratch subtree must be removed"
    );
    // A missing directory is not an error (idempotent sweep).
    lemmy::media::clean_scratch_dir().unwrap();
}

#[tokio::test]
async fn cancelled_download_is_recorded_in_session_history() {
    let manager = test_download_manager();
    let id = manager.start(slow_download_request()).await.unwrap();
    manager.cancel(id).await.unwrap();
    assert_eq!(
        manager.history().get(id).unwrap().status,
        DownloadStatus::Cancelled
    );
}

#[test]
fn explicit_handler_configuration_wins_over_mailcap() {
    let mut policy = MediaPolicyConfig::default();
    policy
        .handlers
        .insert("image/png".into(), "custom-viewer %s".into());
    let handler = policy.select(&image_media());
    assert_eq!(
        handler,
        MediaHandler::External {
            command: "custom-viewer %s".into()
        }
    );
}

#[test]
fn unsupported_types_return_metadata_only() {
    let media = MediaRef::new(Url::parse("https://example.com/data.zzz").unwrap());
    let policy = MediaPolicyConfig::default();
    let handler = policy.select(&media);
    assert_eq!(handler, MediaHandler::MetadataOnly);
}

#[test]
fn mime_resolution_prefers_metadata_then_header_then_filename() {
    let mut media = MediaRef::new(Url::parse("https://example.com/photo.png").unwrap());
    media.mime_type = Some("image/gif".into());
    assert_eq!(
        resolve_mime(&media, Some("image/jpeg")),
        Some("image/gif".into())
    );
    assert_eq!(resolve_mime(&media, None), Some("image/gif".into()));

    let media = MediaRef::new(Url::parse("https://example.com/photo.png").unwrap());
    assert_eq!(
        resolve_mime(&media, Some("image/jpeg")),
        Some("image/jpeg".into())
    );
    assert_eq!(resolve_mime(&media, None), Some("image/png".into()));
}

#[test]
fn mailcap_parses_and_builds_safe_argv() {
    let source = r#"# comment
image/png; feh --fullscreen %s; test=test -n "$DISPLAY"
image/jpeg; eog "%s"; needsterminal
video/*; mpv %s
application/pdf; zathura %s; description="PDF reader"
"#;
    let entries = parse_mailcap(source);
    assert_eq!(entries.len(), 4);

    let png = find_entry(&entries, "image/png").expect("png entry");
    assert_eq!(png.command, "feh --fullscreen %s");
    let argv = build_argv(&png.command, "/tmp/file.png", "image/png");
    assert_eq!(
        argv,
        vec![
            OsString::from("feh"),
            OsString::from("--fullscreen"),
            OsString::from("/tmp/file.png")
        ]
    );

    let video = find_entry(&entries, "video/mp4").expect("video wildcard entry");
    assert_eq!(video.command, "mpv %s");

    let argv = build_argv("mimeopen -M %t %s", "/tmp/f", "image/png");
    assert_eq!(
        argv,
        vec![
            OsString::from("mimeopen"),
            OsString::from("-M"),
            OsString::from("image/png"),
            OsString::from("/tmp/f")
        ]
    );

    // Shell metacharacters stay literal: no interpolation ever happens here.
    let argv = build_argv("sh -c \"echo $(id) > /tmp/x\" %s", "/tmp/f", "text/plain");
    assert_eq!(
        argv,
        vec![
            OsString::from("sh"),
            OsString::from("-c"),
            OsString::from("echo $(id) > /tmp/x"),
            OsString::from("/tmp/f")
        ]
    );

    // A template without %s appends the file as the final argument.
    let argv = build_argv("xdg-open", "/tmp/f", "text/plain");
    assert_eq!(
        argv,
        vec![OsString::from("xdg-open"), OsString::from("/tmp/f")]
    );
}

#[tokio::test]
async fn download_completes_and_renames_atomically() {
    let manager = test_download_manager();
    let body: &'static [u8] = b"hello media download";
    let port = spawn_http_server(body, "image/png");
    let destination = test_dir().join("photo.bin");
    let request = download_request(
        Url::parse(&format!("http://127.0.0.1:{port}/photo.bin")).unwrap(),
        destination.clone(),
        CollisionPolicy::Overwrite,
    );
    let id = manager.start(request).await.unwrap();
    let status = manager.wait_for(id).await;
    assert_eq!(status, DownloadStatus::Completed);

    let record = manager.history().get(id).unwrap();
    assert_eq!(record.local_path, destination);
    assert_eq!(record.mime_type.as_deref(), Some("image/png"));
    assert_eq!(record.filename, "photo.bin");
    assert_eq!(std::fs::read(&destination).unwrap(), body);
    assert!(record.status.is_terminal());

    let leftovers = std::fs::read_dir(test_dir())
        .unwrap()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_name().to_string_lossy().contains(".part-"))
        .count();
    assert_eq!(leftovers, 0);
}

#[tokio::test]
async fn unique_name_collision_appends_suffix() {
    let manager = test_download_manager();
    let body: &'static [u8] = b"unique";
    let port = spawn_http_server(body, "text/plain");
    let destination = test_dir().join("notes.txt");
    std::fs::write(&destination, b"existing").unwrap();
    let request = download_request(
        Url::parse(&format!("http://127.0.0.1:{port}/notes.txt")).unwrap(),
        destination.clone(),
        CollisionPolicy::UniqueName,
    );
    let id = manager.start(request).await.unwrap();
    assert_eq!(manager.wait_for(id).await, DownloadStatus::Completed);
    let record = manager.history().get(id).unwrap();
    assert_ne!(record.local_path, destination);
    assert_eq!(
        record.local_path.file_name().unwrap().to_string_lossy(),
        "notes-1.txt"
    );
    assert_eq!(std::fs::read(&record.local_path).unwrap(), body);
    assert_eq!(std::fs::read(&destination).unwrap(), b"existing");
}

#[tokio::test]
async fn prompt_collision_waits_for_resolution() {
    let manager = test_download_manager();
    let body: &'static [u8] = b"prompted";
    let port = spawn_http_server(body, "text/plain");
    let destination = test_dir().join("notes.txt");
    std::fs::write(&destination, b"existing").unwrap();

    let request = download_request(
        Url::parse(&format!("http://127.0.0.1:{port}/notes.txt")).unwrap(),
        destination.clone(),
        CollisionPolicy::Prompt,
    );
    let keep_id = manager.start(request).await.unwrap();
    assert_eq!(
        manager.history().get(keep_id).unwrap().status,
        DownloadStatus::Prompting
    );
    manager.resolve_collision(keep_id, false).await.unwrap();
    assert_eq!(manager.wait_for(keep_id).await, DownloadStatus::Cancelled);
    assert_eq!(std::fs::read(&destination).unwrap(), b"existing");

    let request = download_request(
        Url::parse(&format!("http://127.0.0.1:{port}/notes.txt")).unwrap(),
        destination.clone(),
        CollisionPolicy::Prompt,
    );
    let overwrite_id = manager.start(request).await.unwrap();
    manager.resolve_collision(overwrite_id, true).await.unwrap();
    assert_eq!(
        manager.wait_for(overwrite_id).await,
        DownloadStatus::Completed
    );
    assert_eq!(std::fs::read(&destination).unwrap(), b"prompted");
}

#[tokio::test]
async fn history_search_filters_by_filename_and_url() {
    let manager = test_download_manager();
    let body: &'static [u8] = b"searchable";
    let port = spawn_http_server(body, "text/plain");
    let request = download_request(
        Url::parse(&format!("http://127.0.0.1:{port}/alpha.txt")).unwrap(),
        test_dir().join("alpha.txt"),
        CollisionPolicy::Overwrite,
    );
    let id = manager.start(request).await.unwrap();
    assert_eq!(manager.wait_for(id).await, DownloadStatus::Completed);
    assert!(manager.history().get(id).is_some());
    assert_eq!(manager.history().filtered("alpha").len(), 1);
    assert_eq!(manager.history().filtered("alpha").first().unwrap().id, id);
    assert_eq!(manager.history().filtered("127.0.0.1").len(), 1);
    assert!(manager.history().filtered("nonexistent").is_empty());
    assert_eq!(manager.history().filtered("").len(), 1);
}

#[tokio::test]
async fn download_records_profile_instance_and_timestamp() {
    let manager = test_download_manager();
    let body: &'static [u8] = b"metadata";
    let port = spawn_http_server(body, "text/plain");
    let request = download_request(
        Url::parse(&format!("http://127.0.0.1:{port}/meta.txt")).unwrap(),
        test_dir().join("meta.txt"),
        CollisionPolicy::Overwrite,
    );
    let id = manager.start(request).await.unwrap();
    assert_eq!(manager.wait_for(id).await, DownloadStatus::Completed);
    let record = manager.history().get(id).unwrap();
    assert_eq!(record.profile, ProfileId::from("fixture"));
    assert_eq!(record.instance_url.as_str(), "http://127.0.0.1/");
    assert!(record.requested_at > 0);
    assert_eq!(
        record.media.url.as_str(),
        format!("http://127.0.0.1:{port}/meta.txt")
    );
}

#[tokio::test]
async fn retry_removes_stale_temp_before_reusing_temp_path() {
    let manager = test_download_manager();
    let body: &'static [u8] = b"retried body";
    let port = spawn_http_server(body, "text/plain");
    let destination = test_dir().join("notes.txt");
    let request = download_request(
        Url::parse(&format!("http://127.0.0.1:{port}/notes.txt")).unwrap(),
        destination.clone(),
        CollisionPolicy::Overwrite,
    );
    let id = manager.start(request).await.unwrap();
    assert_eq!(manager.wait_for(id).await, DownloadStatus::Completed);

    // Simulate an attempt aborted mid-stream: aborting a task drops it
    // without running the cleanup inside the download task, leaving the
    // `.part-{id}` temp file at the exact path `retry` reuses.
    let stale = test_dir().join(format!(".notes.txt.part-{}", id.0));
    std::fs::write(&stale, b"half-written").unwrap();

    // Without the fix the retried task's open_restrictive (create_new) fails
    // with EEXIST and the download dies with "cannot create temporary file".
    manager.retry(id).await.unwrap();
    assert_eq!(manager.wait_for(id).await, DownloadStatus::Completed);
    assert_eq!(std::fs::read(&destination).unwrap(), body);
    assert!(
        !stale.exists(),
        "stale temp must be removed before the temp path is reused"
    );
}

#[test]
fn stale_temp_cleanup_matches_exact_pattern_only() {
    let dir = test_dir();
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let exact = dir.join(".photo.bin.part-3"); // .{name}.part-{numeric} -> stale temp
    let completed = dir.join("photo.bin"); // completed download
    let mid_name = dir.join("notes.part-1.txt"); // bare substring, no leading dot
    let dot_alpha = dir.join(".my.part-notes.txt"); // leading dot, non-numeric id
    let dot_date = dir.join(".report.part-2024-01"); // leading dot, non-numeric id
    let files: [(&std::path::Path, &[u8]); 5] = [
        (exact.as_path(), b"stale"),
        (completed.as_path(), b"kept"),
        (mid_name.as_path(), b"kept"),
        (dot_alpha.as_path(), b"kept"),
        (dot_date.as_path(), b"kept"),
    ];
    for (path, contents) in files {
        std::fs::write(path, contents).unwrap();
    }

    // Constructing the manager runs the startup stale-temp sweep.
    let _manager = DownloadManager::new(dir.clone());

    assert!(!exact.exists(), "exact temp pattern must be reclaimed");
    assert!(
        completed.exists(),
        "completed downloads must survive the sweep"
    );
    assert!(
        mid_name.exists(),
        "files merely containing .part- must survive"
    );
    assert!(dot_alpha.exists(), "non-numeric .part- names must survive");
    assert!(dot_date.exists(), "non-numeric .part- names must survive");
}

#[tokio::test]
async fn retry_prompt_collision_parks_record_in_prompting() {
    let manager = test_download_manager();
    let body: &'static [u8] = b"retried prompt";
    let port = spawn_http_server(body, "text/plain");
    let destination = test_dir().join("notes.txt");
    std::fs::write(&destination, b"pre-existing").unwrap();
    let request = download_request(
        Url::parse(&format!("http://127.0.0.1:{port}/notes.txt")).unwrap(),
        destination.clone(),
        CollisionPolicy::Prompt,
    );
    let id = manager.start(request).await.unwrap();
    assert_eq!(
        manager.history().get(id).unwrap().status,
        DownloadStatus::Prompting
    );
    manager.resolve_collision(id, false).await.unwrap();
    assert_eq!(manager.wait_for(id).await, DownloadStatus::Cancelled);
    assert_eq!(std::fs::read(&destination).unwrap(), b"pre-existing");

    // Retrying a prompt-policy collision must re-park the record in
    // Prompting synchronously, exactly like start() does, so the UI can
    // surface the collision prompt instead of a silent pending state.
    manager.retry(id).await.unwrap();
    assert_eq!(
        manager.history().get(id).unwrap().status,
        DownloadStatus::Prompting
    );
    manager.resolve_collision(id, true).await.unwrap();
    assert_eq!(manager.wait_for(id).await, DownloadStatus::Completed);
    assert_eq!(std::fs::read(&destination).unwrap(), b"retried prompt");
}

#[tokio::test(start_paused = true)]
async fn collision_prompt_wait_parks_instead_of_spinning() {
    let manager = test_download_manager();
    let body: &'static [u8] = b"parked";
    let port = spawn_http_server(body, "text/plain");
    let destination = test_dir().join("notes.txt");
    std::fs::write(&destination, b"pre-existing").unwrap();
    let request = download_request(
        Url::parse(&format!("http://127.0.0.1:{port}/notes.txt")).unwrap(),
        destination.clone(),
        CollisionPolicy::Prompt,
    );
    let id = manager.start(request).await.unwrap();
    assert_eq!(
        manager.history().get(id).unwrap().status,
        DownloadStatus::Prompting
    );

    // The prompt wait must park on a timer, not busy-spin a CPU core: many
    // poll intervals elapse without the wait resolving or the record leaving
    // Prompting, and timer-driven tasks stay responsive meanwhile.
    tokio::time::advance(Duration::from_millis(500)).await;
    assert_eq!(
        manager.history().get(id).unwrap().status,
        DownloadStatus::Prompting
    );

    let ticker = tokio::spawn(async {
        tokio::time::sleep(Duration::from_millis(100)).await;
        true
    });
    tokio::time::advance(Duration::from_millis(100)).await;
    assert!(
        ticker.await.unwrap(),
        "executor must stay responsive while the prompt waits"
    );

    // hyper/reqwest drives its request lifecycle on real timers, so restore
    // real time before the download performs its network request.
    tokio::time::resume();
    manager.resolve_collision(id, true).await.unwrap();
    assert_eq!(manager.wait_for(id).await, DownloadStatus::Completed);
    assert_eq!(std::fs::read(&destination).unwrap(), b"parked");
}
