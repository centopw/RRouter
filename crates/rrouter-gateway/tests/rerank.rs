//! `/v1/rerank` end to end: the real axum app over `oneshot`, talking to a real mock
//! upstream through `base_url_override` — the same path a Cohere or Jina connection takes.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::State;
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use http_body_util::BodyExt;
use rrouter_auth::Credentials;
use rrouter_gateway::config::{Config, ConfigSnapshot};
use rrouter_gateway::{app, AppState};
use rrouter_router::{Catalog, Planner};
use rrouter_store::{Store, UsageQuery};
use serde_json::{json, Value};
use tower::ServiceExt;

// ------------------------------------------------------------------------- mock upstream

/// One recorded upstream request: path, JSON body, headers.
type Recorded = (String, Value, Vec<(String, String)>);

#[derive(Clone)]
struct Mock {
    inner: Arc<MockInner>,
}

struct MockInner {
    hits: AtomicUsize,
    /// One entry per request, in order.
    seen: Mutex<Vec<Recorded>>,
    replies: Mutex<Vec<(u16, Value)>>,
    url: String,
}

impl Mock {
    async fn start(replies: Vec<(u16, Value)>) -> Self {
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
        let router = Router::new()
            .route("/v1/rerank", post(handle))
            .with_state(mock.clone());
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        mock
    }

    /// Origin only, so the route must keep the provider's native path.
    fn url(&self) -> String {
        self.inner.url.clone()
    }

    fn hits(&self) -> usize {
        self.inner.hits.load(Ordering::SeqCst)
    }

    fn seen(&self) -> Vec<Recorded> {
        self.inner.seen.lock().expect("seen").clone()
    }

    fn header(&self, index: usize, name: &str) -> Option<String> {
        self.seen()
            .get(index)?
            .2
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.clone())
    }
}

async fn handle(State(mock): State<Mock>, req: Request<Body>) -> Response {
    mock.inner.hits.fetch_add(1, Ordering::SeqCst);
    let path = req.uri().path().to_string();
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
    let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    mock.inner
        .seen
        .lock()
        .expect("seen")
        .push((path, body, headers));

    let reply = {
        let mut replies = mock.inner.replies.lock().expect("replies");
        if replies.is_empty() {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
        replies.remove(0)
    };
    let status = StatusCode::from_u16(reply.0).unwrap_or(StatusCode::OK);
    (status, Json(reply.1)).into_response()
}

// ----------------------------------------------------------------------------- harness

struct Harness {
    state: Arc<AppState>,
    router: Router,
}

fn config(require_api_key: bool) -> Config {
    Config {
        host: "127.0.0.1".parse().expect("ip"),
        port: 0,
        data_dir: std::env::temp_dir().join(format!("rrouter-rerank-{}", uuid::Uuid::new_v4())),
        master_key: None,
        require_api_key,
        request_timeout_secs: 10,
        connect_timeout_secs: 2,
        log_filter: "rrouter=info".into(),
        static_dir: None,
        cors_origins: vec![],
        allow_remote_admin: false,
        admin_token: None,
        max_concurrency: 0,
        max_attempts: 4,
        // One send per candidate: a 500 falls straight over to the next connection, which
        // is what the fallback test asserts.
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
        semantic_cache: rrouter_gateway::config::CacheMode::Off,
        semantic_cache_threshold: 0.92,
        semantic_embedding_model: None,
        semantic_cache_ttl_secs: 3600,
        semantic_cache_max_entries: 1000,
    }
}

fn build_state(config: Config) -> AppState {
    let data_dir = config.data_dir.clone();
    // The same dispatcher `serve` builds, so the retry policy under test is the real one.
    let dispatcher = Arc::new(rrouter_gateway::state::dispatcher(&config));
    let concurrency = rrouter_gateway::state::concurrency_for(config.max_concurrency);
    AppState {
        store: Arc::new(Store::open_in_memory().expect("store")),
        catalog: Arc::new(Catalog::builtin()),
        refresher: Arc::new(rrouter_auth::Refresher::new(
            reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .expect("client"),
        )),
        planner: Planner::new(),
        dispatcher,
        http: reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .expect("client"),
        config: ConfigSnapshot::from(config),
        started: Instant::now(),
        oauth: rrouter_auth::OAuthFlows::new(),
        counters: Default::default(),
        concurrency: RwLock::new(concurrency),
        auth: rrouter_gateway::auth::Auth::load(&data_dir, false),
    }
}

fn harness(require_api_key: bool) -> Harness {
    let state = Arc::new(build_state(config(require_api_key)));
    let router = app(Arc::clone(&state));
    Harness { state, router }
}

impl Harness {
    /// One connection for `provider`, pointed at `mock`, with an explicit priority.
    async fn connect(&self, provider: &str, mock: &Mock, priority: i32, api_key: &str) {
        let creds = Credentials {
            api_key: Some(api_key.to_string()),
            base_url_override: Some(mock.url()),
            ..Credentials::default()
        };
        let id = self
            .state
            .store
            .create_connection(provider, "api_key", creds, Some("mock"))
            .await
            .expect("connection")
            .id;
        self.state
            .store
            .set_connection_priority(id, priority)
            .await
            .expect("priority");
    }

    async fn post(
        &self,
        body: Value,
        headers: &[(&str, &str)],
    ) -> (StatusCode, axum::http::HeaderMap, Value) {
        let mut builder = Request::builder()
            .method("POST")
            .uri("/v1/rerank")
            .header("content-type", "application/json");
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let response = self
            .router
            .clone()
            .oneshot(builder.body(Body::from(body.to_string())).expect("request"))
            .await
            .expect("oneshot");
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes();
        let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, headers, value)
    }

    async fn usage(&self) -> Vec<rrouter_store::UsageRecord> {
        self.state
            .store
            .recent_usage(&UsageQuery {
                limit: 10,
                ..UsageQuery::default()
            })
            .await
            .expect("usage")
    }
}

// ---------------------------------------------------------------------------- fixtures

fn request() -> Value {
    json!({
        "model": "cohere/rerank-v3.5",
        "query": "What is the capital of the United States?",
        "documents": [
            "Carson City is the capital of Nevada.",
            "Washington, D.C. is the capital of the United States."
        ],
        "top_n": 1,
        "return_documents": true
    })
}

/// Cohere v1, verbatim from the reference: no echoed model, billing under `meta`.
fn cohere_ok() -> Value {
    json!({
        "results": [
            {"index": 1, "relevance_score": 0.999071, "document": {"text": "Washington, D.C. is the capital of the United States."}}
        ],
        "id": "8bc745a3-7871-4597-822e-18c95d5df48c",
        "meta": {"api_version": {"version": "1"}, "billed_units": {"search_units": 1}}
    })
}

// ------------------------------------------------------------------------------- tests

#[tokio::test]
async fn rerank_returns_the_canonical_shape_and_records_an_unmetered_row() {
    let mock = Mock::start(vec![(200, cohere_ok())]).await;
    let h = harness(false);
    h.connect("cohere", &mock, 0, "co-secret-key").await;

    let (status, headers, body) = h.post(request(), &[]).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        headers.contains_key("x-request-id"),
        "responses carry an id"
    );

    // The canonical client shape, with the provider's document flattened to text.
    assert_eq!(body["model"], json!("rerank-v3.5"));
    assert_eq!(body["results"][0]["index"], json!(1));
    assert_eq!(
        body["results"][0]["document"],
        json!("Washington, D.C. is the capital of the United States.")
    );
    assert_eq!(body["usage"]["search_units"], json!(1));
    assert_eq!(body["usage"]["input_tokens"], json!(0));

    // The provider saw the canonical body at its native path, with the key in a header.
    let seen = mock.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(
        seen[0].0, "/v1/rerank",
        "the manifest path is kept under the override"
    );
    assert_eq!(
        seen[0].1["model"],
        json!("rerank-v3.5"),
        "provider id, not the routed ref"
    );
    assert_eq!(
        seen[0].1["documents"][0],
        json!("Carson City is the capital of Nevada.")
    );
    assert_eq!(seen[0].1["return_documents"], json!(true));
    assert_eq!(
        mock.header(0, "authorization").as_deref(),
        Some("Bearer co-secret-key")
    );

    // One row, the endpoint named, no tokens invented.
    let rows = h.usage().await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].endpoint.as_deref(), Some("/v1/rerank"));
    assert_eq!(rows[0].provider, "cohere");
    assert_eq!(rows[0].model, "cohere/rerank-v3.5");
    assert_eq!(rows[0].status, 200);
    assert_eq!(rows[0].input_tokens, 0);
    assert_eq!(rows[0].output_tokens, 0);
    assert_eq!(rows[0].cost_micros, 0);
    assert_eq!(rows[0].error, None);

    // The attempt log reads back under the id the client was given.
    let request_id: uuid::Uuid = headers["x-request-id"]
        .to_str()
        .expect("id")
        .parse()
        .expect("uuid");
    let attempts = h
        .state
        .store
        .attempts_for(request_id)
        .await
        .expect("attempts");
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0].status, 200);
    assert_eq!(
        attempts[0].model, "rerank-v3.5",
        "the attempt log names the id actually sent upstream"
    );
}

#[tokio::test]
async fn a_failing_provider_falls_back_to_the_next_connection() {
    let dead = Mock::start(vec![(500, json!({"message": "upstream exploded"}))]).await;
    let live = Mock::start(vec![(200, cohere_ok())]).await;
    let h = harness(false);
    h.connect("cohere", &dead, 0, "co-secret-key").await;
    h.connect("cohere", &live, 1, "co-secret-key").await;

    let (status, headers, body) = h.post(request(), &[]).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(dead.hits(), 1);
    assert_eq!(live.hits(), 1, "the second account served the request");

    let request_id: uuid::Uuid = headers["x-request-id"]
        .to_str()
        .expect("id")
        .parse()
        .expect("uuid");
    let attempts = h
        .state
        .store
        .attempts_for(request_id)
        .await
        .expect("attempts");
    let statuses: Vec<u16> = attempts.iter().map(|a| a.status).collect();
    assert_eq!(
        statuses,
        vec![500, 200],
        "the fallback is visible in the log"
    );

    let rows = h.usage().await;
    assert_eq!(rows.len(), 1, "one terminal usage row for the request");
    assert_eq!(rows[0].status, 200);
    assert_eq!(rows[0].provider, "cohere");
}

/// A `provider/model` ref is authoritative for a reviewed manifest provider: the planner
/// splits it (every compiled provider counts, manifests included) and the route sends the
/// bare id — the ref itself is never forwarded as a model name.
#[tokio::test]
async fn a_manifest_provider_ref_routes_to_that_provider_with_its_bare_model_id() {
    let cohere = Mock::start(vec![(200, cohere_ok())]).await;
    let jina = Mock::start(vec![(
        200,
        json!({
            "model": "jina-reranker-v2-base-multilingual",
            "object": "list",
            "usage": {"total_tokens": 42},
            "results": [{"index": 0, "relevance_score": 0.8, "document": "Carson City"}]
        }),
    )])
    .await;
    let h = harness(false);
    // Cohere holds the better priority, so only the ref itself can keep it out of the plan.
    h.connect("cohere", &cohere, 0, "co-key").await;
    h.connect("jina", &jina, 1, "jina-key").await;

    let mut body = request();
    body["model"] = json!("jina/jina-reranker-v2-base-multilingual");
    let (status, _, out) = h.post(body, &[]).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(
        cohere.hits(),
        0,
        "the ref names jina; cohere must not see it"
    );
    assert_eq!(jina.hits(), 1);
    assert_eq!(
        jina.seen()[0].1["model"],
        json!("jina-reranker-v2-base-multilingual"),
        "the provider receives its own id, not the routed ref"
    );
    assert_eq!(out["usage"]["total_tokens"], json!(42));
    assert_eq!(out["model"], json!("jina-reranker-v2-base-multilingual"));
}

#[tokio::test]
async fn a_provider_without_a_rerank_endpoint_answers_501() {
    let mock = Mock::start(vec![]).await;
    let h = harness(false);
    h.connect("openai", &mock, 0, "sk-test").await;

    let mut body = request();
    body["model"] = json!("openai/gpt-4.1");
    let (status, _, out) = h.post(body, &[]).await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED, "{out}");
    assert!(
        out["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("rerank"),
        "{out}"
    );
    assert_eq!(mock.hits(), 0, "no guessed path was called");
    assert!(
        h.usage().await.is_empty(),
        "nothing was called, nothing is billed"
    );
}

#[tokio::test]
async fn an_invalid_body_is_a_400_in_the_client_envelope() {
    let mock = Mock::start(vec![]).await;
    let h = harness(false);
    h.connect("cohere", &mock, 0, "co-secret-key").await;

    let mut body = request();
    body["documents"] = json!([]);
    let (status, _, out) = h.post(body, &[]).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{out}");
    assert_eq!(out["error"]["type"], json!("invalid_request_error"));
    assert!(
        out["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("documents must not be empty"),
        "{out}"
    );
    assert_eq!(mock.hits(), 0);
}

#[tokio::test]
async fn an_upstream_error_keeps_the_provider_payload() {
    let mock = Mock::start(vec![(
        401,
        json!({"message": "invalid api token", "id": "9f75d94c"}),
    )])
    .await;
    let h = harness(false);
    h.connect("cohere", &mock, 0, "co-secret-key").await;

    let (status, _, out) = h.post(request(), &[]).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{out}");
    assert_eq!(
        out["error"]["message"],
        json!("invalid api token"),
        "the provider's own message survives, not a formatted summary"
    );

    let rows = h.usage().await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].status, 401);
    assert!(
        rows[0]
            .error
            .as_deref()
            .is_some_and(|error| error.contains("invalid api token")),
        "{:?}",
        rows[0].error
    );
}

#[tokio::test]
async fn auth_is_required_when_configured() {
    let mock = Mock::start(vec![(200, cohere_ok())]).await;
    let h = harness(true);
    h.connect("cohere", &mock, 0, "co-secret-key").await;

    let (status, _, out) = h.post(request(), &[]).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{out}");
    assert_eq!(mock.hits(), 0, "no provider call without a gateway key");
}

#[tokio::test]
async fn the_provider_key_never_appears_in_a_response() {
    const SECRET: &str = "co-secret-7f3a91";
    let ok = Mock::start(vec![(200, cohere_ok())]).await;
    let h = harness(false);
    h.connect("cohere", &ok, 0, SECRET).await;

    let (_, _, body) = h.post(request(), &[]).await;
    assert!(!body.to_string().contains(SECRET), "{body}");
    assert_eq!(
        ok.header(0, "authorization").as_deref(),
        Some("Bearer co-secret-7f3a91")
    );

    // And on the failure path, where error text and the usage row could leak it.
    let dead = Mock::start(vec![(401, json!({"message": "invalid api token"}))]).await;
    let failing = harness(false);
    failing.connect("cohere", &dead, 0, SECRET).await;
    let (status, _, body) = failing.post(request(), &[]).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(!body.to_string().contains(SECRET), "{body}");
    for row in failing.usage().await {
        assert!(
            !row.error.unwrap_or_default().contains(SECRET),
            "a usage row must never carry a credential"
        );
    }
}
