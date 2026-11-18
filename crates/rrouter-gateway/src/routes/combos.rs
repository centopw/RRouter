//! `/api/combos` — ordered fallback chains.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::State;
use axum::response::Response;
use serde_json::json;

use super::{admin_error, body_json, ok_json};
use crate::combo_meta::{self, ComboMetadata, RegistrySnapshot};
use crate::state::AppState;

fn combo_view(c: &rrouter_store::Combo, metadata: &ComboMetadata) -> serde_json::Value {
    json!({
        "id": c.id.to_string(),
        "name": c.name,
        "models": c.models,
        "strategy": c.strategy.as_str(),
        "router_model": c.router_model,
        "cursor": c.cursor,
        "created_at": c.created_at,
        // Additive: the resolved target sets and the numbers `/v1` advertises for it.
        "members": metadata.members_json(),
        "aggregate": metadata.aggregate_json(),
    })
}

pub async fn list(State(state): State<Arc<AppState>>) -> Response {
    let combos = match state.store.list_combos().await {
        Ok(combos) => combos,
        Err(e) => return admin_error(&e),
    };
    let snapshot = match RegistrySnapshot::load(&state).await {
        Ok(snapshot) => snapshot,
        Err(e) => return admin_error(&e),
    };
    let index = snapshot.index();
    ok_json(json!({
        "data": combos
            .iter()
            .map(|combo| combo_view(combo, &combo_meta::combo_metadata(&index, combo)))
            .collect::<Vec<_>>()
    }))
}

/// Body: `{name, models: ["m1","m2"], strategy?, router_model?}`; an existing name is
/// replaced. `strategy` is `fallback` (default), `round_robin` or `smart`; only `smart`
/// accepts a `router_model`, and it must not resolve to a combo (validation in the store).
///
/// A name has one owner: an alias keeps it, because routing resolves aliases first. A
/// combo may still re-post its own name.
pub async fn create(State(state): State<Arc<AppState>>, body: Body) -> Response {
    let value = match body_json(body).await {
        Ok(v) => v,
        Err(resp) => return *resp,
    };
    let Some(name) = super::models::str_field(&value, "name") else {
        return admin_error(&rrouter_core::Error::BadRequest("name is required".into()));
    };
    match state.store.alias_target(&name).await {
        Ok(Some(_)) => {
            return admin_error(&rrouter_core::Error::BadRequest(format!(
                "name {name:?} is already an alias; an alias keeps the name and a combo must choose another"
            )))
        }
        Ok(None) => {}
        Err(e) => return admin_error(&e),
    }
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
    let strategy = match value.get("strategy").and_then(|v| v.as_str()) {
        Some(raw) => match rrouter_store::ComboStrategy::parse(raw) {
            Some(strategy) => strategy,
            None => {
                return admin_error(&rrouter_core::Error::BadRequest(format!(
                    "unknown combo strategy {raw:?}; use fallback, round_robin or smart"
                )))
            }
        },
        None => rrouter_store::ComboStrategy::Fallback,
    };
    let router_model = value.get("router_model").and_then(|v| v.as_str());
    match state
        .store
        .upsert_combo_with(&name, &models, strategy, router_model)
        .await
    {
        Ok(combo) => match RegistrySnapshot::load(&state).await {
            Ok(snapshot) => ok_json(combo_view(
                &combo,
                &combo_meta::combo_metadata(&snapshot.index(), &combo),
            )),
            Err(e) => admin_error(&e),
        },
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
