//! `/api/webhooks` — receiver registry and delivery status.
//!
//! Every endpoint here is admin-only (it lives on the `api` router). Responses never
//! contain a signing secret: a row exposes the *name* of the environment variable
//! (`secret_env`) and whether one is set, never the value.
//!
//! The queue itself is read-only from the API: `GET /api/webhooks/status` reports counts
//! and the most recent deliveries so an operator can see retries without a database
//! shell. Delivery is the worker's job (see [`crate::webhooks`]); nothing here sends.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::response::Response;
use rrouter_core::Error;
use rrouter_store::{WebhookEndpoint, WebhookPatch};
use serde_json::{json, Value};
use uuid::Uuid;

use super::{admin_error, body_json, ok_json};
use crate::events::EventType;
use crate::state::AppState;

/// How many recent deliveries the status screen shows.
const RECENT_LIMIT: usize = 50;

/// The public projection. `secret_env` is a variable name, not a secret; `secret_configured`
/// is the result of resolving it now (value read and discarded), so the UI can warn before
/// the first delivery instead of discovering the gap in `last_error`.
fn project(row: &WebhookEndpoint) -> Value {
    let secret_configured = row
        .secret_env
        .as_deref()
        .is_some_and(crate::webhooks::secret_configured);
    json!({
        "id": row.id.to_string(),
        "url": row.url,
        // Always an array, in both list and row responses.
        "event_types": row.event_types,
        "secret_env": row.secret_env,
        "has_secret": row.secret_env.is_some(),
        "secret_configured": secret_configured,
        "ready": secret_configured && row.enabled,
        "enabled": row.enabled,
        "created_at": row.created_at,
        "updated_at": row.updated_at,
    })
}

fn str_field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// The subscription vocabulary as an array. `*` subscribes to everything.
fn accepted_types() -> Vec<&'static str> {
    let mut names: Vec<&str> = EventType::ALL.iter().map(|kind| kind.as_str()).collect();
    names.push("*");
    names
}

/// Validate against the event vocabulary this build knows. `*` subscribes to all.
fn parse_event_types(value: &Value) -> Result<Vec<String>, Error> {
    let Some(array) = value.get("event_types") else {
        return Err(Error::BadRequest(
            "event_types is required (an array of event names, or [\"*\"])".into(),
        ));
    };
    let Some(array) = array.as_array() else {
        return Err(Error::BadRequest("event_types must be an array".into()));
    };
    let mut out: Vec<String> = Vec::with_capacity(array.len());
    for item in array {
        let Some(name) = item.as_str() else {
            return Err(Error::BadRequest(
                "every event type must be a string".into(),
            ));
        };
        if name == "*" {
            if !out.iter().any(|seen| seen == "*") {
                out.push("*".to_string());
            }
            continue;
        }
        let Some(kind) = EventType::parse(name) else {
            return Err(Error::BadRequest(format!(
                "unknown event type {name:?}; accepted: {}",
                accepted_types().join(", ")
            )));
        };
        let wire = kind.as_str().to_string();
        if !out.iter().any(|seen| seen == &wire) {
            out.push(wire);
        }
    }
    Ok(out)
}

fn parse_id(raw: &str) -> Result<Uuid, Error> {
    Uuid::parse_str(raw).map_err(|_| Error::NotFound(format!("webhook {raw}")))
}

/// `GET /api/webhooks`
pub async fn list(State(state): State<Arc<AppState>>) -> Response {
    match state.store.list_webhooks().await {
        Ok(rows) => ok_json(json!({
            "webhooks": rows.iter().map(project).collect::<Vec<_>>(),
            "max": rrouter_store::MAX_WEBHOOKS,
            // The subscription vocabulary the UI renders as checkboxes; `*` subscribes
            // to everything. The array is the contract; `event_types_csv` keeps older
            // dashboards that read the former comma-joined string working.
            "event_types": accepted_types(),
            "event_types_csv": accepted_types().join(", "),
        })),
        Err(e) => admin_error(&e),
    }
}

/// `POST /api/webhooks`
pub async fn create(State(state): State<Arc<AppState>>, body: Body) -> Response {
    let value = match body_json(body).await {
        Ok(v) => v,
        Err(resp) => return *resp,
    };
    let Some(url) = str_field(&value, "url") else {
        return admin_error(&Error::BadRequest("url is required".into()));
    };
    let event_types = match parse_event_types(&value) {
        Ok(types) => types,
        Err(e) => return admin_error(&e),
    };
    let Some(secret_env) = str_field(&value, "secret_env") else {
        return admin_error(&Error::BadRequest(
            "secret_env is required: deliveries are always signed".into(),
        ));
    };
    match state
        .store
        .create_webhook(&url, event_types, Some(&secret_env), crate::now_unix())
        .await
    {
        Ok(row) => ok_json(json!({ "webhook": project(&row) })),
        Err(e) => admin_error(&e),
    }
}

/// `PATCH /api/webhooks/{id}` — present fields are applied, absent fields are left alone.
pub async fn update(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    body: Body,
) -> Response {
    let id = match parse_id(&id) {
        Ok(id) => id,
        Err(e) => return admin_error(&e),
    };
    let value = match body_json(body).await {
        Ok(v) => v,
        Err(resp) => return *resp,
    };
    let mut patch = WebhookPatch::default();
    if value.get("url").is_some() {
        match str_field(&value, "url") {
            Some(url) => patch.url = Some(url),
            None => return admin_error(&Error::BadRequest("url must be a string".into())),
        }
    }
    if value.get("event_types").is_some() {
        match parse_event_types(&value) {
            Ok(types) => patch.event_types = Some(types),
            Err(e) => return admin_error(&e),
        }
    }
    if value.get("secret_env").is_some() {
        match str_field(&value, "secret_env") {
            Some(name) => patch.secret_env = Some(Some(name)),
            None => {
                return admin_error(&Error::BadRequest(
                    "secret_env cannot be cleared: deliveries are always signed".into(),
                ))
            }
        }
    }
    if value.get("enabled").is_some() {
        match value.get("enabled").and_then(Value::as_bool) {
            Some(enabled) => patch.enabled = Some(enabled),
            None => return admin_error(&Error::BadRequest("enabled must be a boolean".into())),
        }
    }
    match state
        .store
        .update_webhook(id, &patch, crate::now_unix())
        .await
    {
        Ok(Some(row)) => ok_json(json!({ "webhook": project(&row) })),
        Ok(None) => admin_error(&Error::NotFound(format!("webhook {id}"))),
        Err(e) => admin_error(&e),
    }
}

/// `DELETE /api/webhooks/{id}`
pub async fn remove(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    let id = match parse_id(&id) {
        Ok(id) => id,
        Err(e) => return admin_error(&e),
    };
    match state.store.delete_webhook(id).await {
        Ok(true) => ok_json(json!({ "deleted": true, "id": id.to_string() })),
        Ok(false) => admin_error(&Error::NotFound(format!("webhook {id}"))),
        Err(e) => admin_error(&e),
    }
}

/// `GET /api/webhooks/status` — queue depth and recent attempts, never payloads.
pub async fn status(State(state): State<Arc<AppState>>) -> Response {
    let counts = match state.store.outbox_status().await {
        Ok(counts) => counts,
        Err(e) => return admin_error(&e),
    };
    let recent = match state.store.recent_events(RECENT_LIMIT).await {
        Ok(rows) => rows,
        Err(e) => return admin_error(&e),
    };
    let recent: Vec<Value> = recent
        .iter()
        .map(|row| {
            json!({
                "id": row.id,
                "type": row.event_type,
                "status": row.status,
                "attempts": row.attempts,
                "created_at": row.created_at,
                "next_attempt_at": row.next_attempt_at,
                "last_error": row.last_error,
                "delivered_at": row.delivered_at,
            })
        })
        .collect();
    ok_json(json!({
        "outbox": {
            "pending": counts.pending,
            "delivering": counts.delivering,
            "delivered": counts.delivered,
            // Terminal, but nothing was sent: never folded into `delivered`.
            "unmatched": counts.unmatched,
            "dead": counts.dead,
        },
        "recent": recent,
    }))
}
