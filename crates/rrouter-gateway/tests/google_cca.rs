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

mod support;

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

#[derive(Debug)]
struct Call {
    uri: String,
    headers: Vec<(String, String)>,
    body: Value,
}

#[derive(Clone)]
enum Reply {
    Json(u16, Value),
    Sse(String),
    /// Answer like cloudcode-pa for an undocumented envelope member: a `400` naming
    /// `field` whenever the body still carries it, and `then` otherwise. Sticky, so it
    /// keeps rejecting every call until the body is repaired instead of consuming a queue
    /// entry — which is what lets a test tell "repaired" from "probed again".
    RejectField {
        field: &'static str,
        then: Box<Reply>,
    },
}

impl Reply {
    fn sticky(&self) -> bool {
        matches!(self, Self::RejectField { .. })
    }
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
    mock.inner.calls.lock().expect("calls").push(Call {
        uri,
        headers,
        body: body.clone(),
    });
    let reply = {
        let mut replies = mock.inner.replies.lock().expect("replies");
        match replies.front() {
            Some(Reply::RejectField { field, .. }) if body.get(*field).is_some() => {
                return unknown_field(field);
            }
            Some(reply) if reply.sticky() => reply.clone(),
            _ => replies.pop_front().unwrap_or(Reply::Json(
                500,
                json!({"error": {"message": "no reply queued"}}),
            )),
        }
    };
    render(reply)
}

fn render(reply: Reply) -> Response {
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
        Reply::RejectField { then, .. } => render(*then),
    }
}

/// The real cloudcode-pa answer for an undocumented envelope member, at both carriers the
/// gateway reads: the `BadRequest` violation and protobuf's own `Unknown name` wording.
fn unknown_field(field: &str) -> Response {
    let message =
        format!("Invalid JSON payload received. Unknown name \"{field}\": Cannot find field.");
    (
        StatusCode::BAD_REQUEST,
        axum::Json(json!({"error": {
            "code": 400,
            "message": message,
            "status": "INVALID_ARGUMENT",
            "details": [{
                "@type": "type.googleapis.com/google.rpc.BadRequest",
                "fieldViolations": [{"field": field, "description": message}]
            }]
        }})),
    )
        .into_response()
}

// ---------------------------------------------------------------- harness

struct Harness {
    state: Arc<AppState>,
    router: Router,
    _data_dir: support::TempDir,
}

fn config() -> Config {
    Config {
        host: "127.0.0.1".parse().expect("ip"),
        port: 0,
        data_dir: support::temp_path("cca"),
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
        subscription_reserve_percent: 90,
        semantic_cache: rrouter_gateway::config::CacheMode::Off,
        semantic_cache_threshold: 0.92,
        semantic_embedding_model: None,
        semantic_cache_ttl_secs: 3600,
        semantic_cache_max_entries: 1000,
    }
}

fn harness() -> Harness {
    let config = config();
    let data_dir = support::TempDir::adopt(config.data_dir.clone());
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
        providers: Default::default(),
        concurrency: std::sync::RwLock::new(None),
        auth: rrouter_gateway::auth::Auth::load(&data_dir, false),
    });
    let router = app(Arc::clone(&state));
    Harness {
        state,
        router,
        _data_dir: data_dir,
    }
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
                Some(rrouter_translate::google_cca::user_agent()),
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
    assert_eq!(
        sent["userAgent"],
        rrouter_translate::google_cca::ENVELOPE_USER_AGENT
    );
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
    assert_eq!(
        header(&calls[0].headers, "user-agent"),
        rrouter_translate::google_cca::user_agent()
    );
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

#[tokio::test]
async fn an_unknown_envelope_field_is_dropped_for_the_retry_and_remembered() {
    let mock = Mock::start(vec![Reply::RejectField {
        field: "sessionId",
        then: Box::new(Reply::Json(200, cca_response("repaired", 5, 2))),
    }])
    .await;
    let h = harness();
    h.connect(&mock, PROJECT).await;
    let request = json!({
        "model": format!("{PROVIDER}/gemini-3.8-flash-tiered"),
        "messages": [{"role": "user", "content": "hi"}]
    });

    let (status, body) = h.post("/v1/chat/completions", request.clone()).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["choices"][0]["message"]["content"], "repaired");

    let calls = mock.calls();
    assert_eq!(
        calls.len(),
        2,
        "one probe and one repaired resend: {calls:?}"
    );
    assert!(
        calls[0].body["sessionId"].is_string(),
        "the field is still sent on the first request after startup: {}",
        calls[0].body
    );
    assert!(
        calls[1].body.get("sessionId").is_none(),
        "the resend omits exactly the field the 400 named: {}",
        calls[1].body
    );
    assert_eq!(calls[1].body["model"], "gemini-3.8-flash-tiered");
    assert_eq!(calls[1].body["project"], PROJECT);
    assert_eq!(
        calls[1].body["request"]["contents"][0]["parts"][0]["text"], "hi",
        "the inner request is untouched: {}",
        calls[1].body
    );

    // The second client request must not pay for the same probe again.
    let (status, body) = h.post("/v1/chat/completions", request).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["choices"][0]["message"]["content"], "repaired");
    let calls = mock.calls();
    assert_eq!(calls.len(), 3, "no second probe: {calls:?}");
    assert_eq!(mock.hits(), 3);
    assert!(
        calls[2].body.get("sessionId").is_none(),
        "the repair is remembered per connection: {}",
        calls[2].body
    );
}

#[tokio::test]
async fn a_streaming_400_is_repairable_before_any_byte_reaches_the_client() {
    let sse = concat!(
        "data: {\"response\":{\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"Hello\"}]}}]}}\n\n",
        "data: {\"response\":{\"candidates\":[{\"content\":{\"parts\":[{\"text\":\" there\"}]},\"finishReason\":\"STOP\"}],",
        "\"usageMetadata\":{\"promptTokenCount\":2,\"candidatesTokenCount\":2}}}\n\n"
    )
    .to_string();
    let mock = Mock::start(vec![Reply::RejectField {
        field: "sessionId",
        then: Box::new(Reply::Sse(sse)),
    }])
    .await;
    let h = harness();
    h.connect(&mock, PROJECT).await;

    let (status, body) = h
        .post_stream(
            "/v1/messages",
            json!({
                "model": format!("{PROVIDER}/gemini-3.8-flash-tiered"),
                "max_tokens": 64,
                "stream": true,
                "messages": [{"role": "user", "content": "say hello"}]
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(body.contains("Hello"), "{body}");
    assert!(body.contains(" there"), "{body}");
    assert!(body.contains("message_stop"), "{body}");

    let calls = mock.calls();
    assert_eq!(
        calls.len(),
        2,
        "one probe and one repaired resend: {calls:?}"
    );
    assert!(calls[0].body["sessionId"].is_string(), "{}", calls[0].body);
    assert_eq!(
        calls[1].uri, "/v1internal:streamGenerateContent?alt=sse",
        "the resend keeps the streaming path"
    );
    assert!(
        calls[1].body.get("sessionId").is_none(),
        "the resend omits the rejected field: {}",
        calls[1].body
    );
}

#[tokio::test]
async fn route_explain_returns_the_planned_candidate_without_dispatching() {
    let mock = Mock::start(vec![]).await;
    let h = harness();
    h.connect(&mock, PROJECT).await;

    let (status, body) = h
        .post(
            "/api/route/explain",
            json!({"model": format!("{PROVIDER}/gemini-3-pro")}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let data = &body["data"];
    assert_eq!(data["model"], format!("{PROVIDER}/gemini-3-pro"));
    assert_eq!(data["subscription_reserve_percent"], 90);
    assert_eq!(data["capacity_hints"], 0);
    assert_eq!(data["candidates"].as_array().expect("candidates").len(), 1);
    assert_eq!(data["candidates"][0]["provider"], PROVIDER);
    assert_eq!(data["candidates"][0]["dialect"], "google_code_assist");
    assert_eq!(mock.hits(), 0, "explain is a plan, not a dispatch");
}

/// A terminated account is named in the error and leaves rotation, so the next request for
/// the same model never contacts it again. Before the taxonomy it was an opaque
/// `Translation` error and the account was retried on every request.
#[tokio::test]
async fn a_suspended_account_is_named_in_the_error_and_leaves_rotation() {
    let mock = Mock::start(vec![Reply::Json(
        403,
        json!({
            "error": {
                "code": 403,
                "status": "TOS_VIOLATION",
                "message": "The service has been disabled in this account for violation of our \
                            Terms of Service."
            }
        }),
    )])
    .await;
    let h = harness();
    h.connect(&mock, PROJECT).await;
    let request = json!({
        "model": format!("{PROVIDER}/gemini-3-pro"),
        "messages": [{"role": "user", "content": "hi"}]
    });

    let (status, body) = h.post("/v1/chat/completions", request.clone()).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body: {body}");
    assert_eq!(
        body["error"]["code"], "account_suspended",
        "a client can branch on the class: {body}"
    );
    assert!(
        body["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("suspended")),
        "the message names the suspension: {body}"
    );

    // The retirement write is off the request path; wait for it rather than racing it.
    let deadline = Instant::now() + Duration::from_secs(5);
    let retired = loop {
        let mut rows = h.state.store.list_connections(None).await.expect("list");
        let row = rows.pop().expect("the account is still listed");
        if !row.is_active {
            break row;
        }
        assert!(
            Instant::now() < deadline,
            "the refused account was never taken out of rotation"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    assert_eq!(
        retired.refusal.as_deref(),
        Some("account_suspended"),
        "the row records that the provider, not an operator, retired it"
    );
    assert_eq!(retired.last_error_code, Some(403));
    assert!(
        retired
            .last_error
            .as_deref()
            .is_some_and(|reason| reason.contains("Terms of Service")),
        "the retirement keeps the provider's wording: {:?}",
        retired.last_error
    );

    // The dead account is not a candidate any more, and the upstream is not asked again.
    let (status, body) = h.post("/v1/chat/completions", request).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "body: {body}");
    assert_eq!(
        mock.hits(),
        1,
        "the suspended account was contacted once only"
    );
}

/// Verification-required arrives on healthy accounts: it cools the account down for this
/// request but must never retire it, so the next request still has a candidate. The reason
/// is still written down — the breaker that owns the cooldown is in memory, so the row is
/// the only thing that explains the account after a restart.
#[tokio::test]
async fn a_verification_requirement_cools_the_account_without_retiring_it() {
    let mock = Mock::start(vec![Reply::Json(
        403,
        json!({
            "error": {
                "code": 403,
                "status": "VALIDATION_REQUIRED",
                "message": "Verify your account to continue"
            }
        }),
    )])
    .await;
    let h = harness();
    h.connect(&mock, PROJECT).await;

    let (status, body) = h
        .post(
            "/v1/chat/completions",
            json!({
                "model": format!("{PROVIDER}/gemini-3-pro"),
                "messages": [{"role": "user", "content": "hi"}]
            }),
        )
        .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "body: {body}");
    assert_eq!(body["error"]["code"], "account_verification_required");

    // The reason write is off the request path; wait for it rather than racing it.
    let deadline = Instant::now() + Duration::from_secs(5);
    let row = loop {
        let mut rows = h.state.store.list_connections(None).await.expect("list");
        let row = rows.pop().expect("the account is still listed");
        if row.last_error.is_some() {
            break row;
        }
        assert!(
            Instant::now() < deadline,
            "the verification requirement was never recorded on the row"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    assert!(
        row.is_active,
        "a verification requirement must not retire the account"
    );
    assert_eq!(
        row.refusal, None,
        "only a permanent refusal is recorded against the row"
    );
    let reason = row.last_error.as_deref().expect("the reason is recorded");
    assert!(
        reason.starts_with("account_verification_required: "),
        "the class token is what makes the row readable after a restart: {reason}"
    );
    assert!(
        reason.contains("Verify your account to continue"),
        "the provider's own wording rides along: {reason}"
    );
    assert_eq!(
        row.last_error_code,
        Some(403),
        "the class's provider status, the number the breaker's window keys on"
    );
    assert!(row.last_error_at.is_some());
}

// ------------------------------------------------------- refusals inside a 200 stream

/// The one connection's id, whatever the store assigned it.
async fn only_connection(h: &Harness) -> uuid::Uuid {
    let rows = h.state.store.list_connections(None).await.expect("list");
    assert_eq!(rows.len(), 1, "the harness registers exactly one account");
    rows[0].id
}

/// Wait for a retirement that is written after the response was already handed over.
///
/// The stream has closed by the time the body is collected, so the write is in flight at
/// most: poll for it rather than racing it.
async fn retired_row(h: &Harness) -> rrouter_store::Connection {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let rows = h.state.store.list_connections(None).await.expect("list");
        let row = rows
            .into_iter()
            .next()
            .expect("the account is still listed");
        if !row.is_active {
            return row;
        }
        assert!(
            Instant::now() < deadline,
            "the refused account was never retired"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// A streaming request whose answer turns out to be a terminated account must end the
/// same way the buffered answer does: the client reads the refusal, and the row leaves
/// rotation for every later model.
///
/// cloudcode-pa opens the body (`200 text/event-stream`) and only then reports the
/// account's state, so this refusal has no HTTP status of its own and the transport has
/// already committed to serving a stream. There is no failover to attempt; what must not
/// be lost is the *lifecycle* verdict, which arrives one frame too late for the dispatcher
/// to see.
#[tokio::test]
async fn a_suspended_account_in_a_200_stream_is_named_and_leaves_rotation() {
    let sse = concat!(
        // One ordinary frame first: the refusal really is mid-stream, after output the
        // client has already been sent.
        "data: {\"response\":{\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"Hello\"}]}}]}}\n\n",
        "data: {\"error\":{\"code\":403,\"status\":\"TOS_VIOLATION\",\"message\":",
        "\"The service has been disabled in this account for violation of our Terms of Service.\"}}\n\n"
    );
    let mock = Mock::start(vec![Reply::Sse(sse.into())]).await;
    let h = harness();
    h.connect(&mock, PROJECT).await;
    let connection = only_connection(&h).await;
    let request = json!({
        "model": format!("{PROVIDER}/gemini-3-pro"),
        "stream": true,
        "messages": [{"role": "user", "content": "hi"}]
    });

    let (status, body) = h.post_stream("/v1/chat/completions", request.clone()).await;
    assert_eq!(status, StatusCode::OK, "the 200 was already sent: {body}");
    assert!(
        body.contains("account_suspended"),
        "the client reads the class in its terminal error frame: {body}"
    );

    let retired = retired_row(&h).await;
    assert_eq!(
        retired.refusal.as_deref(),
        Some("account_suspended"),
        "the row records that the provider, not an operator, retired it"
    );
    assert_eq!(
        retired.last_error_code,
        Some(403),
        "the class's own status is recorded even though the wire said 200"
    );
    // The refusal is also the breaker's verdict: before this carrier recorded it, the
    // dispatcher's stream wrapper settled the call as a success instead.
    assert!(
        !h.state.dispatcher.breaker().is_available(connection),
        "a streamed refusal must never be booked as a healthy call"
    );

    // Out of rotation: the next request for the same model never contacts it again.
    let (status, body) = h.post_stream("/v1/chat/completions", request).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "body: {body}");
    assert_eq!(
        mock.hits(),
        1,
        "the suspended account was contacted once only"
    );
}

/// The same mid-stream carrier with the verification signal: a healthy, fully-quota'd
/// account that must pass a check. It benches for this request and must stay routable —
/// retiring it would take a fixable account out of rotation for good.
#[tokio::test]
async fn a_streamed_verification_requirement_benches_without_retiring() {
    let sse = concat!(
        "data: {\"response\":{\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"Hello\"}]}}]}}\n\n",
        "data: {\"error\":{\"code\":403,\"status\":\"VALIDATION_REQUIRED\",\"message\":",
        "\"Verify your account to continue\"}}\n\n"
    );
    let mock = Mock::start(vec![Reply::Sse(sse.into())]).await;
    let h = harness();
    h.connect(&mock, PROJECT).await;
    let connection = only_connection(&h).await;

    let (status, body) = h
        .post_stream(
            "/v1/chat/completions",
            json!({
                "model": format!("{PROVIDER}/gemini-3-pro"),
                "stream": true,
                "messages": [{"role": "user", "content": "hi"}]
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "the 200 was already sent: {body}");
    assert!(
        body.contains("account_verification_required"),
        "the class reaches the client: {body}"
    );

    let rows = h.state.store.list_connections(None).await.expect("list");
    assert!(
        rows[0].is_active,
        "a verification requirement must not retire the account"
    );
    assert_eq!(
        rows[0].refusal, None,
        "only a permanent refusal is recorded against the row"
    );
    // The bench is in-process only: this account is cooled down for every later request
    // until the breaker's auth window expires, then becomes eligible again by itself.
    assert!(
        !h.state.dispatcher.breaker().is_available(connection),
        "the streamed refusal must bench the account, not just the row"
    );
}
