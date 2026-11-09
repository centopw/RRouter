//! `/api/model-presets` — the global preset library and each account's active choice.
//!
//! A preset is a named layout document (the dashboard's model board). Presets are global
//! and admin-managed; *which* preset a person is looking at is a per-account preference, so
//! activation lives on the session router (any signed-in account, CSRF-checked) while
//! create/edit/delete/layout stay admin-only. The *default* flag is library configuration,
//! so promoting a preset is admin-only too; the store demotes the previous default in the
//! same transaction. A caller with no dashboard session (admin
//! token, admin key, loopback) has no preference to store and resolves to the default.
//!
//! Hiding a model is a *view* concern: saving a layout never touches `disabled_models`, so
//! the routing surface is unchanged by anything on this page. Metadata edits never bump the
//! layout revision, and a layout write is guarded by the revision the client edited, so two
//! tabs cannot silently overwrite each other.
//!
//! Store contract (v30): `list_model_presets`, `get_model_preset`, `default_model_preset`,
//! `active_model_preset`, `create_model_preset`, `update_model_preset_meta`,
//! `update_model_preset_layout`, `delete_model_preset`, `duplicate_model_preset`,
//! `set_default_model_preset`, `set_active_model_preset`. Every mutation takes the acting account's label so the
//! snapshot history records who wrote a revision; this module passes the same label it puts
//! in the audit trail.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use rrouter_core::Error;
use rrouter_store::{LayoutDocument, ModelPreset, PresetUpdateOutcome};
use serde_json::{json, Value};
use uuid::Uuid;

use super::auth::{actor, audit, current_user};
use super::{admin_error, body_json, ok_json};
use crate::state::AppState;

/// One preset as the API returns it, with `is_default` taken from the stored flag.
fn preset_json(preset: &ModelPreset) -> Result<Value, Error> {
    let mut value = serde_json::to_value(preset)?;
    if let Some(object) = value.as_object_mut() {
        object.insert("id".into(), json!(preset.id.to_string()));
    }
    Ok(value)
}

/// The list row: the same object without the layout document — a picker needs names,
/// revisions and the size of each board, not every model on every board.
///
/// `entry_count` is every item in the document and `visible_count` the active ones, so a
/// selector can say "3 hidden" without loading each layout. Both are `0` for a document
/// with no explicit items (the seeded default merges the live catalog instead).
fn metadata_json(preset: &ModelPreset) -> Result<Value, Error> {
    let mut value = preset_json(preset)?;
    let entry_count = preset.layout.items.len();
    let visible_count = preset
        .layout
        .items
        .iter()
        .filter(|item| item.visible)
        .count();
    if let Some(object) = value.as_object_mut() {
        object.remove("layout");
        object.insert("entry_count".into(), json!(entry_count));
        object.insert("visible_count".into(), json!(visible_count));
    }
    Ok(value)
}

/// A preset error with a stable machine code; `/api` bodies are plain `{"error":{…}}`.
fn fail(status: StatusCode, code: &str, message: &str) -> Response {
    let kind = match status {
        StatusCode::NOT_FOUND => "not_found_error",
        StatusCode::CONFLICT => "conflict_error",
        StatusCode::FORBIDDEN => "permission_error",
        _ => "invalid_request_error",
    };
    (
        status,
        axum::Json(json!({ "error": { "message": message, "type": kind, "code": code } })),
    )
        .into_response()
}

/// `409 preset_conflict`: the client's `revision` is stale, and the body names the revision
/// it must re-read. Nothing is written on this path.
fn conflict(current_revision: i64) -> Response {
    (
        StatusCode::CONFLICT,
        axum::Json(json!({"error": {
            "message": format!(
                "layout revision {current_revision} is current; re-read the preset and retry"
            ),
            "type": "conflict_error",
            "code": "preset_conflict",
            "current_revision": current_revision,
        }})),
    )
        .into_response()
}

/// A path id, or a 404 naming what the client sent (never a nil UUID it did not).
///
/// The `Response` is boxed, like [`super::body_json`]: a `Response` is far larger than the
/// success type, and this is not a hot path.
fn id_or_404(raw: &str) -> Result<Uuid, Box<Response>> {
    Uuid::parse_str(raw).map_err(|_| {
        Box::new(fail(
            StatusCode::NOT_FOUND,
            "preset_not_found",
            &format!("model preset {raw}"),
        ))
    })
}

fn not_found(id: Uuid) -> Response {
    fail(
        StatusCode::NOT_FOUND,
        "preset_not_found",
        &format!("model preset {id}"),
    )
}

fn bad_request(message: &str) -> Response {
    fail(StatusCode::BAD_REQUEST, "invalid_request", message)
}

/// `layout` from a create body: absent means the compatibility default (empty items,
/// `auto_include_new`), invalid is a 400 naming the field.
///
/// The document is deserialized into the store's own type, so a field the store gains
/// later (`last_visible_position`, `hidden_at`, …) needs no change here: serde ignores an
/// unknown key today and round-trips it through GET and PUT once the type carries it.
fn layout_field(value: &Value) -> Result<LayoutDocument, Box<Response>> {
    match value.get("layout") {
        None | Some(Value::Null) => Ok(LayoutDocument::default()),
        Some(layout) => {
            let document: LayoutDocument = serde_json::from_value(layout.clone()).map_err(|e| {
                Box::new(fail(
                    StatusCode::BAD_REQUEST,
                    "layout_invalid",
                    &format!("layout is not a valid layout document: {e}"),
                ))
            })?;
            // The version is a compatibility fact, not a shape the client can choose: a
            // document this build cannot read is a client error with a name, not a blank
            // store message.
            if document.schema_version != rrouter_store::LAYOUT_SCHEMA_VERSION {
                return Err(Box::new(fail(
                    StatusCode::BAD_REQUEST,
                    "layout_invalid",
                    &format!(
                        "layout schema_version must be {}",
                        rrouter_store::LAYOUT_SCHEMA_VERSION
                    ),
                )));
            }
            Ok(document)
        }
    }
}

/// `GET /api/model-presets` — metadata for every preset, plus the caller's active id.
pub async fn list(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let user = current_user(&state, &headers).await;
    let presets = match state.store.list_model_presets().await {
        Ok(presets) => presets,
        Err(e) => return admin_error(&e),
    };
    let default_id = match state.store.default_model_preset().await {
        Ok(default) => default.map(|preset| preset.id),
        Err(e) => return admin_error(&e),
    };
    let active_id = match state
        .store
        .active_model_preset(user.as_ref().map(|user| user.id))
        .await
    {
        Ok(active) => Some(active.id),
        // A database with no default is a storage problem, not an empty list: the store
        // already says so, and pretending the list is empty would hide it.
        Err(Error::Store(_)) if default_id.is_none() => default_id,
        Err(e) => return admin_error(&e),
    };

    let mut data = Vec::with_capacity(presets.len());
    for preset in &presets {
        match metadata_json(preset) {
            Ok(value) => data.push(value),
            Err(e) => return admin_error(&e),
        }
    }
    ok_json(json!({
        "data": data,
        "active_id": active_id.map(|id| id.to_string()),
        "default_id": default_id.map(|id| id.to_string()),
    }))
}

/// `GET /api/model-presets/active` — the preset this caller is looking at, resolved.
pub async fn active(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let user = current_user(&state, &headers).await;
    match state
        .store
        .active_model_preset(user.as_ref().map(|user| user.id))
        .await
    {
        Ok(preset) => match preset_json(&preset) {
            Ok(value) => ok_json(value),
            Err(e) => admin_error(&e),
        },
        Err(e) => admin_error(&e),
    }
}

/// `GET /api/model-presets/{id}` — one preset, layout included.
pub async fn get(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    let id = match id_or_404(&id) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    match state.store.get_model_preset(id).await {
        Ok(Some(preset)) => match preset_json(&preset) {
            Ok(value) => ok_json(value),
            Err(e) => admin_error(&e),
        },
        Ok(None) => not_found(id),
        Err(e) => admin_error(&e),
    }
}

/// `POST /api/model-presets` — create a preset; admin only.
pub async fn create(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let value = match body_json(body).await {
        Ok(value) => value,
        Err(response) => return *response,
    };
    let Some(name) = super::models::str_field(&value, "name") else {
        return bad_request("name is required");
    };
    let description = super::models::str_field(&value, "description");
    let layout = match layout_field(&value) {
        Ok(layout) => layout,
        Err(response) => return *response,
    };
    let actor = actor(&state, &headers).await;
    match state
        .store
        .create_model_preset(&name, description.as_deref(), &layout, &actor)
        .await
    {
        Ok(created) => {
            audit(
                &state,
                "preset_created",
                Some(&actor),
                &crate::auth::client_ip(&headers, None),
                &format!("preset {} · name {}", created.id, created.name),
            )
            .await;
            match preset_json(&created) {
                Ok(value) => (StatusCode::CREATED, axum::Json(value)).into_response(),
                Err(e) => admin_error(&e),
            }
        }
        Err(e) => admin_error(&e),
    }
}

/// `PATCH /api/model-presets/{id}` — rename/re-describe; admin only.
///
/// An absent field is left alone, `null` clears the description, a string replaces it.
/// Neither touches the layout or its revision.
pub async fn update(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Body,
) -> Response {
    let id = match id_or_404(&id) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    let value = match body_json(body).await {
        Ok(value) => value,
        Err(response) => return *response,
    };
    let name = super::models::str_field(&value, "name");
    let description: Option<Option<String>> = match value.get("description") {
        None => None,
        Some(Value::Null) => Some(None),
        Some(Value::String(text)) => Some(Some(text.trim().to_string())),
        Some(_) => return bad_request("description must be a string or null"),
    };
    let actor = actor(&state, &headers).await;
    match state
        .store
        .update_model_preset_meta(
            id,
            name.as_deref(),
            description.as_ref().map(|text| text.as_deref()),
            &actor,
        )
        .await
    {
        Ok(Some(preset)) => {
            audit(
                &state,
                "preset_updated",
                Some(&actor),
                &crate::auth::client_ip(&headers, None),
                &format!("preset {id} · name {}", preset.name),
            )
            .await;
            match preset_json(&preset) {
                Ok(value) => ok_json(value),
                Err(e) => admin_error(&e),
            }
        }
        Ok(None) => not_found(id),
        Err(e) => admin_error(&e),
    }
}

/// `DELETE /api/model-presets/{id}` — admin only; the default preset is refused.
///
/// The refusal is checked here, before the delete, so the client gets a stable
/// `preset_is_default` code instead of a general bad request from the store.
pub async fn remove(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let id = match id_or_404(&id) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    match state.store.get_model_preset(id).await {
        Ok(Some(preset)) if preset.is_default => {
            return fail(
                StatusCode::CONFLICT,
                "preset_is_default",
                "the default model preset cannot be deleted; make another preset the \
                 default first",
            )
        }
        Ok(Some(_)) => {}
        Ok(None) => return not_found(id),
        Err(e) => return admin_error(&e),
    }
    let actor = actor(&state, &headers).await;
    match state.store.delete_model_preset(id, &actor).await {
        Ok(true) => {
            audit(
                &state,
                "preset_deleted",
                Some(&actor),
                &crate::auth::client_ip(&headers, None),
                &format!("preset {id}"),
            )
            .await;
            ok_json(json!({ "deleted": true, "id": id.to_string() }))
        }
        Ok(false) => not_found(id),
        Err(e) => admin_error(&e),
    }
}

/// `POST /api/model-presets/{id}/duplicate` — copy the layout under a new name; admin only.
pub async fn duplicate(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Body,
) -> Response {
    let id = match id_or_404(&id) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    let value = match body_json(body).await {
        Ok(value) => value,
        Err(response) => return *response,
    };
    let Some(name) = super::models::str_field(&value, "name") else {
        return bad_request("name is required");
    };
    let actor = actor(&state, &headers).await;
    match state.store.duplicate_model_preset(id, &name, &actor).await {
        Ok(copy) => {
            audit(
                &state,
                "preset_created",
                Some(&actor),
                &crate::auth::client_ip(&headers, None),
                &format!(
                    "preset {} · duplicated from {id} · name {}",
                    copy.id, copy.name
                ),
            )
            .await;
            match preset_json(&copy) {
                Ok(value) => (StatusCode::CREATED, axum::Json(value)).into_response(),
                Err(e) => admin_error(&e),
            }
        }
        Err(Error::NotFound(_)) => not_found(id),
        Err(e) => admin_error(&e),
    }
}

/// `POST /api/model-presets/{id}/activate` — this account's choice; session-scoped.
///
/// A dashboard session stores the preference. A token/key/loopback caller has no account to
/// store it on, so the call is a no-op that reports the default — the preset such a caller
/// already resolves to. Admin authority is not needed: this changes one preference, not the
/// library.
pub async fn activate(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let id = match id_or_404(&id) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    let Some(user) = current_user(&state, &headers).await else {
        return match state.store.active_model_preset(None).await {
            Ok(preset) => ok_json(json!({
                "active_id": preset.id.to_string(),
                "persisted": false,
            })),
            Err(e) => admin_error(&e),
        };
    };
    if !crate::auth::csrf_ok(&headers) {
        return fail(
            StatusCode::FORBIDDEN,
            "csrf_required",
            "the x-rrouter-csrf header is required for this request",
        );
    }
    match state.store.get_model_preset(id).await {
        Ok(Some(_)) => {}
        Ok(None) => return not_found(id),
        Err(e) => return admin_error(&e),
    }
    match state.store.set_active_model_preset(user.id, id).await {
        Ok(()) => {
            audit(
                &state,
                "preset_activated",
                Some(&user.email),
                &crate::auth::client_ip(&headers, None),
                &format!("preset {id} · account {}", user.email),
            )
            .await;
            ok_json(json!({ "active_id": id.to_string(), "persisted": true }))
        }
        Err(e) => admin_error(&e),
    }
}

/// `PUT /api/model-presets/{id}/layout` — replace the board; admin only.
///
/// `revision` is the revision the client edited. A mismatch is `409 preset_conflict` with
/// the current revision and **no write**, so two tabs cannot silently overwrite each other.
pub async fn save_layout(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Body,
) -> Response {
    let id = match id_or_404(&id) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    let value = match body_json(body).await {
        Ok(value) => value,
        Err(response) => return *response,
    };
    let Some(revision) = value.get("revision").and_then(Value::as_i64) else {
        return bad_request("revision is required");
    };
    let Some(layout) = value.get("layout") else {
        return bad_request("layout is required");
    };
    let layout = match layout_field(&json!({ "layout": layout })) {
        Ok(layout) => layout,
        Err(response) => return *response,
    };
    let actor = actor(&state, &headers).await;
    match state
        .store
        .update_model_preset_layout(id, revision, &layout, &actor)
        .await
    {
        Ok(PresetUpdateOutcome::Updated(preset)) => {
            audit(
                &state,
                "preset_layout_saved",
                Some(&actor),
                &crate::auth::client_ip(&headers, None),
                &format!("preset {id} · revision {}", preset.revision),
            )
            .await;
            match preset_json(&preset) {
                Ok(value) => ok_json(value),
                Err(e) => admin_error(&e),
            }
        }
        Ok(PresetUpdateOutcome::Conflict { current_revision }) => conflict(current_revision),
        Ok(PresetUpdateOutcome::NotFound) => not_found(id),
        Err(e) => admin_error(&e),
    }
}

/// `POST /api/model-presets/{id}/default` — promote a preset to the instance default.
///
/// Admin-only, like the rest of the library: the default is what a caller with no
/// dashboard preference (an admin token, a gateway key, loopback) resolves to. The store
/// demotes the previous default in the same transaction, so exactly one row carries the
/// flag; a layout revision is untouched because the flag is not part of the document.
pub async fn set_default(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let id = match id_or_404(&id) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    let actor = actor(&state, &headers).await;
    match state.store.set_default_model_preset(id, &actor).await {
        Ok(preset) => {
            audit(
                &state,
                "preset_default_set",
                Some(&actor),
                &crate::auth::client_ip(&headers, None),
                &format!("preset {} · name {}", preset.id, preset.name),
            )
            .await;
            match preset_json(&preset) {
                Ok(value) => ok_json(value),
                Err(e) => admin_error(&e),
            }
        }
        Err(Error::NotFound(_)) => not_found(id),
        Err(e) => admin_error(&e),
    }
}
