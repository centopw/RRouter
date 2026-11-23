//! `GET /api/accounts/usage/{id}/history` end to end: one daily point per day of the
//! window, the max percent seen that day, and the admin gate of the sibling
//! `/api/accounts/usage` route.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{HeaderMap, Method, Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use rrouter_auth::Credentials;
use rrouter_gateway::{app, auth::Auth, config::Config, AppState};
use rrouter_router::{Breaker, Catalog, Dispatcher, Planner};
use rrouter_store::Store;
use serde_json::{json, Value};
use tower::ServiceExt;
use uuid::Uuid;

mod support;

const ADMIN_TOKEN: &str = "adm-test";

struct H {
    state: Arc<AppState>,
    router: Router,
    _data_dir: support::TempDir,
}

fn harness() -> H {
    let data_dir = support::TempDir::new("quota-history");
    let client = || {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .expect("client")
    };
    let config = Config {
        host: "127.0.0.1".parse().expect("ip"),
        port: 0,
        data_dir: data_dir.path().to_path_buf(),
        master_key: None,
        require_api_key: false,
        request_timeout_secs: 5,
        connect_timeout_secs: 1,
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
    };
    let state = Arc::new(AppState {
        store: Arc::new(Store::open_in_memory().expect("store")),
        catalog: Arc::new(Catalog::builtin()),
        refresher: Arc::new(rrouter_auth::Refresher::new(client())),
        planner: Planner::new(),
        dispatcher: Arc::new(Dispatcher::new(Breaker::new()).with_max_attempts(4)),
        http: client(),
        config: rrouter_gateway::config::ConfigSnapshot::from(config),
        started: std::time::Instant::now(),
        oauth: rrouter_auth::OAuthFlows::new(),
        counters: Default::default(),
        envelope_drops: Default::default(),
        providers: Default::default(),
        concurrency: std::sync::RwLock::new(None),
        proxy_clients: Default::default(),
        auth: Auth::load(data_dir.path(), false),
    });
    let router = app(Arc::clone(&state));
    H {
        state,
        router,
        _data_dir: data_dir,
    }
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

    /// The owner account and its session cookie.
    async fn setup(&self) -> String {
        let (status, headers, body) = self
            .call(
                Method::POST,
                "/api/auth/setup",
                Some(json!({
                    "setup_code": self.state.auth.code().unwrap_or_default(),
                    "name": "Ada Lovelace",
                    "email": "ada@example.com",
                    "password": "correct horse battery",
                })),
                &[],
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "setup: {body}");
        headers
            .get("set-cookie")
            .and_then(|v| v.to_str().ok())
            .and_then(|raw| raw.split(';').next())
            .map(str::to_string)
            .expect("owner cookie")
    }

    /// Invite and sign up a member, returning their session cookie.
    async fn member(&self, owner: &str) -> String {
        let csrf = [("cookie", owner), ("x-rrouter-csrf", "1")];
        let (status, body) = self
            .post(
                "/api/users/invites",
                json!({"email": "reader@example.com", "role": "member"}),
                &csrf,
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "invite: {body}");
        let token = body["token"].as_str().expect("invite token").to_string();
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
        headers
            .get("set-cookie")
            .and_then(|v| v.to_str().ok())
            .and_then(|raw| raw.split(';').next())
            .map(str::to_string)
            .expect("member cookie")
    }

    async fn account(&self) -> Uuid {
        self.state
            .store
            .create_connection("claude", "oauth", Credentials::default(), Some("Ada"))
            .await
            .expect("connection")
            .id
    }
}

const ADMIN: [(&str, &str); 1] = [("x-admin-token", ADMIN_TOKEN)];

#[tokio::test]
async fn history_groups_by_day_and_keeps_the_daily_max() {
    let h = harness();
    let id = h.account().await;
    let now = rrouter_gateway::now_unix();
    let today = (now / 86_400) * 86_400;
    let yesterday = today - 86_400;
    // Two samples today, one yesterday: the endpoint must keep the larger one per day.
    for (used, at) in [
        (10.0, today + 60),
        (42.5, today + 3_600),
        (5.0, yesterday + 60),
    ] {
        h.state
            .store
            .insert_quota_ping_sample(id, "five_hour", used, None, at)
            .await
            .expect("sample");
    }
    // A different window and another account must not leak into this series.
    h.state
        .store
        .insert_quota_ping_sample(id, "weekly", 99.0, None, today + 120)
        .await
        .expect("sample");

    let (status, body) = h
        .get(
            &format!("/api/accounts/usage/{id}/history?window=five_hour"),
            &ADMIN,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let days = body["data"].as_array().expect("data");
    assert_eq!(days.len(), 2, "{body}");
    assert_eq!(days[0]["day"], json!(yesterday));
    assert_eq!(days[0]["used_percent"], json!(5.0));
    assert_eq!(days[1]["day"], json!(today));
    assert_eq!(days[1]["used_percent"], json!(42.5));
    assert!(days.len() <= 31, "at most one point per day of a month");
}

#[tokio::test]
async fn usage_list_keeps_accounts_it_must_not_fetch() {
    let h = harness();
    // An inactive OAuth account: the background pass skips it, the page still needs it.
    let retired = h.account().await;
    h.state
        .store
        .set_connection_active(retired, false)
        .await
        .expect("deactivate");
    // An API-key account on a supported provider has no plan windows but keeps its row.
    let keyed = h
        .state
        .store
        .create_connection(
            "claude",
            "apikey",
            Credentials {
                api_key: Some("sk-test".into()),
                ..Default::default()
            },
            Some("Key"),
        )
        .await
        .expect("key connection")
        .id;
    // A provider without a usage endpoint never appears.
    h.state
        .store
        .create_connection(
            "openrouter",
            "apikey",
            Credentials::default(),
            Some("Elsewhere"),
        )
        .await
        .expect("unsupported connection");

    let (status, body) = h.get("/api/accounts/usage", &ADMIN).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let rows = body["accounts"].as_array().expect("accounts");
    assert_eq!(rows.len(), 2, "{body}");
    let row = |id: Uuid| {
        rows.iter()
            .find(|row| row["connection_id"] == json!(id.to_string()))
            .unwrap_or_else(|| panic!("no row for {id}: {body}"))
    };
    let retired_row = row(retired);
    assert_eq!(retired_row["windows"], json!([]));
    assert!(
        retired_row["error"]
            .as_str()
            .is_some_and(|error| error.contains("inactive")),
        "{retired_row}"
    );
    let keyed_row = row(keyed);
    assert_eq!(keyed_row["windows"], json!([]));
    assert_eq!(
        keyed_row["error"],
        json!("API-key accounts have no plan windows")
    );
}

#[tokio::test]
async fn history_is_an_admin_read() {
    let h = harness();
    let id = h.account().await;
    let uri = format!("/api/accounts/usage/{id}/history?window=five_hour");

    // A signed-in member holds a real account but no admin authority.
    let owner = h.setup().await;
    let member = h.member(&owner).await;
    let (status, body) = h.get(&uri, &[("cookie", &member)]).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

    // `window` is required: a chart cannot know which series to draw without it.
    let (status, body) = h
        .get(&format!("/api/accounts/usage/{id}/history"), &ADMIN)
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    // An unknown account answers like the sibling usage route: 404, not an empty list.
    let (status, body) = h
        .get(
            &format!(
                "/api/accounts/usage/{}/history?window=five_hour",
                Uuid::new_v4()
            ),
            &ADMIN,
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}
