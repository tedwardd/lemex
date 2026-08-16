use crate::{
    api::{CommentView, MutationResult, Page, PostDetail, PostView},
    cache::{Draft, DraftId},
    domain::{DownloadId, Mutation, PostId, ProfileId},
    error::Result,
    input::Command,
};
use url::Url;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum RequestIdentity {
    Feed,
    Post(PostId),
    Comments(PostId),
    Communities,
    Mutation(Mutation),
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct RequestToken {
    pub generation: u64,
    pub identity: RequestIdentity,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PendingAction {
    DeletePost {
        profile: ProfileId,
        id: PostId,
    },
    Mutation {
        profile: ProfileId,
        mutation: Mutation,
        draft: Option<DraftId>,
    },
    DeleteDownload {
        id: DownloadId,
        path: std::path::PathBuf,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProfileDraft {
    pub id: ProfileId,
    pub instance_url: Url,
    pub account_label: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProfileCommand {
    Switch(ProfileId),
    List,
    New(ProfileDraft),
    Login,
    Logout,
    WhoAmI,
    Delete(ProfileId),
}

/// Which direction a page flip moved, so the apply arm restores cursor and
/// history state correctly on failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PageDirection {
    Next,
    Previous,
}

#[derive(Debug)]
pub enum ApiResult {
    Feed {
        profile: ProfileId,
        request: RequestToken,
        result: Result<Page<PostView>>,
        stale: bool,
    },
    /// A completed page flip (`n`/`>` or `p`/`<`). Carries the cursor state
    /// the flip started from so a failed flip can be rolled back: `cursor`
    /// is the page that was being fetched, `previous` is the page that was
    /// on screen before the flip (already pushed onto `page_history` for a
    /// Next flip, or still on it for a Previous flip).
    FeedPage {
        profile: ProfileId,
        request: RequestToken,
        result: Result<Page<PostView>>,
        stale: bool,
        cursor: Option<String>,
        previous: Option<String>,
        direction: PageDirection,
    },
    Post {
        profile: ProfileId,
        request: RequestToken,
        result: Result<PostDetail>,
        stale: bool,
    },
    Mutation {
        profile: ProfileId,
        request: RequestToken,
        draft: Option<DraftId>,
        mutation: Mutation,
        result: Box<Result<MutationResult>>,
    },
    Comments {
        profile: ProfileId,
        request: RequestToken,
        post: PostId,
        result: Result<Vec<CommentView>>,
        stale: bool,
    },
    Communities {
        profile: ProfileId,
        request: RequestToken,
        result: Result<crate::api::Page<crate::api::CommunityView>>,
        stale: bool,
    },
}

impl ApiResult {
    /// The request identity this result belongs to, when it carries one.
    /// Completion draining uses it to retire the in-flight handle.
    pub fn identity(&self) -> RequestIdentity {
        match self {
            ApiResult::Feed { request, .. }
            | ApiResult::FeedPage { request, .. }
            | ApiResult::Post { request, .. }
            | ApiResult::Mutation { request, .. }
            | ApiResult::Comments { request, .. }
            | ApiResult::Communities { request, .. } => request.identity.clone(),
        }
    }
}

#[derive(Debug)]
pub enum DownloadsAction {
    Search(String),
    Reopen,
    Reveal,
    CopyPath,
    Retry,
    Cancel,
    Delete,
    ResolveCollision { overwrite: bool },
    Close,
}

#[derive(Debug)]
pub enum AppAction {
    Input(Command),
    /// Terminal size changed; the feed page size is sized to the primary
    /// placement adapt to it.
    Resize {
        width: u16,
        height: u16,
    },
    Profile(ProfileCommand),
    SubmitDraft(DraftId),
    DiscardDraft(DraftId),
    OpenSelected,
    OpenCommunity(crate::domain::CommunityId),
    LoadMore,
    Back,
    DeletePost(PostId),
    Mutate(Mutation),
    Confirm,
    Cancel,
    ApiResult(Box<ApiResult>),
    Media,
    DownloadMedia,
    ShowDownloads,
    Downloads(DownloadsAction),
    Tick,
    Quit,
}

impl From<Command> for AppAction {
    fn from(command: Command) -> Self {
        Self::Input(command)
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PendingDraft {
    pub draft: Option<Draft>,
}
