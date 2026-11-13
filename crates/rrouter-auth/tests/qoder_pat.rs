//! PAT (`pt-…`) → job token (`jt-…`) exchange, against a mock and never a real host.
//!
//! Each test owns a [`MockServer`] (its origin is part of the credential-cache key), so
//! hits are counted per test and no cache entry survives between them.

use std::time::Duration;

use serde_json::json;
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use rrouter_auth::Credentials;

const EXCHANGE: &str = "/api/v1/jobToken/exchange";
const USERINFO: &str = "/api/v1/userinfo";

fn credentials(api_key: &str, user_id: Option<&str>) -> Credentials {
    let mut creds = Credentials {
        api_key: Some(api_key.to_string()),
        ..Default::default()
    };
    if let Some(id) = user_id {
        creds.extra.insert("user_id".into(), json!(id));
    }
    creds
}

async fn exchange(server: &MockServer, response: ResponseTemplate) {
    Mock::given(method("POST"))
        .and(path(EXCHANGE))
        .respond_with(response)
        .mount(server)
        .await;
}

async fn userinfo(server: &MockServer, response: ResponseTemplate) {
    Mock::given(method("GET"))
        .and(path(USERINFO))
        .respond_with(response)
        .mount(server)
        .await;
}

/// A successful exchange whose job token lives for `expires_in` milliseconds.
fn exchanged(token: &str, expires_in_ms: u64) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "token": token,
        "refresh_token": "rt-ignored",
        "expires_in": expires_in_ms,
    }))
}

fn user(id: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({ "id": id }))
}

async fn hits(server: &MockServer, target: &str) -> usize {
    server
        .received_requests()
        .await
        .expect("requests")
        .iter()
        .filter(|request| request.url.path() == target)
        .count()
}

#[tokio::test]
async fn one_exchange_serves_two_requests() {
    let server = MockServer::start().await;
    exchange(&server, exchanged("jt-one", 3_600_000)).await;
    userinfo(&server, user("user-9")).await;
    let http = reqwest::Client::new();
    let creds = credentials("pt-repeat", None);

    let first = rrouter_auth::qoder::pat::resolve_at(&http, &creds, &server.uri())
        .await
        .expect("first resolve");
    let second = rrouter_auth::qoder::pat::resolve_at(&http, &creds, &server.uri())
        .await
        .expect("second resolve");

    assert_eq!(first.token, "jt-one");
    assert_eq!(second.token, "jt-one");
    assert_eq!(first.user_id, "user-9");
    assert_eq!(second.user_id, "user-9");
    assert_eq!(hits(&server, EXCHANGE).await, 1, "one exchange");
    assert_eq!(hits(&server, USERINFO).await, 1, "one userinfo lookup");
}

#[tokio::test]
async fn a_token_inside_the_refresh_buffer_is_exchanged_again() {
    let server = MockServer::start().await;
    // 299s < the 5-minute refresh buffer, so the cached entry is never reused.
    exchange(&server, exchanged("jt-buffered", 299_000)).await;
    userinfo(&server, user("user-9")).await;
    let http = reqwest::Client::new();
    let creds = credentials("pt-buffer", None);

    for _ in 0..2 {
        let resolved = rrouter_auth::qoder::pat::resolve_at(&http, &creds, &server.uri())
            .await
            .expect("resolve");
        assert_eq!(resolved.token, "jt-buffered");
    }
    assert_eq!(hits(&server, EXCHANGE).await, 2);
}

#[tokio::test]
async fn an_expired_token_is_exchanged_again() {
    let server = MockServer::start().await;
    exchange(
        &server,
        ResponseTemplate::new(200).set_body_json(json!({
            "token": "jt-expired",
            "expires_at": "2000-01-01T00:00:00Z",
        })),
    )
    .await;
    userinfo(&server, user("user-9")).await;
    let http = reqwest::Client::new();
    let creds = credentials("pt-expired", None);

    for _ in 0..2 {
        rrouter_auth::qoder::pat::resolve_at(&http, &creds, &server.uri())
            .await
            .expect("resolve");
    }
    assert_eq!(hits(&server, EXCHANGE).await, 2, "past expiry is a miss");
}

#[tokio::test]
async fn concurrent_resolves_share_one_exchange() {
    let server = MockServer::start().await;
    exchange(
        &server,
        exchanged("jt-single", 3_600_000).set_delay(Duration::from_millis(150)),
    )
    .await;
    userinfo(&server, user("user-9")).await;
    let http = reqwest::Client::new();
    let creds = credentials("pt-single-flight", None);
    let origin = server.uri();

    let (first, second) = tokio::join!(
        rrouter_auth::qoder::pat::resolve_at(&http, &creds, &origin),
        rrouter_auth::qoder::pat::resolve_at(&http, &creds, &origin),
    );
    assert_eq!(first.expect("first").token, "jt-single");
    assert_eq!(second.expect("second").token, "jt-single");
    assert_eq!(hits(&server, EXCHANGE).await, 1, "single flight");
}

#[tokio::test]
async fn the_user_id_comes_from_userinfo_and_falls_back_to_the_connection() {
    let server = MockServer::start().await;
    exchange(&server, exchanged("jt-user", 3_600_000)).await;
    userinfo(&server, user("userinfo-id")).await;
    let http = reqwest::Client::new();
    let resolved = rrouter_auth::qoder::pat::resolve_at(
        &http,
        &credentials("pt-userinfo", Some("stored-id")),
        &server.uri(),
    )
    .await
    .expect("resolve");
    assert_eq!(resolved.user_id, "userinfo-id");
    assert_eq!(
        resolved.machine_id.len(),
        36,
        "derived from the resolved id"
    );

    let failing = MockServer::start().await;
    exchange(&failing, exchanged("jt-fallback", 3_600_000)).await;
    userinfo(&failing, ResponseTemplate::new(500)).await;
    let resolved = rrouter_auth::qoder::pat::resolve_at(
        &http,
        &credentials("pt-fallback", Some("stored-id")),
        &failing.uri(),
    )
    .await
    .expect("resolve with fallback");
    assert_eq!(resolved.user_id, "stored-id");
    assert_eq!(resolved.machine_id.len(), 36);
}

#[tokio::test]
async fn a_pat_with_no_resolvable_user_id_is_a_clear_error() {
    let server = MockServer::start().await;
    exchange(&server, exchanged("jt-nobody", 3_600_000)).await;
    userinfo(&server, ResponseTemplate::new(503)).await;
    let http = reqwest::Client::new();

    let error = rrouter_auth::qoder::pat::resolve_at(
        &http,
        &credentials("pt-no-user", None),
        &server.uri(),
    )
    .await
    .expect_err("no user id");
    let text = error.to_string();
    assert!(text.contains("user id"), "{text}");
    assert!(!text.contains("pt-no-user"), "the PAT is never echoed");
}

#[tokio::test]
async fn a_refused_exchange_surfaces_a_provider_error_without_the_token() {
    let refused = MockServer::start().await;
    exchange(&refused, ResponseTemplate::new(401)).await;
    let http = reqwest::Client::new();
    let error = rrouter_auth::qoder::pat::resolve_at(
        &http,
        &credentials("pt-401-secret", None),
        &refused.uri(),
    )
    .await
    .expect_err("401");
    assert_eq!(error.status(), 401, "the upstream status is preserved");
    let text = error.to_string();
    assert!(!text.contains("pt-401-secret"), "{text}");
    assert!(!text.contains("jt-"), "{text}");

    let down = MockServer::start().await;
    exchange(&down, ResponseTemplate::new(503)).await;
    let error = rrouter_auth::qoder::pat::resolve_at(
        &http,
        &credentials("pt-503-secret", None),
        &down.uri(),
    )
    .await
    .expect_err("503");
    assert_eq!(error.status(), 503);
    assert!(!error.to_string().contains("pt-503-secret"));
}

#[tokio::test]
async fn an_exchange_without_a_job_token_is_an_error() {
    let server = MockServer::start().await;
    exchange(&server, ResponseTemplate::new(200).set_body_json(json!({}))).await;
    let http = reqwest::Client::new();
    let error =
        rrouter_auth::qoder::pat::resolve_at(&http, &credentials("pt-empty", None), &server.uri())
            .await
            .expect_err("no token");
    assert!(error.to_string().contains("job token"), "{error}");
}

#[tokio::test]
async fn a_device_token_is_unchanged_and_never_exchanged() {
    let server = MockServer::start().await;
    exchange(&server, ResponseTemplate::new(200)).await;
    userinfo(&server, user("never")).await;
    let http = reqwest::Client::new();
    let creds = credentials("dt-device", Some("dt-user"));

    let resolved = rrouter_auth::qoder::pat::resolve_at(&http, &creds, &server.uri())
        .await
        .expect("dt resolve");
    assert_eq!(resolved.token, "dt-device");
    assert_eq!(resolved.user_id, "dt-user");
    assert_eq!(
        resolved.machine_id,
        rrouter_auth::qoder::cosy::derive_machine_id("dt-user")
    );
    assert_eq!(
        hits(&server, EXCHANGE).await + hits(&server, USERINFO).await,
        0,
        "a device token costs no network call"
    );

    let error =
        rrouter_auth::qoder::pat::resolve_at(&http, &credentials("dt-device", None), &server.uri())
            .await
            .expect_err("dt still needs a user id");
    assert!(error.to_string().contains("user_id"), "{error}");
}

#[tokio::test]
async fn the_exchange_request_matches_the_reference_contract() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(EXCHANGE))
        .and(header("content-type", "application/json"))
        .and(header("accept", "application/json"))
        .and(header("user-agent", "qodercli/1.0.0"))
        .and(header("cosy-version", "1.0.0"))
        .and(header("cosy-clienttype", "5"))
        .and(body_json(json!({ "personal_token": "pt-contract" })))
        .respond_with(exchanged("jt-contract", 3_600_000))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(USERINFO))
        .and(header("authorization", "Bearer jt-contract"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "userId": "camel-id" })))
        .mount(&server)
        .await;

    let http = reqwest::Client::new();
    let resolved = rrouter_auth::qoder::pat::resolve_at(
        &http,
        &credentials("pt-contract", None),
        &server.uri(),
    )
    .await
    .expect("contract resolve");
    assert_eq!(resolved.token, "jt-contract");
    assert_eq!(resolved.user_id, "camel-id");

    let requests = server.received_requests().await.expect("requests");
    let exchange_request = requests
        .iter()
        .find(|request| request.url.path() == EXCHANGE)
        .expect("the exchange was called");
    assert!(
        exchange_request.headers.get("authorization").is_none(),
        "the exchange is not COSY-signed, so it carries no authorization header"
    );
}
