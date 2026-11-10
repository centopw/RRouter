//! `/v1/search` end to end: the real axum app over `oneshot`, talking to a real mock
//! upstream through `base_url_override` — the same path a connected provider takes.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::State;
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};
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
        // A fallback handler accepts every provider path: Messages, Responses and Gemini's
        // model-scoped generateContent path.
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
        data_dir: std::env::temp_dir().join(format!("rrouter-search-{}", uuid::Uuid::new_v4())),
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

fn build_state(config: Config) -> AppState {
    let data_dir = config.data_dir.clone();
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
    async fn connect_with(&self, provider: &str, mock: &Mock, priority: i32, creds: Credentials) {
        let creds = Credentials {
            base_url_override: Some(mock.url()),
            ..creds
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

    async fn connect(&self, provider: &str, mock: &Mock, priority: i32, api_key: &str) {
        self.connect_with(
            provider,
            mock,
            priority,
            Credentials {
                api_key: Some(api_key.to_string()),
                ..Credentials::default()
            },
        )
        .await;
    }

    /// OAuth providers authenticate with the access token, not the API-key field.
    async fn connect_oauth(&self, provider: &str, mock: &Mock, priority: i32, access_token: &str) {
        self.connect_with(
            provider,
            mock,
            priority,
            Credentials {
                access_token: Some(access_token.to_string()),
                ..Credentials::default()
            },
        )
        .await;
    }

    async fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<Value>,
        headers: &[(&str, &str)],
    ) -> (StatusCode, axum::http::HeaderMap, Value) {
        let mut builder = Request::builder().method(method).uri(path);
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let body = match body {
            Some(body) => {
                builder = builder.header("content-type", "application/json");
                Body::from(body.to_string())
            }
            None => Body::empty(),
        };
        let response = self
            .router
            .clone()
            .oneshot(builder.body(body).expect("request"))
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

    async fn post(
        &self,
        body: Value,
        headers: &[(&str, &str)],
    ) -> (StatusCode, axum::http::HeaderMap, Value) {
        self.request("POST", "/v1/search", Some(body), headers)
            .await
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

fn request(model: &str) -> Value {
    json!({
        "model": model,
        "query": "What changed in the latest Rust release?",
        "max_results": 2,
        "domain_filter": ["blog.rust-lang.org"]
    })
}

fn anthropic_ok() -> Value {
    json!({
        "id": "msg_01",
        "type": "message",
        "role": "assistant",
        "content": [
            {
                "type": "server_tool_use",
                "id": "srvtoolu_01",
                "name": "web_search",
                "input": {"query": "latest rust release"}
            },
            {
                "type": "web_search_tool_result",
                "tool_use_id": "srvtoolu_01",
                "content": [
                    {
                        "type": "web_search_result",
                        "url": "https://blog.rust-lang.org/2026/01/01/Rust-1.90.0.html",
                        "title": "Announcing Rust 1.90.0",
                        "page_age": "2026-01-01T00:00:00Z"
                    },
                    {
                        "type": "web_search_result",
                        "url": "https://docs.rs/std",
                        "title": "std docs"
                    }
                ]
            },
            {"type": "text", "text": "Rust 1.90 was released."}
        ],
        "usage": {"input_tokens": 120, "output_tokens": 40, "cache_read_input_tokens": 12}
    })
}

fn openai_ok() -> Value {
    json!({
        "id": "resp_01",
        "object": "response",
        "output": [
            {
                "type": "web_search_call",
                "id": "ws_01",
                "status": "completed",
                "action": {
                    "type": "search",
                    "query": "latest rust release",
                    "sources": [
                        {"url": "https://blog.rust-lang.org/release", "title": "Rust release"}
                    ]
                }
            },
            {
                "type": "message",
                "role": "assistant",
                "content": [
                    {
                        "type": "output_text",
                        "text": "The release is out.",
                        "annotations": [
                            {
                                "type": "url_citation",
                                "url": "https://blog.rust-lang.org/release",
                                "title": "Rust release"
                            },
                            {
                                "type": "url_citation",
                                "url": "https://docs.rs/std",
                                "title": "std docs"
                            }
                        ]
                    }
                ]
            }
        ],
        "usage": {
            "input_tokens": 80,
            "output_tokens": 20,
            "input_tokens_details": {"cached_tokens": 5}
        }
    })
}

fn gemini_ok() -> Value {
    json!({
        "candidates": [{
            "content": {"parts": [{"text": "Rust 1.90 is out."}], "role": "model"},
            "groundingMetadata": {
                "groundingChunks": [
                    {"web": {"uri": "https://blog.rust-lang.org/release", "title": "Rust release"}},
                    {"web": {"uri": "https://docs.rs/std", "title": "std docs"}}
                ],
                "groundingSupports": [{
                    "segment": {"text": "Rust 1.90 is out."},
                    "groundingChunkIndices": [0, 1]
                }]
            }
        }],
        "usageMetadata": {
            "promptTokenCount": 55,
            "candidatesTokenCount": 11,
            "cachedContentTokenCount": 4
        }
    })
}

// ------------------------------------------------------------------------------- tests

#[tokio::test]
async fn claude_search_returns_citations_and_records_usage() {
    let mock = Mock::start(vec![(200, anthropic_ok())]).await;
    let h = harness(false);
    h.connect_oauth("claude", &mock, 0, "oauth-token").await;

    let (status, headers, body) = h.post(request("claude"), &[]).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        headers.contains_key("x-request-id"),
        "responses carry an id"
    );

    assert_eq!(body["provider"], json!("claude"));
    assert_eq!(
        body["model"],
        json!("claude-sonnet-4-5"),
        "a provider id expands to its default search model"
    );
    assert_eq!(
        body["query"],
        json!("What changed in the latest Rust release?")
    );
    assert_eq!(body["answer"]["text"], json!("Rust 1.90 was released."));
    assert_eq!(body["results"].as_array().expect("results").len(), 2);
    assert_eq!(
        body["results"][0]["url"],
        json!("https://blog.rust-lang.org/2026/01/01/Rust-1.90.0.html")
    );
    assert_eq!(
        body["results"][0]["published_at"],
        json!("2026-01-01T00:00:00Z")
    );
    assert_eq!(body["results"][0]["citation"]["provider"], json!("claude"));
    assert_eq!(body["usage"]["input_tokens"], json!(120));
    assert_eq!(body["usage"]["output_tokens"], json!(40));
    assert_eq!(body["usage"]["llm_tokens"], json!(160));

    // The provider saw the Messages body at its native path, with the OAuth token.
    let seen = mock.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].0, "/v1/messages");
    assert_eq!(
        seen[0].1["model"],
        json!("claude-sonnet-4-5"),
        "the provider receives its own id, not the routed ref"
    );
    assert_eq!(seen[0].1["tools"][0]["type"], json!("web_search_20250305"));
    assert_eq!(
        seen[0].1["tools"][0]["allowed_domains"],
        json!(["blog.rust-lang.org"])
    );
    assert_eq!(
        mock.header(0, "authorization").as_deref(),
        Some("Bearer oauth-token")
    );

    // One row, the search endpoint named, tokens recorded and priced as far as the
    // registry can price them (the test store has no price rows, so zero).
    let rows = h.usage().await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].endpoint.as_deref(), Some("/v1/search"));
    assert_eq!(rows[0].provider, "claude");
    assert_eq!(rows[0].model, "claude/claude-sonnet-4-5");
    assert_eq!(rows[0].status, 200);
    assert_eq!(rows[0].input_tokens, 120);
    assert_eq!(rows[0].output_tokens, 40);
    assert_eq!(rows[0].cache_read_tokens, 12);
    assert_eq!(rows[0].error, None);

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
        attempts[0].model, "claude-sonnet-4-5",
        "the attempt log names the id actually sent upstream"
    );
}

#[tokio::test]
async fn openai_search_uses_responses_and_dedupes_citations() {
    let mock = Mock::start(vec![(200, openai_ok())]).await;
    let h = harness(false);
    h.connect("openai", &mock, 0, "sk-secret").await;

    let (status, _, body) = h.post(request("openai"), &[]).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["provider"], json!("openai"));
    assert_eq!(body["model"], json!("gpt-5-mini"));
    assert_eq!(body["results"].as_array().expect("results").len(), 2);
    assert_eq!(body["usage"]["input_tokens"], json!(80));
    assert_eq!(body["usage"]["output_tokens"], json!(20));

    let seen = mock.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].0, "/v1/responses");
    assert_eq!(seen[0].1["tools"][0]["type"], json!("web_search"));
    assert_eq!(
        seen[0].1["tools"][0]["filters"]["allowed_domains"],
        json!(["blog.rust-lang.org"])
    );
    assert_eq!(
        mock.header(0, "authorization").as_deref(),
        Some("Bearer sk-secret")
    );
}

#[tokio::test]
async fn gemini_search_uses_google_grounding_and_native_api_key_auth() {
    let mock = Mock::start(vec![(200, gemini_ok())]).await;
    let h = harness(false);
    h.connect("gemini", &mock, 0, "gem-secret").await;

    let (status, _, body) = h.post(request("gemini"), &[]).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["provider"], json!("gemini"));
    assert_eq!(body["model"], json!("gemini-2.5-flash"));
    assert_eq!(body["results"].as_array().expect("results").len(), 2);
    assert_eq!(body["usage"]["input_tokens"], json!(55));

    let seen = mock.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(
        seen[0].0, "/v1beta/models/gemini-2.5-flash:generateContent",
        "the model is part of Gemini's native path"
    );
    assert_eq!(seen[0].1["tools"][0]["google_search"], json!({}));
    assert_eq!(
        mock.header(0, "x-goog-api-key").as_deref(),
        Some("gem-secret"),
        "Gemini's native endpoint uses x-goog-api-key, not Bearer"
    );
    assert!(
        mock.header(0, "authorization").is_none(),
        "the generic Bearer header is removed for Gemini"
    );
}

#[tokio::test]
async fn a_bare_model_routes_to_a_search_capable_provider() {
    let openai = Mock::start(vec![(200, openai_ok())]).await;
    let github = Mock::start(vec![(200, openai_ok())]).await;
    let h = harness(false);
    // Both providers can serve gpt-4.1, but only OpenAI has native web search.
    h.connect("openai", &openai, 0, "sk-secret").await;
    h.connect("github", &github, 1, "gh-token").await;

    let (status, _, body) = h.post(request("gpt-4.1"), &[]).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["provider"], json!("openai"));
    assert_eq!(openai.hits(), 1);
    assert_eq!(github.hits(), 0, "a provider without search is filtered");
}

#[tokio::test]
async fn an_alias_to_a_provider_id_gets_the_default_search_model() {
    let mock = Mock::start(vec![(200, anthropic_ok())]).await;
    let h = harness(false);
    h.connect_oauth("claude", &mock, 0, "oauth-token").await;
    h.state
        .store
        .upsert_alias("fast", "claude")
        .await
        .expect("alias");

    let (status, _, body) = h.post(request("fast"), &[]).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["provider"], json!("claude"));
    assert_eq!(body["model"], json!("claude-sonnet-4-5"));
    assert_eq!(mock.seen()[0].1["model"], json!("claude-sonnet-4-5"));
}

#[tokio::test]
async fn a_provider_without_search_answers_501_without_calling_upstream() {
    let mock = Mock::start(vec![]).await;
    let h = harness(false);
    h.connect("groq", &mock, 0, "gsk-secret").await;

    let (status, _, body) = h.post(request("groq"), &[]).await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("web search"),
        "{body}"
    );
    assert_eq!(mock.hits(), 0, "no guessed tool or endpoint was called");
    assert!(
        h.usage().await.is_empty(),
        "nothing was called, nothing is billed"
    );
}

#[tokio::test]
async fn a_failed_account_falls_back_to_the_next_connection() {
    let dead = Mock::start(vec![(
        500,
        json!({"error": {"message": "upstream exploded"}}),
    )])
    .await;
    let live = Mock::start(vec![(200, anthropic_ok())]).await;
    let h = harness(false);
    h.connect_oauth("claude", &dead, 0, "oauth-token").await;
    h.connect_oauth("claude", &live, 1, "oauth-token").await;

    let (status, headers, body) = h.post(request("claude"), &[]).await;
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
    let statuses: Vec<u16> = attempts.iter().map(|attempt| attempt.status).collect();
    assert_eq!(
        statuses,
        vec![500, 200],
        "the fallback is visible in the log"
    );

    let rows = h.usage().await;
    assert_eq!(rows.len(), 1, "one terminal usage row for the request");
    assert_eq!(rows[0].status, 200);
    assert_eq!(rows[0].provider, "claude");
}

#[tokio::test]
async fn an_invalid_body_is_a_400_in_the_client_envelope() {
    let mock = Mock::start(vec![]).await;
    let h = harness(false);
    h.connect("claude", &mock, 0, "oauth-token").await;

    let mut body = request("claude");
    body["domain_filter"] = json!(["https://example.com/path"]);
    let (status, _, out) = h.post(body, &[]).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{out}");
    assert_eq!(out["error"]["type"], json!("invalid_request_error"));
    assert!(
        out["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("must be a domain, not a URL"),
        "{out}"
    );
    assert_eq!(mock.hits(), 0);
}

#[tokio::test]
async fn auth_is_required_when_configured() {
    let mock = Mock::start(vec![(200, anthropic_ok())]).await;
    let h = harness(true);
    h.connect_oauth("claude", &mock, 0, "oauth-token").await;

    let (status, _, out) = h.post(request("claude"), &[]).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{out}");
    assert_eq!(mock.hits(), 0, "no provider call without a gateway key");
}

#[tokio::test]
async fn search_model_discovery_lists_only_connected_search_providers() {
    let claude = Mock::start(vec![]).await;
    let groq = Mock::start(vec![]).await;
    let h = harness(false);
    h.connect_oauth("claude", &claude, 0, "oauth-token").await;
    h.connect("groq", &groq, 1, "gsk-secret").await;

    let (status, _, body) = h.request("GET", "/v1/models/search", None, &[]).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let data = body["data"].as_array().expect("data");
    assert_eq!(data.len(), 1, "{body}");
    assert_eq!(data[0]["id"], json!("claude"));
    assert_eq!(data[0]["kind"], json!("webSearch"));
    assert_eq!(data[0]["default_model"], json!("claude-sonnet-4-5"));
}
