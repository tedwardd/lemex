use lemmy::api::fixtures::{
    anonymous_context, authenticated_context, fixture_api, fixture_api_with_body,
    fixture_api_with_status, fixture_api_with_status_count, login_fixture_api,
    timeout_fixture_api, truncated_body_fixture_api,
};
use lemmy::api::{FeedQuery, LemmyApi, LoginRequest};
use lemmy::{AppError, Mutation, PostId, SecretString, UserId};

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

#[tokio::test]
async fn truncated_success_body_is_a_network_error() {
    let api = truncated_body_fixture_api();
    let result = api.feed(&anonymous_context(), FeedQuery::home()).await;
    assert!(matches!(result, Err(AppError::Network(message)) if message.contains("body")));
}

#[tokio::test]
async fn empty_success_body_is_not_treated_as_success() {
    let api = fixture_api_with_body("");
    let result = api.feed(&anonymous_context(), FeedQuery::home()).await;
    assert!(matches!(result, Err(AppError::Network(message)) if message.contains("empty response body")));
}

#[tokio::test]
async fn missing_endpoint_reports_actionable_unsupported_error() {
    let api = fixture_api_with_status("/api/v3/post/list", 404);
    let result = api.feed(&anonymous_context(), FeedQuery::home()).await;
    assert!(matches!(result, Err(AppError::Authorization(message)) if message.contains("unsupported")));
}

#[tokio::test]
async fn truncated_mutation_body_marks_outcome_uncertain() {
    let api = truncated_body_fixture_api();
    let result = api
        .mutate(&authenticated_context(), Mutation::DeletePost(PostId(1)))
        .await;
    assert!(matches!(result, Err(AppError::Network(message)) if message.contains("uncertain")));
}

#[tokio::test]
async fn negative_scores_and_comments_are_preserved() {
    let api = fixture_api_with_body(
        r#"{"posts":[{"post":{"id":1,"name":"negative","community_id":1,"creator_id":1,"score":-4,"comments":-3},"counts":{"score":-2,"comments":-5}}]}"#,
    );
    let page = api.feed(&anonymous_context(), FeedQuery::home()).await.unwrap();
    assert_eq!(page.items[0].score, -2);
    assert_eq!(page.items[0].comments, -5);
}

#[tokio::test]
async fn mutation_comment_uses_response_post_id_and_negative_score() {
    let api = fixture_api_with_body(
        r#"{"comment_view":{"comment":{"id":2,"post_id":42,"content":"negative","creator_id":1},"counts":{"score":-3}}}"#,
    );
    let result = api
        .mutate(&authenticated_context(), Mutation::VoteComment { id: lemmy::CommentId(2), score: -1 })
        .await
        .unwrap();
    let comment = result.comment.unwrap();
    assert_eq!(comment.post_id, PostId(42));
    assert_eq!(comment.score, -3);
}

#[tokio::test]
async fn login_preserves_instance_base_path() {
    let (api, instance_url) = login_fixture_api("/lemmy/");
    let session = api
        .login(LoginRequest {
            profile: lemmy::ProfileId::from("fixture"),
            instance_url,
            username: "fixture-user".into(),
            password: SecretString::from("fixture-password"),
        })
        .await
        .unwrap();
    assert_eq!(session.user_id, UserId(1));
}

#[tokio::test]
async fn unknown_site_version_does_not_claim_capabilities() {
    let api = fixture_api_with_body(
        r#"{"site_view":{"site":{"name":"Unknown","version":"1.0.0"},"local_site":{}}}"#,
    );
    let site = api.site(&anonymous_context()).await.unwrap();
    assert!(!site.capabilities.supports_login);
    assert!(!site.capabilities.supports_feed);
    assert!(!site.capabilities.supports_post);
    assert!(!site.capabilities.supports_mutations);
}

#[tokio::test]
async fn malformed_site_response_is_rejected() {
    let api = fixture_api_with_body(r#"{}"#);
    let result = api.site(&anonymous_context()).await;
    assert!(matches!(result, Err(AppError::Network(message)) if message.contains("site_view")));
}

#[tokio::test]
async fn non_transient_501_is_not_retried() {
    let (api, requests) = fixture_api_with_status_count(501);
    let result = api.feed(&anonymous_context(), FeedQuery::home()).await;
    assert!(result.is_err());
    assert_eq!(requests.load(std::sync::atomic::Ordering::SeqCst), 1);
}
