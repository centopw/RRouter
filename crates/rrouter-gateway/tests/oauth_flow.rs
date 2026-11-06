use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
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

static MOCK: OnceLock<OAuthMock> = OnceLock::new();

fn mock() -> &'static OAuthMock {
    MOCK.get_or_init(|| {
        let std_listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("mock listener");
        let addr = std_listener.local_addr().expect("mock address");
        std_listener
            .set_nonblocking(true)
            .expect("mock nonblocking");
        let device_polls = Arc::new(AtomicUsize::new(0));
        let state = Arc::clone(&device_polls);
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

        OAuthMock { device_polls }
    })
}

async fn device() -> Json<Value> {
    Json(json!({
        "device_code": "LIVE-DEVICE-CODE",
        "user_code": "LIVE-USER-CODE",
        "verification_uri": "/verify",
        "expires_in": 900,
        "interval": 1,
    }))
}

async fn token(State(device_polls): State<Arc<AtomicUsize>>, body: String) -> impl IntoResponse {
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
        config: Config {
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
            shutdown_grace_secs: 20,
        strategy: Default::default(),
        },
        started: Instant::now(),
        oauth: rrouter_auth::OAuthFlows::new(),
        counters: Default::default(),
        concurrency: None,
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
    let mut callback = reqwest::Url::parse(uri).expect("callback URI");
    callback
        .query_pairs_mut()
        .append_pair("code", code)
        .append_pair("state", state);
    reqwest::Client::new()
        .get(callback)
        .send()
        .await
        .expect("callback request")
        .status()
}

#[tokio::test]
async fn device_flow_ends_in_stored_connection() {
    let mock = mock();
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
    let _mock = mock();
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
    let _mock = mock();
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
    let _mock = mock();
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
