//! `/api/update` — release checks, staged downloads, and deployment-aware restarts.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::State;
use axum::response::Response;
use serde_json::json;

use super::{admin_error, body_json, ok_json};
use crate::state::AppState;

pub async fn status(State(state): State<Arc<AppState>>) -> Response {
    ok_json(crate::updates::snapshot(&state).await)
}

/// `POST /api/update/check` always checks; `POST /api/update/download` checks and stages.
pub async fn check(State(state): State<Arc<AppState>>) -> Response {
    match crate::updates::check(&state, false).await {
        Ok(value) => ok_json(value),
        Err(e) => admin_error(&e),
    }
}

pub async fn download(State(state): State<Arc<AppState>>) -> Response {
    match crate::updates::check(&state, true).await {
        Ok(value) => ok_json(value),
        Err(e) => admin_error(&e),
    }
}

pub async fn apply(State(state): State<Arc<AppState>>) -> Response {
    match crate::updates::apply(&state).await {
        Ok(value) => ok_json(value),
        Err(e) => admin_error(&e),
    }
}

/// `PUT /api/update/settings {auto_check, auto_download}` — persisted with the rest of
/// the gateway settings; the scheduler reads them on every tick.
pub async fn settings(State(state): State<Arc<AppState>>, body: Body) -> Response {
    let value = match body_json(body).await {
        Ok(v) => v,
        Err(resp) => return *resp,
    };
    let mut patch = serde_json::Map::new();
    for key in ["auto_check", "auto_download"] {
        if let Some(flag) = value.get(key) {
            let Some(flag) = flag.as_bool() else {
                return admin_error(&rrouter_core::Error::BadRequest(format!(
                    "{key} must be a boolean"
                )));
            };
            patch.insert(format!("update_{key}"), json!(flag));
        }
    }
    if patch.is_empty() {
        return admin_error(&rrouter_core::Error::BadRequest(
            "auto_check or auto_download is required".into(),
        ));
    }
    if let Err(e) = crate::settings::merge(&state, serde_json::Value::Object(patch)).await {
        return admin_error(&e);
    }
    ok_json(crate::updates::snapshot(&state).await)
}
