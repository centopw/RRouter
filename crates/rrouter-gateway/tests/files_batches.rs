//! `/v1/files` and `/v1/batches` end to end: a real app over `oneshot`, a real mock
//! upstream reached through `base_url_override`.
//!
//! These routes are transparent, so the assertions are about bytes: the multipart boundary
//! the client signed, a batch body's exact spelling, a downloaded file's bytes, and the
//! provider's error payload. A `501` is asserted just as firmly: nothing is stored or queued
//! locally when the provider does not serve the API.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::State;
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Router;
use http_body_util::BodyExt;
use rrouter_auth::Credentials;
use rrouter_gateway::config::Config;
use rrouter_gateway::{app, AppState};
use rrouter_router::{Catalog, Planner};
use rrouter_store::Store;
use serde_json::{json, Value};
use tower::ServiceExt;

// ------------------------------------------------------------------------- mock upstream

/// One request as the provider saw it.
#[derive(Clone, Debug)]
struct Seen {
    method: String,
    path: String,
    query: String,
    content_type: String,
    body: Vec<u8>,
    headers: Vec<(String, String)>,
}

/// One scripted answer.
#[derive(Clone)]
struct Reply {
    status: u16,
    content_type: String,
    body: Vec<u8>,
}

impl Reply {
    fn json(status: u16, value: Value) -> Self {
        Self {
            status,
            content_type: "application/json".into(),
            body: value.to_string().into_bytes(),
        }
    }

    fn bytes(status: u16, content_type: &str, body: &[u8]) -> Self {
        Self {
            status,
            content_type: content_type.into(),
            body: body.to_vec(),
        }
    }
}

#[derive(Clone)]
struct Mock {
    inner: Arc<MockInner>,
}

struct MockInner {
    hits: AtomicUsize,
    seen: Mutex<Vec<Seen>>,
    replies: Mutex<Vec<Reply>>,
    url: String,
}

impl Mock {
    async fn start(replies: Vec<Reply>) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock");
        let addr = listener.local_addr().expect("addr");
        let mock = Mock {
            inner: Arc::new(MockInner {
                hits: AtomicUsize::new(0),
                seen: Mutex::new(Vec::new()),
                replies: Mutex::new(replies),
                url: format!("http://{addr}"),
            }),
        };
        let router = Router::new().fallback(handle).with_state(mock.clone());
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        mock
    }

    fn url(&self) -> String {
        self.inner.url.clone()
    }

    fn hits(&self) -> usize {
        self.inner.hits.load(Ordering::SeqCst)
    }

    fn seen(&self) -> Vec<Seen> {
        self.inner.seen.lock().expect("seen").clone()
    }

    fn header(&self, index: usize, name: &str) -> Option<String> {
        self.seen()
            .get(index)?
            .headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.clone())
    }
}

async fn handle(State(mock): State<Mock>, req: Request<Body>) -> Response {
    mock.inner.hits.fetch_add(1, Ordering::SeqCst);
    let method = req.method().to_string();
    let path = req.uri().path().to_string();
    let query = req.uri().query().unwrap_or_default().to_string();
    let content_type = req
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
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
    let body = req
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes()
        .to_vec();
    mock.inner.seen.lock().expect("seen").push(Seen {
        method,
        path,
        query,
        content_type,
        body,
        headers,
    });
    let reply = {
        let mut replies = mock.inner.replies.lock().expect("replies");
        if replies.is_empty() {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
        replies.remove(0)
    };
    Response::builder()
        .status(StatusCode::from_u16(reply.status).unwrap_or(StatusCode::OK))
        .header("content-type", reply.content_type)
        .body(Body::from(reply.body))
        .expect("reply")
}

// ----------------------------------------------------------------------------- harness

struct H {
    state: Arc<AppState>,
    router: Router,
}

fn config(require_api_key: bool) -> Config {
    Config {
        host: "127.0.0.1".parse().expect("ip"),
        port: 0,
        data_dir: std::env::temp_dir().join(format!("rrouter-files-{}", uuid::Uuid::new_v4())),
        master_key: None,
        require_api_key,
        request_timeout_secs: 10,
        connect_timeout_secs: 2,
        log_filter: "rrouter=error".into(),
        static_dir: None,
        cors_origins: vec![],
        allow_remote_admin: false,
        admin_token: None,
        max_concurrency: 0,
        max_attempts: 4,
        retry_attempts: 0,
        retry_base_ms: 0,
        retry_max_ms: 0,
        shutdown_grace_secs: 5,
        strategy: rrouter_router::Strategy::Priority,
        cache: rrouter_gateway::config::CacheMode::Off,
        cache_ttl_secs: 300,
        cache_max_entries: 100,
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

fn harness(require_api_key: bool) -> H {
    let config = config(require_api_key);
    let data_dir = config.data_dir.clone();
    let dispatcher = Arc::new(rrouter_gateway::state::dispatcher(&config));
    let concurrency = rrouter_gateway::state::concurrency_for(config.max_concurrency);
    let state = Arc::new(AppState {
        store: Arc::new(Store::open_in_memory().expect("store")),
        catalog: Arc::new(Catalog::builtin()),
        refresher: Arc::new(rrouter_auth::Refresher::new(
            reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .build()
                .expect("client"),
        )),
        planner: Planner::new(),
        dispatcher,
        http: reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .expect("client"),
        config: rrouter_gateway::config::ConfigSnapshot::from(config),
        started: Instant::now(),
        oauth: rrouter_auth::OAuthFlows::new(),
        counters: Default::default(),
        concurrency: RwLock::new(concurrency),
        auth: rrouter_gateway::auth::Auth::load(&data_dir, false),
    });
    let router = app(Arc::clone(&state));
    H { state, router }
}

const API_KEY: &str = "sk-secret-files-9b";

impl H {
    async fn connect(&self, provider: &str, mock: &Mock) {
        let creds = Credentials {
            api_key: Some(API_KEY.into()),
            base_url_override: Some(mock.url()),
            ..Credentials::default()
        };
        self.state
            .store
            .create_connection(provider, "api_key", creds, Some("mock"))
            .await
            .expect("connection");
    }

    async fn call(
        &self,
        method: &str,
        uri: &str,
        body: Option<Vec<u8>>,
        headers: &[(&str, &str)],
    ) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
        let mut builder = Request::builder().method(method).uri(uri);
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let request = builder
            .body(body.map_or_else(Body::empty, Body::from))
            .expect("request");
        let response = self.router.clone().oneshot(request).await.expect("oneshot");
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes()
            .to_vec();
        (status, headers, bytes)
    }

    async fn upload(
        &self,
        content_type: &str,
        body: &[u8],
    ) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
        self.call(
            "POST",
            "/v1/files",
            Some(body.to_vec()),
            &[("content-type", content_type)],
        )
        .await
    }
}

fn json_of(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes).unwrap_or(Value::Null)
}

/// A minimal multipart body with a boundary the client signed.
fn multipart() -> (String, Vec<u8>) {
    const BOUNDARY: &str = "----rrouter9b";
    let body = format!(
        "--{BOUNDARY}\r\n\
         Content-Disposition: form-data; name=\"purpose\"\r\n\r\n\
         batch\r\n\
         --{BOUNDARY}\r\n\
         Content-Disposition: form-data; name=\"file\"; filename=\"requests.jsonl\"\r\n\
         Content-Type: application/jsonl\r\n\r\n\
         {{\"custom_id\":\"1\"}}\r\n\
         --{BOUNDARY}--\r\n"
    );
    (
        format!("multipart/form-data; boundary={BOUNDARY}"),
        body.into_bytes(),
    )
}

fn file_reply() -> Value {
    json!({
        "id": "file-abc123",
        "object": "file",
        "bytes": 42,
        "created_at": 1710000000,
        "filename": "requests.jsonl",
        "purpose": "batch",
        "status": "processed",
    })
}

// ------------------------------------------------------------------------------- tests

#[tokio::test]
async fn an_upload_is_relayed_with_its_own_boundary_and_bytes() {
    let mock = Mock::start(vec![Reply::json(200, file_reply())]).await;
    let h = harness(false);
    h.connect("openai", &mock).await;
    let (content_type, body) = multipart();

    let (status, _, out) = h.upload(&content_type, &body).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&out));
    assert_eq!(
        json_of(&out),
        file_reply(),
        "the provider's body is relayed"
    );

    let seen = mock.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].method, "POST");
    assert_eq!(seen[0].path, "/v1/files");
    assert_eq!(
        seen[0].content_type, content_type,
        "the boundary the client signed is the one the provider receives"
    );
    assert_eq!(seen[0].body, body, "the form is forwarded byte for byte");
    assert_eq!(
        mock.header(0, "authorization").as_deref(),
        Some("Bearer sk-secret-files-9b")
    );
}

#[tokio::test]
async fn a_list_forwards_the_clients_query_and_drops_the_provider_pin() {
    let mock = Mock::start(vec![Reply::json(
        200,
        json!({"object": "list", "data": [file_reply()], "has_more": false}),
    )])
    .await;
    let h = harness(false);
    h.connect("openai", &mock).await;

    let (status, _, out) = h
        .call(
            "GET",
            "/v1/files?purpose=batch&limit=2&provider=openai",
            None,
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json_of(&out)["data"][0]["id"], json!("file-abc123"));

    let seen = mock.seen();
    assert_eq!(seen[0].method, "GET");
    assert_eq!(seen[0].path, "/v1/files");
    assert_eq!(
        seen[0].query, "purpose=batch&limit=2",
        "the gateway's own pin is not part of the provider's query"
    );
}

#[tokio::test]
async fn a_file_download_preserves_bytes_and_content_type() {
    let payload: &[u8] = b"{\"custom_id\":\"1\"}\n{\"custom_id\":\"2\"}\n";
    let mock = Mock::start(vec![Reply::bytes(200, "application/jsonl", payload)]).await;
    let h = harness(false);
    h.connect("openai", &mock).await;

    let (status, headers, out) = h
        .call("GET", "/v1/files/file-abc123/content", None, &[])
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(out, payload, "the file's bytes are untouched");
    assert_eq!(
        headers.get("content-type").and_then(|v| v.to_str().ok()),
        Some("application/jsonl")
    );
    assert_eq!(mock.seen()[0].path, "/v1/files/file-abc123/content");
}

#[tokio::test]
async fn a_batch_body_is_relayed_byte_for_byte() {
    // Deliberate spacing and ordering: a re-serialization would change both.
    let body = br#"{"input_file_id" : "file-abc123",  "endpoint":"/v1/chat/completions", "completion_window":"24h"}"#;
    let mock = Mock::start(vec![Reply::json(
        200,
        json!({"id": "batch_1", "object": "batch", "status": "validating"}),
    )])
    .await;
    let h = harness(false);
    h.connect("openai", &mock).await;

    let (status, _, out) = h
        .call(
            "POST",
            "/v1/batches",
            Some(body.to_vec()),
            &[("content-type", "application/json")],
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&out));
    assert_eq!(json_of(&out)["id"], json!("batch_1"));

    let seen = mock.seen();
    assert_eq!(seen[0].method, "POST");
    assert_eq!(seen[0].path, "/v1/batches");
    assert_eq!(seen[0].body, body, "batch metadata is not re-serialized");
    assert_eq!(seen[0].content_type, "application/json");
}

#[tokio::test]
async fn batch_retrieve_and_cancel_reach_the_provider() {
    let mock = Mock::start(vec![
        Reply::json(200, json!({"id": "batch_1", "status": "in_progress"})),
        Reply::json(200, json!({"id": "batch_1", "status": "cancelling"})),
    ])
    .await;
    let h = harness(false);
    h.connect("openai", &mock).await;

    let (status, _, out) = h.call("GET", "/v1/batches/batch_1", None, &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json_of(&out)["status"], json!("in_progress"));

    let (status, _, out) = h
        .call("POST", "/v1/batches/batch_1/cancel", None, &[])
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json_of(&out)["status"], json!("cancelling"));

    let seen = mock.seen();
    assert_eq!(seen[0].method, "GET");
    assert_eq!(seen[0].path, "/v1/batches/batch_1");
    assert_eq!(seen[1].method, "POST");
    assert_eq!(seen[1].path, "/v1/batches/batch_1/cancel");
}

#[tokio::test]
async fn a_provider_without_the_capability_answers_501() {
    let mock = Mock::start(vec![]).await;
    let h = harness(false);
    h.connect("mistral", &mock).await;
    let (content_type, body) = multipart();

    let (status, _, out) = h.upload(&content_type, &body).await;
    assert_eq!(
        status,
        StatusCode::NOT_IMPLEMENTED,
        "{}",
        String::from_utf8_lossy(&out)
    );
    let body = json_of(&out);
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("file uploads"),
        "{body}"
    );
    assert_eq!(mock.hits(), 0, "no path was guessed for the provider");

    let (status, _, out) = h
        .call(
            "POST",
            "/v1/batches",
            Some(
                json!({"input_file_id": "f", "endpoint": "/v1/chat/completions"})
                    .to_string()
                    .into_bytes(),
            ),
            &[("content-type", "application/json")],
        )
        .await;
    assert_eq!(
        status,
        StatusCode::NOT_IMPLEMENTED,
        "{}",
        String::from_utf8_lossy(&out)
    );
    assert_eq!(mock.hits(), 0);
}

#[tokio::test]
async fn an_upload_requires_a_multipart_boundary() {
    let mock = Mock::start(vec![]).await;
    let h = harness(false);
    h.connect("openai", &mock).await;

    let (status, _, out) = h.upload("application/json", b"{}").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(json_of(&out)["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .contains("multipart/form-data"));
    assert_eq!(mock.hits(), 0);
}

#[tokio::test]
async fn auth_is_required_when_configured() {
    let mock = Mock::start(vec![Reply::json(200, file_reply())]).await;
    let h = harness(true);
    h.connect("openai", &mock).await;
    let (content_type, body) = multipart();

    let (status, _, _) = h.upload(&content_type, &body).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(mock.hits(), 0, "no provider call without a gateway key");
}

#[tokio::test]
async fn an_upstream_error_keeps_the_provider_payload() {
    let mock = Mock::start(vec![Reply::json(
        400,
        json!({"error": {
            "message": "invalid purpose for this file",
            "type": "invalid_request_error",
            "param": "purpose",
            "code": "invalid_value",
        }}),
    )])
    .await;
    let h = harness(false);
    h.connect("openai", &mock).await;
    let (content_type, body) = multipart();

    let (status, _, out) = h.upload(&content_type, &body).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let error = &json_of(&out)["error"];
    assert_eq!(error["message"], json!("invalid purpose for this file"));
    assert_eq!(error["type"], json!("invalid_request_error"));
    assert_eq!(error["param"], json!("purpose"));
    assert_eq!(error["code"], json!("invalid_value"));
}

#[tokio::test]
async fn an_unsafe_file_id_never_reaches_the_provider() {
    let mock = Mock::start(vec![]).await;
    let h = harness(false);
    h.connect("openai", &mock).await;

    for uri in [
        "/v1/files/..%2F..%2Fetc%2Fpasswd",
        "/v1/files/%2E%2E",
        "/v1/batches/..%2Fsecrets",
    ] {
        let (status, _, _) = h.call("GET", uri, None, &[]).await;
        assert!(
            status == StatusCode::BAD_REQUEST || status == StatusCode::NOT_FOUND,
            "{uri} answered {status}"
        );
    }
    assert_eq!(mock.hits(), 0, "a traversal-shaped id is refused locally");
}

#[tokio::test]
async fn no_credential_appears_in_a_response() {
    let mock = Mock::start(vec![
        Reply::json(200, file_reply()),
        Reply::json(500, json!({"error": {"message": "provider exploded"}})),
    ])
    .await;
    let h = harness(false);
    h.connect("openai", &mock).await;
    let (content_type, body) = multipart();

    for out in [
        h.upload(&content_type, &body).await.2,
        h.upload(&content_type, &body).await.2,
    ] {
        let text = String::from_utf8_lossy(&out);
        assert!(!text.contains(API_KEY), "a response leaked the key: {text}");
    }
    // The key travels in the header, which is the only place it belongs.
    assert_eq!(
        mock.header(0, "authorization").as_deref(),
        Some("Bearer sk-secret-files-9b")
    );
}
