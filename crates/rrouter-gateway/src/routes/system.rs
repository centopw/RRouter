//! `GET /api/system` — deployment facts without secrets.
//!
//! Counters and booleans only: no paths, no keys, no tokens, no host addresses. An
//! enterprise operator needs to know *whether* something is configured, not what it is.

use std::sync::Arc;

use axum::extract::State;
use axum::response::Response;
use serde_json::json;

use super::ok_json;
use crate::state::AppState;

pub async fn system(State(state): State<Arc<AppState>>) -> Response {
    // Restart-only fields dominate this view, but read one snapshot for consistency.
    let config = state.config.load();
    let connections = state.store.list_connections(None).await.unwrap_or_default();
    let active = connections.iter().filter(|c| c.is_active).count();
    let providers: Vec<String> = {
        let mut names: Vec<String> = connections.iter().map(|c| c.provider.clone()).collect();
        names.sort();
        names.dedup();
        names
    };
    let api_keys = state.store.count_api_keys().await.unwrap_or(0);
    let combos = state
        .store
        .list_combos()
        .await
        .map(|c| c.len())
        .unwrap_or(0);
    let aliases = state
        .store
        .list_aliases()
        .await
        .map(|a| a.len())
        .unwrap_or(0);
    let disabled = state
        .store
        .disabled_models()
        .await
        .map(|d| d.len())
        .unwrap_or(0);

    ok_json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "uptime_secs": state.uptime_secs(),
        "require_api_key": config.require_api_key,
        "allow_remote_admin": config.allow_remote_admin,
        "static_assets": config.static_dir.is_some(),
        "custom_cors": !config.cors_origins.is_empty(),
        "master_key_source": if config.master_key.is_some() { "env" } else { "file" },
        "counts": {
            "connections": connections.len(),
            "active_connections": active,
            "providers": providers,
            "api_keys": api_keys,
            "combos": combos,
            "aliases": aliases,
            "disabled_models": disabled,
        },
    }))
}
