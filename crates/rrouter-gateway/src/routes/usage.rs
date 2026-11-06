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
    let param = |key: &str| {
        query
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
    };
    let limit = param("limit")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(DEFAULT_LIMIT)
        .clamp(1, MAX_LIMIT);
    // The request log is scoped to the same window as the chart; `hours` is optional
    // so existing callers still get the latest rows.
    let since = param("hours")
        .and_then(|v| v.parse::<i64>().ok())
        .map(|hours| crate::now_unix() - hours.clamp(1, 24 * 365) * 3600);
    match state.store.recent_usage(limit, since).await {
        Ok(records) => {
            let data: Vec<serde_json::Value> = records
                .iter()
                .map(|r| {
                    json!({
                        "id": r.id,
                        "ts": r.ts,
                        "provider": r.provider,
                        "endpoint": r.endpoint,
                        "model": r.model,
                        "connection_id": r.connection_id.map(|c| c.to_string()),
                        "input_tokens": r.input_tokens,
                        "output_tokens": r.output_tokens,
                        "cache_read_tokens": r.cache_read_tokens,
                        "cache_write_tokens": r.cache_write_tokens,
                        "latency_ms": r.latency_ms,
                        "ttft_ms": r.ttft_ms,
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
        Some("5m") => 300,
        _ if hours <= 48 => 3600,
        _ => 86_400,
    };
    let since = crate::now_unix() - hours * 3600;
    // `by=model` answers the stacked traffic chart: one row per bucket and model.
    if param("by").as_deref() == Some("model") {
        return match state.store.usage_series_by_model(since, bucket_secs).await {
            Ok(rows) => {
                let data: Vec<serde_json::Value> = rows
                    .iter()
                    .map(|b| {
                        json!({
                            "start": b.start,
                            "provider": b.provider,
                            "model": b.model,
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
                ok_json(json!({ "data": data, "hours": hours, "bucket_secs": bucket_secs, "by": "model" }))
            }
            Err(e) => admin_error(&e),
        };
    }
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

pub async fn summary(
    State(state): State<Arc<AppState>>,
    Query(query): Query<Vec<(String, String)>>,
) -> Response {
    let since = query
        .iter()
        .find(|(k, _)| k == "hours")
        .and_then(|(_, v)| v.parse::<i64>().ok())
        .map(|hours| crate::now_unix() - hours.clamp(1, 24 * 365) * 3600);
    match state.store.usage_summary(since).await {
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
