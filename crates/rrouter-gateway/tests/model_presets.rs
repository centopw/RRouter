//! Model presets end to end: the admin library, the per-account active choice, and the
//! revision-guarded layout write.
//!
//! The harness mirrors `tests/teams.rs`: an admin token is configured, so every answer here
//! comes from a session's role rather than the loopback shortcut. v30 seeds one default
//! preset, so "Default" is always present; tests that need a different default promote one
//! through the store, which is also the only way to change it (this API has no route for
//! that by contract).

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{HeaderMap, Method, Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use rrouter_gateway::{app, auth::Auth, config::Config, AppState};
use rrouter_router::{Catalog, Planner};
use rrouter_store::{LayoutDocument, Store, DEFAULT_MODEL_PRESET_ID, LAYOUT_SCHEMA_VERSION};
use serde_json::{json, Value};
use tower::ServiceExt;

struct H {
    state: Arc<AppState>,
    router: Router,
}

const ADMIN_TOKEN: &str = "adm-test-presets";

fn config() -> Config {
    Config {
        host: "127.0.0.1".parse().expect("ip"),
        port: 0,
        data_dir: std::env::temp_dir().join(format!("rrouter-presets-{}", uuid::Uuid::new_v4())),
        master_key: None,
        require_api_key: false,
        request_timeout_secs: 30,
        connect_timeout_secs: 5,
        log_filter: "rrouter=error".into(),
        static_dir: None,
        cors_origins: vec![],
        allow_remote_admin: false,
        admin_token: Some(ADMIN_TOKEN.into()),
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
    let dispatcher = Arc::new(rrouter_gateway::state::dispatcher(&config));
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

fn cookie_of(headers: &HeaderMap) -> Option<String> {
    headers
        .get("set-cookie")
        .and_then(|v| v.to_str().ok())
        .and_then(|raw| raw.split(';').next())
        .map(|pair| pair.trim().to_string())
        .filter(|pair| pair.starts_with("rrouter_session="))
}

fn auth_headers(cookie: &str) -> [(&str, &str); 1] {
    [("cookie", cookie)]
}

fn csrf(cookie: &str) -> [(&str, &str); 2] {
    [("cookie", cookie), ("x-rrouter-csrf", "1")]
}

fn token() -> [(&'static str, &'static str); 1] {
    [("x-admin-token", ADMIN_TOKEN)]
}

/// A layout document in the shape the store validates: a schema version, the merge flag and
/// `(provider, id, position)` items.
fn layout() -> Value {
    json!({
        "schema_version": LAYOUT_SCHEMA_VERSION,
        "auto_include_new": true,
        "items": [
            {"provider": "openai", "id": "gpt-4.1", "position": 0, "visible": true},
            {"provider": "claude", "id": "claude-sonnet-4-5", "position": 0, "visible": false},
        ]
    })
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
        let (status, _, out) = self.call(Method::POST, uri, Some(body), headers).await;
        (status, out)
    }

    async fn patch(&self, uri: &str, body: Value, headers: &[(&str, &str)]) -> (StatusCode, Value) {
        let (status, _, out) = self.call(Method::PATCH, uri, Some(body), headers).await;
        (status, out)
    }

    async fn put(&self, uri: &str, body: Value, headers: &[(&str, &str)]) -> (StatusCode, Value) {
        let (status, _, out) = self.call(Method::PUT, uri, Some(body), headers).await;
        (status, out)
    }

    async fn delete(&self, uri: &str, headers: &[(&str, &str)]) -> (StatusCode, Value) {
        let (status, _, out) = self.call(Method::DELETE, uri, None, headers).await;
        (status, out)
    }

    fn setup_code(&self) -> String {
        self.state.auth.code().unwrap_or_default()
    }

    /// Create the owner and return its session cookie.
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

    async fn invite_member(&self, owner: &str, email: &str) -> String {
        let (status, body) = self
            .post(
                "/api/users/invites",
                json!({"email": email, "role": "member"}),
                &csrf(owner),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "invite: {body}");
        let invite = body["token"].as_str().expect("token").to_string();
        let (status, headers, signed) = self
            .call(
                Method::POST,
                "/api/auth/signup",
                Some(
                    json!({"token": invite, "name": "Reader", "password": "another long password"}),
                ),
                &[],
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "signup: {signed}");
        cookie_of(&headers).expect("session cookie")
    }

    /// A preset seeded through the store: the API has no route that creates a *default*,
    /// so promoting one is a store call by design.
    async fn seed(&self, name: &str, make_default: bool) -> uuid::Uuid {
        let preset = self
            .state
            .store
            .create_model_preset(name, Some("seeded"), &LayoutDocument::default(), "test")
            .await
            .expect("create preset");
        if make_default {
            self.state
                .store
                .set_default_model_preset(preset.id, "test")
                .await
                .expect("set default");
        }
        preset.id
    }
}

/// Preset names, sorted: the store's order is its own business, and a test that depended on
/// it would fail for a reason unrelated to the API.
fn names(body: &Value) -> Vec<String> {
    let mut names: Vec<String> = body["data"]
        .as_array()
        .expect("data")
        .iter()
        .map(|row| row["name"].as_str().unwrap_or_default().to_string())
        .collect();
    names.sort();
    names
}

#[tokio::test]
async fn an_admin_creates_lists_and_reads_a_preset() {
    let h = harness();
    let owner = h.setup().await;

    let (status, created) = h
        .post(
            "/api/model-presets",
            json!({"name": "Default board", "description": "the starter", "layout": layout()}),
            &csrf(&owner),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let id = created["id"].as_str().expect("id").to_string();
    assert_eq!(created["name"], json!("Default board"));
    assert_eq!(created["revision"], json!(1));
    assert_eq!(created["layout"], layout(), "the layout round-trips");
    assert_eq!(created["is_default"], json!(false));

    // The list is metadata: names and revisions, not every model on every board. v30's
    // seeded preset is present from the start.
    let (status, list) = h.get("/api/model-presets", &auth_headers(&owner)).await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert_eq!(names(&list), vec!["Default", "Default board"]);
    assert!(
        list["data"]
            .as_array()
            .expect("data")
            .iter()
            .all(|row| row.get("layout").is_none()),
        "the list carries metadata only: {list}"
    );
    assert_eq!(
        list["default_id"],
        json!(DEFAULT_MODEL_PRESET_ID.to_string()),
        "the migration's preset is the default until another is promoted"
    );

    let (status, one) = h
        .get(&format!("/api/model-presets/{id}"), &auth_headers(&owner))
        .await;
    assert_eq!(status, StatusCode::OK, "{one}");
    assert_eq!(one["layout"], layout());
}

#[tokio::test]
async fn a_rename_keeps_the_layout_and_the_revision() {
    let h = harness();
    let owner = h.setup().await;
    let id = h.seed("Board", false).await.to_string();

    let (status, renamed) = h
        .patch(
            &format!("/api/model-presets/{id}"),
            json!({"name": "Renamed"}),
            &csrf(&owner),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{renamed}");
    assert_eq!(renamed["name"], json!("Renamed"));
    assert_eq!(
        renamed["description"],
        json!("seeded"),
        "an absent field is kept"
    );
    assert_eq!(
        renamed["revision"],
        json!(1),
        "metadata does not bump the layout"
    );

    // `null` clears the description; the name is left alone.
    let (status, cleared) = h
        .patch(
            &format!("/api/model-presets/{id}"),
            json!({"description": null}),
            &csrf(&owner),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{cleared}");
    assert_eq!(cleared["description"], Value::Null);
    assert_eq!(cleared["name"], json!("Renamed"));
}

#[tokio::test]
async fn the_default_is_what_a_token_caller_resolves_to() {
    let h = harness();
    let first = h.seed("First", false).await;
    let second = h.seed("Second", true).await;

    let (status, list) = h.get("/api/model-presets", &token()).await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert_eq!(list["default_id"], json!(second.to_string()));
    assert_eq!(
        list["active_id"],
        json!(second.to_string()),
        "a token caller has no preference and resolves to the default"
    );

    let (status, active) = h.get("/api/model-presets/active", &token()).await;
    assert_eq!(status, StatusCode::OK, "{active}");
    assert_eq!(active["id"], json!(second.to_string()));
    assert_eq!(active["is_default"], json!(true));

    let (_, one) = h
        .get(&format!("/api/model-presets/{first}"), &token())
        .await;
    assert_eq!(one["is_default"], json!(false));
}

#[tokio::test]
async fn a_signed_in_account_activates_its_own_preset() {
    let h = harness();
    let owner = h.setup().await;
    let default = h.seed("Default", true).await;
    let own = h.seed("Own", false).await;
    let member = h.invite_member(&owner, "member@example.com").await;

    let (status, activated) = h
        .post(
            &format!("/api/model-presets/{own}/activate"),
            json!({}),
            &csrf(&member),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{activated}");
    assert_eq!(activated["active_id"], json!(own.to_string()));
    assert_eq!(activated["persisted"], json!(true));

    // The member sees their own choice; the owner and a token caller see the default.
    let (_, mine) = h.get("/api/model-presets", &auth_headers(&member)).await;
    assert_eq!(mine["active_id"], json!(own.to_string()));
    let (_, admins) = h.get("/api/model-presets", &auth_headers(&owner)).await;
    assert_eq!(admins["active_id"], json!(default.to_string()));
    let (_, token_view) = h.get("/api/model-presets", &token()).await;
    assert_eq!(token_view["active_id"], json!(default.to_string()));

    // Resolved preset for the member is their choice, not the default.
    let (status, resolved) = h
        .get("/api/model-presets/active", &auth_headers(&member))
        .await;
    assert_eq!(status, StatusCode::OK, "{resolved}");
    assert_eq!(resolved["id"], json!(own.to_string()));

    // Activating without a session persists nothing and reports the default.
    let (status, noop) = h
        .post(
            &format!("/api/model-presets/{own}/activate"),
            json!({}),
            &token(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{noop}");
    assert_eq!(noop["active_id"], json!(default.to_string()));
    assert_eq!(noop["persisted"], json!(false));
}

#[tokio::test]
async fn activation_needs_csrf_and_an_existing_preset() {
    let h = harness();
    let owner = h.setup().await;
    let id = h.seed("Board", false).await;

    let (status, body) = h
        .post(
            &format!("/api/model-presets/{id}/activate"),
            json!({}),
            &auth_headers(&owner),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["error"]["code"], json!("csrf_required"));

    let (status, body) = h
        .post(
            &format!("/api/model-presets/{}/activate", uuid::Uuid::new_v4()),
            json!({}),
            &csrf(&owner),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["error"]["code"], json!("preset_not_found"));
}

#[tokio::test]
async fn duplicating_copies_the_layout_under_a_new_name() {
    let h = harness();
    let owner = h.setup().await;
    let (_, created) = h
        .post(
            "/api/model-presets",
            json!({"name": "Source", "layout": layout()}),
            &csrf(&owner),
        )
        .await;
    let source = created["id"].as_str().expect("id").to_string();

    let (status, copy) = h
        .post(
            &format!("/api/model-presets/{source}/duplicate"),
            json!({"name": "Copy"}),
            &csrf(&owner),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{copy}");
    assert_ne!(copy["id"], json!(source));
    assert_eq!(copy["name"], json!("Copy"));
    assert_eq!(copy["layout"], layout(), "the copy keeps the board");
    assert_eq!(copy["revision"], json!(1), "a copy starts at revision 1");
}

#[tokio::test]
async fn deleting_the_default_is_refused() {
    let h = harness();
    let owner = h.setup().await;

    // The migration's own preset is the default on a fresh instance.
    let (status, body) = h
        .delete(
            &format!("/api/model-presets/{DEFAULT_MODEL_PRESET_ID}"),
            &csrf(&owner),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], json!("preset_is_default"));

    // Promotion moves the flag: a newly promoted preset is refused, the old one is not.
    let promoted = h.seed("Promoted", true).await;
    let (status, body) = h
        .delete(&format!("/api/model-presets/{promoted}"), &csrf(&owner))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], json!("preset_is_default"));

    // Still there, still the default.
    let (status, one) = h
        .get(
            &format!("/api/model-presets/{promoted}"),
            &auth_headers(&owner),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(one["is_default"], json!(true));
}

#[tokio::test]
async fn a_deleted_preset_is_gone_and_audited() {
    let h = harness();
    let owner = h.setup().await;
    let preset = h.seed("Doomed", false).await;

    let (status, body) = h
        .delete(&format!("/api/model-presets/{preset}"), &csrf(&owner))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, _) = h
        .get(
            &format!("/api/model-presets/{preset}"),
            &auth_headers(&owner),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (_, audit) = h
        .get("/api/audit?kind=preset_deleted&limit=10", &token())
        .await;
    assert_eq!(audit["data"].as_array().expect("data").len(), 1, "{audit}");
}

#[tokio::test]
async fn a_stale_layout_revision_conflicts_and_writes_nothing() {
    let h = harness();
    let owner = h.setup().await;
    let (_, created) = h
        .post(
            "/api/model-presets",
            json!({"name": "Board", "layout": layout()}),
            &csrf(&owner),
        )
        .await;
    let id = created["id"].as_str().expect("id").to_string();

    // Revision 1 is current; a client that edited revision 0 is behind.
    let (status, conflict) = h
        .put(
            &format!("/api/model-presets/{id}/layout"),
            json!({"revision": 0, "layout": {"schema_version": LAYOUT_SCHEMA_VERSION}}),
            &csrf(&owner),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{conflict}");
    assert_eq!(conflict["error"]["code"], json!("preset_conflict"));
    assert_eq!(conflict["error"]["current_revision"], json!(1));

    let (_, unchanged) = h
        .get(&format!("/api/model-presets/{id}"), &auth_headers(&owner))
        .await;
    assert_eq!(
        unchanged["layout"],
        layout(),
        "the refused write changed nothing"
    );
    assert_eq!(unchanged["revision"], json!(1));

    // The current revision is accepted, bumps the revision, and is audited.
    let (status, saved) = h
        .put(
            &format!("/api/model-presets/{id}/layout"),
            json!({"revision": 1, "layout": {"schema_version": LAYOUT_SCHEMA_VERSION}}),
            &csrf(&owner),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["revision"], json!(2));
    assert_eq!(saved["layout"]["items"], json!([]));

    let (_, audit) = h
        .get("/api/audit?kind=preset_layout_saved&limit=10", &token())
        .await;
    assert_eq!(audit["data"].as_array().expect("data").len(), 1, "{audit}");
}

#[tokio::test]
async fn a_malformed_layout_is_a_400() {
    let h = harness();
    let owner = h.setup().await;

    for bad in [
        json!({"schema_version": LAYOUT_SCHEMA_VERSION, "items": "not a list"}),
        json!({"items": []}),
        json!({"schema_version": 999}),
    ] {
        let (status, body) = h
            .post(
                "/api/model-presets",
                json!({"name": "Broken", "layout": bad}),
                &csrf(&owner),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}: {body}");
        assert!(
            body["error"]["code"] == json!("layout_invalid")
                || body["error"]["code"] == json!("invalid_request"),
            "{bad}: {body}"
        );
    }

    // A missing name is the client's error too.
    let (status, body) = h
        .post(
            "/api/model-presets",
            json!({"layout": layout()}),
            &csrf(&owner),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

#[tokio::test]
async fn writes_are_admin_only() {
    let h = harness();
    let owner = h.setup().await;
    let preset = h.seed("Board", false).await;
    let member = h.invite_member(&owner, "member@example.com").await;

    for (method, uri, body) in [
        (
            Method::POST,
            "/api/model-presets".to_string(),
            Some(json!({"name": "x"})),
        ),
        (
            Method::PATCH,
            format!("/api/model-presets/{preset}"),
            Some(json!({"name": "y"})),
        ),
        (
            Method::PUT,
            format!("/api/model-presets/{preset}/layout"),
            Some(json!({"revision": 1, "layout": {"schema_version": LAYOUT_SCHEMA_VERSION}})),
        ),
        (
            Method::POST,
            format!("/api/model-presets/{preset}/duplicate"),
            Some(json!({"name": "z"})),
        ),
        (
            Method::POST,
            format!("/api/model-presets/{preset}/default"),
            Some(json!({})),
        ),
        (Method::DELETE, format!("/api/model-presets/{preset}"), None),
    ] {
        let (status, _, out) = h.call(method.clone(), &uri, body, &csrf(&member)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}: {out}");
        assert_eq!(
            out["error"]["code"],
            json!("role_required"),
            "{method} {uri}"
        );
    }

    // Reads stay open to the member.
    let (status, _) = h.get("/api/model-presets", &auth_headers(&member)).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn no_secret_material_is_echoed() {
    let h = harness();
    let owner = h.setup().await;
    let preset = h.seed("Board", true).await;

    let (_, created) = h
        .post(
            "/api/model-presets",
            json!({"name": "Second", "layout": layout()}),
            &csrf(&owner),
        )
        .await;
    let mut bodies = vec![created];
    bodies.push(h.get("/api/model-presets", &auth_headers(&owner)).await.1);
    bodies.push(
        h.get("/api/model-presets/active", &auth_headers(&owner))
            .await
            .1,
    );
    bodies.push(
        h.get(
            &format!("/api/model-presets/{preset}"),
            &auth_headers(&owner),
        )
        .await
        .1,
    );
    bodies.push(
        h.post(
            &format!("/api/model-presets/{preset}/activate"),
            json!({}),
            &csrf(&owner),
        )
        .await
        .1,
    );

    for body in bodies {
        let text = body.to_string();
        assert!(
            !text.contains(ADMIN_TOKEN),
            "a response echoed the admin token: {text}"
        );
        assert!(
            !text.contains("rrouter_session="),
            "a response echoed a session cookie: {text}"
        );
        assert!(
            !text.contains("password"),
            "a response named credential material: {text}"
        );
    }
}

#[tokio::test]
async fn an_admin_promotes_a_preset_to_default() {
    let h = harness();
    let owner = h.setup().await;
    let (_, first) = h
        .post(
            "/api/model-presets",
            json!({"name": "First", "layout": layout()}),
            &csrf(&owner),
        )
        .await;
    let first = first["id"].as_str().expect("id").to_string();
    let (_, second) = h
        .post(
            "/api/model-presets",
            json!({"name": "Second"}),
            &csrf(&owner),
        )
        .await;
    let second = second["id"].as_str().expect("id").to_string();

    let (status, promoted) = h
        .post(
            &format!("/api/model-presets/{second}/default"),
            json!({}),
            &csrf(&owner),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{promoted}");
    assert_eq!(
        promoted["id"],
        json!(second),
        "the updated preset comes back"
    );
    assert_eq!(promoted["is_default"], json!(true));
    assert_eq!(
        promoted["revision"],
        json!(1),
        "the flag is not a layout edit"
    );

    // Exactly one default, and the index agrees.
    let (_, list) = h.get("/api/model-presets", &auth_headers(&owner)).await;
    assert_eq!(list["default_id"], json!(second));
    let defaults: Vec<&Value> = list["data"]
        .as_array()
        .expect("data")
        .iter()
        .filter(|row| row["is_default"] == json!(true))
        .collect();
    assert_eq!(defaults.len(), 1, "one default only: {list}");
    assert_eq!(defaults[0]["id"], json!(second));

    // The migration's preset lost the flag; it is deletable again.
    let (_, previous) = h
        .get(
            &format!("/api/model-presets/{DEFAULT_MODEL_PRESET_ID}"),
            &auth_headers(&owner),
        )
        .await;
    assert_eq!(previous["is_default"], json!(false));
    let (status, body) = h
        .delete(
            &format!("/api/model-presets/{DEFAULT_MODEL_PRESET_ID}"),
            &csrf(&owner),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // A caller with no preference resolves to the promoted default; the other preset is
    // still a listed, non-default row.
    let (_, active) = h.get("/api/model-presets/active", &token()).await;
    assert_eq!(active["id"], json!(second));
    let (status, other) = h
        .get(
            &format!("/api/model-presets/{first}"),
            &auth_headers(&owner),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(other["is_default"], json!(false));

    let (_, audit) = h
        .get("/api/audit?kind=preset_default_set&limit=10", &token())
        .await;
    assert_eq!(audit["data"].as_array().expect("data").len(), 1, "{audit}");
}

#[tokio::test]
async fn promoting_an_unknown_preset_is_a_404() {
    let h = harness();
    let owner = h.setup().await;

    let (status, body) = h
        .post(
            &format!("/api/model-presets/{}/default", uuid::Uuid::new_v4()),
            json!({}),
            &csrf(&owner),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["error"]["code"], json!("preset_not_found"));

    let (status, body) = h
        .post(
            "/api/model-presets/not-a-uuid/default",
            json!({}),
            &csrf(&owner),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["error"]["code"], json!("preset_not_found"));
}

#[tokio::test]
async fn default_and_activate_are_separate_surfaces() {
    let h = harness();
    let owner = h.setup().await;
    let (_, library) = h
        .post(
            "/api/model-presets",
            json!({"name": "Library"}),
            &csrf(&owner),
        )
        .await;
    let library = library["id"].as_str().expect("id").to_string();
    let (_, personal) = h
        .post(
            "/api/model-presets",
            json!({"name": "Personal"}),
            &csrf(&owner),
        )
        .await;
    let personal = personal["id"].as_str().expect("id").to_string();

    // Admin promotes one preset; the member activates another for themselves.
    let (status, _) = h
        .post(
            &format!("/api/model-presets/{library}/default"),
            json!({}),
            &csrf(&owner),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let member = h.invite_member(&owner, "member@example.com").await;
    let (status, activated) = h
        .post(
            &format!("/api/model-presets/{personal}/activate"),
            json!({}),
            &csrf(&member),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{activated}");
    assert_eq!(activated["active_id"], json!(personal));

    // The member keeps their choice; a token caller resolves to the admin default.
    let (_, mine) = h
        .get("/api/model-presets/active", &auth_headers(&member))
        .await;
    assert_eq!(mine["id"], json!(personal));
    let (_, admins) = h.get("/api/model-presets/active", &token()).await;
    assert_eq!(admins["id"], json!(library));
    let (_, owner_view) = h
        .get("/api/model-presets/active", &auth_headers(&owner))
        .await;
    assert_eq!(
        owner_view["id"],
        json!(library),
        "the owner has no preference and resolves to the default"
    );

    // The static `/active` read is not shadowed by the `{id}` routes.
    let (status, _) = h
        .get("/api/model-presets/active", &auth_headers(&member))
        .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn list_rows_carry_entry_and_visible_counts() {
    let h = harness();
    let owner = h.setup().await;
    let (status, created) = h
        .post(
            "/api/model-presets",
            json!({"name": "Counted", "layout": layout()}),
            &csrf(&owner),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let id = created["id"].as_str().expect("id");

    let (_, list) = h.get("/api/model-presets", &auth_headers(&owner)).await;
    let row = list["data"]
        .as_array()
        .expect("data")
        .iter()
        .find(|row| row["id"] == json!(id))
        .expect("row");
    // `layout()` names two items, one of them hidden.
    assert_eq!(row["entry_count"], json!(2), "{row}");
    assert_eq!(row["visible_count"], json!(1), "{row}");
    assert!(
        row.get("layout").is_none(),
        "the layout stays off the list row"
    );

    // The seeded default has no explicit items: counts are zero, not missing.
    let seeded = list["data"]
        .as_array()
        .expect("data")
        .iter()
        .find(|row| row["id"] == json!(DEFAULT_MODEL_PRESET_ID.to_string()))
        .expect("seeded row");
    assert_eq!(seeded["entry_count"], json!(0), "{seeded}");
    assert_eq!(seeded["visible_count"], json!(0), "{seeded}");
}

#[tokio::test]
async fn a_layout_with_future_item_fields_is_accepted() {
    let h = harness();
    let owner = h.setup().await;
    let (_, created) = h
        .post(
            "/api/model-presets",
            json!({"name": "Future", "layout": layout()}),
            &csrf(&owner),
        )
        .await;
    let id = created["id"].as_str().expect("id").to_string();

    // Fields the store type may gain later (hide/restore bookkeeping). The route must not
    // reject them: the document is parsed into the store's own type, so an unknown key is
    // ignored today and round-trips once the type carries it.
    let mut future = layout();
    future["items"][0]["last_visible_position"] = json!(3);
    future["items"][1]["hidden_at"] = json!(1_760_000_000);
    let (status, saved) = h
        .put(
            &format!("/api/model-presets/{id}/layout"),
            json!({"revision": 1, "layout": future}),
            &csrf(&owner),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["revision"], json!(2));

    let (status, one) = h
        .get(&format!("/api/model-presets/{id}"), &auth_headers(&owner))
        .await;
    assert_eq!(status, StatusCode::OK, "{one}");
    // The known fields survive the round trip; the future ones appear once the store
    // type gains them (Agent A's change), which this route needs no change for.
    assert_eq!(one["layout"]["items"][0]["provider"], json!("openai"));
    assert_eq!(one["layout"]["items"][1]["visible"], json!(false));
}
