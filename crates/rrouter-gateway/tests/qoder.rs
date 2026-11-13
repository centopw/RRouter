//! End-to-end Qoder transport tests.
//!
//! These drive the real proxy against a mock that speaks Qoder's wire: an obfuscated,
//! COSY-signed POST to `/algo/api/v2/service/pro/sse/agent_chat_generation?…&Encode=1`, a
//! signed `GET /algo/api/v2/model/list`, and a `{statusCodeValue, body}` SSE answer.
//!
//! The mock does not hold the RSA private key (that round-trip is proven in
//! `rrouter-auth/tests/qoder_cosy.rs`); it asserts the signing contract the gateway owes
//! instead — a fresh COSY signature on every attempt, a body that de-obfuscates to the
//! envelope, and a `Cosy-Bodyhash` that covers the bytes actually sent.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::State;
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Router;
use http_body_util::BodyExt;
use md5::Digest as _;
use rrouter_auth::Credentials;
use rrouter_gateway::{app, config::Config, AppState};
use rrouter_router::{Breaker, Catalog, Dispatcher, Planner};
use rrouter_store::Store;
use serde_json::{json, Value};
use tower::ServiceExt;

const PROVIDER: &str = "qoder";
const USER_ID: &str = "user-42";
const TOKEN: &str = "dt-test-token";
const CHAT_PATH: &str = "/algo/api/v2/service/pro/sse/agent_chat_generation";

// ---------------------------------------------------------------- mock upstream

#[derive(Clone)]
struct Mock {
    url: String,
    inner: Arc<MockInner>,
}

struct MockInner {
    catalog_hits: AtomicUsize,
    chat_hits: AtomicUsize,
    calls: Mutex<Vec<Call>>,
    replies: Mutex<VecDeque<Reply>>,
}

/// One inference call, already de-obfuscated.
#[derive(Debug, Clone)]
struct Call {
    uri: String,
    headers: Vec<(String, String)>,
    body: Value,
    /// `Cosy-Bodyhash` as recomputed over the bytes that arrived.
    body_hash: String,
    /// The raw encoded bytes, so a test can prove the signature covered them.
    raw_len: usize,
}

#[derive(Clone)]
enum Reply {
    /// Code `10605`: the model is momentarily out of capacity.
    Queued,
    /// Code `112`: the account is out of quota.
    Quota,
    Sse(String),
}

impl Mock {
    async fn start(replies: Vec<Reply>) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock");
        let addr = listener.local_addr().expect("addr");
        let mock = Mock {
            url: format!("http://{addr}"),
            inner: Arc::new(MockInner {
                catalog_hits: AtomicUsize::new(0),
                chat_hits: AtomicUsize::new(0),
                calls: Mutex::new(Vec::new()),
                replies: Mutex::new(replies.into()),
            }),
        };
        let router = Router::new().fallback(handle).with_state(mock.clone());
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        mock
    }

    fn url(&self) -> String {
        self.url.clone()
    }

    fn catalog_hits(&self) -> usize {
        self.inner.catalog_hits.load(Ordering::SeqCst)
    }

    fn chat_hits(&self) -> usize {
        self.inner.chat_hits.load(Ordering::SeqCst)
    }

    fn calls(&self) -> Vec<Call> {
        self.inner.calls.lock().expect("calls").clone()
    }
}

async fn handle(State(mock): State<Mock>, req: Request<Body>) -> Response {
    let uri = req.uri().to_string();
    let headers: Vec<(String, String)> = req
        .headers()
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_ascii_lowercase(),
                value.to_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    let bytes = req.into_body().collect().await.expect("body").to_bytes();

    if uri.starts_with("/algo/api/v2/model/list") {
        mock.inner.catalog_hits.fetch_add(1, Ordering::SeqCst);
        return axum::Json(catalog_body()).into_response();
    }
    if !uri.starts_with(CHAT_PATH) {
        return (StatusCode::NOT_FOUND, "not a qoder path").into_response();
    }

    mock.inner.chat_hits.fetch_add(1, Ordering::SeqCst);
    // The body is obfuscated: nothing standard survives base64 or JSON until it is undone.
    let decoded = match rrouter_auth::qoder::encoding::decode_body(&bytes) {
        Ok(decoded) => decoded,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                format!("undecodable body: {error}"),
            )
                .into_response()
        }
    };
    let body: Value = match serde_json::from_slice(&decoded) {
        Ok(body) => body,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                format!("body is not json: {error}"),
            )
                .into_response()
        }
    };
    let body_hash = md5::Md5::digest(&bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    mock.inner.calls.lock().expect("calls").push(Call {
        uri,
        headers,
        body,
        body_hash,
        raw_len: bytes.len(),
    });

    let reply = mock
        .inner
        .replies
        .lock()
        .expect("replies")
        .pop_front()
        .unwrap_or(Reply::Sse(completion_sse("never queued")));
    match reply {
        Reply::Sse(body) => sse(body),
        Reply::Queued => envelope_error(json!({
            "code": "10605",
            "message": "{\"isQueued\":true,\"queueType\":\"p3\",\"retryAfterSeconds\":5,\"subCode\":\"RESOURCE_LIMIT\"}",
        })),
        Reply::Quota => envelope_error(json!({
            "code": "112",
            "pricingUrl": "https://qoder.com/pricing",
            "message": "quota exhausted",
        })),
    }
}

fn catalog_body() -> Value {
    json!({"chat": [
        {
            "key": "auto",
            "display_name": "Auto",
            "format": "openai",
            "source": "system",
            "is_reasoning": false,
            "max_input_tokens": 200000,
            "max_output_tokens": 32768,
        },
        {
            "key": "qmodel",
            "display_name": "Qmodel",
            "format": "openai",
            "source": "system",
            "is_reasoning": true,
            "max_input_tokens": 200000,
            "max_output_tokens": 16384,
        },
    ]})
}

/// One unwrapped OpenAI chunk inside Qoder's envelope.
fn frame(inner: Value) -> String {
    format!(
        "data: {}\n\n",
        json!({"statusCodeValue": 200, "body": inner.to_string()})
    )
}

fn completion_sse(text: &str) -> String {
    [
        frame(json!({
            "id": "chatcmpl-q",
            "model": "auto",
            "choices": [{"index": 0, "delta": {"content": text}}],
        })),
        // Heartbeats are frequent and carry nothing.
        ": keepalive\n\n".to_string(),
        "data:\n\n".to_string(),
        frame(json!({
            "id": "chatcmpl-q",
            "model": "auto",
            "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 9, "completion_tokens": 3, "total_tokens": 12},
        })),
        "data: [DONE]\n\n".to_string(),
    ]
    .concat()
}

fn sse(body: String) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/event-stream")
        .body(Body::from(body))
        .expect("sse")
}

/// An error envelope: HTTP 200, the failure lives inside the first frame.
fn envelope_error(body: Value) -> Response {
    sse(format!(
        "data: {}\n\n",
        json!({"statusCodeValue": 500, "body": body.to_string()})
    ))
}

// ---------------------------------------------------------------- harness

struct Harness {
    state: Arc<AppState>,
    router: Router,
}

fn config(cache: bool) -> Config {
    Config {
        host: "127.0.0.1".parse().expect("ip"),
        port: 0,
        data_dir: std::env::temp_dir().join(format!("rrouter-qoder-{}", uuid::Uuid::new_v4())),
        master_key: None,
        require_api_key: false,
        request_timeout_secs: 30,
        connect_timeout_secs: 5,
        log_filter: "rrouter=error".into(),
        static_dir: None,
        cors_origins: vec![],
        allow_remote_admin: false,
        admin_token: None,
        max_concurrency: 0,
        max_attempts: 2,
        retry_attempts: 1,
        retry_base_ms: 5,
        retry_max_ms: 20,
        shutdown_grace_secs: 20,
        strategy: rrouter_router::Strategy::Priority,
        cache: if cache {
            rrouter_gateway::config::CacheMode::Sqlite
        } else {
            rrouter_gateway::config::CacheMode::Off
        },
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

fn harness(cache: bool) -> Harness {
    let config = config(cache);
    let data_dir = config.data_dir.clone();
    let state = Arc::new(AppState {
        store: Arc::new(Store::open_in_memory().expect("store")),
        catalog: Arc::new(Catalog::builtin()),
        refresher: Arc::new(rrouter_auth::Refresher::new(
            reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .expect("client"),
        )),
        planner: Planner::new(),
        dispatcher: Arc::new(Dispatcher::new(Breaker::new()).with_max_attempts(2)),
        http: reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .expect("client"),
        config: rrouter_gateway::config::ConfigSnapshot::from(config),
        started: Instant::now(),
        oauth: rrouter_auth::OAuthFlows::new(),
        counters: Default::default(),
        envelope_drops: Default::default(),
        concurrency: std::sync::RwLock::new(None),
        auth: rrouter_gateway::auth::Auth::load(&data_dir, false),
    });
    let router = app(Arc::clone(&state));
    Harness { state, router }
}

impl Harness {
    async fn connect(&self, mock: &Mock) {
        self.connect_with_creds(mock, "user_id", USER_ID).await;
    }

    async fn connect_with_creds(&self, mock: &Mock, key: &str, value: &str) {
        let mut extra = serde_json::Map::new();
        extra.insert(key.into(), Value::from(value));
        self.state
            .store
            .create_connection(
                PROVIDER,
                "api_key",
                Credentials {
                    api_key: Some(TOKEN.into()),
                    base_url_override: Some(mock.url()),
                    extra,
                    ..Credentials::default()
                },
                None,
            )
            .await
            .expect("connection");
    }

    async fn post(&self, body: Value) -> (StatusCode, Value) {
        self.post_route("/v1/chat/completions", body).await
    }

    async fn post_route(&self, uri: &str, body: Value) -> (StatusCode, Value) {
        let response = self
            .router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(uri)
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .expect("request"),
            )
            .await
            .expect("response");
        let status = response.status();
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    async fn post_stream(&self, body: Value) -> (StatusCode, String) {
        let response = self
            .router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .expect("request"),
            )
            .await
            .expect("response");
        let status = response.status();
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes();
        (status, String::from_utf8_lossy(&bytes).to_string())
    }
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> &'a str {
    headers
        .iter()
        .find(|(header, _)| header == name)
        .map(|(_, value)| value.as_str())
        .unwrap_or_default()
}

fn chat_body(model: &str, stream: bool) -> Value {
    json!({
        "model": format!("{PROVIDER}/{model}"),
        "stream": stream,
        "messages": [
            {"role": "system", "content": "be terse"},
            {"role": "user", "content": "say hi"},
        ],
        "max_tokens": 64,
        "tools": [{"type": "function", "function": {
            "name": "get_time",
            "parameters": {"type": "object", "properties": {}}
        }}],
    })
}

// ---------------------------------------------------------------- tests

#[tokio::test]
async fn a_signed_envelope_is_sent_and_its_envelope_is_unwrapped() {
    let mock = Mock::start(vec![Reply::Sse(completion_sse("hi there"))]).await;
    let h = harness(false);
    h.connect(&mock).await;

    let (status, body) = h.post(chat_body("auto", false)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["choices"][0]["message"]["content"], "hi there");
    assert_eq!(body["usage"]["prompt_tokens"], 9);

    assert_eq!(mock.catalog_hits(), 1, "the catalog is fetched once");
    let calls = mock.calls();
    assert_eq!(calls.len(), 1, "one inference call per request");
    let call = &calls[0];
    assert!(
        call.uri.starts_with(CHAT_PATH) && call.uri.contains("Encode=1"),
        "the endpoint carries Encode=1: {}",
        call.uri
    );

    // COSY header contract.
    assert!(
        header(&call.headers, "authorization").starts_with("Bearer COSY."),
        "authorization: {}",
        header(&call.headers, "authorization")
    );
    assert_eq!(header(&call.headers, "cosy-user"), USER_ID);
    assert_eq!(
        header(&call.headers, "cosy-sigpath"),
        "/api/v2/service/pro/sse/agent_chat_generation"
    );
    assert_eq!(
        header(&call.headers, "cosy-bodylength"),
        call.raw_len.to_string()
    );
    assert_eq!(header(&call.headers, "cosy-bodyhash"), call.body_hash);
    assert!(
        !header(&call.headers, "cosy-key").is_empty(),
        "the AES key is wrapped"
    );
    assert_eq!(header(&call.headers, "cosy-version"), "1.0.0");
    assert_eq!(header(&call.headers, "accept-encoding"), "identity");
    assert_eq!(header(&call.headers, "x-model-key"), "auto");
    assert!(
        header(&call.headers, "authorization").ne(TOKEN),
        "the device token never travels in the clear"
    );
    let seen: Vec<String> = call
        .headers
        .iter()
        .map(|(_, value)| value.clone())
        .collect();
    assert!(
        !seen.iter().any(|value| value.contains(TOKEN)),
        "the token only lives inside the COSY blob: {seen:?}"
    );

    // Envelope contract.
    let sent = &call.body;
    assert_eq!(sent["chat_task"], "FREE_INPUT");
    assert_eq!(sent["session_type"], "qodercli");
    assert_eq!(sent["stream"], true);
    assert_eq!(sent["system"], "be terse");
    assert_eq!(sent["messages"][0]["role"], "user");
    assert!(
        sent["messages"]
            .as_array()
            .expect("messages")
            .iter()
            .all(|message| message["role"] != "system"),
        "system is hoisted out of messages: {sent}"
    );
    assert_eq!(sent["parameters"]["max_tokens"], 64);
    assert_eq!(sent["model_config"]["key"], "auto");
    assert_eq!(sent["chat_context"]["text"], "say hi");
    assert_eq!(sent["chat_context"]["extra"]["modelConfig"]["key"], "auto");
    assert_eq!(sent["business"]["product"], "cli");
    assert_eq!(sent["business"]["name"], "say hi");
    assert_eq!(sent["tools"][0]["function"]["name"], "get_time");
    assert!(
        sent["session_id"].as_str().is_some_and(|id| id.len() == 16),
        "session id: {sent}"
    );
    assert_eq!(sent["request_set_id"], sent["chat_record_id"]);
}

#[tokio::test]
async fn a_streaming_client_receives_the_unwrapped_chunks() {
    let mock = Mock::start(vec![Reply::Sse(completion_sse("streamed"))]).await;
    let h = harness(false);
    h.connect(&mock).await;

    let (status, body) = h.post_stream(chat_body("auto", true)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(body.contains("data: "), "an SSE stream comes back: {body}");
    assert!(
        body.contains("\"streamed\""),
        "content is unwrapped: {body}"
    );
    assert!(
        body.trim_end().ends_with("[DONE]"),
        "terminal marker: {body}"
    );
    assert!(
        !body.contains("statusCodeValue"),
        "the Qoder envelope never reaches the client: {body}"
    );
    assert_eq!(mock.chat_hits(), 1);
}

#[tokio::test]
async fn a_model_the_account_does_not_list_is_refused_without_sending() {
    let mock = Mock::start(vec![Reply::Sse(completion_sse("never sent"))]).await;
    let h = harness(false);
    h.connect(&mock).await;

    let (status, body) = h.post(chat_body("nope", false)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");
    let message = body["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("does not list model"),
        "the refusal names the catalog: {message}"
    );
    assert_eq!(mock.chat_hits(), 0, "nothing was sent to inference");
}

#[tokio::test]
async fn a_queued_model_is_retried_with_a_fresh_signature() {
    let mock = Mock::start(vec![
        Reply::Queued,
        Reply::Sse(completion_sse("after the queue")),
    ])
    .await;
    let h = harness(false);
    h.connect(&mock).await;

    let (status, body) = h.post(chat_body("auto", false)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["choices"][0]["message"]["content"], "after the queue");

    let calls = mock.calls();
    assert_eq!(calls.len(), 2, "the queued attempt was replayed: {calls:?}");
    assert_ne!(
        header(&calls[0].headers, "authorization"),
        header(&calls[1].headers, "authorization"),
        "the signature covers the body bytes, so a retry is re-signed"
    );
    assert_ne!(
        calls[0].body["request_id"], calls[1].body["request_id"],
        "each attempt carries its own request id"
    );
    assert_eq!(
        calls[0].body["request_set_id"], calls[1].body["request_set_id"],
        "the queue is keyed by request_set_id, so it is stable across attempts"
    );
    assert_eq!(calls[0].body["session_id"], calls[1].body["session_id"]);
    // A retry re-signs; it does not re-fetch the account catalog.
    assert_eq!(mock.catalog_hits(), 1);
}

#[tokio::test]
async fn a_quota_block_cools_the_account_down() {
    let mock = Mock::start(vec![Reply::Quota]).await;
    let h = harness(false);
    h.connect(&mock).await;

    let (status, body) = h.post(chat_body("auto", false)).await;
    assert_eq!(
        status,
        StatusCode::TOO_MANY_REQUESTS,
        "a quota block is the gateway's quota-exhausted path: {body}"
    );
    assert_eq!(
        mock.chat_hits(),
        1,
        "a 429 is not retried on the same account"
    );
}

#[tokio::test]
async fn qoder_requests_are_never_cached() {
    let mock = Mock::start(vec![
        Reply::Sse(completion_sse("first")),
        Reply::Sse(completion_sse("second")),
    ])
    .await;
    // The cache is on: a cacheable provider would answer the second call itself.
    let h = harness(true);
    h.connect(&mock).await;

    let (first_status, first) = h.post(chat_body("auto", false)).await;
    assert_eq!(first_status, StatusCode::OK);
    assert_eq!(first["choices"][0]["message"]["content"], "first");
    let (second_status, second) = h.post(chat_body("auto", false)).await;
    assert_eq!(second_status, StatusCode::OK);
    assert_eq!(
        second["choices"][0]["message"]["content"], "second",
        "the upstream answered again, so nothing was served from cache"
    );
    assert_eq!(mock.chat_hits(), 2);
}

#[tokio::test]
async fn a_connection_without_a_user_id_fails_before_signing() {
    let mock = Mock::start(vec![Reply::Sse(completion_sse("never sent"))]).await;
    let h = harness(false);
    // `machine_id` is optional, `user_id` is not: COSY cannot be signed without it.
    h.connect_with_creds(&mock, "machine_id", "11111111-2222-5333-a444-555555555555")
        .await;

    let (status, body) = h.post(chat_body("auto", false)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "body: {body}");
    assert_eq!(mock.chat_hits(), 0);
    assert_eq!(mock.catalog_hits(), 0);
}

#[tokio::test]
async fn the_catalog_can_be_fetched_into_upstream_rows() {
    let mock = Mock::start(vec![Reply::Sse(completion_sse("never sent"))]).await;
    let h = harness(false);
    h.connect(&mock).await;
    let connection = h
        .state
        .store
        .list_connections(Some(PROVIDER))
        .await
        .expect("connections")
        .into_iter()
        .next()
        .expect("qoder connection");

    let (status, body) = h
        .post_route(
            "/api/models/fetch",
            json!({"connection_id": connection.id.to_string()}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["provider"], PROVIDER);
    assert_eq!(body["count"], 2);
    assert_eq!(body["added"], 2);

    // The stored rows are what routing needs to resolve `qoder/<key>`.
    let rows: Vec<String> = h
        .state
        .store
        .list_models()
        .await
        .expect("models")
        .into_iter()
        .filter(|row| row.provider == PROVIDER)
        .map(|row| row.model)
        .collect();
    assert!(rows.contains(&"auto".to_string()), "rows: {rows:?}");
    assert!(rows.contains(&"qmodel".to_string()), "rows: {rows:?}");
    assert_eq!(mock.chat_hits(), 0, "listing models sends no completion");
}
