//! `POST /api/route/explain` — the plan the proxy would execute, without sending it.
//!
//! This is the answer to "why did that request go there?" and to "what would happen if
//! I changed the model string / provider preference / plan window?". It runs the same
//! planner, strategy, policy, quota and subscription-capacity inputs as a live request.
//! It is admin-scoped: candidate connections and their plan headroom are operator
//! topology, not a tenant's data.
//!
//! The endpoint never performs provider I/O: subscription hints come from the same
//! cached snapshots the request path reads.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::Response;
use serde_json::{json, Value};

use rrouter_core::Error;
use rrouter_router::PlanRequest;

use super::{admin_error, body_json, ok_json};
use crate::state::AppState;

/// `POST /api/route/explain` with at least `{"model": "..."}`.
///
/// Optional `stream` and the same `provider` object / `x-rrouter-*` headers a chat
/// request accepts are honoured, so the plan can be compared before a client changes.
pub async fn explain(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let value = match body_json(body).await {
        Ok(value) => value,
        Err(response) => return *response,
    };
    let Some(model) = value.get("model").and_then(Value::as_str).map(str::trim) else {
        return admin_error(&Error::BadRequest("model is required".into()));
    };
    if model.is_empty() {
        return admin_error(&Error::BadRequest("model is required".into()));
    }
    let stream = value
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let policy = match crate::proxy::provider_policy(&value, &headers) {
        Ok(policy) => policy,
        Err(error) => return admin_error(&error),
    };
    let config = state.config.load();
    let capacity = crate::provider_usage::cached_hints();
    let plan = state
        .planner
        .plan_with_capacity(
            state.catalog.as_ref(),
            state.store.as_ref(),
            PlanRequest {
                model,
                stream,
                affinity: None,
                strategy: config.strategy,
                policy: policy.as_ref(),
                allowed_models: None,
                team_id: None,
                canary: config.canary.as_ref(),
                canary_seed: 0,
                combo_start: None,
            },
            Some(&capacity),
            f64::from(config.subscription_reserve_percent),
        )
        .await;
    let plan = match plan {
        Ok(plan) => plan,
        Err(error) => return admin_error(&error),
    };

    let candidates: Vec<Value> = plan
        .candidates
        .iter()
        .map(|candidate| {
            let hint = capacity.get(candidate.connection);
            json!({
                "provider": candidate.provider,
                "model": candidate.model,
                "connection_id": candidate.connection.to_string(),
                "priority": candidate.priority,
                "dialect": candidate.dialect.as_str(),
                "subscription_used_percent": hint.map(|hint| hint.used_percent),
                "subscription_resets_at": hint.and_then(|hint| hint.resets_at),
            })
        })
        .collect();

    ok_json(json!({
        "data": {
            "model": plan.model,
            "stream": plan.stream,
            "strategy": config.strategy.as_str(),
            "subscription_reserve_percent": config.subscription_reserve_percent,
            "capacity_hints": capacity.len(),
            "candidates": candidates,
        }
    }))
}
