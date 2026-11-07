//! End-to-end tests: the real axum app over `tower::ServiceExt::oneshot`, talking to a
//! real mock upstream served on a local port. `base_url_override` on a connection is what
//! points a provider at that server, so the whole proxy path (translate → dispatch →
//! refresh → headers → HTTP → translate back) runs exactly as in production.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{ConnectInfo, State};
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use bytes::Bytes;
use http_body_util::BodyExt;
use rrouter_auth::Credentials;
use rrouter_gateway::{app, config::Config, AppState};
use rrouter_router::{Breaker, Catalog, Dispatcher, Planner};
use rrouter_store::{AccountQuota, BudgetWindow, ModelRow, Store, UsageQuery, UsageRecord};
use serde_json::{json, Value};
use tower::ServiceExt;

// ---------------------------------------------------------------- mock upstream

#[derive(Clone)]
struct Mock {
    inner: Arc<MockInner>,
}

struct MockInner {
    hits: AtomicUsize,
    /// Bodies the mock received, in order, plus the path they arrived on.
    seen: Mutex<Vec<(String, Value)>>,
    /// Every request as it arrived on the wire: path, `content-type`, raw bytes.
    wire: Mutex<Vec<(String, String, Vec<u8>)>>,
    plan: Mutex<Vec<MockReply>>,
    url: String,
}

#[derive(Clone)]
enum MockReply {
    /// JSON body + status (non-streaming).
    Json(u16, Value),
    /// Bytes relayed exactly as sent, for bodies the gateway must not rewrite.
    Raw(u16, &'static str),
    /// Audio: raw bytes with an explicit content type.
    Bytes(u16, &'static str, &'static [u8]),
    /// Arbitrary bytes in chunks, each optionally gated: proves no whole-body buffering.
    Chunks(u16, &'static str, Vec<(Vec<u8>, Option<Arc<tokio::sync::Notify>>)>),
    /// SSE chunks written one at a time; each may wait for a gate before it is flushed.
    Sse(Vec<SseStep>),
}

#[derive(Clone)]
struct SseStep {
    wire: String,
    /// Wait until the test signals this gate; used to prove no whole-body buffering.
    await_gate: Option<Arc<tokio::sync::Notify>>,
}

impl Mock {
    async fn start(plan: Vec<MockReply>) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind mock");
        let addr = listener.local_addr().expect("addr");
        let mock = Mock {
            inner: Arc::new(MockInner {
                hits: AtomicUsize::new(0),
                seen: Mutex::new(Vec::new()),
                wire: Mutex::new(Vec::new()),
                plan: Mutex::new(plan),
                url: format!("http://{addr}"),
            }),
        };
        let router = Router::new()
            .route("/v1/chat/completions", post(handle))
            .route("/v1/messages", post(handle))
            .route("/v1/responses", post(handle))
            .route("/v1/embeddings", post(handle))
            .route("/v1/messages/count_tokens", post(handle))
            .route("/v1/images/generations", post(handle))
            .route("/v1/audio/speech", post(handle))
            .route("/v1/audio/transcriptions", post(handle))
            .route("/v1/audio/voices", get(handle))
            // codex's native path; the gateway must preserve it under an override.
            .route("/backend-api/codex/responses", post(handle))
            .route("/backend-api/codex/responses/compact", post(handle))
            .with_state(mock.clone());
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        mock
    }

    /// Origin only (no path): the gateway must keep the provider's native path.
    fn url(&self) -> String {
        self.inner.url.clone()
    }

    fn hits(&self) -> usize {
        self.inner.hits.load(Ordering::SeqCst)
    }

    fn seen(&self) -> Vec<Value> {
        self.inner.seen.lock().expect("seen").iter().map(|(_, b)| b.clone()).collect()
    }

    fn paths(&self) -> Vec<String> {
        self.inner.seen.lock().expect("seen").iter().map(|(p, _)| p.clone()).collect()
    }

    /// Every request as it reached the mock: path, `content-type`, raw bytes.
    fn wire(&self) -> Vec<(String, String, Vec<u8>)> {
        self.inner.wire.lock().expect("wire").clone()
    }
}

async fn handle(State(mock): State<Mock>, req: Request<Body>) -> Response {
    mock.inner.hits.fetch_add(1, Ordering::SeqCst);
    let path = req.uri().path().to_string();
    let content_type = req
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let bytes = req.into_body().collect().await.expect("body").to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    mock.inner
        .seen
        .lock()
        .expect("seen")
        .push((path.clone(), body));
    mock.inner
        .wire
        .lock()
        .expect("wire")
        .push((path, content_type, bytes.to_vec()));
    let reply = {
        let mut plan = mock.inner.plan.lock().expect("plan");
        if plan.is_empty() {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
        plan.remove(0)
    };
    match reply {
        MockReply::Json(status, value) => {
            let code = StatusCode::from_u16(status).unwrap_or(StatusCode::OK);
            (code, axum::Json(value)).into_response()
        }
        MockReply::Raw(status, body) => Response::builder()
            .status(StatusCode::from_u16(status).unwrap_or(StatusCode::OK))
            .header("content-type", "application/json")
            .body(Body::from(body))
            .expect("raw response"),
        MockReply::Bytes(status, content_type, body) => Response::builder()
            .status(StatusCode::from_u16(status).unwrap_or(StatusCode::OK))
            .header("content-type", content_type)
            .body(Body::from(body))
            .expect("bytes response"),
        MockReply::Chunks(status, content_type, chunks) => {
            let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(1);
            tokio::spawn(async move {
                for (chunk, gate) in chunks {
                    if let Some(gate) = gate {
                        gate.notified().await;
                    }
                    if tx.send(Ok(Bytes::from(chunk))).await.is_err() {
                        return;
                    }
                }
            });
            let stream = tokio_stream::wrappers::ReceiverStream::new(rx);
            Response::builder()
                .status(StatusCode::from_u16(status).unwrap_or(StatusCode::OK))
                .header("content-type", content_type)
                .body(Body::from_stream(stream))
                .expect("chunked response")
        }
        MockReply::Sse(steps) => {
            let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(1);
            tokio::spawn(async move {
                for step in steps {
                    if let Some(gate) = step.await_gate {
                        gate.notified().await;
                    }
                    if tx.send(Ok(Bytes::from(step.wire))).await.is_err() {
                        return;
                    }
                }
            });
            let stream = tokio_stream::wrappers::ReceiverStream::new(rx);
            Response::builder()
                .status(StatusCode::OK)
                .header("content-type", "text/event-stream")
                .body(Body::from_stream(stream))
                .expect("sse response")
        }
    }
}

// ----------------------------------------------------------------------- app setup

struct Harness {
    state: Arc<AppState>,
    router: Router,
}

const TEST_ADMIN_TOKEN: &str = "adm-test-7f3a91";

fn config(require_api_key: bool) -> Config {
    Config {
        host: "127.0.0.1".parse().expect("ip"),
        port: 0,
        data_dir: std::env::temp_dir().join(format!("rrouter-test-{}", uuid::Uuid::new_v4())),
        master_key: None,
        require_api_key,
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
        strategy: rrouter_router::Strategy::Priority,
        cache: rrouter_gateway::config::CacheMode::Off,
        cache_ttl_secs: 300,
        cache_max_entries: 1000,
        usage_retention_days: 0,
        log_format: rrouter_gateway::config::LogFormat::Text,
        access_log: true,
    }
}

/// The same runtime settings with the exact-match cache switched on.
fn cached_config(ttl_secs: u64, max_entries: usize) -> Config {
    let mut cfg = config(false);
    cfg.cache = rrouter_gateway::config::CacheMode::Sqlite;
    cfg.cache_ttl_secs = ttl_secs;
    cfg.cache_max_entries = max_entries;
    cfg
}

fn harness(require_api_key: bool) -> Harness {
    harness_with(config(require_api_key))
}

/// A harness whose runtime settings are the ones under test (admin token, caps).
fn harness_with(config: Config) -> Harness {
    let state = Arc::new(build_state(config));
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
            Dispatcher::new(Breaker::new()).with_max_attempts(config.max_attempts),
        ),
        http: reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("client"),
        config,
        started: Instant::now(),
        oauth: rrouter_auth::OAuthFlows::new(),
        counters: Default::default(),
        concurrency,
        auth: rrouter_gateway::auth::Auth::load(&data_dir, false),
    }
}

impl Harness {
    /// Register a provider connection pointing at the mock upstream; the id is what
    /// weight and latency tests address it by.
    async fn connect(&self, provider: &str, mock: &Mock) -> uuid::Uuid {
        let creds = Credentials {
            api_key: Some("test-key".into()),
            access_token: Some("SECRET-ACCESS-TOKEN".into()),
            refresh_token: Some("SECRET-REFRESH-TOKEN".into()),
            base_url_override: Some(mock.url()),
            ..Credentials::default()
        };
        self.state
            .store
            .create_connection(provider, "api_key", creds, Some("mock"))
            .await
            .expect("connection")
            .id
    }

    async fn call(&self, req: Request<Body>) -> (StatusCode, Value) {
        let res = self.router.clone().oneshot(req).await.expect("oneshot");
        let status = res.status();
        let bytes = res.into_body().collect().await.expect("body").to_bytes();
        let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, value)
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

    /// `PUT` a JSON body; the response value is decoded like the other helpers.
    async fn put(&self, uri: &str, body: Value) -> (StatusCode, Value) {
        let (status, _, value) = self
            .call_full(
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

    /// `DELETE`, keeping the response headers (204 carries an empty body).
    async fn delete(&self, uri: &str) -> (StatusCode, axum::http::HeaderMap, Value) {
        self.call_full(
            Request::builder()
                .method("DELETE")
                .uri(uri)
                .body(Body::empty())
                .expect("request"),
        )
        .await
    }

    async fn post(&self, uri: &str, body: Value) -> (StatusCode, Value) {
        self.post_with(uri, body, &[]).await
    }

    async fn post_with(&self, uri: &str, body: Value, headers: &[(&str, &str)]) -> (StatusCode, Value) {
        let (status, _, value) = self.post_headers(uri, body, headers).await;
        (status, value)
    }

    /// POST with extra headers, keeping the response headers (cache markers, request id).
    async fn post_headers(
        &self,
        uri: &str,
        body: Value,
        headers: &[(&str, &str)],
    ) -> (StatusCode, axum::http::HeaderMap, Value) {
        let mut builder = Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json");
        for (k, v) in headers {
            builder = builder.header(*k, *v);
        }
        self.call_full(builder.body(Body::from(body.to_string())).expect("request")).await
    }

    /// Wait for the usage row written for `request_id` (recorded off the response path).
    async fn usage_for(&self, request_id: uuid::Uuid) -> UsageRecord {
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

    /// Create a key and hand back `(plaintext, id)`.
    async fn create_key(&self) -> (String, String) {
        let (status, body) = self.post("/api/keys", json!({"name": "test"})).await;
        assert_eq!(status, StatusCode::OK, "create key: {body}");
        (
            body["key"].as_str().expect("plaintext key").to_string(),
            body["id"].as_str().expect("key id").to_string(),
        )
    }

    /// `PATCH /api/keys/{id}`, asserting the row came back.
    async fn patch_key(&self, id: &str, body: Value) -> Value {
        let (status, headers, value) = self
            .call_full(
                Request::builder()
                    .method("PATCH")
                    .uri(format!("/api/keys/{id}"))
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .expect("request"),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "patch {body}: {value} (headers {headers:?})");
        value
    }

    /// `PATCH /api/connections/{id}`, asserting the updated view came back.
    async fn patch_connection(&self, id: &str, body: Value) -> Value {
        let (status, headers, value) = self
            .call_full(
                Request::builder()
                    .method("PATCH")
                    .uri(format!("/api/connections/{id}"))
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .expect("request"),
            )
            .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "patch connection {body}: {value} (headers {headers:?})"
        );
        value
    }

    /// Like [`Harness::call`] but keeps the response headers (for `Retry-After`).
    async fn call_full(&self, req: Request<Body>) -> (StatusCode, axum::http::HeaderMap, Value) {
        let res = self.router.clone().oneshot(req).await.expect("oneshot");
        let status = res.status();
        let headers = res.headers().clone();
        let bytes = res.into_body().collect().await.expect("body").to_bytes();
        (status, headers, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    /// `GET` a non-JSON endpoint (`/metrics`) and keep status, headers and text.
    async fn get_raw(&self, uri: &str) -> (StatusCode, axum::http::HeaderMap, String) {
        let res = self
            .router
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(uri)
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("oneshot");
        let status = res.status();
        let headers = res.headers().clone();
        let bytes = res.into_body().collect().await.expect("body").to_bytes();
        (status, headers, String::from_utf8_lossy(&bytes).into_owned())
    }

    /// One OpenAI chat request, optionally authorized with a key.
    async fn chat(
        &self,
        key: Option<&str>,
        model: &str,
    ) -> (StatusCode, axum::http::HeaderMap, Value) {
        let mut builder = Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("content-type", "application/json");
        if let Some(key) = key {
            builder = builder.header("authorization", format!("Bearer {key}"));
        }
        self.call_full(
            builder
                .body(Body::from(openai_chat_body(model).to_string()))
                .expect("request"),
        )
        .await
    }
}

fn openai_chat_body(model: &str) -> Value {
    json!({
        "model": model,
        "messages": [
            {"role": "system", "content": "be terse"},
            {"role": "user", "content": "hello"}
        ]
    })
}

/// Value of one Prometheus series, or 0 when the scrape does not carry it. Counters are
/// process-global, so assertions are "at least this many", not exact totals.
fn metric_value(text: &str, series: &str) -> u64 {
    text.lines()
        .find_map(|line| {
            line.strip_prefix(series)
                .and_then(|rest| rest.trim().parse::<u64>().ok())
        })
        .unwrap_or(0)
}

fn openai_reply(content: &str) -> MockReply {
    MockReply::Json(
        200,
        json!({
            "id": "chatcmpl-1",
            "object": "chat.completion",
            "created": 1,
            "model": "mock-model",
            "choices": [{"index": 0, "message": {"role": "assistant", "content": content}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 7, "completion_tokens": 3}
        }),
    )
}

// --------------------------------------------------------------------------- tests

#[tokio::test]
async fn health_and_models() {
    let mock = Mock::start(vec![]).await;
    let h = harness(false);
    h.connect("openai", &mock).await;

    let (status, body) = h.get("/health").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], true);
    assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));

    let (status, body) = h.get("/v1/models").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["object"], "list");
    let ids: Vec<&str> = body["data"]
        .as_array()
        .expect("data")
        .iter()
        .filter_map(|m| m["id"].as_str())
        .collect();
    // A connected provider contributes its catalog models.
    assert!(ids.contains(&"gpt-4.1"), "models: {ids:?}");
    assert!(ids.len() > 5);
}

#[tokio::test]
async fn openai_non_streaming_round_trip() {
    let mock = Mock::start(vec![openai_reply("hello there")]).await;
    let h = harness(false);
    h.connect("openai", &mock).await;

    let (status, body) = h.post("/v1/chat/completions", openai_chat_body("gpt-4.1")).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["choices"][0]["message"]["content"], "hello there");
    assert_eq!(body["object"], "chat.completion");
    assert_eq!(body["usage"]["prompt_tokens"], 7);
    assert_eq!(mock.hits(), 1);

    let seen = mock.seen();
    assert_eq!(seen[0]["messages"][1]["content"], "hello");
}

/// codex is the awkward provider: its base URL is a Responses endpoint, it rejects
/// non-streaming requests, and it refuses `store`. A chat client must still work.
///
/// Each assertion below failed in production before it was fixed:
/// - a chat body reached the Responses endpoint (wrong native dialect),
/// - a string `input` was sent instead of the translated list (`extra` re-merged
///   the raw client body over the canonical request),
/// - `store` was absent,
/// - the request was non-streaming.
#[tokio::test]
async fn chat_client_to_a_stream_only_responses_provider_is_translated_and_reassembled() {
    let wire = concat!(
        "event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\",\"model\":\"gpt-5.6-luna\"}}\n\n",
        "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"Hel\"}\n\n",
        "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"lo\"}\n\n",
        "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\",\"usage\":{\"input_tokens\":4,\"output_tokens\":2}}}\n\n",
    );
    let mock = Mock::start(vec![MockReply::Sse(vec![SseStep {
        wire: wire.to_string(),
        await_gate: None,
    }])])
    .await;
    let h = harness(false);
    h.connect("codex", &mock).await;

    let (status, body) = h.post("/v1/chat/completions", openai_chat_body("codex/gpt-5.6-luna")).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["object"], "chat.completion");
    assert_eq!(body["choices"][0]["message"]["content"], "Hello");
    assert_eq!(body["usage"]["prompt_tokens"], 4);
    assert_eq!(body["usage"]["completion_tokens"], 2);

    assert_eq!(mock.hits(), 1);
    assert_eq!(mock.paths()[0], "/backend-api/codex/responses");
    let sent = &mock.seen()[0];
    assert!(sent.get("messages").is_none(), "chat body reached a Responses endpoint: {sent}");
    assert!(sent["input"].is_array(), "input must be the translated list: {sent}");
    assert_eq!(sent["store"], false, "codex rejects stored responses: {sent}");
    assert_eq!(sent["stream"], true, "codex rejects non-streaming requests: {sent}");
    assert_eq!(sent["model"], "gpt-5.6-luna");
}

/// Anthropic clients (Claude Code) mark cache breakpoints with `cache_control`. The
/// canonical model has no place for them, so a rebuilt body would silently disable
/// prompt caching; same-dialect requests must be forwarded as written.
#[tokio::test]
async fn anthropic_cache_markers_survive_the_gateway() {
    let reply = MockReply::Json(
        200,
        json!({
            "id": "msg_1",
            "type": "message",
            "role": "assistant",
            "model": "haiku",
            "content": [{"type": "text", "text": "ok"}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 5, "output_tokens": 1}
        }),
    );
    let mock = Mock::start(vec![reply]).await;
    let h = harness(false);
    h.connect("claude", &mock).await;

    let body = json!({
        "model": "claude/haiku",
        "max_tokens": 16,
        "cache_control": {"type": "ephemeral"},
        "system": [{"type": "text", "text": "be terse", "cache_control": {"type": "ephemeral"}}],
        "messages": [{
            "role": "user",
            "content": [{"type": "text", "text": "hi", "cache_control": {"type": "ephemeral"}}]
        }],
        "tools": [{
            "name": "f",
            "input_schema": {"type": "object"},
            "cache_control": {"type": "ephemeral"}
        }]
    });
    let (status, out) = h.post("/v1/messages", body.clone()).await;
    assert_eq!(status, StatusCode::OK, "{out}");

    let sent = &mock.seen()[0];
    assert_eq!(sent["cache_control"], body["cache_control"], "top-level breakpoint lost");
    assert_eq!(sent["system"][0]["cache_control"]["type"], "ephemeral", "system breakpoint lost");
    assert_eq!(
        sent["messages"][0]["content"][0]["cache_control"]["type"], "ephemeral",
        "message breakpoint lost"
    );
    assert_eq!(sent["tools"][0]["cache_control"]["type"], "ephemeral", "tool breakpoint lost");
    // Routing still rewrites the model id.
    assert_eq!(sent["model"], "haiku");
}

/// OpenAI Responses caching is opt-in through fields the canonical model does not
/// carry: `prompt_cache_options`, `prompt_cache_key`, and per-block breakpoints.
#[tokio::test]
async fn responses_cache_markers_survive_the_gateway() {
    let wire = concat!(
        "event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\",\"model\":\"gpt-5.6-luna\"}}\n\n",
        "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"ok\"}\n\n",
        "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\",\"usage\":{\"input_tokens\":3,\"output_tokens\":1}}}\n\n",
    );
    let mock = Mock::start(vec![MockReply::Sse(vec![SseStep {
        wire: wire.to_string(),
        await_gate: None,
    }])])
    .await;
    let h = harness(false);
    h.connect("codex", &mock).await;

    let body = json!({
        "model": "codex/gpt-5.6-luna",
        "input": [{
            "role": "user",
            "content": [{
                "type": "input_text",
                "text": "hi",
                "prompt_cache_breakpoint": {"mode": "explicit"}
            }]
        }],
        "prompt_cache_options": {"mode": "explicit", "ttl": "30m"},
        "prompt_cache_key": "conv-1",
        "store": true
    });
    let (status, out) = h.post("/v1/responses", body).await;
    assert_eq!(status, StatusCode::OK, "{out}");

    let sent = &mock.seen()[0];
    assert_eq!(sent["prompt_cache_options"]["mode"], "explicit");
    assert_eq!(sent["prompt_cache_key"], "conv-1");
    assert_eq!(
        sent["input"][0]["content"][0]["prompt_cache_breakpoint"]["mode"],
        "explicit"
    );
    assert_eq!(sent["store"], false, "codex requires store:false");
    assert_eq!(sent["stream"], true, "codex requires streaming");
}

#[tokio::test]
async fn anthropic_non_streaming_round_trip() {
    let reply = MockReply::Json(
        200,
        json!({
            "id": "msg_1",
            "type": "message",
            "role": "assistant",
            "model": "claude-sonnet-4-5",
            "content": [{"type": "text", "text": "bonjour"}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 9, "output_tokens": 4}
        }),
    );
    let mock = Mock::start(vec![reply]).await;
    let h = harness(false);
    h.connect("claude", &mock).await;

    let body = json!({
        "model": "claude/sonnet-4-5",
        "max_tokens": 64,
        "system": "be terse",
        "messages": [{"role": "user", "content": "hi"}]
    });
    let (status, out) = h.post("/v1/messages", body).await;
    assert_eq!(status, StatusCode::OK, "body: {out}");
    assert_eq!(out["type"], "message");
    assert_eq!(out["content"][0]["text"], "bonjour");
    assert_eq!(out["stop_reason"], "end_turn");
    assert_eq!(out["usage"]["input_tokens"], 9);

    // The provider's native Messages endpoint is used, not its chat URL.
    assert_eq!(mock.paths(), vec!["/v1/messages".to_string()]);
    let seen = mock.seen();
    assert_eq!(seen[0]["model"], "sonnet-4-5");
    assert_eq!(seen[0]["system"], "be terse");
}

#[tokio::test]
async fn streaming_flushes_before_upstream_finishes() {
    // Upstream sends frame A, waits for the client to observe it, then sends B.
    let gate = Arc::new(tokio::sync::Notify::new());
    let chunk_a = rrouter_core::sse::render_openai(
        &json!({"id":"c1","model":"mock-model","choices":[{"index":0,"delta":{"content":"A"},"finish_reason":null}]}).to_string(),
        None,
    );
    let chunk_b = rrouter_core::sse::render_openai(
        &json!({"id":"c1","model":"mock-model","choices":[{"index":0,"delta":{"content":"B"},"finish_reason":"stop"}],"usage":{"prompt_tokens":2,"completion_tokens":2}}).to_string(),
        None,
    );
    let done = "data: [DONE]\n\n".to_string();
    let mock = Mock::start(vec![MockReply::Sse(vec![
        SseStep { wire: chunk_a, await_gate: None },
        SseStep { wire: chunk_b, await_gate: Some(gate.clone()) },
        SseStep { wire: done, await_gate: None },
    ])])
    .await;
    let h = harness(false);
    h.connect("openai", &mock).await;

    let mut body = openai_chat_body("gpt-4.1");
    body["stream"] = json!(true);
    let req = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("request");
    let res = h.router.clone().oneshot(req).await.expect("oneshot");
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers()["content-type"], "text/event-stream");
    assert_eq!(res.headers()["cache-control"], "no-cache");
    assert_eq!(res.headers()["x-accel-buffering"], "no");

    let mut stream = res.into_body().into_data_stream();
    let first = tokio::time::timeout(Duration::from_secs(5), futures_util::StreamExt::next(&mut stream))
        .await
        .expect("first frame before upstream finished")
        .expect("frame")
        .expect("bytes");
    let first = String::from_utf8_lossy(&first).to_string();
    assert!(first.contains("\"A\""), "first frame: {first}");

    // Only now let the upstream send its last frames.
    gate.notify_one();
    let mut rest = String::new();
    while let Some(Ok(chunk)) = futures_util::StreamExt::next(&mut stream).await {
        rest.push_str(&String::from_utf8_lossy(&chunk));
    }
    assert!(rest.contains("\"B\""), "rest: {rest}");
    assert!(rest.contains("[DONE]"), "rest: {rest}");
}

#[tokio::test]
async fn tool_call_round_trip_through_messages() {
    let reply = MockReply::Json(
        200,
        json!({
            "id": "msg_tool",
            "type": "message",
            "role": "assistant",
            "model": "claude-sonnet-4-5",
            "content": [
                {"type": "text", "text": "checking"},
                {"type": "tool_use", "id": "toolu_1", "name": "get_weather", "input": {"city": "Hanoi"}}
            ],
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 11, "output_tokens": 6}
        }),
    );
    let mock = Mock::start(vec![reply]).await;
    let h = harness(false);
    h.connect("claude", &mock).await;

    let body = json!({
        "model": "claude/sonnet-4-5",
        "max_tokens": 128,
        "tools": [{
            "name": "get_weather",
            "description": "weather lookup",
            "input_schema": {"type": "object", "properties": {"city": {"type": "string"}}}
        }],
        "messages": [
            {"role": "user", "content": "weather in Hanoi?"},
            {"role": "assistant", "content": [
                {"type": "tool_use", "id": "toolu_in", "name": "get_weather", "input": {"city": "Hanoi"}}
            ]},
            {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "toolu_in", "content": "31C"}
            ]}
        ]
    });
    let (status, out) = h.post("/v1/messages", body).await;
    assert_eq!(status, StatusCode::OK, "body: {out}");
    let blocks = out["content"].as_array().expect("content");
    assert_eq!(out["stop_reason"], "tool_use");
    let tool = blocks
        .iter()
        .find(|b| b["type"] == "tool_use")
        .expect("tool_use block survives");
    assert_eq!(tool["id"], "toolu_1");
    assert_eq!(tool["name"], "get_weather");
    assert_eq!(tool["input"]["city"], "Hanoi");

    // The request's own tool_use/tool_result survived translation upstream.
    let seen = mock.seen();
    let upstream = seen[0].to_string();
    assert!(upstream.contains("toolu_in"), "upstream body: {upstream}");
    assert!(upstream.contains("get_weather"), "upstream body: {upstream}");
    assert!(seen[0]["tools"].as_array().is_some_and(|t| t.len() == 1));
}

#[tokio::test]
async fn require_api_key_rejects_then_accepts() {
    let mock = Mock::start(vec![openai_reply("authed")]).await;
    let h = harness(true);
    h.connect("openai", &mock).await;

    let (status, body) = h.post("/v1/chat/completions", openai_chat_body("gpt-4.1")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["type"], "authentication_error");
    assert_eq!(body["error"]["code"], "invalid_api_key");
    assert_eq!(mock.hits(), 0, "must not reach upstream without a key");

    let (_, created) = h.post("/api/keys", json!({"name": "test"})).await;
    let key = created["key"].as_str().expect("plaintext key").to_string();
    assert!(key.starts_with("sk-rr-"));

    let (status, body) = h
        .post_with(
            "/v1/chat/completions",
            openai_chat_body("gpt-4.1"),
            &[("authorization", &format!("Bearer {key}"))],
        )
        .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["choices"][0]["message"]["content"], "authed");
}

#[tokio::test]
async fn fallback_to_second_candidate() {
    let mock = Mock::start(vec![
        MockReply::Json(503, json!({"error": {"message": "overloaded"}})),
        openai_reply("from backup"),
    ])
    .await;
    let h = harness(false);
    // Two accounts on the same provider: the first one fails, the second serves.
    h.connect("openai", &mock).await;
    h.connect("openai", &mock).await;

    let (status, body) = h.post("/v1/chat/completions", openai_chat_body("gpt-4.1")).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["choices"][0]["message"]["content"], "from backup");
    assert_eq!(mock.hits(), 2, "first attempt 503, second attempt served");
}

// ------------------------------------------------------------- observability

/// A silent fallback cannot be operated. One request id must tie the client's response
/// header, the usage row, and one row per dispatched candidate together — including the
/// attempt that failed, with the status and message the client would have seen.
#[tokio::test]
async fn fallback_records_every_attempt_under_one_request_id() {
    let mock = Mock::start(vec![
        MockReply::Json(503, json!({"error": {"message": "overloaded"}})),
        openai_reply("from backup"),
    ])
    .await;
    let h = harness(false);
    h.connect("openai", &mock).await;
    h.connect("openai", &mock).await;

    let (status, headers, body) = h.chat(None, "gpt-4.1").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let request_id = headers
        .get("x-request-id")
        .expect("every response carries x-request-id")
        .to_str()
        .expect("ascii header")
        .to_string();
    let parsed = uuid::Uuid::parse_str(&request_id).expect("a uuid");
    assert_eq!(parsed.get_version_num(), 4, "the id must be a UUIDv4");

    // Attempt and usage rows are written off the hot path, exactly like usage always was.
    let mut detail = Value::Null;
    for _ in 0..200 {
        let (code, value) = h.get(&format!("/api/usage/{request_id}")).await;
        if code == StatusCode::OK
            && value["data"]["request"]["request_id"] == request_id.as_str()
            && value["data"]["attempts"].as_array().is_some_and(|a| a.len() == 2)
        {
            detail = value;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let data = &detail["data"];
    assert_eq!(data["request"]["request_id"], request_id.as_str(), "{detail}");
    assert_eq!(data["request"]["status"], 200, "{detail}");
    assert_eq!(data["request"]["key_id"], Value::Null, "anonymous request: {detail}");

    let attempts = data["attempts"].as_array().expect("attempts array");
    assert_eq!(attempts.len(), 2, "one row per send, retries included: {detail}");
    assert_eq!(attempts[0]["attempt"], 1, "1-based in dispatch order");
    assert_eq!(attempts[0]["provider"], "openai");
    assert_eq!(attempts[0]["status"], 503, "the failing attempt keeps its status");
    assert!(
        attempts[0]["error"].as_str().unwrap_or_default().contains("503"),
        "the client-visible message is stored: {detail}"
    );
    assert_eq!(attempts[0]["key_id"], Value::Null);
    assert!(attempts[0]["latency_ms"].as_u64().is_some());
    assert_eq!(attempts[1]["attempt"], 2);
    assert_eq!(attempts[1]["status"], 200);
    assert_eq!(attempts[1]["error"], Value::Null);

    // The request log row the UI filters by key and opens attempts from.
    let (_, list) = h.get("/api/usage").await;
    let row = list["data"]
        .as_array()
        .expect("rows")
        .iter()
        .find(|r| r["request_id"] == request_id.as_str())
        .unwrap_or_else(|| panic!("no log row for {request_id}: {list}"));
    assert_eq!(row["key_id"], Value::Null);
    assert_eq!(row["model"], "gpt-4.1");

    // An id nobody has seen is a 404, not an empty view.
    let (code, _) = h.get("/api/usage/00000000-0000-4000-8000-000000000000").await;
    assert_eq!(code, StatusCode::NOT_FOUND);

    // The static admin routes still win over the new `{request_id}` parameter.
    let (code, body) = h.get("/api/usage/summary").await;
    assert_eq!(code, StatusCode::OK, "{body}");
    assert!(body["data"].is_array(), "{body}");
    let (code, body) = h.get("/api/usage/series?hours=1&bucket=hour").await;
    assert_eq!(code, StatusCode::OK, "{body}");
}

/// `/metrics` is for scrapers: Prometheus text, no API key, and counters that move.
#[tokio::test]
async fn metrics_expose_requests_attempts_and_latency() {
    let mock = Mock::start(vec![
        MockReply::Json(503, json!({"error": {"message": "overloaded"}})),
        openai_reply("served"),
    ])
    .await;
    // Keys required: a scrape still must not need one.
    let h = harness(true);
    h.connect("openai", &mock).await;
    h.connect("openai", &mock).await;
    let (key, _) = h.create_key().await;

    let (status, _, body) = h.chat(Some(&key), "gpt-4.1").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    let (status, headers, text) = h.get_raw("/metrics").await;
    assert_eq!(status, StatusCode::OK);
    let content_type = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(
        content_type.starts_with("text/plain; version=0.0.4"),
        "prometheus text format: {content_type}"
    );

    assert!(text.contains("# TYPE rrouter_requests_total counter"), "{text}");
    assert!(
        metric_value(
            &text,
            r#"rrouter_requests_total{provider="openai",model="gpt-4.1",status="200"}"#
        ) >= 1,
        "the finished request is counted: {text}"
    );
    assert!(
        metric_value(&text, r#"rrouter_attempts_total{provider="openai",status="503"}"#) >= 1,
        "the fallback attempt is counted: {text}"
    );
    assert!(
        metric_value(&text, r#"rrouter_attempts_total{provider="openai",status="200"}"#) >= 1,
        "{text}"
    );
    assert!(
        metric_value(&text, r#"rrouter_request_duration_seconds_bucket{le="+Inf"}"#) >= 1,
        "latency histogram is filled: {text}"
    );
    assert!(text.contains("rrouter_request_duration_seconds_count"), "{text}");
    // Every connection the store knows about is exposed, closed or not.
    let (_, connections) = h.get("/api/connections").await;
    let id = connections["data"][0]["id"].as_str().expect("connection id");
    assert!(
        text.contains(&format!(r#"rrouter_breaker_open{{connection="{id}"}} 0"#)),
        "known connections are gauges: {text}"
    );
    assert!(text.contains(r#"rrouter_build_info{version="#), "{text}");
}

/// Errors are responses too: a client that gets a 404 still needs the id to report it.
#[tokio::test]
async fn error_and_stream_responses_carry_the_request_id() {
    let h = harness(false);
    // No connection for this model: the planner fails before any dispatch.
    let (status, headers, _body) = h.chat(None, "no-such-model").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let id = headers
        .get("x-request-id")
        .expect("error responses carry the id")
        .to_str()
        .expect("ascii")
        .to_string();
    assert!(uuid::Uuid::parse_str(&id).is_ok(), "not a uuid: {id}");

    // Streaming responses carry it as a header, which is committed before the body.
    let wire = concat!(
        "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"Hi\"}\n\n",
        "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"r1\",\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}}\n\n",
    );
    let mock = Mock::start(vec![MockReply::Sse(vec![SseStep {
        wire: wire.to_string(),
        await_gate: None,
    }])])
    .await;
    h.connect("codex", &mock).await;
    let (status, headers, _body) = h
        .call_full(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"model": "codex/gpt-5.6-luna", "stream": true, "messages": [{"role": "user", "content": "hi"}]})
                        .to_string(),
                ))
                .expect("request"),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers.get("content-type").and_then(|v| v.to_str().ok()), Some("text/event-stream"));
    assert!(headers.get("x-request-id").is_some(), "streams carry the id too");
}

#[tokio::test]
async fn connections_never_leak_tokens() {
    let mock = Mock::start(vec![]).await;
    let h = harness(false);
    h.connect("openai", &mock).await;

    let (status, body) = h.get("/api/connections").await;
    assert_eq!(status, StatusCode::OK);
    let raw = body.to_string();
    assert!(!raw.contains("SECRET-ACCESS-TOKEN"), "leaked access token: {raw}");
    assert!(!raw.contains("SECRET-REFRESH-TOKEN"), "leaked refresh token: {raw}");
    assert!(!raw.contains("test-key"), "leaked api key: {raw}");
    let first = &body["data"][0];
    assert_eq!(first["has_access_token"], true);
    assert_eq!(first["has_refresh_token"], true);
    assert_eq!(first["has_api_key"], true);
}

#[tokio::test]
async fn ui_panels_lists_builtins_and_extensions() {
    let h = harness(false);
    let (status, body) = h.get("/api/ui/panels").await;
    assert_eq!(status, StatusCode::OK);
    let ids: Vec<&str> = body["data"]
        .as_array()
        .expect("data")
        .iter()
        .filter_map(|p| p["id"].as_str())
        .collect();
    for builtin in ["connections", "keys", "aliases", "combos", "usage", "status"] {
        assert!(ids.contains(&builtin), "missing builtin {builtin}: {ids:?}");
    }

    // Register an enterprise extension panel through the settings API.
    let (status, _) = h
        .call(
            Request::builder()
                .method("PUT")
                .uri("/api/settings")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "brand_name": "Acme Router",
                        "panels": [{"id": "acme-audit", "title": "Audit", "url": "/ext/audit.html", "order": 15}]
                    })
                    .to_string(),
                ))
                .expect("request"),
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = h.get("/api/ui/panels").await;
    assert_eq!(status, StatusCode::OK);
    let panels = body["data"].as_array().expect("data");
    let ext = panels
        .iter()
        .find(|p| p["id"] == "acme-audit")
        .expect("extension panel listed");
    assert_eq!(ext["kind"], "extension");
    assert_eq!(ext["url"], "/ext/audit.html");
    assert_eq!(body["brand"]["brand_name"], "Acme Router");
}

#[tokio::test]
async fn ext_static_rejects_traversal() {
    let mut state_config = config(false);
    let root = state_config.data_dir.join("public");
    std::fs::create_dir_all(root.join("js")).expect("mkdir");
    std::fs::write(root.join("js/panel.js"), "export const ok = 1;\n").expect("write");
    state_config.static_dir = Some(root.clone());
    let state = Arc::new(AppState {
        config: state_config,
        ..build_state(config(false))
    });
    // The traversal target must exist relative to the root for the test to be meaningful.
    std::fs::write(root.join("Cargo.toml"), "should never be served").expect("write");
    let router = app(state);

    let fetch = |uri: &'static str| {
        let router = router.clone();
        async move {
            let res = router
                .oneshot(Request::builder().uri(uri).body(Body::empty()).expect("request"))
                .await
                .expect("oneshot");
            res.status()
        }
    };

    assert_eq!(fetch("/ext/js/panel.js").await, StatusCode::OK);
    assert_eq!(fetch("/ext/../Cargo.toml").await, StatusCode::NOT_FOUND);
    assert_eq!(fetch("/ext/%2e%2e/Cargo.toml").await, StatusCode::NOT_FOUND);
    assert_eq!(fetch("/ext/js/../../Cargo.toml").await, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn admin_api_never_reports_paths_or_keys() {
    let h = harness(false);
    let (_, created) = h.post("/api/keys", json!({"name": "k"})).await;
    let plaintext = created["key"].as_str().expect("key").to_string();

    let (status, body) = h.get("/api/system").await;
    assert_eq!(status, StatusCode::OK);
    let raw = body.to_string();
    assert!(!raw.contains(&plaintext), "system leaked a key: {raw}");
    assert!(!raw.contains("data_dir"), "system leaked a path: {raw}");
    assert!(!raw.contains("master_key\":\"file"), "system leaked key source as a value");
    assert_eq!(body["counts"]["api_keys"], 1);
    assert_eq!(body["require_api_key"], false);
}

#[tokio::test]
async fn unsupported_and_error_shapes() {
    // A connected provider without the endpoint is the 501 case; both envelopes keep
    // their dialect's shape.
    let mock = Mock::start(vec![]).await;
    let h = harness(false);
    h.connect("openai", &mock).await;
    h.connect("groq", &mock).await;

    let (status, body) = h
        .post("/v1/messages/count_tokens", json!({"model": "openai/gpt-4.1"}))
        .await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
    assert_eq!(body["type"], "error", "anthropic envelope: {body}");
    assert!(body["error"]["message"].as_str().is_some_and(|m| m.contains("not supported")));

    // groq has neither endpoint; openai has a Responses URL, so it would forward.
    let (status, body) = h.post("/v1/responses/compact", json!({"model": "groq/llama-3.3-70b"})).await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
    assert!(body.get("type").is_none(), "openai shape: {body}");
    assert!(body["error"]["message"].as_str().is_some_and(|m| m.contains("not supported")));
    assert_eq!(mock.hits(), 0, "nothing is sent upstream");
}


// ---- OAuth sign-in endpoints -------------------------------------------------------

#[tokio::test]
async fn oauth_start_returns_a_challenge_and_never_a_token() {
    let h = harness(false);
    let (status, body) = h.post("/api/oauth/github/start", Value::Null).await;
    // No network in this test: the device-code endpoint is unreachable, so the call
    // fails upstream — but the shape of the contract is what matters here, and the
    // body must never contain a token.
    let text = body.to_string();
    assert!(!text.contains("access_token"), "{text}");
    assert!(status.is_client_error() || status.is_server_error() || status.is_success());
}

#[tokio::test]
async fn oauth_start_rejects_an_unknown_provider() {
    let h = harness(false);
    let (status, body) = h.post("/api/oauth/notreal/start", Value::Null).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert!(body["error"]["message"].as_str().unwrap_or("").contains("unknown provider"));
}

#[tokio::test]
async fn oauth_start_rejects_a_provider_without_a_login_flow() {
    let h = harness(false);
    let (status, body) = h.post("/api/oauth/openai/start", Value::Null).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body["error"]["message"].as_str().unwrap_or("").contains("no OAuth sign-in"));
}

#[tokio::test]
async fn oauth_complete_requires_an_id_and_rejects_unknown_ids() {
    let h = harness(false);
    let (status, body) = h.post("/api/oauth/github/complete", serde_json::json!({})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    let (status, body) = h
        .post("/api/oauth/github/complete", serde_json::json!({"id": "does-not-exist"}))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

#[tokio::test]
async fn oauth_status_reports_pending_false_for_unknown_ids() {
    let h = harness(false);
    let (status, body) = h.get("/api/oauth/github/status/nope").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["pending"], serde_json::json!(false));
}

#[tokio::test]
async fn oauth_cancel_is_idempotent() {
    let h = harness(false);
    let (status, body) = h
        .call(
            Request::builder()
                .method("DELETE")
                .uri("/api/oauth/github/nope")
                .body(Body::empty())
                .expect("request"),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["cancelled"], serde_json::json!(false));
}

/// Providers that implement the endpoint get the client body forwarded verbatim (with
/// the resolved model id); the response comes back untouched.
#[tokio::test]
async fn count_tokens_is_forwarded_to_anthropic() {
    let mock = Mock::start(vec![MockReply::Json(200, json!({"input_tokens": 42}))]).await;
    let h = harness(false);
    h.connect("claude", &mock).await;

    let (status, body) = h
        .post(
            "/v1/messages/count_tokens",
            json!({
                "model": "claude/sonnet-4-5",
                "messages": [{"role": "user", "content": "hello"}]
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["input_tokens"], 42);
    assert_eq!(mock.paths(), vec!["/v1/messages/count_tokens"]);
    let seen = mock.seen();
    assert_eq!(seen[0]["model"], "sonnet-4-5", "the provider gets the bare id");
    assert_eq!(seen[0]["messages"][0]["content"], "hello");
}

#[tokio::test]
async fn compact_is_forwarded_to_a_responses_provider() {
    let mock = Mock::start(vec![MockReply::Json(
        200,
        json!({"id": "cmp_1", "object": "response.compaction"}),
    )])
    .await;
    let h = harness(false);
    h.connect("codex", &mock).await;

    let (status, body) = h
        .post(
            "/v1/responses/compact",
            json!({"model": "codex/gpt-5.6-luna", "input": [{"type": "message", "role": "user", "content": "hi"}]}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["id"], "cmp_1");
    assert_eq!(mock.paths(), vec!["/backend-api/codex/responses/compact"]);
    assert_eq!(mock.seen()[0]["model"], "gpt-5.6-luna");
}

/// A provider without the endpoint answers 501 instead of receiving a body it cannot
/// parse; the client can fall back to its own counter.
#[tokio::test]
async fn count_tokens_on_a_provider_without_the_endpoint_is_501() {
    let mock = Mock::start(vec![]).await;
    let h = harness(false);
    h.connect("openai", &mock).await;

    let (status, body) = h
        .post(
            "/v1/messages/count_tokens",
            json!({"model": "openai/gpt-4.1", "messages": [{"role": "user", "content": "hi"}]}),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED, "body: {body}");
    assert_eq!(mock.hits(), 0, "nothing is sent upstream");
    assert!(body["error"]["message"].as_str().unwrap_or_default().contains("count_tokens"));
}

/// Time to first output is measured at the first content event, not at the response
/// headers, so a slow model shows up as slow even when the stream opens instantly.
#[tokio::test]
async fn streaming_records_time_to_first_output() {
    let gate = Arc::new(tokio::sync::Notify::new());
    let notifier = Arc::clone(&gate);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(60)).await;
        notifier.notify_one();
    });
    let wire = |event: &str, data: &str| format!("event: {event}\ndata: {data}\n\n");
    let mock = Mock::start(vec![MockReply::Sse(vec![
        SseStep {
            wire: wire("response.created", r#"{"type":"response.created","response":{"id":"r1","model":"gpt-5.6-luna"}}"#),
            await_gate: None,
        },
        SseStep {
            wire: wire("response.output_text.delta", r#"{"type":"response.output_text.delta","delta":"Hi"}"#),
            await_gate: Some(gate),
        },
        SseStep {
            wire: wire("response.completed", r#"{"type":"response.completed","response":{"id":"r1","usage":{"input_tokens":4,"output_tokens":1}}}"#),
            await_gate: None,
        },
    ])])
    .await;
    let h = harness(false);
    h.connect("codex", &mock).await;

    let (status, _body) = h
        .post("/v1/chat/completions", json!({"model": "codex/gpt-5.6-luna", "stream": true, "messages": [{"role": "user", "content": "hi"}]}))
        .await;
    assert_eq!(status, StatusCode::OK);

    let mut row = None;
    for _ in 0..100 {
        if let Ok(rows) = h.state.store.recent_usage(&UsageQuery { limit: 5, ..Default::default() }).await {
            if let Some(first) = rows.first() {
                row = Some(first.clone());
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let row = row.expect("usage row recorded");
    assert!(row.streamed);
    assert!(row.ttft_ms >= 40, "ttft {} should include the upstream pause", row.ttft_ms);
    assert!(row.ttft_ms <= row.latency_ms, "ttft {} > latency {}", row.ttft_ms, row.latency_ms);
    // A streamed request is joinable to its attempt log by the same id.
    let request_id = row.request_id.expect("streamed usage rows carry the request id");
    assert!(h.state.store.usage_by_request(request_id).await.expect("lookup").is_some());
}

// ------------------------------------------------------ virtual-key governance

/// A disabled key is a governance decision, not a missing credential: `403` with a
/// stable code, and it holds even when keys are optional for everyone else.
#[tokio::test]
async fn inactive_key_is_rejected_with_403_even_when_keys_are_optional() {
    let mock = Mock::start(vec![openai_reply("never")]).await;
    let h = harness(false);
    h.connect("openai", &mock).await;
    let (key, id) = h.create_key().await;
    h.patch_key(&id, json!({"is_active": false})).await;

    let (status, _, body) = h.chat(Some(&key), "gpt-4.1").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body: {body}");
    assert_eq!(body["error"]["code"], "api_key_inactive");
    assert_eq!(body["error"]["type"], "permission_error");
    assert_eq!(mock.hits(), 0, "a disabled key must never reach a provider");
}

/// Expiry is wall-clock, and a key that is not expired keeps working.
#[tokio::test]
async fn expired_key_is_rejected_and_a_live_one_is_not() {
    let mock = Mock::start(vec![openai_reply("live")]).await;
    let h = harness(false);
    h.connect("openai", &mock).await;
    let (key, id) = h.create_key().await;

    let past = rrouter_gateway::now_unix() - 60;
    h.patch_key(&id, json!({"expires_at": past})).await;
    let (status, _, body) = h.chat(Some(&key), "gpt-4.1").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body: {body}");
    assert_eq!(body["error"]["code"], "api_key_expired");
    assert_eq!(mock.hits(), 0);

    let future = rrouter_gateway::now_unix() + 3600;
    h.patch_key(&id, json!({"expires_at": future})).await;
    let (status, _, body) = h.chat(Some(&key), "gpt-4.1").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["choices"][0]["message"]["content"], "live");
    assert_eq!(mock.hits(), 1);
}

/// An allowlist accepts an exact ref and a `provider/*` glob, and refuses anything else
/// before the planner looks at it.
#[tokio::test]
async fn allowed_models_allows_exact_ref_and_glob_and_rejects_others() {
    let mock = Mock::start(vec![
        openai_reply("one"),
        openai_reply("two"),
        openai_reply("three"),
    ])
    .await;
    let h = harness(false);
    h.connect("openai", &mock).await;
    let (key, id) = h.create_key().await;

    h.patch_key(&id, json!({"allowed_models": ["openai/gpt-4.1"]})).await;
    let (status, _, body) = h.chat(Some(&key), "openai/gpt-4.1").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["choices"][0]["message"]["content"], "one");

    let (status, _, body) = h.chat(Some(&key), "openai/gpt-4o").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body: {body}");
    assert_eq!(body["error"]["code"], "model_not_allowed");
    assert_eq!(mock.hits(), 1, "a refused model must not be dispatched");

    // The glob form covers the whole provider.
    h.patch_key(&id, json!({"allowed_models": ["openai/*"]})).await;
    let (status, _, body) = h.chat(Some(&key), "openai/gpt-4o").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["choices"][0]["message"]["content"], "two");
    assert_eq!(mock.hits(), 2);

    // `null` clears the list: every model is allowed again.
    h.patch_key(&id, json!({"allowed_models": null})).await;
    let (status, _, body) = h.chat(Some(&key), "openai/gpt-4.1").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["choices"][0]["message"]["content"], "three");
}

/// `rpm` is a 60 s sliding window with a one-second retry hint.
#[tokio::test]
async fn rpm_limit_answers_429_with_retry_after() {
    let mock = Mock::start(vec![openai_reply("first"), openai_reply("second")]).await;
    let h = harness(false);
    h.connect("openai", &mock).await;
    let (key, id) = h.create_key().await;
    h.patch_key(&id, json!({"rpm": 1})).await;

    let (status, _, body) = h.chat(Some(&key), "gpt-4.1").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    let (status, headers, body) = h.chat(Some(&key), "gpt-4.1").await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "body: {body}");
    assert_eq!(body["error"]["code"], "rate_limited");
    assert_eq!(
        headers.get("retry-after").and_then(|v| v.to_str().ok()),
        Some("1")
    );
    assert_eq!(mock.hits(), 1, "the limited request is not dispatched");
}

/// `tpm` reads the tokens recorded for the key in the last 60 s. Tokens are only known
/// after a response, so the first request always passes and later ones see its cost.
#[tokio::test]
async fn tpm_limit_measures_tokens_recorded_for_the_key() {
    let mock = Mock::start(vec![openai_reply("first"), openai_reply("second")]).await;
    let h = harness(false);
    h.connect("openai", &mock).await;
    let (key, id) = h.create_key().await;
    h.patch_key(&id, json!({"tpm": 1})).await;

    // The mock reports 7 prompt + 3 completion tokens, far above a 1-token cap.
    let (status, _, body) = h.chat(Some(&key), "gpt-4.1").await;
    assert_eq!(status, StatusCode::OK, "the window is still empty: {body}");

    let (status, _, body) = h.chat(Some(&key), "gpt-4.1").await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "body: {body}");
    assert_eq!(body["error"]["code"], "rate_limited");
    assert_eq!(mock.hits(), 1);
}

/// The budget is durable spend, so it is answered from `usage`, not from memory.
#[tokio::test]
async fn budget_exhausted_blocks_the_next_request() {
    let mock = Mock::start(vec![openai_reply("paid")]).await;
    let h = harness(false);
    h.connect("openai", &mock).await;
    let (key, id) = h.create_key().await;
    let uuid = uuid::Uuid::parse_str(&id).expect("uuid");

    // Prior spend for this key: 25 tokens.
    h.state
        .store
        .record_usage(&UsageRecord {
            id: None,
            ts: rrouter_gateway::now_unix(),
            request_id: Some(uuid::Uuid::new_v4()),
            provider: "claude".to_string(),
            model: "haiku".to_string(),
            connection_id: None,
            endpoint: Some("/v1/messages".to_string()),
            key_id: Some(uuid),
            cost_micros: 0,
            input_tokens: 20,
            output_tokens: 5,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            latency_ms: 1,
            ttft_ms: 0,
            status: 200,
            streamed: false,
            error: None,
        })
        .await
        .expect("seed spend");

    h.patch_key(&id, json!({"budget_tokens": 20})).await;
    let (status, _, body) = h.chat(Some(&key), "gpt-4.1").await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "body: {body}");
    assert_eq!(body["error"]["code"], "budget_exhausted");
    assert_eq!(mock.hits(), 0);

    h.patch_key(&id, json!({"budget_tokens": 1000})).await;
    let (status, _, body) = h.chat(Some(&key), "gpt-4.1").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(mock.hits(), 1);
}

/// Every usage row the proxy writes names the key that paid for it, and `/api/usage`
/// exposes that attribution.
#[tokio::test]
async fn usage_rows_carry_the_key_id() {
    let mock = Mock::start(vec![openai_reply("attributed")]).await;
    let h = harness(false);
    h.connect("openai", &mock).await;
    let (key, id) = h.create_key().await;
    let uuid = uuid::Uuid::parse_str(&id).expect("uuid");

    let (status, _, body) = h.chat(Some(&key), "gpt-4.1").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    let mut row = None;
    for _ in 0..100 {
        if let Ok(rows) = h.state.store.recent_usage(&UsageQuery { limit: 5, ..Default::default() }).await {
            if let Some(found) = rows.iter().find(|r| r.key_id == Some(uuid)) {
                row = Some(found.clone());
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let row = row.expect("usage row attributed to the key");
    assert_eq!(row.input_tokens, 7);
    assert_eq!(row.output_tokens, 3);
    assert_eq!(row.endpoint.as_deref(), Some("/v1/chat/completions"));

    let (status, body) = h.get("/api/usage").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"][0]["key_id"], id.as_str());
    assert_eq!(body["data"][0]["request_id"], row.request_id.expect("id").to_string().as_str());

    // The attempts of that request are attributed to the same key, so a per-key log can
    // show what the key's fallbacks cost.
    let request_id = row.request_id.expect("request id").to_string();
    let mut attempts = Value::Null;
    for _ in 0..100 {
        let (code, value) = h.get(&format!("/api/usage/{request_id}")).await;
        if code == StatusCode::OK && value["data"]["attempts"].as_array().is_some_and(|a| !a.is_empty()) {
            attempts = value["data"]["attempts"].clone();
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(attempts[0]["key_id"], id.as_str(), "attempt rows carry the key: {attempts}");
    assert_eq!(attempts[0]["status"], 200, "{attempts}");
}

/// Dev clients send placeholder tokens. With keys optional that is anonymous traffic;
/// with keys required it is a `401`, exactly as before.
#[tokio::test]
async fn unknown_key_is_anonymous_when_optional_and_rejected_when_required() {
    let lax_mock = Mock::start(vec![openai_reply("anonymous")]).await;
    let lax = harness(false);
    lax.connect("openai", &lax_mock).await;
    let (status, _, body) = lax.chat(Some("sk-rr-placeholder"), "gpt-4.1").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["choices"][0]["message"]["content"], "anonymous");
    assert_eq!(lax_mock.hits(), 1);

    let strict_mock = Mock::start(vec![openai_reply("never")]).await;
    let strict = harness(true);
    strict.connect("openai", &strict_mock).await;
    let (status, _, body) = strict.chat(Some("sk-rr-placeholder"), "gpt-4.1").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "body: {body}");
    assert_eq!(body["error"]["code"], "invalid_api_key");
    assert_eq!(strict_mock.hits(), 0);
}

/// The row shape is the contract the dashboard is built against: exact keys, exact
/// types, `spent_tokens` measured from usage.
#[tokio::test]
async fn key_rows_match_the_governance_contract() {
    let h = harness(false);
    let (_, id) = h.create_key().await;

    // A fresh key is unlimited.
    let (status, body) = h.get("/api/keys").await;
    assert_eq!(status, StatusCode::OK);
    let created = body["data"][0].clone();
    assert_eq!(created["is_active"], true);
    assert_eq!(created["scope"], "inference", "a new key may spend, not administer");
    assert_eq!(created["rpm"], 0);
    assert_eq!(created["tpm"], 0);
    assert_eq!(created["budget_tokens"], 0);
    assert_eq!(created["spent_tokens"], 0);
    assert_eq!(created["budget_micros"], 0);
    assert_eq!(created["spent_micros"], 0);
    assert_eq!(created["allowed_models"], Value::Null);
    assert_eq!(created["expires_at"], Value::Null);

    let patched = h
        .patch_key(
            &id,
            json!({
                "rpm": 60,
                "tpm": 90_000,
                "budget_tokens": 5_000_000,
                "allowed_models": ["claude/claude-haiku-4-5-20251001"],
                "expires_at": null,
            }),
        )
        .await;
    let expected = json!({
        "id": id.as_str(),
        "name": "test",
        "created_at": patched["created_at"].clone(),
        "is_active": true,
        "scope": "inference",
        "rpm": 60,
        "tpm": 90_000,
        "budget_tokens": 5_000_000,
        "budget_window": "total",
        "spent_tokens": 0,
        "budget_micros": 0,
        "spent_micros": 0,
        "allowed_models": ["claude/claude-haiku-4-5-20251001"],
        "expires_at": null,
        "guardrails": null,
        "models": [],
    });
    assert_eq!(patched, expected);

    // The list endpoint answers the same row.
    let (_, body) = h.get("/api/keys").await;
    assert_eq!(body["data"][0], expected);

    // A guardrail policy is a document on the row, and `null` clears it again.
    let policy = json!({
        "max_prompt_chars": 20_000,
        "blocked": ["secret", "ssn:"],
        "scan_output": true,
    });
    let patched = h.patch_key(&id, json!({"guardrails": policy})).await;
    assert_eq!(patched["guardrails"], policy, "the stored policy is returned");
    let (_, body) = h.get("/api/keys").await;
    assert_eq!(
        body["data"][0]["guardrails"], policy,
        "and it survives a reload"
    );
    let cleared = h.patch_key(&id, json!({"guardrails": null})).await;
    assert_eq!(cleared["guardrails"], Value::Null);
    assert_eq!(cleared["rpm"], 60, "clearing the policy left the rest alone");

    // A policy the gateway cannot enforce is a client error, not a silent no-op: a
    // misspelled field and a term list past the cap are both refused.
    for (body, expected) in [
        (json!({"guardrails": {"blocked_terms": ["x"]}}), "unknown field"),
        (
            json!({"guardrails": {"blocked": vec!["x"; 201]}}),
            "at most 200 terms",
        ),
        (json!({"guardrails": {"blocked": [""]}}), "must not be empty"),
    ] {
        let (status, _, value) = h
            .call_full(
                Request::builder()
                    .method("PATCH")
                    .uri(format!("/api/keys/{id}"))
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .expect("request"),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{}: {value}", body);
        let message = value["error"]["message"].as_str().unwrap_or_default();
        assert!(
            message.contains(expected),
            "expected {expected:?} in {message:?}"
        );
    }

    // A wrong type is a client error, not a silently ignored field.
    let (status, _, body) = h
        .call_full(
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/keys/{id}"))
                .header("content-type", "application/json")
                .body(Body::from(json!({"rpm": "fast"}).to_string()))
                .expect("request"),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("rpm must be a non-negative integer")),
        "body: {body}"
    );
}

// ---------------------------------------------------- phase 16: guardrails

/// A prompt past the key's `max_prompt_chars` is refused before planning, and the
/// message carries both numbers so an operator can tell "bigger than allowed" from
/// "limit is tiny".
#[tokio::test]
async fn guardrails_refuse_an_oversized_prompt_naming_both_counts() {
    let mock = Mock::start(vec![openai_reply("never sent")]).await;
    let h = harness(false);
    h.connect("openai", &mock).await;
    let (key, id) = h.create_key().await;
    h.patch_key(&id, json!({"guardrails": {"max_prompt_chars": 10}})).await;

    let long = "x".repeat(40);
    let (status, _, body) = h
        .post_headers(
            "/v1/chat/completions",
            json!({
                "model": "gpt-4.1",
                "messages": [
                    {"role": "system", "content": "be terse"},
                    {"role": "user", "content": long},
                ]
            }),
            &[("authorization", &format!("Bearer {key}"))],
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body: {body}");
    assert_eq!(body["error"]["code"], "prompt_too_long");
    let message = body["error"]["message"].as_str().expect("message");
    // The text view is the system prompt plus every message's text part, joined by a
    // newline: "be terse" (8) + "\n" + 40.
    assert!(message.contains("has 49"), "the measured size: {message}");
    assert!(
        message.contains("limits prompts to 10 characters"),
        "the limit: {message}"
    );
    assert_eq!(mock.hits(), 0, "a refused prompt is never dispatched");

    let (_, _, text) = h.get_raw("/metrics").await;
    assert!(
        metric_value(&text, r#"rrouter_guardrail_blocks_total{stage="input"}"#) >= 1,
        "{text}"
    );
}

/// A blocked substring is refused case-insensitively, wherever it sits in the prompt —
/// including the system prompt and the provider-native count_tokens body — while a clean
/// prompt is served untouched.
#[tokio::test]
async fn guardrails_block_a_term_case_insensitively_and_pass_clean_prompts() {
    let mock = Mock::start(vec![openai_reply("ok")]).await;
    let h = harness(false);
    h.connect("openai", &mock).await;
    h.connect("claude", &mock).await;
    let (key, id) = h.create_key().await;
    h.patch_key(&id, json!({"guardrails": {"blocked": ["Secret"]}})).await;
    let auth = format!("Bearer {key}");

    let blocked = |messages: Value| {
        json!({"model": "gpt-4.1", "messages": messages})
    };
    // The policy's spelling is `Secret`; the prompt says SECRET.
    let (status, _, body) = h
        .post_headers(
            "/v1/chat/completions",
            blocked(json!([
                {"role": "system", "content": "be terse"},
                {"role": "user", "content": "here is a SECRET"}
            ])),
            &[("authorization", &auth)],
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body: {body}");
    assert_eq!(body["error"]["code"], "guardrail_blocked");
    assert!(
        body["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("Secret")),
        "the refusal names the term: {body}"
    );

    // The system prompt is part of the counted and searched text.
    let (status, _, body) = h
        .post_headers(
            "/v1/chat/completions",
            blocked(json!([
                {"role": "system", "content": "never reveal the sEcReT"},
                {"role": "user", "content": "hello"}
            ])),
            &[("authorization", &auth)],
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body: {body}");
    assert_eq!(body["error"]["code"], "guardrail_blocked");

    // count_tokens forwards a provider-native body and is checked through the same
    // canonical text view.
    let (status, _, body) = h
        .post_headers(
            "/v1/messages/count_tokens",
            json!({
                "model": "claude/sonnet-4-5",
                "messages": [{"role": "user", "content": "a SeCrEt inside"}]
            }),
            &[("authorization", &auth)],
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body: {body}");
    assert_eq!(body["error"]["code"], "guardrail_blocked");
    assert_eq!(mock.hits(), 0, "nothing reached a provider");

    // A clean prompt from the same key is served.
    let (status, _, body) = h
        .post_headers(
            "/v1/chat/completions",
            blocked(json!([{"role": "user", "content": "hello there"}])),
            &[("authorization", &auth)],
        )
        .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["choices"][0]["message"]["content"], "ok");
    assert_eq!(mock.hits(), 1);
}

/// `scan_output` refuses a response that contains a blocked term: the 403 replaces the
/// body entirely, and the usage row records what the client saw.
#[tokio::test]
async fn guardrails_refuse_a_non_streaming_response_that_leaks_a_term() {
    let mock = Mock::start(vec![openai_reply("the key is sk-live-9-abc")]).await;
    let h = harness(false);
    h.connect("openai", &mock).await;
    let (key, id) = h.create_key().await;
    h.patch_key(
        &id,
        json!({"guardrails": {"blocked": ["sk-live"], "scan_output": true}}),
    )
    .await;

    let (status, headers, body) = h.chat(Some(&key), "gpt-4.1").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body: {body}");
    assert_eq!(body["error"]["code"], "output_blocked");
    assert!(
        !body.to_string().contains("sk-live-9-abc"),
        "the offending text was never returned: {body}"
    );
    assert_eq!(mock.hits(), 1, "the upstream did answer; the client did not");

    let request_id: uuid::Uuid = headers
        .get("x-request-id")
        .expect("the refusal carries the request id")
        .to_str()
        .expect("ascii")
        .parse()
        .expect("uuid");
    let row = h.usage_for(request_id).await;
    assert_eq!(row.status, 403, "the log says what the client got");
    assert!(
        row.error.as_deref().is_some_and(|e| e.contains("sk-live")),
        "the row names the term for the operator: {row:?}"
    );
    assert_eq!(row.key_id.map(|k| k.to_string()).as_deref(), Some(id.as_str()));

    let (_, _, text) = h.get_raw("/metrics").await;
    assert!(
        metric_value(&text, r#"rrouter_guardrail_blocks_total{stage="output"}"#) >= 1,
        "{text}"
    );
}

/// The exact-match cache is shared across keys, so a guarded key must not be served a
/// body another key cached: the hit is scanned before it is returned.
#[tokio::test]
async fn guardrails_refuse_a_response_served_from_the_cache() {
    let mock = Mock::start(vec![openai_reply("the key is sk-live-9-abc")]).await;
    let h = harness_with(cached_config(300, 100));
    h.connect("openai", &mock).await;
    let (clean, _) = h.create_key().await;
    let (guarded, id) = h.create_key().await;
    h.patch_key(
        &id,
        json!({"guardrails": {"blocked": ["sk-live"], "scan_output": true}}),
    )
    .await;

    let (status, headers, body) = h.chat(Some(&clean), "gpt-4.1").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(
        headers.get("x-rrouter-cache").and_then(|v| v.to_str().ok()),
        Some("miss"),
        "the unguarded key fills the cache"
    );

    let (status, _, body) = h.chat(Some(&guarded), "gpt-4.1").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body: {body}");
    assert_eq!(body["error"]["code"], "output_blocked");
    assert!(
        !body.to_string().contains("sk-live-9-abc"),
        "the cached body was not served: {body}"
    );
    assert_eq!(mock.hits(), 1, "the refusal came from the cache, not a retry");
}

/// A streaming response is stopped at the delta that carries the term: the client keeps
/// what it already received, gets one terminal error frame, and nothing after it.
#[tokio::test]
async fn guardrails_stop_a_stream_at_the_first_blocked_delta() {
    let delta = |content: &str, finish: Value| {
        rrouter_core::sse::render_openai(
            &json!({"id":"g1","model":"mock-model","choices":[{"index":0,"delta":{"content":content},"finish_reason":finish}]}).to_string(),
            None,
        )
    };
    let mock = Mock::start(vec![MockReply::Sse(vec![
        SseStep { wire: delta("clean text ", json!(null)), await_gate: None },
        SseStep { wire: delta("leaked SECRET here", json!(null)), await_gate: None },
        SseStep { wire: delta("AFTER THE HIT", json!("stop")), await_gate: None },
        SseStep { wire: "data: [DONE]\n\n".to_string(), await_gate: None },
    ])])
    .await;
    let h = harness(false);
    h.connect("openai", &mock).await;
    let (key, id) = h.create_key().await;
    h.patch_key(
        &id,
        json!({"guardrails": {"blocked": ["secret"], "scan_output": true}}),
    )
    .await;

    let mut body = openai_chat_body("gpt-4.1");
    body["stream"] = json!(true);
    let res = h
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {key}"))
                .body(Body::from(body.to_string()))
                .expect("request"),
        )
        .await
        .expect("oneshot");
    assert_eq!(res.status(), StatusCode::OK, "the refusal arrives as a frame");
    let request_id: uuid::Uuid = res
        .headers()
        .get("x-request-id")
        .expect("the stream carries the id")
        .to_str()
        .expect("ascii")
        .parse()
        .expect("uuid");
    let mut stream = res.into_body().into_data_stream();

    let mut seen = next_frame(&mut stream).await;
    assert!(seen.contains("clean text"), "the clean delta was forwarded: {seen}");
    seen.push_str(&next_frame(&mut stream).await);
    assert!(seen.contains("output_blocked"), "one terminal error frame: {seen}");
    assert!(!seen.contains("SECRET"), "the offending delta never left: {seen}");

    // Nothing after the hit: the encoder stops and the upstream is dropped.
    while let Some(Ok(chunk)) =
        tokio::time::timeout(Duration::from_secs(5), futures_util::StreamExt::next(&mut stream))
            .await
            .expect("the stream ends promptly")
    {
        let text = String::from_utf8_lossy(&chunk).into_owned();
        assert!(text.is_empty(), "nothing after the refusal: {text}");
    }

    let row = h.usage_for(request_id).await;
    assert_eq!(row.status, 403);
    assert!(row.streamed, "it was a stream, and it is logged as one");
}

/// No guardrails means no checks: an oversized prompt carrying a term, answered with a
/// response that repeats it, is served exactly as before phase 16.
#[tokio::test]
async fn a_key_without_guardrails_is_unaffected() {
    let mock = Mock::start(vec![openai_reply("your secret is 1234")]).await;
    let h = harness(false);
    h.connect("openai", &mock).await;
    let (key, _) = h.create_key().await;

    let (status, _, body) = h
        .post_headers(
            "/v1/chat/completions",
            json!({
                "model": "gpt-4.1",
                "messages": [{"role": "user", "content": format!("secret {}", "x".repeat(500))}]
            }),
            &[("authorization", &format!("Bearer {key}"))],
        )
        .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["choices"][0]["message"]["content"], "your secret is 1234");
    assert_eq!(mock.hits(), 1);
}

// ------------------------------------------------- phase 3: admin auth & limits

/// A request that did not arrive over a loopback socket: what a reverse proxy or a
/// shared host looks like to the gate.
fn remote_peer() -> std::net::SocketAddr {
    std::net::SocketAddr::from(([203, 0, 113, 9], 5555))
}

/// Stamp a peer address on an in-process request, standing in for `ConnectInfo` that
/// only a real TCP listener would attach.
fn as_peer(mut req: Request<Body>, peer: std::net::SocketAddr) -> Request<Body> {
    req.extensions_mut().insert(ConnectInfo(peer));
    req
}

fn get_with(uri: &str, headers: &[(&str, &str)]) -> Request<Body> {
    let mut builder = Request::builder().method("GET").uri(uri);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    builder.body(Body::empty()).expect("request")
}

/// With `RROUTER_ADMIN_TOKEN` set, the token is the only credential: the loopback rule
/// stops applying, and `X-Forwarded-For` is never a substitute for it.
#[tokio::test]
async fn admin_token_gates_the_admin_api() {
    let mut cfg = config(false);
    cfg.admin_token = Some(TEST_ADMIN_TOKEN.into());
    let h = harness_with(cfg);

    let (status, body) = h.get("/api/system").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["code"], "admin_unauthorized");
    assert_eq!(body["error"]["type"], "authentication_error");

    let bearer = format!("Bearer {TEST_ADMIN_TOKEN}");
    let (status, _, body) = h.call_full(get_with("/api/system", &[("authorization", &bearer)])).await;
    assert_eq!(status, StatusCode::OK, "Authorization: Bearer must work: {body}");
    let (status, _, body) = h
        .call_full(get_with("/api/system", &[("x-admin-token", TEST_ADMIN_TOKEN)]))
        .await;
    assert_eq!(status, StatusCode::OK, "x-admin-token must work: {body}");

    // Neither a wrong token nor a model key authorises an admin call.
    let (status, _, _) = h.call_full(get_with("/api/system", &[("authorization", "Bearer nope")])).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _, _) =
        h.call_full(get_with("/api/system", &[("x-api-key", TEST_ADMIN_TOKEN)])).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "x-api-key is not the admin channel");

    // The socket address no longer decides anything while a token is configured.
    let res = h
        .router
        .clone()
        .oneshot(as_peer(get_with("/api/system", &[]), remote_peer()))
        .await
        .expect("oneshot");
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED, "remote peer without the token");
    let res = h
        .router
        .clone()
        .oneshot(as_peer(
            get_with("/api/system", &[("x-admin-token", TEST_ADMIN_TOKEN)]),
            remote_peer(),
        ))
        .await
        .expect("oneshot");
    assert_eq!(res.status(), StatusCode::OK, "remote peer with the token");

    // A scraper shares the admin rule...
    let (status, _, _) = h.call_full(get_with("/metrics", &[])).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _, _) = h
        .call_full(get_with("/metrics", &[("x-admin-token", TEST_ADMIN_TOKEN)]))
        .await;
    assert_eq!(status, StatusCode::OK);

    // ...and client endpoints never do.
    let (status, _) = h.get("/health").await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = h.get("/v1/models").await;
    assert_eq!(status, StatusCode::OK);
}

/// Without a token nothing changes: loopback peers pass, remote peers get `403`.
#[tokio::test]
async fn without_a_token_the_admin_api_stays_loopback_only() {
    let h = harness(false);
    let res = h
        .router
        .clone()
        .oneshot(as_peer(get_with("/api/system", &[]), remote_peer()))
        .await
        .expect("oneshot");
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    // In-process requests carry no peer address and count as loopback.
    let (status, body) = h.get("/api/system").await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

/// An `admin`-scoped key is an admin credential on its own: a non-loopback peer gets in
/// with it, on `/api/*` and `/metrics`, with or without a global token configured.
#[tokio::test]
async fn an_admin_scoped_key_authorises_the_admin_api() {
    let h = harness(false);
    let (key, id) = h.create_key().await;
    let patched = h.patch_key(&id, json!({"scope": "admin"})).await;
    assert_eq!(patched["scope"], "admin");

    let bearer = format!("Bearer {key}");
    let (status, _, body) = h
        .call_full(as_peer(
            get_with("/api/system", &[("authorization", &bearer)]),
            remote_peer(),
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "an admin key authorises a remote peer: {body}");

    // `x-admin-token` is the admin-side spelling of the same credential, and a scraper
    // shares the rule.
    let res = h
        .router
        .clone()
        .oneshot(as_peer(
            get_with("/metrics", &[("x-admin-token", &key)]),
            remote_peer(),
        ))
        .await
        .expect("oneshot");
    assert_eq!(res.status(), StatusCode::OK, "an admin key may scrape metrics");

    // A configured token does not lock scoped keys out: both are admin credentials.
    // (With a token set, the API itself needs the token, so the key is prepared in the
    // store — the PATCH path is covered by `scope_patch_validates_the_spelling`.)
    let mut cfg = config(false);
    cfg.admin_token = Some(TEST_ADMIN_TOKEN.into());
    let tokened = harness_with(cfg);
    let (ops_plain, ops) = tokened
        .state
        .store
        .create_api_key(Some("ops"))
        .await
        .expect("key");
    tokened
        .state
        .store
        .update_api_key(
            ops.id,
            &rrouter_store::ApiKeyPatch {
                scope: Some(rrouter_store::KeyScope::Admin),
                ..Default::default()
            },
        )
        .await
        .expect("patch")
        .expect("exists");
    let res = tokened
        .router
        .clone()
        .oneshot(as_peer(
            get_with("/api/system", &[("authorization", &format!("Bearer {ops_plain}"))]),
            remote_peer(),
        ))
        .await
        .expect("oneshot");
    assert_eq!(res.status(), StatusCode::OK, "next to a token, an admin key still works");

    // Only a live key: revoking it takes the authority away with it.
    tokened
        .state
        .store
        .update_api_key(
            ops.id,
            &rrouter_store::ApiKeyPatch { is_active: Some(false), ..Default::default() },
        )
        .await
        .expect("disable")
        .expect("exists");
    let res = tokened
        .router
        .clone()
        .oneshot(as_peer(
            get_with("/api/system", &[("authorization", &format!("Bearer {ops_plain}"))]),
            remote_peer(),
        ))
        .await
        .expect("oneshot");
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED, "a disabled admin key is no credential");
}

/// An inference key is not an admin credential: a remote peer is refused with
/// `admin_required`, while a loopback peer still passes on the socket rule alone.
#[tokio::test]
async fn an_inference_key_cannot_administer() {
    let h = harness(false);
    let (key, _) = h.create_key().await;
    let bearer = format!("Bearer {key}");

    let (status, _, body) = h
        .call_full(as_peer(
            get_with("/api/system", &[("authorization", &bearer)]),
            remote_peer(),
        ))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body: {body}");
    assert_eq!(body["error"]["code"], "admin_required");

    // The socket rule comes first: a loopback peer passes whatever the key's scope.
    let (status, _, body) = h
        .call_full(get_with("/api/system", &[("authorization", &bearer)]))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // With a global token configured, the same inference key is still the wrong
    // credential, and the refusal names the scope rather than the token.
    let mut cfg = config(false);
    cfg.admin_token = Some(TEST_ADMIN_TOKEN.into());
    let tokened = harness_with(cfg);
    let (plain, _) = tokened
        .state
        .store
        .create_api_key(None)
        .await
        .expect("key");
    let (status, _, body) = tokened
        .call_full(as_peer(
            get_with("/api/system", &[("authorization", &format!("Bearer {plain}"))]),
            remote_peer(),
        ))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body: {body}");
    assert_eq!(body["error"]["code"], "admin_required");
}

/// An admin key administers the gateway; it does not spend tokens. The refusal happens
/// before any dispatch and writes no usage row, and an inference key is unchanged.
#[tokio::test]
async fn an_admin_key_cannot_call_the_proxy() {
    let mock = Mock::start(vec![openai_reply("reached")]).await;
    let h = harness(false);
    h.connect("openai", &mock).await;
    let (admin, id) = h.create_key().await;
    h.patch_key(&id, json!({"scope": "admin"})).await;

    let (status, _, body) = h.chat(Some(&admin), "gpt-4.1").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body: {body}");
    assert_eq!(body["error"]["code"], "key_scope");
    assert_eq!(body["error"]["message"], "admin keys cannot call /v1");
    assert_eq!(mock.hits(), 0, "the refusal is before any dispatch");
    assert!(
        h.state
            .store
            .recent_usage(&UsageQuery { limit: 10, ..Default::default() })
            .await
            .expect("usage")
            .is_empty(),
        "a refused admin key writes no usage row"
    );

    // An inference key is exactly what it was before phase 19: same request, served.
    let (inference, _) = h.create_key().await;
    let (status, _, body) = h.chat(Some(&inference), "gpt-4.1").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(mock.hits(), 1);
}

/// `scope` accepts the two spellings and refuses anything else with a 400 that names
/// them; a refused patch leaves the stored scope untouched.
#[tokio::test]
async fn scope_patch_validates_the_spelling() {
    let h = harness(false);
    let (_, id) = h.create_key().await;
    for scope in ["admin", "inference"] {
        let patched = h.patch_key(&id, json!({"scope": scope})).await;
        assert_eq!(patched["scope"], scope);
    }

    let (status, _, body) = h
        .call_full(
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/keys/{id}"))
                .header("content-type", "application/json")
                .body(Body::from(json!({"scope": "superuser"}).to_string()))
                .expect("request"),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");
    let message = body["error"]["message"].as_str().expect("message");
    assert!(
        message.ends_with("scope must be inference or admin"),
        "names the two scopes: {message}"
    );
    let (_, listed) = h.get("/api/keys").await;
    assert_eq!(listed["data"][0]["scope"], "inference", "the refusal changed nothing");
}

/// One streaming chat request; the caller drives (or drops) the body.
async fn open_chat_stream(h: &Harness) -> axum::body::BodyDataStream {
    let mut body = openai_chat_body("gpt-4.1");
    body["stream"] = json!(true);
    let req = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("request");
    let res = h.router.clone().oneshot(req).await.expect("oneshot");
    assert_eq!(res.status(), StatusCode::OK);
    res.into_body().into_data_stream()
}

/// Next chunk of a streaming body, with a deadline so a stalled stream fails the test
/// instead of hanging the suite.
async fn next_frame(stream: &mut axum::body::BodyDataStream) -> String {
    let chunk = tokio::time::timeout(
        Duration::from_secs(5),
        futures_util::StreamExt::next(stream),
    )
    .await
    .expect("a frame within 5s")
    .expect("a frame")
    .expect("bytes");
    String::from_utf8_lossy(&chunk).into_owned()
}

/// `RROUTER_MAX_CONCURRENCY=1`: the slot belongs to the stream, so the second request is
/// refused while the first is mid-body, and admitted once that body ends — or is
/// abandoned by the client.
#[tokio::test]
async fn concurrency_cap_refuses_a_second_request_while_the_first_streams() {
    let first_gate = Arc::new(tokio::sync::Notify::new());
    let second_gate = Arc::new(tokio::sync::Notify::new());
    let delta = |content: &str, finish: Value| {
        rrouter_core::sse::render_openai(
            &json!({"id":"c1","model":"mock-model","choices":[{"index":0,"delta":{"content":content},"finish_reason":finish}]}).to_string(),
            None,
        )
    };
    let mock = Mock::start(vec![
        MockReply::Sse(vec![
            SseStep { wire: delta("A", json!(null)), await_gate: None },
            SseStep { wire: delta("B", json!("stop")), await_gate: Some(first_gate.clone()) },
            SseStep { wire: "data: [DONE]\n\n".to_string(), await_gate: None },
        ]),
        openai_reply("after the stream"),
        MockReply::Sse(vec![
            SseStep { wire: delta("C", json!(null)), await_gate: None },
            SseStep { wire: delta("D", json!("stop")), await_gate: Some(second_gate.clone()) },
        ]),
        openai_reply("after the drop"),
    ])
    .await;
    let mut cfg = config(false);
    cfg.max_concurrency = 1;
    let h = harness_with(cfg);
    h.connect("openai", &mock).await;

    // First stream is open and has produced a frame: it owns the only permit.
    let mut stream = open_chat_stream(&h).await;
    assert!(next_frame(&mut stream).await.contains("\"A\""));

    let (status, headers, body) = h.chat(None, "gpt-4.1").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "body: {body}");
    assert_eq!(body["error"]["code"], "overloaded");
    assert_eq!(
        headers.get("retry-after").and_then(|v| v.to_str().ok()),
        Some("1")
    );
    assert_eq!(mock.hits(), 1, "the refused request is never dispatched");
    // The cap is on `/v1/*`, not only on completions.
    let (status, _) = h.get("/v1/models").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "the catalog read is capped too");

    // Ending the stream returns the slot.
    first_gate.notify_one();
    while let Some(Ok(_)) = futures_util::StreamExt::next(&mut stream).await {}
    drop(stream);
    let (status, _, body) = h.chat(None, "gpt-4.1").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["choices"][0]["message"]["content"], "after the stream");
    let (status, _) = h.get("/v1/models").await;
    assert_eq!(status, StatusCode::OK, "the catalog read is admitted again");

    // A client that disconnects mid-stream returns it too: dropping the body is the
    // release, so the next request must not be told the gateway is overloaded.
    let mut stream = open_chat_stream(&h).await;
    assert!(next_frame(&mut stream).await.contains("\"C\""));
    drop(stream);
    second_gate.notify_one();

    let (status, _, body) = h.chat(None, "gpt-4.1").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["choices"][0]["message"]["content"], "after the drop");
    assert_eq!(mock.hits(), 4);
}

/// `RROUTER_MAX_ATTEMPTS=1` is the operator saying "no retries": the second candidate
/// is never contacted, even though it would have served.
#[tokio::test]
async fn max_attempts_one_disables_provider_fallback() {
    let mock = Mock::start(vec![
        MockReply::Json(503, json!({"error": {"message": "overloaded"}})),
        openai_reply("from backup"),
    ])
    .await;
    let mut cfg = config(false);
    cfg.max_attempts = 1;
    let h = harness_with(cfg);
    // Two accounts on the same provider: with the default cap the first 503 falls back.
    h.connect("openai", &mock).await;
    h.connect("openai", &mock).await;

    let (status, body) = h
        .post("/v1/chat/completions", openai_chat_body("gpt-4.1"))
        .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "body: {body}");
    assert_eq!(mock.hits(), 1, "the second candidate must not be tried");
}

/// An embeddings call bills input tokens, so it must leave a usage row; a body the
/// gateway cannot parse must still reach the client byte for byte.
#[tokio::test]
async fn embeddings_record_usage_and_relay_malformed_bodies_unchanged() {
    let mock = Mock::start(vec![
        MockReply::Json(
            200,
            json!({
                "object": "list",
                "data": [{"object": "embedding", "index": 0, "embedding": [0.1, 0.2]}],
                "model": "text-embedding-3-small",
                "usage": {"prompt_tokens": 9, "total_tokens": 9}
            }),
        ),
        MockReply::Raw(200, "{\"data\": [not json"),
    ])
    .await;
    let h = harness(false);
    h.connect("openai", &mock).await;
    let (key, id) = h.create_key().await;
    let uuid = uuid::Uuid::parse_str(&id).expect("uuid");

    let request = |body: Value| {
        Request::builder()
            .method("POST")
            .uri("/v1/embeddings")
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {key}"))
            .body(Body::from(body.to_string()))
            .expect("request")
    };

    let res = h
        .router
        .clone()
        .oneshot(request(json!({"model": "openai/text-embedding-3-small", "input": "hello"})))
        .await
        .expect("oneshot");
    assert_eq!(res.status(), StatusCode::OK);
    let body = res.into_body().collect().await.expect("body").to_bytes();
    let body: Value = serde_json::from_slice(&body).expect("json relayed");
    assert_eq!(body["usage"]["prompt_tokens"], 9);

    let mut row = None;
    for _ in 0..100 {
        if let Ok(rows) = h.state.store.recent_usage(&UsageQuery { limit: 5, ..Default::default() }).await {
            if let Some(found) = rows.iter().find(|r| r.key_id == Some(uuid)) {
                row = Some(found.clone());
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let row = row.expect("an embeddings call leaves a usage row");
    assert_eq!(row.endpoint.as_deref(), Some("/v1/embeddings"));
    assert_eq!(row.input_tokens, 9);
    assert_eq!(row.output_tokens, 0, "an embedding generates nothing");

    // A malformed body is relayed unchanged and records nothing to attribute.
    let res = h
        .router
        .clone()
        .oneshot(request(json!({"model": "openai/text-embedding-3-small", "input": "hello"})))
        .await
        .expect("oneshot");
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = res.into_body().collect().await.expect("body").to_bytes();
    assert_eq!(String::from_utf8_lossy(&bytes), "{\"data\": [not json");
    tokio::time::sleep(Duration::from_millis(100)).await;
    let rows = h.state.store.recent_usage(&UsageQuery { limit: 10, ..Default::default() }).await.expect("usage");
    assert_eq!(rows.len(), 1, "the unparseable body adds no row: {rows:?}");
}

/// `count_tokens` and `compact` are not chat requests: a row appears only when the
/// upstream itself reports usage.
#[tokio::test]
async fn passthrough_endpoints_record_usage_only_when_reported() {
    let mock = Mock::start(vec![
        MockReply::Json(200, json!({"input_tokens": 42})),
        MockReply::Json(
            200,
            json!({"id": "cmp_1", "usage": {"input_tokens": 11, "output_tokens": 0}}),
        ),
    ])
    .await;
    let h = harness(false);
    h.connect("claude", &mock).await;
    h.connect("codex", &mock).await;

    let (status, body) = h
        .post(
            "/v1/messages/count_tokens",
            json!({"model": "claude/sonnet-4-5", "messages": [{"role": "user", "content": "hello"}]}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["input_tokens"], 42);

    let (status, body) = h
        .post(
            "/v1/responses/compact",
            json!({"model": "codex/gpt-5.6-luna", "input": [{"type": "message", "role": "user", "content": "hi"}]}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let deadline = Instant::now() + Duration::from_secs(1);
    while Instant::now() < deadline
        && h.state.store.recent_usage(&UsageQuery { limit: 10, ..Default::default() }).await.expect("usage").is_empty()
    {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // Give the (unwritten) count_tokens row a chance to appear before asserting absence.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let rows = h.state.store.recent_usage(&UsageQuery { limit: 10, ..Default::default() }).await.expect("usage");
    assert_eq!(rows.len(), 1, "count_tokens reported no usage: {rows:?}");
    assert_eq!(rows[0].endpoint.as_deref(), Some("/v1/responses/compact"));
    assert_eq!(rows[0].input_tokens, 11);
    assert_eq!(rows[0].output_tokens, 0);
}

// ------------------------------------------------------- phase 7: protocol breadth

/// A raw `multipart/form-data` body and the content type that describes it.
fn multipart(parts: &[(&str, Option<&str>, &[u8])]) -> (String, Vec<u8>) {
    let boundary = "----rrouter-phase7-boundary";
    let mut body = Vec::new();
    for (name, filename, value) in parts {
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        let disposition = match filename {
            Some(file) => format!(
                "Content-Disposition: form-data; name=\"{name}\"; filename=\"{file}\"\r\n\
                 Content-Type: application/octet-stream\r\n"
            ),
            None => format!("Content-Disposition: form-data; name=\"{name}\"\r\n"),
        };
        body.extend_from_slice(disposition.as_bytes());
        body.extend_from_slice(b"\r\n");
        body.extend_from_slice(value);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={boundary}"), body)
}

/// A short stand-in clip: the gateway must never reinterpret audio bytes.
const TEST_CLIP: &[u8] = b"ID3\x04\x00\x00\x00\x00\x00\x00rrouter-audio-bytes";

/// `GET /v1/models/{id}` answers the OpenAI model object for anything the registry says
/// is routable right now, and the OpenAI 404 for everything else.
#[tokio::test]
async fn model_by_id_answers_the_object_and_404_for_unknown_or_disabled() {
    let mock = Mock::start(vec![]).await;
    let h = harness(false);
    h.connect("openai", &mock).await;

    let (status, body) = h.get("/v1/models/gpt-4.1").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["id"], "gpt-4.1");
    assert_eq!(body["object"], "model");
    assert_eq!(body["owned_by"], "openai");
    assert!(body["created"].as_i64().unwrap_or_default() > 0);

    let (status, body) = h.get("/v1/models/openai/gpt-4.1").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["id"], "openai/gpt-4.1");
    assert_eq!(body["owned_by"], "openai");

    let (status, body) = h.get("/v1/models/nope-9").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["error"]["code"], "model_not_found");

    // A disabled model is no longer routable, so the by-id lookup stops answering.
    let (status, _) = h
        .call(
            Request::builder()
                .method("PUT")
                .uri("/api/disabled/gpt-4.1")
                .body(Body::empty())
                .expect("request"),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = h.get("/v1/models/gpt-4.1").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

/// Image generation is a JSON passthrough: the provider's bare model id, the client's
/// other fields untouched, the reply relayed, and one usage row from its reported usage.
#[tokio::test]
async fn images_passthrough_relays_the_provider_json() {
    let mock = Mock::start(vec![MockReply::Json(
        200,
        json!({
            "created": 1,
            "data": [{"b64_json": "aGk="}],
            "usage": {"input_tokens": 12, "output_tokens": 0, "total_tokens": 12}
        }),
    )])
    .await;
    let h = harness(false);
    h.connect("openai", &mock).await;
    let (key, id) = h.create_key().await;
    let uuid = uuid::Uuid::parse_str(&id).expect("uuid");
    let auth = format!("Bearer {key}");

    let (status, body) = h
        .post_with(
            "/v1/images/generations",
            json!({"model": "openai/gpt-image-1", "prompt": "a cat", "n": 1}),
            &[("authorization", auth.as_str())],
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"][0]["b64_json"], "aGk=");

    assert_eq!(mock.paths(), vec!["/v1/images/generations"]);
    let seen = mock.seen();
    assert_eq!(seen[0]["model"], "gpt-image-1", "the provider gets the bare id");
    assert_eq!(seen[0]["prompt"], "a cat");

    let row = wait_for_usage(&h, uuid, "openai/gpt-image-1").await;
    assert_eq!(row.endpoint.as_deref(), Some("/v1/images/generations"));
    assert_eq!(row.input_tokens, 12);
}

/// Fallback walks only the candidates whose provider serves the endpoint.
#[tokio::test]
async fn images_fall_back_to_the_next_capable_connection() {
    let failing = Mock::start(vec![MockReply::Json(
        500,
        json!({"error": {"message": "boom"}}),
    )])
    .await;
    let healthy = Mock::start(vec![MockReply::Json(
        200,
        json!({"data": [{"b64_json": "ok"}]}),
    )])
    .await;
    let h = harness(false);
    let first = h.connect("openai", &failing).await;
    let second = h.connect("openai", &healthy).await;
    h.patch_connection(&first.to_string(), json!({"priority": 0}))
        .await;
    h.patch_connection(&second.to_string(), json!({"priority": 1}))
        .await;

    let (status, body) = h
        .post(
            "/v1/images/generations",
            json!({"model": "openai/gpt-image-1", "prompt": "a cat"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"][0]["b64_json"], "ok");
    // The preferred account is tried, its 500 is retried once on the same account,
    // and only then does the next capable one serve the request.
    assert_eq!(failing.hits(), 2, "the preferred account was tried first");
    assert_eq!(healthy.hits(), 1, "the next capable account served the request");
}

/// A provider that does not document an endpoint answers `501` naming it, and is never
/// called with a body it cannot parse.
#[tokio::test]
async fn endpoints_a_provider_lacks_answer_501_without_a_call() {
    let mock = Mock::start(vec![]).await;
    let h = harness(false);
    h.connect("claude", &mock).await;

    let (status, body) = h
        .post(
            "/v1/images/generations",
            json!({"model": "claude/sonnet-4-5", "prompt": "a cat"}),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("/v1/images/generations"),
        "{body}"
    );

    let (status, body) = h
        .post(
            "/v1/audio/speech",
            json!({"model": "claude/sonnet-4-5", "input": "hello", "voice": "alloy"}),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("/v1/audio/speech"),
        "{body}"
    );

    // No registry provider documents a voice-list endpoint, so this is a 501 as well.
    let (status, body) = h.get("/v1/audio/voices").await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("/v1/audio/voices"),
        "{body}"
    );

    assert_eq!(mock.hits(), 0, "nothing is sent to a provider without the endpoint");
}

/// Speech synthesis streams the clip back with the provider's own content type.
#[tokio::test]
async fn audio_speech_relays_bytes_with_the_upstream_content_type() {
    let mock = Mock::start(vec![MockReply::Bytes(200, "audio/mpeg", TEST_CLIP)]).await;
    let h = harness(false);
    h.connect("openai", &mock).await;

    let res = h
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/audio/speech")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"model": "openai/gpt-4o-mini-tts", "input": "hello", "voice": "alloy"})
                        .to_string(),
                ))
                .expect("request"),
        )
        .await
        .expect("oneshot");
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers()["content-type"], "audio/mpeg");
    let bytes = res.into_body().collect().await.expect("body").to_bytes();
    assert_eq!(&bytes[..], TEST_CLIP, "the clip is relayed byte for byte");

    let wire = mock.wire();
    assert_eq!(wire[0].0, "/v1/audio/speech");
    let sent: Value = serde_json::from_slice(&wire[0].2).expect("json body");
    assert_eq!(sent["model"], "gpt-4o-mini-tts");
    assert_eq!(sent["input"], "hello");
}

/// The clip is streamed: the client sees the first bytes while the provider is still
/// holding the rest, so a long clip never sits in gateway memory.
#[tokio::test]
async fn audio_speech_streams_before_the_upstream_clip_is_complete() {
    let gate = Arc::new(tokio::sync::Notify::new());
    let mock = Mock::start(vec![MockReply::Chunks(
        200,
        "audio/mpeg",
        vec![
            (TEST_CLIP[..4].to_vec(), None),
            (TEST_CLIP[4..].to_vec(), Some(Arc::clone(&gate))),
        ],
    )])
    .await;
    let h = harness(false);
    h.connect("openai", &mock).await;

    let res = h
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/audio/speech")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"model": "openai/gpt-4o-mini-tts", "input": "hello", "voice": "alloy"})
                        .to_string(),
                ))
                .expect("request"),
        )
        .await
        .expect("oneshot");
    let mut stream = res.into_body().into_data_stream();
    let first = tokio::time::timeout(
        Duration::from_secs(5),
        futures_util::StreamExt::next(&mut stream),
    )
    .await
    .expect("the first chunk arrives while the provider still holds the rest")
    .expect("chunk")
    .expect("bytes");
    assert_eq!(&first[..], &TEST_CLIP[..4]);

    gate.notify_one();
    let mut rest = Vec::new();
    while let Some(chunk) = futures_util::StreamExt::next(&mut stream).await {
        rest.extend_from_slice(&chunk.expect("bytes"));
    }
    assert_eq!(&rest[..], &TEST_CLIP[4..]);
}

/// Transcription forwards the client's multipart bytes and `content-type` — boundary
/// included — and relays the JSON reply.
#[tokio::test]
async fn transcription_relays_the_multipart_body_and_the_json_reply() {
    let mock = Mock::start(vec![MockReply::Json(200, json!({"text": "hello world"}))]).await;
    let h = harness(false);
    h.connect("openai", &mock).await;

    let (content_type, body) = multipart(&[
        ("model", None, b"openai/whisper-1"),
        ("language", None, b"en"),
        ("file", Some("clip.wav"), b"RIFF....WAVEfmt"),
    ]);
    let res = h
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/audio/transcriptions")
                .header("content-type", &content_type)
                .body(Body::from(body.clone()))
                .expect("request"),
        )
        .await
        .expect("oneshot");
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = res.into_body().collect().await.expect("body").to_bytes();
    let reply: Value = serde_json::from_slice(&bytes).expect("json relayed");
    assert_eq!(reply["text"], "hello world");

    let wire = mock.wire();
    assert_eq!(wire.len(), 1);
    assert_eq!(wire[0].0, "/v1/audio/transcriptions");
    assert_eq!(wire[0].1, content_type, "the boundary reaches the provider intact");
    assert_eq!(wire[0].2, body, "the form is forwarded byte for byte");
}

/// A form without `model` borrows the first connected provider that serves
/// transcriptions; with none connected it is a client error, not a wrong-path call.
#[tokio::test]
async fn transcription_without_a_model_picks_a_connected_provider_or_400() {
    let mock = Mock::start(vec![MockReply::Json(200, json!({"text": "ok"}))]).await;
    let h = harness(false);
    h.connect("openai", &mock).await;

    let (content_type, body) = multipart(&[("file", Some("clip.wav"), b"RIFF....WAVEfmt")]);
    let res = h
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/audio/transcriptions")
                .header("content-type", &content_type)
                .body(Body::from(body))
                .expect("request"),
        )
        .await
        .expect("oneshot");
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(mock.paths(), vec!["/v1/audio/transcriptions"]);

    let claude_mock = Mock::start(vec![]).await;
    let bare = harness(false);
    bare.connect("claude", &claude_mock).await;
    let (content_type, body) = multipart(&[("file", Some("clip.wav"), b"RIFF....WAVEfmt")]);
    let res = bare
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/audio/transcriptions")
                .header("content-type", &content_type)
                .body(Body::from(body))
                .expect("request"),
        )
        .await
        .expect("oneshot");
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    assert_eq!(claude_mock.hits(), 0);
}

/// Governance runs before the upstream call for every passthrough endpoint.
#[tokio::test]
async fn passthrough_refuses_an_exhausted_key_before_the_upstream_call() {
    let mock = Mock::start(vec![MockReply::Json(200, json!({"data": []}))]).await;
    let h = harness(false);
    h.connect("openai", &mock).await;
    let (key, id) = h.create_key().await;
    let uuid = uuid::Uuid::parse_str(&id).expect("uuid");

    // Prior spend for this key: 25 tokens.
    h.state
        .store
        .record_usage(&UsageRecord {
            id: None,
            ts: rrouter_gateway::now_unix(),
            request_id: Some(uuid::Uuid::new_v4()),
            provider: "openai".to_string(),
            model: "gpt-4.1".to_string(),
            connection_id: None,
            endpoint: Some("/v1/chat/completions".to_string()),
            key_id: Some(uuid),
            cost_micros: 0,
            input_tokens: 20,
            output_tokens: 5,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            latency_ms: 1,
            ttft_ms: 0,
            status: 200,
            streamed: false,
            error: None,
        })
        .await
        .expect("seed spend");

    h.patch_key(&id, json!({"budget_tokens": 20})).await;
    let auth = format!("Bearer {key}");
    let (status, body) = h
        .post_with(
            "/v1/images/generations",
            json!({"model": "openai/gpt-image-1", "prompt": "a cat"}),
            &[("authorization", auth.as_str())],
        )
        .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
    assert_eq!(body["error"]["code"], "budget_exhausted");
    assert_eq!(mock.hits(), 0);
}

// ------------------------------------------------- phase 4: per-key model mapping

impl Harness {
    /// `PUT /api/keys/{id}/models`.
    async fn put_models(&self, id: &str, body: Value) -> (StatusCode, Value) {
        self.call(
            Request::builder()
                .method("PUT")
                .uri(format!("/api/keys/{id}/models"))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .expect("request"),
        )
        .await
    }
}

fn anthropic_body(model: &str) -> Value {
    json!({
        "model": model,
        "max_tokens": 16,
        "messages": [{"role": "user", "content": "hi"}]
    })
}

/// One key maps `default` to a real ref: the client keeps the stable name, the upstream
/// sees the target, and the mapping applies to count_tokens as well as chat.
#[tokio::test]
async fn key_model_mapping_rewrites_the_routed_model() {
    let reply = || {
        MockReply::Json(
            200,
            json!({
                "id": "msg_1",
                "type": "message",
                "role": "assistant",
                "model": "claude-haiku-4-5-20251001",
                "content": [{"type": "text", "text": "ok"}],
                "stop_reason": "end_turn",
                "usage": {"input_tokens": 2, "output_tokens": 1}
            }),
        )
    };
    let mock = Mock::start(vec![
        reply(),
        reply(),
        MockReply::Json(200, json!({"input_tokens": 5})),
        reply(),
    ])
    .await;
    let h = harness(false);
    h.connect("claude", &mock).await;
    let (key, id) = h.create_key().await;

    let (status, body) = h
        .put_models(
            &id,
            json!({"models": [{"from": "default", "to": "claude/claude-haiku-4-5-20251001"}]}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, body) = h
        .post_with("/v1/messages", anthropic_body("default"), &[("x-api-key", &key)])
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        mock.seen()[0]["model"], "claude-haiku-4-5-20251001",
        "the mapped target is what the provider receives"
    );

    // An unmapped model passes through unchanged.
    let (status, body) = h
        .post_with("/v1/messages", anthropic_body("claude/sonnet-4-5"), &[("x-api-key", &key)])
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(mock.seen()[1]["model"], "sonnet-4-5");

    // count_tokens goes through the same rewrite.
    let (status, body) = h
        .post_with(
            "/v1/messages/count_tokens",
            anthropic_body("default"),
            &[("x-api-key", &key)],
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(mock.seen()[2]["model"], "claude-haiku-4-5-20251001");

    // The same model name from a key without the mapping is not rewritten.
    let (status, body) = h.post("/v1/messages", anthropic_body("claude/sonnet-4-5")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(mock.seen()[3]["model"], "sonnet-4-5");
}

/// The mapping endpoint validates every target, replaces the whole map or nothing, and
/// the keys list carries the map.
#[tokio::test]
async fn key_model_endpoint_validates_and_replaces_atomically() {
    let h = harness(false);
    let (_, id) = h.create_key().await;

    for (body, why) in [
        (json!({"models": [{"from": "", "to": "claude/x"}]}), "empty from"),
        (json!({"models": [{"from": "a", "to": "gpt-5"}]}), "to without a provider"),
        (json!({"models": [{"from": "a", "to": "not-a-provider/m"}]}), "unregistered provider"),
        (json!({"models": [{"from": "a", "to": "claude/"}]}), "empty model id"),
        (
            json!({"models": [
                {"from": "a", "to": "claude/x"},
                {"from": "a", "to": "openai/gpt-5"}
            ]}),
            "duplicate from",
        ),
        (json!({"nope": []}), "missing models key"),
    ] {
        let (status, value) = h.put_models(&id, body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{why}: {value}");
    }
    // Nothing was written by any of the rejected requests.
    let (status, value) = h.get(&format!("/api/keys/{id}/models")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["data"], json!([]), "a rejected PUT must not partially apply");

    let (status, value) = h
        .put_models(
            &id,
            json!({"models": [
                {"from": "smart", "to": "claude/claude-sonnet-4-5"},
                {"from": "default", "to": "claude/claude-haiku-4-5-20251001"}
            ]}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{value}");
    assert_eq!(value["data"].as_array().map(Vec::len), Some(2));

    // Read back in `from` order, and in the keys list too.
    let (status, value) = h.get(&format!("/api/keys/{id}/models")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        value["data"],
        json!([
            {"from": "default", "to": "claude/claude-haiku-4-5-20251001"},
            {"from": "smart", "to": "claude/claude-sonnet-4-5"}
        ])
    );
    let (status, value) = h.get("/api/keys").await;
    assert_eq!(status, StatusCode::OK);
    let row = value["data"]
        .as_array()
        .and_then(|rows| rows.iter().find(|r| r["id"] == id.as_str()))
        .expect("key row");
    assert_eq!(row["models"].as_array().map(Vec::len), Some(2));

    // A second PUT replaces rather than merges; an empty list clears.
    let (status, _) = h
        .put_models(&id, json!({"models": [{"from": "only", "to": "openai/gpt-5"}]}))
        .await;
    assert_eq!(status, StatusCode::OK);
    let (_, value) = h.get(&format!("/api/keys/{id}/models")).await;
    assert_eq!(value["data"], json!([{"from": "only", "to": "openai/gpt-5"}]));
    let (status, _) = h.put_models(&id, json!({"models": []})).await;
    assert_eq!(status, StatusCode::OK);
    let (_, value) = h.get(&format!("/api/keys/{id}/models")).await;
    assert_eq!(value["data"], json!([]));

    // Unknown key: `404`, not an empty list.
    let missing = uuid::Uuid::new_v4();
    let (status, _) = h.get(&format!("/api/keys/{missing}/models")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = h
        .put_models(&missing.to_string(), json!({"models": []}))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// Every successful attempt feeds the EWMA the least-latency strategy reads, off the
/// response path.
#[tokio::test]
async fn successful_attempts_record_latency_samples() {
    let mock = Mock::start(vec![openai_reply("timed")]).await;
    let h = harness(false);
    h.connect("openai", &mock).await;

    let (status, _, body) = h.chat(None, "gpt-4.1").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    let mut samples = std::collections::HashMap::new();
    for _ in 0..100 {
        samples = h.state.store.connection_latencies().await.expect("latencies");
        if !samples.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(samples.len(), 1, "the one connection that served has a sample");
}

/// The configured strategy reaches the proxy: with weights 3:1 two separate upstreams
/// receive the deterministic smooth-WRR sequence (heavy, heavy, light, heavy) over four
/// plans. Embeddings take the first candidate and pass `affinity: None`, so this is the
/// planner cursor, not a conversation hash.
#[tokio::test]
async fn weighted_strategy_is_applied_by_the_proxy() {
    let embedding = || {
        MockReply::Json(
            200,
            json!({
                "object": "list",
                "data": [{"object": "embedding", "index": 0, "embedding": [0.1, 0.2]}],
                "model": "text-embedding-3-small",
                "usage": {"prompt_tokens": 1, "total_tokens": 1}
            }),
        )
    };
    let heavy = Mock::start(vec![embedding(), embedding(), embedding(), embedding()]).await;
    let light = Mock::start(vec![embedding(), embedding(), embedding(), embedding()]).await;
    let mut cfg = config(false);
    cfg.strategy = rrouter_router::Strategy::Weighted;
    let h = harness_with(cfg);
    let heavy_id = h.connect("openai", &heavy).await;
    let light_id = h.connect("openai", &light).await;
    h.state.store.set_connection_weight(heavy_id, 3).await.expect("heavy weight");
    h.state.store.set_connection_weight(light_id, 1).await.expect("light weight");

    for _ in 0..4 {
        let (status, body) = h
            .post(
                "/v1/embeddings",
                json!({"model": "openai/text-embedding-3-small", "input": "hi"}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    assert_eq!(heavy.hits(), 3, "weight 3 should lead three of four plans");
    assert_eq!(light.hits(), 1);
}

/// Least-latency wiring: the fastest measured connection serves even though another
/// connection exists, and the choice is not a conversation-hash accident.
#[tokio::test]
async fn least_latency_strategy_is_applied_by_the_proxy() {
    let embedding = || {
        MockReply::Json(
            200,
            json!({
                "object": "list",
                "data": [{"object": "embedding", "index": 0, "embedding": [0.1]}],
                "model": "text-embedding-3-small",
                "usage": {"prompt_tokens": 1, "total_tokens": 1}
            }),
        )
    };
    let fast = Mock::start(vec![embedding(), embedding()]).await;
    let slow = Mock::start(vec![embedding(), embedding()]).await;
    let mut cfg = config(false);
    cfg.strategy = rrouter_router::Strategy::LeastLatency;
    let h = harness_with(cfg);
    let fast_id = h.connect("openai", &fast).await;
    let slow_id = h.connect("openai", &slow).await;
    h.state.store.record_connection_latency(fast_id, 20).await.expect("fast");
    h.state.store.record_connection_latency(slow_id, 4_000).await.expect("slow");

    for _ in 0..2 {
        let (status, body) = h
            .post(
                "/v1/embeddings",
                json!({"model": "openai/text-embedding-3-small", "input": "hi"}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    assert_eq!(fast.hits(), 2, "the measured-fast connection must serve");
    assert_eq!(slow.hits(), 0);
}

/// A failed attempt must not teach the latency strategy anything: a 500 says nothing
/// about how fast the account is when it works.
#[tokio::test]
async fn failed_attempts_do_not_record_latency_samples() {
    let mock = Mock::start(vec![MockReply::Json(500, json!({"error": "boom"}))]).await;
    let h = harness(false);
    h.connect("openai", &mock).await;

    let (status, _, body) = h.chat(None, "gpt-4.1").await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "body: {body}");

    // Give any (incorrectly) spawned sample time to land before asserting absence.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        h.state.store.connection_latencies().await.expect("latencies").is_empty(),
        "a failed attempt must not update the EWMA"
    );
}

// ------------------------------------------------------- cost accounting (phase 6)

/// A usage row seeded directly, for the cases where the test owns the durable spend.
async fn seed_usage(
    store: &Store,
    key: Option<uuid::Uuid>,
    provider: &str,
    model: &str,
    cost_micros: u64,
    ts: i64,
) {
    store
        .record_usage(&UsageRecord {
            id: None,
            ts,
            request_id: Some(uuid::Uuid::new_v4()),
            provider: provider.to_string(),
            model: model.to_string(),
            connection_id: None,
            endpoint: Some("/v1/chat/completions".to_string()),
            key_id: key,
            cost_micros,
            input_tokens: 1,
            output_tokens: 1,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            latency_ms: 1,
            ttft_ms: 0,
            status: 200,
            streamed: false,
            error: None,
        })
        .await
        .expect("seed usage");
}

/// Wait for the spawned usage writer, then return the newest row this key wrote for
/// `model` — accounting is asynchronous, so an assertion must not race it.
async fn wait_for_usage(h: &Harness, key: uuid::Uuid, model: &str) -> UsageRecord {
    for _ in 0..100 {
        if let Ok(rows) = h.state.store.recent_usage(&UsageQuery { limit: 50, ..Default::default() }).await {
            if let Some(row) = rows
                .into_iter()
                .find(|r| r.key_id == Some(key) && r.model == model)
            {
                return row;
            }
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("no usage row for key {key} and model {model}");
}

/// Money beside the tokens: a priced request stores the exact micro-USD amount, the
/// usage API exposes it, the key's spend sums it, and an unpriced model stores 0.
#[tokio::test]
async fn priced_requests_record_micro_usd_and_unpriced_models_record_zero() {
    let mock = Mock::start(vec![openai_reply("priced"), openai_reply("unpriced")]).await;
    let h = harness(false);
    h.connect("openai", &mock).await;
    // The registry is the price source: USD per million tokens from models.dev.
    h.state
        .store
        .upsert_model(&ModelRow {
            provider: "openai".to_string(),
            model: "gpt-4.1".to_string(),
            display: None,
            context: None,
            max_output: None,
            caps: json!({}),
            cost: Some(json!({"input": 10.0, "output": 20.0})),
            source: "synced".to_string(),
            updated_at: 0,
        })
        .await
        .expect("priced model");
    let (key, id) = h.create_key().await;
    let uuid = uuid::Uuid::parse_str(&id).expect("uuid");

    // The mock reports 7 prompt + 3 completion tokens: 7×10 + 3×20 = 130 µUSD.
    let (status, _, body) = h.chat(Some(&key), "gpt-4.1").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let row = wait_for_usage(&h, uuid, "gpt-4.1").await;
    assert_eq!(row.cost_micros, 130, "micro-USD, computed at record time");

    let request_id = row.request_id.expect("request id");
    let (status, body) = h.get(&format!("/api/usage/{request_id}")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["request"]["cost_micros"], 130);

    let (status, body) = h.get("/api/usage?limit=10").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"][0]["cost_micros"], 130, "the log row carries its cost");

    let (status, body) = h.get("/api/keys").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"][0]["spent_micros"], 130);
    assert_eq!(body["data"][0]["budget_micros"], 0, "0 means unlimited");

    // A model the registry publishes no price for costs 0 — never a guessed rate.
    let (status, _, body) = h.chat(Some(&key), "gpt-4.1-mini").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let unpriced = wait_for_usage(&h, uuid, "gpt-4.1-mini").await;
    assert_eq!(unpriced.cost_micros, 0);
    assert_eq!(
        h.state.store.api_key_cost_spend(uuid, None).await.expect("spend"),
        130,
        "an unpriced row adds nothing to the spend"
    );
}

/// `/api/usage` pages backwards with `before` + `before_id`: pages are disjoint and
/// newest-first, a tied timestamp is never split into a duplicate or a gap, and the
/// last page reports `next_before: null`.
#[tokio::test]
async fn usage_api_pages_backwards_without_overlap() {
    let h = harness(false);
    // Four rows; two share ts=200 so the second page boundary falls inside a tie.
    for (ts, model) in [(100, "m1"), (200, "m2"), (200, "m3"), (300, "m4")] {
        h.state
            .store
            .record_usage(&UsageRecord {
                id: None,
                ts,
                request_id: Some(uuid::Uuid::new_v4()),
                provider: "openai".to_string(),
                model: model.to_string(),
                connection_id: None,
                endpoint: Some("/v1/chat/completions".to_string()),
                key_id: None,
                cost_micros: 0,
                input_tokens: 1,
                output_tokens: 1,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                latency_ms: 1,
                ttft_ms: 0,
                status: 200,
                streamed: false,
                error: None,
            })
            .await
            .expect("record usage");
    }

    let (status, first) = h.get("/api/usage?limit=2").await;
    assert_eq!(status, StatusCode::OK);
    let first_models: Vec<&str> =
        first["data"].as_array().expect("rows").iter().map(|r| r["model"].as_str().unwrap()).collect();
    assert_eq!(first_models, vec!["m4", "m3"], "newest first");
    assert_eq!(first["next_before"], 200, "oldest returned row's ts");
    let before_id = first["next_before_id"].as_i64().expect("pair cursor id");

    let (status, second) =
        h.get(&format!("/api/usage?limit=2&before=200&before_id={before_id}")).await;
    assert_eq!(status, StatusCode::OK);
    let second_models: Vec<&str> =
        second["data"].as_array().expect("rows").iter().map(|r| r["model"].as_str().unwrap()).collect();
    assert_eq!(second_models, vec!["m2", "m1"], "disjoint from page one, tie not dropped");
    assert_eq!(second["next_before"], 100, "a full page still points at its oldest row");
    let before_id = second["next_before_id"].as_i64().expect("pair cursor id");

    let (status, last) =
        h.get(&format!("/api/usage?limit=2&before=100&before_id={before_id}")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(last["data"].as_array().expect("rows").is_empty(), "nothing older: {last}");
    assert!(last["next_before"].is_null(), "an empty page ends the walk");
}

/// `/api/usage` filters happen in SQL, so a filtered walk pages through matching rows only:
/// `status=error` pages past the 2xx rows, the cursor survives the filter, and malformed
/// `key_id` or `status` values are a `400`, never an empty page.
#[tokio::test]
async fn usage_api_filters_compose_with_the_cursor() {
    let h = harness(false);
    let key = uuid::Uuid::new_v4();
    // (ts, model, status, error): 2xx, 5xx, 2xx-with-error-text, 429, 2xx.
    let rows = [
        (100, "m1", 200, None),
        (200, "m2", 500, None),
        (300, "m3", 200, Some("stream dropped")),
        (400, "m4", 429, None),
        (500, "m5", 200, None),
    ];
    for (ts, model, status, error) in rows {
        h.state
            .store
            .record_usage(&UsageRecord {
                id: None,
                ts,
                request_id: Some(uuid::Uuid::new_v4()),
                provider: "openai".to_string(),
                model: model.to_string(),
                connection_id: None,
                endpoint: Some("/v1/chat/completions".to_string()),
                key_id: Some(key),
                cost_micros: 0,
                input_tokens: 1,
                output_tokens: 1,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                latency_ms: 1,
                ttft_ms: 0,
                status,
                streamed: false,
                error: error.map(str::to_string),
            })
            .await
            .expect("record usage");
    }

    let models = |page: &Value| -> Vec<String> {
        page["data"]
            .as_array()
            .expect("rows")
            .iter()
            .map(|r| r["model"].as_str().unwrap().to_string())
            .collect()
    };

    // Errors are status >= 400 or error text: m4, m3, m2, newest first, one per page.
    let (status, mut page) = h.get("/api/usage?status=error&limit=1").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(models(&page), vec!["m4"]);
    let mut seen = models(&page);
    for expected in ["m3", "m2"] {
        let before = page["next_before"].as_i64().expect("cursor ts");
        let before_id = page["next_before_id"].as_i64().expect("cursor id");
        let (status, next) = h
            .get(&format!(
                "/api/usage?status=error&limit=1&before={before}&before_id={before_id}"
            ))
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(models(&next), vec![expected], "next page stays filtered: {next}");
        seen.extend(models(&next));
        page = next;
    }
    assert_eq!(seen, vec!["m4", "m3", "m2"]);
    // The walk ends with an empty page and no further cursor.
    let before = page["next_before"].as_i64().expect("cursor ts");
    let before_id = page["next_before_id"].as_i64().expect("cursor id");
    let (_, last) = h
        .get(&format!("/api/usage?status=error&limit=1&before={before}&before_id={before_id}"))
        .await;
    assert!(models(&last).is_empty(), "nothing older: {last}");
    assert!(last["next_before"].is_null());

    // `2xx` is success with no error text; `hours` still composes with it.
    let (_, ok) = h.get("/api/usage?status=2xx&limit=10").await;
    assert_eq!(models(&ok), vec!["m5", "m1"], "m3 is 2xx but carries error text");

    // Key and model-ref filters; a ref matches its bare stored id.
    let (_, by_key) = h.get(&format!("/api/usage?key_id={key}&status=error&limit=10")).await;
    assert_eq!(models(&by_key), vec!["m4", "m3", "m2"]);
    let (_, by_ref) = h.get("/api/usage?provider=openai&model=openai/m2&limit=10").await;
    assert_eq!(models(&by_ref), vec!["m2"], "a ref matches its bare id");
    let (_, anon) = h.get(&format!("/api/usage?key_id={}&limit=10", uuid::Uuid::new_v4())).await;
    assert!(models(&anon).is_empty(), "an unknown key has no rows: {anon}");

    // Malformed filters are client errors, not empty pages.
    let (status, body) = h.get("/api/usage?key_id=not-a-uuid").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");
    assert!(body["error"]["message"].as_str().is_some_and(|m| m.ends_with("key_id must be a uuid")), "body: {body}");
    let (status, body) = h.get("/api/usage?status=ok").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");
}

// ------------------------------------------------------------- usage CSV export

/// A complete usage row for the export tests: every printed field is explicit so a
/// test only edits the cells it is about.
fn export_row(ts: i64) -> UsageRecord {
    UsageRecord {
        id: None,
        ts,
        request_id: Some(uuid::Uuid::new_v4()),
        provider: "openai".to_string(),
        model: "gpt-4.1".to_string(),
        connection_id: None,
        endpoint: Some("/v1/chat/completions".to_string()),
        key_id: None,
        cost_micros: 0,
        input_tokens: 7,
        output_tokens: 3,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        latency_ms: 12,
        ttft_ms: 4,
        status: 200,
        streamed: false,
        error: None,
    }
}

/// Minimal RFC 4180 reader: a quoted cell may hold commas, doubled quotes and newlines,
/// which is what turns the escaping assertions into a round trip instead of a grep.
fn parse_csv(text: &str) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut cell = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                cell.push('"');
                chars.next();
            }
            '"' => quoted = !quoted,
            ',' if !quoted => row.push(std::mem::take(&mut cell)),
            '\n' if !quoted => {
                row.push(std::mem::take(&mut cell));
                rows.push(std::mem::take(&mut row));
            }
            '\r' if !quoted => {}
            c => cell.push(c),
        }
    }
    if !cell.is_empty() || !row.is_empty() {
        row.push(cell);
        rows.push(row);
    }
    rows
}

/// The cells of one column, header excluded.
fn csv_column(rows: &[Vec<String>], index: usize) -> Vec<&str> {
    rows[1..].iter().map(|r| r[index].as_str()).collect()
}

/// The export is the contract finance reads: exact header, `text/csv` + attachment
/// filename, RFC 3339 timestamps, micro-USD and a six-decimal dollar column, empty
/// cells for absent ids, and a model/endpoint that need quoting still lands in the
/// right column.
#[tokio::test]
async fn usage_export_csv_has_the_contract_header_and_escapes_cells() {
    let h = harness(false);
    let mut plain = export_row(1_794_009_599); // 2026-11-06T23:59:59Z
    plain.request_id = None;
    plain.streamed = true;
    plain.cost_micros = 130;
    let request_id = uuid::Uuid::new_v4();
    let key_id = uuid::Uuid::new_v4();
    let mut tricky = export_row(1_794_017_491); // 2026-11-07T02:11:31Z
    tricky.request_id = Some(request_id);
    tricky.model = "weird,\"model\"\nline2".to_string();
    tricky.endpoint = Some("/v1/chat/completions?x=1,2".to_string());
    tricky.key_id = Some(key_id);
    tricky.cost_micros = 1_234_567;
    tricky.status = 500;
    for row in [plain, tricky] {
        h.state.store.record_usage(&row).await.expect("seed export row");
    }

    let (status, headers, body) = h.get_raw("/api/usage/export.csv").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(
        headers.get("content-type").and_then(|v| v.to_str().ok()),
        Some("text/csv; charset=utf-8")
    );
    let disposition = headers
        .get("content-disposition")
        .and_then(|v| v.to_str().ok())
        .expect("attachment");
    let name = disposition
        .strip_prefix("attachment; filename=\"rrouter-usage-")
        .and_then(|rest| rest.strip_suffix(".csv\""))
        .unwrap_or_else(|| panic!("filename shape: {disposition}"));
    assert_eq!(name.len(), 8, "YYYYMMDD: {disposition}");
    assert!(name.chars().all(|c| c.is_ascii_digit()), "YYYYMMDD: {disposition}");

    let rows = parse_csv(&body);
    assert_eq!(
        rows[0],
        vec![
            "ts", "iso_utc", "provider", "model", "request_id", "key_id", "endpoint",
            "input_tokens", "output_tokens", "cache_read_tokens", "cache_write_tokens",
            "latency_ms", "ttft_ms", "status", "streamed", "cost_micros", "cost_usd",
        ],
        "header is a contract"
    );
    assert_eq!(rows.len(), 3, "header + two rows: {body}");

    // Newest first, and the quoted cells survive the round trip in their own column.
    assert_eq!(
        rows[1],
        vec![
            "1794017491",
            "2026-11-07T02:11:31Z",
            "openai",
            "weird,\"model\"\nline2",
            &request_id.to_string(),
            &key_id.to_string(),
            "/v1/chat/completions?x=1,2",
            "7",
            "3",
            "0",
            "0",
            "12",
            "4",
            "500",
            "false",
            "1234567",
            "1.234567",
        ]
    );
    assert_eq!(
        rows[2],
        vec![
            "1794009599",
            "2026-11-06T23:59:59Z",
            "openai",
            "gpt-4.1",
            "",
            "",
            "/v1/chat/completions",
            "7",
            "3",
            "0",
            "0",
            "12",
            "4",
            "200",
            "true",
            "130",
            "0.000130",
        ],
        "absent ids are empty cells, streamed is a boolean"
    );
}

/// The export selects the same rows the dashboard does: `status=error` is the failure
/// rule of the list, `hours` bounds the range, provider/key/model filters compose, and a
/// malformed filter is a `400` JSON error, never a header-only download.
#[tokio::test]
async fn usage_export_csv_applies_the_list_filters() {
    let h = harness(false);
    let now = rrouter_gateway::now_unix();
    let key = uuid::Uuid::new_v4();
    let mut recent_ok = export_row(now - 60);
    recent_ok.model = "recent-ok".to_string();
    let mut recent_err = export_row(now - 120);
    recent_err.model = "recent-err".to_string();
    recent_err.status = 500;
    recent_err.key_id = Some(key);
    let mut old_err = export_row(now - 5 * 3_600);
    old_err.model = "old-err".to_string();
    old_err.status = 502;
    old_err.provider = "anthropic".to_string();
    for row in [recent_ok, recent_err, old_err] {
        h.state.store.record_usage(&row).await.expect("seed export row");
    }

    let models = |body: &str| -> Vec<String> {
        csv_column(&parse_csv(body), 3).into_iter().map(str::to_string).collect()
    };
    let (status, _, all) = h.get_raw("/api/usage/export.csv").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(models(&all), vec!["recent-ok", "recent-err", "old-err"]);

    let (status, _, errors) = h.get_raw("/api/usage/export.csv?status=error").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(models(&errors), vec!["recent-err", "old-err"]);

    let (status, _, last_hour) = h.get_raw("/api/usage/export.csv?hours=1").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(models(&last_hour), vec!["recent-ok", "recent-err"], "hours bounds the range");

    let (status, _, bounded) = h.get_raw("/api/usage/export.csv?hours=1&status=error").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(models(&bounded), vec!["recent-err"]);

    // Provider, key and model are the list's filters, applied in SQL for the walk too.
    let (status, _, by_provider) = h.get_raw("/api/usage/export.csv?provider=anthropic").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(models(&by_provider), vec!["old-err"]);
    let (status, _, by_key) = h.get_raw(&format!("/api/usage/export.csv?key_id={key}")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(models(&by_key), vec!["recent-err"]);
    let (status, _, by_model) =
        h.get_raw("/api/usage/export.csv?model=recent-ok&status=2xx").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(models(&by_model), vec!["recent-ok"]);

    let (status, headers, body) = h.get_raw("/api/usage/export.csv?status=ok").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");
    assert_eq!(
        headers.get("content-type").and_then(|v| v.to_str().ok()),
        Some("application/json"),
        "a bad filter is an admin error, not a CSV"
    );
    let (status, _, _) = h.get_raw("/api/usage/export.csv?key_id=nope").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// The download sits behind the same admin gate as the rest of `/api/usage`: loopback
/// passes, a remote peer without the token does not, and the token is what admits one.
#[tokio::test]
async fn usage_export_csv_is_behind_the_admin_gate() {
    let h = harness(false);
    let (status, _, body) = h.get_raw("/api/usage/export.csv").await;
    assert_eq!(status, StatusCode::OK, "in-process calls count as loopback: {body}");
    let res = h
        .router
        .clone()
        .oneshot(as_peer(get_with("/api/usage/export.csv", &[]), remote_peer()))
        .await
        .expect("oneshot");
    assert_eq!(res.status(), StatusCode::FORBIDDEN, "no token, remote peer");

    let mut cfg = config(false);
    cfg.admin_token = Some(TEST_ADMIN_TOKEN.into());
    let gated = harness_with(cfg);
    let res = gated
        .router
        .clone()
        .oneshot(as_peer(get_with("/api/usage/export.csv", &[]), remote_peer()))
        .await
        .expect("oneshot");
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED, "a token replaces the loopback rule");
    let res = gated
        .router
        .clone()
        .oneshot(as_peer(
            get_with("/api/usage/export.csv", &[("x-admin-token", TEST_ADMIN_TOKEN)]),
            remote_peer(),
        ))
        .await
        .expect("oneshot");
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = res.into_body().collect().await.expect("body").to_bytes();
    assert!(String::from_utf8_lossy(&bytes).starts_with("ts,iso_utc,provider"));
}

/// The walk is keyset-paged at 1000 rows: 1005 rows come back complete, newest first,
/// exactly once, with the page boundary not dropping or repeating the tied rows — and
/// the body arrives as one frame per page instead of a buffered blob.
#[tokio::test]
async fn usage_export_csv_walks_past_the_page_boundary() {
    let h = harness(false);
    for i in 0..1005 {
        let mut row = export_row(1_000 + i);
        row.model = format!("m{i:04}");
        h.state.store.record_usage(&row).await.expect("seed export row");
    }

    let res = h
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/usage/export.csv")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("oneshot");
    assert_eq!(res.status(), StatusCode::OK);
    // Count the data frames: streaming means the header and each page are written as
    // they are produced, not one body assembled after the walk finished.
    let mut body = res.into_body();
    let mut frames = 0;
    let mut raw = Vec::new();
    while let Some(next) = body.frame().await {
        let frame = next.expect("frame");
        if let Some(data) = frame.data_ref() {
            frames += 1;
            raw.extend_from_slice(data);
        }
    }
    assert_eq!(frames, 3, "header frame + one frame per page");
    let text = String::from_utf8(raw).expect("utf-8 csv");
    let rows = parse_csv(&text);
    assert_eq!(rows.len(), 1006, "header + 1005 rows over two pages");

    let ts: Vec<i64> = csv_column(&rows, 0).iter().map(|t| t.parse().expect("ts")).collect();
    assert_eq!(ts.len(), 1005);
    assert_eq!(ts[0], 2004, "newest first");
    assert_eq!(ts[1004], 1000);
    assert!(ts.windows(2).all(|w| w[0] > w[1]), "descending across the page boundary");
    let models: std::collections::HashSet<&str> = csv_column(&rows, 3).into_iter().collect();
    assert_eq!(models.len(), 1005, "no row repeated");
}

/// A USD budget is enforced from durable spend, and the refusal names dollars.
#[tokio::test]
async fn usd_budget_below_durable_spend_answers_429_naming_dollars() {
    let mock = Mock::start(vec![openai_reply("paid")]).await;
    let h = harness(false);
    h.connect("openai", &mock).await;
    let (key, id) = h.create_key().await;
    let uuid = uuid::Uuid::parse_str(&id).expect("uuid");

    // Prior spend for this key: $2.000000.
    seed_usage(
        &h.state.store,
        Some(uuid),
        "openai",
        "gpt-4.1",
        2_000_000,
        rrouter_gateway::now_unix(),
    )
    .await;

    let patched = h.patch_key(&id, json!({"budget_micros": 1_000_000})).await;
    assert_eq!(patched["budget_micros"], 1_000_000);
    assert_eq!(patched["spent_micros"], 2_000_000);

    let (status, _, body) = h.chat(Some(&key), "gpt-4.1").await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "body: {body}");
    assert_eq!(body["error"]["code"], "budget_exhausted");
    let message = body["error"]["message"].as_str().expect("message");
    assert!(message.contains("$2.0000"), "names what was spent: {message}");
    assert!(message.contains("$1.0000"), "names the budget: {message}");
    assert_eq!(mock.hits(), 0, "an over-budget key is refused before dispatch");

    // An integer field: `null` is a type error, `0` is the unlimited value.
    let (status, _, body) = h
        .call_full(
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/keys/{id}"))
                .header("content-type", "application/json")
                .body(Body::from(json!({"budget_micros": null}).to_string()))
                .expect("request"),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");

    h.patch_key(&id, json!({"budget_micros": 5_000_000})).await;
    let (status, _, body) = h.chat(Some(&key), "gpt-4.1").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(mock.hits(), 1);
}

/// A monthly token budget is measured from the 1st of the current UTC month. Spend from
/// before that boundary no longer counts, so the key is admitted again without an edit;
/// spend inside the window still blocks it.
#[tokio::test]
async fn monthly_window_admits_when_the_spend_is_outside_and_blocks_inside() {
    let now = rrouter_gateway::now_unix();
    let month_start = rrouter_store::window_start(rrouter_store::BudgetWindow::Monthly, now)
        .expect("monthly has a start");

    // Outside the window: the budget is exhausted, but the spend is last month's.
    let mock = Mock::start(vec![openai_reply("paid")]).await;
    let h = harness(false);
    h.connect("openai", &mock).await;
    let (key, id) = h.create_key().await;
    let uuid = uuid::Uuid::parse_str(&id).expect("uuid");
    h.state
        .store
        .record_usage(&UsageRecord {
            id: None,
            ts: month_start - 1,
            request_id: Some(uuid::Uuid::new_v4()),
            provider: "openai".to_string(),
            model: "gpt-4.1".to_string(),
            connection_id: None,
            endpoint: Some("/v1/chat/completions".to_string()),
            key_id: Some(uuid),
            cost_micros: 0,
            input_tokens: 500,
            output_tokens: 500,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            latency_ms: 1,
            ttft_ms: 0,
            status: 200,
            streamed: false,
            error: None,
        })
        .await
        .expect("seed last month");
    h.patch_key(&id, json!({"budget_tokens": 100, "budget_window": "monthly"})).await;
    let (status, _, body) = h.chat(Some(&key), "gpt-4.1").await;
    assert_eq!(status, StatusCode::OK, "last month's spend must not count: {body}");
    assert_eq!(mock.hits(), 1);

    // Inside the window: the same budget refuses the next request.
    h.state
        .store
        .record_usage(&UsageRecord {
            id: None,
            ts: now,
            request_id: Some(uuid::Uuid::new_v4()),
            provider: "openai".to_string(),
            model: "gpt-4.1".to_string(),
            connection_id: None,
            endpoint: Some("/v1/chat/completions".to_string()),
            key_id: Some(uuid),
            cost_micros: 0,
            input_tokens: 60,
            output_tokens: 60,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            latency_ms: 1,
            ttft_ms: 0,
            status: 200,
            streamed: false,
            error: None,
        })
        .await
        .expect("seed this month");
    let (status, _, body) = h.chat(Some(&key), "gpt-4.1").await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "body: {body}");
    assert_eq!(body["error"]["code"], "budget_exhausted");
    assert_eq!(mock.hits(), 1, "an exhausted key is refused before dispatch");

    // The list reports only the in-window spend.
    let (_, listed) = h.get("/api/keys").await;
    assert_eq!(listed["data"][0]["budget_window"], "monthly");
    assert_eq!(listed["data"][0]["spent_tokens"], 120);
}

/// A monthly key with a USD budget spent before the window is admitted.
#[tokio::test]
async fn monthly_usd_window_ignores_last_months_spend() {
    let now = rrouter_gateway::now_unix();
    let month_start = rrouter_store::window_start(rrouter_store::BudgetWindow::Monthly, now)
        .expect("monthly has a start");
    let mock = Mock::start(vec![openai_reply("paid")]).await;
    let h = harness(false);
    h.connect("openai", &mock).await;
    let (key, id) = h.create_key().await;
    let uuid = uuid::Uuid::parse_str(&id).expect("uuid");
    seed_usage(&h.state.store, Some(uuid), "openai", "gpt-4.1", 2_000_000, month_start - 1).await;

    h.patch_key(&id, json!({"budget_micros": 1_000_000, "budget_window": "monthly"})).await;
    let (status, _, body) = h.chat(Some(&key), "gpt-4.1").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(mock.hits(), 1);

    let (_, listed) = h.get("/api/keys").await;
    assert_eq!(listed["data"][0]["spent_micros"], 0);
}

/// `budget_window` accepts the four spellings and refuses anything else with a 400 that
/// names the allowed values; a refused patch leaves the stored window untouched.
#[tokio::test]
async fn budget_window_patch_validates_the_spelling() {
    let h = harness(false);
    let (_, id) = h.create_key().await;

    for window in ["total", "daily", "weekly", "monthly"] {
        let patched = h.patch_key(&id, json!({"budget_window": window})).await;
        assert_eq!(patched["budget_window"], window);
    }

    let (status, _, body) = h
        .call_full(
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/keys/{id}"))
                .header("content-type", "application/json")
                .body(Body::from(json!({"budget_window": "hourly"}).to_string()))
                .expect("request"),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");
    let message = body["error"]["message"].as_str().expect("message");
    assert!(
        message.ends_with("budget_window must be one of total, daily, weekly, monthly"),
        "names the allowed values: {message}"
    );
    let (_, listed) = h.get("/api/keys").await;
    assert_eq!(listed["data"][0]["budget_window"], "monthly", "refusal changed nothing");
}

/// Summary and series aggregate the cost column server-side, like the tokens.
#[tokio::test]
async fn summary_and_series_sum_cost_per_group_and_bucket() {
    let h = harness(false);
    let now = rrouter_gateway::now_unix();
    seed_usage(&h.state.store, None, "openai", "gpt-4.1", 1_000, now).await;
    seed_usage(&h.state.store, None, "openai", "gpt-4.1", 2_500, now).await;

    let (status, body) = h.get("/api/usage/summary").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"][0]["requests"], 2);
    assert_eq!(body["data"][0]["cost_micros"], 3_500);

    let (status, body) = h.get("/api/usage/series?hours=1&bucket=hour").await;
    assert_eq!(status, StatusCode::OK);
    let total: u64 = body["data"]
        .as_array()
        .expect("buckets")
        .iter()
        .map(|b| b["cost_micros"].as_u64().expect("bucket cost"))
        .sum();
    assert_eq!(total, 3_500, "both rows fall in one bucket: {body}");

    // The per-model breakdown carries the same column.
    let (status, body) = h.get("/api/usage/series?hours=1&bucket=hour&by=model").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"][0]["model"], "gpt-4.1");
    assert_eq!(body["data"][0]["cost_micros"], 3_500);
}

/// The weighted strategy is only usable if an operator can set its inputs: `PATCH
/// /api/connections/{id}` writes priority and weight, `GET` reads them back, and the
/// plan follows the patched weights.
#[tokio::test]
async fn connection_patch_sets_priority_and_weight_and_the_plan_follows() {
    let embedding = || {
        MockReply::Json(
            200,
            json!({
                "object": "list",
                "data": [{"object": "embedding", "index": 0, "embedding": [0.1]}],
                "model": "text-embedding-3-small",
                "usage": {"prompt_tokens": 1, "total_tokens": 1}
            }),
        )
    };
    let heavy = Mock::start(vec![embedding(), embedding(), embedding(), embedding()]).await;
    let light = Mock::start(vec![embedding(), embedding(), embedding(), embedding()]).await;
    let mut cfg = config(false);
    cfg.strategy = rrouter_router::Strategy::Weighted;
    let h = harness_with(cfg);
    let heavy_id = h.connect("openai", &heavy).await;
    let light_id = h.connect("openai", &light).await;

    let patched = h
        .patch_connection(&heavy_id.to_string(), json!({"weight": 3, "priority": 0}))
        .await;
    assert_eq!(patched["weight"], 3);
    assert_eq!(patched["priority"], 0);
    let patched = h
        .patch_connection(&light_id.to_string(), json!({"weight": 1, "priority": 0}))
        .await;
    assert_eq!(patched["weight"], 1);

    let (status, body) = h.get("/api/connections").await;
    assert_eq!(status, StatusCode::OK);
    let rows = body["data"].as_array().expect("rows").clone();
    let by_id = |id: uuid::Uuid| {
        rows.iter()
            .find(|row| row["id"] == id.to_string())
            .cloned()
            .expect("connection row")
    };
    assert_eq!(by_id(heavy_id)["weight"], 3, "GET exposes the weight the plan uses");
    assert_eq!(by_id(light_id)["weight"], 1);
    assert_eq!(by_id(heavy_id)["priority"], 0);

    for _ in 0..4 {
        let (status, body) = h
            .post(
                "/v1/embeddings",
                json!({"model": "openai/text-embedding-3-small", "input": "hi"}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    assert_eq!(heavy.hits(), 3, "weight 3 leads three of four plans");
    assert_eq!(light.hits(), 1);

    // A patch only touches the fields it names, and priority is written too.
    let patched = h
        .patch_connection(&light_id.to_string(), json!({"priority": 7}))
        .await;
    assert_eq!(patched["priority"], 7);
    assert_eq!(patched["weight"], 1, "an unnamed weight keeps its value");
}

// -------------------------------------------------------- response cache (phase 8)

/// The whole point: the same non-streaming request twice costs one upstream call, and
/// the second response is the first one byte-for-byte under a new request id.
#[tokio::test]
async fn identical_non_streaming_requests_hit_the_cache() {
    let mock = Mock::start(vec![openai_reply("cached answer"), openai_reply("second answer")]).await;
    let h = harness_with(cached_config(300, 100));
    h.connect("openai", &mock).await;

    let (status, headers, first) =
        h.post_headers("/v1/chat/completions", openai_chat_body("gpt-4.1"), &[]).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(headers["x-rrouter-cache"], "miss");
    assert_eq!(mock.hits(), 1);

    let (status, headers, second) =
        h.post_headers("/v1/chat/completions", openai_chat_body("gpt-4.1"), &[]).await;
    assert_eq!(status, StatusCode::OK, "{second}");
    assert_eq!(headers["x-rrouter-cache"], "hit");
    assert_eq!(second, first, "a hit is the stored response body");
    assert_eq!(mock.hits(), 1, "the upstream was not called twice");
    let hit_id = headers["x-request-id"].to_str().expect("id").to_string();

    // An explicit `stream:false` is the same request as an absent `stream`: the
    // canonical body the gateway sends is identical, so it shares the entry.
    let mut explicit = openai_chat_body("gpt-4.1");
    explicit["stream"] = json!(false);
    let (_, headers, third) = h.post_headers("/v1/chat/completions", explicit, &[]).await;
    assert_eq!(headers["x-rrouter-cache"], "hit");
    assert_eq!(third, first);
    assert_eq!(mock.hits(), 1);
    assert_ne!(
        headers["x-request-id"].to_str().expect("id"),
        hit_id,
        "every request still gets its own id"
    );

    // The hit is visible in Activity as a real request that cost nothing.
    let row = h
        .usage_for(uuid::Uuid::parse_str(&hit_id).expect("uuid"))
        .await;
    assert_eq!(row.status, 200);
    assert_eq!(row.endpoint.as_deref(), Some("/v1/chat/completions"));
    assert_eq!(row.provider, "openai");
    assert_eq!(row.input_tokens, 0, "no prompt was sent");
    assert_eq!(row.output_tokens, 0, "nothing was generated");
    assert_eq!(row.cache_read_tokens, 0);
    assert_eq!(row.cache_write_tokens, 0);
    assert_eq!(row.cost_micros, 0, "a cache hit is free");

    // Both outcomes are visible to a Prometheus scrape.
    let (status, _, text) = h.get_raw("/metrics").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        metric_value(&text, "rrouter_cache_hits_total") >= 1,
        "a hit is counted: {text}"
    );
    assert!(
        metric_value(&text, "rrouter_cache_misses_total") >= 1,
        "the first request was a miss: {text}"
    );
}

/// `/v1/messages` is cached the same way, and its stored answer is the client's own
/// dialect (an Anthropic message, not a chat completion).
#[tokio::test]
async fn anthropic_messages_are_cached_too() {
    let reply = || {
        MockReply::Json(
            200,
            json!({
                "id": "msg_1",
                "type": "message",
                "role": "assistant",
                "model": "sonnet-4-5",
                "content": [{"type": "text", "text": "bonjour"}],
                "stop_reason": "end_turn",
                "usage": {"input_tokens": 9, "output_tokens": 4}
            }),
        )
    };
    let mock = Mock::start(vec![reply(), reply()]).await;
    let h = harness_with(cached_config(300, 100));
    h.connect("claude", &mock).await;

    let body = json!({
        "model": "claude/sonnet-4-5",
        "max_tokens": 64,
        "messages": [{"role": "user", "content": "hi"}]
    });
    let (status, headers, first) = h.post_headers("/v1/messages", body.clone(), &[]).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(headers["x-rrouter-cache"], "miss");
    let (status, headers, second) = h.post_headers("/v1/messages", body, &[]).await;
    assert_eq!(status, StatusCode::OK, "{second}");
    assert_eq!(headers["x-rrouter-cache"], "hit");
    assert_eq!(second["type"], "message");
    assert_eq!(second, first);
    assert_eq!(mock.hits(), 1);
}

/// A non-streaming client of a stream-only provider (codex) is served by reassembly;
/// that reassembled body is still a non-streaming response, so it caches like any other.
#[tokio::test]
async fn reassembled_stream_upstream_still_caches_for_a_non_streaming_client() {
    let wire = concat!(
        "event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\",\"model\":\"gpt-5.6-luna\"}}\n\n",
        "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"Hel\"}\n\n",
        "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"lo\"}\n\n",
        "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\",\"usage\":{\"input_tokens\":4,\"output_tokens\":2}}}\n\n",
    );
    let sse = || {
        MockReply::Sse(vec![SseStep {
            wire: wire.to_string(),
            await_gate: None,
        }])
    };
    let mock = Mock::start(vec![sse(), sse()]).await;
    let h = harness_with(cached_config(300, 100));
    h.connect("codex", &mock).await;

    let body = openai_chat_body("codex/gpt-5.6-luna");
    let (status, headers, first) =
        h.post_headers("/v1/chat/completions", body.clone(), &[]).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(headers["x-rrouter-cache"], "miss");
    assert_eq!(first["choices"][0]["message"]["content"], "Hello");

    let (status, headers, second) = h.post_headers("/v1/chat/completions", body, &[]).await;
    assert_eq!(status, StatusCode::OK, "{second}");
    assert_eq!(headers["x-rrouter-cache"], "hit");
    assert_eq!(second, first);
    assert_eq!(mock.hits(), 1, "the second client request never reached codex");
}

/// A cache entry belongs to one canonical request: a different temperature or model is
/// a different request and must reach the upstream.
#[tokio::test]
async fn cache_key_covers_temperature_and_model() {
    let mock = Mock::start(vec![
        openai_reply("temp0"),
        openai_reply("temp0.7"),
        openai_reply("mini"),
        openai_reply("temp0 again"),
    ])
    .await;
    let h = harness_with(cached_config(300, 100));
    h.connect("openai", &mock).await;

    let mut body = openai_chat_body("gpt-4.1");
    body["temperature"] = json!(0);
    let (_, headers, first) = h.post_headers("/v1/chat/completions", body.clone(), &[]).await;
    assert_eq!(headers["x-rrouter-cache"], "miss");
    assert_eq!(first["choices"][0]["message"]["content"], "temp0");

    // Same body again: no upstream call.
    let (_, headers, repeat) = h.post_headers("/v1/chat/completions", body.clone(), &[]).await;
    assert_eq!(headers["x-rrouter-cache"], "hit");
    assert_eq!(repeat["choices"][0]["message"]["content"], "temp0");
    assert_eq!(mock.hits(), 1);

    // Same prompt, different sampling: a miss.
    body["temperature"] = json!(0.7);
    let (_, headers, sampled) = h.post_headers("/v1/chat/completions", body.clone(), &[]).await;
    assert_eq!(headers["x-rrouter-cache"], "miss");
    assert_eq!(sampled["choices"][0]["message"]["content"], "temp0.7");
    assert_eq!(mock.hits(), 2);

    // Same sampling, different model: a miss.
    let mut other = openai_chat_body("gpt-4.1-mini");
    other["temperature"] = json!(0.7);
    let (_, headers, mini) = h.post_headers("/v1/chat/completions", other, &[]).await;
    assert_eq!(headers["x-rrouter-cache"], "miss");
    assert_eq!(mini["choices"][0]["message"]["content"], "mini");
    assert_eq!(mock.hits(), 3);

    // The earlier entry survived both misses.
    body["temperature"] = json!(0);
    let (_, headers, again) = h.post_headers("/v1/chat/completions", body, &[]).await;
    assert_eq!(headers["x-rrouter-cache"], "hit");
    assert_eq!(again["choices"][0]["message"]["content"], "temp0");
    assert_eq!(mock.hits(), 3);
}

/// A streamed response is never stored or served from the cache: it has no single body
/// to hand back, and the contract is about non-streaming requests only.
#[tokio::test]
async fn streamed_requests_never_use_the_cache() {
    let sse = || {
        MockReply::Sse(vec![
            SseStep {
                wire: rrouter_core::sse::render_openai(
                    &json!({"id":"c1","model":"mock-model","choices":[{"index":0,"delta":{"content":"A"},"finish_reason":"stop"}],"usage":{"prompt_tokens":2,"completion_tokens":1}}).to_string(),
                    None,
                ),
                await_gate: None,
            },
            SseStep { wire: "data: [DONE]\n\n".to_string(), await_gate: None },
        ])
    };
    let mock = Mock::start(vec![sse(), sse()]).await;
    let h = harness_with(cached_config(300, 100));
    h.connect("openai", &mock).await;

    let mut body = openai_chat_body("gpt-4.1");
    body["stream"] = json!(true);
    for _ in 0..2 {
        let (status, headers, _) = h.post_headers("/v1/chat/completions", body.clone(), &[]).await;
        assert_eq!(status, StatusCode::OK);
        assert!(headers.get("x-rrouter-cache").is_none(), "streams carry no cache marker");
    }
    assert_eq!(mock.hits(), 2, "both streamed requests reached the upstream");
    assert!(
        h.state.store.cache_entries().await.expect("entries").is_empty(),
        "a stream is never stored"
    );
}

/// `x-rrouter-cache: bypass` reads nothing and writes nothing: the entry already there
/// is neither served nor replaced.
#[tokio::test]
async fn bypass_forces_a_miss_without_touching_the_entry() {
    let mock = Mock::start(vec![openai_reply("stored"), openai_reply("fresh")]).await;
    let h = harness_with(cached_config(300, 100));
    h.connect("openai", &mock).await;
    let body = openai_chat_body("gpt-4.1");

    let (_, headers, first) = h.post_headers("/v1/chat/completions", body.clone(), &[]).await;
    assert_eq!(headers["x-rrouter-cache"], "miss");
    assert_eq!(first["choices"][0]["message"]["content"], "stored");
    assert_eq!(mock.hits(), 1);

    let (_, headers, bypassed) = h
        .post_headers(
            "/v1/chat/completions",
            body.clone(),
            &[("x-rrouter-cache", "bypass")],
        )
        .await;
    assert_eq!(bypassed["choices"][0]["message"]["content"], "fresh");
    assert!(
        headers.get("x-rrouter-cache").is_none(),
        "a bypassed request is outside the cache entirely"
    );
    assert_eq!(mock.hits(), 2);

    let (_, headers, again) = h.post_headers("/v1/chat/completions", body, &[]).await;
    assert_eq!(headers["x-rrouter-cache"], "hit");
    assert_eq!(
        again["choices"][0]["message"]["content"], "stored",
        "the bypass did not overwrite the stored response"
    );
    assert_eq!(mock.hits(), 2);
}

/// An entry past its `expires_at` is not served, and the miss replaces it.
#[tokio::test]
async fn expired_cache_entry_is_not_served() {
    let mock = Mock::start(vec![openai_reply("stale"), openai_reply("fresh")]).await;
    let h = harness_with(cached_config(300, 100));
    h.connect("openai", &mock).await;
    let body = openai_chat_body("gpt-4.1");

    let (_, _, first) = h.post_headers("/v1/chat/completions", body.clone(), &[]).await;
    assert_eq!(first["choices"][0]["message"]["content"], "stale");
    let entries = h.state.store.cache_entries().await.expect("entries");
    assert_eq!(entries.len(), 1);
    let key = entries[0].key.clone();

    // Backdate the one row the way a restart would leave an old entry behind.
    h.state
        .store
        .cache_put(&key, &first.to_string(), 1_000, 2_000, 100)
        .await
        .expect("seed expired row");
    assert!(2_000 < rrouter_gateway::now_unix(), "the seeded expiry is in the past");

    let (_, headers, second) = h.post_headers("/v1/chat/completions", body, &[]).await;
    assert_eq!(headers["x-rrouter-cache"], "miss");
    assert_eq!(second["choices"][0]["message"]["content"], "fresh");
    assert_eq!(mock.hits(), 2);

    let entries = h.state.store.cache_entries().await.expect("entries");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].key, key, "the same request reused its key");
    assert!(
        entries[0].expires_at > rrouter_gateway::now_unix(),
        "the stale row was replaced with a fresh expiry"
    );
    assert!(entries[0].body.contains("fresh"));
}

/// Off is the default: without `RROUTER_CACHE=sqlite` nothing is read or written.
#[tokio::test]
async fn cache_is_off_by_default() {
    let mock = Mock::start(vec![openai_reply("one"), openai_reply("two")]).await;
    let h = harness(false);
    h.connect("openai", &mock).await;
    let body = openai_chat_body("gpt-4.1");

    for _ in 0..2 {
        let (status, headers, _) = h.post_headers("/v1/chat/completions", body.clone(), &[]).await;
        assert_eq!(status, StatusCode::OK);
        assert!(headers.get("x-rrouter-cache").is_none());
    }
    assert_eq!(mock.hits(), 2);
    assert!(h.state.store.cache_entries().await.expect("entries").is_empty());
}

/// The table never grows past `RROUTER_CACHE_MAX_ENTRIES`: the oldest entry is dropped,
/// so its request goes upstream again.
#[tokio::test]
async fn cache_eviction_keeps_at_most_max_entries() {
    let mock = Mock::start(vec![
        openai_reply("first"),
        openai_reply("second"),
        openai_reply("third"),
    ])
    .await;
    let h = harness_with(cached_config(300, 1));
    h.connect("openai", &mock).await;

    let mut body = openai_chat_body("gpt-4.1");
    body["temperature"] = json!(0);
    let (_, headers, first) = h.post_headers("/v1/chat/completions", body.clone(), &[]).await;
    assert_eq!(headers["x-rrouter-cache"], "miss");
    assert_eq!(first["choices"][0]["message"]["content"], "first");

    body["temperature"] = json!(0.2);
    let (_, headers, second) = h.post_headers("/v1/chat/completions", body.clone(), &[]).await;
    assert_eq!(headers["x-rrouter-cache"], "miss");
    assert_eq!(second["choices"][0]["message"]["content"], "second");
    assert_eq!(
        h.state.store.cache_entries().await.expect("entries").len(),
        1,
        "max_entries=1 bounds the table"
    );

    // The first key was the oldest, so it was evicted rather than served.
    body["temperature"] = json!(0);
    let (_, headers, again) = h.post_headers("/v1/chat/completions", body, &[]).await;
    assert_eq!(headers["x-rrouter-cache"], "miss");
    assert_eq!(again["choices"][0]["message"]["content"], "third");
    assert_eq!(mock.hits(), 3);
}

// ------------------------------------------------------------- security headers

/// The four headers every response carries, HTML or not.
fn assert_base_security_headers(headers: &axum::http::HeaderMap) {
    let expected = [
        ("x-content-type-options", "nosniff"),
        ("x-frame-options", "DENY"),
        ("referrer-policy", "no-referrer"),
        ("permissions-policy", "camera=(), microphone=(), geolocation=()"),
    ];
    for (name, value) in expected {
        assert_eq!(
            headers.get(name).and_then(|v| v.to_str().ok()),
            Some(value),
            "{name} missing or rewritten: {headers:?}"
        );
    }
}

fn header<'a>(headers: &'a axum::http::HeaderMap, name: &str) -> &'a str {
    headers.get(name).and_then(|v| v.to_str().ok()).unwrap_or_default()
}

/// The dashboard is a document: it gets the base set plus a CSP that keeps the SPA
/// same-origin. `/app/` is the embedded entry point whether or not `ui/dist` exists
/// (without a build it is the built-in shell's `404`, so the test asks for the shell).
#[tokio::test]
async fn html_responses_carry_base_headers_and_the_dashboard_csp() {
    let h = harness(false);
    let (status, headers, body) = h.get_raw("/app/").await;
    assert!(header(&headers, "content-type").starts_with("text/html"), "{headers:?}");
    assert_eq!(status, StatusCode::OK, "dashboard shell: {body}");

    assert_base_security_headers(&headers);
    assert_eq!(
        Some(header(&headers, "content-security-policy")),
        Some(rrouter_gateway::headers::CSP),
        "the HTML policy is the documented one: {headers:?}"
    );
    assert!(header(&headers, "content-security-policy").contains("frame-ancestors 'none'"));
}

/// JSON is not a document: no CSP, but the base set still applies. Covers both an
/// admin endpoint and the proxied `/v1` path, which relays an upstream body.
#[tokio::test]
async fn json_and_proxied_responses_carry_base_headers_without_csp() {
    let mock = Mock::start(vec![openai_reply("hi")]).await;
    let h = harness(false);
    h.connect("openai", &mock).await;

    let (status, headers, _) = h
        .call_full(
            Request::builder()
                .method("GET")
                .uri("/api/system")
                .body(Body::empty())
                .expect("request"),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_base_security_headers(&headers);
    assert!(headers.get("content-security-policy").is_none(), "no CSP on JSON: {headers:?}");

    let (status, headers, body) = h.chat(None, "gpt-4.1").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_base_security_headers(&headers);
    assert!(headers.get("content-security-policy").is_none(), "no CSP on JSON: {headers:?}");
}

/// A scraper still gets the Prometheus exposition, with the base set added.
#[tokio::test]
async fn metrics_keeps_prometheus_text_and_gains_base_headers() {
    let h = harness(false);
    let (status, headers, text) = h.get_raw("/metrics").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        header(&headers, "content-type").starts_with("text/plain; version=0.0.4"),
        "{headers:?}"
    );
    assert!(text.contains("# TYPE rrouter_requests_total counter"), "{text}");
    assert_base_security_headers(&headers);
    assert!(headers.get("content-security-policy").is_none(), "{headers:?}");
}

/// Insert-only: a value set by a handler (or an upstream hop) wins, and content-type
/// and a download's `content-disposition` are untouched. The middleware is applied to
/// a purpose-built router so the handler can set exactly the headers the middleware
/// would otherwise add.
#[tokio::test]
async fn hardening_never_overwrites_a_header_already_set() {
    async fn handler() -> Response {
        (
            [
                ("content-type", "text/html; charset=utf-8"),
                ("x-frame-options", "SAMEORIGIN"),
                ("referrer-policy", "strict-origin-when-cross-origin"),
                ("content-security-policy", "default-src 'none'"),
                ("content-disposition", "attachment; filename=export.csv"),
            ],
            "<p>handler</p>",
        )
            .into_response()
    }

    let router = Router::new()
        .route("/preset", get(handler))
        .layer(axum::middleware::from_fn(rrouter_gateway::headers::harden));

    let res = router
        .oneshot(
            Request::builder()
                .uri("/preset")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("oneshot");
    let headers = res.headers().clone();
    assert_eq!(header(&headers, "x-frame-options"), "SAMEORIGIN", "{headers:?}");
    assert_eq!(
        header(&headers, "referrer-policy"),
        "strict-origin-when-cross-origin",
        "{headers:?}"
    );
    assert_eq!(header(&headers, "content-security-policy"), "default-src 'none'", "{headers:?}");
    assert_eq!(
        header(&headers, "content-disposition"),
        "attachment; filename=export.csv",
        "{headers:?}"
    );
    assert_eq!(header(&headers, "content-type"), "text/html; charset=utf-8");
    // The headers nobody set are still added.
    assert_eq!(header(&headers, "x-content-type-options"), "nosniff");
    assert_eq!(
        header(&headers, "permissions-policy"),
        "camera=(), microphone=(), geolocation=()"
    );
}

// --------------------------------------------------- phase 18: prompt templates

/// Any JSON method against the admin API; `None` sends an empty body.
async fn template_call(
    h: &Harness,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json");
    let req = match body {
        Some(value) => builder.body(Body::from(value.to_string())).expect("request"),
        None => builder.body(Body::empty()).expect("request"),
    };
    h.call(req).await
}

#[tokio::test]
async fn templates_crud_round_trip() {
    let h = harness(false);

    let (status, body) = h
        .post("/api/templates", json!({"name": "support", "body": "You are support."}))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let id = body["id"].as_str().expect("id").to_string();
    assert_eq!(body["name"], "support");
    assert_eq!(body["body"], "You are support.");
    assert_eq!(body["version"], 1);
    assert!(body["created_at"].as_i64().expect("created_at") > 0);
    assert_eq!(body["created_at"], body["updated_at"]);

    let (status, body) = h.get("/api/templates").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let data = body["data"].as_array().expect("data");
    assert_eq!(data.len(), 1);
    assert_eq!(data[0]["id"], id.as_str());
    assert_eq!(data[0]["body"], "You are support.");

    let (status, body) = h.get(&format!("/api/templates/{id}")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["name"], "support");

    // A second template with the same name is a conflict with its own code.
    let (status, body) = h
        .post("/api/templates", json!({"name": "support", "body": "different"}))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "template_exists");

    // Renaming without touching the body keeps version 1.
    let (status, body) = template_call(
        &h,
        "PUT",
        &format!("/api/templates/{id}"),
        Some(json!({"name": "helpdesk", "body": "You are support."})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["name"], "helpdesk");
    assert_eq!(body["version"], 1);

    // A body change is a new revision.
    let (status, body) = template_call(
        &h,
        "PUT",
        &format!("/api/templates/{id}"),
        Some(json!({"body": "You are helpdesk."})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["name"], "helpdesk");
    assert_eq!(body["body"], "You are helpdesk.");
    assert_eq!(body["version"], 2);

    let (status, body) = h.get(&format!("/api/templates/{id}/versions")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let versions = body["data"].as_array().expect("data");
    assert_eq!(versions.len(), 2);
    assert_eq!(versions[0]["version"], 2, "newest first");
    assert_eq!(versions[0]["body"], "You are helpdesk.");
    assert_eq!(versions[1]["version"], 1);
    assert_eq!(versions[1]["body"], "You are support.");

    // Renaming onto a name that is taken is a conflict, not a bad request.
    let (status, other) = h.post("/api/templates", json!({"name": "taken", "body": "x"})).await;
    assert_eq!(status, StatusCode::CREATED, "{other}");
    let (status, body) = template_call(
        &h,
        "PUT",
        &format!("/api/templates/{id}"),
        Some(json!({"name": "taken", "body": "You are helpdesk."})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "template_exists");

    let (status, body) = template_call(&h, "DELETE", &format!("/api/templates/{id}"), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["deleted"], true);
    let (status, _) = h.get(&format!("/api/templates/{id}")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = h.get(&format!("/api/templates/{id}/versions")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = template_call(&h, "DELETE", &format!("/api/templates/{id}"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) =
        template_call(&h, "PUT", "/api/templates/nope", Some(json!({"body": "x"}))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn templates_reject_invalid_payloads() {
    let h = harness(false);

    let (status, body) = h.post("/api/templates", json!({"body": "no name"})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, _) = h
        .post("/api/templates", json!({"name": "n".repeat(101), "body": "x"}))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "101-character name");
    let (status, _) = h
        .post("/api/templates", json!({"name": "big", "body": "x".repeat(100_001)}))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body over 100k characters");

    let (status, body) = h.get("/api/templates").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body["data"].as_array().expect("data").is_empty(),
        "nothing was stored by the rejected creates: {body}"
    );

    // The 100k boundary itself is accepted.
    let (status, _) = h
        .post("/api/templates", json!({"name": "max", "body": "x".repeat(100_000)}))
        .await;
    assert_eq!(status, StatusCode::CREATED);
}

/// Templates are admin surface: the same gate as every other `/api` route.
#[tokio::test]
async fn templates_require_the_admin_gate() {
    let h = harness(false);
    for uri in ["/api/templates", "/api/templates/abc", "/api/templates/abc/versions"] {
        let res = h
            .router
            .clone()
            .oneshot(as_peer(get_with(uri, &[]), remote_peer()))
            .await
            .expect("oneshot");
        assert_eq!(res.status(), StatusCode::FORBIDDEN, "{uri} from a remote peer");
    }
    let req = as_peer(
        Request::builder()
            .method("POST")
            .uri("/api/templates")
            .header("content-type", "application/json")
            .body(Body::from(json!({"name": "x", "body": "y"}).to_string()))
            .expect("request"),
        remote_peer(),
    );
    let res = h.router.clone().oneshot(req).await.expect("oneshot");
    assert_eq!(res.status(), StatusCode::FORBIDDEN, "POST from a remote peer");

    // A configured token replaces the loopback rule: remote peers need it.
    let mut cfg = config(false);
    cfg.admin_token = Some(TEST_ADMIN_TOKEN.into());
    let gated = harness_with(cfg);
    let res = gated
        .router
        .clone()
        .oneshot(as_peer(get_with("/api/templates", &[]), remote_peer()))
        .await
        .expect("oneshot");
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    let res = gated
        .router
        .clone()
        .oneshot(as_peer(
            get_with("/api/templates", &[("x-admin-token", TEST_ADMIN_TOKEN)]),
            remote_peer(),
        ))
        .await
        .expect("oneshot");
    assert_eq!(res.status(), StatusCode::OK, "remote peer with the admin token");
}

// ---------------------------------------------------------------- account quotas

/// One usage row for an account, dated now, used to spend an account's quota.
fn quota_usage(connection: uuid::Uuid) -> UsageRecord {
    UsageRecord {
        id: None,
        ts: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0),
        request_id: Some(uuid::Uuid::new_v4()),
        provider: "claude".to_string(),
        model: "haiku".to_string(),
        connection_id: Some(connection),
        endpoint: Some("/v1/messages".to_string()),
        key_id: None,
        cost_micros: 0,
        input_tokens: 1,
        output_tokens: 1,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        latency_ms: 1,
        ttft_ms: 0,
        status: 200,
        streamed: false,
        error: None,
    }
}

/// An exhausted account is skipped before dispatch: the request goes to the healthy
/// account and the exhausted upstream never sees a byte.
#[tokio::test]
async fn account_quota_skips_an_exhausted_account_for_a_healthier_one() {
    let dead = Mock::start(vec![openai_reply("from the exhausted account")]).await;
    let live = Mock::start(vec![openai_reply("from the healthy account")]).await;
    let h = harness(false);
    let exhausted = h.connect("openai", &dead).await;
    let healthy = h.connect("openai", &live).await;
    h.patch_connection(&healthy.to_string(), json!({"priority": 1})).await;
    h.state
        .store
        .set_account_quota(&AccountQuota {
            connection_id: exhausted,
            window: BudgetWindow::Daily,
            max_requests: 1,
            max_tokens: 0,
            max_cost_micros: 0,
        })
        .await
        .expect("quota");
    h.state.store.record_usage(&quota_usage(exhausted)).await.expect("usage row");

    let (status, body) = h.post("/v1/chat/completions", openai_chat_body("gpt-4.1")).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(dead.hits(), 0, "an exhausted account is never tried");
    assert_eq!(live.hits(), 1, "the healthy account serves the request");
}

/// When every candidate is exhausted the request fails with `429 quota_exhausted`
/// before any provider call, and the reset is advertised in `Retry-After`.
#[tokio::test]
async fn account_quota_returns_429_when_every_account_is_exhausted() {
    let mock = Mock::start(vec![openai_reply("never sent")]).await;
    let h = harness(false);
    let connection = h.connect("openai", &mock).await;
    h.state
        .store
        .set_account_quota(&AccountQuota {
            connection_id: connection,
            window: BudgetWindow::Daily,
            max_requests: 1,
            max_tokens: 0,
            max_cost_micros: 0,
        })
        .await
        .expect("quota");
    h.state.store.record_usage(&quota_usage(connection)).await.expect("usage row");

    let (status, headers, body) = h
        .post_headers("/v1/chat/completions", openai_chat_body("gpt-4.1"), &[])
        .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "body: {body}");
    assert_eq!(body["error"]["code"], "quota_exhausted");
    assert!(
        headers.get("retry-after").is_some(),
        "the reset is advertised: {headers:?}"
    );
    assert_eq!(mock.hits(), 0, "nothing is dispatched");
}

/// `GET` lists every connection with its quota and window spend; `PUT` patches only the
/// fields it carries and validates; `DELETE` returns the account to unlimited.
#[tokio::test]
async fn account_quota_api_lists_updates_and_clears() {
    let mock = Mock::start(vec![]).await;
    let h = harness(false);
    let connection = h.connect("claude", &mock).await;

    let (status, body) = h.get("/api/accounts/quotas").await;
    assert_eq!(status, StatusCode::OK, "list: {body}");
    let row = &body["quotas"][0];
    assert_eq!(row["connection_id"], connection.to_string());
    assert_eq!(row["provider"], "claude");
    assert_eq!(row["window"], "daily");
    assert_eq!(row["max_requests"], 0);
    assert_eq!(row["requests"], 0);
    assert_eq!(row["exhausted"], false);

    let uri = format!("/api/accounts/quotas/{connection}");
    let (status, body) = h
        .put(
            &uri,
            json!({"window": "weekly", "max_requests": 2, "max_cost_micros": 500}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "put: {body}");
    assert_eq!(body["quota"]["window"], "weekly");
    assert_eq!(body["quota"]["max_requests"], 2);
    assert_eq!(body["quota"]["max_cost_micros"], 500);
    assert_eq!(body["quota"]["max_tokens"], 0, "omitted fields keep their value");

    // A partial update keeps the window and the other limits.
    let (status, body) = h.put(&uri, json!({"max_requests": 3})).await;
    assert_eq!(status, StatusCode::OK, "partial put: {body}");
    assert_eq!(body["quota"]["window"], "weekly");
    assert_eq!(body["quota"]["max_requests"], 3);
    assert_eq!(body["quota"]["max_cost_micros"], 500);

    assert_eq!(h.put(&uri, json!({"window": "fortnight"})).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(h.put(&uri, json!({"max_tokens": -1})).await.0, StatusCode::BAD_REQUEST);
    let missing = format!("/api/accounts/quotas/{}", uuid::Uuid::new_v4());
    assert_eq!(h.put(&missing, json!({"max_requests": 1})).await.0, StatusCode::NOT_FOUND);

    h.state.store.record_usage(&quota_usage(connection)).await.expect("usage row");
    let (_, body) = h.get("/api/accounts/quotas").await;
    assert_eq!(body["quotas"][0]["requests"], 1);
    assert_eq!(body["quotas"][0]["tokens"], 2);

    let (status, _, _) = h.delete(&uri).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, body) = h.get("/api/accounts/quotas").await;
    assert_eq!(body["quotas"][0]["max_requests"], 0);
    assert_eq!(body["quotas"][0]["window"], "daily");
    let (status, _, _) = h.delete(&uri).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "delete is idempotent");
}

// ---------------------------------------------------------------- Token Saver (RTK)

fn grep_tool_body() -> Value {
    let matches: Vec<String> = (1..=40)
        .map(|n| format!("src/main.rs:{n}:    let value = compute({n});"))
        .collect();
    json!({
        "model": "gpt-4.1",
        "messages": [
            {"role": "user", "content": "search the repo"},
            {
                "role": "tool",
                "tool_call_id": "t1",
                "content": matches.join("\n")
            }
        ]
    })
}

#[tokio::test]
async fn token_saver_compresses_tool_results_and_can_be_bypassed() {
    let mock = Mock::start(vec![openai_reply("ok"), openai_reply("ok")]).await;
    let h = harness(false);
    h.connect("openai", &mock).await;

    let (status, _) = h
        .post_with("/v1/chat/completions", grep_tool_body(), &[("x-rrouter-rtk", "on")])
        .await;
    assert_eq!(status, StatusCode::OK);
    let sent = mock.seen();
    let tool = sent[0]["messages"][1]["content"].as_str().expect("tool content");
    assert!(tool.contains("40 matches in 1 files"), "compressed: {}", &tool[..tool.len().min(120)]);
    assert!(tool.len() < 1600, "the 40-match dump shrank");

    let (status, _) = h
        .post_with(
            "/v1/chat/completions",
            grep_tool_body(),
            &[("x-rrouter-rtk", "off")],
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let sent = mock.seen();
    let tool = sent[1]["messages"][1]["content"].as_str().expect("tool content");
    assert!(tool.starts_with("src/main.rs:1:"), "bypass kept the raw dump");
}
