//! Opt-in capture of request and response bodies, end to end through the real app.
//!
//! The gateway writes captures off the response path (`tokio::spawn`), so every read here
//! polls briefly. Both tests take one process-wide lock because the switches they flip
//! (`capture_bodies`, `system_logs`) are atomics shared by the whole binary.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::State;
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::Router;
use bytes::Bytes;
use http_body_util::BodyExt;
use rrouter_auth::Credentials;
use rrouter_gateway::{app, config::Config, AppState};
use rrouter_router::{Breaker, Catalog, Dispatcher, Planner};
use rrouter_store::{CaptureRecord, Store};
use serde_json::{json, Value};
use tower::ServiceExt;

/// Serializes tests that flip process-global switches.
static SWITCHES: Mutex<()> = Mutex::new(());

fn lock_switches() -> MutexGuard<'static, ()> {
    SWITCHES.lock().unwrap_or_else(|e| e.into_inner())
}

// ---------------------------------------------------------------- mock upstream

#[derive(Clone)]
struct Mock {
    hits: Arc<AtomicUsize>,
    url: String,
    plan: Arc<Mutex<Vec<Reply>>>,
}

#[derive(Clone)]
enum Reply {
    Json(Value),
    /// Raw SSE frames, written as one response body.
    Sse(String),
}

impl Mock {
    async fn start(plan: Vec<Reply>) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock");
        let addr = listener.local_addr().expect("addr");
        let mock = Mock {
            hits: Arc::new(AtomicUsize::new(0)),
            url: format!("http://{addr}"),
            plan: Arc::new(Mutex::new(plan)),
        };
        let router = Router::new()
            .route("/v1/chat/completions", post(handle))
            .with_state(mock.clone());
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        mock
    }

    fn url(&self) -> String {
        self.url.clone()
    }

    fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }
}

async fn handle(State(mock): State<Mock>, req: Request<Body>) -> Response {
    mock.hits.fetch_add(1, Ordering::SeqCst);
    // Drain the request body so the client side cannot stall on a full socket buffer.
    let _ = req.into_body().collect().await;
    let reply = {
        let mut plan = mock.plan.lock().expect("plan");
        if plan.is_empty() {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
        plan.remove(0)
    };
    match reply {
        Reply::Json(value) => axum::Json(value).into_response(),
        Reply::Sse(frames) => Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "text/event-stream")
            .body(Body::from(frames))
            .expect("sse response"),
    }
}

fn openai_reply(content: &str) -> Reply {
    Reply::Json(json!({
        "id": "chatcmpl-1",
        "object": "chat.completion",
        "created": 1,
        "model": "mock-model",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": content}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 7, "completion_tokens": 3}
    }))
}

fn openai_sse(content: &str) -> Reply {
    let delta = rrouter_core::sse::render_openai(
        &json!({"id":"c1","model":"mock-model","choices":[{"index":0,"delta":{"content":content},"finish_reason":"stop"}],"usage":{"prompt_tokens":2,"completion_tokens":2}}).to_string(),
        None,
    );
    Reply::Sse(format!("{delta}data: [DONE]\n\n"))
}

// ----------------------------------------------------------------------- app setup

struct Harness {
    state: Arc<AppState>,
    router: Router,
}

fn config() -> Config {
    Config {
        host: "127.0.0.1".parse().expect("ip"),
        port: 0,
        data_dir: std::env::temp_dir().join(format!("rrouter-capture-{}", uuid::Uuid::new_v4())),
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
        max_attempts: 2,
        retry_attempts: 0,
        retry_base_ms: 200,
        retry_max_ms: 5000,
        shutdown_grace_secs: 20,
        strategy: rrouter_router::Strategy::Priority,
        cache: rrouter_gateway::config::CacheMode::Off,
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
    }
}

fn harness() -> Harness {
    let cfg = config();
    let state = Arc::new(build_state(cfg));
    let router = app(Arc::clone(&state));
    Harness { state, router }
}

fn build_state(config: Config) -> AppState {
    let concurrency = rrouter_gateway::state::concurrency_for(config.max_concurrency);
    let data_dir = config.data_dir.clone();
    AppState {
        store: Arc::new(Store::open_in_memory().expect("store")),
        catalog: Arc::new(Catalog::builtin()),
        refresher: Arc::new(rrouter_auth::Refresher::new(
            reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .expect("client"),
        )),
        planner: Planner::new(),
        dispatcher: Arc::new(
            Dispatcher::new(Breaker::new())
                .with_max_attempts(config.max_attempts)
                .with_hedge((config.hedge_ms > 0).then(|| Duration::from_millis(config.hedge_ms))),
        ),
        http: reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("client"),
        config: rrouter_gateway::config::ConfigSnapshot::from(config),
        started: Instant::now(),
        oauth: rrouter_auth::OAuthFlows::new(),
        counters: Default::default(),
        envelope_drops: Default::default(),
        providers: Default::default(),
        concurrency: std::sync::RwLock::new(concurrency),
        auth: rrouter_gateway::auth::Auth::load(&data_dir, false),
    }
}

impl Harness {
    async fn connect(&self, provider: &str, mock: &Mock) {
        let creds = Credentials {
            api_key: Some("test-key".into()),
            base_url_override: Some(mock.url()),
            ..Credentials::default()
        };
        self.state
            .store
            .create_connection(provider, "api_key", creds, Some("mock"))
            .await
            .expect("connection");
    }

    async fn call(&self, req: Request<Body>) -> (StatusCode, axum::http::HeaderMap, Value) {
        let res = self.router.clone().oneshot(req).await.expect("oneshot");
        let status = res.status();
        let headers = res.headers().clone();
        let bytes = res.into_body().collect().await.expect("body").to_bytes();
        (
            status,
            headers,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    async fn get(&self, uri: &str) -> (StatusCode, Value) {
        let (status, _, value) = self
            .call(
                Request::builder()
                    .method("GET")
                    .uri(uri)
                    .body(Body::empty())
                    .expect("request"),
            )
            .await;
        (status, value)
    }

    async fn put(&self, uri: &str, body: Value) -> (StatusCode, Value) {
        let (status, _, value) = self
            .call(
                Request::builder()
                    .method("PUT")
                    .uri(uri)
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .expect("request"),
            )
            .await;
        (status, value)
    }

    async fn delete(&self, uri: &str) -> StatusCode {
        let (status, _, _) = self
            .call(
                Request::builder()
                    .method("DELETE")
                    .uri(uri)
                    .body(Body::empty())
                    .expect("request"),
            )
            .await;
        status
    }

    /// One chat completion, returning the response headers (for the request id).
    async fn chat(
        &self,
        body: Value,
        headers: &[(&str, &str)],
    ) -> (StatusCode, axum::http::HeaderMap) {
        let mut builder = Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("content-type", "application/json");
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let (status, headers, _) = self
            .call(builder.body(Body::from(body.to_string())).expect("request"))
            .await;
        (status, headers)
    }

    /// A streamed chat request, consumed to the last chunk so the capture is flushed.
    async fn chat_streamed(&self, body: Value) -> (StatusCode, axum::http::HeaderMap, String) {
        let req = Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .expect("request");
        let res = self.router.clone().oneshot(req).await.expect("oneshot");
        let status = res.status();
        let headers = res.headers().clone();
        let mut stream = res.into_body().into_data_stream();
        let mut wire = String::new();
        while let Some(Ok(chunk)) =
            futures_util::StreamExt::next(&mut stream).await
        {
            wire.push_str(&String::from_utf8_lossy(&chunk));
        }
        (status, headers, wire)
    }

    /// Wait for the usage row the proxy writes off the response path.
    async fn usage_for(&self, request_id: uuid::Uuid) -> rrouter_store::UsageRecord {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(row) = self
                .state
                .store
                .usage_by_request(request_id)
                .await
                .expect("usage lookup")
            {
                return row;
            }
            assert!(Instant::now() < deadline, "no usage row for {request_id}");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Wait for the capture row, or prove it never appears.
    async fn capture_for(&self, request_id: uuid::Uuid) -> Option<CaptureRecord> {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(row) = self
                .state
                .store
                .capture_by_request(request_id)
                .await
                .expect("capture lookup")
            {
                return Some(row);
            }
            if Instant::now() >= deadline {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Wait for the response half. The request half is written before the handler runs,
    /// so a row can exist for a moment with `response_body` unset; both are spawned work.
    async fn captured_response_for(&self, request_id: uuid::Uuid) -> CaptureRecord {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(row) = self
                .state
                .store
                .capture_by_request(request_id)
                .await
                .expect("capture lookup")
            {
                if row.response_body.is_some() {
                    return row;
                }
            }
            assert!(
                Instant::now() < deadline,
                "no captured response for {request_id}"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

fn request_id_of(headers: &axum::http::HeaderMap) -> uuid::Uuid {
    headers
        .get("x-request-id")
        .expect("every response carries the request id")
        .to_str()
        .expect("ascii")
        .parse()
        .expect("uuid")
}

fn chat_body() -> Value {
    json!({
        "model": "gpt-4.1",
        "messages": [
            {"role": "system", "content": "be terse"},
            {"role": "user", "content": "hello"}
        ]
    })
}

// --------------------------------------------------------------------------- tests

/// The whole opt-in story: off writes nothing, on stores redacted bodies for a JSON
/// answer and for SSE, and deleting the usage row removes the capture with it.
#[tokio::test]
async fn capture_is_opt_in_and_dies_with_its_usage_row() {
    let _guard = lock_switches();
    let mock = Mock::start(vec![
        openai_reply("off"),
        openai_reply("on"),
        openai_sse("streamed"),
        openai_reply("after"),
    ])
    .await;
    let h = harness();
    h.connect("openai", &mock).await;

    // Default: capture is off, so a served request stores no capture row at all.
    let (status, headers) = h
        .chat(chat_body(), &[("cookie", "session=topsecret")])
        .await;
    assert_eq!(status, StatusCode::OK);
    let uncaptured = request_id_of(&headers);
    h.usage_for(uncaptured).await;
    assert!(
        h.capture_for(uncaptured).await.is_none(),
        "capture off must write nothing"
    );

    // Switch it on through the same admin surface the settings page uses.
    let (status, settings) = h
        .put("/api/settings", json!({"capture_bodies": true}))
        .await;
    assert_eq!(status, StatusCode::OK, "{settings}");
    assert_eq!(settings["capture_bodies"], true);
    assert!(rrouter_gateway::settings::capture_bodies());

    // A JSON completion: both halves stored, the client cookie redacted.
    let (status, headers) = h
        .chat(chat_body(), &[("cookie", "session=topsecret")])
        .await;
    assert_eq!(status, StatusCode::OK);
    let plain = request_id_of(&headers);
    let captured = h.captured_response_for(plain).await;
    assert!(
        captured.request_body.contains("\"hello\""),
        "request body: {}",
        captured.request_body
    );
    let response = captured.response_body.as_deref().expect("response body");
    assert!(
        response.contains("\"on\""),
        "response body: {response}"
    );
    let headers_json = captured.request_headers.as_deref().expect("headers");
    assert!(
        headers_json.contains("\"[redacted]\""),
        "headers: {headers_json}"
    );
    assert!(
        !headers_json.contains("topsecret"),
        "cookie value leaked: {headers_json}"
    );
    assert!(!captured.request_truncated && !captured.response_truncated);

    // The inspector's read endpoint returns the same row and confirms capture is on.
    let (status, view) = h
        .get(&format!("/api/usage/{plain}/capture"))
        .await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(view["captured"], true);
    assert_eq!(view["enabled"], true);
    assert_eq!(view["request_headers"]["cookie"], "[redacted]");
    assert!(view["response_body"].as_str().unwrap_or("").contains("on"));

    // A streamed response stores the SSE frames as they passed.
    let mut body = chat_body();
    body["stream"] = json!(true);
    let (status, headers, wire) = h.chat_streamed(body).await;
    assert_eq!(status, StatusCode::OK);
    assert!(wire.contains("streamed"), "client wire: {wire}");
    let streamed = request_id_of(&headers);
    let captured = h.captured_response_for(streamed).await;
    let response = captured.response_body.as_deref().expect("response body");
    assert!(response.contains("data:"), "SSE frames kept: {response}");
    assert!(
        response.contains("streamed"),
        "streamed content kept: {response}"
    );

    // Deleting the request-log row deletes its capture in the same transaction.
    let row = h.usage_for(plain).await;
    let row_id = row.id.expect("row id");
    assert_eq!(
        h.delete(&format!("/api/usage/{row_id}")).await,
        StatusCode::NO_CONTENT
    );
    assert!(h.capture_for(plain).await.is_none());
    assert!(
        h.state
            .store
            .usage_by_request(plain)
            .await
            .expect("usage")
            .is_none()
    );
    // A second delete of the same id is a 404, not a silent success.
    assert_eq!(
        h.delete(&format!("/api/usage/{row_id}")).await,
        StatusCode::NOT_FOUND
    );

    // And the switch really is reversible: off again, nothing new is stored.
    let (status, _) = h
        .put("/api/settings", json!({"capture_bodies": false}))
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, headers) = h.chat(chat_body(), &[]).await;
    assert_eq!(status, StatusCode::OK);
    let after = request_id_of(&headers);
    h.usage_for(after).await;
    assert!(h.capture_for(after).await.is_none());
}

/// One system-log entry can be deleted by id; an id that is gone is a 404.
#[tokio::test]
async fn system_log_entries_can_be_deleted_one_by_one() {
    let _guard = lock_switches();
    let h = harness();
    let (status, body) = h.put("/api/system/logs", json!({"enabled": true})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // The tracing layer is installed by the binary, not by this harness, so emit the
    // entry through the same buffer the layer feeds.
    rrouter_gateway::syslog::push("info", "test", "capture-test entry".into());
    let (_, body) = h.get("/api/system/logs?after=0&limit=10").await;
    let entries = body["entries"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(!entries.is_empty(), "no entries: {body}");
    let id = entries[0]["id"].as_u64().expect("entry id");
    // Deleting an id that was never issued is a 404, so nothing is claimed falsely.
    assert_eq!(
        h.delete(&format!("/api/system/logs/{}", u64::MAX)).await,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        h.delete(&format!("/api/system/logs/{id}")).await,
        StatusCode::NO_CONTENT
    );
    let (_, body) = h.get("/api/system/logs?after=0&limit=10").await;
    let remaining: Vec<u64> = body["entries"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|entry| entry["id"].as_u64())
        .collect();
    assert!(!remaining.contains(&id), "entry {id} still listed");
    let _ = rrouter_gateway::syslog::set_enabled(false);
}
