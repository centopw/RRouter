//! `/api/keys` — gateway API key (virtual key) management.
//!
//! A key carries the limits an operator sells: requests and tokens per minute, a
//! lifetime token budget, a model allowlist, and an expiry. `0` / `null` means
//! unlimited, so a freshly created key behaves exactly as before the phase-1 schema.
//!
//! A key also owns a model mapping (`/api/keys/{id}/models`): the client keeps a stable
//! model name and the operator repoints it for that key alone. Validation is here, not
//! in the store, because "resolves to a known provider" is a routing fact.

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::response::Response;
use rrouter_core::Error;
use rrouter_store::{window_start, ApiKey, ApiKeyPatch, BudgetWindow, KeyModel};
use serde_json::{json, Value};
use uuid::Uuid;

use super::{admin_error, body_json, ok_json};
use crate::state::AppState;

/// The row shape shared by `GET /api/keys`, `POST` and `PATCH`. `spent_tokens` and
/// `spent_micros` are durable SQL sums over `usage` for this key, scoped to its
/// `budget_window`, not stored counters.
fn row(key: &ApiKey, spent: u64, spent_micros: u64, models: &[KeyModel]) -> Value {
    json!({
        "id": key.id.to_string(),
        "name": key.name,
        "created_at": key.created_at,
        "is_active": key.is_active,
        "rpm": key.rpm,
        "tpm": key.tpm,
        "budget_tokens": key.budget_tokens,
        "budget_window": key.budget_window.as_str(),
        "spent_tokens": spent,
        "budget_micros": key.budget_micros,
        "spent_micros": spent_micros,
        "allowed_models": key.allowed_models,
        "expires_at": key.expires_at,
        "models": models_value(models),
    })
}

/// `[{"from": …, "to": …}]`, the shape both the keys list and the mapping endpoint use.
fn models_value(models: &[KeyModel]) -> Value {
    Value::Array(
        models
            .iter()
            .map(|m| json!({ "from": m.from, "to": m.to }))
            .collect(),
    )
}

async fn view(state: &AppState, key: &ApiKey) -> Value {
    let since = window_start(key.budget_window, crate::now_unix());
    let spent = state.store.api_key_spend(key.id, since).await.unwrap_or(0);
    let spent_micros = state.store.api_key_cost_spend(key.id, since).await.unwrap_or(0);
    let models = state.store.key_models(key.id).await.unwrap_or_default();
    row(key, spent, spent_micros, &models)
}

pub async fn list(State(state): State<Arc<AppState>>) -> Response {
    let keys = match state.store.list_api_keys().await {
        Ok(keys) => keys,
        Err(e) => return admin_error(&e),
    };
    // Each key is measured over its own window, so the spend is one aggregate per
    // distinct window start (at most four), never one query per key.
    let now = crate::now_unix();
    let mut spend: HashMap<Option<i64>, HashMap<Uuid, u64>> = HashMap::new();
    let mut cost: HashMap<Option<i64>, HashMap<Uuid, u64>> = HashMap::new();
    for key in &keys {
        let since = window_start(key.budget_window, now);
        if spend.contains_key(&since) {
            continue;
        }
        match state.store.api_key_spend_all(since).await {
            Ok(s) => spend.insert(since, s),
            Err(e) => return admin_error(&e),
        };
        match state.store.api_key_cost_spend_all(since).await {
            Ok(c) => cost.insert(since, c),
            Err(e) => return admin_error(&e),
        };
    }
    // One query for every key's mapping, so the list stays O(1) in statement count.
    let models = match state.store.key_models_all().await {
        Ok(models) => models,
        Err(e) => return admin_error(&e),
    };
    let data: Vec<Value> = keys
        .iter()
        .map(|k| {
            let since = window_start(k.budget_window, now);
            row(
                k,
                spend.get(&since).and_then(|m| m.get(&k.id)).copied().unwrap_or(0),
                cost.get(&since).and_then(|m| m.get(&k.id)).copied().unwrap_or(0),
                models.get(&k.id).map(Vec::as_slice).unwrap_or_default(),
            )
        })
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
/// `budget_micros`, `budget_window` (`total`, `daily`, `weekly`, `monthly`),
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

/// `GET /api/keys/{id}/models` — the key's model mapping, in `from` order.
pub async fn list_models(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let Ok(uuid) = uuid::Uuid::parse_str(&id) else {
        return admin_error(&Error::BadRequest("key id must be a uuid".into()));
    };
    match state.store.get_api_key(uuid).await {
        Ok(Some(_)) => {}
        Ok(None) => return admin_error(&Error::NotFound(format!("key {uuid}"))),
        Err(e) => return admin_error(&e),
    }
    match state.store.key_models(uuid).await {
        Ok(models) => ok_json(json!({ "data": models_value(&models) })),
        Err(e) => admin_error(&e),
    }
}

/// `PUT /api/keys/{id}/models` — replace the whole mapping atomically; an empty list
/// clears it. Every `to` must be a `provider/model` ref for a registered provider,
/// because a mapping that cannot route is worse than no mapping at all.
pub async fn put_models(
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
    let models = match models_from(&value) {
        Ok(models) => models,
        Err(e) => return admin_error(&e),
    };
    match state.store.replace_key_models(uuid, &models).await {
        Ok(true) => ok_json(json!({ "data": models_value(&models) })),
        Ok(false) => admin_error(&Error::NotFound(format!("key {uuid}"))),
        Err(e) => admin_error(&e),
    }
}

/// Decode `{"models":[{"from","to"}]}` and reject anything that could not route.
fn models_from(value: &Value) -> Result<Vec<KeyModel>, Error> {
    let items = value
        .get("models")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::BadRequest("body must be {\"models\": [...]}".into()))?;
    let mut out: Vec<KeyModel> = Vec::with_capacity(items.len());
    for item in items {
        let from = item
            .get("from")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| Error::BadRequest("each mapping needs a non-empty \"from\"".into()))?;
        let to = item
            .get("to")
            .and_then(Value::as_str)
            .map(str::trim)
            .ok_or_else(|| Error::BadRequest("each mapping needs a \"to\" ref".into()))?;
        let (provider, model) = to.split_once('/').ok_or_else(|| {
            Error::BadRequest(format!("\"to\" must be provider/model, got {to:?}"))
        })?;
        if model.is_empty() || rrouter_auth::get(provider).is_none() {
            return Err(Error::BadRequest(format!(
                "\"to\" must name a registered provider and a model, got {to:?}"
            )));
        }
        if out.iter().any(|m| m.from == from) {
            return Err(Error::BadRequest(format!("duplicate mapping for {from:?}")));
        }
        out.push(KeyModel { from: from.to_string(), to: to.to_string() });
    }
    Ok(out)
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
            "budget_window" => {
                let raw = raw.as_str().unwrap_or_default();
                patch.budget_window = Some(BudgetWindow::parse(raw).ok_or_else(|| {
                    Error::BadRequest(
                        "budget_window must be one of total, daily, weekly, monthly".into(),
                    )
                })?);
            }
            "tpm" => patch.tpm = Some(u32_field(raw, "tpm")?),
            "budget_tokens" => patch.budget_tokens = Some(uint(raw, "budget_tokens")?),
            // Micro-USD. An integer field: `null` is a type error, `0` means unlimited.
            "budget_micros" => patch.budget_micros = Some(uint(raw, "budget_micros")?),
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
