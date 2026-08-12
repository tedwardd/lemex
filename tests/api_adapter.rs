use lemmy::api::fixtures::{
    anonymous_context, authenticated_context, fixture_api, fixture_api_with_status,
    timeout_fixture_api,
};
use lemmy::api::{FeedQuery, LemmyApi};
use lemmy::{AppError, Mutation, PostId};

#[tokio::test]
async fn feed_response_normalizes_into_domain_posts() {
    let api = fixture_api("feed.json");
    let page = api
        .feed(&anonymous_context(), FeedQuery::home())
        .await
        .unwrap();
    assert_eq!(page.items.len(), 2);
    assert_eq!(page.items[0].title, "Fixture post");
}

#[tokio::test]
async fn expired_session_is_classified_as_authentication_error() {
    let api = fixture_api_with_status("/api/v3/post/list", 401);
    let result = api.feed(&anonymous_context(), FeedQuery::home()).await;
    assert!(matches!(result, Err(AppError::Authentication(_))));
}

#[tokio::test]
async fn mutation_timeout_is_not_reported_as_confirmed_failure() {
    let api = timeout_fixture_api();
    let result = api
        .mutate(&authenticated_context(), Mutation::DeletePost(PostId(1)))
        .await;
    assert!(matches!(result, Err(AppError::Network(message)) if message.contains("uncertain")));
}
