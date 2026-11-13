//! Webhooks over HTTP and through the delivery worker.
//!
//! A real axum receiver stands in for the operator's endpoint: it records every request,
//! verifies the HMAC exactly as a receiver would, and can fail the first N attempts so the
//! retry path is exercised with a fixed clock instead of sleeps. The worker's network
//! entry point is `webhooks::deliver_due`, so each test drives one poll tick explicitly.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{HeaderMap, Method, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::Router;
use http_body_util::BodyExt;
use rrouter_gateway::{app, auth::Auth, config::Config, events, AppState};
use rrouter_router::{Breaker, Catalog, Dispatcher, Planner};
use rrouter_store::Store;
use serde_json::{json, Value};
use tokio::sync::Notify;
use tower::ServiceExt;

const SECRET_ENV: &str = "RR_WEBHOOK_TEST_SECRET_8D";
const SECRET: &[u8] = b"shhh-8d-do-not-return-this";

fn set_secret() {
    std::env::set_var(
        SECRET_ENV,
        std::str::from_utf8(SECRET).expect("utf-8 secret"),
    );
}

// ----- mock receiver -------------------------------------------------------------

#[derive(Debug, Default)]
struct Received {
    valid: bool,
    event: String,
    delivery: String,
    body: String,
}

struct Receiver {
    hits: AtomicUsize,
    fail_first: AtomicUsize,
    received: Mutex<Vec<Received>>,
}

impl Receiver {
    fn new(fail_first: usize) -> Arc<Self> {
        Arc::new(Self {
            hits: AtomicUsize::new(0),
            fail_first: AtomicUsize::new(fail_first),
            received: Mutex::new(Vec::new()),
        })
    }

    fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }

    fn received(&self) -> Vec<Received> {
        let guard = self.received.lock().expect("received lock");
        guard
            .iter()
            .map(|r| Received {
                valid: r.valid,
                event: r.event.clone(),
                delivery: r.delivery.clone(),
                body: r.body.clone(),
            })
            .collect()
    }
}

fn header(headers: &HeaderMap, name: &str) -> String {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string()
}

async fn receive(State(rx): State<Arc<Receiver>>, headers: HeaderMap, body: Bytes) -> Response {
    // The worker stamps the attempt time, which tests advance on a simulated clock; a wide
    // window keeps this test about the MAC and headers, not about wall-clock skew (the
    // freshness rule is covered in `events` unit tests).
    // Verify over the exact bytes with the attempt timestamp the sender put inside the
    // MAC (the freshness window itself is covered in `events` unit tests).
    let signature = header(&headers, events::HEADER_SIGNATURE);
    let timestamp = signature
        .split(',')
        .next()
        .and_then(|part| part.strip_prefix("t="))
        .and_then(|raw| raw.parse::<i64>().ok())
        .unwrap_or_default();
    let valid = events::verify_with_window(SECRET, &signature, &body, timestamp, 1).is_ok();
    let remaining = rx.fail_first.load(Ordering::SeqCst);
    if remaining > 0 {
        rx.fail_first.fetch_sub(1, Ordering::SeqCst);
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    rx.received.lock().expect("received lock").push(Received {
        valid,
        event: header(&headers, events::HEADER_EVENT),
        delivery: header(&headers, events::HEADER_DELIVERY),
        body: String::from_utf8_lossy(&body).to_string(),
    });
    rx.hits.fetch_add(1, Ordering::SeqCst);
    StatusCode::OK.into_response()
}

async fn start_receiver(fail_first: usize) -> (Arc<Receiver>, String) {
    let receiver = Receiver::new(fail_first);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind receiver");
    let addr = listener.local_addr().expect("addr");
    let router = Router::new()
        .route("/hook", post(receive))
        .with_state(Arc::clone(&receiver));
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    (receiver, format!("http://{addr}/hook"))
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
        data_dir: std::env::temp_dir().join(format!("rrouter-webhooks-{}", uuid::Uuid::new_v4())),
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
    set_secret();
    let config = config();
    let data_dir = config.data_dir.clone();
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("client");
    let state = Arc::new(AppState {
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
        envelope_drops: Default::default(),
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

    async fn admin(&self, method: Method, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
        let (status, _, value) = self
            .call(method, uri, body, &[("authorization", "Bearer adm-test")])
            .await;
        (status, value)
    }

    async fn create_endpoint(&self, url: &str, event_types: Value) -> String {
        let (status, body) = self
            .admin(
                Method::POST,
                "/api/webhooks",
                Some(json!({
                    "url": url,
                    "event_types": event_types,
                    "secret_env": SECRET_ENV,
                })),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "create endpoint: {body}");
        body["webhook"]["id"].as_str().expect("id").to_string()
    }

    async fn deliver(&self, now: i64) -> Value {
        let report =
            rrouter_gateway::webhooks::deliver_due(&self.state.store, &self.state.http, now, 8)
                .await
                .expect("deliver");
        json!({
            "claimed": report.claimed,
            "delivered": report.delivered,
            "unmatched": report.unmatched,
            "retried": report.retried,
            "dead": report.dead,
        })
    }

    async fn outbox_status(&self) -> Value {
        let (status, body) = self.admin(Method::GET, "/api/webhooks/status", None).await;
        assert_eq!(status, StatusCode::OK, "status: {body}");
        body
    }

    async fn enqueue(&self, id: &str, event_type: &str, now: i64) {
        let payload = json!({
            "id": id,
            "type": event_type,
            "created_at": now,
            "data": {"probe": "8d"},
        })
        .to_string();
        assert!(
            self.state
                .store
                .enqueue_event(id, event_type, &payload, now)
                .await
                .expect("enqueue"),
            "fresh id"
        );
    }

    async fn row(&self, id: &str) -> Value {
        let rows = self.state.store.recent_events(50).await.expect("rows");
        let row = rows
            .into_iter()
            .find(|row| row.id == id)
            .unwrap_or_else(|| panic!("row {id}"));
        json!({
            "id": row.id,
            "status": row.status,
            "attempts": row.attempts,
            "next_attempt_at": row.next_attempt_at,
            "last_error": row.last_error,
        })
    }
}

// ----- API -----------------------------------------------------------------------

#[tokio::test]
async fn api_validates_and_never_returns_the_secret() {
    let h = harness();
    let secret_value = std::str::from_utf8(SECRET).expect("utf-8");

    let (status, body) = h
        .admin(
            Method::POST,
            "/api/webhooks",
            Some(json!({"url": "http://example.test/hook", "event_types": ["key.created"], "secret_env": SECRET_ENV})),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "plain http off loopback: {body}"
    );

    let (status, body) = h
        .admin(
            Method::POST,
            "/api/webhooks",
            Some(json!({"url": "https://example.test/hook", "event_types": ["made.up"], "secret_env": SECRET_ENV})),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "unknown type: {body}");
    assert!(
        body.to_string().contains("key.created"),
        "names the vocabulary"
    );

    let (status, body) = h
        .admin(
            Method::POST,
            "/api/webhooks",
            Some(json!({"url": "https://example.test/hook", "event_types": ["key.created"]})),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "secret_env required: {body}"
    );

    let id = h
        .create_endpoint("https://example.test/hook", json!(["key.created"]))
        .await;
    let (status, listed) = h.admin(Method::GET, "/api/webhooks", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listed["webhooks"][0]["id"], id.as_str());
    assert_eq!(listed["webhooks"][0]["secret_env"], SECRET_ENV);
    assert_eq!(listed["webhooks"][0]["has_secret"], true);
    assert_eq!(listed["webhooks"][0]["secret_configured"], true);
    assert_eq!(listed["webhooks"][0]["ready"], true);
    assert!(listed["webhooks"][0]["event_types"].is_array());
    let wire = listed.to_string();
    assert!(
        !wire.contains(secret_value),
        "the secret value never leaves the process"
    );
    assert!(!wire.contains("\"secret\""), "no raw secret field");

    let (status, patched) = h
        .admin(
            Method::PATCH,
            &format!("/api/webhooks/{id}"),
            Some(json!({"enabled": false, "event_types": ["key.updated", "key.created"]})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{patched}");
    assert_eq!(patched["webhook"]["enabled"], false);
    assert_eq!(
        patched["webhook"]["event_types"],
        json!(["key.updated", "key.created"])
    );

    let (status, body) = h
        .admin(
            Method::PATCH,
            &format!("/api/webhooks/{id}"),
            Some(json!({"secret_env": null})),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "cannot clear the secret: {body}"
    );

    let (status, _) = h
        .admin(Method::DELETE, &format!("/api/webhooks/{id}"), None)
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = h
        .admin(Method::DELETE, &format!("/api/webhooks/{id}"), None)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "second delete");
}

#[tokio::test]
async fn delivery_is_signed_and_subscription_filtered() {
    let h = harness();
    let (receiver, url) = start_receiver(0).await;
    h.create_endpoint(&url, json!(["key.created"])).await;
    h.enqueue("evt-signed", "key.created", 1_000).await;
    h.enqueue("evt-other", "team.created", 1_000).await;

    let report = h.deliver(1_000).await;
    assert_eq!(report["claimed"], 2);
    assert_eq!(report["delivered"], 1);
    assert_eq!(report["unmatched"], 1, "team.created had no subscriber");
    let received = receiver.received();
    assert_eq!(receiver.hits(), 1, "only the subscribed event is posted");
    assert_eq!(received.len(), 1);
    assert!(received[0].valid, "HMAC over the exact body verifies");
    assert_eq!(received[0].event, "key.created");
    assert_eq!(received[0].delivery, "evt-signed");
    let body: Value = serde_json::from_str(&received[0].body).expect("json body");
    assert_eq!(body["id"], "evt-signed");
    assert_eq!(body["type"], "key.created");
    assert_eq!(body["data"]["probe"], "8d");
}

#[tokio::test]
async fn a_failed_attempt_is_retried_then_delivered() {
    let h = harness();
    let (receiver, url) = start_receiver(1).await;
    h.create_endpoint(&url, json!(["key.created"])).await;
    h.enqueue("evt-retry", "key.created", 1_000).await;

    let first = h.deliver(1_000).await;
    assert_eq!(first["retried"], 1);
    assert_eq!(receiver.hits(), 0);
    let row = h.row("evt-retry").await;
    assert_eq!(row["status"], "pending");
    assert_eq!(row["attempts"], 1);
    let retry_at = row["next_attempt_at"].as_i64().expect("next attempt");
    assert!(retry_at > 1_000, "backoff pushed the attempt forward");
    assert!(row["last_error"]
        .as_str()
        .expect("error")
        .contains("answered 500"));

    // Not due yet: nothing is claimed.
    assert_eq!(h.deliver(1_000).await["claimed"], 0);

    let second = h.deliver(retry_at).await;
    assert_eq!(second["delivered"], 1);
    assert_eq!(receiver.hits(), 1);
    let received = receiver.received();
    assert!(received[0].valid);
    assert_eq!(h.row("evt-retry").await["status"], "delivered");
}

#[tokio::test]
async fn an_always_failing_receiver_ends_dead_at_the_cap() {
    let h = harness();
    let (receiver, url) = start_receiver(usize::MAX).await;
    h.create_endpoint(&url, json!(["key.created"])).await;
    h.enqueue("evt-dead", "key.created", 1_000).await;

    let mut now = 1_000;
    let mut status = String::new();
    for attempt in 1..=8 {
        let report = h.deliver(now).await;
        assert_eq!(report["claimed"], 1, "attempt {attempt}");
        let row = h.row("evt-dead").await;
        status = row["status"].as_str().expect("status").to_string();
        assert_eq!(row["attempts"], attempt);
        if status == "dead" {
            break;
        }
        now = row["next_attempt_at"].as_i64().expect("next");
    }
    assert_eq!(status, "dead", "8 attempts exhaust the default policy");
    assert_eq!(receiver.hits(), 0, "no attempt was accepted");
    assert_eq!(h.row("evt-dead").await["status"], "dead");
    assert_eq!(h.outbox_status().await["outbox"]["dead"], 1);
    // A dead row is never retried.
    assert_eq!(h.deliver(now + 100_000).await["claimed"], 0);
}

#[tokio::test]
async fn a_disabled_endpoint_is_skipped() {
    let h = harness();
    let (receiver, url) = start_receiver(0).await;
    let id = h.create_endpoint(&url, json!(["key.created"])).await;
    let (status, _) = h
        .admin(
            Method::PATCH,
            &format!("/api/webhooks/{id}"),
            Some(json!({"enabled": false})),
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    h.enqueue("evt-disabled", "key.created", 1_000).await;
    let report = h.deliver(1_000).await;
    assert_eq!(report["delivered"], 0, "nothing was sent");
    assert_eq!(report["unmatched"], 1);
    assert_eq!(receiver.hits(), 0);
    let row = h.row("evt-disabled").await;
    assert_eq!(row["status"], "unmatched", "terminal, not delivered");
    assert_eq!(row["attempts"], 1);

    // The counter separates the two outcomes, and the row is never retried.
    let status = h.outbox_status().await;
    assert_eq!(status["outbox"]["delivered"], 0);
    assert_eq!(status["outbox"]["unmatched"], 1);
    assert_eq!(h.deliver(100_000).await["claimed"], 0);
}

#[tokio::test]
async fn duplicate_delivery_is_idempotent() {
    let h = harness();
    let (receiver, url) = start_receiver(0).await;
    h.create_endpoint(&url, json!(["key.created"])).await;
    h.enqueue("evt-dup", "key.created", 1_000).await;
    let payload = "{\"id\":\"evt-dup\",\"type\":\"key.created\",\"created_at\":1000,\"data\":{}}";
    assert!(
        !h.state
            .store
            .enqueue_event("evt-dup", "key.created", payload, 1_000)
            .await
            .expect("duplicate enqueue"),
        "same id is ignored"
    );

    assert_eq!(h.deliver(1_000).await["delivered"], 1);
    assert_eq!(receiver.hits(), 1);

    // A late duplicate enqueue (caller retry) cannot resurrect or re-send the event.
    assert!(!h
        .state
        .store
        .enqueue_event("evt-dup", "key.created", payload, 2_000)
        .await
        .expect("late duplicate"));
    assert_eq!(h.deliver(2_000).await["claimed"], 0);
    assert_eq!(receiver.hits(), 1);
}

#[tokio::test]
async fn an_audit_mutation_succeeds_when_delivery_is_impossible() {
    let h = harness();
    // Port 1 on loopback refuses connections; the endpoint stays valid (loopback http) but
    // cannot be reached.
    let id = h
        .create_endpoint("http://127.0.0.1:1/hook", json!(["key.created"]))
        .await;

    let (status, created) = h
        .admin(
            Method::POST,
            "/api/keys",
            Some(json!({"name": "invoice-bot"})),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "mutation must not depend on delivery: {created}"
    );
    let key_id = created["id"].as_str().expect("key id").to_string();

    // The audit row produced an outbox event, visible to an operator.
    let (status, listed) = h.admin(Method::GET, "/api/webhooks/status", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listed["outbox"]["pending"], 1, "{listed}");

    let report = h.deliver(rrouter_gateway::now_unix()).await;
    assert_eq!(report["retried"], 1);
    let rows = h.state.store.recent_events(10).await.expect("rows");
    let event = rows.first().expect("outbox row");
    assert_eq!(event.event_type, "key.created");
    assert!(
        event
            .last_error
            .as_deref()
            .expect("error")
            .starts_with("transport:"),
        "recorded without leaking the url"
    );

    // The key itself is untouched by the failed delivery.
    let keys = h.state.store.list_api_keys().await.expect("keys");
    assert!(keys.iter().any(|key| key.id.to_string() == key_id));
    let (status, _) = h
        .admin(Method::DELETE, &format!("/api/webhooks/{id}"), None)
        .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn the_spawned_worker_delivers_and_stops_with_the_server() {
    let h = harness();
    let (receiver, url) = start_receiver(0).await;
    h.create_endpoint(&url, json!(["key.created"])).await;
    h.enqueue("evt-live", "key.created", rrouter_gateway::now_unix())
        .await;

    let stop = Arc::new(Notify::new());
    let worker = rrouter_gateway::webhooks::spawn(
        Arc::clone(&h.state.store),
        h.state.http.clone(),
        Arc::clone(&stop),
    );

    let deadline = Instant::now() + Duration::from_secs(5);
    while receiver.hits() == 0 {
        assert!(Instant::now() < deadline, "worker never polled");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(receiver.received()[0].valid);

    // Shutdown must not leak the task: after `stop`, `serve` can join it.
    stop.notify_one();
    tokio::time::timeout(Duration::from_secs(2), worker)
        .await
        .expect("worker exited after stop")
        .expect("worker did not panic");
    assert_eq!(h.row("evt-live").await["status"], "delivered");
}

#[tokio::test]
async fn endpoint_list_reports_whether_the_signing_secret_is_configured() {
    let h = harness();
    let missing = "RR_WEBHOOK_MISSING_SECRET_8D";
    std::env::remove_var(missing);
    let (status, created) = h
        .admin(
            Method::POST,
            "/api/webhooks",
            Some(json!({
                "url": "https://example.test/warn",
                "event_types": ["key.created"],
                "secret_env": missing,
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "create: {created}");

    let (_, listed) = h.admin(Method::GET, "/api/webhooks", None).await;
    let row = &listed["webhooks"][0];
    assert_eq!(row["secret_env"], missing);
    assert_eq!(row["secret_configured"], false, "env var is absent");
    assert_eq!(row["ready"], false);

    let value = "value-that-must-not-be-returned";
    std::env::set_var(missing, value);
    let (_, listed) = h.admin(Method::GET, "/api/webhooks", None).await;
    let row = &listed["webhooks"][0];
    assert_eq!(row["secret_configured"], true, "env var resolves now");
    assert_eq!(row["ready"], true);
    assert!(!listed.to_string().contains(value), "never the value");
    std::env::remove_var(missing);
}

#[tokio::test]
async fn the_event_vocabulary_is_an_array_and_keeps_the_csv_alias() {
    let h = harness();
    h.create_endpoint("https://example.test/hook", json!(["key.created"]))
        .await;
    let (status, listed) = h.admin(Method::GET, "/api/webhooks", None).await;
    assert_eq!(status, StatusCode::OK);

    let vocabulary = listed["event_types"]
        .as_array()
        .expect("vocabulary is an array");
    assert!(vocabulary.iter().any(|name| name == "key.created"));
    assert!(vocabulary.iter().any(|name| name == "audit.recorded"));
    assert!(vocabulary.iter().any(|name| name == "*"));
    let csv = listed["event_types_csv"]
        .as_str()
        .expect("legacy csv string kept for older dashboards");
    assert_eq!(
        csv,
        vocabulary
            .iter()
            .map(|name| name.as_str().expect("string"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    assert!(
        listed["webhooks"][0]["event_types"].is_array(),
        "row types are an array"
    );
}
