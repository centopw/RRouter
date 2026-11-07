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
/// database. Default on: 9Router ships the same feature on, and a request can opt out with
/// `x-rrouter-rtk: off`.
static RTK: AtomicBool = AtomicBool::new(true);

pub fn rtk() -> bool {
    RTK.load(Ordering::Relaxed)
}

pub fn defaults() -> Value {
    json!({ "brand_name": "RRouter", "rtk": true })
}

/// Load the stored switch once at startup (and after a settings write).
pub async fn prime(state: &Arc<AppState>) -> Result<()> {
    let stored = load(state).await?;
    RTK.store(stored.get("rtk").and_then(Value::as_bool).unwrap_or(true), Ordering::Relaxed);
    Ok(())
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
    let merged = Value::Object(current);
    write_blob(state, &merged).await?;
    Ok(merged)
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
    state.config.data_dir.join("settings.db")
}

fn open(state: &AppState) -> Result<rusqlite::Connection> {
    if let Err(e) = std::fs::create_dir_all(&state.config.data_dir) {
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
            .query_row("SELECT value FROM settings WHERE key = 'ui'", [], |row| row.get(0))
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
}
