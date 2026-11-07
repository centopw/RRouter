//! `/api/usage`, `/api/usage/{request_id}`, `/api/usage/summary`, and the CSV export.
//!
//! The list is the request log; the detail view is what makes a fallback debuggable —
//! one request id, its usage row, and every upstream attempt under it. The export is the
//! same filtered log as a download, for the reports that need raw rows.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::header::{CONTENT_DISPOSITION, CONTENT_TYPE};
use axum::http::HeaderValue;
use axum::response::Response;
use bytes::Bytes;
use rrouter_core::Error;
use rrouter_store::{AttemptRecord, StatusFilter, UsageQuery, UsageRecord};
use serde_json::{json, Value};
use tokio_stream::wrappers::ReceiverStream;
use uuid::Uuid;

use super::{admin_error, ok_json};
use crate::state::AppState;

const DEFAULT_LIMIT: usize = 50;
const MAX_LIMIT: usize = 1000;
/// Rows per `recent_usage` call during the export walk. The CSV body is produced one
/// page at a time, so peak memory is a page, not the whole range.
const EXPORT_PAGE: usize = 1000;
/// Exact column order of the export; clients parse this, so it is a contract.
const CSV_HEADER: &str = "ts,iso_utc,provider,model,request_id,key_id,endpoint,\
input_tokens,output_tokens,cache_read_tokens,cache_write_tokens,latency_ms,ttft_ms,\
upstream_ms,status,streamed,cost_micros,cost_usd\n";

/// One usage row as the admin API exposes it. Shared by the list and the detail view so
/// a client filtering by `request_id` sees exactly the row the attempt view links to.
fn row_json(r: &UsageRecord) -> Value {
    json!({
        "id": r.id,
        "ts": r.ts,
        "request_id": r.request_id.map(|id| id.to_string()),
        "provider": r.provider,
        "endpoint": r.endpoint,
        "model": r.model,
        "connection_id": r.connection_id.map(|c| c.to_string()),
        "key_id": r.key_id.map(|k| k.to_string()),
        "cost_micros": r.cost_micros,
        "input_tokens": r.input_tokens,
        "output_tokens": r.output_tokens,
        "cache_read_tokens": r.cache_read_tokens,
        "cache_write_tokens": r.cache_write_tokens,
        "latency_ms": r.latency_ms,
        "ttft_ms": r.ttft_ms,
        "upstream_ms": r.upstream_ms,
        "status": r.status,
        "streamed": r.streamed,
        "error": r.error,
    })
}

/// One attempt row: what was tried, in dispatch order, and how it ended.
fn attempt_json(a: &AttemptRecord) -> Value {
    json!({
        "attempt": a.attempt,
        "ts": a.ts,
        "provider": a.provider,
        "model": a.model,
        "connection_id": a.connection_id.map(|c| c.to_string()),
        "status": a.status,
        "error": a.error,
        "latency_ms": a.latency_ms,
        "key_id": a.key_id.map(|k| k.to_string()),
    })
}

/// The filters `/api/usage` and `/api/usage/export.csv` share, so an export selects
/// exactly the rows the dashboard is showing.
#[derive(Debug, Default)]
struct Filters {
    since: Option<i64>,
    provider: Option<String>,
    model: Option<String>,
    bare_model: Option<String>,
    key_id: Option<Uuid>,
    status: Option<StatusFilter>,
}

impl Filters {
    /// A query for this filter set with the cursor and page size of one walk step.
    fn query(&self, before_ts: Option<i64>, before_id: Option<i64>, limit: usize) -> UsageQuery {
        UsageQuery {
            since: self.since,
            before_ts,
            before_id,
            provider: self.provider.clone(),
            model: self.model.clone(),
            bare_model: self.bare_model.clone(),
            key_id: self.key_id,
            status: self.status,
            limit,
        }
    }
}

fn param<'a>(query: &'a [(String, String)], key: &str) -> Option<&'a str> {
    query
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

/// Parse the shared filter parameters. Filters are applied in SQL so a filtered walk
/// pages through matching rows only. An empty value is the same as an absent one, so a
/// select left on "all" is harmless.
fn parse_filters(query: &[(String, String)]) -> Result<Filters, Error> {
    let text = |key: &str| param(query, key).filter(|v| !v.is_empty());
    // The request log is scoped to the same window as the chart; `hours` is optional
    // so existing callers still get the latest rows.
    let since = param(query, "hours")
        .and_then(|v| v.parse::<i64>().ok())
        .map(|hours| crate::now_unix() - hours.clamp(1, 24 * 365) * 3600);
    let provider = text("provider").map(str::to_string);
    let key_id = match text("key_id") {
        None => None,
        Some(raw) => Some(
            Uuid::parse_str(raw).map_err(|_| Error::BadRequest("key_id must be a uuid".into()))?,
        ),
    };
    let status = match text("status") {
        None => None,
        Some("2xx") => Some(StatusFilter::Success),
        Some("error") => Some(StatusFilter::Error),
        Some(_) => return Err(Error::BadRequest("status must be 2xx or error".into())),
    };
    // `model` matches the stored string exactly. A `provider/model` ref also matches its
    // bare id, the same normalization the cost lookup applies: a provider prefix is only
    // stripped when it names the filtered provider, or a registered one when none is set.
    let (model, bare_model) = match text("model") {
        None => (None, None),
        Some(m) => {
            let bare = match provider.as_deref() {
                Some(p) => m.strip_prefix(p).and_then(|rest| rest.strip_prefix('/')),
                None => m
                    .split_once('/')
                    .filter(|(head, _)| rrouter_auth::get(head).is_some())
                    .map(|(_, rest)| rest),
            }
            .map(str::to_string);
            (Some(m.to_string()), bare)
        }
    };
    Ok(Filters {
        since,
        provider,
        model,
        bare_model,
        key_id,
        status,
    })
}

pub async fn list(
    State(state): State<Arc<AppState>>,
    Query(query): Query<Vec<(String, String)>>,
) -> Response {
    let limit = param(&query, "limit")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(DEFAULT_LIMIT)
        .clamp(1, MAX_LIMIT);
    // Keyset cursor: `before` is the `ts` of the last row on the previous page and
    // `before_id` its `id`. Both together page exactly past that row, even when rows
    // share a timestamp; `before` alone keeps only strictly older rows.
    let before_ts = param(&query, "before").and_then(|v| v.parse::<i64>().ok());
    let before_id = param(&query, "before_id").and_then(|v| v.parse::<i64>().ok());
    let filters = match parse_filters(&query) {
        Ok(filters) => filters,
        Err(e) => return admin_error(&e),
    };
    let query = filters.query(before_ts, before_id, limit);
    match state.store.recent_usage(&query).await {
        Ok(records) => {
            // A short page is the last one: there is nothing older to ask for.
            let next = (records.len() == limit).then(|| records.last()).flatten();
            let next_before = next.map(|r| r.ts);
            let next_before_id = next.and_then(|r| r.id);
            let data: Vec<Value> = records.iter().map(row_json).collect();
            ok_json(json!({
                "data": data,
                "limit": limit,
                "next_before": next_before,
                "next_before_id": next_before_id,
            }))
        }
        Err(e) => admin_error(&e),
    }
}

/// `GET /api/usage/export.csv` — the same filtered request log as a CSV download.
///
/// The walk is keyset-paged at [`EXPORT_PAGE`] rows and each page is serialized and sent
/// to the body as soon as it arrives, so a 100k-row export holds one page of rows and one
/// page of text at a time, never the whole range. A read failure after the headers are
/// sent can only end the body, which is what a client sees as a truncated download.
pub async fn export_csv(
    State(state): State<Arc<AppState>>,
    Query(query): Query<Vec<(String, String)>>,
) -> Response {
    let filters = match parse_filters(&query) {
        Ok(filters) => filters,
        Err(e) => return admin_error(&e),
    };
    let attachment = HeaderValue::from_str(&format!(
        "attachment; filename=\"rrouter-usage-{}.csv\"",
        utc_date(crate::now_unix())
    ))
    .unwrap_or_else(|_| HeaderValue::from_static("attachment"));
    // A small bounded channel is the backpressure: a slow client pauses the walk rather
    // than letting the export accumulate in memory.
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(2);
    let store = Arc::clone(&state.store);
    tokio::spawn(async move {
        if tx
            .send(Ok(Bytes::from_static(CSV_HEADER.as_bytes())))
            .await
            .is_err()
        {
            return;
        }
        let mut cursor: Option<(i64, i64)> = None;
        loop {
            let (before_ts, before_id) = match cursor {
                Some((ts, id)) => (Some(ts), Some(id)),
                None => (None, None),
            };
            let page = match store
                .recent_usage(&filters.query(before_ts, before_id, EXPORT_PAGE))
                .await
            {
                Ok(page) => page,
                Err(e) => {
                    let _ = tx.send(Err(std::io::Error::other(e.to_string()))).await;
                    return;
                }
            };
            let full = page.len() == EXPORT_PAGE;
            let mut chunk = String::with_capacity(page.len() * 96);
            for row in &page {
                csv_row(&mut chunk, row);
            }
            if tx.send(Ok(Bytes::from(chunk))).await.is_err() {
                return; // the client hung up; nothing left to write to
            }
            // The cursor is the last row's `(ts, id)`, so the next page starts strictly
            // after it even when rows share a timestamp. A row read back always has an
            // id; if one somehow does not, the next page would repeat this one — stop.
            match page
                .last()
                .filter(|_| full)
                .and_then(|r| r.id.map(|id| (r.ts, id)))
            {
                Some(next) => cursor = Some(next),
                None => return,
            }
        }
    });
    let mut response = Response::new(Body::from_stream(ReceiverStream::new(rx)));
    let headers = response.headers_mut();
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/csv; charset=utf-8"),
    );
    headers.insert(CONTENT_DISPOSITION, attachment);
    response
}

/// Append one RFC 4180 cell: quoted only when it contains `,`, `"` or a line break, with
/// inner quotes doubled, so a model name or endpoint never shifts the columns.
fn push_cell(out: &mut String, value: &str) {
    if !value.contains([',', '"', '\n', '\r']) {
        out.push_str(value);
        return;
    }
    out.push('"');
    for ch in value.chars() {
        if ch == '"' {
            out.push('"');
        }
        out.push(ch);
    }
    out.push('"');
}

/// One export row, in [`CSV_HEADER`] order. Absent ids are empty cells, `cost_usd` is
/// the stored micro-USD at six decimals, and `streamed` is a JSON-style boolean.
fn csv_row(out: &mut String, r: &UsageRecord) {
    out.push_str(&r.ts.to_string());
    out.push(',');
    out.push_str(&iso_utc(r.ts));
    out.push(',');
    push_cell(out, &r.provider);
    out.push(',');
    push_cell(out, &r.model);
    out.push(',');
    push_cell(
        out,
        &r.request_id.map(|id| id.to_string()).unwrap_or_default(),
    );
    out.push(',');
    push_cell(out, &r.key_id.map(|id| id.to_string()).unwrap_or_default());
    out.push(',');
    push_cell(out, r.endpoint.as_deref().unwrap_or_default());
    for n in [
        r.input_tokens,
        r.output_tokens,
        r.cache_read_tokens,
        r.cache_write_tokens,
        r.latency_ms,
        r.ttft_ms,
        r.upstream_ms,
    ] {
        out.push(',');
        out.push_str(&n.to_string());
    }
    out.push(',');
    out.push_str(&r.status.to_string());
    out.push_str(if r.streamed { ",true," } else { ",false," });
    out.push_str(&r.cost_micros.to_string());
    out.push(',');
    // 6 decimals is the micro-USD unit itself: `$0.000130` for 130 µUSD.
    out.push_str(&format!("{:.6}", r.cost_micros as f64 / 1_000_000.0));
    out.push('\n');
}

/// `2026-11-07T02:11:31Z` — RFC 3339 in UTC, second precision, from a unix timestamp.
fn iso_utc(ts: i64) -> String {
    let days = ts.div_euclid(86_400);
    let secs = ts.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        secs / 3_600,
        secs % 3_600 / 60,
        secs % 60
    )
}

/// `20261107` — the UTC calendar date in the export filename.
fn utc_date(ts: i64) -> String {
    let (year, month, day) = civil_from_days(ts.div_euclid(86_400));
    format!("{year:04}{month:02}{day:02}")
}

/// (year, month, day) of `days` since 1970-01-01 (H. Hinnant's algorithm). The store
/// uses the same conversion for budget windows; this crate has no date dependency.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// `GET /api/usage/{request_id}` — the usage row plus every attempt it made.
///
/// A request that failed on every candidate has attempts but no usage row (usage is
/// only written for a served request), so the attempts alone are a known id and are
/// returned with `"request": null` instead of a misleading 404.
pub async fn detail(
    State(state): State<Arc<AppState>>,
    Path(request_id): Path<String>,
) -> Response {
    let Ok(id) = Uuid::parse_str(&request_id) else {
        return admin_error(&Error::NotFound(format!("unknown request {request_id}")));
    };
    let request = match state.store.usage_by_request(id).await {
        Ok(row) => row,
        Err(e) => return admin_error(&e),
    };
    let attempts = match state.store.attempts_for(id).await {
        Ok(rows) => rows,
        Err(e) => return admin_error(&e),
    };
    if request.is_none() && attempts.is_empty() {
        return admin_error(&Error::NotFound(format!("unknown request {id}")));
    }
    let attempts: Vec<Value> = attempts.iter().map(attempt_json).collect();
    ok_json(json!({
        "data": {
            "request": request.as_ref().map(row_json),
            "attempts": attempts,
        }
    }))
}

/// `GET /api/usage/series?hours=168&bucket=hour|day`
///
/// Aggregated in SQL so the dashboard draws a week of traffic without reading every
/// event. `bucket` defaults to whichever of hour/day keeps the point count readable.
pub async fn series(
    State(state): State<Arc<AppState>>,
    Query(query): Query<Vec<(String, String)>>,
) -> Response {
    let param = |key: &str| query.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone());
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
                            "cost_micros": b.cost_micros,
                            "avg_latency_ms": b.avg_latency_ms,
                        })
                    })
                    .collect();
                ok_json(
                    json!({ "data": data, "hours": hours, "bucket_secs": bucket_secs, "by": "model" }),
                )
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
                        "cost_micros": b.cost_micros,
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
                        "cost_micros": s.cost_micros,
                        "avg_latency_ms": s.avg_latency_ms,
                    })
                })
                .collect();
            ok_json(json!({ "data": data }))
        }
        Err(e) => admin_error(&e),
    }
}
