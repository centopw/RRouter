//! `/api/combos` — ordered fallback chains.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::State;
use axum::response::Response;
use serde_json::json;

use super::{admin_error, body_json, ok_json};
use crate::state::AppState;

fn combo_view(c: &rrouter_store::Combo) -> serde_json::Value {
    json!({
        "id": c.id.to_string(),
        "name": c.name,
        "models": c.models,
        "created_at": c.created_at,
    })
}

pub async fn list(State(state): State<Arc<AppState>>) -> Response {
    match state.store.list_combos().await {
        Ok(combos) => ok_json(json!({ "data": combos.iter().map(combo_view).collect::<Vec<_>>() })),
        Err(e) => admin_error(&e),
    }
}

/// Body: `{name, models: ["m1","m2"]}`; an existing name is replaced.
pub async fn create(State(state): State<Arc<AppState>>, body: Body) -> Response {
    let value = match body_json(body).await {
        Ok(v) => v,
        Err(resp) => return *resp,
    };
    let Some(name) = super::models::str_field(&value, "name") else {
        return admin_error(&rrouter_core::Error::BadRequest("name is required".into()));
    };
    let models: Vec<String> = value
        .get("models")
        .and_then(|m| m.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.trim().to_string()))
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();
    if models.is_empty() {
        return admin_error(&rrouter_core::Error::BadRequest(
            "models must be a non-empty array of model strings".into(),
        ));
    }
    match state.store.upsert_combo(&name, &models).await {
        Ok(combo) => ok_json(combo_view(&combo)),
        Err(e) => admin_error(&e),
    }
}

/// `DELETE /api/combos?name=x` (or `{name}` in the body).
pub async fn remove(State(state): State<Arc<AppState>>, req: axum::extract::Request) -> Response {
    let query = req.uri().query().unwrap_or("").to_string();
    let name = match super::models::query_param(&query, "name") {
        Some(name) if !name.is_empty() => name,
        _ => {
            let (_, body) = req.into_parts();
            match body_json(body).await {
                Ok(value) => match super::models::str_field(&value, "name") {
                    Some(name) => name,
                    None => {
                        return admin_error(&rrouter_core::Error::BadRequest(
                            "name is required".into(),
                        ))
                    }
                },
                Err(resp) => return *resp,
            }
        }
    };
    match state.store.delete_combo(&name).await {
        Ok(true) => ok_json(json!({ "deleted": true, "name": name })),
        Ok(false) => admin_error(&rrouter_core::Error::NotFound(format!("combo {name}"))),
        Err(e) => admin_error(&e),
    }
}
