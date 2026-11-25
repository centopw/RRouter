//! Free-mode import, discovery and teardown, against a mock OpenAI-compatible upstream.
//!
//! Same harness style as `tests/models.rs`: the real router over `oneshot`, plus a real
//! local upstream so discovery goes through the HTTP path rather than a stub.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use http_body_util::BodyExt;
use rrouter_gateway::{app, config::Config, AppState};
use rrouter_router::{Breaker, Catalog, Dispatcher, Planner};
use rrouter_store::{ModelRow, Store};
use serde_json::{json, Value};
use tower::ServiceExt;

// ---------------------------------------------------------------- mock upstream

#[derive(Clone)]
struct Mock {
    models: Value,
    hits: Arc<AtomicUsize>,
}

/// A model list that only the working account may read. The 401 body echoes the presented
/// credential on purpose: the discovery answer must scrub it.
async fn mock_models(State(mock): State<Mock>, headers: HeaderMap) -> Response {
    mock.hits.fetch_add(1, Ordering::SeqCst);
    let presented = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if presented != "Bearer good-key" {
        return (
            StatusCode::UNAUTHORIZED,
            axum::Json(json!({
                "error": { "message": format!("Incorrect API key provided: {presented}") }
            })),
        )
            .into_response();
    }
    axum::Json(mock.models.clone()).into_response()
}

/// A proxy URL that refuses connections immediately.
///
/// Discovery must leave through the account's own proxy, so an account with a proxy cannot
/// reach the mock upstream at all — which is the point of the proxy. A port bound and then
/// released refuses instantly (`ECONNREFUSED`), so the "one broken account does not fail
/// the run" case costs no wall clock, unlike a black-holed address behind a connect timeout.
fn refusing_proxy() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind proxy port");
    let port = listener.local_addr().expect("proxy addr").port();
    drop(listener);
    format!("http://user:pass@127.0.0.1:{port}")
}

async fn start_mock(models: Value) -> (String, Arc<AtomicUsize>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock");
    let addr = listener.local_addr().expect("addr");
    let hits = Arc::new(AtomicUsize::new(0));
    let router = Router::new()
        .route("/v1/models", get(mock_models))
        .with_state(Mock {
            models,
            hits: Arc::clone(&hits),
        });
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    (format!("http://{addr}"), hits)
}

// ----------------------------------------------------------------------- harness

struct Harness {
    state: Arc<AppState>,
    router: Router,
    /// The mock upstream's origin, used as a custom provider's base URL.
    upstream: String,
    hits: Arc<AtomicUsize>,
    /// The egress proxy this harness's document assigns to `acc-1`. One port per harness, so
    /// building the document twice (a re-import) sends the same URL.
    proxy: String,
}

fn config() -> Config {
    Config {
        host: "127.0.0.1".parse().expect("ip"),
        port: 0,
        data_dir: std::env::temp_dir().join(format!("rrouter-test-{}", uuid::Uuid::new_v4())),
        master_key: None,
        require_api_key: false,
        request_timeout_secs: 30,
        connect_timeout_secs: 5,
        log_filter: "rrouter=info".into(),
        static_dir: None,
        cors_origins: vec![],
        allow_remote_admin: false,
        admin_token: None,
        max_concurrency: 0,
        max_attempts: 4,
        retry_attempts: 1,
        retry_base_ms: 200,
        retry_max_ms: 5000,
        shutdown_grace_secs: 20,
        strategy: Default::default(),
        cache: Default::default(),
        cache_ttl_secs: 300,
        cache_max_entries: 1000,
        usage_retention_days: 0,
        log_format: rrouter_gateway::config::LogFormat::Text,
        access_log: true,
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

async fn harness() -> Harness {
    let models = json!({
        "object": "list",
        "data": [
            { "id": "mock-alpha:free" },
            { "id": "mock-beta" },
            { "id": "mock-gamma" },
            { "id": "mock-delta" },
        ],
    });
    let (upstream, hits) = start_mock(models).await;
    let state = Arc::new(AppState {
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
        config: rrouter_gateway::config::ConfigSnapshot::from(config()),
        started: Instant::now(),
        oauth: rrouter_auth::OAuthFlows::new(),
        counters: Default::default(),
        envelope_drops: Default::default(),
        providers: Default::default(),
        concurrency: std::sync::RwLock::new(None),
        proxy_clients: Default::default(),
        auth: rrouter_gateway::auth::Auth::load(&config().data_dir, false),
    });
    let router = app(Arc::clone(&state));
    Harness {
        state,
        router,
        upstream,
        hits,
        proxy: refusing_proxy(),
    }
}

impl Harness {
    /// A document with one custom free-tier provider and two accounts: the first through a
    /// named proxy with its own rpm, the second direct and falling back to the routing knob.
    fn document(&self) -> Value {
        json!({
            "version": 1,
            "routing": { "enabled": true, "per_account_rpm": 7, "max_attempts": 3 },
            "proxies": { "p1": self.proxy.clone() },
            "providers": [{
                "provider": "mock-free",
                "custom": {
                    "id": "mock-free",
                    "display": "Mock Free Host",
                    "base_url": self.upstream,
                    "dialect": "chat",
                },
                "models": ["mock-seed"],
                "accounts": [
                    {
                        "label": "acc-1",
                        "api_key": "good-key",
                        "proxy": "p1",
                        "rpm": 5,
                        "priority": 2,
                        "weight": 40,
                    },
                    { "label": "acc-2", "api_key": "good-key" },
                ],
            }],
        })
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
        self.call(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .expect("request"),
        )
        .await
    }

    async fn delete(&self, uri: &str) -> (StatusCode, Value) {
        self.call(
            Request::builder()
                .method("DELETE")
                .uri(uri)
                .body(Body::empty())
                .expect("request"),
        )
        .await
    }

    async fn connections(&self) -> Vec<rrouter_store::Connection> {
        self.state
            .store
            .list_connections(Some("mock-free"))
            .await
            .expect("connections")
    }

    async fn account(&self, label: &str) -> rrouter_store::Connection {
        self.connections()
            .await
            .into_iter()
            .find(|conn| conn.credentials.account_label.as_deref() == Some(label))
            .unwrap_or_else(|| panic!("account {label} missing"))
    }
}

// ------------------------------------------------------------------------- tests

/// An import creates the accounts the document describes: the proxy each one leaves
/// through, the rpm (own or inherited), the routing knobs and the provider itself.
#[tokio::test]
async fn import_creates_accounts_with_their_proxy_and_rpm() {
    let h = harness().await;
    let (status, body) = h.post("/api/free-mode/import", h.document()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["ok"], true);
    assert_eq!(body["providers"], 1);
    assert_eq!(body["accounts"]["created"], 2);
    assert_eq!(body["accounts"]["updated"], 0);

    let first = h.account("acc-1").await;
    assert_eq!(first.provider, "mock-free");
    assert_eq!(first.auth_type, "api_key");
    assert_eq!(first.name.as_deref(), Some("acc-1"));
    assert!(
        first.credentials.free_mode,
        "an imported account is free-mode"
    );
    assert_eq!(
        first.credentials.proxy_url.as_deref(),
        Some(h.proxy.as_str()),
        "the account keeps the proxy the document named"
    );
    assert_eq!(first.credentials.free_rpm, Some(5));
    assert_eq!(first.priority, 2);
    assert_eq!(first.weight, 40);
    assert!(first.is_active);
    assert_eq!(
        first.credentials.base_url_override.as_deref(),
        Some(h.upstream.as_str()),
        "a custom provider pins its accounts to its base URL"
    );

    // No `rpm` falls back to `routing.per_account_rpm`; no `proxy` means direct.
    let second = h.account("acc-2").await;
    assert_eq!(second.credentials.free_rpm, Some(7));
    assert_eq!(second.credentials.proxy_url, None);
    assert_eq!(second.priority, 0);
    assert_eq!(second.weight, 100);

    // The provider was created through the same path `POST /api/models/providers` uses.
    let profile = h
        .state
        .store
        .provider_profile("mock-free")
        .await
        .expect("profile")
        .expect("custom provider row");
    assert_eq!(profile.display.as_deref(), Some("Mock Free Host"));
    assert_eq!(profile.dialect.as_deref(), Some("openai"));
    assert_eq!(profile.base_url.as_deref(), Some(h.upstream.as_str()));
    assert_eq!(
        h.state.provider_dialect("mock-free"),
        Some(rrouter_core::Dialect::OpenAiChat)
    );

    // The document is stored, and its declared model is seeded as a free row.
    let config = h
        .state
        .store
        .free_mode_config()
        .await
        .expect("config")
        .expect("stored document");
    assert_eq!(config.routing["max_attempts"], 3);
    assert_eq!(config.routing["prefer"], "spread");
    let rows = h.state.store.list_free_models().await.expect("models");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].model, "mock-seed");
    assert!(rows[0].free);
    assert_eq!(rows[0].source, "document");
}

/// Enabling the pool on a fresh instance answers at once and probes in the background.
///
/// The probe is a live send per enabled model, so awaiting it inside the PATCH held the
/// toggle open for the whole pool (the release gate's smoke harness timed out on it). The
/// seeded model here points at a closed port, so its probe would stall until the connect
/// timeout; the answer must still be immediate, and discovery must not have run.
#[tokio::test]
async fn enabling_the_pool_answers_without_waiting_for_probes() {
    let h = harness().await;
    let (status, imported) = h.post("/api/free-mode/import", h.document()).await;
    assert_eq!(status, StatusCode::OK, "{imported}");

    let started = Instant::now();
    let (status, body) = h
        .call(
            Request::builder()
                .method("PATCH")
                .uri("/api/free-mode")
                .header("content-type", "application/json")
                .body(Body::from(json!({ "enabled": true }).to_string()))
                .expect("request"),
        )
        .await;
    let elapsed = started.elapsed();
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["enabled"], true);
    assert!(
        elapsed < Duration::from_secs(2),
        "the toggle must not wait for the probe round: {elapsed:?}"
    );
    let config = h
        .state
        .store
        .free_mode_config()
        .await
        .expect("config")
        .expect("stored routing");
    assert_eq!(config.routing["enabled"], true);
}

/// Re-importing the same document updates the accounts instead of duplicating them, and a
/// changed proxy or rpm lands on the existing row.
#[tokio::test]
async fn re_import_is_idempotent() {
    let h = harness().await;
    let (status, body) = h.post("/api/free-mode/import", h.document()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = h.post("/api/free-mode/import", h.document()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["accounts"]["created"], 0);
    assert_eq!(body["accounts"]["updated"], 2);
    assert_eq!(h.connections().await.len(), 2, "no duplicate connections");

    // A document that drops the proxy and the rpm clears both on the existing account.
    let mut document = h.document();
    document["providers"][0]["accounts"][0]
        .as_object_mut()
        .expect("account")
        .remove("proxy");
    document["providers"][0]["accounts"][0]
        .as_object_mut()
        .expect("account")
        .remove("rpm");
    document["routing"]
        .as_object_mut()
        .expect("routing")
        .remove("per_account_rpm");
    let (status, body) = h.post("/api/free-mode/import", document).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let first = h.account("acc-1").await;
    assert_eq!(first.credentials.proxy_url, None);
    assert_eq!(first.credentials.free_rpm, None);
    assert_eq!(h.connections().await.len(), 2);
}

/// A bad document is refused as a whole and every problem is named.
#[tokio::test]
async fn an_invalid_document_writes_nothing_and_names_every_problem() {
    let h = harness().await;
    let document = json!({
        "providers": [
            {
                "provider": "nope-not-a-provider",
                "accounts": [{ "label": "a", "api_key": "good-key" }],
            },
            {
                "provider": "mock-free",
                "custom": {
                    "id": "mock-free",
                    "display": "Mock Free Host",
                    "base_url": h.upstream,
                    "dialect": "chat",
                },
                "accounts": [
                    { "label": "b", "api_key": "good-key", "proxy": "p9" },
                    { "label": "b", "api_key": "good-key" },
                    { "label": "c" },
                ],
            },
        ],
    });
    let (status, body) = h.post("/api/free-mode/import", document).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let details: Vec<String> = body["error"]["details"]
        .as_array()
        .expect("details")
        .iter()
        .map(|detail| detail.as_str().unwrap_or_default().to_string())
        .collect();
    assert!(
        details
            .iter()
            .any(|detail| detail.contains("unknown provider nope-not-a-provider")),
        "{details:?}"
    );
    assert!(
        details
            .iter()
            .any(|detail| detail == "providers[1].accounts[0].proxy: unknown proxy key p9"),
        "{details:?}"
    );
    assert!(
        details
            .iter()
            .any(|detail| detail.contains("providers[1].accounts[1].label")
                && detail.contains("appears twice")),
        "{details:?}"
    );
    assert!(
        details
            .iter()
            .any(|detail| detail
                .contains("providers[1].accounts[2]: one of api_key or access_token")),
        "{details:?}"
    );

    // Nothing was written, not even the valid half.
    assert!(h.connections().await.is_empty());
    assert!(h
        .state
        .store
        .free_mode_config()
        .await
        .expect("config")
        .is_none());
    assert!(h
        .state
        .store
        .list_free_models()
        .await
        .expect("models")
        .is_empty());
    assert!(h
        .state
        .store
        .provider_profile("mock-free")
        .await
        .expect("profile")
        .is_none());
}

/// `routing.models` names the models the bare `free` ref may use, and it round-trips.
#[tokio::test]
async fn routing_models_is_stored_and_echoed() {
    let h = harness().await;
    let mut document = h.document();
    document["routing"]["models"] = json!(["mock-alpha:free", "mock-beta"]);
    // The `{"document": {...}}` wrapper the dashboard posts is accepted too.
    let (status, body) = h
        .post("/api/free-mode/import", json!({ "document": document }))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["routing"]["models"],
        json!(["mock-alpha:free", "mock-beta"]),
        "the import echoes the selection it stored"
    );

    let (status, body) = h.get("/api/free-mode").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["routing"]["models"],
        json!(["mock-alpha:free", "mock-beta"])
    );
    assert_eq!(body["routing"]["max_attempts"], 3);
    assert_eq!(body["routing"]["per_account_rpm"], 7);

    // The provider seed list is routable before any discovery ran.
    let seeded = body["models"]
        .as_array()
        .expect("models")
        .iter()
        .find(|row| row["model"] == "mock-seed")
        .expect("seeded model row");
    assert_eq!(seeded["free"], true);
    assert_eq!(seeded["source"], "document");
}

/// A blank, duplicated, whitespace-carrying or over-long selection entry names its index.
#[tokio::test]
async fn a_bad_routing_models_entry_is_refused_by_index() {
    let h = harness().await;

    let mut document = h.document();
    document["routing"]["models"] = json!(["mock-seed", "mock-seed"]);
    let (status, body) = h.post("/api/free-mode/import", document).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["error"]["details"]
            .as_array()
            .expect("details")
            .iter()
            .any(|detail| detail
                .as_str()
                .unwrap_or_default()
                .contains("routing.models[1]")),
        "{body}"
    );

    let mut document = h.document();
    document["routing"]["models"] = json!(["mock-seed", "   "]);
    let (status, body) = h.post("/api/free-mode/import", document).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["error"]["details"]
            .as_array()
            .expect("details")
            .iter()
            .any(|detail| detail
                .as_str()
                .unwrap_or_default()
                .contains("routing.models[1]")),
        "{body}"
    );

    let mut document = h.document();
    document["routing"]["models"] = json!(["has space"]);
    let (status, body) = h.post("/api/free-mode/import", document).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["error"]["details"]
            .as_array()
            .expect("details")
            .iter()
            .any(|detail| detail
                .as_str()
                .unwrap_or_default()
                .contains("routing.models[0]")),
        "{body}"
    );

    assert!(h.connections().await.is_empty(), "nothing was applied");
}

/// Discovery asks each account for its own list, classifies every id and stores the row
/// with the signal that decided it. One broken account does not fail the run.
#[tokio::test]
async fn discovery_stores_rows_with_their_source_and_survives_one_bad_account() {
    let h = harness().await;
    // Prices for the mock ids: `beta` is free by price, `gamma` is not, `delta` has none.
    for (model, cost) in [
        ("mock-beta", json!({ "input": 0.0, "output": 0.0 })),
        ("mock-gamma", json!({ "input": 6.0, "output": 6.0 })),
    ] {
        h.state
            .store
            .upsert_model(&ModelRow {
                provider: "mock-free".into(),
                model: model.into(),
                display: None,
                context: None,
                max_output: None,
                caps: json!({}),
                cost: Some(cost),
                source: "custom".into(),
                updated_at: 0,
            })
            .await
            .expect("model row");
    }

    let mut document = h.document();
    // A second account whose key the upstream rejects: its own row reports the failure.
    document["providers"][0]["accounts"]
        .as_array_mut()
        .expect("accounts")
        .push(json!({ "label": "acc-bad", "api_key": "bad-key" }));
    let (status, body) = h.post("/api/free-mode/import", document).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, body) = h.post("/api/free-mode/discover", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["ok"], true);
    assert_eq!(body["providers"][0]["provider"], "mock-free");
    assert_eq!(body["providers"][0]["accounts"], 3);
    assert_eq!(body["providers"][0]["models"], 4);
    assert_eq!(body["providers"][0]["free"], 2, "alpha:free and beta");

    let accounts = body["accounts"].as_array().expect("accounts");
    let failed = accounts
        .iter()
        .find(|row| row["label"] == "acc-bad")
        .expect("failed account row");
    assert!(failed["error"].is_string(), "{failed}");
    let error = failed["error"].as_str().expect("error");
    assert!(error.contains("401"), "{error}");
    assert!(
        !error.contains("bad-key"),
        "the echoed key was scrubbed: {error}"
    );
    // `acc-1` left through its own (refusing) proxy, so only the other two accounts reached
    // the upstream: an account with a proxy never connects directly, even for discovery.
    assert_eq!(
        h.hits.load(Ordering::SeqCst),
        2,
        "one request per account without a proxy; the proxied account never reaches the upstream"
    );
    let proxied = accounts
        .iter()
        .find(|row| row["label"] == "acc-1")
        .expect("proxied account row");
    assert!(
        proxied["error"].is_string(),
        "the proxied account reports its own failure: {proxied}"
    );

    let rows = h.state.store.list_free_models().await.expect("rows");
    let row = |model: &str| {
        rows.iter()
            .find(|row| row.model == model)
            .unwrap_or_else(|| panic!("{model} missing from free_models: {rows:?}"))
    };
    assert!(row("mock-alpha:free").free);
    assert_eq!(row("mock-alpha:free").source, "provider-list");
    assert_eq!(
        row("mock-alpha:free")
            .connection_id
            .map(|id| id.to_string()),
        Some(h.account("acc-2").await.id.to_string()),
        "the row names the account that actually proved it"
    );
    assert!(row("mock-beta").free);
    assert_eq!(row("mock-beta").source, "registry");
    assert_eq!(row("mock-beta").price, Some(0.0));
    assert!(!row("mock-gamma").free);
    assert_eq!(row("mock-gamma").price, Some(12.0));
    assert!(!row("mock-delta").free, "an unknown price is not free");
    // The document's own seed survived the run: a model the provider does not list is
    // still routable.
    assert!(row("mock-seed").free);
    assert_eq!(row("mock-seed").source, "document");
}

/// A whole-key free tier (Groq) prices nothing at zero and carries no `:free` marker, so
/// `classify_free` alone refuses every listed model. The vendored `all-models-on-free-key`
/// rule marks only the models the vendored catalog names as free — a billed key flagged
/// `free_mode` must never leak a paid model (gemini-2.5-pro, imagen/veo/tts) into the pool.
#[tokio::test]
async fn a_whole_key_free_tier_widens_only_vendored_models() {
    let h = harness().await;
    // Point a Groq free-mode account at the mock upstream. Groq's compiled base_url has no
    // models_url, so `base_url_override` drives `models_url(base)` → `/v1/models`, the path
    // the mock already serves.
    h.state
        .store
        .create_connection(
            "groq",
            "api_key",
            rrouter_auth::Credentials {
                api_key: Some("good-key".to_string()),
                base_url_override: Some(h.upstream.clone()),
                free_mode: true,
                ..Default::default()
            },
            Some("groq-free"),
        )
        .await
        .expect("groq connection");

    // The mock's own list already carries `mock-alpha:free` (a real marker) but none of its
    // ids are vendored Groq models, so only the marker model is free.
    let (status, body) = h
        .post("/api/free-mode/discover", json!({ "providers": ["groq"] }))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // `mock-alpha:free` (marker) only; `mock-gamma`/`mock-delta`/`mock-beta` are NOT in the
    // vendored Groq rows, so a billed key's paid models stay out of the pool.
    assert_eq!(body["providers"][0]["free"], 1, "{body}");

    let rows = h.state.store.list_free_models().await.expect("rows");
    let row = |model: &str| {
        rows.iter()
            .find(|row| row.model == model)
            .unwrap_or_else(|| panic!("{model} missing: {rows:?}"))
    };
    assert!(row("mock-alpha:free").free);
    assert!(
        !row("mock-gamma").free,
        "an unvendored model is not widened"
    );
    assert!(
        !row("mock-delta").free,
        "an unvendored model is not widened"
    );
}

/// A paid (non-free-mode) account on a whole-key provider is never consulted by free-mode
/// discovery, so the rule cannot widen it into the free pool.
#[tokio::test]
async fn a_paid_account_on_a_whole_key_provider_is_never_widened() {
    let h = harness().await;
    h.state
        .store
        .create_connection(
            "groq",
            "api_key",
            rrouter_auth::Credentials {
                api_key: Some("good-key".to_string()),
                base_url_override: Some(h.upstream.clone()),
                free_mode: false,
                ..Default::default()
            },
            Some("groq-paid"),
        )
        .await
        .expect("groq paid connection");

    let (status, body) = h
        .post("/api/free-mode/discover", json!({ "providers": ["groq"] }))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // No free-mode account existed, so no account rows and no provider summary.
    assert!(
        body["providers"].as_array().is_none_or(|p| p.is_empty()),
        "{body}"
    );
    let rows = h.state.store.list_free_models().await.expect("rows");
    assert!(
        rows.is_empty(),
        "a paid account must never seed free rows: {rows:?}"
    );
}

/// Nothing a read path returns may contain credential material.
#[tokio::test]
async fn no_api_key_or_proxy_userinfo_is_ever_echoed() {
    let h = harness().await;
    let (status, imported) = h.post("/api/free-mode/import", h.document()).await;
    assert_eq!(status, StatusCode::OK, "{imported}");
    let (status, shown) = h.get("/api/free-mode").await;
    assert_eq!(status, StatusCode::OK, "{shown}");
    let (status, discovered) = h.post("/api/free-mode/discover", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{discovered}");

    for body in [&imported, &shown, &discovered] {
        let encoded = body.to_string();
        assert!(!encoded.contains("good-key"), "{encoded}");
        assert!(!encoded.contains("user:pass"), "{encoded}");
    }
    // The proxy is masked, not dropped: an operator still has to tell two apart. The host
    // is read back from the document this test sent, so the assertion does not depend on a
    // fixed port.
    let masked = h.proxy.replace("user:pass@", "***@");
    let proxied = shown["accounts"]
        .as_array()
        .expect("accounts")
        .iter()
        .find(|row| row["label"] == "acc-1")
        .expect("acc-1 row");
    assert_eq!(proxied["proxy"], masked, "{shown}");
    assert_eq!(shown["document"]["proxies"]["p1"], masked);
    assert_eq!(
        shown["document"]["providers"][0]["accounts"][0]["has_api_key"],
        true
    );
    assert!(
        shown["document"]["providers"][0]["accounts"][0]
            .get("api_key")
            .is_none(),
        "{shown}"
    );
}

/// Teardown forgets the document and the discovery rows, disables the accounts when asked,
/// and never touches the credentials.
#[tokio::test]
async fn teardown_clears_both_tables_and_keeps_credentials() {
    let h = harness().await;
    let (status, body) = h.post("/api/free-mode/import", h.document()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = h.post("/api/free-mode/discover", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!h
        .state
        .store
        .list_free_models()
        .await
        .expect("rows")
        .is_empty());

    let (status, body) = h.delete("/api/free-mode?disable=true").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["ok"], true);
    assert_eq!(body["disabled"], 2);

    assert!(h
        .state
        .store
        .free_mode_config()
        .await
        .expect("config")
        .is_none());
    assert!(h
        .state
        .store
        .list_free_models()
        .await
        .expect("rows")
        .is_empty());
    let connections = h.connections().await;
    assert_eq!(connections.len(), 2, "teardown never deletes a connection");
    for conn in &connections {
        assert!(!conn.is_active, "{} is still active", conn.id);
        assert_eq!(
            conn.credentials.api_key.as_deref(),
            Some("good-key"),
            "the credential survives so a re-import can adopt the account"
        );
    }

    // Meanwhile the read path answers an empty, well-formed document.
    let (status, body) = h.get("/api/free-mode").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["document"].is_null());
    assert!(body["models"].as_array().expect("models").is_empty());
    assert_eq!(body["accounts"][0]["is_active"], false);
}

/// Gemini's OpenAI-compatible `/models` endpoint prefixes ids with `models/`. Discovery
/// strips that prefix when consulting the vendored free-tier list so `models/gemini-2.5-flash`
/// is marked free while paid/non-chat models (`models/gemini-2.5-pro`, embeddings) stay out.
#[tokio::test]
async fn gemini_models_prefix_discovery_widens_only_vendored_free_rows() {
    let (gemini_upstream, _hits) = start_mock(json!({
        "data": [
            { "id": "models/gemini-2.5-flash" },
            { "id": "models/gemini-2.5-pro" },
            { "id": "models/text-embedding-004" }
        ]
    }))
    .await;
    let h = harness().await;
    h.state
        .store
        .create_connection(
            "gemini",
            "api_key",
            rrouter_auth::Credentials {
                api_key: Some("good-key".to_string()),
                base_url_override: Some(gemini_upstream),
                free_mode: true,
                ..Default::default()
            },
            Some("gemini-free"),
        )
        .await
        .expect("gemini connection");

    let (status, body) = h
        .post(
            "/api/free-mode/discover",
            json!({ "providers": ["gemini"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["providers"][0]["free"], 1, "{body}");

    let rows = h.state.store.list_free_models().await.expect("rows");
    let row = |model: &str| {
        rows.iter()
            .find(|r| r.model == model)
            .unwrap_or_else(|| panic!("{model} missing: {rows:?}"))
    };
    assert!(row("models/gemini-2.5-flash").free);
    assert_eq!(row("models/gemini-2.5-flash").source, "provider-list");
    assert!(!row("models/gemini-2.5-pro").free);
    assert!(!row("models/text-embedding-004").free);
}

/// A daily exhaustion 429 parks that `(connection, model)` until UTC midnight, learns any
/// stated limit from the error body, skips subsequent attempts without hitting upstream,
/// and lets `free:<provider>` fall back cleanly to the next healthy free model.
#[tokio::test]
async fn per_model_daily_park_skips_exhausted_model_and_learns_limit() {
    use axum::routing::post;

    let llama_hits = Arc::new(AtomicUsize::new(0));
    let scout_hits = Arc::new(AtomicUsize::new(0));
    let l_hits = Arc::clone(&llama_hits);
    let s_hits = Arc::clone(&scout_hits);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    let mock = Router::new().fallback(post(move |axum::Json(req): axum::Json<Value>| {
        let l_hits = Arc::clone(&l_hits);
        let s_hits = Arc::clone(&s_hits);
        async move {
            let model = req["model"].as_str().unwrap_or_default().to_string();
            if model == "llama-3.3-70b-versatile" {
                l_hits.fetch_add(1, Ordering::SeqCst);
                return (
                    StatusCode::TOO_MANY_REQUESTS,
                    axum::Json(json!({
                        "error": {
                            "message": "Rate limit reached for model llama-3.3-70b-versatile on tokens per day (TPD): Limit 100000, Used 100000, Requested 500."
                        }
                    })),
                )
                    .into_response();
            }
            s_hits.fetch_add(1, Ordering::SeqCst);
            (
                StatusCode::OK,
                axum::Json(json!({
                    "id": "chatcmpl-scout",
                    "object": "chat.completion",
                    "created": 1,
                    "model": model,
                    "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"}, "finish_reason": "stop"}],
                    "usage": {"prompt_tokens": 5, "completion_tokens": 2}
                })),
            )
                .into_response()
        }
    }));
    tokio::spawn(async move {
        let _ = axum::serve(listener, mock).await;
    });

    let h = harness().await;
    let conn = h
        .state
        .store
        .create_connection(
            "groq",
            "api_key",
            rrouter_auth::Credentials {
                api_key: Some("good-key".to_string()),
                base_url_override: Some(format!("http://{addr}/v1")),
                free_mode: true,
                ..Default::default()
            },
            Some("groq-free"),
        )
        .await
        .expect("groq conn");
    let now = rrouter_gateway::now_unix();
    for model in [
        "llama-3.3-70b-versatile",
        "meta-llama/llama-4-scout-17b-16e-instruct",
    ] {
        h.state
            .store
            .upsert_free_model(&rrouter_store::FreeModel {
                provider: "groq".to_string(),
                model: model.to_string(),
                free: true,
                price: Some(0.0),
                source: "provider-list".to_string(),
                connection_id: Some(conn.id),
                discovered_at: now,
            })
            .await
            .expect("seed model");
    }
    h.state
        .store
        .save_free_mode_config(&json!({}), &json!({ "enabled": true }))
        .await
        .expect("enable");

    // 1. Pinned call to llama-3.3-70b-versatile hits daily 429, parks it, and learns TPD.
    let (status, _) = h
        .post(
            "/v1/chat/completions",
            json!({ "model": "free/llama-3.3-70b-versatile", "messages": [{"role": "user", "content": "hi"}] }),
        )
        .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(llama_hits.load(Ordering::SeqCst), 1);
    assert!(
        h.state
            .planner
            .free_rotation()
            .model_daily_park_wait(conn.id, "llama-3.3-70b-versatile")
            .is_some(),
        "daily exhaustion must park the model until UTC midnight"
    );
    assert_eq!(
        h.state
            .planner
            .free_rotation()
            .learned_model_limits("groq", "llama-3.3-70b-versatile")
            .tpd,
        Some(100_000)
    );

    // 2. Even if the transient breaker is cleared, the daily park skips llama locally (0 new hits).
    h.state
        .dispatcher
        .breaker()
        .record_success_for(conn.id, "llama-3.3-70b-versatile");
    let (status, _) = h
        .post(
            "/v1/chat/completions",
            json!({ "model": "free/llama-3.3-70b-versatile", "messages": [{"role": "user", "content": "hi"}] }),
        )
        .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        llama_hits.load(Ordering::SeqCst),
        1,
        "daily-parked model must not spend another upstream attempt"
    );

    // 3. Requesting the Groq free pool (`free:groq`) falls back to llama-4-scout and succeeds.
    let (status, body) = h
        .post(
            "/v1/chat/completions",
            json!({ "model": "free:groq", "messages": [{"role": "user", "content": "hi"}] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(scout_hits.load(Ordering::SeqCst), 1);
    assert_eq!(llama_hits.load(Ordering::SeqCst), 1);
}

/// On boot (fresh `FreeRotation`), today's `usage` rows rebuild per-model RPD counts so an
/// already-exhausted Gemini free-mode key (20 RPD) is parked before spending an upstream
/// attempt — while a *paid* Gemini key (`free_mode = false`) is never gated by free limits.
#[tokio::test]
async fn boot_usage_seed_enforces_gemini_rpd_and_never_gates_paid_account() {
    use axum::routing::post;

    let hits = Arc::new(AtomicUsize::new(0));
    let h_hits = Arc::clone(&hits);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    let mock = Router::new().fallback(post(move |axum::Json(req): axum::Json<Value>| {
        let h_hits = Arc::clone(&h_hits);
        async move {
            h_hits.fetch_add(1, Ordering::SeqCst);
            let model = req["model"].as_str().unwrap_or("gemini-2.5-flash");
            (
                StatusCode::OK,
                axum::Json(json!({
                    "id": "chatcmpl-gem",
                    "object": "chat.completion",
                    "created": 1,
                    "model": model,
                    "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"}, "finish_reason": "stop"}],
                    "usage": {"prompt_tokens": 4, "completion_tokens": 2}
                })),
            )
                .into_response()
        }
    }));
    tokio::spawn(async move {
        let _ = axum::serve(listener, mock).await;
    });

    // Harness 1: free-mode Gemini account with 20 usage rows already logged today.
    let h_free = harness().await;
    let free_conn = h_free
        .state
        .store
        .create_connection(
            "gemini",
            "api_key",
            rrouter_auth::Credentials {
                api_key: Some("good-key".to_string()),
                base_url_override: Some(format!("http://{addr}/v1")),
                free_mode: true,
                ..Default::default()
            },
            Some("gemini-free"),
        )
        .await
        .expect("free conn");
    let now = rrouter_gateway::now_unix();
    h_free
        .state
        .store
        .upsert_free_model(&rrouter_store::FreeModel {
            provider: "gemini".to_string(),
            model: "gemini-2.5-flash".to_string(),
            free: true,
            price: Some(0.0),
            source: "provider-list".to_string(),
            connection_id: Some(free_conn.id),
            discovered_at: now,
        })
        .await
        .expect("seed");
    h_free
        .state
        .store
        .save_free_mode_config(&json!({}), &json!({ "enabled": true }))
        .await
        .expect("enable");
    // Record in the real `usage.model` shape (`"{provider}/{model}"`) that `record_once`
    // writes in production — mixing `gemini/gemini-2.5-flash` and `gemini/models/gemini-2.5-flash`
    // to prove both strip cleanly to the live bare key `gemini-2.5-flash`.
    for i in 0..20 {
        let recorded_model = if i == 0 {
            "gemini/models/gemini-2.5-flash"
        } else {
            "gemini/gemini-2.5-flash"
        };
        h_free
            .state
            .store
            .record_usage(&rrouter_store::UsageRecord {
                id: None,
                ts: now,
                request_id: Some(uuid::Uuid::new_v4()),
                trace_id: None,
                provider: "gemini".to_string(),
                model: recorded_model.to_string(),
                connection_id: Some(free_conn.id),
                endpoint: Some("/v1/chat/completions".to_string()),
                key_id: None,
                cost_micros: 0,
                input_tokens: 10,
                output_tokens: 5,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                latency_ms: 10,
                ttft_ms: 0,
                upstream_ms: 10,
                status: 200,
                streamed: false,
                error: None,
                requested_model: None,
                finish_reason: Some("stop".to_string()),
                reasoning_effort: None,
                reasoning_tokens: 0,
                session_id: None,
                fallback_reason: None,
                unrouted_reason: None,
            })
            .await
            .expect("record usage");
    }

    assert!(h_free.state.planner.free_rotation().needs_usage_seed());
    let (status, _) = h_free
        .post(
            "/v1/chat/completions",
            json!({ "model": "free/gemini-2.5-flash", "messages": [{"role": "user", "content": "hi"}] }),
        )
        .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        hits.load(Ordering::SeqCst),
        0,
        "seeded 20/20 RPD must skip before any upstream call"
    );
    assert!(h_free
        .state
        .planner
        .free_rotation()
        .model_daily_park_wait(free_conn.id, "gemini-2.5-flash")
        .is_some());

    // Harness 2: paid Gemini account (free_mode=false) with 20 usage rows today is NOT gated.
    let h_paid = harness().await;
    let paid_conn = h_paid
        .state
        .store
        .create_connection(
            "gemini",
            "api_key",
            rrouter_auth::Credentials {
                api_key: Some("good-key".to_string()),
                base_url_override: Some(format!("http://{addr}/v1")),
                free_mode: false,
                ..Default::default()
            },
            Some("gemini-paid"),
        )
        .await
        .expect("paid conn");
    for _ in 0..20 {
        h_paid
            .state
            .store
            .record_usage(&rrouter_store::UsageRecord {
                id: None,
                ts: now,
                request_id: Some(uuid::Uuid::new_v4()),
                trace_id: None,
                provider: "gemini".to_string(),
                model: "gemini/gemini-2.5-flash".to_string(),
                connection_id: Some(paid_conn.id),
                endpoint: Some("/v1/chat/completions".to_string()),
                key_id: None,
                cost_micros: 0,
                input_tokens: 10,
                output_tokens: 5,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                latency_ms: 10,
                ttft_ms: 0,
                upstream_ms: 10,
                status: 200,
                streamed: false,
                error: None,
                requested_model: None,
                finish_reason: Some("stop".to_string()),
                reasoning_effort: None,
                reasoning_tokens: 0,
                session_id: None,
                fallback_reason: None,
                unrouted_reason: None,
            })
            .await
            .expect("record paid usage");
    }

    let (status, body) = h_paid
        .post(
            "/v1/chat/completions",
            json!({ "model": "gemini/gemini-2.5-flash", "messages": [{"role": "user", "content": "hi"}] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "paid account must reach upstream unaffected by free-tier RPD"
    );
}

/// A 413/429 error body naming a tighter TPM limit (Groq's real prose shape) tightens the
/// effective per-model TPM cap in `FreeRotation`, and completion token recording feeds the
/// 60 s TPM window so the next request is gated locally without hitting upstream.
#[tokio::test]
async fn learned_limit_from_413_tightens_tpm_and_gates_next_send() {
    use axum::routing::post;

    let hits = Arc::new(AtomicUsize::new(0));
    let h_hits = Arc::clone(&hits);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    let mock = Router::new().fallback(post(move || {
        let h_hits = Arc::clone(&h_hits);
        async move {
            let n = h_hits.fetch_add(1, Ordering::SeqCst) + 1;
            if n == 1 {
                return (
                    StatusCode::OK,
                    axum::Json(json!({
                        "id": "chatcmpl-1",
                        "object": "chat.completion",
                        "created": 1,
                        "model": "llama-3.3-70b-versatile",
                        "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"}, "finish_reason": "stop"}],
                        "usage": {"prompt_tokens": 4000, "completion_tokens": 3000}
                    })),
                )
                    .into_response();
            }
            if n == 2 {
                return (
                    StatusCode::PAYLOAD_TOO_LARGE,
                    axum::Json(json!({
                        "error": {
                            "message": "Rate limit reached for model `llama-3.3-70b-versatile` in organization `org_test` on tokens per minute (TPM): Limit 6000, Used 7000, Requested 500. Please try again in 640ms."
                        }
                    })),
                )
                    .into_response();
            }
            (
                StatusCode::OK,
                axum::Json(json!({
                    "id": "chatcmpl-3",
                    "object": "chat.completion",
                    "created": 1,
                    "model": "llama-3.3-70b-versatile",
                    "choices": [{"index": 0, "message": {"role": "assistant", "content": "should not reach"}, "finish_reason": "stop"}],
                    "usage": {"prompt_tokens": 1, "completion_tokens": 1}
                })),
            )
                .into_response()
        }
    }));
    tokio::spawn(async move {
        let _ = axum::serve(listener, mock).await;
    });

    let h = harness().await;
    let conn = h
        .state
        .store
        .create_connection(
            "groq",
            "api_key",
            rrouter_auth::Credentials {
                api_key: Some("good-key".to_string()),
                base_url_override: Some(format!("http://{addr}/v1")),
                free_mode: true,
                ..Default::default()
            },
            Some("groq-free"),
        )
        .await
        .expect("groq conn");
    h.state
        .store
        .upsert_free_model(&rrouter_store::FreeModel {
            provider: "groq".to_string(),
            model: "llama-3.3-70b-versatile".to_string(),
            free: true,
            price: Some(0.0),
            source: "provider-list".to_string(),
            connection_id: Some(conn.id),
            discovered_at: rrouter_gateway::now_unix(),
        })
        .await
        .expect("seed");
    h.state
        .store
        .save_free_mode_config(&json!({}), &json!({ "enabled": true }))
        .await
        .expect("enable");

    // 1. First request succeeds and records 7,000 tokens (under vendored 12,000 TPM).
    let (status, body) = h
        .post(
            "/v1/chat/completions",
            json!({ "model": "free/llama-3.3-70b-versatile", "messages": [{"role": "user", "content": "hi"}] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(hits.load(Ordering::SeqCst), 1);

    // 2. Second request returns Groq 413 naming Limit 6000 TPM -> tightens effective TPM to 6,000.
    let (status, _) = h
        .post(
            "/v1/chat/completions",
            json!({ "model": "free/llama-3.3-70b-versatile", "messages": [{"role": "user", "content": "hi"}] }),
        )
        .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(hits.load(Ordering::SeqCst), 2);
    assert_eq!(
        h.state
            .planner
            .free_rotation()
            .learned_model_limits("groq", "llama-3.3-70b-versatile")
            .tpm,
        Some(6_000),
        "413 error body must tighten the learned TPM limit"
    );

    // 3. Clear any breaker state and retry: proactive TPM gate (7,000 >= 6,000) skips locally.
    h.state
        .dispatcher
        .breaker()
        .record_success_for(conn.id, "llama-3.3-70b-versatile");
    let (status, _) = h
        .post(
            "/v1/chat/completions",
            json!({ "model": "free/llama-3.3-70b-versatile", "messages": [{"role": "user", "content": "hi"}] }),
        )
        .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        hits.load(Ordering::SeqCst),
        2,
        "tightened TPM cap must gate the next send without hitting upstream"
    );
}
