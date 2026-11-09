//! End-to-end Google Code Assist (Antigravity) transport tests.
//!
//! These exercise the real proxy against a mock CCA server: envelope, headers, two client
//! grammars, streaming, the production-host fallback and the missing-project refusal.
//!
//! The auth registry entry (`google-antigravity`) is Wave 15B's slice; these tests assert
//! the transport contract it registers.

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
use rrouter_auth::Credentials;
use rrouter_gateway::{app, config::Config, AppState};
use rrouter_router::{Breaker, Catalog, Dispatcher, Planner};
use rrouter_store::Store;
use serde_json::{json, Value};
use tower::ServiceExt;

const PROVIDER: &str = "google-antigravity";
const PROJECT: &str = "proj-test";

// ---------------------------------------------------------------- mock upstream

#[derive(Clone)]
struct Mock {
    url: String,
    inner: Arc<MockInner>,
}

struct MockInner {
    hits: AtomicUsize,
    calls: Mutex<Vec<Call>>,
    replies: Mutex<VecDeque<Reply>>,
}

struct Call {
    uri: String,
    headers: Vec<(String, String)>,
    body: Value,
}

enum Reply {
    Json(u16, Value),
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
                hits: AtomicUsize::new(0),
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

    fn hits(&self) -> usize {
        self.inner.hits.load(Ordering::SeqCst)
    }

    fn calls(&self) -> Vec<Call> {
        let calls = self.inner.calls.lock().expect("calls");
        calls
            .iter()
            .map(|call| Call {
                uri: call.uri.clone(),
                headers: call.headers.clone(),
                body: call.body.clone(),
            })
            .collect()
    }
}

async fn handle(State(mock): State<Mock>, req: Request<Body>) -> Response {
    mock.inner.hits.fetch_add(1, Ordering::SeqCst);
    let uri = req.uri().to_string();
    let headers = req
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
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    mock.inner
        .calls
        .lock()
        .expect("calls")
        .push(Call { uri, headers, body });
    let reply = {
        let mut replies = mock.inner.replies.lock().expect("replies");
        replies.pop_front().unwrap_or(Reply::Json(
            500,
            json!({"error": {"message": "no reply queued"}}),
        ))
    };
    match reply {
        Reply::Json(status, value) => {
            let status = StatusCode::from_u16(status).unwrap_or(StatusCode::OK);
            (status, axum::Json(value)).into_response()
        }
        Reply::Sse(body) => Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "text/event-stream")
            .body(Body::from(body))
            .expect("sse"),
    }
}

// ---------------------------------------------------------------- harness

struct Harness {
    state: Arc<AppState>,
    router: Router,
}

fn config() -> Config {
    Config {
        host: "127.0.0.1".parse().expect("ip"),
        port: 0,
        data_dir: std::env::temp_dir().join(format!("rrouter-cca-{}", uuid::Uuid::new_v4())),
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
        cache: rrouter_gateway::config::CacheMode::Off,
        cache_ttl_secs: 300,
        cache_max_entries: 1000,
        usage_retention_days: 0,
        log_format: rrouter_gateway::config::LogFormat::Text,
        access_log: false,
        hedge_ms: 0,
        canary: None,
        semantic_cache: rrouter_gateway::config::CacheMode::Off,
        semantic_cache_threshold: 0.92,
        semantic_embedding_model: None,
        semantic_cache_ttl_secs: 3600,
        semantic_cache_max_entries: 1000,
    }
}

fn harness() -> Harness {
    let config = config();
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
        concurrency: std::sync::RwLock::new(None),
        auth: rrouter_gateway::auth::Auth::load(&data_dir, false),
    });
    let router = app(Arc::clone(&state));
    Harness { state, router }
}

impl Harness {
    async fn connect(&self, mock: &Mock, project: &str) {
        let mut extra = serde_json::Map::new();
        if !project.is_empty() {
            extra.insert("project_id".into(), Value::from(project));
        }
        self.connect_with_creds(
            mock,
            Credentials {
                access_token: Some("antigravity-access".into()),
                extra,
                ..Credentials::default()
            },
        )
        .await;
    }

    async fn connect_with_creds(&self, mock: &Mock, creds: Credentials) {
        self.state
            .store
            .create_connection(
                PROVIDER,
                "oauth",
                Credentials {
                    base_url_override: Some(mock.url()),
                    ..creds
                },
                Some("antigravity"),
            )
            .await
            .expect("connection");
    }

    async fn post(&self, uri: &str, body: Value) -> (StatusCode, Value) {
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

    async fn post_stream(&self, uri: &str, body: Value) -> (StatusCode, String) {
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
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> &'a str {
    headers
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
        .unwrap_or_default()
}

fn cca_response(text: &str, prompt_tokens: u32, output_tokens: u32) -> Value {
    json!({
        "response": {
            "responseId": "resp-cca",
            "modelVersion": "gemini-3-pro",
            "candidates": [{
                "content": {"role": "model", "parts": [{"text": text}]},
                "finishReason": "STOP"
            }],
            "usageMetadata": {
                "promptTokenCount": prompt_tokens,
                "candidatesTokenCount": output_tokens
            }
        },
        "traceId": "trace-cca"
    })
}

// ---------------------------------------------------------------- tests

#[tokio::test]
async fn openai_client_gets_cca_text_tools_usage_and_the_envelope() {
    let mock = Mock::start(vec![Reply::Json(
        200,
        json!({"response": {
            "responseId": "resp-1",
            "modelVersion": "gemini-3-pro",
            "candidates": [{
                "content": {"parts": [
                    {"text": "18C"},
                    {"functionCall": {"name": "get_weather", "args": {"city": "Paris"}}}
                ]},
                "finishReason": "STOP"
            }],
            "usageMetadata": {"promptTokenCount": 11, "candidatesTokenCount": 7}
        }, "traceId": "t-1"}),
    )])
    .await;
    let h = harness();
    h.connect(&mock, PROJECT).await;

    let (status, body) = h
        .post(
            "/v1/chat/completions",
            json!({
                "model": format!("{PROVIDER}/gemini-3-pro"),
                "messages": [{"role": "user", "content": "weather in Paris?"}],
                "tools": [{"type": "function", "function": {
                    "name": "get_weather",
                    "parameters": {"type": "object", "properties": {"city": {"type": "string"}}}
                }}]
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["choices"][0]["message"]["content"], "18C");
    assert_eq!(
        body["choices"][0]["message"]["tool_calls"][0]["function"]["name"],
        "get_weather"
    );
    assert_eq!(
        body["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"],
        "{\"city\":\"Paris\"}"
    );
    assert_eq!(body["usage"]["prompt_tokens"], 11);
    assert_eq!(body["usage"]["completion_tokens"], 7);

    let calls = mock.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].uri, "/v1internal:generateContent");
    let sent = &calls[0].body;
    assert_eq!(sent["model"], "gemini-3-pro");
    assert_eq!(sent["userAgent"], "antigravity");
    assert_eq!(sent["requestType"], "agent");
    assert_eq!(sent["project"], PROJECT);
    assert!(
        sent["requestId"]
            .as_str()
            .is_some_and(|id| id.starts_with("agent-")),
        "requestId must be agent-<uuid>: {sent}"
    );
    assert!(sent["request"].get("model").is_none(), "{sent}");
    assert!(sent["request"].get("safetySettings").is_none(), "{sent}");
    assert_eq!(
        sent["request"]["tools"][0]["functionDeclarations"][0]["name"],
        "get_weather"
    );
    assert_eq!(header(&calls[0].headers, "user-agent"), "antigravity");
    assert_eq!(
        header(&calls[0].headers, "x-goog-api-client"),
        "google-cloud-sdk vscode_cloudshelleditor/0.1"
    );
    assert_eq!(
        header(&calls[0].headers, "authorization"),
        "Bearer antigravity-access"
    );
}

#[tokio::test]
async fn anthropic_client_streams_cca_sse_with_usage() {
    let sse = [
        "data: {\"response\":{\"responseId\":\"r\",\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"Hel\"}]}}]}}\n\n",
        "data: {\"response\":{\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"lo\"}]},\"finishReason\":\"STOP\"}]}}\n\n",
        "data: {\"response\":{\"usageMetadata\":{\"promptTokenCount\":3,\"candidatesTokenCount\":2}}}\n\n",
    ]
    .concat();
    let mock = Mock::start(vec![Reply::Sse(sse)]).await;
    let h = harness();
    h.connect(&mock, PROJECT).await;

    let (status, body) = h
        .post_stream(
            "/v1/messages",
            json!({
                "model": format!("{PROVIDER}/gemini-3-pro"),
                "max_tokens": 128,
                "stream": true,
                "messages": [{"role": "user", "content": "say hello"}]
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(body.contains("message_start"), "{body}");
    assert!(body.contains("content_block_delta"), "{body}");
    assert!(body.contains("Hel"), "{body}");
    assert!(body.contains("lo"), "{body}");
    assert!(body.contains("message_stop"), "{body}");
    assert!(body.contains("output_tokens"), "usage is reported: {body}");

    let calls = mock.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0].uri, "/v1internal:streamGenerateContent?alt=sse",
        "streaming uses the SSE path and selector"
    );
    assert_eq!(calls[0].body["project"], PROJECT);
    assert_eq!(
        calls[0].body["request"]["generationConfig"]["maxOutputTokens"],
        128
    );
}

#[tokio::test]
async fn a_server_error_falls_back_from_the_daily_host_to_production() {
    static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _guard = ENV_LOCK.lock().await;

    let daily = Mock::start(vec![Reply::Json(
        503,
        json!({"error": {"message": "daily unavailable"}}),
    )])
    .await;
    let prod = Mock::start(vec![Reply::Json(200, cca_response("from prod", 4, 2))]).await;
    std::env::set_var("RROUTER_ANTIGRAVITY_PROD_HOST", prod.url());
    let h = harness();
    h.connect(&daily, PROJECT).await;

    let (status, body) = h
        .post(
            "/v1/chat/completions",
            json!({
                "model": format!("{PROVIDER}/gemini-3-pro"),
                "messages": [{"role": "user", "content": "hi"}]
            }),
        )
        .await;
    std::env::remove_var("RROUTER_ANTIGRAVITY_PROD_HOST");
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["choices"][0]["message"]["content"], "from prod");
    assert_eq!(daily.hits(), 1, "the daily host was tried first");
    assert_eq!(prod.hits(), 1, "the production host served the retry");
}

#[tokio::test]
async fn a_missing_project_is_a_clear_400_without_an_upstream_call() {
    let mock = Mock::start(vec![Reply::Json(200, cca_response("nope", 1, 1))]).await;
    let h = harness();
    h.connect(&mock, "").await;

    let (status, body) = h
        .post(
            "/v1/chat/completions",
            json!({
                "model": format!("{PROVIDER}/gemini-3-pro"),
                "messages": [{"role": "user", "content": "hi"}]
            }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("project_id")),
        "the refusal names the missing field: {body}"
    );
    assert_eq!(mock.hits(), 0, "a missing project never reaches upstream");
}
