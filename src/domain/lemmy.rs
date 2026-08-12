use url::Url;

macro_rules! id_type {
    ($name:ident) => {
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(pub i64);
    };
}

id_type!(PostId);
id_type!(CommentId);
id_type!(CommunityId);
id_type!(UserId);

/// Fields required to create a post through the adapter.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct CreatePostRequest {
    pub community: CommunityId,
    pub name: String,
    pub body: Option<String>,
    pub url: Option<Url>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct EditPostRequest {
    pub id: PostId,
    pub name: Option<String>,
    pub body: Option<String>,
    pub url: Option<Url>,
}

/// Fields required to create a comment.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct CreateCommentRequest {
    pub post: PostId,
    pub content: String,
}

/// Fields that may be changed on an existing comment.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct EditCommentRequest {
    pub id: CommentId,
    pub content: String,
}

/// A user-visible mutation independent of Lemmy API-version request shapes.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum Mutation {
    VotePost {
        id: PostId,
        score: i8,
    },
    VoteComment {
        id: CommentId,
        score: i8,
    },
    SavePost {
        id: PostId,
        saved: bool,
    },
    Subscribe {
        community: CommunityId,
        subscribed: bool,
    },
    CreatePost(CreatePostRequest),
    EditPost(EditPostRequest),
    DeletePost(PostId),
    CreateComment(CreateCommentRequest),
    EditComment(EditCommentRequest),
    DeleteComment(CommentId),
}
