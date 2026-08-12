use super::{
    Capabilities, CommentView, FeedQuery, LemmyApi, LoginRequest, MutationResult, Page, PostDetail,
    PostView, SiteInfo,
};
use crate::domain::{CommentId, Mutation, PostId, ProfileContext, Session, UserId};
use crate::error::{AppError, Result};
use reqwest::{Client, Response, StatusCode};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use url::Url;

const MAX_READ_ATTEMPTS: usize = 3;

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
            .timeout(timeout)
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
                        if let Err(error) = response.bytes().await {
                            return Err(network_error(
                                operation,
                                format!("could not read response body: {error}"),
                                false,
                            ));
                        }
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
    let body = response.text().await.map_err(|error| {
        network_error(
            operation,
            format!("could not read response body: {error}"),
            mutation,
        )
    })?;
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

fn server_detail(body: &str) -> String {
    serde_json::from_str::<Value>(body)
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
        })
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
        title: string(post, "name")
            .or_else(|| string(post, "title"))
            .unwrap_or_default(),
        body: string(post, "body"),
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
    CommentView {
        id: CommentId(number(comment, "id")),
        post_id,
        content: string(comment, "content").unwrap_or_default(),
        creator_id: UserId(number(comment, "creator_id")),
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
        let version = site.get("version").and_then(Value::as_str).ok_or_else(|| {
            AppError::Network("site: response did not contain site version".into())
        })?;
        let recognized_version = version.split('.').next() == Some("0")
            && matches!(version.split('.').nth(1), Some("18" | "19"));
        let observed_v3_shape = site_view.get("local_site").is_some_and(Value::is_object);
        let core_v3 = recognized_version && observed_v3_shape;
        Ok(SiteInfo {
            name: name.to_owned(),
            version: version.to_owned(),
            capabilities: Capabilities {
                supports_login: core_v3,
                supports_feed: core_v3,
                supports_post: core_v3,
                supports_mutations: core_v3,
            },
        })
    }

    async fn feed(&self, ctx: &ProfileContext, query: FeedQuery) -> Result<Page<PostView>> {
        let mut request = self
            .client
            .get(self.endpoint(ctx, "post/list")?)
            .query(&[("sort", query.sort), ("type_", "All".into())]);
        if let Some(page) = query.page {
            request = request.query(&[("page", page)]);
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
            .map(normalize_post)
            .collect::<Result<Vec<_>>>()?;
        let next_page = response
            .get("next_page")
            .and_then(Value::as_u64)
            .map(|value| value as u32);
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
                    .map(|comment| normalize_comment(comment, post.id))
                    .collect()
            })
            .unwrap_or_default();
        Ok(PostDetail { post, comments })
    }

    async fn login(&self, request: LoginRequest) -> Result<Session> {
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
        let user_id = response
            .get("person_id")
            .and_then(Value::as_i64)
            .unwrap_or_default();
        Ok(Session {
            token: crate::domain::SecretString::from(token),
            user_id: UserId(user_id),
        })
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
