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

#[derive(Debug)]
pub enum ApiResult {
    Feed {
        profile: ProfileId,
        request: RequestToken,
        result: Result<Page<PostView>>,
        stale: bool,
    },
    Post {
        profile: ProfileId,
        request: RequestToken,
        result: Result<PostDetail>,
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
    },
    Communities {
        profile: ProfileId,
        request: RequestToken,
        result: Result<crate::api::Page<crate::api::CommunityView>>,
    },
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
