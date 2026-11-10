//! `GET /api/system` — deployment facts without secrets.
//!
//! Counters and booleans only: no paths, no keys, no tokens, no host addresses. An
//! enterprise operator needs to know *whether* something is configured, not what it is.

use std::sync::Arc;

use axum::extract::State;
use axum::response::Response;
use serde_json::json;

use super::{admin_error, ok_json};
use crate::state::AppState;

pub async fn system(State(state): State<Arc<AppState>>) -> Response {
    // Restart-only fields dominate this view, but read one snapshot for consistency.
    let config = state.config.load();
    let connections = match state.store.list_connections(None).await {
        Ok(rows) => rows,
        Err(e) => return admin_error(&e),
    };
    let active = connections.iter().filter(|c| c.is_active).count();
    let providers: Vec<String> = {
        let mut names: Vec<String> = connections.iter().map(|c| c.provider.clone()).collect();
        names.sort();
        names.dedup();
        names
    };
    let api_keys = match state.store.count_api_keys().await {
        Ok(n) => n,
        Err(e) => return admin_error(&e),
    };
    let combos = match state.store.list_combos().await {
        Ok(rows) => rows.len(),
        Err(e) => return admin_error(&e),
    };
    let aliases = match state.store.list_aliases().await {
        Ok(rows) => rows.len(),
        Err(e) => return admin_error(&e),
    };
    let disabled = match state.store.disabled_models().await {
        Ok(rows) => rows.len(),
        Err(e) => return admin_error(&e),
    };

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
