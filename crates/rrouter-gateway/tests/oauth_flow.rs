use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock};
use std::thread;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::State;
use axum::http::{Request, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use http_body_util::BodyExt;
use rrouter_gateway::{app, config::Config, AppState};
use rrouter_router::{Breaker, Catalog, Dispatcher, Planner};
use rrouter_store::Store;
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tower::ServiceExt;

const ACCESS_TOKEN: &str = "LIVE-ACCESS-TOKEN";
const REFRESH_TOKEN: &str = "LIVE-REFRESH-TOKEN";
const PKCE_ACCESS_TOKEN: &str = "PKCE-ACCESS-TOKEN";
const PKCE_REFRESH_TOKEN: &str = "PKCE-REFRESH-TOKEN";
const ID_TOKEN: &str = "eyJhbGciOiJub25lIn0.eyJlbWFpbCI6InRlc3RAZXhhbXBsZS5jb20ifQ.";

#[derive(Clone)]
struct OAuthMock {
    device_polls: Arc<AtomicUsize>,
}

static MOCK: LazyLock<OAuthMock> = LazyLock::new(|| {
    let std_listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("mock listener");
    let addr = std_listener.local_addr().expect("mock address");
    std_listener
        .set_nonblocking(true)
        .expect("mock nonblocking");
    let device_polls = Arc::new(AtomicUsize::new(0));
    let code_exchanges = Arc::new(AtomicUsize::new(0));
    let state = (Arc::clone(&device_polls), Arc::clone(&code_exchanges));
    let router = Router::new()
        .route("/device", post(device))
        .route("/token", post(token))
        .route("/authorize", get(authorize))
        .route("/verify", get(verify))
        .route("/user", get(user))
        .route("/copilot_internal/v2/token", get(copilot))
        .with_state(state);
    thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("mock runtime");
        runtime.block_on(async move {
            let listener = TcpListener::from_std(std_listener).expect("tokio listener");
            axum::serve(listener, router).await.expect("mock server");
        });
    });

    std::env::set_var("RROUTER_ALLOW_INSECURE_OAUTH", "1");
    std::env::set_var(
        "RROUTER_OAUTH_OVERRIDE_GITHUB_DEVICE",
        format!("http://{addr}/device"),
    );
    std::env::set_var(
        "RROUTER_OAUTH_OVERRIDE_GITHUB_TOKEN",
        format!("http://{addr}/token"),
    );
    std::env::set_var(
        "RROUTER_OAUTH_OVERRIDE_CLAUDE_AUTHORIZE",
        format!("http://{addr}/authorize"),
    );
    std::env::set_var(
        "RROUTER_OAUTH_OVERRIDE_CLAUDE_TOKEN",
        format!("http://{addr}/token"),
    );
    std::env::set_var(
        "RROUTER_OAUTH_OVERRIDE_CODEX_AUTHORIZE",
        format!("http://{addr}/authorize"),
    );
    std::env::set_var(
        "RROUTER_OAUTH_OVERRIDE_CODEX_TOKEN",
        format!("http://{addr}/token"),
    );

    OAuthMock { device_polls }
});

/// Exchanges of the manual test's unique code, so assertions cannot race other tests.
static MANUAL_EXCHANGES: AtomicUsize = AtomicUsize::new(0);

/// Serializes the Google credential-env test (the start path reads env per call).
static GOOGLE_ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

async fn device() -> Json<Value> {
    Json(json!({
        "device_code": "LIVE-DEVICE-CODE",
        "user_code": "LIVE-USER-CODE",
        "verification_uri": "/verify",
        "expires_in": 900,
        "interval": 1,
    }))
}

async fn token(
    State((device_polls, code_exchanges)): State<(Arc<AtomicUsize>, Arc<AtomicUsize>)>,
    body: String,
) -> impl IntoResponse {
    if body.contains("device_code=LIVE-DEVICE-CODE") {
        let poll = device_polls.fetch_add(1, Ordering::SeqCst) + 1;
        if poll == 1 {
            return Json(json!({"error": "authorization_pending"}));
        }
        return Json(json!({
            "access_token": ACCESS_TOKEN,
            "refresh_token": REFRESH_TOKEN,
            "id_token": ID_TOKEN,
            "expires_in": 3600,
        }));
    }
    code_exchanges.fetch_add(1, Ordering::SeqCst);
    if body.contains("code=MANUAL-CODE") {
        MANUAL_EXCHANGES.fetch_add(1, Ordering::SeqCst);
    }
    Json(json!({
        "access_token": PKCE_ACCESS_TOKEN,
        "refresh_token": PKCE_REFRESH_TOKEN,
        "id_token": ID_TOKEN,
        "expires_in": 3600,
    }))
}

async fn authorize() -> &'static str {
    "mock authorization page"
}

async fn verify() -> &'static str {
    "mock verification page"
}

async fn user() -> Json<Value> {
    Json(json!({"login": "live-user"}))
}

async fn copilot() -> Json<Value> {
    Json(json!({"token": "LIVE-COPILOT-TOKEN", "expires_at": 4102444800_i64}))
}

struct Harness {
    router: axum::Router,
}

fn harness() -> Harness {
    let state = Arc::new(AppState {
        store: Arc::new(Store::open_in_memory().expect("store")),
        catalog: Arc::new(Catalog::builtin()),
        refresher: Arc::new(rrouter_auth::Refresher::new(
            reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .expect("client"),
        )),
        planner: Planner::new(),
        dispatcher: Arc::new(Dispatcher::new(Breaker::new())),
        http: reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("client"),
        config: rrouter_gateway::config::ConfigSnapshot::from(Config {
            host: "127.0.0.1".parse().expect("host"),
            port: 0,
            data_dir: std::env::temp_dir().join(format!("rrouter-oauth-{}", uuid::Uuid::new_v4())),
            master_key: None,
            require_api_key: false,
            request_timeout_secs: 30,
            connect_timeout_secs: 5,
            log_filter: "rrouter=info".into(),
            static_dir: None,
            cors_origins: vec![],
            allow_remote_admin: false,
            admin_token: None,
            max_concurrency: 0,
            max_attempts: 4,
            retry_attempts: 1,
            retry_base_ms: 200,
            retry_max_ms: 5000,
            shutdown_grace_secs: 20,
            strategy: Default::default(),
            cache: Default::default(),
            cache_ttl_secs: 300,
            cache_max_entries: 1000,
            usage_retention_days: 0,
            log_format: rrouter_gateway::config::LogFormat::Text,
            access_log: true,
            hedge_ms: 0,
            canary: None,
            subscription_reserve_percent: 90,
            semantic_cache: rrouter_gateway::config::CacheMode::Off,
            semantic_cache_threshold: 0.92,
            semantic_embedding_model: None,
            semantic_cache_ttl_secs: 3600,
            semantic_cache_max_entries: 1000,
        }),
        started: Instant::now(),
        oauth: rrouter_auth::OAuthFlows::new(),
        counters: Default::default(),
        concurrency: std::sync::RwLock::new(None),
        auth: rrouter_gateway::auth::Auth::load(&std::env::temp_dir(), false),
    });
    Harness { router: app(state) }
}

impl Harness {
    async fn call(&self, request: Request<Body>) -> (StatusCode, Value) {
        let response = self.router.clone().oneshot(request).await.expect("request");
        let status = response.status();
        let body = response
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes();
        (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
    }

    async fn get(&self, uri: &str) -> (StatusCode, Value) {
        self.call(
            Request::builder()
                .method("GET")
                .uri(uri)
                .body(Body::empty())
                .expect("request"),
        )
        .await
    }

    async fn post(&self, uri: &str, body: Value) -> (StatusCode, Value) {
        self.call(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .expect("request"),
        )
        .await
    }

    /// A start call with no body at all, as the current dashboard sends it.
    async fn post_empty(&self, uri: &str) -> (StatusCode, Value) {
        self.call(
            Request::builder()
                .method("POST")
                .uri(uri)
                .body(Body::empty())
                .expect("request"),
        )
        .await
    }

    async fn delete(&self, uri: &str) -> (StatusCode, Value) {
        self.call(
            Request::builder()
                .method("DELETE")
                .uri(uri)
                .body(Body::empty())
                .expect("request"),
        )
        .await
    }
}

fn pkce_details(body: &Value) -> (String, String, String) {
    let id = body["id"].as_str().expect("PKCE id").to_string();
    let redirect = body["challenge"]["redirect_uri"]
        .as_str()
        .expect("redirect URI")
        .to_string();
    let authorize = reqwest::Url::parse(
        body["challenge"]["authorize_url"]
            .as_str()
            .expect("authorize URL"),
    )
    .expect("authorize URL parse");
    let state = authorize
        .query_pairs()
        .find(|(key, _)| key == "state")
        .map(|(_, value)| value.into_owned())
        .expect("state");
    (id, redirect, state)
}

async fn complete_in_task(
    router: axum::Router,
    id: String,
) -> tokio::task::JoinHandle<(StatusCode, Value)> {
    tokio::spawn(async move {
        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/oauth/claude/complete")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"id": id}).to_string()))
                    .expect("request"),
            )
            .await
            .expect("complete request");
        let status = response.status();
        let body = response
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes();
        (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
    })
}

async fn visit_callback(uri: &str, code: &str, state: &str) -> StatusCode {
    callback_response(uri, code, state).await.0
}

/// Play the browser and keep the page body, for brand/content assertions.
async fn callback_response(uri: &str, code: &str, state: &str) -> (StatusCode, String) {
    let mut callback = reqwest::Url::parse(uri).expect("callback URI");
    callback
        .query_pairs_mut()
        .append_pair("code", code)
        .append_pair("state", state);
    let response = reqwest::Client::new()
        .get(callback)
        .send()
        .await
        .expect("callback request");
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    (status, body)
}

#[tokio::test]
async fn device_flow_ends_in_stored_connection() {
    let mock = &*MOCK;
    let harness = harness();
    let (status, start) = harness.post("/api/oauth/github/start", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{start}");
    assert_eq!(start["challenge"]["user_code"], "LIVE-USER-CODE");
    assert!(!start.to_string().contains("access_token"));

    let id = start["id"].as_str().expect("device id");
    let (status, pending) = harness.get(&format!("/api/oauth/github/status/{id}")).await;
    assert_eq!(status, StatusCode::OK, "{pending}");
    assert_eq!(pending["pending"], true);

    let (status, complete) = harness
        .post("/api/oauth/github/complete", json!({"id": id}))
        .await;
    assert_eq!(status, StatusCode::OK, "{complete}");
    assert!(complete["id"].as_str().is_some());
    assert!(mock.device_polls.load(Ordering::SeqCst) >= 2);

    let before = mock.device_polls.load(Ordering::SeqCst);
    let (status, _) = harness
        .post("/api/oauth/github/complete", json!({"id": id}))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(mock.device_polls.load(Ordering::SeqCst), before);

    let (status, connections) = harness.get("/api/connections").await;
    assert_eq!(status, StatusCode::OK, "{connections}");
    let rows = connections["data"].as_array().expect("rows");
    let oauth = rows
        .iter()
        .filter(|row| row["auth_type"] == "oauth")
        .collect::<Vec<_>>();
    assert_eq!(oauth.len(), 2);
    assert!(oauth[0]["has_access_token"] == true);
    assert!(!connections.to_string().contains(ACCESS_TOKEN));
}

#[tokio::test]
async fn pkce_flow_includes_loopback_redirect() {
    let _mock = &*MOCK;
    let harness = harness();
    let (status, start) = harness.post("/api/oauth/claude/start", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{start}");
    let (id, redirect, state) = pkce_details(&start);
    let complete = complete_in_task(harness.router.clone(), id).await;
    assert_eq!(
        visit_callback(&redirect, "PKCE-CODE", &state).await,
        StatusCode::OK
    );
    let (status, body) = complete.await.expect("complete task");
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["id"].as_str().is_some());
}

#[tokio::test]
async fn state_mismatch_fails_without_storing_connection() {
    let _mock = &*MOCK;
    let harness = harness();
    let (status, start) = harness.post("/api/oauth/claude/start", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{start}");
    let (id, redirect, _state) = pkce_details(&start);
    let complete = complete_in_task(harness.router.clone(), id).await;
    assert_eq!(
        visit_callback(&redirect, "PKCE-CODE", "wrong-state").await,
        StatusCode::BAD_REQUEST
    );
    let (status, _) = complete.await.expect("complete task");
    assert!(status.is_client_error() || status.is_server_error());
    let (status, connections) = harness.get("/api/connections").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(connections["data"].as_array().expect("rows").len(), 0);
}

#[tokio::test]
async fn connection_view_contains_no_token_material() {
    let _mock = &*MOCK;
    let harness = harness();
    let (status, start) = harness.post("/api/oauth/claude/start", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{start}");
    let (id, redirect, state) = pkce_details(&start);
    let complete = complete_in_task(harness.router.clone(), id).await;
    assert_eq!(
        visit_callback(&redirect, "PKCE-CODE", &state).await,
        StatusCode::OK
    );
    assert_eq!(complete.await.expect("complete task").0, StatusCode::OK);

    let (status, connections) = harness.get("/api/connections").await;
    assert_eq!(status, StatusCode::OK, "{connections}");
    let text = connections.to_string();
    assert!(text.contains("has_access_token"));
    assert!(text.contains("has_refresh_token"));
    assert!(!text.contains(PKCE_ACCESS_TOKEN));
    assert!(!text.contains(PKCE_REFRESH_TOKEN));
}

/// Manual mode is the remote-VPS path: no listener is bound, the authorize URL still
/// carries a registered redirect, and completion accepts the pasted final URL.
#[tokio::test]
async fn manual_pkce_start_completes_from_the_pasted_callback() {
    let _mock = &*MOCK;
    let harness = harness();
    let (status, start) = harness
        .post("/api/oauth/codex/start", json!({"manual": true}))
        .await;
    assert_eq!(status, StatusCode::OK, "{start}");
    assert_eq!(start["flow"], "pkce");
    assert_eq!(start["challenge"]["manual"], true);
    assert_eq!(
        start["challenge"]["redirect_uri"],
        "http://127.0.0.1:1455/auth/callback"
    );
    // The registered port is free: manual mode never bound it.
    let free = std::net::TcpListener::bind(("127.0.0.1", 1455));
    assert!(
        free.is_ok(),
        "manual start must not bind the registered port"
    );
    drop(free);

    let (id, _redirect, state) = pkce_details(&start);
    let before = MANUAL_EXCHANGES.load(Ordering::SeqCst);
    let callback = format!("http://127.0.0.1:1455/auth/callback?code=MANUAL-CODE&state={state}");
    let (status, done) = harness
        .post(
            "/api/oauth/codex/complete",
            json!({"id": id, "callback_url": callback}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{done}");
    assert!(done["id"].as_str().is_some());
    assert_eq!(
        MANUAL_EXCHANGES.load(Ordering::SeqCst),
        before + 1,
        "the pasted code was exchanged exactly once"
    );

    // Duplicate completion replays and must not spend the code again.
    let (status, _again) = harness
        .post(
            "/api/oauth/codex/complete",
            json!({"id": id, "callback_url": callback}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        MANUAL_EXCHANGES.load(Ordering::SeqCst),
        before + 1,
        "no second exchange"
    );

    let (status, connections) = harness.get("/api/connections").await;
    assert_eq!(status, StatusCode::OK, "{connections}");
    let rows = connections["data"].as_array().expect("rows");
    assert!(rows.iter().any(|row| row["provider"] == "codex"));
    assert!(!connections.to_string().contains(PKCE_ACCESS_TOKEN));
}

/// A pending id is bound to the provider that started it. Redeeming it through another
/// provider route must fail before any exchange or connection write.
#[tokio::test]
async fn completion_rejects_a_pending_id_from_another_provider() {
    let _mock = &*MOCK;
    let harness = harness();
    let (status, start) = harness
        .post("/api/oauth/codex/start", json!({"manual": true}))
        .await;
    assert_eq!(status, StatusCode::OK, "{start}");
    let (id, _, _) = pkce_details(&start);

    let (status, body) = harness
        .post("/api/oauth/claude/complete", json!({"id": id}))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body.to_string().contains("belongs to provider codex"),
        "{body}"
    );

    let (status, body) = harness.get(&format!("/api/oauth/claude/status/{id}")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    let (status, body) = harness.delete(&format!("/api/oauth/claude/{id}")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    let (status, body) = harness.get(&format!("/api/oauth/codex/status/{id}")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["pending"], true);

    let (status, body) = harness.delete(&format!("/api/oauth/codex/{id}")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["cancelled"], true);

    let (status, connections) = harness.get("/api/connections").await;
    assert_eq!(status, StatusCode::OK, "{connections}");
    assert_eq!(connections["data"].as_array().expect("rows").len(), 0);
}

#[tokio::test]
async fn manual_pkce_start_uses_a_stable_redirect_for_a_dynamic_port_provider() {
    let _mock = &*MOCK;
    let harness = harness();
    let (status, start) = harness
        .post("/api/oauth/claude/start", json!({"manual": true}))
        .await;
    assert_eq!(status, StatusCode::OK, "{start}");
    assert_eq!(start["challenge"]["manual"], true);
    assert_eq!(
        start["challenge"]["redirect_uri"],
        "http://localhost:54545/callback"
    );
    let (id, _, _) = pkce_details(&start);
    let (status, _) = harness.delete(&format!("/api/oauth/claude/{id}")).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn manual_complete_rejects_a_foreign_state() {
    let _mock = &*MOCK;
    let harness = harness();
    let (status, start) = harness
        .post("/api/oauth/codex/start", json!({"manual": true}))
        .await;
    assert_eq!(status, StatusCode::OK, "{start}");
    let (id, _, _) = pkce_details(&start);
    let (status, body) = harness
        .post(
            "/api/oauth/codex/complete",
            json!({
                "id": id,
                "callback_url": "http://127.0.0.1:1455/auth/callback?code=STOLEN&state=ATTACKER"
            }),
        )
        .await;
    assert!(status.is_client_error(), "{status} {body}");
    assert!(body.to_string().contains("state mismatch"), "{body}");

    let (status, connections) = harness.get("/api/connections").await;
    assert_eq!(status, StatusCode::OK, "{connections}");
    assert_eq!(connections["data"].as_array().expect("rows").len(), 0);
}

/// Empty start bodies remain valid for the current dashboard, and imply loopback mode.
#[tokio::test]
async fn start_with_an_empty_body_stays_loopback() {
    let _mock = &*MOCK;
    let harness = harness();
    let (status, start) = harness.post_empty("/api/oauth/claude/start").await;
    assert_eq!(status, StatusCode::OK, "{start}");
    assert_eq!(start["challenge"]["manual"], false);
    let id = start["id"].as_str().expect("id");
    let (status, _) = harness.delete(&format!("/api/oauth/claude/{id}")).await;
    assert_eq!(status, StatusCode::OK);
}

/// The loopback pages are branded, accessible and never echo token material.
#[tokio::test]
async fn loopback_pages_are_branded_and_token_free() {
    let _mock = &*MOCK;
    let harness = harness();
    let (status, start) = harness.post("/api/oauth/claude/start", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{start}");
    let (id, redirect, state) = pkce_details(&start);
    let complete = complete_in_task(harness.router.clone(), id).await;
    let (status, body) = callback_response(&redirect, "PKCE-CODE", &state).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    for needle in ["RRouter", "Signed in", "Claude", "You can close this tab."] {
        assert!(body.contains(needle), "page missing {needle}: {body}");
    }
    assert!(!body.contains(PKCE_ACCESS_TOKEN), "{body}");
    assert!(!body.contains(PKCE_REFRESH_TOKEN), "{body}");
    assert_eq!(complete.await.expect("complete task").0, StatusCode::OK);
}

/// A mismatched redirect renders the failure card, not the success card.
#[tokio::test]
async fn loopback_failure_page_is_branded() {
    let _mock = &*MOCK;
    let harness = harness();
    let (status, start) = harness.post("/api/oauth/claude/start", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{start}");
    let (id, redirect, _state) = pkce_details(&start);
    let complete = complete_in_task(harness.router.clone(), id).await;
    let (status, body) = callback_response(&redirect, "PKCE-CODE", "ATTACKER").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.contains("RRouter"), "{body}");
    assert!(body.contains("Sign-in failed"), "{body}");
    assert!(!body.contains(PKCE_ACCESS_TOKEN), "{body}");
    assert!(complete.await.expect("complete task").0.is_client_error());
}

/// No Google OAuth client ships in the repository: start must refuse with a 400 naming
/// The public desktop client is built in, so sign-in works without operator env.
#[allow(clippy::await_holding_lock)] // serializes the credential env mutation
#[tokio::test]
async fn google_antigravity_start_uses_the_builtin_client() {
    let _guard = GOOGLE_ENV
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    for name in [
        "GOOGLE_ANTIGRAVITY_CLIENT_ID",
        "GOOGLE_ANTIGRAVITY_CLIENT_SECRET",
        "RROUTER_OAUTH_CLIENT_ID_GOOGLE_ANTIGRAVITY",
        "RROUTER_OAUTH_CLIENT_SECRET_GOOGLE_ANTIGRAVITY",
    ] {
        std::env::remove_var(name);
    }

    let harness = harness();
    let (status, body) = harness
        .post(
            "/api/oauth/google-antigravity/start",
            json!({"manual": true}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let url = body["challenge"]["authorize_url"]
        .as_str()
        .expect("authorize_url");
    assert!(
        url.contains(rrouter_auth::antigravity::DEFAULT_CLIENT_ID),
        "{url}"
    );
    assert!(url.contains("localhost%3A54545%2Fcallback"), "{url}");
}
