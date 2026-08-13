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
    async fn comments(&self, ctx: &ProfileContext, post_id: PostId) -> Result<Vec<CommentView>>;
    async fn login(&self, request: LoginRequest) -> Result<Session>;
    async fn mutate(&self, ctx: &ProfileContext, mutation: Mutation) -> Result<MutationResult>;
    /// List communities (`community/list`): the whole instance (`All`), only
    /// local ones (`Local`), or the logged-in user's subscriptions
    /// (`Subscribed`). The default refuses so test doubles that never list
    /// communities stay untouched.
    async fn communities(
        &self,
        _ctx: &ProfileContext,
        _query: CommunityQuery,
    ) -> Result<Page<CommunityView>> {
        Err(crate::error::AppError::Network(
            "communities: unsupported by this API".into(),
        ))
    }
}

/// Which listing a feed shows. Lemmy's `type_` parameter: the whole
/// instance (`All`), only this instance's communities (`Local`), or the
/// logged-in user's subscribed communities (`Subscribed`).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Serialize)]
pub enum FeedListing {
    #[default]
    All,
    Local,
    Subscribed,
}

/// Sort orders Lemmy accepts for post listings (the `sort` query parameter),
/// in canonical spelling. `:sort` accepts any of these case-insensitively.
pub const FEED_SORTS: &[&str] = &[
    "Active",
    "Hot",
    "New",
    "Old",
    "TopHour",
    "TopSixHour",
    "TopTwelveHour",
    "TopDay",
    "TopWeek",
    "TopMonth",
    "TopYear",
    "TopAll",
    "MostComments",
    "NewComments",
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FeedQuery {
    pub sort: String,
    /// Opaque page cursor (Lemmy 0.19+ returns `next_page` as an opaque
    /// string sent back as `page_cursor`); `None` starts at the first page.
    pub page: Option<String>,
    pub limit: Option<u32>,
    pub community: Option<CommunityId>,
    pub search: Option<String>,
    pub listing: FeedListing,
}

impl FeedQuery {
    /// The web UI default; large enough to fill a typical terminal without
    /// requiring an immediate next-page flip.
    pub const DEFAULT_LIMIT: u32 = 20;

    pub fn home() -> Self {
        Self {
            sort: "Active".into(),
            page: None,
            limit: Some(Self::DEFAULT_LIMIT),
            community: None,
            search: None,
            listing: FeedListing::All,
        }
    }

    /// The logged-in user's subscribed communities (`type_=Subscribed`), the
    /// feed Lemmy's web UI shows for `listingType=Subscribed`. Needs a
    /// session; without one the server has no subscriptions to show.
    pub fn subscribed() -> Self {
        Self {
            listing: FeedListing::Subscribed,
            ..Self::home()
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Page<T> {
    pub items: Vec<T>,
    /// Opaque cursor for the next page, passed back as `page_cursor`.
    pub next_page: Option<String>,
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
    /// Display name of the comment author, for thread rendering.
    pub creator_name: String,
    pub score: i64,
}

/// A community as returned by `community/list`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommunityView {
    pub id: CommunityId,
    pub name: String,
    pub title: Option<String>,
    pub subscribers: i64,
    /// Whether the session is subscribed to this community (server-reported).
    pub subscribed: bool,
}

/// Query for the community list: which listing, and how many rows.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommunityQuery {
    pub listing: FeedListing,
    pub limit: Option<u32>,
}

impl CommunityQuery {
    /// The server's maximum, like the feed: 50 rows per fetch.
    pub const DEFAULT_LIMIT: u32 = 50;
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
