pub mod fixtures;
mod http;

pub use http::HttpLemmyApi;

use crate::domain::{CommunityId, Mutation, PostId, ProfileContext, ProfileId, Session, UserId};
use crate::error::Result;
use std::fmt;
use url::Url;

#[async_trait::async_trait]
pub trait LemmyApi: Send + Sync {
    async fn site(&self, ctx: &ProfileContext) -> Result<SiteInfo>;
    async fn feed(&self, ctx: &ProfileContext, query: FeedQuery) -> Result<Page<PostView>>;
    async fn post(&self, ctx: &ProfileContext, id: PostId) -> Result<PostDetail>;
    async fn login(&self, request: LoginRequest) -> Result<Session>;
    async fn mutate(&self, ctx: &ProfileContext, mutation: Mutation) -> Result<MutationResult>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FeedQuery {
    pub sort: String,
    pub page: Option<u32>,
    pub limit: Option<u32>,
    pub community: Option<CommunityId>,
    pub search: Option<String>,
}

impl FeedQuery {
    pub fn home() -> Self {
        Self {
            sort: "Active".into(),
            page: None,
            limit: None,
            community: None,
            search: None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub next_page: Option<u32>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PostView {
    pub id: PostId,
    pub title: String,
    pub body: Option<String>,
    pub url: Option<Url>,
    pub community_id: CommunityId,
    pub creator_id: crate::domain::UserId,
    pub score: i64,
    pub comments: i64,
    pub published: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommentView {
    pub id: crate::domain::CommentId,
    pub post_id: PostId,
    pub content: String,
    pub creator_id: crate::domain::UserId,
    pub score: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PostDetail {
    pub post: PostView,
    pub comments: Vec<CommentView>,
}

#[derive(Clone, Eq, PartialEq)]
pub struct LoginRequest {
    /// Profile whose credential store receives the session on success.
    pub profile: ProfileId,
    pub instance_url: Url,
    pub username: String,
    pub password: crate::domain::SecretString,
}

impl fmt::Debug for LoginRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LoginRequest")
            .field("profile", &self.profile)
            .field("instance_url", &self.instance_url)
            .field("username", &self.username)
            .field("password", &"[REDACTED]")
            .finish()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SiteInfo {
    pub name: String,
    pub version: String,
    /// The authenticated user's id, present only when `/site` is called with
    /// a session and the server returned a `my_user` block.
    pub user_id: Option<UserId>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MutationResult {
    pub success: bool,
    pub post: Option<PostView>,
    pub comment: Option<CommentView>,
    pub message: Option<String>,
}
