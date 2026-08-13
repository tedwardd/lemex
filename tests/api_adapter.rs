use levim::api::fixtures::{
    anonymous_context, authenticated_context, fixture_api, fixture_api_recording_user_agent,
    fixture_api_with_body, fixture_api_with_status, fixture_api_with_status_count,
    login_fixture_api, timeout_fixture_api, truncated_body_fixture_api,
};
use levim::api::{FeedQuery, LemmyApi, LoginRequest};
use levim::{AppError, CommentId, Mutation, PostId, SecretString, UserId};

#[tokio::test]
async fn feed_response_carries_the_opaque_next_page_cursor() {
    // Lemmy 0.19+ returns `next_page` as an opaque string cursor, not a
    // number; the adapter must carry it verbatim so `>` can flip pages.
    let body = r#"{"posts":[{"post":{"id":1,"name":"Fixture post","body":null,"community_id":1,"creator_id":1,"published":"2026-01-01T00:00:00Z"},"counts":{"score":1,"comments":0}}],"next_page":"P303839a"}"#;
    let api = fixture_api_with_body(body);
    let page = api
        .feed(&anonymous_context(), FeedQuery::home())
        .await
        .unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(
        page.next_page.as_deref(),
        Some("P303839a"),
        "the opaque next_page cursor must survive normalization"
    );
}

#[test]
fn home_feed_requests_twenty_posts() {
    assert_eq!(
        FeedQuery::home().limit,
        Some(FeedQuery::DEFAULT_LIMIT),
        "the home feed must request a full screen of posts by default"
    );
}

#[tokio::test]
async fn client_sends_descriptive_user_agent() {
    let (api, user_agent) = fixture_api_recording_user_agent("{}");
    let _ = api.feed(&anonymous_context(), FeedQuery::home()).await;
    let captured = user_agent
        .lock()
        .expect("fixture UA lock")
        .clone()
        .expect("fixture must have recorded a User-Agent header");
    assert!(
        captured.starts_with("levim-client/"),
        "client must send a descriptive User-Agent identifying the app, got {captured:?}"
    );
}

#[tokio::test]
async fn comment_list_normalizes_thread_comments() {
    let body = r#"{"comments":[
        {"comment":{"id":7,"post_id":1,"content":"A threaded comment","creator_id":2},"creator":{"id":2,"name":"alice"},"counts":{"score":4}},
        {"comment":{"id":8,"post_id":1,"content":"A reply","creator_id":2},"creator":{"id":2,"name":"alice"},"counts":{"score":-1}}
    ]}"#;
    let api = fixture_api_with_body(body);
    let comments = api.comments(&anonymous_context(), PostId(1)).await.unwrap();
    assert_eq!(comments.len(), 2);
    assert_eq!(comments[0].id, CommentId(7));
    assert_eq!(comments[0].content, "A threaded comment");
    assert_eq!(comments[0].creator_name, "alice");
    assert_eq!(comments[0].score, 4);
    assert_eq!(
        comments[1].score, -1,
        "negative comment scores must be preserved"
    );
}

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
    assert!(
        matches!(result, Err(AppError::Network(message)) if message.contains("empty response body"))
    );
}

#[tokio::test]
async fn missing_endpoint_reports_actionable_unsupported_error() {
    let api = fixture_api_with_status("/api/v3/post/list", 404);
    let result = api.feed(&anonymous_context(), FeedQuery::home()).await;
    assert!(
        matches!(result, Err(AppError::Authorization(message)) if message.contains("unsupported"))
    );
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
    let page = api
        .feed(&anonymous_context(), FeedQuery::home())
        .await
        .unwrap();
    assert_eq!(page.items[0].score, -2);
    assert_eq!(page.items[0].comments, -5);
}

#[tokio::test]
async fn mutation_comment_uses_response_post_id_and_negative_score() {
    let api = fixture_api_with_body(
        r#"{"comment_view":{"comment":{"id":2,"post_id":42,"content":"negative","creator_id":1},"counts":{"score":-3}}}"#,
    );
    let result = api
        .mutate(
            &authenticated_context(),
            Mutation::VoteComment {
                id: levim::CommentId(2),
                score: -1,
            },
        )
        .await
        .unwrap();
    let comment = result.comment.unwrap();
    assert_eq!(comment.post_id, PostId(42));
    assert_eq!(comment.score, -3);
}

#[tokio::test]
async fn login_preserves_instance_base_path() {
    let (api, instance_url) = login_fixture_api("/levim/");
    let session = api
        .login(LoginRequest {
            profile: levim::ProfileId::from("fixture"),
            instance_url,
            username: "fixture-user".into(),
            password: SecretString::from("fixture-password"),
        })
        .await
        .unwrap();
    assert_eq!(session.user_id, UserId(1));
}

#[tokio::test]
async fn site_reports_authenticated_user_id_from_my_user() {
    let api = fixture_api_with_body(
        r#"{"site_view":{"site":{"name":"Fixture","version":"0.19.5"},"local_site":{}},"my_user":{"local_user_view":{"local_user":{"id":1,"person_id":42}}}}"#,
    );
    let site = api.site(&anonymous_context()).await.unwrap();
    assert_eq!(site.name, "Fixture");
    assert_eq!(site.user_id, Some(UserId(42)));

    // Without a `my_user` block (anonymous request, or a server that omits
    // it) the user id is absent rather than fabricated.
    let anonymous = fixture_api_with_body(
        r#"{"site_view":{"site":{"name":"Fixture","version":"0.19.5"},"local_site":{}}}"#,
    );
    let site = anonymous.site(&anonymous_context()).await.unwrap();
    assert_eq!(site.user_id, None);
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
