//! `GET /health`.

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
