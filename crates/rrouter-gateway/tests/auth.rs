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
        hedge_ms: 0,
        canary: None,
        subscription_reserve_percent: 90,
        semantic_cache: rrouter_gateway::config::CacheMode::Off,
        semantic_cache_threshold: 0.92,
        semantic_embedding_model: None,
        semantic_cache_ttl_secs: 3600,
        semantic_cache_max_entries: 1000,
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
        config: rrouter_gateway::config::ConfigSnapshot::from(config),
        started: std::time::Instant::now(),
        oauth: rrouter_auth::OAuthFlows::new(),
        counters: Default::default(),
        envelope_drops: Default::default(),
        concurrency: std::sync::RwLock::new(None),
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
        (
            status,
            headers,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    async fn post(
        &self,
        uri: &str,
        body: Value,
        headers: &[(&str, &str)],
    ) -> (StatusCode, HeaderMap, Value) {
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
    assert!(
        !h.state.store.users_exist().await.expect("users"),
        "a bad code writes nothing"
    );

    let (cookie, codes) = h.setup().await;
    assert_eq!(codes.len(), 10, "ten recovery codes, shown once");
    assert!(h.setup_code().is_empty(), "no code is minted after setup");

    let (_, _, state) = h
        .call(Method::GET, "/api/auth/state", None, &auth_headers(&cookie))
        .await;
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
    assert_eq!(
        unknown["error"]["message"], body["error"]["message"],
        "same message"
    );

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
    assert_eq!(
        body["user"]["email"], "ada@example.com",
        "email is normalised"
    );
    let cookie = cookie_of(&headers).expect("cookie");
    let set_cookie = headers
        .get("set-cookie")
        .and_then(|v| v.to_str().ok())
        .expect("header");
    assert!(set_cookie.contains("HttpOnly") && set_cookie.contains("Max-Age="));

    // The session authenticates the admin API even though RROUTER_ADMIN_TOKEN is set.
    let (status, _, users) = h
        .call(Method::GET, "/api/users", None, &auth_headers(&cookie))
        .await;
    assert_eq!(status, StatusCode::OK, "users: {users}");

    // A cookie-authenticated write without the CSRF header is refused.
    let (status, _, body) = h
        .post("/api/keys", json!({"name": "x"}), &auth_headers(&cookie))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"]["code"], "csrf_required");
    let (status, _, body) = h
        .post("/api/keys", json!({"name": "x"}), &csrf(&cookie))
        .await;
    assert_eq!(status, StatusCode::OK, "with csrf: {body}");

    // Logout ends the session server-side.
    let (status, _, _) = h
        .post("/api/auth/logout", Value::Null, &auth_headers(&cookie))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _, _) = h
        .call(Method::GET, "/api/users", None, &auth_headers(&cookie))
        .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "the token gate refuses the dead cookie"
    );

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
        .call(
            Method::GET,
            &format!("/api/auth/invites/{token}"),
            None,
            &[],
        )
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
    assert_eq!(
        signed["recovery_codes"].as_array().expect("codes").len(),
        10
    );
    let member = cookie_of(&headers).expect("member cookie");

    // A member signs in but cannot administer; the refusal names the reason.
    let (status, _, body) = h
        .call(Method::GET, "/api/users", None, &auth_headers(&member))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"]["code"], "role_required");
    let (status, _, _) = h
        .call(Method::GET, "/api/auth/state", None, &auth_headers(&member))
        .await;
    assert_eq!(status, StatusCode::OK);

    // A used invitation is gone, not merely marked.
    let (status, _, _) = h
        .call(
            Method::GET,
            &format!("/api/auth/invites/{token}"),
            None,
            &[],
        )
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
    let member_id = signed["user"]["id"]
        .as_str()
        .expect("member id")
        .to_string();

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
    let (status, _, _) = h
        .call(
            Method::GET,
            "/api/users",
            None,
            &auth_headers(&owner_cookie),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "old session is signed out"
    );

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

    let (status, _, me) = h
        .call(Method::GET, "/api/auth/me", None, &auth_headers(&cookie))
        .await;
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
    let (status, _, _) = h
        .call(Method::GET, "/api/users", None, &auth_headers(&cookie))
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "the old session ended");
    let (status, _, users) = h
        .call(Method::GET, "/api/users", None, &auth_headers(&fresh))
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the current tab stays signed in: {users}"
    );

    let (status, _, audit) = h
        .call(
            Method::GET,
            "/api/audit?limit=50",
            None,
            &auth_headers(&fresh),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "audit: {audit}");
    let kinds: Vec<&str> = audit["data"]
        .as_array()
        .expect("rows")
        .iter()
        .filter_map(|row| row["kind"].as_str())
        .collect();
    for expected in [
        "setup",
        "profile_updated",
        "password_changed",
        "password_change_failed",
    ] {
        assert!(kinds.contains(&expected), "missing {expected} in {kinds:?}");
    }

    let (status, _, _) = h
        .post(
            "/api/auth/me/sessions/revoke-others",
            Value::Null,
            &csrf(&fresh),
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    let (status, _, ready) = h.call(Method::GET, "/ready", None, &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(ready["ready"], true);
}

/// Invite `email` with `role`, accept the invitation, and return the new session cookie.
/// The owner creates the invitation, so the same path the dashboard uses is what the
/// reader tests exercise.
async fn invite_and_join(h: &H, owner: &str, email: &str, role: &str) -> String {
    let (status, _, body) = h
        .post(
            "/api/users/invites",
            json!({"email": email, "role": role}),
            &csrf(owner),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "invite: {body}");
    let token = body["token"].as_str().expect("token").to_string();
    let (status, headers, signed) = h
        .post(
            "/api/auth/signup",
            json!({"token": token, "name": "Reader", "password": "another long password"}),
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "signup: {signed}");
    cookie_of(&headers).expect("session cookie")
}

/// A viewer reads the read-only surface and is refused everything else. The harness has
/// an admin token configured, so loopback proves nothing: these answers come from the
/// role alone.
#[tokio::test]
async fn viewer_reads_usage_but_not_admin_surface() {
    let h = harness();
    let (owner, _) = h.setup().await;
    let viewer = invite_and_join(&h, &owner, "viewer@example.com", "viewer").await;
    let headers = auth_headers(&viewer);

    for uri in [
        "/api/usage",
        "/api/usage/summary",
        "/api/usage/series?hours=24&bucket=hour",
        "/api/models",
        "/api/models/providers",
        "/api/templates",
        "/api/usage/export.csv",
    ] {
        let (status, _, body) = h.call(Method::GET, uri, None, &headers).await;
        assert_eq!(status, StatusCode::OK, "viewer GET {uri}: {body}");
    }

    // Provider accounts are a scoped read: a viewer may ask, and sees only the rows owned
    // by a team they belong to — none here, so the list is empty and still carries no
    // credential material. Mutations stay admin-only (`tests/connection_scope.rs`).
    let (status, _, body) = h
        .call(Method::GET, "/api/connections", None, &headers)
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "viewer GET /api/connections: {body}"
    );
    assert_eq!(body["data"], json!([]), "no team, no visible account");

    // The same viewer still cannot see keys, users, budgets or the trail.
    for uri in [
        "/api/keys",
        "/api/accounts/quotas",
        "/api/settings",
        "/api/system",
        "/api/users",
        "/api/audit",
    ] {
        let (status, _, body) = h.call(Method::GET, uri, None, &headers).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "viewer GET {uri}: {body}");
        assert_eq!(
            body["error"]["code"], "role_required",
            "viewer GET {uri} names the role"
        );
    }

    // Signed out, the read-only surface is still gated.
    let (status, _, _) = h.call(Method::GET, "/api/usage", None, &[]).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

/// A reader credential never authorises a mutation, with or without the CSRF header.
#[tokio::test]
async fn member_and_viewer_cannot_write() {
    let h = harness();
    let (owner, _) = h.setup().await;
    let (_, _, created) = h
        .post("/api/keys", json!({"name": "target"}), &csrf(&owner))
        .await;
    let key_id = created["id"].as_str().expect("key id").to_string();

    for role in ["member", "viewer"] {
        let email = format!("{role}@example.com");
        let cookie = invite_and_join(&h, &owner, &email, role).await;
        let with_csrf = csrf(&cookie);
        let cases = [
            (
                Method::POST,
                "/api/templates".to_string(),
                Some(json!({"name": "x", "body": "y"})),
            ),
            (
                Method::POST,
                "/api/models/sync".to_string(),
                Some(Value::Null),
            ),
            (
                Method::PATCH,
                format!("/api/keys/{key_id}"),
                Some(json!({"rpm": 1})),
            ),
            (Method::DELETE, format!("/api/keys/{key_id}"), None),
        ];
        for (method, uri, body) in cases {
            let (status, _, response) =
                h.call(method.clone(), &uri, body.clone(), &with_csrf).await;
            assert_eq!(
                status,
                StatusCode::FORBIDDEN,
                "{role} {method} {uri}: {response}"
            );
            assert_eq!(
                response["error"]["code"], "role_required",
                "{role} {method} {uri}"
            );
            // The role refusal does not depend on the CSRF header being absent.
            let (status, _, _) = h
                .call(method.clone(), &uri, body, &auth_headers(&cookie))
                .await;
            assert_eq!(
                status,
                StatusCode::FORBIDDEN,
                "{role} {method} {uri} without csrf"
            );
        }
    }

    // The target key survived every attempt.
    let (status, _, keys) = h
        .call(Method::GET, "/api/keys", None, &auth_headers(&owner))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(keys["data"][0]["rpm"], 0, "no reader write landed: {keys}");
}

/// Minting, repointing and deleting a key each leave one audit row, with the actor and
/// the policy fields — and never the plaintext secret.
#[tokio::test]
async fn key_create_update_and_delete_are_audited() {
    let h = harness();
    let (owner, _) = h.setup().await;
    let headers = auth_headers(&owner);

    let (status, _, created) = h
        .post("/api/keys", json!({"name": "laptop"}), &csrf(&owner))
        .await;
    assert_eq!(status, StatusCode::OK, "create: {created}");
    let id = created["id"].as_str().expect("id").to_string();
    let plaintext = created["key"].as_str().expect("plaintext").to_string();

    let (status, _, updated) = h
        .call(
            Method::PATCH,
            &format!("/api/keys/{id}"),
            Some(json!({
                "scope": "admin",
                "budget_micros": 5_000_000,
                "guardrails": {"blocked": ["ssn"], "scan_output": true},
            })),
            &csrf(&owner),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "update: {updated}");

    let (status, _, _) = h
        .call(
            Method::DELETE,
            &format!("/api/keys/{id}"),
            None,
            &csrf(&owner),
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    let (status, _, audit) = h
        .call(Method::GET, "/api/audit?limit=50", None, &headers)
        .await;
    assert_eq!(status, StatusCode::OK, "audit: {audit}");
    let rows = audit["data"].as_array().expect("rows");
    let kinds: Vec<&str> = rows.iter().filter_map(|row| row["kind"].as_str()).collect();
    for expected in ["key_created", "key_updated", "key_deleted"] {
        assert!(kinds.contains(&expected), "missing {expected} in {kinds:?}");
    }

    let row = |kind: &str| {
        rows.iter()
            .find(|row| row["kind"] == kind)
            .unwrap_or_else(|| panic!("no {kind} row"))
            .clone()
    };
    let created_row = row("key_created");
    assert_eq!(
        created_row["actor"], "ada@example.com",
        "the session is the actor"
    );
    let detail = created_row["detail"].as_str().expect("detail");
    assert!(detail.contains(&id), "names the key: {detail}");
    assert!(
        detail.contains("scope inference"),
        "names the scope: {detail}"
    );
    assert!(
        !detail.contains(&plaintext),
        "the plaintext key must never be audited"
    );
    assert!(
        !audit.to_string().contains(&plaintext),
        "nor anywhere in the trail"
    );

    let updated_row = row("key_updated");
    let detail = updated_row["detail"].as_str().expect("detail");
    for field in ["scope", "budget_micros", "guardrails"] {
        assert!(
            detail.contains(field),
            "update detail names {field}: {detail}"
        );
    }
    assert!(
        detail.contains("now scope admin"),
        "escalation is explicit: {detail}"
    );

    let deleted_row = row("key_deleted");
    assert!(deleted_row["detail"]
        .as_str()
        .expect("detail")
        .contains("laptop"));
    assert!(
        deleted_row["ip"].as_str().is_some(),
        "the source is recorded"
    );
}

/// The parts of a rename the store's own test and `tests/proxy.rs` do not cover: a blank
/// name clears, an unrelated patch leaves the name alone, the cap counts characters, and
/// the trail records the field without ever holding the secret.
#[tokio::test]
async fn key_name_patch_clears_blanks_and_keeps_the_trail_clean() {
    let h = harness();
    let (owner, _) = h.setup().await;
    let headers = auth_headers(&owner);

    let (status, _, created) = h
        .post("/api/keys", json!({"name": "laptop"}), &csrf(&owner))
        .await;
    assert_eq!(status, StatusCode::OK, "create: {created}");
    let id = created["id"].as_str().expect("id").to_string();
    let plaintext = created["key"].as_str().expect("plaintext").to_string();
    let uri = format!("/api/keys/{id}");

    // Whitespace alone is a clear, like `null`.
    let (status, _, blanked) = h
        .call(
            Method::PATCH,
            &uri,
            Some(json!({"name": "   "})),
            &csrf(&owner),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "blank: {blanked}");
    assert_eq!(blanked["name"], Value::Null);

    // A patch that does not mention the name leaves it alone.
    let (_, _, named) = h
        .call(
            Method::PATCH,
            &uri,
            Some(json!({"name": "edge gateway"})),
            &csrf(&owner),
        )
        .await;
    assert_eq!(named["name"], "edge gateway");
    let (status, _, patched) = h
        .call(Method::PATCH, &uri, Some(json!({"rpm": 45})), &csrf(&owner))
        .await;
    assert_eq!(status, StatusCode::OK, "{patched}");
    assert_eq!(
        patched["name"], "edge gateway",
        "an unrelated patch kept it"
    );
    assert_eq!(patched["rpm"], 45);

    // The cap counts characters: 100 multibyte characters fit, 200 bytes notwithstanding.
    let fits = "é".repeat(rrouter_store::MAX_API_KEY_NAME);
    let (status, _, stored) = h
        .call(
            Method::PATCH,
            &uri,
            Some(json!({ "name": fits })),
            &csrf(&owner),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "100 characters: {stored}");
    assert_eq!(stored["name"], fits);

    // Every rename is audited by field name, never by secret.
    let (status, _, audit) = h
        .call(Method::GET, "/api/audit?limit=50", None, &headers)
        .await;
    assert_eq!(status, StatusCode::OK, "audit: {audit}");
    let rows = audit["data"].as_array().expect("rows");
    assert!(
        rows.iter().any(|row| {
            row["kind"] == "key_updated"
                && row["detail"]
                    .as_str()
                    .unwrap_or_default()
                    .split('·')
                    .nth(1)
                    .is_some_and(|fields| fields.split(',').any(|field| field.trim() == "name"))
        }),
        "a rename names the field: {audit}"
    );
    assert!(
        !audit.to_string().contains(&plaintext),
        "the plaintext key must never be audited"
    );
}

/// A successful and a failed password sign-in both land in the trail, under the email
/// that was attempted, with no credential material.
#[tokio::test]
async fn sign_in_success_and_failure_are_audited() {
    let h = harness();
    let (owner, _) = h.setup().await;

    let (status, _, _) = h
        .post(
            "/api/auth/login",
            json!({"email": "ada@example.com", "password": "wrong password"}),
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, _, _) = h
        .post(
            "/api/auth/login",
            json!({"email": "ada@example.com", "password": "correct horse battery"}),
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    let (status, _, audit) = h
        .call(
            Method::GET,
            "/api/audit?limit=50",
            None,
            &auth_headers(&owner),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "audit: {audit}");
    let rows = audit["data"].as_array().expect("rows");
    let find = |kind: &str| {
        rows.iter()
            .find(|row| row["kind"] == kind)
            .unwrap_or_else(|| panic!("no {kind} row in {rows:?}"))
            .clone()
    };
    let failed = find("sign_in_failed");
    assert_eq!(failed["actor"], "ada@example.com");
    assert_eq!(failed["detail"], "invalid credentials");
    let ok = find("sign_in");
    assert_eq!(ok["actor"], "ada@example.com");
    assert!(ok["detail"]
        .as_str()
        .expect("detail")
        .starts_with("password sign-in"));

    let trail = audit.to_string();
    for secret in ["wrong password", "correct horse battery"] {
        assert!(!trail.contains(secret), "passwords must never be audited");
    }
}

/// The session the caller is signed in under, from a `/api/auth/me` body.
fn current_session(me: &Value) -> &Value {
    me["sessions"]
        .as_array()
        .expect("sessions")
        .iter()
        .find(|session| session["current"] == true)
        .unwrap_or_else(|| panic!("no current session in {me}"))
}

#[tokio::test]
async fn sign_in_captures_ip_and_user_agent_and_never_leaks_the_token() {
    let h = harness();
    let (_setup_cookie, _) = h.setup().await;
    let ua = "Mozilla/5.0 (Macintosh; Intel Mac OS X 14_0) AppleWebKit/537.36 \
              (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";
    let (status, headers, body) = h
        .post(
            "/api/auth/login",
            json!({
                "email": "ada@example.com",
                "password": "correct horse battery",
            }),
            &[
                ("x-forwarded-for", "203.0.113.9, 10.0.0.1"),
                ("user-agent", ua),
            ],
        )
        .await;
    assert_eq!(status, StatusCode::OK, "login: {body}");
    let cookie = cookie_of(&headers).expect("cookie");
    let token = cookie.trim_start_matches("rrouter_session=");

    let (status, _, me) = h
        .call(Method::GET, "/api/auth/me", None, &auth_headers(&cookie))
        .await;
    assert_eq!(status, StatusCode::OK, "me: {me}");
    let sessions = me["sessions"].as_array().expect("sessions");
    assert_eq!(sessions.len(), 2, "setup + login sessions");
    let current = current_session(&me);
    assert_eq!(current["ip"], "203.0.113.9", "first forwarded hop only");
    assert_eq!(current["user_agent"], ua);
    assert_eq!(current["device_label"], "Chrome on macOS");

    // A session created before device capture (or by a caller with no headers) is null,
    // not an invented value.
    let previous = sessions
        .iter()
        .find(|session| session["current"] == false)
        .expect("setup session");
    assert_eq!(previous["ip"], Value::Null);
    assert_eq!(previous["user_agent"], Value::Null);
    assert_eq!(previous["device_label"], Value::Null);

    let serialized = me.to_string();
    assert!(
        !serialized.contains(token),
        "the session token must never be returned"
    );
    assert!(!serialized.contains("token_hash"));
}

#[tokio::test]
async fn an_oversized_user_agent_is_truncated() {
    let h = harness();
    h.setup().await;
    let ua = "A".repeat(2000);
    let (status, headers, _) = h
        .post(
            "/api/auth/login",
            json!({"email": "ada@example.com", "password": "correct horse battery"}),
            &[("user-agent", ua.as_str())],
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let cookie = cookie_of(&headers).expect("cookie");
    let (_, _, me) = h
        .call(Method::GET, "/api/auth/me", None, &auth_headers(&cookie))
        .await;
    let stored = current_session(&me)["user_agent"]
        .as_str()
        .expect("user agent");
    assert_eq!(stored.chars().count(), 512);
    assert!(stored.chars().all(|c| c == 'A'));
}

#[tokio::test]
async fn invitation_sign_up_captures_device_metadata() {
    let h = harness();
    let (owner, _) = h.setup().await;
    let (status, _, body) = h
        .post(
            "/api/users/invites",
            json!({"email": "reader@example.com", "role": "viewer"}),
            &csrf(&owner),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "invite: {body}");
    let token = body["token"].as_str().expect("token");
    let ua = "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15 \
              (KHTML, like Gecko) Version/17.0 Mobile/15E148 Safari/604.1";
    let (status, headers, signed) = h
        .post(
            "/api/auth/signup",
            json!({"token": token, "name": "Reader", "password": "another long password"}),
            &[("x-forwarded-for", "198.51.100.4"), ("user-agent", ua)],
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "signup: {signed}");
    let cookie = cookie_of(&headers).expect("cookie");
    let (_, _, me) = h
        .call(Method::GET, "/api/auth/me", None, &auth_headers(&cookie))
        .await;
    assert_eq!(me["user"]["email"], "reader@example.com");
    let current = current_session(&me);
    assert_eq!(current["ip"], "198.51.100.4");
    assert_eq!(current["user_agent"], ua);
    assert_eq!(current["device_label"], "Safari on iPhone");
}

#[tokio::test]
async fn sessions_are_scoped_to_the_signed_in_user() {
    let h = harness();
    let (owner, _) = h.setup().await;
    let reader = invite_and_join(&h, &owner, "reader2@example.com", "viewer").await;

    let (_, _, owner_me) = h
        .call(Method::GET, "/api/auth/me", None, &auth_headers(&owner))
        .await;
    let (_, _, reader_me) = h
        .call(Method::GET, "/api/auth/me", None, &auth_headers(&reader))
        .await;
    assert_eq!(owner_me["user"]["email"], "ada@example.com");
    assert_eq!(reader_me["user"]["email"], "reader2@example.com");
    let owner_sessions = owner_me["sessions"].as_array().expect("owner sessions");
    let reader_sessions = reader_me["sessions"].as_array().expect("reader sessions");
    assert_eq!(owner_sessions.len(), 1);
    assert_eq!(reader_sessions.len(), 1);
    assert_ne!(owner_sessions[0]["id"], reader_sessions[0]["id"]);
    assert_eq!(owner_sessions[0]["current"], true);
    assert_eq!(reader_sessions[0]["current"], true);
}
