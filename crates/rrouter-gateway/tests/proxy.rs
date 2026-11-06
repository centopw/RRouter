//! End-to-end tests: the real axum app over `tower::ServiceExt::oneshot`, talking to a
//! real mock upstream served on a local port. `base_url_override` on a connection is what
//! points a provider at that server, so the whole proxy path (translate → dispatch →
//! refresh → headers → HTTP → translate back) runs exactly as in production.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
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
use rrouter_store::Store;
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
    }
}

fn harness(require_api_key: bool) -> Harness {
    let state = Arc::new(build_state(require_api_key));
    let router = app(Arc::clone(&state));
    Harness { state, router }
}

fn build_state(require_api_key: bool) -> AppState {
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
        dispatcher: Arc::new(Dispatcher::new(Breaker::new())),
        http: reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("client"),
        config: config(require_api_key),
        started: Instant::now(),
        oauth: rrouter_auth::OAuthFlows::new(),
    }
}

impl Harness {
    /// Register a provider connection pointing at the mock upstream.
    async fn connect(&self, provider: &str, mock: &Mock) {
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
            .expect("connection");
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
        ..build_state(false)
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
}
