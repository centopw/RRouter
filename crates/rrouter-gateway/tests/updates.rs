//! Update checks against a mock GitHub: conditional requests, redirect fallback, token
//! handling, and rate-limit backoff. No test reaches the real network.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Router;
use rrouter_gateway::config::Config;
use rrouter_gateway::updates::{self, Endpoints};
use rrouter_gateway::{auth::Auth, AppState};
use rrouter_router::{Breaker, Catalog, Dispatcher, Planner};
use rrouter_store::Store;
use serde_json::{json, Value};

/// The module keeps process-wide status; tests take this lock so scenarios cannot
/// interleave with each other. `tokio::sync::Mutex` so the guard can be held across the
/// awaits of a test without the `await_holding_lock` lint firing.
async fn serial() -> tokio::sync::MutexGuard<'static, ()> {
    static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    LOCK.lock().await
}

#[derive(Clone)]
enum Reply {
    Status(u16),
    Json {
        status: u16,
        headers: Vec<(String, String)>,
        body: Value,
    },
    Text {
        status: u16,
        body: String,
    },
    Redirect {
        status: u16,
        location: String,
    },
}

#[derive(Clone)]
struct Record {
    path: String,
    headers: HeaderMap,
}

#[derive(Default)]
struct Mock {
    replies: Mutex<VecDeque<Reply>>,
    requests: Mutex<Vec<Record>>,
    hits: AtomicUsize,
}

impl Mock {
    fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn plan(&self, replies: Vec<Reply>) {
        *self.replies.lock().expect("replies") = replies.into();
    }

    fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }

    fn requests(&self) -> Vec<Record> {
        self.requests.lock().expect("requests").clone()
    }

    fn paths(&self) -> Vec<String> {
        self.requests()
            .into_iter()
            .map(|record| record.path)
            .collect()
    }
}

async fn handle(State(mock): State<Arc<Mock>>, request: Request<Body>) -> Response {
    mock.hits.fetch_add(1, Ordering::SeqCst);
    mock.requests.lock().expect("requests").push(Record {
        path: request.uri().path().to_string(),
        headers: request.headers().clone(),
    });
    let reply = mock.replies.lock().expect("replies").pop_front();
    match reply.unwrap_or(Reply::Status(500)) {
        Reply::Status(status) => StatusCode::from_u16(status)
            .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR)
            .into_response(),
        Reply::Json {
            status,
            headers,
            body,
        } => {
            let mut response = (
                StatusCode::from_u16(status).unwrap_or(StatusCode::OK),
                axum::Json(body),
            )
                .into_response();
            for (name, value) in headers {
                if let (Ok(name), Ok(value)) = (
                    axum::http::HeaderName::from_bytes(name.as_bytes()),
                    axum::http::HeaderValue::from_str(&value),
                ) {
                    response.headers_mut().insert(name, value);
                }
            }
            response
        }
        Reply::Text { status, body } => {
            (StatusCode::from_u16(status).unwrap_or(StatusCode::OK), body).into_response()
        }
        Reply::Redirect { status, location } => {
            let mut response = StatusCode::from_u16(status)
                .unwrap_or(StatusCode::FOUND)
                .into_response();
            if let Ok(value) = axum::http::HeaderValue::from_str(&location) {
                response
                    .headers_mut()
                    .insert(axum::http::header::LOCATION, value);
            }
            response
        }
    }
}

async fn mock_server() -> (Arc<Mock>, String) {
    let mock = Mock::new();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    let router = Router::new().fallback(handle).with_state(Arc::clone(&mock));
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    (mock, format!("http://{addr}"))
}

fn endpoints(base: &str) -> Endpoints {
    Endpoints {
        api: format!("{base}/api/latest"),
        releases_latest: format!("{base}/releases/latest"),
        release_tag_base: format!("{base}/releases/tag"),
        download_base: format!("{base}/download"),
    }
}

fn config() -> Config {
    Config {
        host: "127.0.0.1".parse().expect("ip"),
        port: 0,
        data_dir: std::env::temp_dir().join(format!("rrouter-updates-{}", uuid::Uuid::new_v4())),
        master_key: None,
        require_api_key: false,
        request_timeout_secs: 10,
        connect_timeout_secs: 5,
        log_filter: "rrouter=error".into(),
        static_dir: None,
        cors_origins: vec![],
        allow_remote_admin: false,
        admin_token: Some("adm-test".into()),
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

fn harness() -> Arc<AppState> {
    let config = config();
    let data_dir = config.data_dir.clone();
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("client");
    Arc::new(AppState {
        store: Arc::new(Store::open_in_memory().expect("store")),
        catalog: Arc::new(Catalog::builtin()),
        refresher: Arc::new(rrouter_auth::Refresher::new(http.clone())),
        planner: Planner::new(),
        dispatcher: Arc::new(Dispatcher::new(Breaker::new()).with_max_attempts(4)),
        http,
        config: rrouter_gateway::config::ConfigSnapshot::from(config),
        started: std::time::Instant::now(),
        oauth: rrouter_auth::OAuthFlows::new(),
        counters: Default::default(),
        concurrency: std::sync::RwLock::new(None),
        auth: Auth::load(&data_dir, false),
    })
}

fn release_json(version: &str, notes: &str) -> Value {
    json!({
        "tag_name": format!("v{version}"),
        "body": notes,
        "published_at": "2026-01-01T00:00:00Z",
        "html_url": format!("https://github.com/centopw/RRouter/releases/tag/v{version}"),
        "assets": [],
    })
}

#[tokio::test]
async fn conditional_get_reuses_the_cached_release_on_304() {
    let _guard = serial().await;
    updates::reset_for_tests();
    let (mock, base) = mock_server().await;
    let state = harness();

    mock.plan(vec![
        Reply::Json {
            status: 200,
            headers: vec![("etag".into(), "\"v1\"".into())],
            body: release_json("9.9.9", "notes from the API"),
        },
        Reply::Status(304),
    ]);
    let first = updates::check_with(&state, &endpoints(&base), Some("gh-token"), false, true)
        .await
        .expect("first check");
    assert_eq!(first["latest_version"], "9.9.9");
    assert_eq!(first["notes"], "notes from the API");
    assert_eq!(first["error"], Value::Null);

    let second = updates::check_with(&state, &endpoints(&base), Some("gh-token"), false, true)
        .await
        .expect("304 is not an error");
    assert_eq!(second["latest_version"], "9.9.9", "cached release is kept");
    assert_eq!(second["notes"], "notes from the API");
    assert!(second["checked_at"].as_i64().is_some());

    let requests = mock.requests();
    assert_eq!(requests.len(), 2);
    let second_request = &requests[1];
    assert_eq!(
        second_request
            .headers
            .get("if-none-match")
            .and_then(|value| value.to_str().ok()),
        Some("\"v1\""),
        "the stored ETag is sent as a conditional request"
    );
    assert_eq!(
        second_request
            .headers
            .get("authorization")
            .and_then(|value| value.to_str().ok()),
        Some("Bearer gh-token"),
        "the configured token is sent as a bearer header"
    );
}

#[tokio::test]
async fn without_a_token_discovery_uses_the_release_redirect_and_never_calls_the_api() {
    let _guard = serial().await;
    updates::reset_for_tests();
    let (mock, base) = mock_server().await;
    let state = harness();

    mock.plan(vec![
        Reply::Redirect {
            status: 302,
            location: format!("{base}/releases/tag/v9.9.8"),
        },
        Reply::Text {
            status: 200,
            body: "release page".into(),
        },
    ]);
    let status = updates::check_with(&state, &endpoints(&base), None, false, true)
        .await
        .expect("redirect discovery");
    assert_eq!(status["latest_version"], "9.9.8");
    assert_eq!(status["notes"], Value::Null, "a redirect carries no notes");
    assert_eq!(status["release_url"], format!("{base}/releases/tag/v9.9.8"));
    let paths = mock.paths();
    assert!(
        !paths.iter().any(|path| path == "/api/latest"),
        "no API request without a token: {paths:?}"
    );
    assert_eq!(
        paths,
        vec![
            "/releases/latest".to_string(),
            "/releases/tag/v9.9.8".to_string()
        ]
    );
}

#[tokio::test]
async fn a_rate_limited_api_sets_backoff_and_a_second_automatic_check_sends_nothing() {
    let _guard = serial().await;
    updates::reset_for_tests();
    let (mock, base) = mock_server().await;
    let state = harness();
    let reset = rrouter_gateway::now_unix() + 3_600;

    mock.plan(vec![
        Reply::Json {
            status: 403,
            headers: vec![
                ("x-ratelimit-remaining".into(), "0".into()),
                ("x-ratelimit-reset".into(), reset.to_string()),
            ],
            body: json!({"message": "API rate limit exceeded for 203.0.113.9"}),
        },
        Reply::Redirect {
            status: 302,
            location: format!("{base}/releases/tag/v9.9.7"),
        },
        Reply::Text {
            status: 200,
            body: "tag".into(),
        },
        // The manual check during backoff falls back to the redirect again.
        Reply::Redirect {
            status: 302,
            location: format!("{base}/releases/tag/v9.9.7"),
        },
        Reply::Text {
            status: 200,
            body: "tag".into(),
        },
    ]);
    let first = updates::check_with(&state, &endpoints(&base), Some("gh-token"), false, true)
        .await
        .expect("a rate-limited check still discovers the version");
    assert_eq!(
        first["latest_version"], "9.9.7",
        "the redirect fallback served the version"
    );
    let until = first["rate_limited_until"]
        .as_i64()
        .expect("backoff timestamp");
    assert!(
        until >= reset && until <= reset + 30,
        "reset plus jitter: {until} vs {reset}"
    );
    assert!(
        first["error"]
            .as_str()
            .is_some_and(|text| text.contains(&until.to_string())),
        "status explains when the API retries: {first}"
    );
    assert!(
        !first.to_string().contains("API rate limit exceeded"),
        "no upstream body in status"
    );

    let hits_after_first = mock.hits();
    let automatic = updates::check_with(&state, &endpoints(&base), Some("gh-token"), false, false)
        .await
        .expect("automatic check during backoff");
    assert_eq!(automatic["rate_limited_until"], first["rate_limited_until"]);
    assert_eq!(mock.hits(), hits_after_first, "no request before the reset");

    let second_manual =
        updates::check_with(&state, &endpoints(&base), Some("gh-token"), false, true)
            .await
            .expect("manual check during backoff uses the redirect");
    assert_eq!(second_manual["latest_version"], "9.9.7");
    assert_eq!(
        mock.requests()
            .iter()
            .filter(|record| record.path == "/api/latest")
            .count(),
        1,
        "the API is called exactly once before the reset"
    );
    assert!(
        second_manual["error"]
            .as_str()
            .is_some_and(|text| text.contains("rate limit")),
        "manual check explains the backoff: {second_manual}"
    );
}

#[tokio::test]
async fn a_malformed_reset_uses_the_safe_default_backoff() {
    let _guard = serial().await;
    updates::reset_for_tests();
    let (mock, base) = mock_server().await;
    let state = harness();

    mock.plan(vec![
        Reply::Json {
            status: 403,
            headers: vec![
                ("x-ratelimit-remaining".into(), "0".into()),
                ("x-ratelimit-reset".into(), "not-a-number".into()),
            ],
            body: json!({"message": "rate limited"}),
        },
        Reply::Redirect {
            status: 302,
            location: format!("{base}/releases/tag/v9.9.6"),
        },
        Reply::Text {
            status: 200,
            body: "tag".into(),
        },
    ]);
    let now = rrouter_gateway::now_unix();
    let status = updates::check_with(&state, &endpoints(&base), Some("t"), false, true)
        .await
        .expect("malformed reset is not fatal");
    let until = status["rate_limited_until"]
        .as_i64()
        .expect("fallback backoff");
    assert!(
        until > now && until <= now + 15 * 60 + 30,
        "fallback backoff is bounded: {until}"
    );
    assert_eq!(status["latest_version"], "9.9.6");
}

#[tokio::test]
async fn an_api_failure_falls_back_and_keeps_cached_notes_while_never_echoing_the_token() {
    let _guard = serial().await;
    updates::reset_for_tests();
    let (mock, base) = mock_server().await;
    let state = harness();
    let token = "ghp_secret_token_value";

    // Seed notes through a successful API call first.
    mock.plan(vec![Reply::Json {
        status: 200,
        headers: vec![],
        body: release_json("9.9.5", "cached notes"),
    }]);
    updates::check_with(&state, &endpoints(&base), Some(token), false, true)
        .await
        .expect("seed");

    // Then the API breaks; discovery falls back to the redirect.
    mock.plan(vec![
        Reply::Status(500),
        Reply::Redirect {
            status: 302,
            location: format!("{base}/releases/tag/v9.9.4"),
        },
        Reply::Text {
            status: 200,
            body: "tag".into(),
        },
    ]);
    let status = updates::check_with(&state, &endpoints(&base), Some(token), false, true)
        .await
        .expect("fallback after an API failure");
    assert_eq!(status["latest_version"], "9.9.4");
    assert_eq!(
        status["notes"], "cached notes",
        "cached notes survive the fallback"
    );
    assert!(
        status["error"]
            .as_str()
            .is_some_and(|text| text.contains("HTTP 500")),
        "{status}"
    );
    assert!(
        !status.to_string().contains(token),
        "the token never reaches the status"
    );
}

#[tokio::test]
async fn download_urls_follow_the_release_layout() {
    let base = "https://github.com/centopw/RRouter/releases/download";
    assert_eq!(
        updates::download_url("v1.2.3", "rrouter-linux-amd64.tar.gz", base),
        "https://github.com/centopw/RRouter/releases/download/v1.2.3/rrouter-linux-amd64.tar.gz"
    );
    assert_eq!(
        updates::download_url("v1.2.3", "rrouter-darwin-arm64.tar.gz", &format!("{base}/")),
        "https://github.com/centopw/RRouter/releases/download/v1.2.3/rrouter-darwin-arm64.tar.gz"
    );
}

#[tokio::test]
async fn status_json_keeps_existing_fields_and_adds_backoff_fields() {
    let _guard = serial().await;
    updates::reset_for_tests();
    let state = harness();
    let status = updates::snapshot(&state).await;
    for field in [
        "current_version",
        "latest_version",
        "release_url",
        "notes",
        "published_at",
        "checked_at",
        "phase",
        "error",
        "downloaded",
        "deployment",
        "options",
        "rate_limited_until",
        "last_error",
    ] {
        assert!(status.get(field).is_some(), "status has {field}: {status}");
    }
}
