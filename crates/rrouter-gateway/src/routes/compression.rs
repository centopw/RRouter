//! `GET /api/compression/stats`: what compression actually removed in one time range.
//!
//! One aggregate row over `compression_events`, scoped exactly like `/api/usage/summary`:
//! a routine reader sees their own teams, an admin may narrow to one team. The counters
//! are the text bytes of the tool results the token saver saw and produced — never the
//! whole request body; the token figure is a documented estimate, never a tokenizer run.

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::Response;
use rrouter_core::Error;
use serde_json::json;
use uuid::Uuid;

use super::{admin_error, ok_json};
use crate::state::AppState;

/// Default window: one week, matching the Compression page's default range.
const DEFAULT_HOURS: i64 = 168;
/// One year is the widest window the usage summary accepts; keep the two aligned so a
/// range that works on one page is not a 400 on the other.
const MAX_HOURS: i64 = 8_760;
/// Bytes per token for the estimate. The gateway never tokenizes here — no provider's
/// tokenizer is authoritative for the bytes RTK dropped — so the figure is a rough
/// ~4 bytes/token (the usual English-text rule of thumb) and the UI labels it as such.
const BYTES_PER_TOKEN: u64 = 4;

fn param<'a>(query: &'a [(String, String)], key: &str) -> Option<&'a str> {
    query
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

pub async fn stats(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<Vec<(String, String)>>,
) -> Response {
    let hours = match param(&query, "hours").filter(|raw| !raw.is_empty()) {
        None => DEFAULT_HOURS,
        Some(raw) => match raw.parse::<i64>() {
            Ok(value) => value.clamp(1, MAX_HOURS),
            Err(_) => {
                return admin_error(&Error::BadRequest("hours must be a number".into()));
            }
        },
    };
    let requested = match param(&query, "team_id").filter(|raw| !raw.is_empty()) {
        None => None,
        Some(raw) => match Uuid::parse_str(raw) {
            Ok(id) => Some(id),
            Err(_) => return admin_error(&Error::BadRequest("team_id must be a uuid".into())),
        },
    };
    let teams = match super::teams::scoped_teams(&state, &headers, requested).await {
        Ok(scope) => scope,
        Err(response) => return *response,
    };
    let since = crate::now_unix() - hours * 3600;
    match state
        .store
        .compression_summary(Some(since), teams.as_deref())
        .await
    {
        Ok(summary) => {
            let saved = summary.rtk_before.saturating_sub(summary.rtk_after);
            ok_json(json!({
                "hours": hours,
                "requests": summary.requests,
                "rtk_requests": summary.rtk_requests,
                "rtk_before": summary.rtk_before,
                "rtk_after": summary.rtk_after,
                "rtk_saved": saved,
                "rtk_saved_tokens_estimate": saved / BYTES_PER_TOKEN,
                "caveman_requests": summary.caveman_requests,
            }))
        }
        Err(e) => admin_error(&e),
    }
}
