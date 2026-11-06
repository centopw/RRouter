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
                        "cache_read_tokens": r.cache_read_tokens,
                        "cache_write_tokens": r.cache_write_tokens,
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

/// `GET /api/usage/series?hours=168&bucket=hour|day`
///
/// Aggregated in SQL so the dashboard draws a week of traffic without reading every
/// event. `bucket` defaults to whichever of hour/day keeps the point count readable.
pub async fn series(
    State(state): State<Arc<AppState>>,
    Query(query): Query<Vec<(String, String)>>,
) -> Response {
    let param = |key: &str| {
        query
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
    };
    let hours = param("hours")
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(24)
        .clamp(1, 24 * 365);
    let bucket_secs = match param("bucket").as_deref() {
        Some("hour") => 3600,
        Some("day") => 86_400,
        Some("minute") => 60,
        _ if hours <= 48 => 3600,
        _ => 86_400,
    };
    let since = crate::now_unix() - hours * 3600;
    match state.store.usage_series(since, bucket_secs).await {
        Ok(buckets) => {
            let data: Vec<serde_json::Value> = buckets
                .iter()
                .map(|b| {
                    json!({
                        "start": b.start,
                        "requests": b.requests,
                        "errors": b.errors,
                        "input_tokens": b.input_tokens,
                        "output_tokens": b.output_tokens,
                        "cache_read_tokens": b.cache_read_tokens,
                        "cache_write_tokens": b.cache_write_tokens,
                        "avg_latency_ms": b.avg_latency_ms,
                    })
                })
                .collect();
            ok_json(json!({ "data": data, "hours": hours, "bucket_secs": bucket_secs }))
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
                        "cache_read_tokens": s.cache_read_tokens,
                        "cache_write_tokens": s.cache_write_tokens,
                        "avg_latency_ms": s.avg_latency_ms,
                    })
                })
                .collect();
            ok_json(json!({ "data": data }))
        }
        Err(e) => admin_error(&e),
    }
}
