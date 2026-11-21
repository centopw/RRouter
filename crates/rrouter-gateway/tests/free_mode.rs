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
