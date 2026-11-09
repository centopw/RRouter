//! `/api/keys` — gateway API key (virtual key) management.
//!
//! A key carries the limits an operator sells: requests and tokens per minute, a
//! lifetime token budget, a model allowlist, and an expiry. `0` / `null` means
//! unlimited, so a freshly created key behaves exactly as before the phase-1 schema.
//!
//! A key also has a `scope`: `inference` (spend through `/v1/*`, the default) or
//! `admin` (administer this API and `/metrics`, refused by `/v1/*`). Scope is separate
//! from the limits so administration never implies spend.
//!
//! A key also owns a model mapping (`/api/keys/{id}/models`): the client keeps a stable
//! model name and the operator repoints it for that key alone. Validation is here, not
//! in the store, because "resolves to a known provider" is a routing fact.

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::Response;
use rrouter_core::Error;
use rrouter_store::{
    window_start, ApiKey, ApiKeyPatch, BudgetWindow, KeyModel, KeyScope, MAX_API_KEY_NAME,
};
use serde_json::{json, Value};
use uuid::Uuid;

use super::{admin_error, body_json, ok_json};
use crate::state::AppState;

/// The row shape shared by `GET /api/keys`, `POST` and `PATCH`. `spent_tokens` and
/// `spent_micros` are durable SQL sums over `usage` for this key, scoped to its
/// `budget_window`, not stored counters.
fn row(
    key: &ApiKey,
    spent: u64,
    spent_micros: u64,
    models: &[KeyModel],
    team_id: Option<Uuid>,
) -> Value {
    json!({
        "id": key.id.to_string(),
        "name": key.name,
        "created_at": key.created_at,
        "is_active": key.is_active,
        "scope": key.scope.as_str(),
        "rpm": key.rpm,
        "tpm": key.tpm,
        "budget_tokens": key.budget_tokens,
        "budget_window": key.budget_window.as_str(),
        "spent_tokens": spent,
        "budget_micros": key.budget_micros,
        "spent_micros": spent_micros,
        "allowed_models": key.allowed_models,
        "expires_at": key.expires_at,
        "guardrails": key.guardrails,
        "team_id": team_id.map(|team| team.to_string()),
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
    let team_id = state.store.key_team(key.id).await.ok().flatten();
    let since = window_start(key.budget_window, crate::now_unix());
    let spent = state.store.api_key_spend(key.id, since).await.unwrap_or(0);
    let spent_micros = state
        .store
        .api_key_cost_spend(key.id, since)
        .await
        .unwrap_or(0);
    let models = state.store.key_models(key.id).await.unwrap_or_default();
    row(key, spent, spent_micros, &models, team_id)
}

/// `GET /api/keys[?team_id=]` — every key, or one team's. The filter is what a team
/// report uses, and it can never widen a caller's authority: the route is admin-only.
pub async fn list(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(query): axum::extract::Query<Vec<(String, String)>>,
) -> Response {
    let team = query
        .iter()
        .find(|(key, _)| key == "team_id")
        .map(|(_, value)| value.trim())
        .filter(|value| !value.is_empty());
    let keys = match team {
        None => state.store.list_api_keys().await,
        Some(raw) => match uuid::Uuid::parse_str(raw) {
            Ok(team) => state.store.list_api_keys_in_team(team).await,
            Err(_) => return admin_error(&Error::BadRequest("team_id must be a uuid".into())),
        },
    };
    let keys = match keys {
        Ok(keys) => keys,
        Err(e) => return admin_error(&e),
    };
    match project(&state, &keys).await {
        Ok(data) => ok_json(json!({ "data": data })),
        Err(e) => admin_error(&e),
    }
}

/// The row projection shared by the admin list and the team-scoped list.
///
/// Spend is one aggregate per distinct window start (at most four), never one query per
/// key; mappings and tenancy are one query each. `row()` is the only shape either caller
/// emits, so a new field cannot appear on one list and not the other.
async fn project(state: &AppState, keys: &[ApiKey]) -> Result<Vec<Value>, Error> {
    let now = crate::now_unix();
    let mut spend: HashMap<Option<i64>, HashMap<Uuid, u64>> = HashMap::new();
    let mut cost: HashMap<Option<i64>, HashMap<Uuid, u64>> = HashMap::new();
    for key in keys {
        let since = window_start(key.budget_window, now);
        if spend.contains_key(&since) {
            continue;
        }
        spend.insert(since, state.store.api_key_spend_all(since).await?);
        cost.insert(since, state.store.api_key_cost_spend_all(since).await?);
    }
    let models = state.store.key_models_all().await?;
    // Tenancy in one more query: a key absent from the map is instance-wide.
    let teams = state.store.key_teams().await?;
    Ok(keys
        .iter()
        .map(|k| {
            let since = window_start(k.budget_window, now);
            row(
                k,
                spend
                    .get(&since)
                    .and_then(|m| m.get(&k.id))
                    .copied()
                    .unwrap_or(0),
                cost.get(&since)
                    .and_then(|m| m.get(&k.id))
                    .copied()
                    .unwrap_or(0),
                models.get(&k.id).map(Vec::as_slice).unwrap_or_default(),
                teams.get(&k.id).copied().flatten(),
            )
        })
        .collect())
}

/// `GET /api/teams/{id}/keys` — one team's key rows, in the same projection as the admin
/// list. The caller's authority is checked by `routes::teams`; the plaintext and the hash
/// are not part of `row()` at all.
pub(crate) async fn rows_for_team(state: &AppState, team_id: Uuid) -> Result<Vec<Value>, Error> {
    let keys = state.store.list_api_keys_in_team(team_id).await?;
    project(state, &keys).await
}

/// Creates a key and returns the plaintext **once**; only its hash is stored.
///
/// The new key starts unlimited; every limit is set afterwards with `PATCH`.
pub async fn create(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    peer: crate::auth::Peer,
    body: Body,
) -> Response {
    let value = match body_json(body).await {
        Ok(v) => v,
        Err(resp) => return *resp,
    };
    let name = super::models::str_field(&value, "name");
    // A create has no assignment to preserve: absent and `null` both mean instance-wide.
    let team_id = match cut_team_id(&value) {
        Ok(team) => team.flatten(),
        Err(e) => return admin_error(&e),
    };
    match state
        .store
        .create_api_key_in_team(name.as_deref(), team_id)
        .await
    {
        Ok((plaintext, key)) => {
            // The plaintext is returned once and never recorded: the audit row names the
            // key and its scope, nothing that authenticates.
            let actor = super::auth::actor(&state, &headers).await;
            super::auth::audit(
                &state,
                "key_created",
                Some(&actor),
                &crate::auth::client_ip(&headers, peer.0),
                &format!(
                    "key {} · name {} · scope {}{}",
                    key.id,
                    key.name.as_deref().unwrap_or("—"),
                    key.scope.as_str(),
                    team_id
                        .map(|team| format!(" · team {team}"))
                        .unwrap_or_default()
                ),
            )
            .await;
            ok_json(json!({
                "key": plaintext,
                "id": key.id.to_string(),
                "name": key.name,
                "created_at": key.created_at,
                "notice": "store this key now; it cannot be shown again",
            }))
        }
        Err(e) => admin_error(&e),
    }
}

/// `PATCH /api/keys/{id}` — any subset of `is_active`, `name` (trimmed; null or blank
/// clears; 100 characters at most), `scope` (`inference` or `admin`), `rpm`, `tpm`,
/// `budget_tokens`, `budget_micros`, `budget_window` (`total`, `daily`, `weekly`,
/// `monthly`), `allowed_models` (null clears), `expires_at` (null clears), `guardrails`
/// (null clears; `{}` is the same as no policy). Returns the merged row.
pub async fn update(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    peer: crate::auth::Peer,
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
    // Tenancy is applied separately: it is not traffic policy, and a body that only moves
    // a key between teams must not rewrite its limits. `null` releases the key back to
    // instance-wide, which is what every pre-v23 key was.
    let team_change = match cut_team_id(&value) {
        Ok(team) => team,
        Err(e) => return admin_error(&e),
    };
    // Validate the destination before writing anything: a patch that names a missing team
    // must not leave the limits half-applied.
    if let Some(Some(team)) = team_change {
        match state.store.team(team).await {
            Ok(Some(_)) => {}
            Ok(None) => return admin_error(&Error::NotFound(format!("team {team}"))),
            Err(e) => return admin_error(&e),
        }
    }
    match state.store.update_api_key(uuid, &patch).await {
        Ok(Some(key)) => {
            if let Some(team) = team_change {
                match state.store.set_api_key_team(uuid, team).await {
                    Ok(true) => {}
                    Ok(false) => return admin_error(&Error::NotFound(format!("key {uuid}"))),
                    Err(e) => return admin_error(&e),
                }
            }
            // Which policy fields moved, never their secret values; scope and window
            // carry the new value because those two decide what the key may do.
            let actor = super::auth::actor(&state, &headers).await;
            let mut detail = format!(
                "key {} · {}",
                key.id,
                describe_patch(&patch, key.scope, key.budget_window)
            );
            match team_change {
                Some(Some(team)) => detail.push_str(&format!(" · team {team}")),
                Some(None) => detail.push_str(" · team cleared"),
                None => {}
            }
            super::auth::audit(
                &state,
                "key_updated",
                Some(&actor),
                &crate::auth::client_ip(&headers, peer.0),
                &detail,
            )
            .await;
            ok_json(view(&state, &key).await)
        }
        Ok(None) => admin_error(&Error::NotFound(format!("key {uuid}"))),
        Err(e) => admin_error(&e),
    }
}

pub async fn remove(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    peer: crate::auth::Peer,
    Path(id): Path<String>,
) -> Response {
    let Ok(uuid) = uuid::Uuid::parse_str(&id) else {
        return admin_error(&Error::BadRequest("key id must be a uuid".into()));
    };
    // Read the name before the row goes: the audit row should outlive it.
    let name = state
        .store
        .get_api_key(uuid)
        .await
        .ok()
        .flatten()
        .and_then(|key| key.name);
    match state.store.delete_api_key(uuid).await {
        Ok(true) => {
            // A deleted key's windows must not survive into a future id.
            state.counters.forget(uuid);
            let actor = super::auth::actor(&state, &headers).await;
            super::auth::audit(
                &state,
                "key_deleted",
                Some(&actor),
                &crate::auth::client_ip(&headers, peer.0),
                &format!("key {uuid} · name {}", name.as_deref().unwrap_or("—")),
            )
            .await;
            ok_json(json!({ "deleted": true, "id": uuid.to_string() }))
        }
        Ok(false) => admin_error(&Error::NotFound(format!("key {uuid}"))),
        Err(e) => admin_error(&e),
    }
}

/// Name the policy fields a patch touches, for the audit row.
///
/// Only names, never values: a budget's amount is not secret but a diff of eight numbers
/// is unreadable, while "scope, budget_micros" answers the question an audit asks. Scope
/// and window do carry their new value because those decide what the key may do and when
/// its spend resets.
fn describe_patch(patch: &ApiKeyPatch, scope: KeyScope, window: BudgetWindow) -> String {
    let mut fields: Vec<&str> = Vec::new();
    for (present, name) in [
        (patch.is_active.is_some(), "is_active"),
        (patch.name.is_some(), "name"),
        (patch.scope.is_some(), "scope"),
        (patch.rpm.is_some(), "rpm"),
        (patch.tpm.is_some(), "tpm"),
        (patch.budget_tokens.is_some(), "budget_tokens"),
        (patch.budget_micros.is_some(), "budget_micros"),
        (patch.budget_window.is_some(), "budget_window"),
        (patch.allowed_models.is_some(), "allowed_models"),
        (patch.expires_at.is_some(), "expires_at"),
        (patch.guardrails.is_some(), "guardrails"),
    ] {
        if present {
            fields.push(name);
        }
    }
    let changed = if fields.is_empty() {
        "no changes".to_string()
    } else {
        fields.join(", ")
    };
    format!(
        "{changed} · now scope {} window {}",
        scope.as_str(),
        window.as_str()
    )
}

/// `GET /api/keys/{id}/models` — the key's model mapping, in `from` order.
pub async fn list_models(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
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
        out.push(KeyModel {
            from: from.to_string(),
            to: to.to_string(),
        });
    }
    Ok(out)
}

/// Read `team_id` out of a key body, distinguishing "not mentioned" from "clear it".
///
/// `None` leaves the assignment alone (the PATCH contract for absent fields), `Some(None)`
/// releases the key to instance-wide, and `Some(Some(id))` moves it to that team.
fn cut_team_id(value: &Value) -> Result<Option<Option<Uuid>>, Error> {
    let Some(raw) = value.get("team_id") else {
        return Ok(None);
    };
    if raw.is_null() {
        return Ok(Some(None));
    }
    let Some(text) = raw.as_str() else {
        return Err(Error::BadRequest("team_id must be a uuid or null".into()));
    };
    match Uuid::parse_str(text) {
        Ok(id) => Ok(Some(Some(id))),
        Err(_) => Err(Error::BadRequest("team_id must be a uuid or null".into())),
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
                patch.is_active = Some(
                    raw.as_bool()
                        .ok_or_else(|| Error::BadRequest("is_active must be a boolean".into()))?,
                );
            }
            "name" => patch.name = Some(name_field(raw)?),

            "rpm" => patch.rpm = Some(u32_field(raw, "rpm")?),
            "scope" => {
                let raw = raw.as_str().unwrap_or_default();
                patch.scope =
                    Some(KeyScope::parse(raw).ok_or_else(|| {
                        Error::BadRequest("scope must be inference or admin".into())
                    })?);
            }
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
            "guardrails" => {
                patch.guardrails = Some(match raw {
                    Value::Null => None,
                    // `deny_unknown_fields` on the type is deliberate: a misspelled
                    // policy field must be a 400, not a silently unfiltered key.
                    other => Some(
                        serde_json::from_value(other.clone())
                            .map_err(|e| Error::BadRequest(format!("guardrails: {e}")))?,
                    ),
                });
            }
            // The dashboard sends the full row back on save; unknown fields are ignored
            // so a newer UI cannot break an older backend.
            _ => {}
        }
    }
    Ok(patch)
}

/// A key name: `null` clears it, a string is trimmed and capped. The store applies the
/// same rule before writing, so a direct library caller cannot store a name the API
/// would refuse to update.
fn name_field(raw: &Value) -> Result<Option<String>, Error> {
    match raw {
        Value::Null => Ok(None),
        Value::String(name) => {
            let name = name.trim();
            if name.chars().count() > MAX_API_KEY_NAME {
                return Err(Error::BadRequest(format!(
                    "name must be at most {MAX_API_KEY_NAME} characters"
                )));
            }
            Ok(Some(name.to_string()))
        }
        _ => Err(Error::BadRequest("name must be a string or null".into())),
    }
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
    u32::try_from(raw).map_err(|_| Error::BadRequest(format!("{name} must fit in 32 bits")))
}

fn i64_field(value: &Value, name: &str) -> Result<i64, Error> {
    value
        .as_i64()
        .ok_or_else(|| Error::BadRequest(format!("{name} must be an integer or null")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A rename is a trimmed string; `null` and a blank string both mean "no name"
    /// (the store folds the empty string to `None` when it merges the row).
    #[test]
    fn name_patch_trims_and_clears() {
        let renamed = patch_from(&json!({"name": "  production key  "})).expect("patch");
        assert_eq!(renamed.name, Some(Some("production key".to_string())));

        let cleared = patch_from(&json!({"name": null})).expect("patch");
        assert_eq!(cleared.name, Some(None));

        let blank = patch_from(&json!({"name": "   "})).expect("patch");
        assert_eq!(blank.name, Some(Some(String::new())));
    }

    /// The cap counts characters, not bytes, and is applied after trimming.
    #[test]
    fn name_patch_caps_length_in_chars() {
        let at_limit = "é".repeat(rrouter_store::MAX_API_KEY_NAME);
        let patch = patch_from(&json!({ "name": at_limit })).expect("100 characters fit");
        assert_eq!(patch.name, Some(Some(at_limit)));

        let padded = format!("  {}  ", "x".repeat(rrouter_store::MAX_API_KEY_NAME));
        assert!(
            patch_from(&json!({ "name": padded })).is_ok(),
            "padding is trimmed before the count"
        );

        let over = "x".repeat(rrouter_store::MAX_API_KEY_NAME + 1);
        let error = patch_from(&json!({ "name": over })).expect_err("101 characters");
        assert_eq!(error.status(), 400);
        assert!(
            error.to_string().contains("100"),
            "names the limit: {error}"
        );
    }

    /// Anything that is not a string or `null` is a client error, not a silent no-op.
    #[test]
    fn name_patch_rejects_a_non_string() {
        for bad in [json!(7), json!(true), json!(["a"]), json!({"n": "x"})] {
            let error = patch_from(&json!({ "name": bad })).expect_err("not a string");
            assert_eq!(error.status(), 400);
            assert!(
                error.to_string().contains("string or null"),
                "explains the type: {error}"
            );
        }
    }

    /// The audit detail names `name` only when the patch carries it.
    #[test]
    fn describe_patch_names_a_rename() {
        let patch = ApiKeyPatch {
            name: Some(Some("x".to_string())),
            ..Default::default()
        };
        let detail = describe_patch(&patch, KeyScope::Inference, BudgetWindow::Total);
        assert!(detail.starts_with("name ·"), "{detail}");

        let unrelated = ApiKeyPatch {
            rpm: Some(1),
            ..Default::default()
        };
        let detail = describe_patch(&unrelated, KeyScope::Inference, BudgetWindow::Total);
        let fields = detail.split('·').next().unwrap_or_default();
        assert!(!fields.contains("name"), "{detail}");
    }

    /// Omitting the field is not a clear: the audit row and the SQL must see no change.
    #[test]
    fn an_unrelated_patch_leaves_the_name_absent() {
        let patch = patch_from(&json!({"rpm": 10})).expect("patch");
        assert_eq!(patch.name, None);
        assert_eq!(patch.rpm, Some(10));
    }
}
