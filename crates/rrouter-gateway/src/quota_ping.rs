//! Background quota-window warmer for Claude/Codex OAuth accounts.
//!
//! The scheduler is operational, not client traffic: it sends a tiny provider request,
//! drains the answer and records only its own ping metadata. Pings never create usage or
//! API-key rows and a failure is a logged/recorded condition, never an HTTP failure.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use rrouter_store::{Connection, QuotaPing, QuotaPingMode};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::quota_ping_plan::{
    due_slot, plan_smart, reset_due_at, Schedule, SmartAccount, SLOT_GRACE_SECS,
};
use crate::state::AppState;

const TICK: Duration = Duration::from_secs(60);
const FAILURE_COOLDOWN_SECS: i64 = 15 * 60;
const PING_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_ERROR: usize = 256;
const CLAUDE_URL: &str = "https://api.anthropic.com/v1/messages?beta=true";
const CLAUDE_MODEL: &str = "claude-haiku-4-5-20251001";
const CODEX_URL: &str = "https://chatgpt.com/backend-api/codex/responses";
const CODEX_MODEL: &str = "gpt-5.6-terra";

fn supported(conn: &Connection) -> bool {
    conn.is_active
        && conn.auth_type == "oauth"
        && matches!(conn.provider.as_str(), "claude" | "codex")
}

fn schedule(config: &QuotaPing) -> Schedule {
    Schedule {
        anchor: config.anchor.clone(),
        timezone: config.timezone.clone(),
        window_hours: config.window_hours,
        slots_per_day: config.slots_per_day,
    }
}

fn reset_at(usage: Option<&crate::provider_usage::AccountUsage>) -> Option<i64> {
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
}

fn used_percent(usage: Option<&crate::provider_usage::AccountUsage>) -> f64 {
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

/// Start the process-wide 60-second scheduler.
pub fn spawn(state: Arc<AppState>) {
    tokio::spawn(async move {
        tokio::time::sleep(TICK).await;
        loop {
            if let Err(error) = tick(&state).await {
                tracing::debug!(error = %error, "quota ping tick failed");
            }
            tokio::time::sleep(TICK).await;
        }
    });
}

async fn tick(state: &AppState) -> rrouter_core::Result<()> {
    let now = crate::now_unix();
    let connections: Vec<Connection> = state
        .store
        .list_connections(None)
        .await?
        .into_iter()
        .filter(supported)
        .collect();
    let configs: HashMap<Uuid, QuotaPing> = state
        .store
        .quota_pings()
        .await?
        .into_iter()
        .filter(|config| config.mode != QuotaPingMode::Off)
        .map(|config| (config.connection_id, config))
        .collect();
    if configs.is_empty() {
        return Ok(());
    }
    let recent = state.store.recent_request_counts(now - 86_400).await?;

    // Refresh usage first: it is both the reset source and the smart input. A failed
    // read is cached by `provider_usage` and does not prevent a schedule ping later.
    let mut usages: HashMap<Uuid, crate::provider_usage::AccountUsage> = HashMap::new();
    for conn in &connections {
        if configs.contains_key(&conn.id) {
            let usage = crate::provider_usage::account(state, conn).await;
            usages.insert(conn.id, usage);
        }
    }
    let smart_accounts: Vec<SmartAccount> = connections
        .iter()
        .filter_map(|conn| {
            let config = configs.get(&conn.id)?;
            (config.mode == QuotaPingMode::Smart).then(|| SmartAccount {
                connection_id: conn.id,
                provider: conn.provider.clone(),
                recent_requests: recent.get(&conn.id).copied().unwrap_or(0),
                reset_at: reset_at(usages.get(&conn.id)),
                used_percent: used_percent(usages.get(&conn.id)),
                window_hours: config.window_hours,
            })
        })
        .collect();
    let smart = plan_smart(&smart_accounts, now);

    for conn in connections {
        let Some(config) = configs.get(&conn.id) else {
            continue;
        };
        if config.last_error.is_some() && now - config.updated_at < FAILURE_COOLDOWN_SECS {
            continue;
        }
        let usage = usages.get(&conn.id);
        let reset = reset_at(usage);
        let (due, reset_marker, slot_marker) = match config.mode {
            QuotaPingMode::Off => (false, None, None),
            QuotaPingMode::Reset => {
                let due_at = reset.map(reset_due_at);
                let due = due_at.is_some_and(|at| now >= at) && reset != config.last_ping_reset_at;
                (due, reset, None)
            }
            QuotaPingMode::Schedule => match due_slot(&schedule(config), now, SLOT_GRACE_SECS) {
                Some(slot) if config.last_ping_slot.as_deref() != Some(slot.key.as_str()) => {
                    (true, None, Some(slot.key))
                }
                _ => (false, None, None),
            },
            QuotaPingMode::Smart => match smart.get(&conn.id).and_then(|plan| plan.due_key.clone())
            {
                Some(key) if config.last_ping_slot.as_deref() != Some(key.as_str()) => {
                    let reset_marker = key.starts_with("smart-reset:").then_some(reset).flatten();
                    (true, reset_marker, Some(key))
                }
                _ => (false, None, None),
            },
        };
        if !due {
            continue;
        }
        let Some(spec) = rrouter_auth::get(&conn.provider) else {
            continue;
        };
        let result = async {
            let creds = state
                .refresher
                .fresh(state.store.as_ref(), spec, conn.id)
                .await
                .map_err(|error| error.to_string())?;
            send_ping(state, &conn, &creds, config).await
        }
        .await;
        match result {
            Ok(()) => {
                tracing::info!(connection = %conn.id, mode = config.mode.as_str(), "quota ping sent");
                if let Err(error) = state
                    .store
                    .record_quota_ping(
                        conn.id,
                        Some(now),
                        reset_marker,
                        slot_marker.as_deref(),
                        None,
                    )
                    .await
                {
                    tracing::warn!(error = %error, "quota ping result could not be stored");
                }
            }
            Err(error) => {
                tracing::warn!(connection = %conn.id, mode = config.mode.as_str(), error = %error, "quota ping failed");
                let message = truncate(&error);
                if let Err(store_error) = state
                    .store
                    .record_quota_ping(conn.id, None, None, None, Some(&message))
                    .await
                {
                    tracing::warn!(error = %store_error, "quota ping error could not be stored");
                }
            }
        }
    }
    Ok(())
}

async fn send_ping(
    state: &AppState,
    conn: &Connection,
    creds: &rrouter_auth::Credentials,
    config: &QuotaPing,
) -> Result<(), String> {
    match conn.provider.as_str() {
        "claude" => {
            let spec = rrouter_auth::get("claude").ok_or("unknown claude provider")?;
            let mut headers =
                rrouter_auth::build_headers(spec, creds).map_err(|e| e.to_string())?;
            headers.push(("content-type".into(), "application/json".into()));
            let body = json!({
                "model": config.ping_model.as_deref().unwrap_or(CLAUDE_MODEL),
                "max_tokens": 1,
                "messages": [{"role": "user", "content": "hi"}],
            });
            send(state, CLAUDE_URL, headers, &body).await
        }
        "codex" => {
            let spec = rrouter_auth::get("codex").ok_or("unknown codex provider")?;
            let mut headers =
                rrouter_auth::build_headers(spec, creds).map_err(|e| e.to_string())?;
            if let Some(account) = crate::provider_usage::codex_account_id(
                creds.id_token.as_deref(),
                &creds.extra,
            ) {
                headers.push(("ChatGPT-Account-Id".into(), account));
            }
            headers.push(("content-type".into(), "application/json".into()));
            let body = json!({
                "model": config.ping_model.as_deref().unwrap_or(CODEX_MODEL),
                "input": [{"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]}],
                "instructions": "Reply with OK.",
                "reasoning": {"effort": "none"},
                "store": false,
                "stream": true,
            });
            send(state, CODEX_URL, headers, &body).await
        }
        provider => Err(format!("{provider} has no quota ping")),
    }
}

async fn send(
    state: &AppState,
    url: &str,
    headers: Vec<(String, String)>,
    body: &Value,
) -> Result<(), String> {
    let mut request = state.http.post(url).timeout(PING_TIMEOUT).json(body);
    for (name, value) in headers {
        request = request.header(name, value);
    }
    let response = request.send().await.map_err(|error| {
        if error.is_timeout() {
            "quota ping timed out".to_string()
        } else {
            format!("quota ping transport: {error}")
        }
    })?;
    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .map_err(|error| format!("quota ping body: {error}"))?;
    if !status.is_success() {
        return Err(format!(
            "quota ping answered {}: {}",
            status.as_u16(),
            truncate(&String::from_utf8_lossy(&bytes))
        ));
    }
    Ok(())
}

fn truncate(raw: &str) -> String {
    raw.chars().take(MAX_ERROR).collect()
}

/// Test-only request fixture used by the gateway tests.
#[cfg(test)]
pub(crate) fn codex_body(ping_model: Option<&str>) -> Value {
    json!({
        "model": ping_model.unwrap_or(CODEX_MODEL),
        "input": [{"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]}],
        "instructions": "Reply with OK.",
        "reasoning": {"effort": "none"},
        "store": false,
        "stream": true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_ping_body_matches_the_responses_shape() {
        let body = codex_body(None);
        assert_eq!(body["model"], CODEX_MODEL);
        assert_eq!(body["input"][0]["type"], "message");
        assert_eq!(body["input"][0]["role"], "user");
        assert_eq!(body["input"][0]["content"][0]["type"], "input_text");
        assert_eq!(body["input"][0]["content"][0]["text"], "hi");
        assert_eq!(body["instructions"], "Reply with OK.");
        assert_eq!(body["reasoning"]["effort"], "none");
        assert_eq!(body["store"], false);
        assert_eq!(body["stream"], true);
        assert_eq!(codex_body(Some("custom"))["model"], "custom");
    }
}
