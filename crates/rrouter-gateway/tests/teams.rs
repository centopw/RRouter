//! Team control plane end to end: CRUD, membership scoping, key assignment and the usage
//! filter.
//!
//! The harness mirrors `tests/auth.rs`: an admin token is configured, so every answer here
//! comes from a session's role rather than the loopback shortcut.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{HeaderMap, Method, Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use rrouter_gateway::{app, auth::Auth, config::Config, AppState};
use rrouter_router::{Breaker, Catalog, Dispatcher, Planner};
use rrouter_store::{Store, UsageRecord};
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
        data_dir: std::env::temp_dir().join(format!("rrouter-teams-{}", uuid::Uuid::new_v4())),
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

    async fn post(&self, uri: &str, body: Value, headers: &[(&str, &str)]) -> (StatusCode, Value) {
        let (status, _, body) = self.call(Method::POST, uri, Some(body), headers).await;
        (status, body)
    }

    async fn get(&self, uri: &str, headers: &[(&str, &str)]) -> (StatusCode, Value) {
        let (status, _, body) = self.call(Method::GET, uri, None, headers).await;
        (status, body)
    }

    async fn patch(&self, uri: &str, body: Value, headers: &[(&str, &str)]) -> (StatusCode, Value) {
        let (status, _, body) = self.call(Method::PATCH, uri, Some(body), headers).await;
        (status, body)
    }

    async fn delete(&self, uri: &str, headers: &[(&str, &str)]) -> StatusCode {
        let (status, _, _) = self.call(Method::DELETE, uri, None, headers).await;
        status
    }

    fn setup_code(&self) -> String {
        self.state.auth.code().unwrap_or_default()
    }

    /// Create the owner and return the session cookie.
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

    async fn create_team(&self, owner: &str, body: Value) -> Value {
        let (status, body) = self.post("/api/teams", body, &csrf(owner)).await;
        assert_eq!(status, StatusCode::CREATED, "team: {body}");
        body["data"].clone()
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

    async fn create_key(&self, owner: &str, body: Value) -> Value {
        let (status, body) = self.post("/api/keys", body, &csrf(owner)).await;
        assert_eq!(status, StatusCode::OK, "key: {body}");
        body
    }

    async fn record(&self, key_id: uuid::Uuid, provider: &str, model: &str, cost: u64) {
        let record = UsageRecord {
            id: None,
            ts: rrouter_gateway::now_unix(),
            request_id: Some(uuid::Uuid::new_v4()),
            trace_id: None,
            provider: provider.into(),
            model: model.into(),
            connection_id: None,
            endpoint: Some("/v1/chat/completions".into()),
            key_id: Some(key_id),
            cost_micros: cost,
            input_tokens: 10,
            output_tokens: 20,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            latency_ms: 5,
            ttft_ms: 1,
            upstream_ms: 1,
            status: 200,
            streamed: false,
            error: None,
        };
        self.state.store.record_usage(&record).await.expect("usage");
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
    body["data"]
        .as_array()
        .expect("data")
        .iter()
        .map(|row| row["name"].as_str().expect("name").to_string())
        .collect()
}

#[tokio::test]
async fn team_crud_round_trip_and_budget_validation() {
    let h = harness();
    let owner = h.setup().await;

    let core = h.create_team(&owner, json!({"name": "  Core  "})).await;
    assert_eq!(core["name"], "Core", "the name is trimmed");
    assert_eq!(core["budget_tokens"], 0, "0 means unlimited");
    assert_eq!(core["budget_micros"], 0);
    assert_eq!(core["budget_window"], "total");
    assert_eq!(core["members"], 0);
    assert_eq!(core["spend"]["requests"], 0);

    // A duplicate name is a client error naming the field, not a 500.
    let (status, body) = h
        .post("/api/teams", json!({"name": "Core"}), &csrf(&owner))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    let edge = h
        .create_team(
            &owner,
            json!({
                "name": "Edge",
                "budget_tokens": 1_000,
                "budget_micros": 2_000,
                "budget_window": "daily",
            }),
        )
        .await;
    assert_eq!(edge["budget_window"], "daily");

    let (status, list) = h.get("/api/teams", &auth_headers(&owner)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(names(&list), vec!["Core", "Edge"], "ordered by name");

    let id = edge["id"].as_str().expect("id");
    let (status, patched) = h
        .patch(
            &format!("/api/teams/{id}"),
            json!({"budget_window": "weekly", "budget_micros": 5_000}),
            &csrf(&owner),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{patched}");
    assert_eq!(patched["data"]["budget_window"], "weekly");
    assert_eq!(patched["data"]["budget_micros"], 5_000);
    assert_eq!(
        patched["data"]["name"], "Edge",
        "an absent field is left alone"
    );

    // A window spelling is a closed set.
    let (status, body) = h
        .patch(
            &format!("/api/teams/{id}"),
            json!({"budget_window": "fortnightly"}),
            &csrf(&owner),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    assert_eq!(
        h.delete(&format!("/api/teams/{id}"), &csrf(&owner)).await,
        StatusCode::OK
    );
    assert_eq!(
        h.delete(&format!("/api/teams/{id}"), &csrf(&owner)).await,
        StatusCode::NOT_FOUND,
        "deletion is not idempotent-by-accident"
    );
    let (_, list) = h.get("/api/teams", &auth_headers(&owner)).await;
    assert_eq!(names(&list), vec!["Core"]);
}

#[tokio::test]
async fn membership_scopes_reads_and_mutations_stay_admin_only() {
    let h = harness();
    let owner = h.setup().await;
    let core = h.create_team(&owner, json!({"name": "Core"})).await;
    let edge = h.create_team(&owner, json!({"name": "Edge"})).await;
    let core_id = core["id"].as_str().expect("id").to_string();
    let edge_id = edge["id"].as_str().expect("id").to_string();

    let viewer = h.invite(&owner, "viewer@example.com", "viewer").await;
    let member = h.invite(&owner, "member@example.com", "member").await;

    // Only an admin may add a member.
    let (status, body) = h
        .post(
            &format!("/api/teams/{core_id}/members"),
            json!({"email": "viewer@example.com", "role": "viewer"}),
            &csrf(&viewer),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["error"]["code"], "role_required");

    for (cookie, team, email, role) in [
        (&owner, &core_id, "viewer@example.com", "viewer"),
        (&owner, &core_id, "member@example.com", "team_admin"),
        (&owner, &edge_id, "member@example.com", "member"),
    ] {
        let (status, body) = h
            .post(
                &format!("/api/teams/{team}/members"),
                json!({"email": email, "role": role}),
                &csrf(cookie),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert_eq!(body["data"]["role"], role);
    }

    // Re-adding changes the role in place: add and edit are the same call.
    let (status, body) = h
        .post(
            &format!("/api/teams/{edge_id}/members"),
            json!({"email": "member@example.com", "role": "team_admin"}),
            &csrf(&owner),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["data"]["role"], "team_admin");
    let (_, roster) = h
        .get(
            &format!("/api/teams/{edge_id}/members"),
            &auth_headers(&owner),
        )
        .await;
    assert_eq!(
        roster["data"].as_array().expect("roster").len(),
        1,
        "an upsert does not duplicate the row: {roster}"
    );

    // A viewer sees only their own teams, and only their own team's roster.
    let (status, list) = h.get("/api/teams", &auth_headers(&viewer)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(names(&list), vec!["Core"], "not Edge");

    let (status, one) = h
        .get(&format!("/api/teams/{core_id}"), &auth_headers(&viewer))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        one["data"]["members_list"]
            .as_array()
            .expect("members")
            .len(),
        2
    );

    let (status, body) = h
        .get(&format!("/api/teams/{edge_id}"), &auth_headers(&viewer))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["error"]["code"], "team_forbidden");

    let (status, _) = h
        .get(
            &format!("/api/teams/{edge_id}/members"),
            &auth_headers(&viewer),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "another team's roster");

    // Membership can be revoked, and a revoked member falls back to "no team, no data".
    assert_eq!(
        h.delete(
            &format!(
                "/api/teams/{core_id}/members/{}",
                user_id(&h, "viewer@example.com").await
            ),
            &csrf(&owner)
        )
        .await,
        StatusCode::OK
    );
    let (status, list) = h.get("/api/teams", &auth_headers(&viewer)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(names(&list), Vec::<String>::new());

    // The member cookie exists only to prove the second invitation worked.
    let (status, _) = h.get("/api/teams", &auth_headers(&member)).await;
    assert_eq!(status, StatusCode::OK);
}

/// The dashboard's `user_id` for an email, read from the admin roster API.
async fn user_id(h: &H, email: &str) -> String {
    let users = h.state.store.users().await.expect("users");
    users
        .iter()
        .find(|user| user.email == email)
        .map(|user| user.id.to_string())
        .unwrap_or_else(|| panic!("no user {email}"))
}

#[tokio::test]
async fn key_assigned_to_a_team_appears_only_in_that_team() {
    let h = harness();
    let owner = h.setup().await;
    let core = h.create_team(&owner, json!({"name": "Core"})).await;
    let core_id = core["id"].as_str().expect("id").to_string();
    let edge = h.create_team(&owner, json!({"name": "Edge"})).await;
    let edge_id = edge["id"].as_str().expect("id").to_string();

    h.create_key(&owner, json!({"name": "a", "team_id": core_id}))
        .await;
    let b = h.create_key(&owner, json!({"name": "b"})).await;
    let b_id = b["id"].as_str().expect("id");

    // The create response is the plaintext reveal, not a row: read the assignment back.
    let (_, all) = h.get("/api/keys", &auth_headers(&owner)).await;
    let row = |body: &Value, name: &str| {
        body["data"]
            .as_array()
            .expect("data")
            .iter()
            .find(|row| row["name"] == name)
            .cloned()
            .unwrap_or_else(|| panic!("no key {name} in {body}"))
    };
    assert_eq!(row(&all, "a")["team_id"], core_id, "assigned at insert");
    assert_eq!(
        row(&all, "b")["team_id"],
        Value::Null,
        "instance-wide by default"
    );
    let (status, moved) = h
        .patch(
            &format!("/api/keys/{b_id}"),
            json!({"team_id": edge_id}),
            &csrf(&owner),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{moved}");
    assert_eq!(moved["team_id"], edge_id);
    assert_eq!(moved["rpm"], 0, "moving a key does not rewrite its limits");

    let count = |body: &Value| body["data"].as_array().expect("data").len();
    let (_, filtered) = h
        .get(
            &format!("/api/keys?team_id={core_id}"),
            &auth_headers(&owner),
        )
        .await;
    assert_eq!(count(&filtered), 1, "{filtered}");
    assert_eq!(filtered["data"][0]["name"], "a");

    let (_, filtered) = h
        .get(
            &format!("/api/keys?team_id={edge_id}"),
            &auth_headers(&owner),
        )
        .await;
    assert_eq!(count(&filtered), 1);
    assert_eq!(filtered["data"][0]["name"], "b");

    let (_, all) = h.get("/api/keys", &auth_headers(&owner)).await;
    assert_eq!(count(&all), 2, "no filter is every key");

    // Clearing the assignment puts the key back to instance-wide.
    let (status, cleared) = h
        .patch(
            &format!("/api/keys/{b_id}"),
            json!({"team_id": null}),
            &csrf(&owner),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(cleared["team_id"], Value::Null);
    let (_, filtered) = h
        .get(
            &format!("/api/keys?team_id={edge_id}"),
            &auth_headers(&owner),
        )
        .await;
    assert_eq!(count(&filtered), 0);

    // Bad input and unknown teams are refused instead of storing a dangling id.
    let (status, body) = h
        .post(
            "/api/keys",
            json!({"name": "c", "team_id": "not-a-uuid"}),
            &csrf(&owner),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, body) = h
        .post(
            "/api/keys",
            json!({"name": "c", "team_id": uuid::Uuid::new_v4().to_string()}),
            &csrf(&owner),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

    // Deleting a team releases its keys rather than deleting them.
    assert_eq!(
        h.delete(&format!("/api/teams/{core_id}"), &csrf(&owner))
            .await,
        StatusCode::OK
    );
    let (_, released) = h
        .get(
            &format!("/api/keys?team_id={core_id}"),
            &auth_headers(&owner),
        )
        .await;
    assert_eq!(count(&released), 0);
    let (_, all) = h.get("/api/keys", &auth_headers(&owner)).await;
    assert_eq!(count(&all), 2, "the key survived its team: {all}");
    let survivor = all["data"]
        .as_array()
        .expect("data")
        .iter()
        .find(|row| row["name"] == "a")
        .expect("key a");
    assert_eq!(survivor["team_id"], Value::Null);
}

#[tokio::test]
async fn usage_filter_and_reader_scope_follow_membership() {
    let h = harness();
    let owner = h.setup().await;
    let core = h.create_team(&owner, json!({"name": "Core"})).await;
    let edge = h.create_team(&owner, json!({"name": "Edge"})).await;
    let core_id = core["id"].as_str().expect("id").to_string();
    let edge_id = edge["id"].as_str().expect("id").to_string();

    let a = h
        .create_key(&owner, json!({"name": "a", "team_id": core_id}))
        .await;
    let b = h
        .create_key(&owner, json!({"name": "b", "team_id": edge_id}))
        .await;
    let zero = h.create_key(&owner, json!({"name": "zero"})).await;
    let uuid = |row: &Value| uuid::Uuid::parse_str(row["id"].as_str().expect("id")).expect("uuid");

    // Usage is attributed to the key's team at write time; the instance-wide key stays out
    // of every team report.
    h.record(uuid(&a), "openai", "gpt-4.1", 100).await;
    h.record(uuid(&b), "anthropic", "claude-haiku-4-5", 200)
        .await;
    h.record(uuid(&zero), "openai", "gpt-4.1-mini", 300).await;

    let admin = auth_headers(&owner);
    let (status, list) = h
        .get(&format!("/api/usage?team_id={core_id}"), &admin)
        .await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert_eq!(list["data"].as_array().expect("data").len(), 1);
    assert_eq!(list["data"][0]["model"], "gpt-4.1");

    let (_, list) = h
        .get(&format!("/api/usage?team_id={edge_id}"), &admin)
        .await;
    assert_eq!(list["data"][0]["model"], "claude-haiku-4-5");

    let (_, all) = h.get("/api/usage", &admin).await;
    assert_eq!(
        all["data"].as_array().expect("data").len(),
        3,
        "no filter is the whole instance"
    );

    // The team row reports the same spend, so the Teams page and the log agree.
    let (status, teams) = h.get("/api/teams", &admin).await;
    assert_eq!(status, StatusCode::OK, "{teams}");
    let core_row = teams["data"]
        .as_array()
        .expect("teams")
        .iter()
        .find(|row| row["name"] == "Core")
        .expect("Core")
        .clone();
    assert_eq!(core_row["spend"]["requests"], 1, "{core_row}");
    assert_eq!(core_row["spend"]["tokens"], 30, "10 in + 20 out");
    assert_eq!(core_row["spend"]["cost_micros"], 100);

    // The summary and the chart honour the same filter.
    let (_, summary) = h
        .get(&format!("/api/usage/summary?team_id={core_id}"), &admin)
        .await;
    assert_eq!(summary["data"].as_array().expect("summary").len(), 1);
    assert_eq!(summary["data"][0]["model"], "gpt-4.1");
    let (status, series) = h
        .get(
            &format!("/api/usage/series?hours=24&team_id={core_id}"),
            &admin,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{series}");
    assert_eq!(
        series["data"]
            .as_array()
            .expect("buckets")
            .iter()
            .map(|bucket| bucket["requests"].as_i64().unwrap_or(0))
            .sum::<i64>(),
        1,
        "one team's bucket: {series}"
    );

    // A member sees their own team without asking, cannot name another team, and sees
    // nothing of the instance-wide traffic.
    let member = h.invite(&owner, "member@example.com", "member").await;
    let (status, body) = h
        .post(
            &format!("/api/teams/{core_id}/members"),
            json!({"email": "member@example.com", "role": "member"}),
            &csrf(&owner),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let (status, scoped) = h.get("/api/usage", &auth_headers(&member)).await;
    assert_eq!(status, StatusCode::OK, "{scoped}");
    assert_eq!(
        scoped["data"].as_array().expect("data").len(),
        1,
        "only the member's team: {scoped}"
    );
    assert_eq!(scoped["data"][0]["model"], "gpt-4.1");

    let (status, body) = h
        .get(
            &format!("/api/usage?team_id={edge_id}"),
            &auth_headers(&member),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["error"]["code"], "team_forbidden");

    let (_, own) = h
        .get(
            &format!("/api/usage?team_id={core_id}"),
            &auth_headers(&member),
        )
        .await;
    assert_eq!(own["data"].as_array().expect("data").len(), 1);

    // A request id is a capability only inside the caller's scope.
    let (_, detail) = h
        .get(&format!("/api/usage?team_id={edge_id}"), &admin)
        .await;
    let edge_request = detail["data"][0]["request_id"]
        .as_str()
        .expect("request id")
        .to_string();
    let (status, _) = h
        .get(
            &format!("/api/usage/{edge_request}"),
            &auth_headers(&member),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "another team's request is invisible"
    );
    let (status, body) = h.get(&format!("/api/usage/{edge_request}"), &admin).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // The CSV export applies the same scope.
    let (status, _) = h
        .get(&format!("/api/usage/export.csv?team_id={core_id}"), &admin)
        .await;
    assert_eq!(status, StatusCode::OK);
}

/// A key that predates teams keeps working and stays out of every team report.
#[tokio::test]
async fn null_team_key_keeps_working() {
    let h = harness();
    let owner = h.setup().await;
    let team = h.create_team(&owner, json!({"name": "Core"})).await;
    let team_id = team["id"].as_str().expect("id").to_string();

    let created = h.create_key(&owner, json!({"name": "legacy"})).await;
    let id = created["id"].as_str().expect("id").to_string();
    let plaintext = created["key"].as_str().expect("plaintext").to_string();
    assert_eq!(created["team_id"], Value::Null);

    // The key still authenticates for `/v1` and still takes policy patches.
    let key = h
        .state
        .store
        .verify_api_key(&plaintext)
        .await
        .expect("lookup")
        .expect("key");
    assert!(key.is_live(rrouter_gateway::now_unix()));
    assert_eq!(
        h.state.store.key_team(key.id).await.expect("team"),
        None,
        "an instance-wide key has no team"
    );

    let (status, patched) = h
        .patch(
            &format!("/api/keys/{id}"),
            json!({"rpm": 60, "budget_tokens": 1_000}),
            &csrf(&owner),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{patched}");
    assert_eq!(patched["rpm"], 60);
    assert_eq!(patched["team_id"], Value::Null);

    let (_, filtered) = h
        .get(
            &format!("/api/keys?team_id={team_id}"),
            &auth_headers(&owner),
        )
        .await;
    assert_eq!(
        filtered["data"].as_array().expect("data").len(),
        0,
        "an instance-wide key is not any team's key"
    );
}

// ------------------------------------------------------------- delegated team admin

/// A `team_admin` runs their own roster: add, change role, remove — but never their own
/// row, and never another team.
#[tokio::test]
async fn team_admin_manages_membership_of_their_own_team() {
    let h = harness();
    let owner = h.setup().await;
    let core = h.create_team(&owner, json!({"name": "Core"})).await;
    let edge = h.create_team(&owner, json!({"name": "Edge"})).await;
    let core_id = core["id"].as_str().expect("id").to_string();
    let edge_id = edge["id"].as_str().expect("id").to_string();

    let tm = h.invite(&owner, "tm@example.com", "member").await;
    // Cookies are not needed for these accounts: only their ids are, to move them around
    // the roster.
    h.invite(&owner, "peer@example.com", "member").await;
    h.invite(&owner, "guest@example.com", "member").await;
    let staff = h.invite(&owner, "staff@example.com", "member").await;

    for (team, email, role) in [
        (&core_id, "tm@example.com", "team_admin"),
        (&core_id, "peer@example.com", "member"),
        (&core_id, "staff@example.com", "member"),
        (&edge_id, "guest@example.com", "member"),
    ] {
        let (status, body) = h
            .post(
                &format!("/api/teams/{team}/members"),
                json!({"email": email, "role": role}),
                &csrf(&owner),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }
    let tm_id = user_id(&h, "tm@example.com").await;
    let peer_id = user_id(&h, "peer@example.com").await;
    let guest_id = user_id(&h, "guest@example.com").await;

    // Add a user from another team, then change an existing member's role: an upsert.
    let (status, body) = h
        .post(
            &format!("/api/teams/{core_id}/members"),
            json!({"email": "guest@example.com", "role": "viewer"}),
            &csrf(&tm),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["data"]["role"], "viewer");

    let (status, body) = h
        .post(
            &format!("/api/teams/{core_id}/members"),
            json!({"user_id": peer_id, "role": "team_admin"}),
            &csrf(&tm),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["data"]["role"], "team_admin", "a delegate may promote");

    // Remove the guest from Core; their Edge membership is untouched.
    assert_eq!(
        h.delete(
            &format!("/api/teams/{core_id}/members/{guest_id}"),
            &csrf(&tm)
        )
        .await,
        StatusCode::OK
    );
    let (_, edge_roster) = h
        .get(
            &format!("/api/teams/{edge_id}/members"),
            &auth_headers(&owner),
        )
        .await;
    assert_eq!(
        edge_roster["data"].as_array().expect("roster").len(),
        1,
        "the other team is untouched: {edge_roster}"
    );

    // The personal rules: no removing or demoting themselves, with either verb.
    assert_eq!(
        h.delete(&format!("/api/teams/{core_id}/members/{tm_id}"), &csrf(&tm))
            .await,
        StatusCode::FORBIDDEN,
        "a team admin cannot remove themselves"
    );
    let (status, body) = h
        .post(
            &format!("/api/teams/{core_id}/members"),
            json!({"user_id": tm_id, "role": "member"}),
            &csrf(&tm),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["error"]["code"], "cannot_manage_self");

    // Another team's roster is out of reach, for both verbs.
    let (status, body) = h
        .post(
            &format!("/api/teams/{edge_id}/members"),
            json!({"email": "peer@example.com", "role": "member"}),
            &csrf(&tm),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["error"]["code"], "role_required");
    assert_eq!(
        h.delete(
            &format!("/api/teams/{edge_id}/members/{guest_id}"),
            &csrf(&tm)
        )
        .await,
        StatusCode::FORBIDDEN
    );

    // A plain member of the same team has no roster rights.
    let (status, body) = h
        .post(
            &format!("/api/teams/{core_id}/members"),
            json!({"email": "staff@example.com", "role": "viewer"}),
            &csrf(&staff),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["error"]["code"], "role_required");

    // A cookie-authenticated write still needs the CSRF header.
    let (status, body) = h
        .post(
            &format!("/api/teams/{core_id}/members"),
            json!({"email": "guest@example.com", "role": "viewer"}),
            &auth_headers(&tm),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["error"]["code"], "csrf_required");

    // The delegate cannot promote themselves away from the rule by writing their own row.
    let (_, roster) = h
        .get(
            &format!("/api/teams/{core_id}/members"),
            &auth_headers(&owner),
        )
        .await;
    let me = roster["data"]
        .as_array()
        .expect("roster")
        .iter()
        .find(|row| row["user_id"] == tm_id)
        .expect("tm row")
        .clone();
    assert_eq!(me["role"], "team_admin", "still a team admin: {me}");
}

/// A `team_admin` is not an administrator: teams themselves, budgets and every admin-only
/// surface stay closed.
#[tokio::test]
async fn team_admin_cannot_administer_teams_or_reach_admin_surfaces() {
    let h = harness();
    let owner = h.setup().await;
    let core = h.create_team(&owner, json!({"name": "Core"})).await;
    let core_id = core["id"].as_str().expect("id").to_string();
    let edge = h.create_team(&owner, json!({"name": "Edge"})).await;
    let edge_id = edge["id"].as_str().expect("id").to_string();

    let tm = h.invite(&owner, "tm@example.com", "member").await;
    let (status, body) = h
        .post(
            &format!("/api/teams/{core_id}/members"),
            json!({"email": "tm@example.com", "role": "team_admin"}),
            &csrf(&owner),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    // Team lifecycle and budget are gateway administration, not team administration.
    let (status, body) = h
        .post("/api/teams", json!({"name": "Owned"}), &csrf(&tm))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["error"]["code"], "role_required");

    let (status, body) = h
        .patch(
            &format!("/api/teams/{core_id}"),
            json!({"budget_tokens": 1_000}),
            &csrf(&tm),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

    let (status, body) = h
        .patch(
            &format!("/api/teams/{edge_id}"),
            json!({"name": "Renamed"}),
            &csrf(&tm),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

    assert_eq!(
        h.delete(&format!("/api/teams/{core_id}"), &csrf(&tm)).await,
        StatusCode::FORBIDDEN
    );

    // Provider accounts are a scoped read rather than an admin surface: the delegate sees
    // the rows their own team owns, never a shared instance row or another team's.
    for (name, team) in [
        ("core-account", Some(core_id.as_str())),
        ("edge-account", Some(edge_id.as_str())),
        ("shared-account", None),
    ] {
        let mut body = json!({
            "provider": "openai",
            "auth_type": "api_key",
            "name": name,
            "credentials": {"api_key": "sk-test", "base_url_override": "http://127.0.0.1:1"},
        });
        if let Some(team) = team {
            body["team_id"] = json!(team);
        }
        let (status, created) = h.post("/api/connections", body, &csrf(&owner)).await;
        assert_eq!(status, StatusCode::CREATED, "{name}: {created}");
    }

    // The admin surface stays closed even where the delegate can read team data.
    for uri in [
        "/api/keys",
        "/api/accounts/quotas",
        "/api/settings",
        "/api/users",
        "/api/audit",
        "/api/system",
    ] {
        let (status, body) = h.get(uri, &auth_headers(&tm)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{uri}: {body}");
        assert_eq!(body["error"]["code"], "role_required", "{uri}");
    }

    // The connection list is one of those scoped reads.
    let (status, connections) = h.get("/api/connections", &auth_headers(&tm)).await;
    assert_eq!(status, StatusCode::OK, "{connections}");
    let visible: Vec<&str> = connections["data"]
        .as_array()
        .expect("data")
        .iter()
        .map(|row| row["name"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(
        visible,
        vec!["core-account"],
        "only their own team's account"
    );
    let (_, all) = h.get("/api/connections", &auth_headers(&owner)).await;
    assert_eq!(all["data"].as_array().expect("data").len(), 3);

    // Reads the reader surface allows still work, scoped to their own team.
    for uri in ["/api/usage", "/api/teams", "/api/models"] {
        let (status, body) = h.get(uri, &auth_headers(&tm)).await;
        assert_eq!(status, StatusCode::OK, "{uri}: {body}");
    }
    let (_, teams) = h.get("/api/teams", &auth_headers(&tm)).await;
    assert_eq!(names(&teams), vec!["Core"], "only their own team");
}

/// Team-scoped key visibility: the delegate sees the rows of their team, never another
/// team's, never a plaintext key, and a plain member sees none.
#[tokio::test]
async fn team_admin_reads_only_their_teams_key_rows() {
    let h = harness();
    let owner = h.setup().await;
    let core = h.create_team(&owner, json!({"name": "Core"})).await;
    let edge = h.create_team(&owner, json!({"name": "Edge"})).await;
    let core_id = core["id"].as_str().expect("id").to_string();
    let edge_id = edge["id"].as_str().expect("id").to_string();

    let core_key = h
        .create_key(&owner, json!({"name": "core-key", "team_id": core_id}))
        .await;
    let core_plaintext = core_key["key"].as_str().expect("plaintext").to_string();
    h.create_key(&owner, json!({"name": "edge-key", "team_id": edge_id}))
        .await;
    h.create_key(&owner, json!({"name": "global-key"})).await;

    let tm = h.invite(&owner, "tm@example.com", "member").await;
    let member = h.invite(&owner, "member@example.com", "member").await;
    for (email, role) in [
        ("tm@example.com", "team_admin"),
        ("member@example.com", "member"),
    ] {
        let (status, body) = h
            .post(
                &format!("/api/teams/{core_id}/members"),
                json!({"email": email, "role": role}),
                &csrf(&owner),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }

    let (status, body) = h
        .get(&format!("/api/teams/{core_id}/keys"), &auth_headers(&tm))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let rows = body["data"].as_array().expect("rows");
    assert_eq!(rows.len(), 1, "only the team's key: {body}");
    assert_eq!(rows[0]["name"], "core-key");
    assert_eq!(rows[0]["team_id"], core_id, "same projection as /api/keys");
    assert!(
        rows[0].get("key").is_none(),
        "no plaintext field: {}",
        rows[0]
    );
    assert!(
        !body.to_string().contains(&core_plaintext),
        "the plaintext key never appears in a list"
    );
    assert!(!body.to_string().contains("key_hash"));

    // Another team, and a plain member of this one, are refused.
    let (status, body) = h
        .get(&format!("/api/teams/{edge_id}/keys"), &auth_headers(&tm))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["error"]["code"], "role_required");

    let (status, _) = h
        .get(
            &format!("/api/teams/{core_id}/keys"),
            &auth_headers(&member),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "a member does not see keys");

    // The admin list is unchanged for a gateway admin, and is scoped by the same query.
    let (status, admin_view) = h
        .get(&format!("/api/teams/{edge_id}/keys"), &auth_headers(&owner))
        .await;
    assert_eq!(status, StatusCode::OK, "{admin_view}");
    assert_eq!(admin_view["data"].as_array().expect("rows").len(), 1);
    let (status, all) = h.get("/api/keys", &auth_headers(&owner)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(all["data"].as_array().expect("keys").len(), 3);
}

/// Global administration is unchanged: the rules the delegate is denied are the ones an
/// admin still performs.
#[tokio::test]
async fn global_admin_is_unaffected_by_the_team_rules() {
    let h = harness();
    let owner = h.setup().await;
    let core = h.create_team(&owner, json!({"name": "Core"})).await;
    let core_id = core["id"].as_str().expect("id").to_string();

    let tm = h.invite(&owner, "tm@example.com", "member").await;
    let peer = h.invite(&owner, "peer@example.com", "member").await;
    let peer_id = user_id(&h, "peer@example.com").await;

    // The admin adds the delegate, changes the budget, renames the team, edits the roster
    // and reads keys: every action the delegate was refused above.
    let (status, body) = h
        .post(
            &format!("/api/teams/{core_id}/members"),
            json!({"email": "tm@example.com", "role": "team_admin"}),
            &csrf(&owner),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let (status, body) = h
        .patch(
            &format!("/api/teams/{core_id}"),
            json!({"budget_tokens": 5_000, "budget_window": "daily"}),
            &csrf(&owner),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["budget_tokens"], 5_000);

    let (status, body) = h
        .patch(
            &format!("/api/teams/{core_id}"),
            json!({"name": "Core Platform"}),
            &csrf(&owner),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["name"], "Core Platform");

    let (status, body) = h
        .post(
            &format!("/api/teams/{core_id}/members"),
            json!({"user_id": peer_id, "role": "viewer"}),
            &csrf(&owner),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let (status, body) = h
        .get(&format!("/api/teams/{core_id}/keys"), &auth_headers(&owner))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // An admin may remove a team_admin, including one who blocked themselves.
    let tm_id = user_id(&h, "tm@example.com").await;
    assert_eq!(
        h.delete(
            &format!("/api/teams/{core_id}/members/{tm_id}"),
            &csrf(&owner)
        )
        .await,
        StatusCode::OK
    );

    assert_eq!(
        h.delete(&format!("/api/teams/{core_id}"), &csrf(&owner))
            .await,
        StatusCode::OK
    );
    // The invited cookies exist only to prove the delegation setup; both still read.
    for cookie in [&tm, &peer] {
        let (status, _) = h.get("/api/teams", &auth_headers(cookie)).await;
        assert_eq!(status, StatusCode::OK);
    }
}
