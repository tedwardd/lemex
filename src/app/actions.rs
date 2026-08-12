use crate::{
    api::{CommentView, MutationResult, Page, PostDetail, PostView},
    cache::{Draft, DraftId},
    domain::{Mutation, PostId, ProfileId},
    error::Result,
    input::Command,
};
use url::Url;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum RequestIdentity {
    Feed,
    Post(PostId),
    Comments(PostId),
    Mutation(Mutation),
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct RequestToken {
    pub generation: u64,
    pub identity: RequestIdentity,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PendingAction {
    DeletePost { profile: ProfileId, id: PostId },
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
    Feed { profile: ProfileId, request: RequestToken, result: Result<Page<PostView>>, stale: bool },
    Post { profile: ProfileId, request: RequestToken, result: Result<PostDetail> },
    Mutation { profile: ProfileId, request: RequestToken, draft: Option<DraftId>, mutation: Mutation, result: Result<MutationResult> },
    Comments { profile: ProfileId, request: RequestToken, post: PostId, result: Result<Vec<CommentView>> },
}

#[derive(Debug)]
pub enum AppAction {
    Input(Command),
    Profile(ProfileCommand),
    SubmitDraft(DraftId),
    OpenSelected,
    Back,
    DeletePost(PostId),
    Confirm,
    Cancel,
    ApiResult(ApiResult),
    Tick,
    Quit,
}

impl From<Command> for AppAction {
    fn from(command: Command) -> Self { Self::Input(command) }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PendingDraft {
    pub draft: Option<Draft>,
}
