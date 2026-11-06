//! `/api/keys` — gateway API key management.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::State;
use axum::response::Response;
use serde_json::json;

use super::{admin_error, body_json, ok_json};
use crate::state::AppState;

pub async fn list(State(state): State<Arc<AppState>>) -> Response {
    match state.store.list_api_keys().await {
        Ok(keys) => {
            let data: Vec<serde_json::Value> = keys
                .iter()
                .map(|k| {
                    json!({
                        "id": k.id.to_string(),
                        "name": k.name,
                        "is_active": k.is_active,
                        "created_at": k.created_at,
                    })
                })
                .collect();
            ok_json(json!({ "data": data }))
        }
        Err(e) => admin_error(&e),
    }
}

/// Creates a key and returns the plaintext **once**; only its hash is stored.
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

pub async fn remove(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Response {
    let Ok(uuid) = uuid::Uuid::parse_str(&id) else {
        return admin_error(&rrouter_core::Error::BadRequest("key id must be a uuid".into()));
    };
    match state.store.delete_api_key(uuid).await {
        Ok(true) => ok_json(json!({ "deleted": true, "id": uuid.to_string() })),
        Ok(false) => admin_error(&rrouter_core::Error::NotFound(format!("key {uuid}"))),
        Err(e) => admin_error(&e),
    }
}
