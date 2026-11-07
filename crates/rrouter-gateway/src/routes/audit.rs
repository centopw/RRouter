//! `/api/audit` — the persisted security trail. Admin only, behind the admin gate.

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::response::Response;
use serde::Deserialize;
use serde_json::json;

use super::{admin_error, ok_json};
use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    kind: Option<String>,
    limit: Option<usize>,
    before: Option<i64>,
}

pub async fn list(State(state): State<Arc<AppState>>, Query(query): Query<ListQuery>) -> Response {
    let kind = query.kind.as_deref().filter(|kind| !kind.is_empty());
    let limit = query.limit.unwrap_or(100);
    match state.store.audit_events(kind, limit, query.before).await {
        Ok(events) => ok_json(json!({
            "data": events.iter().map(|event| json!({
                "id": event.id,
                "ts": event.ts,
                "kind": event.kind,
                "actor": event.actor,
                "ip": event.ip,
                "detail": event.detail,
            })).collect::<Vec<_>>(),
        })),
        Err(e) => admin_error(&e),
    }
}
