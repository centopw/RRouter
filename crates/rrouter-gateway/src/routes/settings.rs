//! `/api/settings` — white-label branding and extension panel declarations.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::State;
use axum::http::HeaderMap;
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
///
/// The mutation is audited after it commits: one row naming the keys that moved. Values
/// are deliberately not recorded — the blob holds operator-supplied panel configuration,
/// and the audit trail must be safe to read for anyone who can read it.
pub async fn put_settings(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    peer: crate::auth::Peer,
    body: Body,
) -> Response {
    let patch = match body_json(body).await {
        Ok(v) => v,
        Err(resp) => return *resp,
    };
    let keys = patched_keys(&patch);
    match crate::settings::merge(&state, patch).await {
        Ok(value) => {
            let actor = super::auth::actor(&state, &headers).await;
            // Best effort: `audit` logs a failed write and never fails the mutation, and
            // it owns the webhook enqueue for the row.
            super::auth::audit(
                &state,
                "settings_updated",
                Some(&actor),
                &crate::auth::client_ip(&headers, peer.0),
                &format!("settings · keys {keys}"),
            )
            .await;
            ok_json(value)
        }
        Err(e) => admin_error(&e),
    }
}

/// Top-level keys a settings patch writes, for the audit row. Names only, sorted: the
/// detail is stable across clients and cannot carry a value.
fn patched_keys(patch: &serde_json::Value) -> String {
    let Some(map) = patch.as_object() else {
        return "none".to_string();
    };
    let mut keys: Vec<&str> = map.keys().map(String::as_str).collect();
    keys.sort_unstable();
    if keys.is_empty() {
        "none".to_string()
    } else {
        keys.join(", ")
    }
}
