//! `/api/system/logs` — opt-in, in-memory platform log capture for the dashboard.
//!
//! Logging is off by default. Enabling it only affects this process (`tracing` events are
//! still written to the console either way) and stores at most
//! [`crate::syslog::CAPACITY`] entries in memory; nothing is written to disk and the
//! buffer is cleared on restart.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Query, State};
use axum::response::Response;
use serde_json::json;

use super::{admin_error, body_json, ok_json};
use crate::state::AppState;

/// `GET /api/system/logs?after=<id>&limit=<n>`: newest `limit` events after `after`.
pub async fn list(Query(query): Query<Vec<(String, String)>>) -> Response {
    let param = |key: &str| {
        query
            .iter()
            .find(|(k, _)| k == key)
            .and_then(|(_, v)| v.parse::<u64>().ok())
    };
    let after = param("after").unwrap_or(0);
    let limit = param("limit")
        .unwrap_or(500)
        .clamp(1, crate::syslog::CAPACITY as u64) as usize;
    ok_json(json!({
        "enabled": crate::syslog::enabled(),
        "entries": crate::syslog::snapshot(after, limit),
        "capacity": crate::syslog::CAPACITY,
    }))
}

/// `PUT /api/system/logs {enabled}`: persist the switch and start/stop capture.
pub async fn update(State(state): State<Arc<AppState>>, body: Body) -> Response {
    let value = match body_json(body).await {
        Ok(v) => v,
        Err(resp) => return *resp,
    };
    let Some(enabled) = value.get("enabled").and_then(serde_json::Value::as_bool) else {
        return admin_error(&rrouter_core::Error::BadRequest(
            "enabled (boolean) is required".into(),
        ));
    };
    if let Err(e) = crate::settings::merge(&state, json!({ "system_logs": enabled })).await {
        return admin_error(&e);
    }
    if !enabled {
        crate::syslog::clear();
    }
    ok_json(json!({ "enabled": enabled }))
}

/// `DELETE /api/system/logs`: drop the in-memory buffer. The switch is unchanged.
pub async fn clear() -> Response {
    crate::syslog::clear();
    ok_json(json!({ "cleared": true }))
}
