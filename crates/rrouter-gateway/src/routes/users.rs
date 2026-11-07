//! `/api/users` — dashboard accounts. Admin only, behind the admin gate.
//!
//! The owner is protected in the store (not here), so a demote or delete through any
//! other caller is refused too. Invitations live in `routes::auth` next to signup.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use rrouter_core::Error;
use rrouter_store::UserRole;
use serde_json::{json, Value};
use uuid::Uuid;

use super::auth::user_json;
use super::{admin_error, body_json, ok_json};
use crate::state::AppState;

/// `GET /api/users`
pub async fn list(State(state): State<Arc<AppState>>) -> Response {
    match state.store.users().await {
        Ok(users) => ok_json(json!({
            "users": users.iter().map(user_json).collect::<Vec<_>>(),
        })),
        Err(e) => admin_error(&e),
    }
}

/// `PUT /api/users/{id}` — `{ role?, disabled? }`. The owner is refused by the store.
pub async fn update(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Body,
) -> Response {
    let Ok(id) = Uuid::parse_str(&id) else {
        return admin_error(&Error::NotFound("user".into()));
    };
    let value = match body_json(body).await {
        Ok(v) => v,
        Err(resp) => return *resp,
    };
    let role = match value.get("role") {
        None | Some(Value::Null) => None,
        Some(raw) => match raw.as_str().and_then(UserRole::parse) {
            Some(role) => Some(role),
            None => {
                return admin_error(&Error::BadRequest(
                    "role must be admin, member or viewer".into(),
                ))
            }
        },
    };
    let disabled = value.get("disabled").and_then(Value::as_bool);
    match state.store.update_user(id, role, disabled).await {
        Ok(Some(user)) => {
            super::auth::audit(
                &state,
                "user_updated",
                Some(&user.email),
                &crate::auth::client_ip(&headers, None),
                &format!(
                    "role {} · {}",
                    user.role.as_str(),
                    if user.disabled { "disabled" } else { "enabled" }
                ),
            )
            .await;
            tracing::info!(email = %user.email, role = user.role.as_str(), disabled = user.disabled, "user updated");
            ok_json(json!({ "user": user_json(&user) }))
        }
        Ok(None) => admin_error(&Error::NotFound(format!("user {id}"))),
        Err(e) => admin_error(&e),
    }
}

/// `DELETE /api/users/{id}` — the owner cannot be removed.
pub async fn remove(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let Ok(id) = Uuid::parse_str(&id) else {
        return admin_error(&Error::NotFound("user".into()));
    };
    match state.store.delete_user(id).await {
        Ok(true) => {
            super::auth::audit(
                &state,
                "user_removed",
                None,
                &crate::auth::client_ip(&headers, None),
                &format!("user {id}"),
            )
            .await;
            tracing::info!(user = %id, "user removed");
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => admin_error(&Error::NotFound(format!("user {id}"))),
        Err(e) => admin_error(&e),
    }
}
