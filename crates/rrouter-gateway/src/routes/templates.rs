//! `/api/templates` — reusable named system prompts with revision history.
//!
//! A template is a name, the current body and a version counter; every body ever
//! published is kept in `prompt_template_versions`, so a prompt can be rolled back or
//! diffed without any client keeping copies. Bodies are plain text and never wrapped in
//! a dialect envelope: the playground and any app read them as-is.
//!
//! Limits (100-character name, 100k-character body) live in the store, which is the only
//! writer; this module adds only the HTTP shape and the duplicate-name conflict.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use rrouter_core::Error;
use rrouter_store::{PromptTemplate, PromptTemplateVersion};
use serde_json::{json, Value};

use super::{admin_error, body_json, ok_json};
use crate::error::error_object_with;
use crate::state::AppState;

fn template_view(t: &PromptTemplate) -> Value {
    json!({
        "id": t.id,
        "name": t.name,
        "body": t.body,
        "version": t.version,
        "created_at": t.created_at,
        "updated_at": t.updated_at,
    })
}

fn version_view(v: &PromptTemplateVersion) -> Value {
    json!({
        "version": v.version,
        "body": v.body,
        "created_at": v.created_at,
    })
}

fn created_json(value: Value) -> Response {
    (StatusCode::CREATED, axum::Json(value)).into_response()
}

fn not_found(id: &str) -> Response {
    admin_error(&Error::NotFound(format!("template {id}")))
}

/// `409 template_exists`. A conflict is its own status and its own code so a client can
/// offer "overwrite?" without parsing prose.
fn conflict(name: &str) -> Response {
    (
        StatusCode::CONFLICT,
        axum::Json(json!({
            "error": error_object_with(
                409,
                &format!("template name already exists: {name}"),
                Some("template_exists"),
            )
        })),
    )
        .into_response()
}

/// A failed write is a conflict exactly when the requested name now belongs to a
/// different template. The store cannot tell the route apart from any other bad request,
/// so the check is made here, after the write; a concurrent creator that won the race is
/// reported the same way as a sequential duplicate.
async fn conflict_or_fail(
    state: &AppState,
    name: &str,
    keep_id: Option<&str>,
    err: Error,
) -> Response {
    match state.store.get_template_by_name(name).await {
        Ok(Some(other)) if Some(other.id.as_str()) != keep_id => conflict(name),
        _ => admin_error(&err),
    }
}

pub async fn list(State(state): State<Arc<AppState>>) -> Response {
    match state.store.list_templates().await {
        Ok(rows) => ok_json(json!({
            "data": rows.iter().map(template_view).collect::<Vec<_>>()
        })),
        Err(e) => admin_error(&e),
    }
}

/// Body: `{name, body}`. Returns `201` with the stored row.
pub async fn create(State(state): State<Arc<AppState>>, body: Body) -> Response {
    let value = match body_json(body).await {
        Ok(v) => v,
        Err(resp) => return *resp,
    };
    let Some(name) = super::models::str_field(&value, "name") else {
        return admin_error(&Error::BadRequest("name is required".into()));
    };
    let Some(text) = body_field(&value) else {
        return admin_error(&Error::BadRequest("body is required".into()));
    };
    match state.store.create_template(&name, &text).await {
        Ok(row) => created_json(template_view(&row)),
        Err(e) => conflict_or_fail(&state, &name, None, e).await,
    }
}

pub async fn get(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    match state.store.get_template(&id).await {
        Ok(Some(row)) => ok_json(template_view(&row)),
        Ok(None) => not_found(&id),
        Err(e) => admin_error(&e),
    }
}

/// Body: `{name?, body}`. A body change appends a revision; a rename alone does not.
pub async fn update(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    body: Body,
) -> Response {
    let value = match body_json(body).await {
        Ok(v) => v,
        Err(resp) => return *resp,
    };
    let name = match optional_name(&value) {
        Ok(name) => name,
        Err(e) => return admin_error(&e),
    };
    let Some(text) = body_field(&value) else {
        return admin_error(&Error::BadRequest("body is required".into()));
    };
    match state.store.update_template(&id, name.as_deref(), &text).await {
        Ok(row) => ok_json(template_view(&row)),
        // A missing id is a 404 whatever name was requested; only a write against an
        // existing row can be telling us the new name is taken.
        Err(e @ Error::NotFound(_)) => admin_error(&e),
        Err(e) => match name {
            Some(requested) => conflict_or_fail(&state, &requested, Some(&id), e).await,
            None => admin_error(&e),
        },
    }
}

/// Deletes the template and every revision it owns; `404` when the id is unknown.
pub async fn remove(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    match state.store.delete_template(&id).await {
        Ok(true) => ok_json(json!({ "deleted": true, "id": id })),
        Ok(false) => not_found(&id),
        Err(e) => admin_error(&e),
    }
}

/// Revision history, newest first. An unknown id is `404`, not an empty list: "no such
/// template" and "a template with no revisions" would otherwise look alike.
pub async fn versions(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    match state.store.get_template(&id).await {
        Ok(None) => not_found(&id),
        Ok(Some(_)) => match state.store.template_versions(&id).await {
            Ok(rows) => ok_json(json!({
                "data": rows.iter().map(version_view).collect::<Vec<_>>()
            })),
            Err(e) => admin_error(&e),
        },
        Err(e) => admin_error(&e),
    }
}

/// The body verbatim: leading and trailing whitespace is part of a prompt, so this
/// deliberately does not trim the way [`super::models::str_field`] trims a name.
fn body_field(value: &Value) -> Option<String> {
    value.get("body").and_then(Value::as_str).map(str::to_string)
}

/// `name` is optional on update, but present-and-empty is an error rather than "keep the
/// old name": a client that meant to rename should not silently not rename.
fn optional_name(value: &Value) -> Result<Option<String>, Error> {
    match value.get("name") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(raw)) => {
            let name = raw.trim();
            if name.is_empty() {
                Err(Error::BadRequest("name is required".into()))
            } else {
                Ok(Some(name.to_string()))
            }
        }
        Some(_) => Err(Error::BadRequest("name must be a string".into())),
    }
}
