//! The system-managed playground key end to end: minted on first use, stable across a
//! restart, listed and editable on the Access page, and refused on `/v1` unless the
//! request comes from the dashboard playground with an admin session or the configured
//! admin token.

use std::path::{Path, PathBuf};
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

const ADMIN_TOKEN: &str = "adm-test";

struct H {
    state: Arc<AppState>,
    router: Router,
}

fn config(data_dir: &Path) -> Config {
    Config {
        host: "127.0.0.1".parse().expect("ip"),
        port: 0,
        data_dir: data_dir.to_path_buf(),
        master_key: None,
        require_api_key: true,
        request_timeout_secs: 5,
        connect_timeout_secs: 1,
        log_filter: "rrouter=error".into(),
        static_dir: None,
        cors_origins: vec![],
        allow_remote_admin: false,
        admin_token: Some(ADMIN_TOKEN.into()),
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

fn data_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rrouter-keys-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("data dir");
    dir
}

/// A gateway over a file-backed store in `dir`, so a second harness on the same dir is a
/// restart: same database, same `master.key`.
fn harness(dir: &Path) -> H {
    let client = || {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .expect("client")
    };
    let state = Arc::new(AppState {
        store: Arc::new(Store::open(&dir.join("rrouter.db"), None).expect("store")),
        catalog: Arc::new(Catalog::builtin()),
        refresher: Arc::new(rrouter_auth::Refresher::new(client())),
        planner: Planner::new(),
        dispatcher: Arc::new(Dispatcher::new(Breaker::new()).with_max_attempts(4)),
        http: client(),
        config: rrouter_gateway::config::ConfigSnapshot::from(config(dir)),
        started: std::time::Instant::now(),
        oauth: rrouter_auth::OAuthFlows::new(),
        counters: Default::default(),
        concurrency: std::sync::RwLock::new(None),
        auth: Auth::load(dir, false),
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

    async fn get(&self, uri: &str, headers: &[(&str, &str)]) -> (StatusCode, Value) {
        let (status, _, body) = self.call(Method::GET, uri, None, headers).await;
        (status, body)
    }

    async fn post(&self, uri: &str, body: Value, headers: &[(&str, &str)]) -> (StatusCode, Value) {
        let (status, _, out) = self.call(Method::POST, uri, Some(body), headers).await;
        (status, out)
    }

    /// `POST /api/playground/key` with the admin token: `(key, id, created)`.
    async fn ensure(&self) -> (String, String, bool) {
        let (status, body) = self
            .post(
                "/api/playground/key",
                json!({}),
                &[("x-admin-token", ADMIN_TOKEN)],
            )
            .await;
        assert_eq!(status, StatusCode::OK, "ensure: {body}");
        (
            body["key"].as_str().expect("key").to_string(),
            body["id"].as_str().expect("id").to_string(),
            body["created"].as_bool().expect("created"),
        )
    }

    /// Create the owner (an admin) and return the session cookie.
    async fn setup(&self) -> String {
        let (status, headers, body) = self
            .call(
                Method::POST,
                "/api/auth/setup",
                Some(json!({
                    "setup_code": self.state.auth.code().unwrap_or_default(),
                    "name": "Ada Lovelace",
                    "email": "ada@example.com",
                    "password": "correct horse battery",
                })),
                &[],
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "setup: {body}");
        cookie_of(&headers).expect("owner cookie")
    }

    /// Invite a non-admin `member`, accept, and return their session cookie.
    async fn member(&self, owner: &str) -> String {
        let (status, body) = self
            .post(
                "/api/users/invites",
                json!({"email": "member@example.com", "role": "member"}),
                &[("cookie", owner), ("x-rrouter-csrf", "1")],
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "invite: {body}");
        let token = body["token"].as_str().expect("token").to_string();
        let (status, headers, signed) = self
            .call(
                Method::POST,
                "/api/auth/signup",
                Some(
                    json!({"token": token, "name": "Member", "password": "another long password"}),
                ),
                &[],
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "signup: {signed}");
        cookie_of(&headers).expect("member cookie")
    }
}

fn cookie_of(headers: &HeaderMap) -> Option<String> {
    headers
        .get("set-cookie")
        .and_then(|v| v.to_str().ok())
        .and_then(|raw| raw.split(';').next())
        .map(|pair| pair.trim().to_string())
        .filter(|pair| pair.starts_with("rrouter_session="))
}

fn code(body: &Value) -> &str {
    body["error"]["code"].as_str().unwrap_or_default()
}

#[tokio::test]
async fn playground_key_is_minted_once_and_survives_a_restart() {
    let dir = data_dir();
    let h = harness(&dir);
    let (key, id, created) = h.ensure().await;
    assert!(created);
    assert!(key.starts_with(rrouter_store::API_KEY_PREFIX));
    let (again, same, created) = h.ensure().await;
    assert_eq!(
        (again.as_str(), same.as_str(), created),
        (key.as_str(), id.as_str(), false)
    );
    drop(h);

    let restarted = harness(&dir);
    let (after, same, created) = restarted.ensure().await;
    assert_eq!(
        (after.as_str(), same.as_str(), created),
        (key.as_str(), id.as_str(), false)
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn playground_key_is_listed_with_its_kind_editable_and_not_counted() {
    let dir = data_dir();
    let h = harness(&dir);
    let admin = [("x-admin-token", ADMIN_TOKEN)];
    let (key, id, _) = h.ensure().await;
    let (status, _) = h.post("/api/keys", json!({"name": "client"}), &admin).await;
    assert_eq!(status, StatusCode::OK);

    let (status, list) = h.get("/api/keys", &admin).await;
    assert_eq!(status, StatusCode::OK);
    let rows = list["data"].as_array().expect("rows");
    let row = rows
        .iter()
        .find(|r| r["id"] == id.as_str())
        .expect("listed");
    assert_eq!(row["kind"], "playground");
    assert!(rows.iter().any(|r| r["kind"] == "user"));
    assert!(
        !list.to_string().contains(&key),
        "the list must never carry the plaintext"
    );

    let (status, system) = h.get("/api/system", &admin).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        system["counts"]["api_keys"], 1,
        "only the operator's key counts"
    );

    let (status, _, patched) = h
        .call(
            Method::PATCH,
            &format!("/api/keys/{id}"),
            Some(json!({"rpm": 7, "allowed_models": ["openai/*"]})),
            &admin,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "patch: {patched}");
    assert_eq!(
        (patched["rpm"].as_u64(), patched["kind"].as_str()),
        (Some(7), Some("playground"))
    );

    let (status, _, refused) = h
        .call(
            Method::PATCH,
            &format!("/api/keys/{id}"),
            Some(json!({"scope": "admin"})),
            &admin,
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "admin scope: {refused}");
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn deleting_the_playground_key_means_the_next_ensure_recreates_it() {
    let dir = data_dir();
    let h = harness(&dir);
    let (key, id, _) = h.ensure().await;
    let (status, _, _) = h
        .call(
            Method::DELETE,
            &format!("/api/keys/{id}"),
            None,
            &[("x-admin-token", ADMIN_TOKEN)],
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (fresh, fresh_id, created) = h.ensure().await;
    assert!(created);
    assert_ne!(
        (fresh.as_str(), fresh_id.as_str()),
        (key.as_str(), id.as_str())
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn ensure_requires_admin() {
    let dir = data_dir();
    let h = harness(&dir);
    let owner = h.setup().await;
    let member = h.member(&owner).await;
    let (status, _) = h.post("/api/playground/key", json!({}), &[]).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = h
        .post(
            "/api/playground/key",
            json!({}),
            &[("cookie", member.as_str()), ("x-rrouter-csrf", "1")],
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, body) = h
        .post(
            "/api/playground/key",
            json!({}),
            &[("cookie", owner.as_str()), ("x-rrouter-csrf", "1")],
        )
        .await;
    assert_eq!(status, StatusCode::OK, "admin session: {body}");
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn playground_key_is_refused_on_v1_outside_the_playground() {
    let dir = data_dir();
    let h = harness(&dir);
    let owner = h.setup().await;
    let member = h.member(&owner).await;
    let (key, _, _) = h.ensure().await;
    let bearer = format!("Bearer {key}");
    let bearer = bearer.as_str();

    // The bearer alone, the marker without a session, and a session without the marker
    // are all refused with the scope code; so is a non-admin account.
    for headers in [
        vec![("authorization", bearer)],
        vec![("authorization", bearer), ("x-rrouter-playground", "1")],
        vec![("authorization", bearer), ("cookie", owner.as_str())],
        vec![
            ("authorization", bearer),
            ("x-rrouter-playground", "1"),
            ("cookie", member.as_str()),
        ],
    ] {
        let (status, body) = h.get("/v1/models", &headers).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{headers:?}: {body}");
        assert_eq!(code(&body), "key_scope", "{headers:?}");
    }
    let (status, body) = h
        .post(
            "/v1/chat/completions",
            json!({"model": "gpt-4o-mini", "messages": [{"role": "user", "content": "hi"}]}),
            &[("authorization", bearer)],
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "chat: {body}");
    assert_eq!(code(&body), "key_scope");

    // Marker + admin session: accepted.
    let (status, body) = h
        .get(
            "/v1/models",
            &[
                ("authorization", bearer),
                ("x-rrouter-playground", "1"),
                ("cookie", owner.as_str()),
            ],
        )
        .await;
    assert_eq!(status, StatusCode::OK, "playground: {body}");

    // An ordinary key is unaffected by the marker rule.
    let (status, created) = h
        .post(
            "/api/keys",
            json!({"name": "client"}),
            &[("x-admin-token", ADMIN_TOKEN)],
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let user = format!("Bearer {}", created["key"].as_str().expect("key"));
    let (status, body) = h
        .get("/v1/models", &[("authorization", user.as_str())])
        .await;
    assert_eq!(status, StatusCode::OK, "user key: {body}");
    std::fs::remove_dir_all(&dir).ok();
}

/// A token-only instance has no dashboard session, so the configured admin token is the
/// admin authority: the marker plus `x-admin-token` admits the playground key, the token
/// without the marker is still outside the playground, a wrong token proves nothing, and
/// a signed-in member gains no authority from the marker.
#[tokio::test]
async fn playground_key_is_admitted_by_the_admin_token_alone() {
    let dir = data_dir();
    let h = harness(&dir);
    let owner = h.setup().await;
    let member = h.member(&owner).await;
    let (key, _, _) = h.ensure().await;
    let bearer = format!("Bearer {key}");

    // No cookie at all: the token is the whole authority.
    let (status, body) = h
        .get(
            "/v1/models",
            &[
                ("authorization", bearer.as_str()),
                ("x-admin-token", ADMIN_TOKEN),
                ("x-rrouter-playground", "1"),
            ],
        )
        .await;
    assert_eq!(status, StatusCode::OK, "admin token: {body}");

    let (status, body) = h
        .get(
            "/v1/models",
            &[
                ("authorization", bearer.as_str()),
                ("x-admin-token", ADMIN_TOKEN),
            ],
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "no marker: {body}");
    assert_eq!(code(&body), "key_scope");

    let (status, body) = h
        .get(
            "/v1/models",
            &[
                ("authorization", bearer.as_str()),
                ("x-admin-token", "not-the-admin-token"),
                ("x-rrouter-playground", "1"),
            ],
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "wrong token: {body}");
    assert_eq!(code(&body), "key_scope");

    let (status, body) = h
        .get(
            "/v1/models",
            &[
                ("authorization", bearer.as_str()),
                ("x-rrouter-playground", "1"),
                ("cookie", member.as_str()),
            ],
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "member: {body}");
    assert_eq!(code(&body), "key_scope");
    std::fs::remove_dir_all(&dir).ok();
}
