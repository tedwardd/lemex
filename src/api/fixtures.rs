use super::HttpLemmyApi;
use crate::domain::{Profile, ProfileContext, ProfileId, SecretString, Session, UserId};
use crate::error::{AppError, Result};
use std::{
    net::TcpListener as StdTcpListener,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use url::Url;

struct FixtureRoute {
    path: Option<String>,
    status: u16,
    body: Option<String>,
    delay: Option<Duration>,
    malformed_body: bool,
    requests: Option<Arc<AtomicUsize>>,
    /// When true, requests without an `Authorization` header get 401. Lets
    /// smoke tests prove the client attaches the session bearer token.
    require_auth: bool,
    /// When set, the first N requests answer 500 (a transient failure the
    /// client retries) and later requests answer the configured body.
    transient_failures: Option<Arc<AtomicUsize>>,
    /// Additional (path, body) routes served when `path` does not match, so
    /// one server can answer the follow-up requests a flow makes (for
    /// example the `/site` call that derives the login user id).
    extra_paths: Vec<(String, String)>,
    /// When set, the raw `User-Agent` header of the first request is
    /// captured here so tests can assert the client identifies itself
    /// instead of sending the generic reqwest default (which at least one
    /// public Lemmy edge resets the connection on).
    user_agent: Option<Arc<Mutex<Option<String>>>>,
    /// When set, requests whose raw query contains `page_cursor=` are
    /// answered with this body instead of `body`, so tests can serve a
    /// distinct next page and prove the client sends the opaque cursor
    /// back as `page_cursor` (Lemmy 0.19+ protocol).
    cursor_body: Option<String>,
}

pub struct FixtureServer {
    task: tokio::task::JoinHandle<()>,
}
impl Drop for FixtureServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn fixture_body(name: &str) -> Result<String> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join("levim")
        .join(name);
    std::fs::read_to_string(&path)
        .map_err(|error| AppError::Network(format!("fixture {}: {error}", path.display())))
}

fn start_server(route: FixtureRoute) -> Result<(Url, Arc<FixtureServer>)> {
    let listener = StdTcpListener::bind(("127.0.0.1", 0))
        .map_err(|error| AppError::Network(format!("fixture server: {error}")))?;
    listener
        .set_nonblocking(true)
        .map_err(|error| AppError::Network(format!("fixture server: {error}")))?;
    let address = listener
        .local_addr()
        .map_err(|error| AppError::Network(format!("fixture server address: {error}")))?;
    let listener = TcpListener::from_std(listener)
        .map_err(|error| AppError::Network(format!("fixture server: {error}")))?;
    let task = tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let route = FixtureRoute {
                path: route.path.clone(),
                status: route.status,
                body: route.body.clone(),
                delay: route.delay,
                malformed_body: route.malformed_body,
                requests: route.requests.clone(),
                require_auth: route.require_auth,
                transient_failures: route.transient_failures.clone(),
                extra_paths: route.extra_paths.clone(),
                user_agent: route.user_agent.clone(),
                cursor_body: route.cursor_body.clone(),
            };
            tokio::spawn(async move {
                let mut request = vec![0_u8; 8192];
                let _ = stream.read(&mut request).await;
                if let Some(requests) = &route.requests {
                    requests.fetch_add(1, Ordering::SeqCst);
                }
                if let Some(delay) = route.delay {
                    tokio::time::sleep(delay).await;
                    return;
                }
                let request = String::from_utf8_lossy(&request);
                if let Some(capture) = &route.user_agent {
                    let value = request
                        .lines()
                        .find(|line| line.to_ascii_lowercase().starts_with("user-agent:"))
                        .and_then(|line| line.split_once(':'))
                        .map(|(_, value)| value.trim().to_owned())
                        .unwrap_or_default();
                    if let Ok(mut guard) = capture.lock()
                        && guard.is_none()
                    {
                        *guard = Some(value);
                    }
                }
                if route.require_auth && !request.to_ascii_lowercase().contains("authorization:") {
                    write_response(
                        &mut stream,
                        401,
                        r#"{"error":"fixture requires authentication"}"#,
                        false,
                    )
                    .await;
                    return;
                }
                if let Some(remaining) = &route.transient_failures {
                    let was_failure = remaining
                        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |current| {
                            (current > 0).then(|| current - 1)
                        })
                        .is_ok();
                    if was_failure {
                        write_response(
                            &mut stream,
                            500,
                            r#"{"error":"transient fixture failure"}"#,
                            false,
                        )
                        .await;
                        return;
                    }
                }
                let requested_path = request
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .unwrap_or_default()
                    .split('?')
                    .next()
                    .unwrap_or_default();
                let body = if let Some(cursor_body) = &route.cursor_body
                    && route
                        .path
                        .as_deref()
                        .is_none_or(|path| path == requested_path)
                    && request.contains("page_cursor=")
                {
                    cursor_body.clone()
                } else {
                    match route.path.as_deref() {
                        Some(path) if path == requested_path => {
                            route.body.clone().unwrap_or_else(|| "{}".into())
                        }
                        Some(_) => match route
                            .extra_paths
                            .iter()
                            .find(|(path, _)| path == requested_path)
                        {
                            Some((_, body)) => body.clone(),
                            None => {
                                write_response(
                                    &mut stream,
                                    404,
                                    r#"{"error":"fixture route not found"}"#,
                                    false,
                                )
                                .await;
                                return;
                            }
                        },
                        None => route.body.clone().unwrap_or_else(|| "{}".into()),
                    }
                };
                write_response(&mut stream, route.status, &body, route.malformed_body).await;
            });
        }
    });
    Ok((
        Url::parse(&format!("http://{address}/")).expect("local fixture URL"),
        Arc::new(FixtureServer { task }),
    ))
}

async fn write_response(
    stream: &mut tokio::net::TcpStream,
    status: u16,
    body: &str,
    malformed_body: bool,
) {
    let reason = if status >= 400 { "Error" } else { "OK" };
    let content_length = if malformed_body {
        body.len() + 16
    } else {
        body.len()
    };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {content_length}\r\nConnection: close\r\n\r\n{body}"
    );
    let _ = stream.write_all(response.as_bytes()).await;
}

fn api_for(server: Arc<FixtureServer>, base: Url, timeout: Duration) -> HttpLemmyApi {
    HttpLemmyApi::with_timeout(timeout)
        .expect("fixture client")
        .with_fixture_server(server)
        .with_base_url(base)
}

pub fn fixture_api(name: &str) -> HttpLemmyApi {
    let body = fixture_body(name).expect("fixture exists");
    fixture_api_with_body(&body)
}

pub fn fixture_api_with_body(body: &str) -> HttpLemmyApi {
    let (base, server) = start_server(FixtureRoute {
        path: None,
        status: 200,
        body: Some(body.into()),
        delay: None,
        malformed_body: false,
        requests: None,
        require_auth: false,
        transient_failures: None,
        user_agent: None,
        cursor_body: None,
        extra_paths: Vec::new(),
    })
    .expect("fixture server starts");
    api_for(server, base, Duration::from_secs(2))
}

pub fn fixture_api_with_status(path: &str, status: u16) -> HttpLemmyApi {
    let (base, server) = start_server(FixtureRoute {
        path: Some(path.into()),
        status,
        body: Some(r#"{"error":"session expired"}"#.into()),
        delay: None,
        malformed_body: false,
        requests: None,
        require_auth: false,
        transient_failures: None,
        user_agent: None,
        cursor_body: None,
        extra_paths: Vec::new(),
    })
    .expect("fixture server starts");
    api_for(server, base, Duration::from_secs(2))
}

pub fn fixture_api_with_status_count(status: u16) -> (HttpLemmyApi, Arc<AtomicUsize>) {
    let requests = Arc::new(AtomicUsize::new(0));
    let (base, server) = start_server(FixtureRoute {
        path: None,
        status,
        body: Some(r#"{"error":"fixture status"}"#.into()),
        delay: None,
        malformed_body: false,
        requests: Some(requests.clone()),
        require_auth: false,
        transient_failures: None,
        user_agent: None,
        cursor_body: None,
        extra_paths: Vec::new(),
    })
    .expect("fixture server starts");
    (api_for(server, base, Duration::from_secs(2)), requests)
}

pub fn truncated_body_fixture_api() -> HttpLemmyApi {
    let (base, server) = start_server(FixtureRoute {
        path: None,
        status: 200,
        body: Some("{}".into()),
        delay: None,
        malformed_body: true,
        requests: None,
        require_auth: false,
        transient_failures: None,
        user_agent: None,
        cursor_body: None,
        extra_paths: Vec::new(),
    })
    .expect("fixture server starts");
    api_for(server, base, Duration::from_secs(2))
}

pub fn login_fixture_api(path: &str) -> (HttpLemmyApi, Url) {
    let normalized = format!(
        "{}api/v3/user/login",
        path.trim_end_matches('/').to_owned() + "/"
    );
    let body = fixture_body("login.json").expect("fixture exists");
    let site_body = fixture_body("site.json").expect("fixture exists");
    let (base, server) = start_server(FixtureRoute {
        path: Some(normalized),
        status: 200,
        body: Some(body),
        delay: None,
        malformed_body: false,
        requests: None,
        require_auth: false,
        transient_failures: None,
        user_agent: None,
        cursor_body: None,
        // Login derives the user id from the authenticated `/site` response;
        // serve the my_user-bearing fixture on that route too.
        extra_paths: vec![("/api/v3/site".to_owned(), site_body)],
    })
    .expect("fixture server starts");
    let instance_url = base
        .join(path.trim_start_matches('/'))
        .expect("fixture instance URL");
    (api_for(server, base, Duration::from_secs(2)), instance_url)
}
pub fn timeout_fixture_api() -> HttpLemmyApi {
    let (base, server) = start_server(FixtureRoute {
        path: None,
        status: 200,
        body: None,
        delay: Some(Duration::from_secs(2)),
        malformed_body: false,
        requests: None,
        require_auth: false,
        transient_failures: None,
        user_agent: None,
        cursor_body: None,
        extra_paths: Vec::new(),
    })
    .expect("fixture server starts");
    api_for(server, base, Duration::from_millis(50))
}

/// A fixture server that delays every answer by `delay` (the request is
/// counted before the delay, so retry loops observe a per-attempt stall on
/// an otherwise healthy server). Lets tests prove the whole-read budget
/// cuts across retries: with a delay longer than the per-attempt deadline,
/// the natural exhaustion of three attempts would take ~3 × delay, and a
/// tight total budget must fail much sooner.
pub fn fixture_api_with_delay(delay: Duration) -> HttpLemmyApi {
    let (base, server) = start_server(FixtureRoute {
        path: None,
        status: 200,
        body: None,
        delay: Some(delay),
        malformed_body: false,
        requests: None,
        require_auth: false,
        transient_failures: None,
        user_agent: None,
        cursor_body: None,
        extra_paths: Vec::new(),
    })
    .expect("fixture server starts");
    api_for(server, base, Duration::from_secs(2))
}

/// A fixture server that answers every request with `body` once the request
/// carries an `Authorization` header; unauthenticated requests get 401. The
/// request counter lets smoke tests prove exactly one authenticated call was
/// made (and that anonymous calls are rejected before the body is served).
pub fn fixture_api_requiring_auth(body: &str) -> (HttpLemmyApi, Arc<AtomicUsize>) {
    let requests = Arc::new(AtomicUsize::new(0));
    let (base, server) = start_server(FixtureRoute {
        path: None,
        status: 200,
        body: Some(body.into()),
        delay: None,
        malformed_body: false,
        requests: Some(requests.clone()),
        require_auth: true,
        transient_failures: None,
        user_agent: None,
        cursor_body: None,
        extra_paths: Vec::new(),
    })
    .expect("fixture server starts");
    (api_for(server, base, Duration::from_secs(2)), requests)
}

/// A fixture server that answers the first `failures` requests with 500 and
/// every later request with `body`. The remaining-failure counter lets smoke
/// tests observe the transient window being consumed by the client's bounded
/// retry loop and by a later, successful attempt.
pub fn fixture_api_with_transient_failures(
    body: &str,
    failures: usize,
) -> (HttpLemmyApi, Arc<AtomicUsize>) {
    let remaining = Arc::new(AtomicUsize::new(failures));
    let (base, server) = start_server(FixtureRoute {
        path: None,
        status: 200,
        body: Some(body.into()),
        delay: None,
        malformed_body: false,
        requests: None,
        require_auth: false,
        transient_failures: Some(remaining.clone()),
        user_agent: None,
        cursor_body: None,
        extra_paths: Vec::new(),
    })
    .expect("fixture server starts");
    (api_for(server, base, Duration::from_secs(2)), remaining)
}

/// A fixture server that answers the first page with `first` and any
/// follow-up request carrying `page_cursor=` with `next`, so tests can prove
/// the client sends the opaque cursor back as `page_cursor`.
pub fn fixture_api_with_pages(first: &str, next: &str) -> HttpLemmyApi {
    let (base, server) = start_server(FixtureRoute {
        path: None,
        status: 200,
        body: Some(first.into()),
        delay: None,
        malformed_body: false,
        requests: None,
        require_auth: false,
        transient_failures: None,
        extra_paths: Vec::new(),
        user_agent: None,
        cursor_body: Some(next.into()),
    })
    .expect("fixture server starts");
    api_for(server, base, Duration::from_secs(2))
}

/// A fixture server that answers every request with `body` and captures the
/// raw `User-Agent` header of the first request, so tests can assert the
/// client identifies itself instead of sending the generic reqwest default.
pub fn fixture_api_recording_user_agent(body: &str) -> (HttpLemmyApi, Arc<Mutex<Option<String>>>) {
    let user_agent = Arc::new(Mutex::new(None));
    let (base, server) = start_server(FixtureRoute {
        path: None,
        status: 200,
        body: Some(body.into()),
        delay: None,
        malformed_body: false,
        requests: None,
        require_auth: false,
        transient_failures: None,
        extra_paths: Vec::new(),
        user_agent: Some(user_agent.clone()),
        cursor_body: None,
    })
    .expect("fixture server starts");
    (api_for(server, base, Duration::from_secs(2)), user_agent)
}

pub fn anonymous_context() -> ProfileContext {
    context(None)
}
pub fn authenticated_context() -> ProfileContext {
    context(Some(Session {
        token: SecretString::from("fixture-token"),
        user_id: UserId(1),
    }))
}
fn context(session: Option<Session>) -> ProfileContext {
    ProfileContext {
        profile: Profile {
            id: ProfileId::from("fixture"),
            instance_url: Url::parse("http://127.0.0.1/").expect("fixture URL"),
            account_label: None,
        },
        session,
    }
}
