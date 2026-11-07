use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::Request;
use http_body_util::BodyExt;
use rrouter_gateway::{app, config::Config, AppState};
use rrouter_router::{Breaker, Catalog, Dispatcher, Planner};
use rrouter_store::Store;
use serde_json::Value;
use tower::ServiceExt;

fn test_state() -> Arc<AppState> {
    Arc::new(AppState {
        store: Arc::new(Store::open_in_memory().expect("store")),
        catalog: Arc::new(Catalog::builtin()),
        refresher: Arc::new(rrouter_auth::Refresher::new(
            reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .build()
                .expect("client"),
        )),
        planner: Planner::new(),
        dispatcher: Arc::new(Dispatcher::new(Breaker::new())),
        http: reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .expect("client"),
        config: Config {
            host: "127.0.0.1".parse().expect("host"),
            port: 0,
            data_dir: std::env::temp_dir(),
            master_key: None,
            require_api_key: false,
            request_timeout_secs: 5,
            connect_timeout_secs: 5,
            log_filter: "rrouter=error".into(),
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
        },
        started: Instant::now(),
        oauth: rrouter_auth::OAuthFlows::new(),
        counters: Default::default(),
        concurrency: None,
    })
}

#[tokio::test]
async fn providers_lists_only_registered_login_flows() {
    let response = app(test_state())
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/oauth/providers")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let body = response.into_body().collect().await.expect("body").to_bytes();
    let value: Value = serde_json::from_slice(&body).expect("json");
    let providers = value["data"].as_array().expect("provider list");
    assert_eq!(providers.len(), 3);

    let flow = |id: &str| {
        providers
            .iter()
            .find(|provider| provider["id"] == id)
            .unwrap_or_else(|| panic!("missing provider {id}"))["flow"]
            .as_str()
            .expect("flow")
    };
    assert_eq!(flow("claude"), "pkce");
    assert_eq!(flow("codex"), "pkce");
    assert_eq!(flow("github"), "device_code");
    assert!(!providers.iter().any(|provider| provider["id"] == "openai"));
}
