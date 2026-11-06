//! `/api/usage` and `/api/usage/summary`.

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::response::Response;
use serde_json::json;

use super::{admin_error, ok_json};
use crate::state::AppState;

const DEFAULT_LIMIT: usize = 50;
const MAX_LIMIT: usize = 1000;

pub async fn list(
    State(state): State<Arc<AppState>>,
    Query(query): Query<Vec<(String, String)>>,
) -> Response {
    let limit = query
        .iter()
        .find(|(k, _)| k == "limit")
        .and_then(|(_, v)| v.parse::<usize>().ok())
        .unwrap_or(DEFAULT_LIMIT)
        .clamp(1, MAX_LIMIT);
    match state.store.recent_usage(limit).await {
        Ok(records) => {
            let data: Vec<serde_json::Value> = records
                .iter()
                .map(|r| {
                    json!({
                        "ts": r.ts,
                        "provider": r.provider,
                        "model": r.model,
                        "connection_id": r.connection_id.map(|c| c.to_string()),
                        "input_tokens": r.input_tokens,
                        "output_tokens": r.output_tokens,
                        "latency_ms": r.latency_ms,
                        "status": r.status,
                        "streamed": r.streamed,
                        "error": r.error,
                    })
                })
                .collect();
            ok_json(json!({ "data": data, "limit": limit }))
        }
        Err(e) => admin_error(&e),
    }
}

pub async fn summary(State(state): State<Arc<AppState>>) -> Response {
    match state.store.usage_summary().await {
        Ok(rows) => {
            let data: Vec<serde_json::Value> = rows
                .iter()
                .map(|s| {
                    json!({
                        "provider": s.provider,
                        "model": s.model,
                        "requests": s.requests,
                        "errors": s.errors,
                        "input_tokens": s.input_tokens,
                        "output_tokens": s.output_tokens,
                        "avg_latency_ms": s.avg_latency_ms,
                    })
                })
                .collect();
            ok_json(json!({ "data": data }))
        }
        Err(e) => admin_error(&e),
    }
}
