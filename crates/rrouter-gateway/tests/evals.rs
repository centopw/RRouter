//! Evals over HTTP: suites, cases, and runs executed through the gateway's own `/v1` path.
//!
//! The harness mirrors `tests/teams.rs` (admin token + sessions so reader denial is real)
//! and `tests/models.rs` (a mock upstream reached through a connection's base URL). The
//! runner is background work, so a test starts a run and polls it to completion instead of
//! waiting on the start response.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, Method, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use http_body_util::BodyExt;
use rrouter_gateway::{app, auth::Auth, config::Config, AppState};
use rrouter_router::{Breaker, Catalog, Dispatcher, Planner};
use rrouter_store::{Credentials, Store};
use serde_json::{json, Value};
use tower::ServiceExt;

// ----- mock upstream -------------------------------------------------------------

/// What the mock answers, decided from the prompt it was sent.
struct Mock {
    delay: Duration,
    hits: AtomicUsize,
    in_flight: AtomicUsize,
    peak: AtomicUsize,
    last_body: Mutex<String>,
}

impl Mock {
    fn new(delay: Duration) -> Arc<Self> {
        Arc::new(Self {
            delay,
            hits: AtomicUsize::new(0),
            in_flight: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            last_body: Mutex::new(String::new()),
        })
    }

    fn hits(&self) -> usize {
        self.hits.load(Ordering::Relaxed)
    }

    fn peak(&self) -> usize {
        self.peak.load(Ordering::Relaxed)
    }
}

async fn mock_chat(State(mock): State<Arc<Mock>>, body: String) -> Response {
    let current = mock.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
    mock.peak.fetch_max(current, Ordering::SeqCst);
    *mock.last_body.lock().expect("body lock") = body.clone();
    if !mock.delay.is_zero() {
        tokio::time::sleep(mock.delay).await;
    }
    let value: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    let messages = value
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let text = |message: &Value| {
        message
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let system = messages
        .iter()
        .find(|message| message.get("role").and_then(Value::as_str) == Some("system"))
        .map(text)
        .unwrap_or_default();
    let prompt = messages.last().map(text).unwrap_or_default();
    mock.in_flight.fetch_sub(1, Ordering::SeqCst);
    mock.hits.fetch_add(1, Ordering::SeqCst);

    if system.contains("eval judge") {
        let answer = if prompt.contains("JUDGE_BAD") {
            "not json"
        } else if prompt.contains("JUDGE_FAIL") {
            "{\"pass\":false}"
        } else {
            "```json\n{\"pass\":true}\n```"
        };
        return Json(json!({
            "id": "chatcmpl-judge",
            "created": 1,
            "model": "gpt-4.1",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": answer},
                "finish_reason": "stop",
            }],
            "usage": {"prompt_tokens": 3, "completion_tokens": 1},
        }))
        .into_response();
    }

    if prompt.contains("FAIL500") {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": {"message": "upstream exploded"}})),
        )
            .into_response();
    }
    let answer = if prompt.contains("ITALY") {
        "Lyon"
    } else if prompt.contains("ECHOKEY") {
        // The harness passes an API key; a provider echoing it must not reach a result.
        "sk-rr-do-not-echo-this"
    } else {
        "Paris"
    };
    Json(json!({
        "id": "chatcmpl-eval",
        "created": 1,
        "model": "gpt-4.1",
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": answer},
            "finish_reason": "stop",
        }],
        "usage": {"prompt_tokens": 3, "completion_tokens": 1},
    }))
    .into_response()
}

async fn start_mock(delay: Duration) -> (Arc<Mock>, String) {
    let mock = Mock::new(delay);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock");
    let addr = listener.local_addr().expect("addr");
    let router = Router::new()
        .route("/v1/chat/completions", post(mock_chat))
        .with_state(Arc::clone(&mock));
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    (mock, format!("http://{addr}"))
}

// ----- harness -------------------------------------------------------------------

struct H {
    state: Arc<AppState>,
    router: Router,
}

fn config() -> Config {
    Config {
        host: "127.0.0.1".parse().expect("ip"),
        port: 0,
        data_dir: std::env::temp_dir().join(format!("rrouter-evals-{}", uuid::Uuid::new_v4())),
        master_key: None,
        require_api_key: false,
        request_timeout_secs: 30,
        connect_timeout_secs: 5,
        log_filter: "rrouter=error".into(),
        static_dir: None,
        cors_origins: vec![],
        allow_remote_admin: false,
        admin_token: Some("adm-test".into()),
        max_concurrency: 0,
        max_attempts: 4,
        retry_attempts: 1,
        retry_base_ms: 10,
        retry_max_ms: 50,
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

fn harness() -> H {
    let config = config();
    let data_dir = config.data_dir.clone();
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
        dispatcher: Arc::new(Dispatcher::new(Breaker::new()).with_max_attempts(4)),
        http: reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .expect("client"),
        config: rrouter_gateway::config::ConfigSnapshot::from(config),
        started: std::time::Instant::now(),
        oauth: rrouter_auth::OAuthFlows::new(),
        counters: Default::default(),
        concurrency: std::sync::RwLock::new(None),
        auth: Auth::load(&data_dir, false),
    });
    let router = app(Arc::clone(&state));
    H { state, router }
}

impl H {
    async fn call(
        &self,
        method: Method,
        uri: &str,
        body: Option<Value>,
        headers: &[(&str, &str)],
    ) -> (StatusCode, HeaderMap, Value) {
        let mut builder = Request::builder().method(method).uri(uri);
        if body.is_some() {
            builder = builder.header("content-type", "application/json");
        }
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let request = builder
            .body(body.map_or_else(Body::empty, |value| Body::from(value.to_string())))
            .expect("request");
        let res = self.router.clone().oneshot(request).await.expect("oneshot");
        let status = res.status();
        let headers = res.headers().clone();
        let bytes = res.into_body().collect().await.expect("body").to_bytes();
        (
            status,
            headers,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    async fn get(&self, uri: &str, headers: &[(&str, &str)]) -> (StatusCode, Value) {
        let (status, _, body) = self.call(Method::GET, uri, None, headers).await;
        (status, body)
    }

    async fn post(&self, uri: &str, body: Value, headers: &[(&str, &str)]) -> (StatusCode, Value) {
        let (status, _, value) = self.call(Method::POST, uri, Some(body), headers).await;
        (status, value)
    }

    async fn delete(&self, uri: &str, headers: &[(&str, &str)]) -> StatusCode {
        let (status, _, _) = self.call(Method::DELETE, uri, None, headers).await;
        status
    }

    fn setup_code(&self) -> String {
        self.state.auth.code().unwrap_or_default()
    }

    async fn setup(&self) -> String {
        let (status, headers, body) = self
            .call(
                Method::POST,
                "/api/auth/setup",
                Some(json!({
                    "setup_code": self.setup_code(),
                    "name": "Ada Lovelace",
                    "email": "ada@example.com",
                    "password": "correct horse battery",
                })),
                &[],
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "setup: {body}");
        cookie_of(&headers).expect("owner cookie")
    }

    async fn invite(&self, owner: &str, email: &str, role: &str) -> String {
        let (status, body) = self
            .post(
                "/api/users/invites",
                json!({"email": email, "role": role}),
                &csrf(owner),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "invite: {body}");
        let token = body["token"].as_str().expect("token").to_string();
        let (status, headers, signed) = self
            .call(
                Method::POST,
                "/api/auth/signup",
                Some(
                    json!({"token": token, "name": "Reader", "password": "another long password"}),
                ),
                &[],
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "signup: {signed}");
        cookie_of(&headers).expect("session cookie")
    }

    /// Point `provider` at the mock upstream, as an operator would.
    async fn connect(&self, provider: &str, base_url: &str) {
        let creds = Credentials {
            api_key: Some("test-key".into()),
            base_url_override: Some(base_url.to_string()),
            ..Credentials::default()
        };
        self.state
            .store
            .create_connection(provider, "api_key", creds, Some("mock"))
            .await
            .expect("connection");
    }

    /// Start a run and poll until it finishes.
    async fn run_to_completion(&self, owner: &str, suite_id: &str, model: &str) -> Value {
        self.run_to_completion_body(owner, json!({"suite_id": suite_id, "model": model}))
            .await
    }

    async fn run_to_completion_body(&self, owner: &str, body: Value) -> Value {
        let (status, body) = self.post("/api/evals/runs", body, &csrf(owner)).await;
        assert_eq!(status, StatusCode::ACCEPTED, "start: {body}");
        let id = body["data"]["id"].as_str().expect("run id").to_string();
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        loop {
            let (status, body) = self
                .get(&format!("/api/evals/runs/{id}"), &auth(owner))
                .await;
            assert_eq!(status, StatusCode::OK, "{body}");
            let state = body["data"]["status"].as_str().unwrap_or("");
            if state == "completed" || state == "failed" {
                return body;
            }
            assert!(std::time::Instant::now() < deadline, "run stuck: {body}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

fn cookie_of(headers: &HeaderMap) -> Option<String> {
    headers
        .get("set-cookie")
        .and_then(|v| v.to_str().ok())
        .and_then(|raw| raw.split(';').next())
        .map(|pair| pair.trim().to_string())
        .filter(|pair| pair.starts_with("rrouter_session="))
}

fn auth(cookie: &str) -> [(&str, &str); 1] {
    [("cookie", cookie)]
}

fn csrf(cookie: &str) -> [(&str, &str); 2] {
    [("cookie", cookie), ("x-rrouter-csrf", "1")]
}

async fn create_suite(h: &H, owner: &str, name: &str) -> String {
    let (status, body) = h
        .post("/api/evals/suites", json!({"name": name}), &csrf(owner))
        .await;
    assert_eq!(status, StatusCode::CREATED, "suite: {body}");
    body["data"]["id"].as_str().expect("suite id").to_string()
}

async fn add_case(h: &H, owner: &str, suite: &str, input: &str, expected: Option<&str>) -> Value {
    let (status, body) = h
        .post(
            &format!("/api/evals/suites/{suite}/cases"),
            json!({"input": input, "expected": expected}),
            &csrf(owner),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "case: {body}");
    body["data"].clone()
}

// ----- tests ---------------------------------------------------------------------

#[tokio::test]
async fn suite_and_case_round_trip_with_validation() {
    let h = harness();
    let owner = h.setup().await;
    let suite = create_suite(&h, &owner, "support smoke").await;

    let (status, body) = h.get("/api/evals/suites", &auth(&owner)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let rows = body["data"].as_array().expect("data");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["name"], "support smoke");
    assert_eq!(rows[0]["cases"], 0);

    // A duplicate name and an empty input are client errors, not 500s.
    let (status, body) = h
        .post(
            "/api/evals/suites",
            json!({"name": "support smoke"}),
            &csrf(&owner),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, body) = h
        .post(
            "/api/evals/suites",
            json!({"name": "bad context", "system_prompt": 5}),
            &csrf(&owner),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    let case = add_case(
        &h,
        &owner,
        &suite,
        "What is the capital of France?",
        Some("Paris"),
    )
    .await;
    assert_eq!(case["expected"], "Paris");
    assert_eq!(case["input"], "What is the capital of France?");

    let (status, body) = h
        .post(
            &format!("/api/evals/suites/{suite}/cases"),
            json!({"input": "   "}),
            &csrf(&owner),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    let (status, body) = h
        .get(&format!("/api/evals/suites/{suite}"), &auth(&owner))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["cases"].as_array().expect("cases").len(), 1);
    assert_eq!(body["data"]["cases"], 1);

    // Ordering is creation order, which the UI relies on for the results table.
    add_case(&h, &owner, &suite, "Name a colour", None).await;
    let (_, body) = h
        .get(&format!("/api/evals/suites/{suite}"), &auth(&owner))
        .await;
    let inputs: Vec<&str> = body["cases"]
        .as_array()
        .expect("cases")
        .iter()
        .map(|case| case["input"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(inputs[0], "What is the capital of France?");
    assert_eq!(inputs[1], "Name a colour");

    let case_id = case["id"].as_str().expect("case id");
    assert_eq!(
        h.delete(
            &format!("/api/evals/suites/{suite}/cases/{case_id}"),
            &csrf(&owner)
        )
        .await,
        StatusCode::OK
    );
    assert_eq!(
        h.delete(&format!("/api/evals/suites/{suite}"), &csrf(&owner))
            .await,
        StatusCode::OK
    );
    let (_, body) = h.get("/api/evals/suites", &auth(&owner)).await;
    assert_eq!(body["data"].as_array().expect("data").len(), 0);
}

#[tokio::test]
async fn a_run_records_pass_and_fail_in_case_order() {
    let (mock, url) = start_mock(Duration::ZERO).await;
    let h = harness();
    h.connect("openai", &url).await;
    let owner = h.setup().await;
    let suite = create_suite(&h, &owner, "capitals").await;
    add_case(&h, &owner, &suite, "FRANCE capital?", Some("Paris")).await;
    add_case(&h, &owner, &suite, "ITALY capital?", Some("Paris")).await;
    add_case(&h, &owner, &suite, "Name a colour", None).await;

    let (status, started) = h
        .post(
            "/api/evals/runs",
            json!({"suite_id": suite, "model": "openai/gpt-4.1"}),
            &csrf(&owner),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{started}");
    assert_eq!(started["data"]["model"], "openai/gpt-4.1");
    assert_eq!(started["data"]["progress"]["total"], 3);
    // The request is answered before any case is executed: the runner is async.
    let run_id = started["data"]["id"].as_str().expect("run id").to_string();

    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    let finished = loop {
        let (_, body) = h
            .get(&format!("/api/evals/runs/{run_id}"), &auth(&owner))
            .await;
        let state = body["data"]["status"].as_str().unwrap_or("");
        if state == "completed" || state == "failed" {
            break body;
        }
        assert!(std::time::Instant::now() < deadline, "run stuck: {body}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(finished["data"]["status"], "completed");
    let results = finished["results"].as_array().expect("results");
    assert_eq!(results.len(), 3, "{finished}");
    assert_eq!(results[0]["case_input"], "FRANCE capital?");
    assert_eq!(results[0]["passed"], true);
    assert_eq!(results[0]["output"], "Paris");
    assert_eq!(results[1]["case_input"], "ITALY capital?");
    assert_eq!(results[1]["passed"], false);
    assert_eq!(results[1]["output"], "Lyon");
    assert_eq!(results[2]["passed"], serde_json::Value::Null, "unscored");
    assert_eq!(results[2]["score"], serde_json::Value::Null);
    assert!(results[0]["latency_ms"].as_u64().is_some());

    let summary = &finished["data"]["summary"];
    assert_eq!(summary["cases"], 3);
    assert_eq!(summary["passed"], 1);
    assert_eq!(summary["failed"], 1);
    assert_eq!(summary["unscored"], 1);
    assert_eq!(summary["errors"], 0);
    assert_eq!(mock.hits(), 3, "one upstream call per case");
}

#[tokio::test]
async fn a_failing_case_is_recorded_and_the_run_still_completes() {
    let (_mock, url) = start_mock(Duration::ZERO).await;
    let h = harness();
    h.connect("openai", &url).await;
    let owner = h.setup().await;
    let suite = create_suite(&h, &owner, "mixed").await;
    add_case(&h, &owner, &suite, "FRANCE?", Some("Paris")).await;
    add_case(&h, &owner, &suite, "FAIL500 please", Some("anything")).await;
    add_case(&h, &owner, &suite, "FRANCE again?", Some("Paris")).await;

    let finished = h.run_to_completion(&owner, &suite, "openai/gpt-4.1").await;
    assert_eq!(finished["data"]["status"], "completed");
    let results = finished["results"].as_array().expect("results");
    assert_eq!(results.len(), 3);
    let failed_case = &results[1];
    assert_eq!(failed_case["case_input"], "FAIL500 please");
    assert_eq!(failed_case["passed"], serde_json::Value::Null);
    let error = failed_case["error"].as_str().expect("error text");
    assert!(!error.is_empty(), "the case keeps its reason: {error}");
    assert_eq!(finished["data"]["summary"]["errors"], 1);
    assert_eq!(finished["data"]["summary"]["passed"], 2);
}

#[tokio::test]
async fn cases_run_in_parallel_but_bounded() {
    let (mock, url) = start_mock(Duration::from_millis(60)).await;
    let h = harness();
    h.connect("openai", &url).await;
    let owner = h.setup().await;
    let suite = create_suite(&h, &owner, "concurrency").await;
    for index in 0..8 {
        add_case(
            &h,
            &owner,
            &suite,
            &format!("FRANCE {index}?"),
            Some("Paris"),
        )
        .await;
    }

    let finished = h.run_to_completion(&owner, &suite, "openai/gpt-4.1").await;
    assert_eq!(finished["data"]["summary"]["passed"], 8);
    assert_eq!(mock.hits(), 8);
    let peak = mock.peak();
    assert!(peak <= 4, "at most four cases in flight, saw {peak}");
    assert!(peak >= 2, "the runner is parallel, saw {peak}");
}

#[tokio::test]
async fn a_reader_can_view_but_not_change_evals() {
    let h = harness();
    let owner = h.setup().await;
    let suite = create_suite(&h, &owner, "core").await;
    add_case(&h, &owner, &suite, "FRANCE?", Some("Paris")).await;
    let reader = h.invite(&owner, "reader@example.com", "member").await;

    let (status, body) = h.get("/api/evals/suites", &auth(&reader)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"].as_array().expect("data").len(), 1);
    let (status, body) = h
        .get(&format!("/api/evals/suites/{suite}"), &auth(&reader))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, _) = h.get("/api/evals/runs", &auth(&reader)).await;
    assert_eq!(status, StatusCode::OK);

    for (uri, body) in [
        ("/api/evals/suites".to_string(), json!({"name": "nope"})),
        (
            format!("/api/evals/suites/{suite}/cases"),
            json!({"input": "nope"}),
        ),
        (
            "/api/evals/runs".to_string(),
            json!({"suite_id": suite, "model": "openai/gpt-4.1"}),
        ),
    ] {
        let (status, value) = h.post(&uri, body, &csrf(&reader)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{uri}: {value}");
        assert_eq!(value["error"]["code"], "role_required");
    }
    assert_eq!(
        h.delete(&format!("/api/evals/suites/{suite}"), &csrf(&reader))
            .await,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn the_run_credential_never_reaches_a_result() {
    let (_mock, url) = start_mock(Duration::ZERO).await;
    let h = harness();
    h.connect("openai", &url).await;
    let owner = h.setup().await;
    let suite = create_suite(&h, &owner, "echo").await;
    add_case(&h, &owner, &suite, "ECHOKEY", None).await;

    // The key is accepted for the run and used as the inbound credential; the mock echoes
    // it, and the runner must scrub it before it becomes a stored output.
    let secret = "sk-rr-do-not-echo-this";
    let (status, started) = h
        .post(
            "/api/evals/runs",
            json!({"suite_id": suite, "model": "openai/gpt-4.1", "api_key": secret}),
            &csrf(&owner),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{started}");
    assert!(
        !started.to_string().contains(secret),
        "the start response must not echo the key: {started}"
    );

    let run_id = started["data"]["id"].as_str().expect("run id").to_string();
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    let finished = loop {
        let (_, body) = h
            .get(&format!("/api/evals/runs/{run_id}"), &auth(&owner))
            .await;
        if matches!(
            body["data"]["status"].as_str(),
            Some("completed" | "failed")
        ) {
            break body;
        }
        assert!(std::time::Instant::now() < deadline, "run stuck: {body}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    let results = finished["results"].as_array().expect("results");
    assert_eq!(results.len(), 1);
    assert!(
        !finished.to_string().contains(secret),
        "no result, error or summary may contain the run credential: {finished}"
    );
    assert_eq!(results[0]["output"], "[redacted]");
    let (_, run_list) = h.get("/api/evals/runs", &auth(&owner)).await;
    assert!(!run_list.to_string().contains(secret), "{run_list}");
}

/// The suite system prompt is the first message of every case request.
#[tokio::test]
async fn suite_system_prompt_precedes_each_case() {
    let (mock, url) = start_mock(Duration::ZERO).await;
    let h = harness();
    h.connect("openai", &url).await;
    let owner = h.setup().await;
    let (status, created) = h
        .post(
            "/api/evals/suites",
            json!({
                "name": "support policy",
                "system_prompt": "You are support. Cite the refund policy.",
            }),
            &csrf(&owner),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(
        created["data"]["system_prompt"],
        "You are support. Cite the refund policy."
    );
    let suite = created["data"]["id"]
        .as_str()
        .expect("suite id")
        .to_string();
    add_case(&h, &owner, &suite, "Can I get a refund?", Some("30 days")).await;

    let finished = h.run_to_completion(&owner, &suite, "openai/gpt-4.1").await;
    assert_eq!(finished["data"]["status"], "completed");

    let sent: Value =
        serde_json::from_str(&mock.last_body.lock().expect("body lock")).expect("json body");
    let messages = sent["messages"].as_array().expect("messages");
    assert_eq!(messages[0]["role"], "system");
    assert_eq!(
        messages[0]["content"],
        "You are support. Cite the refund policy."
    );
    assert_eq!(messages[1]["role"], "user");
    assert_eq!(messages[1]["content"], "Can I get a refund?");

    // `null` clears the context, and the view says so.
    let (status, _, cleared) = h
        .call(
            Method::PATCH,
            &format!("/api/evals/suites/{suite}"),
            Some(json!({"system_prompt": null})),
            &csrf(&owner),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{cleared}");
    assert_eq!(cleared["data"]["system_prompt"], Value::Null);
}

/// A run-level judge replaces containment with an LLM verdict. The judge call is an
/// ordinary gateway completion, so the mock sees two upstream calls per scored case.
#[tokio::test]
async fn judge_model_scores_criteria_and_round_trips_on_the_run() {
    let (mock, url) = start_mock(Duration::ZERO).await;
    let h = harness();
    h.connect("openai", &url).await;
    let owner = h.setup().await;
    let suite = create_suite(&h, &owner, "judged").await;
    add_case(&h, &owner, &suite, "first case", Some("JUDGE_PASS")).await;
    add_case(&h, &owner, &suite, "second case", Some("JUDGE_FAIL")).await;

    let (status, body) = h
        .post(
            "/api/evals/runs",
            json!({"suite_id": suite, "model": "openai/gpt-4.1", "judge_model": true}),
            &csrf(&owner),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    let finished = h
        .run_to_completion_body(
            &owner,
            json!({
                "suite_id": suite,
                "model": "openai/gpt-4.1",
                "judge_model": "openai/gpt-4.1",
            }),
        )
        .await;
    assert_eq!(finished["data"]["status"], "completed", "{finished}");
    assert_eq!(finished["data"]["judge_model"], "openai/gpt-4.1");
    let results = finished["results"].as_array().expect("results");
    assert_eq!(results.len(), 2);
    assert_eq!(results[0]["score"], 1.0);
    assert_eq!(results[0]["passed"], true);
    assert_eq!(results[1]["score"], 0.0);
    assert_eq!(results[1]["passed"], false);
    let summary = &finished["data"]["summary"];
    assert_eq!(summary["passed"], 1);
    assert_eq!(summary["failed"], 1);
    assert_eq!(summary["errors"], 0);
    assert_eq!(mock.hits(), 4, "two cases plus two judge calls");
}

/// A judge that answers garbage becomes a case error, never a guessed score.
#[tokio::test]
async fn judge_parse_failure_is_a_case_error_not_a_score() {
    let (_mock, url) = start_mock(Duration::ZERO).await;
    let h = harness();
    h.connect("openai", &url).await;
    let owner = h.setup().await;
    let suite = create_suite(&h, &owner, "bad judge").await;
    add_case(&h, &owner, &suite, "needs judge", Some("JUDGE_BAD")).await;

    let finished = h
        .run_to_completion_body(
            &owner,
            json!({
                "suite_id": suite,
                "model": "openai/gpt-4.1",
                "judge_model": "openai/gpt-4.1",
            }),
        )
        .await;
    let result = &finished["results"][0];
    assert_eq!(result["score"], Value::Null, "{finished}");
    assert_eq!(result["passed"], Value::Null, "{finished}");
    let error = result["error"].as_str().expect("error text");
    assert!(error.contains("judge"), "{error}");
    assert_eq!(finished["data"]["summary"]["errors"], 1, "{finished}");
}
