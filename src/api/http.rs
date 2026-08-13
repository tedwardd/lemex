use super::{
    CommentView, FeedQuery, LemmyApi, LoginRequest, MutationResult, Page, PostDetail, PostView,
    SiteInfo,
};
use crate::domain::{CommentId, Mutation, PostId, Profile, ProfileContext, Session, UserId};
use crate::error::{AppError, Result};
use reqwest::{Client, Response, StatusCode};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use url::Url;

const MAX_READ_ATTEMPTS: usize = 3;

/// Hard cap on a single API response body. Lemmy responses are small
/// (feeds are ≤ 50 posts); a malicious instance must not be able to make the
/// client buffer an unbounded body (the request timeout bounds duration, not
/// size).
const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;

/// Hard cap on the number of items accepted from a server-provided list.
/// The `limit` query parameter is client-side; a hostile server can ignore
/// it and return an arbitrarily large array.
const MAX_ARRAY_ITEMS: usize = 1024;

/// Cap on server-derived text embedded in user-visible error messages.
const MAX_ERROR_DETAIL_CHARS: usize = 200;

/// Redirect policy for the authenticated API client. Credentials ride in the
/// `Authorization` header and the JSON body, so a redirect off the configured
/// origin must never be followed (reqwest would replay a 307/308 body to the
/// target host), and a downgrade to plaintext http must never keep the
/// header. Same-origin redirects (host canonicalization) and http→https
/// upgrades stay allowed.
fn api_redirect_policy() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        let origin = attempt.previous().first();
        let next = attempt.url();
        let Some(origin) = origin else {
            return attempt.stop();
        };
        let same_origin = origin.scheme() == next.scheme()
            && origin.host_str() == next.host_str()
            && origin.port_or_known_default() == next.port_or_known_default();
        let https_upgrade = origin.scheme() == "http"
            && next.scheme() == "https"
            && origin.host_str() == next.host_str()
            && origin.port_or_known_default() == next.port_or_known_default();
        if same_origin || https_upgrade {
            attempt.follow()
        } else {
            attempt.stop()
        }
    })
}

#[derive(Clone)]
pub struct HttpLemmyApi {
    client: Client,
    timeout: Duration,
    base_url: Option<Url>,
    fixture_server: Option<Arc<dyn Send + Sync>>,
}

impl HttpLemmyApi {
    pub fn new() -> Result<Self> {
        Self::with_timeout(Duration::from_secs(10))
    }

    pub fn with_timeout(timeout: Duration) -> Result<Self> {
        let client = Client::builder()
            .use_rustls_tls()
            .redirect(api_redirect_policy())
            .timeout(timeout)
            // reqwest sends no User-Agent with these features, and at least
            // one public Lemmy edge resets connections that carry none.
            // Identify the client explicitly instead of relying on the
            // header being absent.
            .user_agent(concat!("levim-client/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|error| AppError::Network(format!("could not build HTTP client: {error}")))?;
        Ok(Self {
            client,
            timeout,
            base_url: None,
            fixture_server: None,
        })
    }

    pub(crate) fn with_fixture_server(mut self, server: Arc<dyn Send + Sync>) -> Self {
        self.fixture_server = Some(server);
        self
    }

    pub(crate) fn with_base_url(mut self, base_url: Url) -> Self {
        self.base_url = Some(base_url);
        self
    }

    fn endpoint_from(&self, mut url: Url, path: &str) -> Result<Url> {
        let base = url.path().trim_end_matches('/');
        url.set_path(&format!("{base}/api/v3/{path}"));
        Ok(url)
    }

    fn endpoint(&self, ctx: &ProfileContext, path: &str) -> Result<Url> {
        self.endpoint_from(
            self.base_url
                .clone()
                .unwrap_or_else(|| ctx.profile.instance_url.clone()),
            path,
        )
    }

    fn auth_request(
        &self,
        request: reqwest::RequestBuilder,
        ctx: &ProfileContext,
    ) -> reqwest::RequestBuilder {
        match &ctx.session {
            Some(session) => request.bearer_auth(session.token.expose_secret()),
            None => request,
        }
    }

    async fn read_json(&self, request: reqwest::RequestBuilder, operation: &str) -> Result<Value> {
        self.send_json(request, operation, true).await
    }
    fn auth_body(&self, ctx: &ProfileContext, mut body: Value) -> Value {
        if let Some(session) = &ctx.session {
            body["auth"] = Value::String(session.token.expose_secret().to_owned());
        }
        body
    }

    async fn send_json(
        &self,
        request: reqwest::RequestBuilder,
        operation: &str,
        retry_read: bool,
    ) -> Result<Value> {
        for attempt in 0..MAX_READ_ATTEMPTS {
            let request = request.try_clone().ok_or_else(|| {
                AppError::Network(format!("{operation}: request could not be cloned"))
            })?;
            match request.send().await {
                Ok(response) => {
                    if retry_read
                        && is_transient(response.status())
                        && attempt + 1 < MAX_READ_ATTEMPTS
                    {
                        // Drop the response without buffering it: reading the
                        // body here would let a hostile server send an
                        // unbounded stream on every transient status.
                        drop(response);
                        tokio::time::sleep(Duration::from_millis(20 * (attempt as u64 + 1))).await;
                        continue;
                    }
                    return parse_response(response, operation, !retry_read).await;
                }
                Err(error) => {
                    if retry_read && error.is_timeout() && attempt + 1 < MAX_READ_ATTEMPTS {
                        tokio::time::sleep(Duration::from_millis(20 * (attempt as u64 + 1))).await;
                        continue;
                    }
                    let detail = error.to_string();
                    return Err(if retry_read {
                        AppError::Network(format!("{operation}: {detail}"))
                    } else {
                        AppError::Network(format!("{operation} outcome uncertain: {detail}"))
                    });
                }
            }
        }
        Err(AppError::Network(format!(
            "{operation}: request attempts exhausted"
        )))
    }

    async fn mutation_request(
        &self,
        request: reqwest::RequestBuilder,
        operation: &str,
    ) -> Result<Value> {
        self.send_json(request, operation, false).await
    }

    pub fn request_timeout(&self) -> Duration {
        self.timeout
    }
}

fn is_transient(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::REQUEST_TIMEOUT
            | StatusCode::TOO_MANY_REQUESTS
            | StatusCode::INTERNAL_SERVER_ERROR
            | StatusCode::BAD_GATEWAY
            | StatusCode::SERVICE_UNAVAILABLE
            | StatusCode::GATEWAY_TIMEOUT
    )
}

fn network_error(operation: &str, detail: impl Into<String>, mutation: bool) -> AppError {
    let detail = detail.into();
    if mutation {
        AppError::Network(format!("{operation} outcome uncertain: {detail}"))
    } else {
        AppError::Network(format!("{operation}: {detail}"))
    }
}

async fn parse_response(response: Response, operation: &str, mutation: bool) -> Result<Value> {
    let status = response.status();
    let body = read_body_bounded(response, MAX_RESPONSE_BYTES)
        .await
        .map_err(|error| network_error(operation, error.to_string(), mutation))?;
    if !status.is_success() {
        let detail = server_detail(&body);
        let message = format!("{operation} ({status}): {detail}");
        return Err(match status {
            StatusCode::UNAUTHORIZED => AppError::Authentication(message),
            StatusCode::FORBIDDEN => AppError::Authorization(message),
            StatusCode::NOT_FOUND => AppError::Authorization(format!(
                "{message}; this Lemmy capability may be unsupported"
            )),
            _ if mutation => AppError::Network(format!("{message}; mutation outcome uncertain")),
            _ => AppError::Network(message),
        });
    }
    if body.trim().is_empty() {
        return Err(network_error(operation, "empty response body", mutation));
    }
    serde_json::from_str(&body).map_err(|error| {
        network_error(
            operation,
            format!("invalid JSON response: {error}"),
            mutation,
        )
    })
}

/// Read a response body into a string, refusing anything over `max` bytes.
/// `Response::text` buffers without limit and the request timeout only bounds
/// duration, so an endless body could otherwise exhaust memory.
async fn read_body_bounded(mut response: Response, max: usize) -> Result<String> {
    if let Some(length) = response.content_length()
        && length as usize > max
    {
        return Err(AppError::Network(format!(
            "response body too large ({length} bytes)"
        )));
    }
    let mut bytes = Vec::new();
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                if bytes.len().saturating_add(chunk.len()) > max {
                    return Err(AppError::Network(format!(
                        "response body exceeds the {max}-byte limit"
                    )));
                }
                bytes.extend_from_slice(&chunk);
            }
            Ok(None) => break,
            Err(error) => {
                return Err(AppError::Network(format!(
                    "could not read response body: {error}"
                )));
            }
        }
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn server_detail(body: &str) -> String {
    let detail = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|value| {
            value
                .get("error")
                .or_else(|| value.get("message"))
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .unwrap_or_else(|| {
            if body.trim().is_empty() {
                "empty server response".into()
            } else {
                body.trim().into()
            }
        });
    // The body is attacker-controlled: it must never reach the status bar or
    // the log unbounded or carrying terminal/bidi control characters (which
    // could forge log lines or reorder TUI text). Strip control characters,
    // collapse newlines, and hard-truncate.
    crate::text::clean_text(&detail)
        .replace(['\n', '\r'], " ")
        .chars()
        .take(MAX_ERROR_DETAIL_CHARS)
        .collect()
}

fn number(value: &Value, key: &str) -> i64 {
    value.get(key).and_then(Value::as_i64).unwrap_or_default()
}
fn metric(value: &Value, fallback: &Value, key: &str) -> i64 {
    value
        .get(key)
        .and_then(Value::as_i64)
        .or_else(|| fallback.get(key).and_then(Value::as_i64))
        .unwrap_or_default()
}
fn string(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

fn normalize_post(value: &Value) -> Result<PostView> {
    let post = value.get("post").unwrap_or(value);
    let url = string(post, "url").and_then(|raw| Url::parse(&raw).ok());
    Ok(PostView {
        id: PostId(number(post, "id")),
        title: crate::text::clean_text(
            string(post, "name")
                .or_else(|| string(post, "title"))
                .unwrap_or_default()
                .as_str(),
        ),
        body: string(post, "body").map(|body| crate::text::clean_text(&body)),
        url,
        community_id: crate::domain::CommunityId(number(post, "community_id")),
        creator_id: UserId(number(post, "creator_id")),
        score: metric(value.get("counts").unwrap_or(&Value::Null), post, "score"),
        comments: metric(
            value.get("counts").unwrap_or(&Value::Null),
            post,
            "comments",
        ),
        published: string(post, "published"),
    })
}

fn normalize_comment(value: &Value, post_id: PostId) -> CommentView {
    let comment = value.get("comment").unwrap_or(value);
    let creator = value.get("creator").unwrap_or(&Value::Null);
    CommentView {
        id: CommentId(number(comment, "id")),
        post_id,
        content: crate::text::clean_text(string(comment, "content").unwrap_or_default().as_str()),
        creator_id: UserId(number(comment, "creator_id")),
        creator_name: crate::text::clean_text(
            string(creator, "name")
                .unwrap_or_else(|| "unknown".to_owned())
                .as_str(),
        ),
        score: metric(
            value.get("counts").unwrap_or(&Value::Null),
            comment,
            "score",
        ),
    }
}

#[async_trait::async_trait]
impl LemmyApi for HttpLemmyApi {
    async fn site(&self, ctx: &ProfileContext) -> Result<SiteInfo> {
        let response = self
            .read_json(
                self.auth_request(self.client.get(self.endpoint(ctx, "site")?), ctx),
                "site",
            )
            .await?;
        let site_view = response
            .get("site_view")
            .and_then(Value::as_object)
            .ok_or_else(|| {
                AppError::Network("site: response did not contain site_view metadata".into())
            })?;
        let site = site_view
            .get("site")
            .and_then(Value::as_object)
            .ok_or_else(|| {
                AppError::Network("site: response did not contain site metadata".into())
            })?;
        let name = site
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| AppError::Network("site: response did not contain site name".into()))?;
        // The Lemmy version is a top-level field of `GetSiteResponse`
        // (`version`); some older shapes nested it inside `site_view.site`,
        // so accept both. Reading the wrong location made the whole call
        // error, which silently skipped the login user-id enrichment.
        let version = response
            .get("version")
            .and_then(Value::as_str)
            .or_else(|| site.get("version").and_then(Value::as_str))
            .ok_or_else(|| {
                AppError::Network("site: response did not contain site version".into())
            })?;
        // The authenticated user's id lives in the `my_user` block, which the
        // server includes only when the request carries a session.
        let user_id = response
            .get("my_user")
            .and_then(|my_user| my_user.get("local_user_view"))
            .and_then(|view| view.get("local_user"))
            .and_then(|local_user| local_user.get("person_id"))
            .and_then(Value::as_i64)
            .map(UserId);
        Ok(SiteInfo {
            name: crate::text::clean_text(name),
            version: version.to_owned(),
            user_id,
        })
    }

    async fn feed(&self, ctx: &ProfileContext, query: FeedQuery) -> Result<Page<PostView>> {
        let mut request = self
            .client
            .get(self.endpoint(ctx, "post/list")?)
            .query(&[("sort", query.sort), ("type_", "All".into())]);
        if let Some(cursor) = query.page {
            request = request.query(&[("page_cursor", cursor)]);
        }
        if let Some(limit) = query.limit {
            request = request.query(&[("limit", limit)]);
        }
        if let Some(community) = query.community {
            request = request.query(&[("community_id", community.0)]);
        }
        if let Some(search) = query.search {
            request = request.query(&[("q", search)]);
        }
        let response = self
            .read_json(self.auth_request(request, ctx), "feed")
            .await?;
        let items = response
            .get("posts")
            .and_then(Value::as_array)
            .ok_or_else(|| AppError::Network("feed: response did not contain posts".into()))?
            .iter()
            .take(MAX_ARRAY_ITEMS)
            .map(normalize_post)
            .collect::<Result<Vec<_>>>()?;
        let next_page = response
            .get("next_page")
            .and_then(Value::as_str)
            .map(str::to_owned);
        Ok(Page { items, next_page })
    }

    async fn post(&self, ctx: &ProfileContext, id: PostId) -> Result<PostDetail> {
        let request = self.auth_request(
            self.client
                .get(self.endpoint(ctx, "post")?)
                .query(&[("id", id.0)]),
            ctx,
        );
        let response = self.read_json(request, "post").await?;
        let post = normalize_post(response.get("post_view").unwrap_or(&response))?;
        let comments = response
            .get("comments")
            .and_then(Value::as_array)
            .map(|comments| {
                comments
                    .iter()
                    .take(MAX_ARRAY_ITEMS)
                    .map(|comment| normalize_comment(comment, post.id))
                    .collect()
            })
            .unwrap_or_default();
        Ok(PostDetail { post, comments })
    }

    /// Lemmy serves a post's thread from `comment/list`, not from the post
    /// detail response. `type_=All` is required by at least lemmy.ml (a
    /// missing value returns an empty thread); `max_depth` asks the server
    /// for the full tree in one flat list.
    async fn comments(&self, ctx: &ProfileContext, post_id: PostId) -> Result<Vec<CommentView>> {
        let request = self.auth_request(
            self.client
                .get(self.endpoint(ctx, "comment/list")?)
                .query(&[
                    ("post_id", post_id.0.to_string()),
                    ("type_", "All".to_string()),
                    ("sort", "Top".to_string()),
                    ("max_depth", "10".to_string()),
                ]),
            ctx,
        );
        let response = self.read_json(request, "comments").await?;
        Ok(response
            .get("comments")
            .and_then(Value::as_array)
            .map(|comments| {
                comments
                    .iter()
                    .take(MAX_ARRAY_ITEMS)
                    .map(|comment| normalize_comment(comment, post_id))
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn login(&self, request: LoginRequest) -> Result<Session> {
        let profile = request.profile.clone();
        let instance_url = request.instance_url.clone();
        let endpoint_base = if let Some(mut base) = self.base_url.clone() {
            if request.instance_url.path() != "/" && !request.instance_url.path().is_empty() {
                base.set_path(request.instance_url.path());
            }
            base
        } else {
            request.instance_url
        };
        let endpoint = self.endpoint_from(endpoint_base, "user/login")?;
        let body = json!({ "username_or_email": request.username, "password": request.password.expose_secret() });
        let response = self
            .mutation_request(self.client.post(endpoint).json(&body), "login")
            .await?;
        let token = response.get("jwt").and_then(Value::as_str).ok_or_else(|| {
            AppError::Authentication("login response did not contain a session token".into())
        })?;
        let mut session = Session {
            token: crate::domain::SecretString::from(token),
            // Lemmy's login response carries no user identity (only the JWT
            // and a registration flag); the id is derived from the
            // authenticated `/site` `my_user` block below.
            user_id: UserId(0),
        };
        let context = ProfileContext {
            profile: Profile {
                id: profile,
                instance_url,
                account_label: None,
            },
            session: Some(session.clone()),
        };
        // Enrichment is best-effort: a flaky `/site` after a successful login
        // must not fail the login itself, and some servers omit `my_user`.
        if let Ok(site) = self.site(&context).await
            && let Some(user_id) = site.user_id
        {
            session.user_id = user_id;
        } else {
            tracing::warn!("could not derive login user id from /site my_user");
        }
        Ok(session)
    }

    async fn mutate(&self, ctx: &ProfileContext, mutation: Mutation) -> Result<MutationResult> {
        let (path, body) = mutation_request_parts(&mutation)?;
        let body = self.auth_body(ctx, body);
        let request =
            self.auth_request(self.client.post(self.endpoint(ctx, path)?).json(&body), ctx);
        let response = self.mutation_request(request, path).await?;
        let success = response
            .get("success")
            .and_then(Value::as_bool)
            .unwrap_or(response.get("error").is_none());
        let comment = response.get("comment_view").map(|value| {
            let comment = value.get("comment").unwrap_or(value);
            normalize_comment(value, PostId(number(comment, "post_id")))
        });
        Ok(MutationResult {
            success,
            post: response.get("post_view").map(normalize_post).transpose()?,
            comment,
            message: string(&response, "message"),
        })
    }
}

fn mutation_request_parts(mutation: &Mutation) -> Result<(&'static str, Value)> {
    let result = match mutation {
        Mutation::VotePost { id, score } => {
            ("post/like", json!({ "post_id": id.0, "score": score }))
        }
        Mutation::VoteComment { id, score } => (
            "comment/like",
            json!({ "comment_id": id.0, "score": score }),
        ),
        Mutation::SavePost { id, saved } => {
            ("post/save", json!({ "post_id": id.0, "save": saved }))
        }
        Mutation::Subscribe {
            community,
            subscribed,
        } => (
            "community/follow",
            json!({ "community_id": community.0, "follow": subscribed }),
        ),
        Mutation::CreatePost(request) => (
            "post",
            json!({ "community_id": request.community.0, "name": request.name, "body": request.body, "url": request.url.as_ref().map(Url::as_str) }),
        ),
        Mutation::EditPost(request) => (
            "post",
            json!({ "post_id": request.id.0, "name": request.name, "body": request.body, "url": request.url.as_ref().map(Url::as_str) }),
        ),
        Mutation::DeletePost(id) => ("post/delete", json!({ "post_id": id.0, "deleted": true })),
        Mutation::CreateComment(request) => (
            "comment",
            json!({ "post_id": request.post.0, "content": request.content }),
        ),
        Mutation::EditComment(request) => (
            "comment",
            json!({ "comment_id": request.id.0, "content": request.content }),
        ),
        Mutation::DeleteComment(id) => (
            "comment/delete",
            json!({ "comment_id": id.0, "deleted": true }),
        ),
    };
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::server_detail;

    #[test]
    fn server_detail_extracts_and_truncates_error_text() {
        let long = "x".repeat(500);
        let detail = server_detail(&format!(r#"{{"error":"{long}"}}"#));
        assert_eq!(
            detail.chars().count(),
            200,
            "server detail must be truncated"
        );
    }

    #[test]
    fn server_detail_strips_terminal_and_bidi_control_characters() {
        // A malicious instance can echo the token it received or craft an
        // error body with escape sequences and bidi overrides; the detail
        // must never carry them into the status bar or the log.
        let detail = server_detail("boom\x1b[2J\x1b]0;evil\x07\u{202E}overridden");
        assert!(!detail.contains('\u{1b}'), "ESC must be stripped");
        assert!(
            !detail.contains('\u{202E}'),
            "bidi overrides must be stripped"
        );
        assert!(detail.contains("boom"), "the plain text survives");
        assert!(!detail.contains('\n'), "newlines must be collapsed");
    }

    #[test]
    fn server_detail_falls_back_to_the_trimmed_body() {
        let detail = server_detail("not json at all");
        assert_eq!(detail, "not json at all");
    }
}
