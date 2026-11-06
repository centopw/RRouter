//! `/api/keys` — gateway API key (virtual key) management.
//!
//! A key carries the limits an operator sells: requests and tokens per minute, a
//! lifetime token budget, a model allowlist, and an expiry. `0` / `null` means
//! unlimited, so a freshly created key behaves exactly as before the phase-1 schema.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::response::Response;
use rrouter_core::Error;
use rrouter_store::{ApiKey, ApiKeyPatch};
use serde_json::{json, Value};

use super::{admin_error, body_json, ok_json};
use crate::state::AppState;

/// The row shape shared by `GET /api/keys`, `POST` and `PATCH`. `spent_tokens` is the
/// durable SQL sum over `usage` for this key, not a stored counter.
fn row(key: &ApiKey, spent: u64) -> Value {
    json!({
        "id": key.id.to_string(),
        "name": key.name,
        "created_at": key.created_at,
        "is_active": key.is_active,
        "rpm": key.rpm,
        "tpm": key.tpm,
        "budget_tokens": key.budget_tokens,
        "spent_tokens": spent,
        "allowed_models": key.allowed_models,
        "expires_at": key.expires_at,
    })
}

async fn view(state: &AppState, key: &ApiKey) -> Value {
    let spent = state.store.api_key_spend(key.id).await.unwrap_or(0);
    row(key, spent)
}

pub async fn list(State(state): State<Arc<AppState>>) -> Response {
    let keys = match state.store.list_api_keys().await {
        Ok(keys) => keys,
        Err(e) => return admin_error(&e),
    };
    let spend = match state.store.api_key_spend_all().await {
        Ok(spend) => spend,
        Err(e) => return admin_error(&e),
    };
    let data: Vec<Value> = keys
        .iter()
        .map(|k| row(k, spend.get(&k.id).copied().unwrap_or(0)))
        .collect();
    ok_json(json!({ "data": data }))
}

/// Creates a key and returns the plaintext **once**; only its hash is stored.
///
/// The new key starts unlimited; every limit is set afterwards with `PATCH`.
pub async fn create(State(state): State<Arc<AppState>>, body: Body) -> Response {
    let value = match body_json(body).await {
        Ok(v) => v,
        Err(resp) => return *resp,
    };
    let name = super::models::str_field(&value, "name");
    match state.store.create_api_key(name.as_deref()).await {
        Ok((plaintext, key)) => ok_json(json!({
            "key": plaintext,
            "id": key.id.to_string(),
            "name": key.name,
            "created_at": key.created_at,
            "notice": "store this key now; it cannot be shown again",
        })),
        Err(e) => admin_error(&e),
    }
}

/// `PATCH /api/keys/{id}` — any subset of `is_active`, `rpm`, `tpm`, `budget_tokens`,
/// `allowed_models` (null clears), `expires_at` (null clears). Returns the merged row.
pub async fn update(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    body: Body,
) -> Response {
    let Ok(uuid) = uuid::Uuid::parse_str(&id) else {
        return admin_error(&Error::BadRequest("key id must be a uuid".into()));
    };
    let value = match body_json(body).await {
        Ok(v) => v,
        Err(resp) => return *resp,
    };
    let patch = match patch_from(&value) {
        Ok(patch) => patch,
        Err(e) => return admin_error(&e),
    };
    match state.store.update_api_key(uuid, &patch).await {
        Ok(Some(key)) => ok_json(view(&state, &key).await),
        Ok(None) => admin_error(&Error::NotFound(format!("key {uuid}"))),
        Err(e) => admin_error(&e),
    }
}

pub async fn remove(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let Ok(uuid) = uuid::Uuid::parse_str(&id) else {
        return admin_error(&Error::BadRequest("key id must be a uuid".into()));
    };
    match state.store.delete_api_key(uuid).await {
        Ok(true) => {
            // A deleted key's windows must not survive into a future id.
            state.counters.forget(uuid);
            ok_json(json!({ "deleted": true, "id": uuid.to_string() }))
        }
        Ok(false) => admin_error(&Error::NotFound(format!("key {uuid}"))),
        Err(e) => admin_error(&e),
    }
}

/// Decode a PATCH body: absent fields are left alone, an explicit `null` clears a
/// nullable one, and a wrong type is a client error rather than a silently ignored
/// change.
fn patch_from(value: &Value) -> Result<ApiKeyPatch, Error> {
    let Some(object) = value.as_object() else {
        if value.is_null() {
            return Ok(ApiKeyPatch::default());
        }
        return Err(Error::BadRequest("patch body must be a JSON object".into()));
    };
    let mut patch = ApiKeyPatch::default();
    for (name, raw) in object {
        match name.as_str() {
            "is_active" => {
                patch.is_active = Some(raw.as_bool().ok_or_else(|| {
                    Error::BadRequest("is_active must be a boolean".into())
                })?);
            }
            "rpm" => patch.rpm = Some(u32_field(raw, "rpm")?),
            "tpm" => patch.tpm = Some(u32_field(raw, "tpm")?),
            "budget_tokens" => patch.budget_tokens = Some(uint(raw, "budget_tokens")?),
            "allowed_models" => {
                patch.allowed_models = Some(match raw {
                    Value::Null => None,
                    Value::Array(items) => {
                        let mut models = Vec::with_capacity(items.len());
                        for item in items {
                            let model = item.as_str().ok_or_else(|| {
                                Error::BadRequest(
                                    "allowed_models must be an array of model refs".into(),
                                )
                            })?;
                            models.push(model.to_string());
                        }
                        Some(models)
                    }
                    _ => {
                        return Err(Error::BadRequest(
                            "allowed_models must be an array of model refs or null".into(),
                        ))
                    }
                });
            }
            "expires_at" => {
                patch.expires_at = Some(match raw {
                    Value::Null => None,
                    other => Some(i64_field(other, "expires_at")?),
                });
            }
            // The dashboard sends the full row back on save; unknown fields are ignored
            // so a newer UI cannot break an older backend.
            _ => {}
        }
    }
    Ok(patch)
}

/// Non-negative integer for a numeric cap.
fn uint(value: &Value, name: &str) -> Result<u64, Error> {
    value
        .as_u64()
        .ok_or_else(|| Error::BadRequest(format!("{name} must be a non-negative integer")))
}

/// Per-minute caps are `u32` in the schema; an out-of-range value is a client error
/// rather than a silent wrap to a much smaller limit.
fn u32_field(value: &Value, name: &str) -> Result<u32, Error> {
    let raw = uint(value, name)?;
    u32::try_from(raw)
        .map_err(|_| Error::BadRequest(format!("{name} must fit in 32 bits")))
}

fn i64_field(value: &Value, name: &str) -> Result<i64, Error> {
    value
        .as_i64()
        .ok_or_else(|| Error::BadRequest(format!("{name} must be an integer or null")))
}
