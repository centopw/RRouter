//! Model registry tests: merge precedence, the admin routes, and the models.dev parser.
//! Same harness style as `tests/proxy.rs`: the real router over `oneshot`, plus a real
//! local mock upstream where a provider call is involved.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::State;
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use http_body_util::BodyExt;
use rrouter_auth::Credentials;
use rrouter_gateway::models_registry::{parse_models_dev, MAX_FETCHED_IDS};
use rrouter_gateway::{app, config::Config, AppState};
use rrouter_router::{Breaker, Catalog, Dispatcher, Planner};
use rrouter_store::{LayoutDocument, LayoutGroup, LayoutItem, ModelRow, Store};
use serde_json::{json, Value};
use tower::ServiceExt;

// ---------------------------------------------------------------- mock upstream

#[derive(Clone)]
struct Mock;

async fn mock_models(State(_mock): State<Mock>) -> Response {
    axum::Json(json!({
        "object": "list",
        "data": [{"id": "alpha"}, {"id": "beta"}, {"id": ""}, {"id": "alpha"}],
    }))
    .into_response()
}

async fn mock_chat(State(_mock): State<Mock>) -> Response {
    axum::Json(json!({
        "id": "chatcmpl-1",
        "object": "chat.completion",
        "created": 1,
        "model": "gpt-4.1",
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": "pong"},
            "finish_reason": "stop",
        }],
        "usage": {"prompt_tokens": 3, "completion_tokens": 1},
    }))
    .into_response()
}

#[derive(Clone)]
struct SlowMock {
    delay: Duration,
    hits: Arc<AtomicUsize>,
}

async fn slow_models(State(mock): State<SlowMock>) -> Response {
    mock.hits.fetch_add(1, Ordering::SeqCst);
    tokio::time::sleep(mock.delay).await;
    mock_models(State(Mock)).await
}

/// A catalogue that answers after `delay`, for the bounded-wait test.
async fn start_slow_mock(delay: Duration) -> (String, Arc<AtomicUsize>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock");
    let addr = listener.local_addr().expect("addr");
    let hits = Arc::new(AtomicUsize::new(0));
    let router = Router::new()
        .route("/v1/models", get(slow_models))
        .with_state(SlowMock {
            delay,
            hits: Arc::clone(&hits),
        });
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    (format!("http://{addr}"), hits)
}

/// Serve both the model list and a chat completion on one origin.
async fn start_mock() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock");
    let addr = listener.local_addr().expect("addr");
    let router = Router::new()
        .route("/v1/models", get(mock_models))
        .route("/v1/chat/completions", post(mock_chat))
        .with_state(Mock);
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    format!("http://{addr}")
}

// ----------------------------------------------------------------------- harness

struct Harness {
    state: Arc<AppState>,
    router: Router,
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

fn harness() -> Harness {
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
        concurrency: std::sync::RwLock::new(None),
        auth: rrouter_gateway::auth::Auth::load(&config().data_dir, false),
    });
    let router = app(Arc::clone(&state));
    Harness { state, router }
}

impl Harness {
    async fn connect(&self, provider: &str, base_url: &str) -> String {
        let creds = Credentials {
            api_key: Some("test-key".into()),
            base_url_override: Some(base_url.to_string()),
            ..Credentials::default()
        };
        self.state
            .store
            .create_connection(provider, "api_key", creds, Some("mock"))
            .await
            .expect("connection")
            .id
            .to_string()
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

    async fn send(&self, method: &str, uri: &str, body: Value) -> (StatusCode, Value) {
        self.call(
            Request::builder()
                .method(method)
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .expect("request"),
        )
        .await
    }

    async fn post(&self, uri: &str, body: Value) -> (StatusCode, Value) {
        self.send("POST", uri, body).await
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

    async fn models(&self) -> Vec<Value> {
        let (status, body) = self.get("/api/models").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body["data"].as_array().expect("data").clone()
    }

    async fn item(&self, reference: &str) -> Value {
        self.models()
            .await
            .into_iter()
            .find(|item| item["ref"] == reference)
            .unwrap_or_else(|| panic!("{reference} missing from GET /api/models"))
    }
}

const CAPABILITY_KEYS: [&str; 9] = [
    "vision",
    "pdf",
    "audio_input",
    "video_input",
    "image_output",
    "audio_output",
    "tools",
    "reasoning",
    "search",
];

// ------------------------------------------------------------------------- tests

#[tokio::test]
async fn catalog_rows_have_nine_capabilities_and_are_sorted_newest_first() {
    let h = harness();
    h.connect("codex", "http://127.0.0.1:9").await;
    let data = h.models().await;
    assert!(!data.is_empty());

    for item in &data {
        let caps = item["capabilities"]
            .as_object()
            .expect("capabilities object");
        assert_eq!(caps.len(), 9, "{} has {caps:?}", item["ref"]);
        for key in CAPABILITY_KEYS {
            assert!(caps[key].is_boolean(), "{} lacks {key}", item["ref"]);
        }
    }

    let order: Vec<(String, String)> = data
        .iter()
        .map(|item| {
            (
                item["provider"].as_str().expect("provider").to_string(),
                item["id"].as_str().expect("id").to_string(),
            )
        })
        .collect();
    // Newest family first inside a provider: 6 > 5.6 > an unversioned id. Retired rows
    // (none in this catalog) would sort last.
    let newest: Vec<(String, String)> = vec![
        ("codex".into(), "gpt-6-luna".into()),
        ("codex".into(), "gpt-6-sol".into()),
        ("codex".into(), "gpt-5.6-luna".into()),
        ("codex".into(), "gpt-5.6-terra".into()),
        ("codex".into(), "gpt-reserve".into()),
    ];
    assert_eq!(
        order, newest,
        "items must be grouped by provider, newest first"
    );

    let codex = h.item("codex/gpt-5.6-luna").await;
    assert_eq!(codex["source"], "catalog");
    assert_eq!(codex["provider"], "codex");
    assert_eq!(codex["id"], "gpt-5.6-luna");
    assert_eq!(codex["capabilities"]["tools"], true);
    assert_eq!(codex["capabilities"]["vision"], false);
    assert_eq!(codex["enabled"], true);
    assert_eq!(
        codex["available"], true,
        "a listing implies an active connection"
    );
    assert_eq!(codex["max_output"], 128_000);
}

/// A preference is the same on every provider row of an id, and drives `/v1/models`
/// `owned_by` only when the named provider actually serves the id.
#[tokio::test]
async fn model_preferences_drive_api_rows_and_v1_owned_by() {
    let h = harness();
    for provider in ["openai", "github"] {
        h.connect(provider, "http://127.0.0.1:9").await;
        let (status, body) = h
            .post(
                "/api/models/custom",
                json!({"provider": provider, "id": "shared-model"}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }

    for provider in ["openai", "github"] {
        let (_, body) = h.get("/api/models").await;
        let row = body["data"]
            .as_array()
            .expect("data")
            .iter()
            .find(|row| row["id"] == "shared-model" && row["provider"] == provider)
            .unwrap_or_else(|| panic!("shared row {provider} missing: {body}"));
        assert_eq!(row["preferred_provider"], Value::Null);
    }
    let (_, body) = h.get("/api/models/preferences").await;
    assert_eq!(body["data"].as_array().expect("data").len(), 0);

    let (status, body) = h
        .send(
            "PUT",
            "/api/models/preference",
            json!({"model": "shared-model", "provider": "github"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["preferences"]["shared-model"], "github");
    assert_eq!(body["data"][0]["provider"], "github");

    for provider in ["openai", "github"] {
        let (_, body) = h.get("/api/models").await;
        let row = body["data"]
            .as_array()
            .expect("data")
            .iter()
            .find(|row| row["id"] == "shared-model" && row["provider"] == provider)
            .expect("shared row");
        assert_eq!(
            row["preferred_provider"], "github",
            "both rows carry the same preference: {row}"
        );
    }
    let (_, models) = h.get("/v1/models").await;
    let shared: Vec<&Value> = models["data"]
        .as_array()
        .expect("v1 data")
        .iter()
        .filter(|row| row["id"] == "shared-model")
        .collect();
    assert_eq!(shared.len(), 1, "one entry per id");
    assert_eq!(shared[0]["owned_by"], "github");

    let (status, body) = h
        .send(
            "PUT",
            "/api/models/preference",
            json!({"model": "shared-model", "provider": null}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["preferences"].get("shared-model").is_none(), "{body}");
    let (_, models) = h.get("/v1/models").await;
    let shared = models["data"]
        .as_array()
        .expect("v1 data")
        .iter()
        .find(|row| row["id"] == "shared-model")
        .expect("shared entry");
    assert_eq!(
        shared["owned_by"], "github",
        "clearing restores the registry's first provider (sorted order)"
    );

    let (status, body) = h
        .send(
            "PUT",
            "/api/models/preference",
            json!({"model": "shared-model", "provider": 5}),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

/// `/v1/models` stays an OpenAI list but carries additive group metadata. With no
/// customized preset every provider is an implicit bucket; a default preset can rename
/// the buckets, add a mixed group and reorder them without changing the model set.
#[tokio::test]
async fn v1_models_groups_follow_the_default_preset() {
    let h = harness();
    h.connect("openai", "http://127.0.0.1:9").await;
    h.connect("anthropic", "http://127.0.0.1:9").await;

    let (status, body) = h.get("/v1/models").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let groups = body["groups"].as_array().expect("groups");
    assert!(
        groups
            .iter()
            .any(|group| group["id"] == "provider:openai" && group["name"] == "OpenAI"),
        "implicit provider buckets are the default: {groups:?}"
    );
    let gpt = body["data"]
        .as_array()
        .expect("data")
        .iter()
        .find(|model| model["id"] == "gpt-4.1" && model["owned_by"] == "openai")
        .expect("gpt-4.1");
    assert_eq!(gpt["group"], "provider:openai");

    let preset = h
        .state
        .store
        .default_model_preset()
        .await
        .expect("preset lookup")
        .expect("seeded default");
    let layout = LayoutDocument {
        schema_version: rrouter_store::LAYOUT_SCHEMA_VERSION,
        auto_include_new: true,
        groups: vec![
            LayoutGroup {
                id: "g-fast".into(),
                name: "Fast".into(),
                provider: None,
                visible: true,
            },
            LayoutGroup {
                id: "provider:openai".into(),
                name: "OpenAI custom".into(),
                provider: Some("openai".into()),
                visible: true,
            },
            LayoutGroup {
                id: "provider:anthropic".into(),
                name: "Anthropic custom".into(),
                provider: Some("anthropic".into()),
                visible: true,
            },
        ],
        items: vec![
            LayoutItem {
                provider: "openai".into(),
                id: "gpt-4.1".into(),
                group: Some("g-fast".into()),
                position: 0,
                visible: true,
                last_visible_position: None,
                hidden_at: None,
            },
            LayoutItem {
                provider: "anthropic".into(),
                id: "claude-sonnet-4-5".into(),
                group: None,
                position: 1,
                visible: true,
                last_visible_position: None,
                hidden_at: None,
            },
        ],
    };
    h.state
        .store
        .update_model_preset_layout(preset.id, preset.revision, &layout, "test")
        .await
        .expect("layout write");

    let (status, body) = h.get("/v1/models").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let groups = body["groups"].as_array().expect("groups");
    assert_eq!(groups[0]["id"], "g-fast");
    assert_eq!(groups[0]["name"], "Fast");
    assert_eq!(groups[0]["provider"], Value::Null);
    assert_eq!(groups[1]["id"], "provider:openai");
    assert_eq!(groups[1]["name"], "OpenAI custom");
    assert_eq!(groups[2]["id"], "provider:anthropic");

    let data = body["data"].as_array().expect("data");
    let gpt = data
        .iter()
        .find(|model| model["id"] == "gpt-4.1" && model["owned_by"] == "openai")
        .expect("gpt-4.1 after grouping");
    assert_eq!(gpt["group"], "g-fast");
    let claude = data
        .iter()
        .find(|model| model["id"] == "claude-sonnet-4-5" && model["owned_by"] == "anthropic")
        .expect("claude after grouping");
    assert_eq!(claude["group"], "provider:anthropic");
    assert_eq!(
        data.first().map(|model| model["id"].clone()),
        Some(Value::String("gpt-4.1".into())),
        "the custom group is first: {body}"
    );
    // Grouping does not hide models: every entry still reports a group.
    assert!(
        data.iter().all(|model| model["group"].is_string()),
        "{body}"
    );

    // A removed provider leaves rows behind in the preset (reconnect restores them), but
    // neither the model nor its provider bucket may appear in the live list.
    let current = h
        .state
        .store
        .default_model_preset()
        .await
        .expect("preset lookup")
        .expect("default");
    let mut stale = current.layout.clone();
    stale.groups.push(LayoutGroup {
        id: "provider:gone".into(),
        name: "Gone".into(),
        provider: Some("gone".into()),
        visible: true,
    });
    stale.items.push(LayoutItem {
        provider: "gone".into(),
        id: "old-model".into(),
        group: None,
        position: 2,
        visible: true,
        last_visible_position: None,
        hidden_at: None,
    });
    h.state
        .store
        .update_model_preset_layout(current.id, current.revision, &stale, "test")
        .await
        .expect("stale layout write");
    let (_, body) = h.get("/v1/models").await;
    assert!(
        body["groups"]
            .as_array()
            .expect("groups")
            .iter()
            .all(|group| group["id"] != "provider:gone"),
        "a disconnected provider's bucket is not advertised: {body}"
    );
    assert!(
        body["data"]
            .as_array()
            .expect("data")
            .iter()
            .all(|model| model["id"] != "old-model"),
        "a disconnected provider's stale model is not advertised: {body}"
    );

    // The default preset is also the discovery policy: a hidden item and every model in
    // a group toggled off disappear from `/v1/models`, while routing is untouched.
    let current = h
        .state
        .store
        .default_model_preset()
        .await
        .expect("preset lookup")
        .expect("default");
    let hidden = LayoutDocument {
        schema_version: rrouter_store::LAYOUT_SCHEMA_VERSION,
        auto_include_new: true,
        groups: vec![LayoutGroup {
            id: "g-hidden".into(),
            name: "Hidden".into(),
            provider: None,
            visible: false,
        }],
        items: vec![
            LayoutItem {
                provider: "anthropic".into(),
                id: "claude-sonnet-4-5".into(),
                group: None,
                position: 0,
                visible: true,
                last_visible_position: None,
                hidden_at: None,
            },
            LayoutItem {
                provider: "openai".into(),
                id: "gpt-4.1".into(),
                group: None,
                position: 0,
                visible: false,
                last_visible_position: Some(1),
                hidden_at: Some(1),
            },
            LayoutItem {
                provider: "anthropic".into(),
                id: "claude-opus-4-1".into(),
                group: Some("g-hidden".into()),
                position: 1,
                visible: true,
                last_visible_position: None,
                hidden_at: None,
            },
        ],
    };
    h.state
        .store
        .update_model_preset_layout(current.id, current.revision, &hidden, "test")
        .await
        .expect("visibility layout write");
    let (_, body) = h.get("/v1/models").await;
    let data = body["data"].as_array().expect("data");
    assert!(
        data.iter().all(|model| model["id"] != "gpt-4.1"),
        "a hidden item is omitted: {body}"
    );
    assert!(
        data.iter().all(|model| model["id"] != "claude-opus-4-1"),
        "a hidden group's models are omitted: {body}"
    );
    assert!(
        data.iter().any(|model| model["id"] == "claude-sonnet-4-5"),
        "a visible item remains listed: {body}"
    );
    assert!(
        body["groups"]
            .as_array()
            .expect("groups")
            .iter()
            .all(|group| group["id"] != "g-hidden"),
        "a hidden group is not advertised: {body}"
    );

    // Aliases and combos are routable names too: they join the default group by default
    // and can be hidden or assigned through the same layout items.
    h.state
        .store
        .upsert_alias("fast", "openai/gpt-4.1")
        .await
        .expect("alias");
    h.state
        .store
        .upsert_combo("duo", &["openai/gpt-4.1".to_string()])
        .await
        .expect("combo");
    let (_, body) = h.get("/v1/models").await;
    let data = body["data"].as_array().expect("data");
    assert!(
        data.iter()
            .any(|model| model["id"] == "fast" && model["owned_by"] == "rrouter"),
        "alias is listed: {body}"
    );
    assert!(
        data.iter()
            .any(|model| model["id"] == "duo" && model["owned_by"] == "rrouter"),
        "combo is listed: {body}"
    );

    let current = h
        .state
        .store
        .default_model_preset()
        .await
        .expect("preset lookup")
        .expect("default");
    let mut aliases_hidden = current.layout.clone();
    aliases_hidden.groups.push(LayoutGroup {
        id: "g-hidden-names".into(),
        name: "Hidden names".into(),
        provider: None,
        visible: false,
    });
    aliases_hidden.items.extend([
        LayoutItem {
            provider: "rrouter".into(),
            id: "fast".into(),
            group: None,
            position: 1,
            visible: false,
            last_visible_position: Some(2),
            hidden_at: Some(1),
        },
        LayoutItem {
            provider: "rrouter".into(),
            id: "duo".into(),
            group: Some("g-hidden-names".into()),
            position: 2,
            visible: true,
            last_visible_position: None,
            hidden_at: None,
        },
    ]);
    h.state
        .store
        .update_model_preset_layout(current.id, current.revision, &aliases_hidden, "test")
        .await
        .expect("alias layout write");
    let (_, body) = h.get("/v1/models").await;
    let data = body["data"].as_array().expect("data");
    assert!(
        data.iter().all(|model| model["id"] != "fast"),
        "hidden alias is omitted: {body}"
    );
    assert!(
        data.iter().all(|model| model["id"] != "duo"),
        "combo in a hidden group is omitted: {body}"
    );
}

#[tokio::test]
async fn custom_row_wins_over_a_catalog_id_of_the_same_provider() {
    let h = harness();
    h.connect("codex", "http://127.0.0.1:9").await;
    let (status, body) = h
        .post(
            "/api/models/custom",
            json!({
                "provider": "codex",
                "id": "gpt-5",
                "display": "My Own GPT",
                "context": 999_999,
                "max_output": 4_096,
                "capabilities": {"vision": true},
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["source"], "custom");

    let item = h.item("codex/gpt-5").await;
    assert_eq!(item["source"], "custom");
    assert_eq!(item["display"], "My Own GPT");
    assert_eq!(item["context"], 999_999);
    assert_eq!(item["max_output"], 4_096);
    // `vision` came from the custom row, `tools` fell through to the catalog default.
    assert_eq!(item["capabilities"]["vision"], true);
    assert_eq!(item["capabilities"]["tools"], true);
    assert_eq!(item["capabilities"]["pdf"], false);
    assert_eq!(
        h.models()
            .await
            .iter()
            .filter(|i| i["ref"] == "codex/gpt-5")
            .count(),
        1,
        "a custom row must not duplicate the catalog entry"
    );
}

#[tokio::test]
async fn custom_post_delete_round_trip() {
    let h = harness();
    let (status, body) = h
        .post(
            "/api/models/custom",
            json!({
                "provider": "openai",
                "id": "my-model",
                "display": "My Model",
                "context": 128_000,
                "max_output": 8_000,
                "capabilities": {"tools": true, "vision": true, "search": true},
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["ref"], "openai/my-model");
    assert_eq!(body["provider"], "openai");
    assert_eq!(body["id"], "my-model");
    assert_eq!(body["display"], "My Model");
    assert_eq!(body["context"], 128_000);
    assert_eq!(body["max_output"], 8_000);
    assert_eq!(body["source"], "custom");
    assert_eq!(body["capabilities"].as_object().expect("caps").len(), 9);
    assert_eq!(body["capabilities"]["vision"], true);
    assert_eq!(body["capabilities"]["search"], true);
    assert_eq!(body["capabilities"]["pdf"], false);

    let (status, body) = h
        .delete("/api/models/custom?provider=openai&id=my-model")
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["deleted"], true);
    assert!(h
        .models()
        .await
        .iter()
        .all(|item| item["ref"] != "openai/my-model"));

    let (status, body) = h
        .delete("/api/models/custom?provider=openai&id=my-model")
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert!(body["error"]["code"].is_string(), "{body}");
    assert!(body["error"]["message"].is_string(), "{body}");
    assert!(body["error"]["type"].is_string(), "{body}");
}

#[tokio::test]
async fn deleting_a_non_custom_row_is_a_404() {
    let h = harness();
    h.state
        .store
        .upsert_model(&ModelRow {
            provider: "openai".into(),
            model: "synced-only".into(),
            display: None,
            context: None,
            max_output: None,
            caps: json!({}),
            cost: None,
            source: "synced".into(),
            updated_at: 0,
        })
        .await
        .expect("row");
    let (status, _) = h
        .delete("/api/models/custom?provider=openai&id=synced-only")
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // models.dev rows describe models; they never become entries of their own.
    assert!(
        !h.models()
            .await
            .iter()
            .any(|item| item["ref"] == "openai/synced-only"),
        "a synced row must not add a model to the registry"
    );
}

#[tokio::test]
async fn disabled_models_flips_enabled_to_false() {
    let h = harness();
    h.connect("codex", "http://127.0.0.1:9").await;
    h.connect("openai", "http://127.0.0.1:9").await;
    // Two providers serving the same id: disabling is keyed by model id alone, so
    // both must flip. Custom rows make the test independent of catalog contents.
    for provider in ["codex", "openai"] {
        let (status, body) = h
            .post(
                "/api/models/custom",
                json!({"provider": provider, "id": "shared-id", "display": "Shared"}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    assert_eq!(h.item("codex/shared-id").await["enabled"], true);
    assert_eq!(h.item("openai/shared-id").await["enabled"], true);

    let (status, body) = h.send("PUT", "/api/disabled/shared-id", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(h.item("codex/shared-id").await["enabled"], false);
    assert_eq!(h.item("openai/shared-id").await["enabled"], false);

    let (status, _) = h
        .send("PUT", "/api/disabled/shared-id/enable", Value::Null)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(h.item("codex/shared-id").await["enabled"], true);
}

/// `/v1/models` must not advertise a disabled model, and re-enabling restores it.
/// A shared id stays listed while one of its providers is connected.
#[tokio::test]
async fn v1_models_omits_disabled_models_and_keeps_preferred_ownership() {
    let h = harness();
    for provider in ["openai", "github"] {
        h.connect(provider, "http://127.0.0.1:9").await;
        let (status, body) = h
            .post(
                "/api/models/custom",
                json!({"provider": provider, "id": "shared-id"}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    h.send(
        "PUT",
        "/api/models/preference",
        json!({"model": "shared-id", "provider": "github"}),
    )
    .await;

    let (status, body) = h.get("/v1/models").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let shared = body["data"]
        .as_array()
        .expect("data")
        .iter()
        .find(|row| row["id"] == "shared-id")
        .expect("enabled shared id");
    assert_eq!(shared["owned_by"], "github", "preference still wins");

    let (status, body) = h.send("PUT", "/api/disabled/shared-id", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (_, body) = h.get("/v1/models").await;
    assert!(
        !body["data"]
            .as_array()
            .expect("data")
            .iter()
            .any(|row| row["id"] == "shared-id"),
        "a disabled model is not advertised: {body}"
    );

    let (status, _) = h
        .send("PUT", "/api/disabled/shared-id/enable", Value::Null)
        .await;
    assert_eq!(status, StatusCode::OK);
    let (_, body) = h.get("/v1/models").await;
    let shared = body["data"]
        .as_array()
        .expect("data")
        .iter()
        .find(|row| row["id"] == "shared-id")
        .expect("re-enabled id returns");
    assert_eq!(shared["owned_by"], "github");

    // One provider disconnected: the id still lists through the other, and ownership
    // falls back because the preferred provider no longer serves it.
    h.state
        .store
        .set_connection_active(
            h.state
                .store
                .list_connections(Some("github"))
                .await
                .expect("connections")[0]
                .id,
            false,
        )
        .await
        .expect("disable connection");
    let (_, body) = h.get("/v1/models").await;
    let shared = body["data"]
        .as_array()
        .expect("data")
        .iter()
        .find(|row| row["id"] == "shared-id")
        .expect("one enabled provider still lists the id");
    assert_eq!(shared["owned_by"], "openai");
}

/// Auto-ping API: one row per active Claude/Codex OAuth account, defaults off, and
/// validated schedule edits.
#[tokio::test]
async fn quota_ping_api_lists_supported_accounts_and_validates_edits() {
    let h = harness();
    let openai = h.connect("openai", "http://127.0.0.1:9").await;
    let claude = h
        .state
        .store
        .create_connection(
            "claude",
            "oauth",
            Credentials {
                access_token: Some("claude-token".into()),
                ..Credentials::default()
            },
            Some("subscription"),
        )
        .await
        .expect("claude")
        .id;
    let _codex = h
        .state
        .store
        .create_connection(
            "codex",
            "oauth",
            Credentials {
                access_token: Some("codex-token".into()),
                ..Credentials::default()
            },
            Some("codex sub"),
        )
        .await
        .expect("codex")
        .id;

    let (status, body) = h.get("/api/quota-pings").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let rows = body["data"].as_array().expect("data");
    assert_eq!(rows.len(), 2, "only supported OAuth accounts: {body}");
    for row in rows {
        assert_eq!(row["mode"], "off");
        assert_eq!(row["anchor"], "09:30");
        assert_eq!(row["timezone"], "UTC");
        assert_eq!(row["window_hours"], 5.0);
        assert_eq!(row["slots_per_day"], 3);
        assert_eq!(row["smart"]["recent_requests"], 0);
    }

    let (status, body) = h
        .send(
            "PUT",
            &format!("/api/quota-pings/{claude}"),
            json!({
                "mode": "schedule",
                "anchor": "07:15",
                "timezone": "Europe/Berlin",
                "window_hours": 5,
                "slots_per_day": 4,
                "ping_model": "claude-haiku-4-5",
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["mode"], "schedule");
    assert_eq!(body["anchor"], "07:15");
    assert_eq!(body["timezone"], "Europe/Berlin");
    assert_eq!(body["slots_per_day"], 4);
    assert!(body["next_ping_at"].as_i64().is_some());

    for invalid in [
        json!({"timezone": "Mars/Olympus"}),
        json!({"anchor": "7:15"}),
        json!({"window_hours": 0.4}),
        json!({"slots_per_day": 5}),
        json!({"mode": "sometimes"}),
        json!({"ping_model": 7}),
    ] {
        let (status, body) = h
            .send("PUT", &format!("/api/quota-pings/{claude}"), invalid)
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    }

    let (status, body) = h
        .send(
            "PUT",
            &format!("/api/quota-pings/{claude}"),
            json!({"mode": "off"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["mode"], "off");
    assert_eq!(body["next_ping_at"], Value::Null);

    let (status, _) = h
        .send(
            "PUT",
            &format!("/api/quota-pings/{openai}"),
            json!({"mode": "off"}),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "an API-key provider has no quota ping"
    );

    let (status, _) = h
        .send(
            "PUT",
            "/api/quota-pings/00000000-0000-0000-0000-000000000000",
            json!({"mode": "off"}),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// The registry answers one question: what can this gateway call right now.
#[tokio::test]
async fn registry_lists_nothing_until_a_provider_is_connected() {
    let h = harness();
    assert!(h.models().await.is_empty(), "no connection means no models");

    // models.dev rows are metadata: they describe a model, they do not add one.
    h.state
        .store
        .upsert_model(&ModelRow {
            provider: "openai".into(),
            model: "gpt-4.1".into(),
            display: Some("GPT-4.1".into()),
            context: Some(400_000),
            max_output: Some(32_768),
            caps: json!({"vision": true}),
            cost: Some(json!({"input": 2.0, "output": 8.0})),
            source: "synced".into(),
            updated_at: 0,
        })
        .await
        .expect("synced row");
    assert!(
        h.models().await.is_empty(),
        "a synced row must not list a model"
    );

    h.connect("openai", "http://127.0.0.1:9").await;
    let item = h.item("openai/gpt-4.1").await;
    assert_eq!(item["source"], "catalog", "the catalog is why it exists");
    // ...and models.dev is why we know its limits and price.
    assert_eq!(item["context"], 400_000);
    assert_eq!(item["max_output"], 32_768);
    assert_eq!(item["capabilities"]["vision"], true);
    assert_eq!(item["cost"]["input"], 2.0);
    assert_eq!(item["cost"]["output"], 8.0);
}

/// A custom definition outlives the account: it is hidden from the list while its
/// provider is disconnected, and echoed by the POST that created it.
#[tokio::test]
async fn custom_row_is_hidden_until_its_provider_is_connected() {
    let h = harness();
    let (status, body) = h
        .post(
            "/api/models/custom",
            json!({"provider": "openai", "id": "internal-model", "context": 8_000}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["ref"], "openai/internal-model");
    assert_eq!(body["available"], false);
    assert!(!h
        .models()
        .await
        .iter()
        .any(|i| i["ref"] == "openai/internal-model"));

    h.connect("openai", "http://127.0.0.1:9").await;
    assert!(h
        .models()
        .await
        .iter()
        .any(|i| i["ref"] == "openai/internal-model"));
}

#[tokio::test]
async fn providers_report_connection_and_list_support() {
    let h = harness();
    let (status, body) = h.get("/api/models/providers").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let data = body["data"].as_array().expect("data");
    assert!(!data.is_empty());
    let codex = data.iter().find(|p| p["id"] == "codex").expect("codex");
    assert_eq!(codex["listed"], false);
    assert_eq!(codex["has_connection"], false);
    assert!(codex["display"]
        .as_str()
        .expect("display")
        .contains("Codex"));
    let openai = data.iter().find(|p| p["id"] == "openai").expect("openai");
    assert_eq!(openai["listed"], true);

    let mock = start_mock().await;
    h.connect("openai", &mock).await;
    let (_, body) = h.get("/api/models/providers").await;
    let openai = body["data"]
        .as_array()
        .expect("data")
        .iter()
        .find(|p| p["id"] == "openai")
        .expect("openai")
        .clone();
    assert_eq!(openai["has_connection"], true);
}

#[tokio::test]
async fn fetch_stores_upstream_ids_and_never_beats_custom() {
    let h = harness();
    let mock = start_mock().await;
    let connection_id = h.connect("openai", &mock).await;

    let (status, body) = h
        .post("/api/models/fetch", json!({"connection_id": connection_id}))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["provider"], "openai");
    assert_eq!(body["status"], 200);
    // The mock repeats "alpha" and includes an empty id; both are dropped.
    assert_eq!(body["count"], 2);
    assert_eq!(body["added"], 2);
    assert_eq!(body["data"], json!(["alpha", "beta"]));

    let item = h.item("openai/alpha").await;
    assert_eq!(item["source"], "upstream");
    assert_eq!(item["available"], true);

    // A second fetch adds nothing.
    let (_, body) = h
        .post("/api/models/fetch", json!({"connection_id": connection_id}))
        .await;
    assert_eq!(body["added"], 0);

    // An operator row for the same id outranks the upstream row, and a later fetch
    // must not demote it.
    h.post(
        "/api/models/custom",
        json!({"provider": "openai", "id": "alpha", "display": "Alpha Custom"}),
    )
    .await;
    h.post("/api/models/fetch", json!({"connection_id": connection_id}))
        .await;
    let item = h.item("openai/alpha").await;
    assert_eq!(item["source"], "custom");
    assert_eq!(item["display"], "Alpha Custom");
}

#[tokio::test]
async fn fetch_for_codex_is_a_400_admin_error() {
    let h = harness();
    let mock = start_mock().await;
    let connection_id = h.connect("codex", &mock).await;
    let (status, body) = h
        .post("/api/models/fetch", json!({"connection_id": connection_id}))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body["error"]["message"]
        .as_str()
        .expect("message")
        .contains("no model list endpoint"));
}

#[tokio::test]
async fn model_test_runs_a_real_completion() {
    let h = harness();
    let mock = start_mock().await;
    h.connect("openai", &mock).await;

    let (status, body) = h
        .post("/api/models/test", json!({"model": "openai/gpt-4.1"}))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["ok"], true);
    assert_eq!(body["status"], 200);
    assert_eq!(body["model"], "openai/gpt-4.1");
    assert_eq!(body["reply"], "pong");
    assert!(body["latency_ms"].is_number());
    assert!(body.get("error").is_none());
}

#[tokio::test]
async fn model_test_failure_keeps_http_200_and_reports_status() {
    let h = harness();
    let (status, body) = h
        .post(
            "/api/models/test",
            json!({"model": "no-such-provider/ghost"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["ok"], false);
    assert_eq!(body["status"], 404);
    assert!(body["error"].as_str().expect("error").contains("ghost"));
    assert!(body["latency_ms"].is_number());
}

#[test]
fn sync_parser_maps_models_dev_into_rows() {
    let document = json!({
        "openai": {
            "models": {
                "gpt-4.1": {
                    "name": "GPT-4.1",
                    "reasoning": false,
                    "tool_call": true,
                    "modalities": {"input": ["text", "image"], "output": ["text"]},
                    "limit": {"context": 1_047_576, "output": 32_768}
                }
            }
        },
        "google": {
            "models": {
                "gemini-2.5-pro": {
                    "name": "Gemini 2.5 Pro",
                    "reasoning": true,
                    "tool_call": true,
                    "modalities": {"input": ["text", "image", "pdf"], "output": ["text"]},
                    "limit": {"context": 1_048_576, "output": 65_536}
                }
            }
        },
        "some-new-vendor": {
            "models": {
                "x1": {
                    "name": "X1",
                    "tool_call": false,
                    "modalities": {"input": ["text"], "output": ["text"]},
                    "limit": {"context": 1_000, "output": 100}
                }
            }
        }
    });
    let rows = parse_models_dev(&document);
    assert!(!rows.is_empty());
    assert!(
        rows.len() <= 3 * 4,
        "at most one row per mirrored provider id"
    );

    let find = |provider: &str, model: &str| {
        rows.iter()
            .find(|row| row.provider == provider && row.model == model)
            .unwrap_or_else(|| panic!("{provider}/{model} missing"))
    };

    let openai = find("openai", "gpt-4.1");
    assert_eq!(openai.source, "synced");
    assert_eq!(openai.display.as_deref(), Some("GPT-4.1"));
    assert_eq!(openai.context, Some(1_047_576));
    assert_eq!(openai.max_output, Some(32_768));
    assert_eq!(openai.caps["tools"], true);
    assert_eq!(openai.caps["reasoning"], false);
    assert_eq!(openai.caps["vision"], true);
    assert_eq!(openai.caps["pdf"], false);
    assert_eq!(openai.caps["audio_output"], false);
    assert!(
        openai.caps.get("search").is_none(),
        "search is never derived"
    );

    // The alias map mirrors the vendor's models under product lines that serve the same
    // catalog, but never under codex: that account cannot call OpenAI's full list.
    assert!(
        !rows.iter().any(|row| row.provider == "codex"),
        "codex must not inherit the vendor catalog"
    );
    assert_eq!(find("gemini", "gemini-2.5-pro").context, Some(1_048_576));
    assert_eq!(find("gemini", "gemini-2.5-pro").caps["pdf"], true);
    assert_eq!(find("gemini", "gemini-2.5-pro").caps["reasoning"], true);

    // A vendor this binary cannot route is dropped: storing 240 providers' models
    // floods every list and model select with entries nobody can call.
    assert!(
        !rows.iter().any(|row| row.provider == "some-new-vendor"),
        "unroutable provider kept: {:?}",
        rows.iter().map(|r| &r.provider).collect::<Vec<_>>()
    );
}

#[test]
fn sync_parser_ignores_a_non_object_document() {
    assert!(parse_models_dev(&json!([])).is_empty());
    assert!(parse_models_dev(&json!({"p": {"no_models": 1}})).is_empty());
    assert_eq!(MAX_FETCHED_IDS, 5000);
}

// ------------------------------------------------------------ provider base URLs

/// The provider row carries the two facts the add-provider form needs: whether a base URL
/// is mandatory, and a shape to type into the field.
#[tokio::test]
async fn providers_payload_flags_the_override_requirement() {
    let h = harness();
    let (status, body) = h.get("/api/models/providers").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let data = body["data"].as_array().expect("data");
    let row = |id: &str| {
        data.iter()
            .find(|p| p["id"] == id)
            .unwrap_or_else(|| panic!("{id} missing"))
            .clone()
    };

    let azure = row("azure");
    assert_eq!(azure["requires_base_url_override"], true);
    assert_eq!(
        azure["base_url_example"], "https://<resource>.openai.azure.com",
        "the example is the origin shape, not the manifest placeholder: {azure}"
    );
    assert_eq!(azure["kind"], "api_key");
    assert_eq!(
        azure["listed"], false,
        "Azure publishes no plain model list"
    );

    // Every other provider keeps its old answer and gains `false`, not a missing key.
    let openai = row("openai");
    assert_eq!(openai["requires_base_url_override"], false);
    assert_eq!(openai["base_url_example"], "https://api.openai.com");
    assert_eq!(openai["listed"], true);
    assert_eq!(openai["has_connection"], false);

    // Nothing in the payload is credential material.
    let text = body.to_string();
    assert!(!text.contains("api-key"), "{text}");
    assert!(!text.contains("Bearer "), "{text}");
}

/// Saving Azure without a resource URL is refused before the row exists, and the message
/// names what the operator must supply.
#[tokio::test]
async fn an_azure_connection_needs_a_resource_url() {
    let h = harness();
    let (status, body) = h
        .post(
            "/api/connections",
            json!({
                "provider": "azure",
                "auth_type": "api_key",
                "credentials": {"api_key": "azure-secret"},
            }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let message = body["error"]["message"].as_str().expect("message");
    assert!(
        message.contains("requires a connection base URL"),
        "{message}"
    );
    assert!(
        !body.to_string().contains("azure-secret"),
        "the credential never echoes back: {body}"
    );

    let (status, list) = h.get("/api/connections").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        list["data"].as_array().expect("data").len(),
        0,
        "a refused save writes nothing"
    );
}

/// A resource URL is what makes the account usable; the view reports it without the key.
#[tokio::test]
async fn an_azure_connection_accepts_a_resource_url() {
    let h = harness();
    let (status, created) = h
        .post(
            "/api/connections",
            json!({
                "provider": "azure",
                "auth_type": "api_key",
                "name": "contoso",
                "credentials": {
                    "api_key": "azure-secret",
                    "base_url_override": "https://contoso.openai.azure.com",
                },
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["provider"], "azure");
    assert_eq!(
        created["base_url_override"],
        "https://contoso.openai.azure.com"
    );
    assert_eq!(created["has_api_key"], true);
    assert!(
        !created.to_string().contains("azure-secret"),
        "the row reports booleans, not the key: {created}"
    );

    let (status, body) = h.get("/api/models/providers").await;
    assert_eq!(status, StatusCode::OK);
    let azure = body["data"]
        .as_array()
        .expect("data")
        .iter()
        .find(|p| p["id"] == "azure")
        .expect("azure")
        .clone();
    assert_eq!(azure["has_connection"], true);
}

/// Copying the manifest placeholder into the field is the same mistake spelled out, and it
/// is caught even though the UI would have blocked it.
#[tokio::test]
async fn an_azure_placeholder_override_is_refused() {
    let h = harness();
    for placeholder in [
        "https://RESOURCE.openai.azure.com",
        "https://your-resource.openai.azure.com",
        "https://placeholder.openai.azure.com",
    ] {
        let (status, body) = h
            .post(
                "/api/connections",
                json!({
                    "provider": "azure",
                    "auth_type": "api_key",
                    "credentials": {
                        "api_key": "azure-secret",
                        "base_url_override": placeholder,
                    },
                }),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{placeholder}: {body}");
        let message = body["error"]["message"].as_str().expect("message");
        assert!(
            message.contains("placeholder host"),
            "{placeholder}: {message}"
        );
    }
    let (_, list) = h.get("/api/connections").await;
    assert_eq!(list["data"].as_array().expect("data").len(), 0);
}

/// An update validates what the row would keep: omitting the URL keeps it, sending a new
/// one replaces it, and an explicitly empty value is refused instead of breaking dispatch.
#[tokio::test]
async fn updating_a_connection_keeps_or_replaces_the_override() {
    let h = harness();
    let (status, created) = h
        .post(
            "/api/connections",
            json!({
                "provider": "azure",
                "auth_type": "api_key",
                "credentials": {
                    "api_key": "azure-secret",
                    "base_url_override": "https://contoso.openai.azure.com",
                },
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let id = created["id"].as_str().expect("id");

    // Touching another credential field leaves the base URL alone.
    let (status, kept) = h
        .send(
            "PATCH",
            &format!("/api/connections/{id}"),
            json!({"credentials": {"api_key": "rotated-secret"}}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{kept}");
    assert_eq!(
        kept["base_url_override"],
        "https://contoso.openai.azure.com"
    );
    assert_eq!(kept["has_api_key"], true);

    // A new resource replaces it.
    let (status, replaced) = h
        .send(
            "PATCH",
            &format!("/api/connections/{id}"),
            json!({"credentials": {"base_url_override": "https://fabrikam.openai.azure.com"}}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{replaced}");
    assert_eq!(
        replaced["base_url_override"],
        "https://fabrikam.openai.azure.com"
    );

    // Replacing it with the placeholder is refused, and the good value survives.
    let (status, body) = h
        .send(
            "PATCH",
            &format!("/api/connections/{id}"),
            json!({"credentials": {"base_url_override": "https://RESOURCE.openai.azure.com"}}),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    // `null` means "no change" for credentials (the merge model), and `""` is refused
    // rather than stored as a URL that would dispatch to the origin root.
    let (status, kept) = h
        .send(
            "PATCH",
            &format!("/api/connections/{id}"),
            json!({"credentials": {"base_url_override": null}}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{kept}");
    assert_eq!(
        kept["base_url_override"],
        "https://fabrikam.openai.azure.com"
    );

    let (status, body) = h
        .send(
            "PATCH",
            &format!("/api/connections/{id}"),
            json!({"credentials": {"base_url_override": "  "}}),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let message = body["error"]["message"].as_str().expect("message");
    assert!(message.contains("cannot be empty"), "{message}");

    let (_, after) = h.get("/api/connections").await;
    assert_eq!(
        after["data"][0]["base_url_override"], "https://fabrikam.openai.azure.com",
        "a refused clear writes nothing"
    );
}

/// Providers whose manifest host is real keep working exactly as before: no override is
/// required, and one that is supplied still wins.
#[tokio::test]
async fn providers_without_the_flag_stay_unchanged() {
    let h = harness();
    let (status, created) = h
        .post(
            "/api/connections",
            json!({
                "provider": "openai",
                "auth_type": "api_key",
                "credentials": {"api_key": "sk-test"},
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(
        created["base_url_override"],
        serde_json::Value::Null,
        "no override, no requirement"
    );

    let id = created["id"].as_str().expect("id");
    let mock = start_mock().await;
    let (status, override_url) = h
        .send(
            "PATCH",
            &format!("/api/connections/{id}"),
            json!({"credentials": {"base_url_override": mock}}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{override_url}");
    assert_eq!(override_url["base_url_override"], mock);

    // A placeholder is still refused even for a provider that does not require an
    // override: the host would never resolve.
    let (status, body) = h
        .send(
            "PATCH",
            &format!("/api/connections/{id}"),
            json!({"credentials": {"base_url_override": "https://RESOURCE.example.com"}}),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

/// Creating a connection waits for the catalogue, so a dashboard invalidation fired
/// immediately after the 201 sees the provider's rows.
#[tokio::test]
async fn connection_create_returns_after_the_model_fetch() {
    let h = harness();
    let mock = start_mock().await;
    let (status, created) = h
        .post(
            "/api/connections",
            json!({
                "provider": "openai",
                "auth_type": "api_key",
                "name": "catalogue",
                "credentials": {
                    "api_key": "test-key",
                    "base_url_override": mock,
                },
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let rows = h.state.store.list_models().await.expect("models");
    assert!(
        rows.iter()
            .any(|row| row.provider == "openai" && row.model == "alpha"),
        "the rows exist before the response: {rows:?}"
    );
}

/// Credential edits refresh the catalogue; name-only edits do not need a provider call.
#[tokio::test]
async fn connection_update_refreshes_models_on_credential_change() {
    let h = harness();
    let mock = start_mock().await;
    let id = h.connect("openai", &mock).await;
    h.state
        .store
        .clear_models_by_source("upstream")
        .await
        .expect("clear");

    let (status, updated) = h
        .send(
            "PATCH",
            &format!("/api/connections/{id}"),
            json!({"name": "renamed"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    assert!(
        h.state
            .store
            .list_models()
            .await
            .expect("models")
            .is_empty(),
        "a name-only edit must not fetch"
    );

    let (status, updated) = h
        .send(
            "PATCH",
            &format!("/api/connections/{id}"),
            json!({"credentials": {"api_key": "rotated"}}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    let rows = h.state.store.list_models().await.expect("models");
    assert!(
        rows.iter()
            .any(|row| row.provider == "openai" && row.model == "alpha"),
        "a credential edit refreshes before responding: {rows:?}"
    );
}

/// The wait is a bound, not a cancellation: a timed-out fetch keeps running and writes
/// its rows afterwards.
#[tokio::test]
async fn a_timed_out_initial_fetch_is_detached_not_aborted() {
    let h = harness();
    let (mock, hits) = start_slow_mock(Duration::from_millis(400)).await;
    let id = uuid::Uuid::parse_str(&h.connect("openai", &mock).await).expect("id");

    let started = Instant::now();
    rrouter_gateway::routes::models::wait_initial_fetch_within(
        Arc::clone(&h.state),
        id,
        Duration::from_millis(50),
    )
    .await;
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "the wait must return at its bound, not the provider's pace"
    );

    tokio::time::sleep(Duration::from_millis(600)).await;
    assert!(hits.load(Ordering::SeqCst) >= 1, "the detached task ran");
    let rows = h.state.store.list_models().await.expect("models");
    assert!(
        rows.iter()
            .any(|row| row.provider == "openai" && row.model == "alpha"),
        "the detached task wrote its rows: {rows:?}"
    );
}
