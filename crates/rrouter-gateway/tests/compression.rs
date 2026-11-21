//! `GET /api/compression/stats` end to end: the aggregate the Compression page reads.
//!
//! The counters are seeded straight through the store, so the route test covers the
//! handler's windowing, arithmetic and team narrowing rather than the proxy's recording
//! path (which the store tests cover from the writer side).

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use rrouter_gateway::{app, auth::Auth, config::Config, AppState};
use rrouter_router::{Breaker, Catalog, Dispatcher, Planner};
use rrouter_store::{BudgetWindow, CompressionEvent, Store};
use serde_json::Value;
use tower::ServiceExt;

mod support;

struct H {
    state: Arc<AppState>,
    router: Router,
    _data_dir: support::TempDir,
}

fn config() -> Config {
    Config {
        host: "127.0.0.1".parse().expect("ip"),
        port: 0,
        data_dir: support::temp_path("compression"),
        master_key: None,
        require_api_key: false,
        request_timeout_secs: 30,
        connect_timeout_secs: 5,
        log_filter: "rrouter=error".into(),
        static_dir: None,
        cors_origins: vec![],
        allow_remote_admin: false,
        // No admin token: an in-process `oneshot` call is loopback, which `require_reader`
        // admits as an admin. That keeps this test about the handler, not about sessions.
        admin_token: None,
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
    let data_dir = support::TempDir::adopt(config.data_dir.clone());
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
        envelope_drops: Default::default(),
        providers: Default::default(),
        concurrency: std::sync::RwLock::new(None),
        auth: Auth::load(&data_dir, false),
    });
    let router = app(Arc::clone(&state));
    H {
        state,
        router,
        _data_dir: data_dir,
    }
}

impl H {
    async fn get(&self, uri: &str) -> (StatusCode, Value) {
        let request = Request::builder()
            .method("GET")
            .uri(uri)
            .body(Body::empty())
            .expect("request");
        let res = self.router.clone().oneshot(request).await.expect("oneshot");
        let status = res.status();
        let bytes = res.into_body().collect().await.expect("body").to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    async fn record(
        &self,
        ts: i64,
        key: Option<uuid::Uuid>,
        before: u64,
        after: u64,
        caveman: bool,
    ) {
        self.state
            .store
            .record_compression(&CompressionEvent {
                ts,
                request_id: Some(uuid::Uuid::new_v4()),
                key_id: key,
                rtk_before: before,
                rtk_after: after,
                caveman,
            })
            .await
            .expect("record");
    }
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs() as i64
}

/// Totals, the saturating saving, the documented token estimate, and the window: rows
/// older than the requested range are excluded.
#[tokio::test]
async fn stats_sums_the_window_and_estimates_tokens() {
    let h = harness();
    let now = now();
    h.record(now - 60, None, 4_000, 1_200, false).await;
    h.record(now - 120, None, 0, 0, true).await;
    h.record(now - 30 * 24 * 3600, None, 999, 1, false).await;

    let (status, body) = h.get("/api/compression/stats?hours=24").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["hours"], 24);
    assert_eq!(body["requests"], 2, "{body}");
    assert_eq!(body["rtk_requests"], 1);
    assert_eq!(body["rtk_before"], 4_000);
    assert_eq!(body["rtk_after"], 1_200);
    assert_eq!(body["rtk_saved"], 2_800);
    assert_eq!(body["rtk_saved_tokens_estimate"], 700, "2800 bytes / 4");
    assert_eq!(body["caveman_requests"], 1);

    // A narrower window still includes both recent rows.
    let (status, recent) = h.get("/api/compression/stats?hours=1").await;
    assert_eq!(status, StatusCode::OK, "{recent}");
    assert_eq!(recent["requests"], 2);
}

/// No rows is zeros, never a missing field or a 404: the page renders `0` and `—` from a
/// successful answer.
#[tokio::test]
async fn stats_is_zero_without_events() {
    let h = harness();
    let (status, body) = h.get("/api/compression/stats").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["requests"], 0);
    assert_eq!(body["rtk_requests"], 0);
    assert_eq!(body["rtk_before"], 0);
    assert_eq!(body["rtk_after"], 0);
    assert_eq!(body["rtk_saved"], 0);
    assert_eq!(body["rtk_saved_tokens_estimate"], 0);
    assert_eq!(body["caveman_requests"], 0);
}

/// `team_id` narrows for an admin, exactly like the usage summary, and an unparseable id
/// is a 400 rather than a silently unscoped answer.
#[tokio::test]
async fn stats_narrows_to_one_team() {
    let h = harness();
    let now = now();
    let team = h
        .state
        .store
        .create_team("alpha", 0, 0, BudgetWindow::Total)
        .await
        .expect("team");
    let (_, in_team) = h
        .state
        .store
        .create_api_key_in_team(Some("a"), Some(team.id))
        .await
        .expect("team key");
    let (_, global) = h.state.store.create_api_key(Some("g")).await.expect("key");
    h.record(now - 60, Some(in_team.id), 1_000, 250, true).await;
    h.record(now - 60, Some(global.id), 8_000, 8_000, false)
        .await;

    let (status, all) = h.get("/api/compression/stats").await;
    assert_eq!(status, StatusCode::OK, "{all}");
    assert_eq!(all["hours"], 168, "one week is the default window");
    assert_eq!(all["requests"], 2);

    let (status, scoped) = h
        .get(&format!("/api/compression/stats?team_id={}", team.id))
        .await;
    assert_eq!(status, StatusCode::OK, "{scoped}");
    assert_eq!(scoped["requests"], 1, "{scoped}");
    assert_eq!(scoped["rtk_before"], 1_000);
    assert_eq!(scoped["rtk_saved"], 750);
    assert_eq!(scoped["caveman_requests"], 1);

    let (status, body) = h.get("/api/compression/stats?team_id=nope").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}
