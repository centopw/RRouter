//! Subscription usage windows, read from the provider that owns the account.
//!
//! These are the limits a plan already has (Claude's 5-hour and weekly windows, Codex's
//! primary/secondary windows). They are **not** operator quotas: RRouter only displays
//! them and caches the answer briefly, because every provider endpoint here is shared
//! with the vendor's own client and is cheap to hammer.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use base64::Engine as _;
use rrouter_core::{Error, Result};
use rrouter_router::{CapacityHint, CapacityHints};
use rrouter_store::Connection;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::state::AppState;

// Anthropic rate-limits this endpoint aggressively (it is the one Claude Code itself
// polls). Five minutes is the documented polite cadence; failures wait fifteen.
const SUCCESS_TTL: Duration = Duration::from_secs(300);
const FAILURE_TTL: Duration = Duration::from_secs(900);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// One subscription window: percent consumed plus when it rolls over.
#[derive(Debug, Clone, serde::Serialize)]
pub struct UsageWindow {
    /// Stable id: `five_hour`, `weekly`, `weekly_opus`, `extra`.
    pub id: String,
    pub label: String,
    /// 0–100, the percentage already consumed.
    pub used_percent: f64,
    pub window_seconds: Option<i64>,
    pub resets_at: Option<i64>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AccountUsage {
    /// String form: the JSON contract spells ids as strings everywhere else.
    pub connection_id: String,
    pub provider: String,
    pub name: Option<String>,
    pub plan: Option<String>,
    pub windows: Vec<UsageWindow>,
    /// Why no windows were returned (API key mode, unsupported provider, upstream error).
    pub error: Option<String>,
    pub fetched_at: i64,
}

struct Cached {
    at: Instant,
    value: AccountUsage,
}

static CACHE: LazyLock<Mutex<HashMap<Uuid, Cached>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// Providers whose plan limits this module knows how to read.
pub fn supports(provider: &str) -> bool {
    matches!(provider, "claude" | "codex" | "github")
}

/// Fetch one account's subscription windows, with a short success cache and a longer
/// failure cache so a broken provider endpoint is not called on every page load.
pub async fn account(state: &AppState, conn: &Connection) -> AccountUsage {
    if let Some(cached) = cache_get(conn.id) {
        return cached;
    }
    let value = match fetch(state, conn).await {
        Ok(windows) => AccountUsage {
            connection_id: conn.id.to_string(),
            provider: conn.provider.clone(),
            name: conn.name.clone(),
            plan: windows.plan,
            windows: windows.windows,
            error: None,
            fetched_at: crate::now_unix(),
        },
        Err(e) => {
            tracing::debug!(provider = %conn.provider, error = %e, "subscription usage fetch failed");
            AccountUsage {
                connection_id: conn.id.to_string(),
                provider: conn.provider.clone(),
                name: conn.name.clone(),
                plan: None,
                windows: Vec::new(),
                error: Some(explain(&e)),
                fetched_at: crate::now_unix(),
            }
        }
    };
    cache_put(conn.id, value.clone());
    value
}

/// Cached usage for one connection, without a provider call.
pub fn cached(id: Uuid) -> Option<AccountUsage> {
    cache_get(id)
}

/// Subscription hints for every connection with a fresh cached snapshot.
///
/// Routing must never wait on a plan-usage endpoint: the background refresh (or a
/// dashboard read) populates this cache, and an unobserved account simply has no hint.
pub fn cached_hints() -> CapacityHints {
    let map = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let mut hints = CapacityHints::new();
    for (connection, entry) in map.iter() {
        let ttl = if entry.value.error.is_none() {
            SUCCESS_TTL
        } else {
            FAILURE_TTL
        };
        if entry.at.elapsed() >= ttl || entry.value.error.is_some() {
            continue;
        }
        if let Some(hint) = hint(&entry.value) {
            hints.insert(*connection, hint);
        }
    }
    hints
}

/// The binding window: the most-consumed one, with the earliest reset among ties.
fn hint(usage: &AccountUsage) -> Option<CapacityHint> {
    let used = usage
        .windows
        .iter()
        .map(|window| window.used_percent)
        .fold(None::<f64>, |worst, used| {
            Some(worst.map_or(used, |worst| worst.max(used)))
        })?;
    let resets_at = usage
        .windows
        .iter()
        .filter(|window| (window.used_percent - used).abs() < f64::EPSILON)
        .filter_map(|window| window.resets_at)
        .min();
    Some(CapacityHint {
        used_percent: used,
        resets_at,
    })
}

/// Refresh every OAuth subscription account in the background.
///
/// `account` already applies a five-minute success TTL and a fifteen-minute failure TTL,
/// so a one-minute poll fetches each provider at a polite cadence rather than on every
/// request. The first pass waits a minute so a restart is never delayed by a provider.
pub fn spawn_refresh(state: std::sync::Arc<AppState>) {
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(60)).await;
        loop {
            if let Err(e) = refresh_once(&state).await {
                tracing::debug!(error = %e, "subscription usage refresh failed");
            }
            tokio::time::sleep(Duration::from_secs(60)).await;
        }
    });
}

async fn refresh_once(state: &AppState) -> Result<()> {
    let connections = state.store.list_connections(None).await?;
    for conn in connections {
        if !conn.is_active || conn.auth_type != "oauth" || !supports(&conn.provider) {
            continue;
        }
        // `account` caches its own answer; a cached entry costs no network I/O.
        let _ = account(state, &conn).await;
    }
    Ok(())
}

fn cache_get(id: Uuid) -> Option<AccountUsage> {
    let map = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let entry = map.get(&id)?;
    let ttl = if entry.value.error.is_none() {
        SUCCESS_TTL
    } else {
        FAILURE_TTL
    };
    (entry.at.elapsed() < ttl).then(|| entry.value.clone())
}

fn cache_put(id: Uuid, value: AccountUsage) {
    CACHE.lock().unwrap_or_else(|e| e.into_inner()).insert(
        id,
        Cached {
            at: Instant::now(),
            value,
        },
    );
}

/// A dashboard sentence, not an upstream dump: the raw body belongs in the log.
fn explain(error: &Error) -> String {
    match error {
        Error::Upstream {
            status: 401 | 403, ..
        } => "the provider rejected this account's token; sign in again".to_string(),
        Error::Upstream { status: 429, .. } => {
            "the provider is rate-limiting usage checks; retrying in a few minutes".to_string()
        }
        Error::Upstream { status, .. } => format!("the provider answered {status}"),
        Error::Unauthorized(message) => message.clone(),
        Error::Transport(_) | Error::Timeout(_) => "could not reach the provider".to_string(),
        _ => "subscription usage is unavailable right now".to_string(),
    }
}

struct Windows {
    plan: Option<String>,
    windows: Vec<UsageWindow>,
}

async fn fetch(state: &AppState, conn: &Connection) -> Result<Windows> {
    let Some(spec) = rrouter_auth::get(&conn.provider) else {
        return Err(Error::BadRequest(format!(
            "unknown provider {}",
            conn.provider
        )));
    };
    // An API key has no plan: asking the OAuth usage endpoint would only produce a 401
    // that reads like a broken account.
    if conn.auth_type != "oauth" {
        return Err(Error::Unauthorized(
            "API-key accounts have no plan windows".into(),
        ));
    }
    let creds = state
        .refresher
        .fresh(state.store.as_ref(), spec, conn.id)
        .await?;
    let token = creds
        .access_token
        .as_deref()
        .filter(|token| !token.is_empty())
        .ok_or_else(|| Error::Unauthorized("API-key accounts have no subscription usage".into()))?;
    match conn.provider.as_str() {
        "claude" => claude(state, token).await,
        "codex" => codex(state, token, creds.id_token.as_deref(), &creds.extra).await,
        "github" => github(state, spec, &creds).await,
        provider => Err(Error::BadRequest(format!(
            "{provider} does not expose subscription usage"
        ))),
    }
}

/// Claude Code's own `/usage` endpoint. `utilization` is the consumed percentage; buckets
/// that do not apply to a plan are absent or null.
async fn claude(state: &AppState, token: &str) -> Result<Windows> {
    let response = state
        .http
        .get("https://api.anthropic.com/api/oauth/usage")
        .header("authorization", format!("Bearer {token}"))
        .header("anthropic-beta", "oauth-2025-04-20")
        .header("user-agent", "rrouter")
        .timeout(REQUEST_TIMEOUT)
        .send()
        .await
        .map_err(|e| Error::Transport(format!("claude usage: {e}")))?;
    let status = response.status().as_u16();
    if !response.status().is_success() {
        let body: String = response
            .text()
            .await
            .unwrap_or_default()
            .chars()
            .take(256)
            .collect();
        return Err(Error::Upstream {
            provider: "claude".into(),
            status,
            body,
            payload: None,
        });
    }
    let body: Value = response
        .json()
        .await
        .map_err(|e| Error::Transport(format!("claude usage body: {e}")))?;
    claude_windows(&body)
}

/// Pure parser for Claude's usage payload (fixtures cover the shape without a network).
fn claude_windows(body: &Value) -> Result<Windows> {
    let mut windows = Vec::new();
    for (key, label) in [
        ("five_hour", "5 hour"),
        ("seven_day", "Weekly"),
        ("seven_day_opus", "Weekly · Opus"),
        ("seven_day_sonnet", "Weekly · Sonnet"),
    ] {
        let Some(bucket) = body.get(key).filter(|v| v.is_object()) else {
            continue;
        };
        let Some(used) = bucket.get("utilization").and_then(Value::as_f64) else {
            continue;
        };
        windows.push(UsageWindow {
            id: key.to_string(),
            label: label.to_string(),
            used_percent: used.clamp(0.0, 100.0),
            window_seconds: match key {
                "five_hour" => Some(5 * 3600),
                _ => Some(7 * 24 * 3600),
            },
            resets_at: bucket
                .get("resets_at")
                .and_then(Value::as_str)
                .and_then(parse_rfc3339),
        });
    }
    if let Some(extra) = body.get("extra_usage").filter(|v| v.is_object()) {
        if let Some(used) = extra.get("utilization").and_then(Value::as_f64) {
            windows.push(UsageWindow {
                id: "extra".into(),
                label: "Extra usage".into(),
                used_percent: used.clamp(0.0, 100.0),
                window_seconds: None,
                resets_at: None,
            });
        }
    }
    if windows.is_empty() {
        return Err(Error::Store("claude returned no usage windows".into()));
    }
    Ok(Windows {
        plan: None,
        windows,
    })
}

/// Codex surfaces the ChatGPT plan's rate-limit windows. The response shape changed
/// twice in 2026, so every window is parsed defensively and classified by its own
/// `limit_window_seconds` rather than by field position.
async fn codex(
    state: &AppState,
    token: &str,
    id_token: Option<&str>,
    extra: &serde_json::Map<String, Value>,
) -> Result<Windows> {
    let mut request = state
        .http
        .get("https://chatgpt.com/backend-api/wham/usage")
        .header("authorization", format!("Bearer {token}"))
        .header("accept", "application/json")
        .header("origin", "https://chatgpt.com")
        .header("referer", "https://chatgpt.com/")
        .header("user-agent", "Mozilla/5.0")
        .timeout(REQUEST_TIMEOUT);
    if let Some(account) = codex_account_id(id_token, extra) {
        request = request.header("ChatGPT-Account-Id", account);
    }
    let response = request
        .send()
        .await
        .map_err(|e| Error::Transport(format!("codex usage: {e}")))?;
    let status = response.status().as_u16();
    if !response.status().is_success() {
        let body: String = response
            .text()
            .await
            .unwrap_or_default()
            .chars()
            .take(256)
            .collect();
        return Err(Error::Upstream {
            provider: "codex".into(),
            status,
            body,
            payload: None,
        });
    }
    let body: Value = response
        .json()
        .await
        .map_err(|e| Error::Transport(format!("codex usage body: {e}")))?;
    codex_windows(&body)
}

/// Pure parser for Codex's WHAM payload.
fn codex_windows(body: &Value) -> Result<Windows> {
    let root = body
        .get("rate_limit")
        .or_else(|| body.get("rate_limits"))
        .ok_or_else(|| Error::Store("codex usage has no rate_limit object".into()))?;
    let mut windows = Vec::new();
    for (key, alias) in [
        ("primary_window", "five_hour"),
        ("secondary_window", "weekly"),
        ("five_hour", "five_hour"),
        ("weekly", "weekly"),
    ] {
        let Some(window) = root.get(key).filter(|v| v.is_object()) else {
            continue;
        };
        let Some(used) = percent_used(window) else {
            continue;
        };
        let seconds = window.get("limit_window_seconds").and_then(Value::as_i64);
        // Classify by the window itself; older payloads only carried positions.
        let weekly = seconds
            .map(|s| s >= 2 * 24 * 3600)
            .unwrap_or(alias == "weekly");
        let id = if weekly { "weekly" } else { "five_hour" };
        if windows.iter().any(|w: &UsageWindow| w.id == id) {
            continue;
        }
        windows.push(UsageWindow {
            id: id.to_string(),
            label: if weekly {
                "Weekly".into()
            } else {
                "5 hour".into()
            },
            used_percent: used,
            window_seconds: seconds,
            resets_at: reset_at(window),
        });
    }
    if windows.is_empty() {
        return Err(Error::Store("codex returned no usage windows".into()));
    }
    Ok(Windows {
        plan: body
            .get("plan_type")
            .and_then(Value::as_str)
            .map(str::to_string),
        windows,
    })
}

/// GitHub Copilot reports monthly request allowances per feature under
/// `quota_snapshots`. It authenticates with the derived copilot token.
async fn github(
    state: &AppState,
    spec: &'static rrouter_auth::ProviderSpec,
    creds: &rrouter_auth::Credentials,
) -> Result<Windows> {
    let mut request = state
        .http
        .get("https://api.github.com/copilot_internal/user")
        .header("accept", "application/json")
        .timeout(REQUEST_TIMEOUT);
    for (name, value) in rrouter_auth::build_headers(spec, creds)? {
        if let (Ok(name), Ok(value)) = (
            axum::http::HeaderName::from_bytes(name.as_bytes()),
            axum::http::HeaderValue::from_str(&value),
        ) {
            request = request.header(name, value);
        }
    }
    let response = request
        .send()
        .await
        .map_err(|e| Error::Transport(format!("copilot usage: {e}")))?;
    let status = response.status().as_u16();
    if !response.status().is_success() {
        let body: String = response
            .text()
            .await
            .unwrap_or_default()
            .chars()
            .take(256)
            .collect();
        return Err(Error::Upstream {
            provider: "github".into(),
            status,
            body,
            payload: None,
        });
    }
    let body: Value = response
        .json()
        .await
        .map_err(|e| Error::Transport(format!("copilot usage body: {e}")))?;
    github_windows(&body)
}

/// Pure parser for Copilot's snapshot payload.
fn github_windows(body: &Value) -> Result<Windows> {
    let mut windows = Vec::new();
    let snapshots = body
        .get("quota_snapshots")
        .and_then(Value::as_object)
        .ok_or_else(|| Error::Store("copilot usage has no quota_snapshots".into()))?;
    for (key, label) in [
        ("premium_interactions", "Premium"),
        ("chat", "Chat"),
        ("completions", "Completions"),
    ] {
        let Some(snapshot) = snapshots.get(key).filter(|v| v.is_object()) else {
            continue;
        };
        if snapshot.get("unlimited").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let used = snapshot
            .get("percent_remaining")
            .and_then(Value::as_f64)
            .map(|left| (100.0 - left).clamp(0.0, 100.0))
            .or_else(|| {
                let remaining = snapshot.get("remaining").and_then(Value::as_f64)?;
                let entitlement = snapshot.get("entitlement").and_then(Value::as_f64)?;
                (entitlement > 0.0)
                    .then(|| ((entitlement - remaining) / entitlement * 100.0).clamp(0.0, 100.0))
            });
        let Some(used) = used else { continue };
        let reset = body
            .get("quota_reset_date_utc")
            .and_then(Value::as_str)
            .and_then(parse_rfc3339)
            .or_else(|| {
                snapshot
                    .get("quota_reset_at")
                    .and_then(Value::as_str)
                    .and_then(parse_rfc3339)
            });
        windows.push(UsageWindow {
            id: key.replace('_', "-"),
            label: label.to_string(),
            used_percent: used,
            window_seconds: Some(30 * 24 * 3600),
            resets_at: reset,
        });
    }
    if windows.is_empty() {
        return Err(Error::Store(
            "copilot reports no finite quota windows".into(),
        ));
    }
    Ok(Windows {
        plan: body
            .get("copilot_plan")
            .and_then(Value::as_str)
            .map(str::to_string),
        windows,
    })
}

/// `used_percent`, or `percent_left` converted to used.
fn percent_used(window: &Value) -> Option<f64> {
    if let Some(used) = window.get("used_percent").and_then(Value::as_f64) {
        return Some(used.clamp(0.0, 100.0));
    }
    window
        .get("percent_left")
        .and_then(Value::as_f64)
        .map(|left| (100.0 - left).clamp(0.0, 100.0))
}

/// `reset_time_ms` (ms), `reset_at`/`resetsAt` (seconds or RFC 3339).
fn reset_at(window: &Value) -> Option<i64> {
    for key in ["reset_time_ms", "reset_at", "resetsAt"] {
        let Some(value) = window.get(key) else {
            continue;
        };
        if let Some(number) = value.as_i64() {
            return Some(if key == "reset_time_ms" || number >= 100_000_000_000 {
                number / 1000
            } else {
                number
            });
        }
        if let Some(text) = value.as_str() {
            if let Some(stamp) = parse_rfc3339(text) {
                return Some(stamp);
            }
            if let Ok(number) = text.parse::<i64>() {
                return Some(if number >= 100_000_000_000 {
                    number / 1000
                } else {
                    number
                });
            }
        }
    }
    None
}

/// The ChatGPT account id lives in the OAuth id token (or an extra credential field).
fn codex_account_id(
    id_token: Option<&str>,
    extra: &serde_json::Map<String, Value>,
) -> Option<String> {
    for key in ["chatgpt_account_id", "account_id"] {
        if let Some(value) = extra.get(key).and_then(Value::as_str) {
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    let payload = id_token?.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .ok()?;
    let claims: Value = serde_json::from_slice(&bytes).ok()?;
    claims
        .pointer("/https:~1~1api.openai.com~1auth/chatgpt_account_id")
        .or_else(|| claims.get("chatgpt_account_id"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Minimal RFC 3339 (`2026-04-08T18:59:59Z`, offset ignored after parsing as UTC).
fn parse_rfc3339(raw: &str) -> Option<i64> {
    let bytes = raw.as_bytes();
    if bytes.len() < 19 {
        return None;
    }
    let number = |range: std::ops::Range<usize>| raw.get(range)?.parse::<i64>().ok();
    let (year, month, day) = (number(0..4)?, number(5..7)?, number(8..10)?);
    let (hour, minute, second) = (number(11..13)?, number(14..16)?, number(17..19)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    Some(days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second)
}

/// Days since 1970-01-01 (H. Hinnant), duplicated from the store so this module does not
/// depend on a private helper.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// JSON for the admin API: every account with a known usage endpoint.
pub async fn list(state: &AppState) -> Result<Value> {
    let connections = state.store.list_connections(None).await?;
    let targets: Vec<Connection> = connections
        .into_iter()
        .filter(|conn| supports(&conn.provider))
        .collect();
    let mut accounts = Vec::with_capacity(targets.len());
    for conn in targets {
        accounts.push(account(state, &conn).await);
    }
    Ok(json!({ "accounts": accounts }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_payload_skips_null_buckets() {
        let body = json!({
            "five_hour": {"utilization": 6.0, "resets_at": "2026-04-08T18:59:59Z"},
            "seven_day": {"utilization": 35.0, "resets_at": "2026-04-14T16:59:59Z"},
            "seven_day_opus": null,
            "seven_day_sonnet": {"utilization": 21.0, "resets_at": "2026-04-14T17:59:59Z"},
            "extra_usage": {"is_enabled": true, "utilization": 12.5}
        });
        let windows = claude_windows(&body).expect("windows");
        let ids: Vec<&str> = windows.windows.iter().map(|w| w.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["five_hour", "seven_day", "seven_day_sonnet", "extra"]
        );
        assert_eq!(windows.windows[0].used_percent, 6.0);
        assert_eq!(windows.windows[0].resets_at, Some(1_775_674_799));
        assert_eq!(windows.windows[3].window_seconds, None);
    }

    #[test]
    fn codex_classifies_windows_by_duration() {
        // Current shape: one weekly window in the primary slot.
        let single = json!({
            "plan_type": "team",
            "rate_limit": {
                "primary_window": {"used_percent": 1, "limit_window_seconds": 604800, "reset_at": 1785260773},
                "secondary_window": null
            }
        });
        let windows = codex_windows(&single).expect("windows");
        assert_eq!(windows.plan.as_deref(), Some("team"));
        assert_eq!(windows.windows.len(), 1);
        assert_eq!(windows.windows[0].id, "weekly");
        assert_eq!(windows.windows[0].used_percent, 1.0);
        assert_eq!(windows.windows[0].resets_at, Some(1_785_260_773));

        // Legacy shape: both windows, percent_left instead of used.
        let legacy = json!({
            "rate_limits": {
                "five_hour": {"percent_left": 73.4, "limit_window_seconds": 18000, "reset_time_ms": 1_775_000_000_000i64},
                "weekly": {"percent_left": 87.1, "limit_window_seconds": 604800, "reset_time_ms": 1_776_000_000_000i64}
            }
        });
        let windows = codex_windows(&legacy).expect("windows");
        assert_eq!(windows.windows[0].id, "five_hour");
        assert!((windows.windows[0].used_percent - 26.6).abs() < 0.01);
        assert_eq!(windows.windows[1].id, "weekly");
        assert_eq!(windows.windows[0].resets_at, Some(1_775_000_000));
    }

    #[test]
    fn copilot_skips_unlimited_and_computes_used() {
        let body = json!({
            "copilot_plan": "individual",
            "quota_reset_date_utc": "2026-05-01T00:00:00Z",
            "quota_snapshots": {
                "premium_interactions": {"entitlement": 300, "remaining": 225, "percent_remaining": 75.0},
                "chat": {"unlimited": true},
                "completions": {"percent_remaining": 100.0}
            }
        });
        let windows = github_windows(&body).expect("windows");
        let ids: Vec<&str> = windows.windows.iter().map(|w| w.id.as_str()).collect();
        assert_eq!(ids, vec!["premium-interactions", "completions"]);
        assert_eq!(windows.windows[0].used_percent, 25.0);
        assert_eq!(windows.windows[1].used_percent, 0.0);
        assert!(windows.windows[0].resets_at.is_some());
    }

    #[test]
    fn rfc3339_matches_known_epoch() {
        assert_eq!(parse_rfc3339("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_rfc3339("2026-04-08T18:59:59Z"), Some(1_775_674_799));
        assert_eq!(parse_rfc3339("nope"), None);
    }

    #[test]
    fn percent_left_is_inverted() {
        assert_eq!(percent_used(&json!({"used_percent": 12.5})), Some(12.5));
        assert_eq!(percent_used(&json!({"percent_left": 78.0})), Some(22.0));
        assert_eq!(percent_used(&json!({})), None);
    }

    #[test]
    fn reset_at_accepts_all_three_shapes() {
        assert_eq!(
            reset_at(&json!({"reset_at": 1_775_000_000})),
            Some(1_775_000_000)
        );
        assert_eq!(
            reset_at(&json!({"reset_time_ms": 1_775_000_000_000i64})),
            Some(1_775_000_000)
        );
        assert_eq!(
            reset_at(&json!({"resetsAt": "2026-04-08T18:59:59Z"})),
            Some(1_775_674_799)
        );
    }
}

#[cfg(test)]
mod capacity_tests {
    use super::*;

    fn usage(windows: &[(f64, Option<i64>)]) -> AccountUsage {
        AccountUsage {
            connection_id: Uuid::new_v4().to_string(),
            provider: "claude".into(),
            name: None,
            plan: None,
            windows: windows
                .iter()
                .enumerate()
                .map(|(index, (used, resets_at))| UsageWindow {
                    id: format!("w{index}"),
                    label: format!("window {index}"),
                    used_percent: *used,
                    window_seconds: None,
                    resets_at: *resets_at,
                })
                .collect(),
            error: None,
            fetched_at: 0,
        }
    }

    #[test]
    fn the_most_consumed_window_is_the_binding_one() {
        let hint = hint(&usage(&[
            (20.0, Some(900)),
            (95.0, Some(300)),
            (10.0, None),
        ]))
        .expect("a hint");
        assert_eq!(hint.used_percent, 95.0);
        assert_eq!(hint.resets_at, Some(300));
    }

    #[test]
    fn equal_windows_report_the_earliest_reset() {
        let hint = hint(&usage(&[(80.0, Some(900)), (80.0, Some(300))])).expect("a hint");
        assert_eq!(hint.used_percent, 80.0);
        assert_eq!(hint.resets_at, Some(300), "the soonest reset wins a tie");
    }

    #[test]
    fn no_windows_yields_no_hint() {
        assert!(hint(&usage(&[])).is_none());
    }

    #[test]
    fn cached_hints_ignore_a_stale_snapshot() {
        let value = usage(&[(95.0, Some(300))]);
        let id = Uuid::parse_str(&value.connection_id).expect("id");
        CACHE.lock().unwrap_or_else(|e| e.into_inner()).insert(
            id,
            Cached {
                at: Instant::now()
                    .checked_sub(SUCCESS_TTL + Duration::from_secs(1))
                    .expect("clock"),
                value,
            },
        );
        assert!(
            cached_hints().get(id).is_none(),
            "an expired snapshot is not headroom data"
        );
        CACHE.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
    }

    #[test]
    fn cached_hints_skip_error_entries() {
        let ok = usage(&[(95.0, Some(300))]);
        let mut broken = usage(&[(99.0, Some(1))]);
        broken.error = Some("provider rejected the token".into());
        let ok_id = Uuid::parse_str(&ok.connection_id).expect("id");
        let broken_id = Uuid::parse_str(&broken.connection_id).expect("id");
        {
            let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
            for value in [&ok, &broken] {
                cache.insert(
                    Uuid::parse_str(&value.connection_id).expect("id"),
                    Cached {
                        at: Instant::now(),
                        value: value.clone(),
                    },
                );
            }
        }
        let hints = cached_hints();
        assert_eq!(hints.used_percent(ok_id), Some(95.0));
        assert!(
            hints.get(broken_id).is_none(),
            "a failed usage fetch is not headroom data"
        );
        CACHE
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&ok_id);
        CACHE
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&broken_id);
    }
}
