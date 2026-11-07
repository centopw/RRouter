//! `/api/accounts/quotas` — per-account limits and the spend of their current window.
//!
//! Usage is aggregated from the `usage` table at read time, so a quota needs no counter
//! to reset and survives a restart. The planner skips exhausted accounts; this API is
//! how an operator sees and edits them.

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use rrouter_store::{window_reset, window_start, AccountQuota, AccountUsage, BudgetWindow, Connection};
use serde_json::{json, Value};
use uuid::Uuid;

use super::{admin_error, body_json, ok_json};
use crate::state::AppState;

/// One account's quota and the spend it is measured against. A connection without a
/// quota row reads as the default daily window with unlimited limits.
struct Row {
    connection: Connection,
    window: BudgetWindow,
    max_requests: u64,
    max_tokens: u64,
    max_cost_micros: u64,
    usage: AccountUsage,
    exhausted: bool,
    resets_at: Option<i64>,
}

impl Row {
    fn view(&self) -> Value {
        json!({
            "connection_id": self.connection.id.to_string(),
            "name": self.connection.name,
            "provider": self.connection.provider,
            "is_active": self.connection.is_active,
            "window": self.window.as_str(),
            "max_requests": self.max_requests,
            "max_tokens": self.max_tokens,
            "max_cost_micros": self.max_cost_micros,
            "requests": self.usage.requests,
            "tokens": self.usage.tokens,
            "cost_micros": self.usage.cost_micros,
            "exhausted": self.exhausted,
            "resets_at": self.resets_at,
        })
    }
}

/// Aggregate every connection with its quota, using one grouped query for the quota
/// windows and one for the default daily window of unquoted accounts.
async fn rows(state: &AppState) -> rrouter_core::Result<Vec<Row>> {
    let store = &state.store;
    let connections = store.list_connections(None).await?;
    let states = store.account_quota_states().await?;
    let now = crate::now_unix();
    let daily = window_start(BudgetWindow::Daily, now);
    let daily_usage = store.account_usage_by_connection(daily).await?;
    let mut quota: HashMap<Uuid, &rrouter_store::AccountQuotaState> =
        HashMap::with_capacity(states.len());
    for state in &states {
        quota.insert(state.quota.connection_id, state);
    }
    Ok(connections
        .into_iter()
        .map(|connection| match quota.get(&connection.id) {
            Some(state) => Row {
                window: state.quota.window,
                max_requests: state.quota.max_requests,
                max_tokens: state.quota.max_tokens,
                max_cost_micros: state.quota.max_cost_micros,
                usage: state.usage.clone(),
                exhausted: state.exhausted,
                resets_at: state.resets_at,
                connection,
            },
            None => Row {
                window: BudgetWindow::Daily,
                max_requests: 0,
                max_tokens: 0,
                max_cost_micros: 0,
                usage: daily_usage.get(&connection.id).cloned().unwrap_or_default(),
                exhausted: false,
                resets_at: daily.and_then(|start| window_reset(BudgetWindow::Daily, start)),
                connection,
            },
        })
        .collect())
}

pub async fn list(State(state): State<Arc<AppState>>) -> Response {
    match rows(&state).await {
        Ok(rows) => ok_json(json!({
            "quotas": rows.iter().map(Row::view).collect::<Vec<_>>(),
        })),
        Err(e) => admin_error(&e),
    }
}

/// `PUT /api/accounts/quotas/{id}` — any subset of `window`, `max_requests`,
/// `max_tokens`, `max_cost_micros`; omitted fields keep their stored value.
pub async fn update(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    body: Body,
) -> Response {
    let id = match Uuid::parse_str(&id) {
        Ok(id) => id,
        Err(_) => return admin_error(&rrouter_core::Error::NotFound("connection".into())),
    };
    let value = match body_json(body).await {
        Ok(v) => v,
        Err(resp) => return *resp,
    };
    match state.store.get_connection(id).await {
        Ok(Some(_)) => {}
        Ok(None) => {
            return admin_error(&rrouter_core::Error::NotFound(format!("connection {id}")))
        }
        Err(e) => return admin_error(&e),
    }

    let existing = match state.store.account_quotas().await {
        Ok(quotas) => quotas.into_iter().find(|q| q.connection_id == id),
        Err(e) => return admin_error(&e),
    };
    let mut quota = existing.unwrap_or(AccountQuota {
        connection_id: id,
        window: BudgetWindow::Daily,
        max_requests: 0,
        max_tokens: 0,
        max_cost_micros: 0,
    });
    if let Some(window) = value.get("window").filter(|v| !v.is_null()) {
        let Some(parsed) = window.as_str().and_then(BudgetWindow::parse) else {
            return admin_error(&rrouter_core::Error::BadRequest(
                "window must be daily, weekly, monthly or total".into(),
            ));
        };
        quota.window = parsed;
    }
    for (field, slot) in [
        ("max_requests", &mut quota.max_requests),
        ("max_tokens", &mut quota.max_tokens),
        ("max_cost_micros", &mut quota.max_cost_micros),
    ] {
        let Some(raw) = value.get(field).filter(|v| !v.is_null()) else {
            continue;
        };
        let Some(number) = raw.as_u64() else {
            return admin_error(&rrouter_core::Error::BadRequest(format!(
                "{field} must be a non-negative integer"
            )));
        };
        *slot = number;
    }
    if let Err(e) = state.store.set_account_quota(&quota).await {
        return admin_error(&e);
    }
    match rows(&state).await {
        Ok(rows) => match rows.into_iter().find(|r| r.connection.id == id) {
            Some(row) => ok_json(json!({ "quota": row.view() })),
            None => admin_error(&rrouter_core::Error::NotFound(format!("connection {id}"))),
        },
        Err(e) => admin_error(&e),
    }
}

/// `DELETE /api/accounts/quotas/{id}` — back to unlimited; idempotent.
pub async fn remove(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    let id = match Uuid::parse_str(&id) {
        Ok(id) => id,
        Err(_) => return admin_error(&rrouter_core::Error::NotFound("connection".into())),
    };
    match state.store.clear_account_quota(id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => admin_error(&e),
    }
}
