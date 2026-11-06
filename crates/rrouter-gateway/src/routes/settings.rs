//! `/api/settings` — white-label branding and extension panel declarations.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::State;
use axum::response::Response;

use super::{admin_error, body_json, ok_json};
use crate::state::AppState;

pub async fn get_settings(State(state): State<Arc<AppState>>) -> Response {
    match crate::settings::load(&state).await {
        Ok(value) => ok_json(value),
        Err(e) => admin_error(&e),
    }
}

/// `PUT /api/settings` merges the body into the stored settings.
pub async fn put_settings(State(state): State<Arc<AppState>>, body: Body) -> Response {
    let patch = match body_json(body).await {
        Ok(v) => v,
        Err(resp) => return *resp,
    };
    match crate::settings::merge(&state, patch).await {
        Ok(value) => ok_json(value),
        Err(e) => admin_error(&e),
    }
}
