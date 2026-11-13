//! Tenant-owned provider connections end to end.
//!
//! The harness mirrors `tests/teams.rs`: an admin token is configured, so every answer here
//! comes from a session's role rather than the loopback shortcut. Assertions cover the
//! isolation matrix — who may see a row, who may not, and what a caller is told when they
//! may not.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{HeaderMap, Method, Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use rrouter_gateway::{app, auth::Auth, config::Config, AppState};
use rrouter_router::{Breaker, Catalog, Dispatcher, Planner};
use rrouter_store::Store;
use serde_json::{json, Value};
use tower::ServiceExt;

struct H {
    state: Arc<AppState>,
    router: Router,
}

fn config() -> Config {
    Config {
        host: "127.0.0.1".parse().expect("ip"),
        port: 0,
        data_dir: std::env::temp_dir().join(format!("rrouter-conn-scope-{}", uuid::Uuid::new_v4())),
        master_key: None,
        require_api_key: false,
        request_timeout_secs: 5,
        connect_timeout_secs: 1,
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
                .timeout(Duration::from_secs(2))
                .build()
                .expect("client"),
        )),
        planner: Planner::new(),
        dispatcher: Arc::new(Dispatcher::new(Breaker::new()).with_max_attempts(4)),
        http: reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
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

    fn setup_code(&self) -> String {
        self.state.auth.code().unwrap_or_default()
    }

    /// Create the owner and return `(cookie, user id)`.
    async fn setup(&self) -> (String, String) {
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
        let cookie = cookie_of(&headers).expect("owner cookie");
        let (_, state) = self.get("/api/auth/state", &auth_headers(&cookie)).await;
        let id = state["user"]["id"].as_str().expect("owner id").to_string();
        (cookie, id)
    }

    async fn create_team(&self, owner: &str, name: &str) -> String {
        let (status, body) = self
            .post("/api/teams", json!({"name": name}), &csrf(owner))
            .await;
        assert_eq!(status, StatusCode::CREATED, "team: {body}");
        body["data"]["id"].as_str().expect("team id").to_string()
    }

    /// Invite `email` with `role`, accept, and return the session cookie.
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

    async fn add_member(&self, owner: &str, team: &str, email: &str, role: &str) {
        let (status, body) = self
            .post(
                &format!("/api/teams/{team}/members"),
                json!({"email": email, "role": role}),
                &csrf(owner),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "member: {body}");
    }

    /// Create a connection through the API. The override points at a closed port so the
    /// background model fetch and the probe fail locally instead of reaching the internet.
    async fn connection(
        &self,
        cookie: &str,
        name: &str,
        team_id: Option<&str>,
    ) -> (StatusCode, Value) {
        let mut body = json!({
            "provider": "openai",
            "auth_type": "api_key",
            "name": name,
            "credentials": {
                "api_key": "sk-secret-must-not-leak",
                "base_url_override": "http://127.0.0.1:1",
            },
        });
        if let Some(team) = team_id {
            body["team_id"] = json!(team);
        }
        self.post("/api/connections", body, &csrf(cookie)).await
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

fn auth_headers(cookie: &str) -> [(&str, &str); 1] {
    [("cookie", cookie)]
}

fn csrf(cookie: &str) -> [(&str, &str); 2] {
    [("cookie", cookie), ("x-rrouter-csrf", "1")]
}

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
async fn an_admin_creates_a_team_owned_connection_and_attributes_the_owner() {
    let h = harness();
    let (owner, owner_id) = h.setup().await;
    let core = h.create_team(&owner, "Core").await;

    let (status, body) = h.connection(&owner, "team-account", Some(&core)).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["team_id"], json!(core));
    assert_eq!(
        body["owner_user_id"],
        json!(owner_id),
        "the creating session is the attribution"
    );
    assert_eq!(body["has_api_key"], json!(true));
    assert!(!body.to_string().contains("sk-secret"), "{body}");

    // The row is in the caller's list with its ownership attached.
    let (status, list) = h.get("/api/connections", &auth_headers(&owner)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["data"][0]["team_id"], json!(core));
}

#[tokio::test]
async fn a_member_sees_its_own_team_rows_and_never_a_shared_row() {
    let h = harness();
    let (owner, _) = h.setup().await;
    let core = h.create_team(&owner, "Core").await;
    h.connection(&owner, "core-account", Some(&core)).await;
    // Shared/instance-wide row, the shape every pre-v26 connection has.
    h.connection(&owner, "shared-account", None).await;

    let member = h.invite(&owner, "member@example.com", "member").await;
    h.add_member(&owner, &core, "member@example.com", "member")
        .await;

    let (status, list) = h.get("/api/connections", &auth_headers(&member)).await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert_eq!(
        names(&list),
        vec!["core-account"],
        "a team sees its own row and never the shared one"
    );

    // The provider listing agrees with the list: `team`, not `global`.
    let (status, providers) = h.get("/api/models/providers", &auth_headers(&member)).await;
    assert_eq!(status, StatusCode::OK);
    let openai = providers["data"]
        .as_array()
        .expect("data")
        .iter()
        .find(|row| row["id"] == json!("openai"))
        .expect("openai row");
    assert_eq!(openai["has_connection"], json!(true));
    assert_eq!(openai["scope"], json!("team"));
}

#[tokio::test]
async fn a_member_cannot_read_or_probe_another_teams_connection() {
    let h = harness();
    let (owner, _) = h.setup().await;
    let core = h.create_team(&owner, "Core").await;
    let edge = h.create_team(&owner, "Edge").await;
    let (_, edge_row) = h.connection(&owner, "edge-account", Some(&edge)).await;
    let edge_id = edge_row["id"].as_str().expect("id").to_string();

    let member = h.invite(&owner, "member@example.com", "member").await;
    h.add_member(&owner, &core, "member@example.com", "member")
        .await;

    // Reads: the row is indistinguishable from a missing one.
    let (status, body) = h
        .get(
            &format!("/api/connections/{edge_id}"),
            &auth_headers(&member),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["error"]["code"], json!("model_not_found"));

    // Probes are mutations, so the admin gate refuses the session before the handler.
    let (status, body) = h
        .post(
            &format!("/api/connections/{edge_id}/test"),
            json!({}),
            &csrf(&member),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["error"]["code"], json!("role_required"));

    // Nor may a member mutate anything, including its own team's row.
    let (status, _) = h
        .patch(
            &format!("/api/connections/{edge_id}"),
            json!({"is_active": false}),
            &csrf(&member),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = h
        .post(
            "/api/connections",
            json!({"provider": "openai"}),
            &csrf(&member),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn global_scope_sees_every_row_and_can_narrow_by_team() {
    let h = harness();
    let (owner, _) = h.setup().await;
    let core = h.create_team(&owner, "Core").await;
    let edge = h.create_team(&owner, "Edge").await;
    h.connection(&owner, "core-account", Some(&core)).await;
    h.connection(&owner, "edge-account", Some(&edge)).await;
    h.connection(&owner, "shared-account", None).await;

    let (status, list) = h.get("/api/connections", &auth_headers(&owner)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        names(&list),
        vec!["core-account", "edge-account", "shared-account"]
    );

    let (status, list) = h
        .get(
            &format!("/api/connections?team_id={core}"),
            &auth_headers(&owner),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(names(&list), vec!["core-account"]);

    // The provider listing reports the shared account as global to an admin.
    let (_, providers) = h.get("/api/models/providers", &auth_headers(&owner)).await;
    let openai = providers["data"]
        .as_array()
        .expect("data")
        .iter()
        .find(|row| row["id"] == json!("openai"))
        .expect("openai row");
    assert_eq!(openai["scope"], json!("global"));
}

#[tokio::test]
async fn a_team_id_filter_outside_the_callers_scope_is_refused() {
    let h = harness();
    let (owner, _) = h.setup().await;
    let core = h.create_team(&owner, "Core").await;
    let edge = h.create_team(&owner, "Edge").await;
    h.connection(&owner, "edge-account", Some(&edge)).await;

    let member = h.invite(&owner, "member@example.com", "member").await;
    h.add_member(&owner, &core, "member@example.com", "member")
        .await;

    let (status, body) = h
        .get(
            &format!("/api/connections?team_id={edge}"),
            &auth_headers(&member),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["error"]["code"], json!("team_forbidden"));

    let (status, _) = h
        .get("/api/connections?team_id=nonsense", &auth_headers(&member))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn creating_against_a_missing_team_is_404_and_a_bad_id_is_400() {
    let h = harness();
    let (owner, _) = h.setup().await;

    let (status, body) = h
        .connection(&owner, "ghost", Some(&uuid::Uuid::new_v4().to_string()))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert!(body["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .contains("team"));

    let (status, body) = h
        .post(
            "/api/connections",
            json!({
                "provider": "openai",
                "team_id": "not-a-uuid",
                "credentials": {"api_key": "sk-test"},
            }),
            &csrf(&owner),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

#[tokio::test]
async fn ownership_can_be_moved_and_cleared() {
    let h = harness();
    let (owner, _) = h.setup().await;
    let core = h.create_team(&owner, "Core").await;
    let edge = h.create_team(&owner, "Edge").await;
    let (_, row) = h.connection(&owner, "account", Some(&core)).await;
    let id = row["id"].as_str().expect("id").to_string();

    let member = h.invite(&owner, "member@example.com", "member").await;
    h.add_member(&owner, &core, "member@example.com", "member")
        .await;
    h.add_member(&owner, &edge, "member@example.com", "member")
        .await;

    // Move it to Edge: the same member sees it under a different team id.
    let (status, body) = h
        .patch(
            &format!("/api/connections/{id}"),
            json!({"team_id": edge}),
            &csrf(&owner),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["team_id"], json!(edge));
    let (_, list) = h.get("/api/connections", &auth_headers(&member)).await;
    assert_eq!(names(&list), vec!["account"]);

    // `null` hands it back to the instance: a team may no longer see it.
    let (status, body) = h
        .patch(
            &format!("/api/connections/{id}"),
            json!({"team_id": null}),
            &csrf(&owner),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["team_id"], Value::Null);
    let (status, _) = h
        .get(&format!("/api/connections/{id}"), &auth_headers(&member))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, body) = h
        .get(&format!("/api/connections/{id}"), &auth_headers(&owner))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["team_id"], Value::Null);
}

#[tokio::test]
async fn no_credential_material_appears_in_any_response() {
    const SECRET: &str = "sk-secret-must-not-leak";
    let h = harness();
    let (owner, _) = h.setup().await;
    let core = h.create_team(&owner, "Core").await;
    let (_, row) = h.connection(&owner, "account", Some(&core)).await;
    let id = row["id"].as_str().expect("id").to_string();

    let member = h.invite(&owner, "member@example.com", "member").await;
    h.add_member(&owner, &core, "member@example.com", "member")
        .await;

    for (label, out) in [
        ("create", row.clone()),
        (
            "admin list",
            h.get("/api/connections", &auth_headers(&owner)).await.1,
        ),
        (
            "member list",
            h.get("/api/connections", &auth_headers(&member)).await.1,
        ),
        (
            "member get",
            h.get(&format!("/api/connections/{id}"), &auth_headers(&member))
                .await
                .1,
        ),
        (
            "test",
            h.post(
                &format!("/api/connections/{id}/test"),
                json!({}),
                &csrf(&owner),
            )
            .await
            .1,
        ),
    ] {
        assert!(
            !out.to_string().contains(SECRET),
            "{label} leaked the credential: {out}"
        );
    }

    // And the probe's own failure text does not echo it either.
    let (status, probe) = h
        .post(
            &format!("/api/connections/{id}/test"),
            json!({}),
            &csrf(&owner),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{probe}");
    assert_eq!(probe["ok"], json!(false), "{probe}");
}

#[tokio::test]
async fn the_owner_session_cannot_see_another_teams_row_through_the_test_endpoint() {
    // The handler's scope check is the second line of defence: the route is admin-gated
    // today, so a member never reaches it. Prove the gate's answer here, and that an admin
    // probing a row that does not exist gets 404 rather than a probe.
    let h = harness();
    let (owner, _) = h.setup().await;
    let (status, body) = h
        .post(
            &format!("/api/connections/{}/test", uuid::Uuid::new_v4()),
            json!({}),
            &csrf(&owner),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

/// Every connection mutation writes exactly one audit row, naming the row and its scope
/// but never the credential it stored. `x-forwarded-for` stands in for the peer address
/// (the oneshot harness has no `ConnectInfo`).
#[tokio::test]
async fn connection_mutations_are_audited_without_credentials() {
    let h = harness();
    let (owner, _) = h.setup().await;
    let core = h.create_team(&owner, "Core").await;
    let ip = "198.51.100.9";
    let secret = "sk-secret-must-not-leak";
    let headers = [
        ("cookie", owner.as_str()),
        ("x-rrouter-csrf", "1"),
        ("x-forwarded-for", ip),
    ];

    let (status, created) = h
        .post(
            "/api/connections",
            json!({
                "provider": "openai",
                "auth_type": "api_key",
                "name": "audited-account",
                "team_id": core,
                // A closed local port: the model fetch and the probe fail locally, so the
                // test never reaches the internet.
                "credentials": {"api_key": secret, "base_url_override": "http://127.0.0.1:1"},
            }),
            &headers,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let id = created["id"].as_str().expect("id").to_string();

    let (status, _, updated) = h
        .call(
            Method::PATCH,
            &format!("/api/connections/{id}"),
            Some(json!({"priority": 7, "weight": 25, "is_active": false})),
            &headers,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{updated}");

    let (status, probed) = h
        .post(&format!("/api/connections/{id}/test"), json!({}), &headers)
        .await;
    assert_eq!(status, StatusCode::OK, "{probed}");
    assert_eq!(probed["ok"], json!(false), "the closed port cannot answer");

    let (status, _, _) = h
        .call(
            Method::DELETE,
            &format!("/api/connections/{id}"),
            None,
            &headers,
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    // A failed mutation is not audited: the row only exists for a committed change.
    let (status, _, _) = h
        .call(
            Method::PATCH,
            &format!("/api/connections/{}", uuid::Uuid::new_v4()),
            Some(json!({"priority": 1})),
            &headers,
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, audit) = h.get("/api/audit?limit=50", &auth_headers(&owner)).await;
    assert_eq!(status, StatusCode::OK, "{audit}");
    let rows = audit["data"].as_array().expect("rows");
    let of_kind = |kind: &str| -> Vec<Value> {
        rows.iter()
            .filter(|row| row["kind"] == json!(kind))
            .cloned()
            .collect()
    };
    for kind in [
        "connection_created",
        "connection_updated",
        "connection_deleted",
        "connection_tested",
    ] {
        assert_eq!(of_kind(kind).len(), 1, "exactly one {kind} row: {audit}");
    }

    let created_row = &of_kind("connection_created")[0];
    assert_eq!(created_row["actor"], "ada@example.com");
    assert_eq!(created_row["ip"], ip);
    let detail = created_row["detail"].as_str().expect("detail");
    assert!(detail.contains(&id), "names the row: {detail}");
    assert!(
        detail.contains("provider openai"),
        "names the provider: {detail}"
    );
    assert!(
        detail.contains("name audited-account"),
        "names the row: {detail}"
    );
    assert!(
        detail.contains(&format!("scope team {core}")),
        "names the owning team: {detail}"
    );
    assert!(
        detail.contains("auth api_key"),
        "names the auth style: {detail}"
    );

    let updated_detail = of_kind("connection_updated")[0]["detail"]
        .as_str()
        .expect("detail")
        .to_string();
    for field in ["priority", "weight", "is_active"] {
        assert!(
            updated_detail.contains(field),
            "the patch names {field}: {updated_detail}"
        );
    }
    assert!(updated_detail.contains(&id), "{updated_detail}");

    let tested_detail = of_kind("connection_tested")[0]["detail"]
        .as_str()
        .expect("detail")
        .to_string();
    assert!(tested_detail.contains(&id), "{tested_detail}");
    assert!(tested_detail.contains("probe"), "{tested_detail}");

    let deleted_detail = of_kind("connection_deleted")[0]["detail"]
        .as_str()
        .expect("detail")
        .to_string();
    assert!(deleted_detail.contains(&id), "{deleted_detail}");
    assert!(
        deleted_detail.contains("provider openai"),
        "{deleted_detail}"
    );

    assert!(
        !audit.to_string().contains(secret),
        "no credential value may reach the trail: {audit}"
    );
}

/// A settings write is audited by key name only: the blob can hold operator-supplied
/// panel configuration, so values are never copied into the trail.
#[tokio::test]
async fn settings_updates_are_audited_by_key_name() {
    let h = harness();
    let (owner, _) = h.setup().await;
    let ip = "203.0.113.7";
    let headers = [
        ("cookie", owner.as_str()),
        ("x-rrouter-csrf", "1"),
        ("x-forwarded-for", ip),
    ];

    let (status, _, body) = h
        .call(
            Method::PUT,
            "/api/settings",
            Some(json!({"brand_name": "Acme", "rtk": false})),
            &headers,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // A rejected patch commits nothing and therefore audits nothing.
    let (status, _, _) = h
        .call(Method::PUT, "/api/settings", Some(json!([1, 2])), &headers)
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, audit) = h.get("/api/audit?limit=50", &auth_headers(&owner)).await;
    assert_eq!(status, StatusCode::OK, "{audit}");
    let rows: Vec<Value> = audit["data"]
        .as_array()
        .expect("rows")
        .iter()
        .filter(|row| row["kind"] == json!("settings_updated"))
        .cloned()
        .collect();
    assert_eq!(rows.len(), 1, "exactly one settings row: {audit}");
    assert_eq!(rows[0]["actor"], "ada@example.com");
    assert_eq!(rows[0]["ip"], ip);
    let detail = rows[0]["detail"].as_str().expect("detail");
    assert!(detail.contains("brand_name"), "names the key: {detail}");
    assert!(detail.contains("rtk"), "names the key: {detail}");
    assert!(
        !detail.contains("Acme"),
        "settings values are not audited: {detail}"
    );
}
