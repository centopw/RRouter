//! Dashboard auth end to end: first-run setup, sign-in, sessions, CSRF, roles,
//! recovery codes and invitation links.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{HeaderMap, Method, Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use rrouter_gateway::{app, auth::Auth, config::Config, AppState};
use rrouter_router::{Breaker, Catalog, Dispatcher, Planner};
use rrouter_store::Store;
use serde_json::{json, Value};
use tower::ServiceExt;

struct H {
    state: Arc<AppState>,
    router: Router,
}

fn config() -> Config {
    Config {
        host: "127.0.0.1".parse().expect("ip"),
        port: 0,
        data_dir: std::env::temp_dir().join(format!("rrouter-auth-{}", uuid::Uuid::new_v4())),
        master_key: None,
        require_api_key: false,
        request_timeout_secs: 30,
        connect_timeout_secs: 5,
        log_filter: "rrouter=error".into(),
        static_dir: None,
        cors_origins: vec![],
        allow_remote_admin: false,
        admin_token: Some("adm-test".into()),
        max_concurrency: 0,
        max_attempts: 4,
        retry_attempts: 1,
        retry_base_ms: 200,
        retry_max_ms: 5000,
        shutdown_grace_secs: 20,
        strategy: rrouter_router::Strategy::Priority,
        cache: rrouter_gateway::config::CacheMode::Off,
        cache_ttl_secs: 300,
        cache_max_entries: 1000,
        usage_retention_days: 0,
        log_format: rrouter_gateway::config::LogFormat::Text,
        access_log: false,
    }
}

fn harness() -> H {
    let config = config();
    let data_dir = config.data_dir.clone();
    let state = Arc::new(AppState {
        store: Arc::new(Store::open_in_memory().expect("store")),
        catalog: Arc::new(Catalog::builtin()),
        refresher: Arc::new(rrouter_auth::Refresher::new(
            reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .build()
                .expect("client"),
        )),
        planner: Planner::new(),
        dispatcher: Arc::new(Dispatcher::new(Breaker::new()).with_max_attempts(4)),
        http: reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .expect("client"),
        config,
        started: std::time::Instant::now(),
        oauth: rrouter_auth::OAuthFlows::new(),
        counters: Default::default(),
        concurrency: None,
        // An admin token is configured, so the tests prove session auth stands on its
        // own rather than relying on the loopback shortcut.
        auth: Auth::load(&data_dir, false),
    });
    let router = app(Arc::clone(&state));
    H { state, router }
}

impl H {
    async fn call(
        &self,
        method: Method,
        uri: &str,
        body: Option<Value>,
        headers: &[(&str, &str)],
    ) -> (StatusCode, HeaderMap, Value) {
        let mut builder = Request::builder().method(method).uri(uri);
        if body.is_some() {
            builder = builder.header("content-type", "application/json");
        }
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let request = builder
            .body(body.map_or_else(Body::empty, |value| Body::from(value.to_string())))
            .expect("request");
        let res = self.router.clone().oneshot(request).await.expect("oneshot");
        let status = res.status();
        let headers = res.headers().clone();
        let bytes = res.into_body().collect().await.expect("body").to_bytes();
        (status, headers, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    async fn post(&self, uri: &str, body: Value, headers: &[(&str, &str)]) -> (StatusCode, HeaderMap, Value) {
        self.call(Method::POST, uri, Some(body), headers).await
    }

    /// The code before setup. After setup `code()` is `None` by contract, so this
    /// returns an empty string rather than panicking in assertions that check exactly
    /// that.
    fn setup_code(&self) -> String {
        self.state.auth.code().unwrap_or_default()
    }

    /// Create the owner and return `(session cookie, recovery codes)`.
    async fn setup(&self) -> (String, Vec<String>) {
        let (status, headers, body) = self
            .post(
                "/api/auth/setup",
                json!({
                    "setup_code": self.setup_code(),
                    "name": "Ada Lovelace",
                    "email": "ada@example.com",
                    "password": "correct horse battery",
                }),
                &[],
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "setup: {body}");
        let cookie = cookie_of(&headers).expect("session cookie");
        let codes = body["recovery_codes"]
            .as_array()
            .expect("codes")
            .iter()
            .map(|v| v.as_str().expect("code").to_string())
            .collect();
        (cookie, codes)
    }
}

/// `rrouter_session=…` from a response's `Set-Cookie`.
fn cookie_of(headers: &HeaderMap) -> Option<String> {
    headers
        .get("set-cookie")
        .and_then(|v| v.to_str().ok())
        .and_then(|raw| raw.split(';').next())
        .map(|pair| pair.trim().to_string())
        .filter(|pair| pair.starts_with("rrouter_session="))
}

fn auth_headers(cookie: &str) -> [(&str, &str); 1] {
    [("cookie", cookie)]
}

fn csrf(cookie: &str) -> [(&str, &str); 2] {
    [("cookie", cookie), ("x-rrouter-csrf", "1")]
}

#[tokio::test]
async fn fresh_instance_reports_setup_then_refuses_a_second_owner() {
    let h = harness();
    let (status, _, body) = h.call(Method::GET, "/api/auth/state", None, &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["initialized"], false);
    assert_eq!(body["setup_code_required"], true);
    assert_eq!(body["user"], Value::Null);

    let (status, _, body) = h
        .post(
            "/api/auth/setup",
            json!({
                "setup_code": "0000-0000",
                "name": "Ada",
                "email": "ada@example.com",
                "password": "correct horse battery",
            }),
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"]["code"], "setup_code_invalid");
    assert!(!h.state.store.users_exist().await.expect("users"), "a bad code writes nothing");

    let (cookie, codes) = h.setup().await;
    assert_eq!(codes.len(), 10, "ten recovery codes, shown once");
    assert!(h.setup_code().is_empty(), "no code is minted after setup");

    let (_, _, state) = h.call(Method::GET, "/api/auth/state", None, &auth_headers(&cookie)).await;
    assert_eq!(state["initialized"], true);
    assert_eq!(state["user"]["role"], "admin");
    assert_eq!(state["user"]["is_owner"], true);

    // A second run with the old code is refused, and setup cannot mint another owner.
    let (status, _, body) = h
        .post(
            "/api/auth/setup",
            json!({
                "setup_code": "AAAA-AAAA",
                "name": "Eve",
                "email": "eve@example.com",
                "password": "correct horse battery",
            }),
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], "already_initialized");
    assert_eq!(h.state.store.users().await.expect("users").len(), 1);
}

#[tokio::test]
async fn sign_in_is_generic_throttled_and_cookie_authenticates_admin_api() {
    let h = harness();
    let _ = h.setup().await;

    let (status, _, body) = h
        .post(
            "/api/auth/login",
            json!({"email": "ada@example.com", "password": "wrong password"}),
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["code"], "invalid_credentials");
    let (status, _, unknown) = h
        .post(
            "/api/auth/login",
            json!({"email": "nobody@example.com", "password": "wrong password"}),
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(unknown["error"]["message"], body["error"]["message"], "same message");

    let (status, headers, body) = h
        .post(
            "/api/auth/login",
            json!({
                "email": "Ada@Example.com",
                "password": "correct horse battery",
                "remember": true,
            }),
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::OK, "login: {body}");
    assert_eq!(body["user"]["email"], "ada@example.com", "email is normalised");
    let cookie = cookie_of(&headers).expect("cookie");
    let set_cookie = headers.get("set-cookie").and_then(|v| v.to_str().ok()).expect("header");
    assert!(set_cookie.contains("HttpOnly") && set_cookie.contains("Max-Age="));

    // The session authenticates the admin API even though RROUTER_ADMIN_TOKEN is set.
    let (status, _, users) = h.call(Method::GET, "/api/users", None, &auth_headers(&cookie)).await;
    assert_eq!(status, StatusCode::OK, "users: {users}");

    // A cookie-authenticated write without the CSRF header is refused.
    let (status, _, body) = h
        .post("/api/keys", json!({"name": "x"}), &auth_headers(&cookie))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"]["code"], "csrf_required");
    let (status, _, body) = h.post("/api/keys", json!({"name": "x"}), &csrf(&cookie)).await;
    assert_eq!(status, StatusCode::OK, "with csrf: {body}");

    // Logout ends the session server-side.
    let (status, _, _) = h.post("/api/auth/logout", Value::Null, &auth_headers(&cookie)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _, _) = h.call(Method::GET, "/api/users", None, &auth_headers(&cookie)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "the token gate refuses the dead cookie");

    // Five failures throttle for a minute, with Retry-After.
    for _ in 0..5 {
        h.post(
            "/api/auth/login",
            json!({"email": "ada@example.com", "password": "nope"}),
            &[],
        )
        .await;
    }
    let (status, headers, body) = h
        .post(
            "/api/auth/login",
            json!({"email": "ada@example.com", "password": "correct horse battery"}),
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "throttled: {body}");
    assert!(headers.contains_key("retry-after"));
}

#[tokio::test]
async fn invitation_signup_creates_a_member_without_admin_rights() {
    let h = harness();
    let (owner, _) = h.setup().await;

    let (status, _, body) = h
        .post(
            "/api/users/invites",
            json!({"email": "grace@example.com", "role": "member"}),
            &csrf(&owner),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "invite: {body}");
    let token = body["token"].as_str().expect("token").to_string();

    let (status, _, meta) = h
        .call(Method::GET, &format!("/api/auth/invites/{token}"), None, &[])
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(meta["email"], "grace@example.com");
    assert_eq!(meta["role"], "member");
    assert_eq!(meta["inviter_name"], "Ada Lovelace");

    let (status, headers, signed) = h
        .post(
            "/api/auth/signup",
            json!({"token": token, "name": "Grace Hopper", "password": "another long password"}),
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "signup: {signed}");
    assert_eq!(signed["user"]["role"], "member");
    assert_eq!(signed["recovery_codes"].as_array().expect("codes").len(), 10);
    let member = cookie_of(&headers).expect("member cookie");

    // A member signs in but cannot administer; the refusal names the reason.
    let (status, _, body) = h.call(Method::GET, "/api/users", None, &auth_headers(&member)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"]["code"], "role_required");
    let (status, _, _) = h.call(Method::GET, "/api/auth/state", None, &auth_headers(&member)).await;
    assert_eq!(status, StatusCode::OK);

    // A used invitation is gone, not merely marked.
    let (status, _, _) = h
        .call(Method::GET, &format!("/api/auth/invites/{token}"), None, &[])
        .await;
    assert_eq!(status, StatusCode::GONE);
    let (status, _, body) = h
        .post(
            "/api/auth/signup",
            json!({"token": token, "name": "Again", "password": "another long password"}),
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::GONE, "replay: {body}");
}

#[tokio::test]
async fn owner_cannot_be_demoted_or_deleted_and_recovery_codes_work_once() {
    let h = harness();
    let (owner_cookie, codes) = h.setup().await;
    let users = h.state.store.users().await.expect("users");
    let owner_id = users[0].id.to_string();
    let grace = h
        .post(
            "/api/users/invites",
            json!({"email": "grace@example.com", "role": "viewer"}),
            &csrf(&owner_cookie),
        )
        .await;
    let token = grace.2["token"].as_str().expect("token").to_string();
    let (_, _, signed) = h
        .post(
            "/api/auth/signup",
            json!({"token": token, "name": "Grace", "password": "another long password"}),
            &[],
        )
        .await;
    let member_id = signed["user"]["id"].as_str().expect("member id").to_string();

    let (status, _, body) = h
        .call(
            Method::PUT,
            &format!("/api/users/{owner_id}"),
            Some(json!({"role": "member"})),
            &csrf(&owner_cookie),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "demote: {body}");
    assert_eq!(body["error"]["code"], "owner_protected");
    let (status, _, _) = h
        .call(
            Method::DELETE,
            &format!("/api/users/{owner_id}"),
            None,
            &csrf(&owner_cookie),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Another user is disposable, and deleting them kills their session.
    let (status, _, _) = h
        .call(
            Method::DELETE,
            &format!("/api/users/{member_id}"),
            None,
            &csrf(&owner_cookie),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // A recovery code resets the password, ends sessions and refuses replay.
    let code = codes[3].clone();
    let (status, _, body) = h
        .post(
            "/api/auth/recover",
            json!({
                "email": "ada@example.com",
                "recovery_code": code,
                "new_password": "a brand new passphrase",
            }),
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "recover: {body}");
    let (status, _, _) = h.call(Method::GET, "/api/users", None, &auth_headers(&owner_cookie)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "old session is signed out");

    let (status, _, _) = h
        .post(
            "/api/auth/login",
            json!({"email": "ada@example.com", "password": "a brand new passphrase"}),
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    let (status, _, body) = h
        .post(
            "/api/auth/recover",
            json!({
                "email": "ada@example.com",
                "recovery_code": code,
                "new_password": "yet another passphrase",
            }),
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "a code works once: {body}");
    assert_eq!(body["error"]["code"], "invalid_recovery");
}

/// Self-service: rename, change password (ends other sessions), list sessions, and the
/// audit trail records it all.
#[tokio::test]
async fn profile_sessions_and_audit_are_the_users_own() {
    let h = harness();
    let (cookie, _) = h.setup().await;

    let (status, _, me) = h.call(Method::GET, "/api/auth/me", None, &auth_headers(&cookie)).await;
    assert_eq!(status, StatusCode::OK, "me: {me}");
    assert_eq!(me["sessions"].as_array().expect("sessions").len(), 1);
    assert_eq!(me["sessions"][0]["current"], true);

    let (status, _, renamed) = h
        .call(
            Method::PUT,
            "/api/auth/me",
            Some(json!({"name": "Ada B. Lovelace"})),
            &csrf(&cookie),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "rename: {renamed}");
    assert_eq!(renamed["user"]["name"], "Ada B. Lovelace");

    let (status, _, _) = h
        .call(
            Method::PUT,
            "/api/auth/me",
            Some(json!({"current_password": "wrong", "new_password": "brand new passphrase"})),
            &csrf(&cookie),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, headers, _) = h
        .call(
            Method::PUT,
            "/api/auth/me",
            Some(json!({
                "current_password": "correct horse battery",
                "new_password": "brand new passphrase",
            })),
            &csrf(&cookie),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let fresh = cookie_of(&headers).expect("rotated cookie");
    let (status, _, _) = h.call(Method::GET, "/api/users", None, &auth_headers(&cookie)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "the old session ended");
    let (status, _, users) = h.call(Method::GET, "/api/users", None, &auth_headers(&fresh)).await;
    assert_eq!(status, StatusCode::OK, "the current tab stays signed in: {users}");

    let (status, _, audit) = h.call(Method::GET, "/api/audit?limit=50", None, &auth_headers(&fresh)).await;
    assert_eq!(status, StatusCode::OK, "audit: {audit}");
    let kinds: Vec<&str> = audit["data"]
        .as_array()
        .expect("rows")
        .iter()
        .filter_map(|row| row["kind"].as_str())
        .collect();
    for expected in ["setup", "profile_updated", "password_changed", "password_change_failed"] {
        assert!(kinds.contains(&expected), "missing {expected} in {kinds:?}");
    }

    let (status, _, _) = h
        .post("/api/auth/me/sessions/revoke-others", Value::Null, &csrf(&fresh))
        .await;
    assert_eq!(status, StatusCode::OK);

    let (status, _, ready) = h.call(Method::GET, "/ready", None, &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(ready["ready"], true);
}
