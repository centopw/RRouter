//! `/api/quota-pings` — subscription auto-ping configuration and next-run preview.
//!
//! One row exists for every active `claude`/`codex` OAuth connection, even before an
//! operator configures it (defaults: mode `off`). The scheduler lives in
//! [`crate::quota_ping`]; this module only writes configuration and reads the plan.

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::response::Response;
use rrouter_store::{Connection, QuotaPing, QuotaPingMode};
use serde_json::{json, Value};
use uuid::Uuid;

use super::{admin_error, body_json, ok_json};
use crate::quota_ping_plan::{
    known_timezone, next_slot, plan_smart, reset_due_at, Schedule, SmartAccount,
};
use crate::state::AppState;

/// Providers with subscription windows this feature can warm.
fn supported(conn: &Connection) -> bool {
    conn.is_active
        && conn.auth_type == "oauth"
        && matches!(conn.provider.as_str(), "claude" | "codex")
}

fn config_map(configs: Vec<QuotaPing>) -> HashMap<Uuid, QuotaPing> {
    configs
        .into_iter()
        .map(|config| (config.connection_id, config))
        .collect()
}

fn schedule(config: &QuotaPing) -> Schedule {
    Schedule {
        anchor: config.anchor.clone(),
        timezone: config.timezone.clone(),
        window_hours: config.window_hours,
        slots_per_day: config.slots_per_day,
    }
}

fn reset_at(
    config: &QuotaPing,
    usage: Option<&crate::provider_usage::AccountUsage>,
) -> Option<i64> {
    let usage = usage?;
    usage
        .windows
        .iter()
        .find(|window| window.id == "five_hour")
        .or_else(|| {
            usage
                .windows
                .iter()
                .find(|window| window.resets_at.is_some())
        })
        .and_then(|window| window.resets_at)
        .filter(|_| config.mode != QuotaPingMode::Off)
}

fn used(usage: Option<&crate::provider_usage::AccountUsage>) -> f64 {
    usage
        .and_then(|usage| {
            usage
                .windows
                .iter()
                .map(|window| window.used_percent)
                .reduce(f64::max)
        })
        .unwrap_or(0.0)
}

/// Every active supported connection with its effective configuration and preview.
pub(crate) async fn rows(state: &AppState) -> rrouter_core::Result<Vec<Value>> {
    let connections: Vec<Connection> = state
        .store
        .list_connections(None)
        .await?
        .into_iter()
        .filter(supported)
        .collect();
    let configs = config_map(state.store.quota_pings().await?);
    let now = crate::now_unix();
    let recent = state.store.recent_request_counts(now - 86_400).await?;
    let usages: HashMap<Uuid, crate::provider_usage::AccountUsage> = connections
        .iter()
        .filter_map(|conn| crate::provider_usage::cached(conn.id).map(|usage| (conn.id, usage)))
        .collect();

    let smart_accounts: Vec<SmartAccount> = connections
        .iter()
        .filter_map(|conn| {
            let config = configs.get(&conn.id)?;
            (config.mode == QuotaPingMode::Smart).then(|| SmartAccount {
                connection_id: conn.id,
                provider: conn.provider.clone(),
                recent_requests: recent.get(&conn.id).copied().unwrap_or(0),
                reset_at: reset_at(config, usages.get(&conn.id)),
                used_percent: used(usages.get(&conn.id)),
                window_hours: config.window_hours,
            })
        })
        .collect();
    let smart = plan_smart(&smart_accounts, now);

    Ok(connections
        .into_iter()
        .map(|conn| {
            let config = configs
                .get(&conn.id)
                .cloned()
                .unwrap_or_else(|| QuotaPing::defaults(conn.id));
            let usage = usages.get(&conn.id);
            let recent_requests = recent.get(&conn.id).copied().unwrap_or(0);
            let plan = smart.get(&conn.id);
            let next_ping_at = match config.mode {
                QuotaPingMode::Off => None,
                QuotaPingMode::Reset => {
                    reset_at(&config, usage).map(|reset| reset_due_at(reset).max(now))
                }
                QuotaPingMode::Schedule => next_slot(&schedule(&config), now).map(|slot| slot.at),
                QuotaPingMode::Smart => plan.and_then(|plan| plan.next_ping_at),
            };
            json!({
                "connection_id": conn.id.to_string(),
                "provider": conn.provider,
                "name": conn.name,
                "mode": config.mode.as_str(),
                "anchor": config.anchor,
                "timezone": config.timezone,
                "window_hours": config.window_hours,
                "slots_per_day": config.slots_per_day,
                "ping_model": config.ping_model,
                "last_ping_at": config.last_ping_at,
                "last_ping_reset_at": config.last_ping_reset_at,
                "next_ping_at": next_ping_at,
                "last_error": config.last_error,
                "updated_at": config.updated_at,
                "smart": {
                    "recent_requests": recent_requests,
                    "phase_offset_secs": plan.map(|plan| plan.phase_offset_secs).unwrap_or(0),
                }
            })
        })
        .collect())
}

pub async fn list(State(state): State<Arc<AppState>>) -> Response {
    match rows(&state).await {
        Ok(rows) => ok_json(json!({ "data": rows })),
        Err(e) => admin_error(&e),
    }
}

fn string_field(value: &Value, field: &str) -> Result<Option<String>, rrouter_core::Error> {
    match value.get(field) {
        None => Ok(None),
        Some(Value::String(raw)) => Ok(Some(raw.clone())),
        Some(_) => Err(rrouter_core::Error::BadRequest(format!(
            "{field} must be a string"
        ))),
    }
}

/// `PUT /api/quota-pings/{connection_id}` — any subset; returns the updated row.
pub async fn update(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    body: Body,
) -> Response {
    let Ok(connection_id) = Uuid::parse_str(&id) else {
        return admin_error(&rrouter_core::Error::BadRequest(
            "connection id must be a uuid".into(),
        ));
    };
    let conn = match state.store.get_connection(connection_id).await {
        Ok(Some(conn)) if supported(&conn) => conn,
        Ok(_) => {
            return admin_error(&rrouter_core::Error::NotFound(format!(
                "quota-ping connection {connection_id}"
            )))
        }
        Err(e) => return admin_error(&e),
    };
    let value = match body_json(body).await {
        Ok(value) => value,
        Err(response) => return *response,
    };
    let mut config = match state.store.quota_ping(connection_id).await {
        Ok(Some(config)) => config,
        Ok(None) => QuotaPing::defaults(connection_id),
        Err(e) => return admin_error(&e),
    };
    let mode = match string_field(&value, "mode") {
        Ok(mode) => mode,
        Err(e) => return admin_error(&e),
    };
    if let Some(raw) = mode {
        let Some(mode) = QuotaPingMode::parse(&raw) else {
            return admin_error(&rrouter_core::Error::BadRequest(format!(
                "unknown quota-ping mode {raw:?}; use off, reset, schedule or smart"
            )));
        };
        config.mode = mode;
    }
    let anchor = match string_field(&value, "anchor") {
        Ok(anchor) => anchor,
        Err(e) => return admin_error(&e),
    };
    if let Some(anchor) = anchor {
        config.anchor = anchor;
    }
    let timezone = match string_field(&value, "timezone") {
        Ok(timezone) => timezone,
        Err(e) => return admin_error(&e),
    };
    if let Some(timezone) = timezone {
        if !known_timezone(&timezone) {
            return admin_error(&rrouter_core::Error::BadRequest(format!(
                "unknown IANA timezone {timezone:?}"
            )));
        }
        config.timezone = timezone;
    }
    if let Some(window) = value.get("window_hours") {
        let Some(window) = window.as_f64() else {
            return admin_error(&rrouter_core::Error::BadRequest(
                "window_hours must be a number".into(),
            ));
        };
        config.window_hours = window;
    }
    if let Some(slots) = value.get("slots_per_day") {
        let Some(slots) = slots.as_u64().and_then(|slots| u32::try_from(slots).ok()) else {
            return admin_error(&rrouter_core::Error::BadRequest(
                "slots_per_day must be a positive integer".into(),
            ));
        };
        config.slots_per_day = slots;
    }
    match value.get("ping_model") {
        None => {}
        Some(Value::Null) => config.ping_model = None,
        Some(Value::String(model)) => config.ping_model = Some(model.clone()),
        Some(_) => {
            return admin_error(&rrouter_core::Error::BadRequest(
                "ping_model must be a string or null".into(),
            ))
        }
    }

    let saved = match state.store.upsert_quota_ping(&config).await {
        Ok(saved) => saved,
        Err(e) => return admin_error(&e),
    };
    if saved.mode == QuotaPingMode::Off {
        if let Err(e) = state.store.clear_quota_ping_schedule(connection_id).await {
            return admin_error(&e);
        }
    }
    match rows(&state).await {
        Ok(rows) => match rows
            .into_iter()
            .find(|row| row["connection_id"] == conn.id.to_string())
        {
            Some(row) => ok_json(row),
            None => admin_error(&rrouter_core::Error::NotFound(format!(
                "quota-ping connection {connection_id}"
            ))),
        },
        Err(e) => admin_error(&e),
    }
}
