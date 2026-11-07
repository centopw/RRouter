//! `GET /health` and `GET /ready`.

use std::sync::Arc;

use axum::extract::State;
use axum::response::{IntoResponse, Response};

use crate::state::AppState;

pub async fn health(State(state): State<Arc<AppState>>) -> Response {
    axum::Json(serde_json::json!({
        "ok": true,
        "version": env!("CARGO_PKG_VERSION"),
        "uptime_secs": state.uptime_secs(),
    }))
    .into_response()
}

/// `GET /ready` — readiness for orchestrators: the process is up *and* the store answers.
/// Unlike `/health`, a database that cannot be read is not ready.
pub async fn ready(State(state): State<Arc<AppState>>) -> Response {
    match state.store.users_exist().await {
        Ok(_) => (
            axum::http::StatusCode::OK,
            axum::Json(serde_json::json!({ "ready": true, "version": env!("CARGO_PKG_VERSION") })),
        )
            .into_response(),
        Err(e) => (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            axum::Json(serde_json::json!({ "ready": false, "error": e.to_string() })),
        )
            .into_response(),
    }
}
