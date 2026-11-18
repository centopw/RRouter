//! Gateway-wide settings blob (`brand_name`, `brand_color`, `logo_url`, `footer_text`).
//!
//! `rrouter-store` has no key/value accessor, and this crate owns the settings surface,
//! so the table lives here: one row (`key TEXT PRIMARY KEY, value TEXT`). It is only
//! touched by admin requests — never on the proxy path.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use rrouter_core::{Error, Result};
use rusqlite::OptionalExtension;
use serde_json::{json, Map, Value};

use crate::state::AppState;

/// Keys a white-label deployment may set on `brand`.
pub const BRAND_KEYS: &[&str] = &["brand_name", "brand_color", "logo_url", "footer_text"];

/// Token Saver (RTK) switch, readable on the proxy path without touching the settings
/// database. Default on; a request can opt out with `x-rrouter-rtk: off`.
static RTK: AtomicBool = AtomicBool::new(true);

/// Opt-in storage of request and response bodies, readable on the proxy path. Default
/// off: while off the proxy writes no capture row at all, so nothing needs to be cleaned
/// up when an operator changes their mind.
static CAPTURE_BODIES: AtomicBool = AtomicBool::new(false);

/// Per-attempt traces, readable on the proxy path. Default on: the request drawer's
/// fallback trail is the first thing an operator reaches for when a model misbehaves.
static RECORD_ATTEMPTS: AtomicBool = AtomicBool::new(true);

/// The security audit trail, readable on every mutating handler. Default on: turning it
/// off is a deliberate trade of accountability for disk, never an accident.
static RECORD_AUDIT: AtomicBool = AtomicBool::new(true);

/// Ten years, the ceiling the environment knob already enforces.
pub const MAX_RETENTION_DAYS: usize = 3650;

pub fn rtk() -> bool {
    RTK.load(Ordering::Relaxed)
}

/// Whether the proxy should store request and response bodies for a request.
pub fn capture_bodies() -> bool {
    CAPTURE_BODIES.load(Ordering::Relaxed)
}

/// Whether each dispatch attempt is written to `attempts`.
pub fn record_attempts() -> bool {
    RECORD_ATTEMPTS.load(Ordering::Relaxed)
}

/// Whether mutating handlers write their row to `audit_events`.
pub fn record_audit() -> bool {
    RECORD_AUDIT.load(Ordering::Relaxed)
}

pub fn defaults() -> Value {
    json!({
        "brand_name": "RRouter",
        "rtk": true,
        "system_logs": false,
        "capture_bodies": false,
        "record_attempts": true,
        "record_audit": true,
        "usage_retention_days": 0,
        "attempts_retention_days": null,
        "capture_retention_days": null,
        "audit_retention_days": null,
        "system_log_retention_days": null,
    })
}

/// Load the stored switch once at startup (and after a settings write).
pub async fn prime(state: &Arc<AppState>) -> Result<()> {
    let stored = load(state).await?;
    RTK.store(
        stored.get("rtk").and_then(Value::as_bool).unwrap_or(true),
        Ordering::Relaxed,
    );
    CAPTURE_BODIES.store(
        stored
            .get("capture_bodies")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        Ordering::Relaxed,
    );
    RECORD_ATTEMPTS.store(
        stored
            .get("record_attempts")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        Ordering::Relaxed,
    );
    RECORD_AUDIT.store(
        stored
            .get("record_audit")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        Ordering::Relaxed,
    );
    crate::syslog::set_enabled(
        stored
            .get("system_logs")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    );
    Ok(())
}

/// Switches the dashboard may flip; each must stay a boolean.
const TOGGLE_KEYS: &[&str] = &[
    "rtk",
    "system_logs",
    "capture_bodies",
    "record_attempts",
    "record_audit",
];

/// Retention knobs, in days. `null` (or absent) means "follow the usage window", which is
/// the behaviour that existed before each store got its own answer.
const RETENTION_KEYS: &[&str] = &[
    "usage_retention_days",
    "attempts_retention_days",
    "capture_retention_days",
    "audit_retention_days",
    "system_log_retention_days",
];

/// How long each stored artifact is kept, resolved for one sweep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Retention {
    pub usage_days: usize,
    pub attempts_days: usize,
    pub captures_days: usize,
    pub audit_days: usize,
    pub system_logs_days: usize,
}

/// Resolve the retention policy: the stored blob wins over the environment, and a store
/// without its own number follows the usage window. `0` means keep until deleted by hand.
pub async fn retention(state: &Arc<AppState>) -> Result<Retention> {
    let stored = read_blob(state).await?.unwrap_or(Value::Null);
    let usage_days = days_of(&stored, "usage_retention_days")
        .unwrap_or(state.config.load().usage_retention_days);
    Ok(Retention {
        usage_days,
        attempts_days: days_of(&stored, "attempts_retention_days").unwrap_or(usage_days),
        captures_days: days_of(&stored, "capture_retention_days").unwrap_or(usage_days),
        audit_days: days_of(&stored, "audit_retention_days").unwrap_or(usage_days),
        system_logs_days: days_of(&stored, "system_log_retention_days").unwrap_or(0),
    })
}

fn days_of(settings: &Value, key: &str) -> Option<usize> {
    settings.get(key).and_then(Value::as_u64).map(|days| {
        usize::try_from(days)
            .unwrap_or(usize::MAX)
            .min(MAX_RETENTION_DAYS)
    })
}

/// The stored settings merged over the defaults.
pub async fn load(state: &Arc<AppState>) -> Result<Value> {
    let mut settings = defaults();
    let Some(stored) = read_blob(state).await? else {
        return Ok(settings);
    };
    if let (Some(base), Some(extra)) = (settings.as_object_mut(), stored.as_object()) {
        for (k, v) in extra {
            base.insert(k.clone(), v.clone());
        }
    }
    Ok(settings)
}

/// Merge a patch into the stored settings and return the merged value.
pub async fn merge(state: &Arc<AppState>, patch: Value) -> Result<Value> {
    let Value::Object(patch) = patch else {
        return Err(Error::BadRequest("settings must be a JSON object".into()));
    };
    validate(&patch)?;
    let mut current = match load(state).await? {
        Value::Object(map) => map,
        _ => Map::new(),
    };
    for (k, v) in patch {
        current.insert(k, v);
    }
    if let Some(rtk) = current.get("rtk").and_then(Value::as_bool) {
        RTK.store(rtk, Ordering::Relaxed);
    }
    if let Some(capture) = current.get("capture_bodies").and_then(Value::as_bool) {
        CAPTURE_BODIES.store(capture, Ordering::Relaxed);
    }
    if let Some(logs) = current.get("system_logs").and_then(Value::as_bool) {
        crate::syslog::set_enabled(logs);
    }
    if let Some(on) = current.get("record_attempts").and_then(Value::as_bool) {
        RECORD_ATTEMPTS.store(on, Ordering::Relaxed);
    }
    if let Some(on) = current.get("record_audit").and_then(Value::as_bool) {
        RECORD_AUDIT.store(on, Ordering::Relaxed);
    }
    let merged = Value::Object(current);
    write_blob(state, &merged).await?;
    Ok(merged)
}

/// Refuse a patch the sweep cannot honour: a switch that is not a boolean, or a retention
/// that is neither `null` nor a day count inside the ceiling. A silently clamped value
/// would report a policy the gateway is not running.
fn validate(patch: &Map<String, Value>) -> Result<()> {
    for key in TOGGLE_KEYS {
        match patch.get(*key) {
            None | Some(Value::Bool(_)) => {}
            Some(_) => {
                return Err(Error::BadRequest(format!("{key} must be true or false")));
            }
        }
    }
    for key in RETENTION_KEYS {
        match patch.get(*key) {
            None | Some(Value::Null) => {}
            Some(Value::Number(n)) => {
                let days = n.as_u64().filter(|days| *days <= MAX_RETENTION_DAYS as u64);
                if days.is_none() {
                    return Err(Error::BadRequest(format!(
                        "{key} must be a whole number of days between 0 and {MAX_RETENTION_DAYS}"
                    )));
                }
            }
            Some(_) => {
                return Err(Error::BadRequest(format!(
                    "{key} must be a whole number of days between 0 and {MAX_RETENTION_DAYS}, or null"
                )));
            }
        }
    }
    Ok(())
}

/// The `ui` settings object: `{brand: {...}, panels: [...]}` with defaults applied.
pub async fn ui_settings(state: &Arc<AppState>) -> Result<Value> {
    let stored = load(state).await?;
    let brand = brand_of(&stored);
    let panels = stored
        .get("panels")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    Ok(json!({ "brand": brand, "panels": panels, "rtk": rtk() }))
}

/// Only the four documented brand keys, with `RRouter` defaults.
pub fn brand_of(settings: &Value) -> Value {
    let mut brand = json!({ "brand_name": "RRouter", "brand_color": "#7c5cff" });
    let Some(obj) = brand.as_object_mut() else {
        // `json!` above is always an object; degrade instead of panicking if that changes.
        return brand;
    };
    for key in BRAND_KEYS {
        if let Some(v) = settings.get(*key) {
            obj.insert((*key).to_string(), v.clone());
        }
    }
    brand
}

/// Settings live in their own SQLite file beside the main DB, not in it: `rrouter-store`
/// owns the main connection and has no key/value surface, and a second writer on the same
/// file would only add lock contention.
fn settings_path(state: &AppState) -> std::path::PathBuf {
    state.config.load().data_dir.join("settings.db")
}

fn open(state: &AppState) -> Result<rusqlite::Connection> {
    let data_dir = state.config.load().data_dir.clone();
    if let Err(e) = std::fs::create_dir_all(&data_dir) {
        return Err(Error::Store(format!("create data dir: {e}")));
    }
    let conn = rusqlite::Connection::open(settings_path(state))
        .map_err(|e| Error::Store(format!("open settings db: {e}")))?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
    )
    .map_err(|e| Error::Store(format!("migrate settings: {e}")))?;
    Ok(conn)
}

async fn read_blob(state: &Arc<AppState>) -> Result<Option<Value>> {
    let state = Arc::clone(state);
    tokio::task::spawn_blocking(move || {
        let conn = open(&state)?;
        let raw: Option<String> = conn
            .query_row("SELECT value FROM settings WHERE key = 'ui'", [], |row| {
                row.get(0)
            })
            .optional()
            .map_err(|e| Error::Store(format!("read settings: {e}")))?;
        Ok(raw.and_then(|r| serde_json::from_str(&r).ok()))
    })
    .await
    .map_err(|e| Error::Store(format!("settings task: {e}")))?
}

async fn write_blob(state: &Arc<AppState>, value: &Value) -> Result<()> {
    let raw = serde_json::to_string(value)?;
    let state = Arc::clone(state);
    tokio::task::spawn_blocking(move || {
        let conn = open(&state)?;
        conn.execute(
            "INSERT INTO settings (key, value) VALUES ('ui', ?1) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [raw],
        )
        .map_err(|e| Error::Store(format!("write settings: {e}")))?;
        Ok(())
    })
    .await
    .map_err(|e| Error::Store(format!("settings task: {e}")))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brand_defaults_and_overrides() {
        let brand = brand_of(&json!({"brand_name": "Acme AI", "brand_color": "#000", "junk": 1}));
        assert_eq!(brand["brand_name"], "Acme AI");
        assert_eq!(brand["brand_color"], "#000");
        assert_eq!(brand["footer_text"], Value::Null);
        assert!(brand.get("junk").is_none());
    }

    /// A retention the sweep cannot honour is refused rather than clamped: reporting a
    /// policy the gateway is not running is worse than rejecting the write.
    #[test]
    fn retention_patches_are_validated() {
        let ok = serde_json::json!({"usage_retention_days": 30, "audit_retention_days": null});
        assert!(validate(ok.as_object().expect("object")).is_ok());

        let negative = serde_json::json!({"usage_retention_days": -1});
        assert!(validate(negative.as_object().expect("object")).is_err());

        let past_ceiling = serde_json::json!({"capture_retention_days": MAX_RETENTION_DAYS + 1});
        assert!(validate(past_ceiling.as_object().expect("object")).is_err());

        let stringy = serde_json::json!({"audit_retention_days": "never"});
        assert!(validate(stringy.as_object().expect("object")).is_err());

        let wrong_toggle = serde_json::json!({"record_attempts": 1});
        assert!(validate(wrong_toggle.as_object().expect("object")).is_err());
    }

    /// A store without its own number follows the usage window; `0` is kept, not turned
    /// into a cutoff of "now".
    #[test]
    fn days_follow_usage_until_overridden() {
        let stored = serde_json::json!({"attempts_retention_days": 7});
        assert_eq!(days_of(&stored, "attempts_retention_days"), Some(7));
        assert_eq!(days_of(&stored, "capture_retention_days"), None);
        assert_eq!(
            days_of(&json!({"usage_retention_days": 0}), "usage_retention_days"),
            Some(0)
        );
        let capped = serde_json::json!({"usage_retention_days": 999_999});
        assert_eq!(
            days_of(&capped, "usage_retention_days"),
            Some(MAX_RETENTION_DAYS),
            "a stored value above the ceiling is read at the ceiling (validation only admits legal writes)"
        );
    }

    /// Capture of request/response bodies is opt-in: the default blob says off and the
    /// atomic the proxy reads starts off, so an untouched install stores no bodies.
    #[test]
    fn capture_is_off_by_default() {
        assert_eq!(defaults()["capture_bodies"], json!(false));
        assert!(!capture_bodies());
    }
}
