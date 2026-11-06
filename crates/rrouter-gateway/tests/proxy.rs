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
use axum::routing::post;
use axum::Router;
use bytes::Bytes;
use http_body_util::BodyExt;
use rrouter_auth::Credentials;
use rrouter_gateway::{app, config::Config, AppState};
use rrouter_router::{Breaker, Catalog, Dispatcher, Planner};
use rrouter_store::{Store, UsageRecord};
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
    plan: Mutex<Vec<MockReply>>,
    url: String,
}

#[derive(Clone)]
enum MockReply {
    /// JSON body + status (non-streaming).
    Json(u16, Value),
    /// Bytes relayed exactly as sent, for bodies the gateway must not rewrite.
    Raw(u16, &'static str),
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
}

async fn handle(State(mock): State<Mock>, req: Request<Body>) -> Response {
    mock.inner.hits.fetch_add(1, Ordering::SeqCst);
    let path = req.uri().path().to_string();
    let bytes = req.into_body().collect().await.expect("body").to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    mock.inner
        .seen
        .lock()
        .expect("seen")
        .push((path, body));
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
        shutdown_grace_secs: 20,
        strategy: rrouter_router::Strategy::Priority,
    }
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

    async fn post(&self, uri: &str, body: Value) -> (StatusCode, Value) {
        self.post_with(uri, body, &[]).await
    }

    async fn post_with(&self, uri: &str, body: Value, headers: &[(&str, &str)]) -> (StatusCode, Value) {
        let mut builder = Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json");
        for (k, v) in headers {
            builder = builder.header(*k, *v);
        }
        self.call(builder.body(Body::from(body.to_string())).expect("request")).await
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
    assert_eq!(attempts.len(), 2, "one row per candidate: {detail}");
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
        if let Ok(rows) = h.state.store.recent_usage(5, None).await {
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
        if let Ok(rows) = h.state.store.recent_usage(5, None).await {
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
    assert_eq!(created["rpm"], 0);
    assert_eq!(created["tpm"], 0);
    assert_eq!(created["budget_tokens"], 0);
    assert_eq!(created["spent_tokens"], 0);
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
        "rpm": 60,
        "tpm": 90_000,
        "budget_tokens": 5_000_000,
        "spent_tokens": 0,
        "allowed_models": ["claude/claude-haiku-4-5-20251001"],
        "expires_at": null,
        "models": [],
    });
    assert_eq!(patched, expected);

    // The list endpoint answers the same row.
    let (_, body) = h.get("/api/keys").await;
    assert_eq!(body["data"][0], expected);

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
        if let Ok(rows) = h.state.store.recent_usage(5, None).await {
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
    let rows = h.state.store.recent_usage(10, None).await.expect("usage");
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
        && h.state.store.recent_usage(10, None).await.expect("usage").is_empty()
    {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // Give the (unwritten) count_tokens row a chance to appear before asserting absence.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let rows = h.state.store.recent_usage(10, None).await.expect("usage");
    assert_eq!(rows.len(), 1, "count_tokens reported no usage: {rows:?}");
    assert_eq!(rows[0].endpoint.as_deref(), Some("/v1/responses/compact"));
    assert_eq!(rows[0].input_tokens, 11);
    assert_eq!(rows[0].output_tokens, 0);
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
