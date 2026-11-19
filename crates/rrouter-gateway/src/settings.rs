//! Gateway-wide settings blob: the brand keys, the `compression` block
//! (`rtk`, `caveman`, `caveman_level`), the `capability_defaults` block (the operator's
//! default provider/model for search, rerank and embeddings), the switches and the
//! retention knobs.
//!
//! `rrouter-store` has no key/value accessor, and this crate owns the settings surface,
//! so the table lives here: one row (`key TEXT PRIMARY KEY, value TEXT`). It is only
//! touched by admin requests — never on the proxy path: the switches the proxy reads are
//! mirrored into atomics by [`prime`] and [`merge`].

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, RwLock};

use rrouter_core::{Error, Result};
use rusqlite::OptionalExtension;
use serde_json::{json, Map, Value};

use crate::state::AppState;

/// Keys a white-label deployment may set on `brand`.
pub const BRAND_KEYS: &[&str] = &["brand_name", "brand_color", "logo_url", "footer_text"];

/// Token Saver (RTK) switch, readable on the proxy path without touching the settings
/// database. Default on; a request can opt out with `x-rrouter-rtk: off`.
static RTK: AtomicBool = AtomicBool::new(true);

/// Caveman terse-output switch, readable on the proxy path. Default off: the injected
/// instruction changes how every model reply reads, so it is an explicit choice.
static CAVEMAN: AtomicBool = AtomicBool::new(false);

/// Caveman level, stored as [`rrouter_caveman::Level`]'s index (`0` lite, `1` full,
/// `2` ultra). Default `full`.
static CAVEMAN_LEVEL: AtomicU8 = AtomicU8::new(1);

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

/// Which non-chat endpoint a stored default applies to.
#[derive(Clone, Copy)]
pub enum CapabilityDefault {
    Search,
    Rerank,
    Embeddings,
}

impl CapabilityDefault {
    const ALL: [Self; 3] = [Self::Search, Self::Rerank, Self::Embeddings];

    /// The settings sub-key and wire name for this capability.
    pub fn key(self) -> &'static str {
        match self {
            Self::Search => "search",
            Self::Rerank => "rerank",
            Self::Embeddings => "embeddings",
        }
    }

    fn slot(self) -> usize {
        match self {
            Self::Search => 0,
            Self::Rerank => 1,
            Self::Embeddings => 2,
        }
    }
}

/// The stored capability defaults, mirrored out of SQLite so a request that omits its
/// model reads one lock instead of the settings database.
static CAPABILITY_DEFAULTS: RwLock<[Option<String>; 3]> = RwLock::new([None, None, None]);

/// The operator's default `provider` or `provider/model` for `cap`, or `None` when the
/// endpoint keeps its own "missing model" answer.
pub fn capability_default(cap: CapabilityDefault) -> Option<String> {
    CAPABILITY_DEFAULTS
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())[cap.slot()]
    .clone()
}

/// Ten years, the ceiling the environment knob already enforces.
pub const MAX_RETENTION_DAYS: usize = 3650;

pub fn rtk() -> bool {
    RTK.load(Ordering::Relaxed)
}

/// Whether the proxy should append the Caveman instruction to an inbound request.
pub fn caveman() -> bool {
    CAVEMAN.load(Ordering::Relaxed)
}

/// The configured Caveman level, readable on the proxy path.
pub fn caveman_level() -> rrouter_caveman::Level {
    match CAVEMAN_LEVEL.load(Ordering::Relaxed) {
        0 => rrouter_caveman::Level::Lite,
        2 => rrouter_caveman::Level::Ultra,
        _ => rrouter_caveman::Level::Full,
    }
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
        "system_logs": false,
        "capture_bodies": false,
        "record_attempts": true,
        "record_audit": true,
        "usage_retention_days": 0,
        "attempts_retention_days": null,
        "capture_retention_days": null,
        "audit_retention_days": null,
        "system_log_retention_days": null,
        "capability_defaults": capability_defaults_defaults(),
    })
}

/// The compression defaults: RTK on (preserving the behaviour that predates the
/// `compression` block), Caveman off at `full`.
fn compression_defaults() -> Value {
    json!({ "rtk": true, "caveman": false, "caveman_level": "full" })
}

/// The boolean sub-keys of `compression`; every other sub-key is ignored.
const COMPRESSION_BOOL_KEYS: &[&str] = &["rtk", "caveman"];
const CAVEMAN_LEVEL_KEY: &str = "caveman_level";

/// Resolve the effective compression block: the defaults, then the stored sub-keys, and —
/// when the block itself is absent — the legacy top-level `rtk` switch. The old key is
/// left in the stored blob untouched so a rollback still honours it.
fn compression_of(settings: &Value) -> Value {
    let mut compression = compression_defaults();
    let Some(out) = compression.as_object_mut() else {
        // `json!` above is always an object; degrade instead of panicking.
        return compression;
    };
    match settings.get("compression") {
        Some(Value::Object(stored)) => {
            for key in COMPRESSION_BOOL_KEYS {
                if let Some(value) = stored.get(*key).and_then(Value::as_bool) {
                    out.insert((*key).to_string(), Value::Bool(value));
                }
            }
            if let Some(level) = stored
                .get(CAVEMAN_LEVEL_KEY)
                .and_then(Value::as_str)
                .and_then(rrouter_caveman::Level::parse)
            {
                out.insert(
                    CAVEMAN_LEVEL_KEY.to_string(),
                    Value::String(level.as_str().to_string()),
                );
            }
        }
        _ => {
            if let Some(rtk) = settings.get("rtk").and_then(Value::as_bool) {
                out.insert("rtk".to_string(), Value::Bool(rtk));
            }
        }
    }
    compression
}

/// Fold a validated partial `compression` patch over the current block.
fn merge_compression(current: Option<&Value>, patch: &Map<String, Value>) -> Value {
    let mut compression = current.cloned().unwrap_or_else(compression_defaults);
    let Some(out) = compression.as_object_mut() else {
        return compression_defaults();
    };
    for key in COMPRESSION_BOOL_KEYS {
        if let Some(value) = patch.get(*key) {
            out.insert((*key).to_string(), value.clone());
        }
    }
    if let Some(value) = patch.get(CAVEMAN_LEVEL_KEY) {
        out.insert(CAVEMAN_LEVEL_KEY.to_string(), value.clone());
    }
    compression
}

/// Mirror the compression block into the atomics the proxy reads.
fn apply_compression(settings: &Value) {
    let compression = compression_of(settings);
    RTK.store(
        compression["rtk"].as_bool().unwrap_or(true),
        Ordering::Relaxed,
    );
    CAVEMAN.store(
        compression["caveman"].as_bool().unwrap_or(false),
        Ordering::Relaxed,
    );
    let level = compression[CAVEMAN_LEVEL_KEY]
        .as_str()
        .and_then(rrouter_caveman::Level::parse)
        .unwrap_or(rrouter_caveman::Level::Full);
    CAVEMAN_LEVEL.store(level as u8, Ordering::Relaxed);
}

/// Longest accepted capability default, in characters.
const MAX_CAPABILITY_DEFAULT_CHARS: usize = 200;

/// The `capability_defaults` block with every capability present and unset.
fn capability_defaults_defaults() -> Value {
    json!({ "search": null, "rerank": null, "embeddings": null })
}

/// A stored value the request path may use: non-empty, already trimmed, and short enough
/// to be a reference rather than a body. Validation rejects anything else on write; this
/// is the read-side guard for a blob edited by hand.
fn is_valid_capability_default(value: &str) -> bool {
    !value.is_empty()
        && value.trim() == value
        && value.chars().count() <= MAX_CAPABILITY_DEFAULT_CHARS
}

/// Resolve the effective `capability_defaults` block: unknown sub-keys dropped, invalid
/// values read as unset.
fn capability_defaults_of(settings: &Value) -> Value {
    let mut out = capability_defaults_defaults();
    let stored = settings
        .get("capability_defaults")
        .and_then(Value::as_object);
    if let (Some(out), Some(stored)) = (out.as_object_mut(), stored) {
        for cap in CapabilityDefault::ALL {
            if let Some(value) = stored
                .get(cap.key())
                .and_then(Value::as_str)
                .filter(|value| is_valid_capability_default(value))
            {
                out.insert(cap.key().to_string(), Value::String(value.to_string()));
            }
        }
    }
    out
}

/// Fold a validated partial `capability_defaults` patch over the current block: a patch
/// that names one capability keeps the other two, exactly as `compression` does.
fn merge_capability_defaults(current: Option<&Value>, patch: &Map<String, Value>) -> Value {
    let mut block = capability_defaults_defaults();
    let Some(out) = block.as_object_mut() else {
        return block;
    };
    if let Some(current) = current.and_then(Value::as_object) {
        for cap in CapabilityDefault::ALL {
            if let Some(value) = current
                .get(cap.key())
                .and_then(Value::as_str)
                .filter(|value| is_valid_capability_default(value))
            {
                out.insert(cap.key().to_string(), Value::String(value.to_string()));
            }
        }
    }
    for cap in CapabilityDefault::ALL {
        if let Some(value) = patch.get(cap.key()) {
            out.insert(cap.key().to_string(), value.clone());
        }
    }
    block
}

/// Mirror the capability defaults into the lock the proxy and the API read.
fn apply_capability_defaults(settings: &Value) {
    let block = capability_defaults_of(settings);
    let mut slots: [Option<String>; 3] = [None, None, None];
    for cap in CapabilityDefault::ALL {
        slots[cap.slot()] = block
            .get(cap.key())
            .and_then(Value::as_str)
            .map(str::to_string);
    }
    *CAPABILITY_DEFAULTS
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = slots;
}

/// Load the stored switch once at startup (and after a settings write).
pub async fn prime(state: &Arc<AppState>) -> Result<()> {
    let stored = load(state).await?;
    apply_compression(&stored);
    apply_capability_defaults(&stored);
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

/// The stored settings merged over the defaults, with the compression block resolved.
pub async fn load(state: &Arc<AppState>) -> Result<Value> {
    let mut settings = defaults();
    if let Some(stored) = read_blob(state).await? {
        if let (Some(base), Some(extra)) = (settings.as_object_mut(), stored.as_object()) {
            for (k, v) in extra {
                base.insert(k.clone(), v.clone());
            }
        }
    }
    let compression = compression_of(&settings);
    if let Some(base) = settings.as_object_mut() {
        base.insert("compression".to_string(), compression);
    }
    let capability_defaults = capability_defaults_of(&settings);
    if let Some(base) = settings.as_object_mut() {
        base.insert("capability_defaults".to_string(), capability_defaults);
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
    // `compression` is the one object that deep-merges: a partial patch keeps the
    // sub-keys it does not mention.
    if let Some(patch_compression) = patch.get("compression") {
        let Value::Object(patch_compression) = patch_compression else {
            // `validate` rejects any non-object; unreachable, but fail safe.
            return Err(Error::BadRequest("compression must be an object".into()));
        };
        let compression = merge_compression(current.get("compression"), patch_compression);
        current.insert("compression".to_string(), compression);
    }
    // `capability_defaults` deep-merges the same way: a patch naming one endpoint keeps
    // the operator's default for the other two.
    if let Some(patch_defaults) = patch.get("capability_defaults") {
        let Value::Object(patch_defaults) = patch_defaults else {
            // `validate` rejects any non-object; unreachable, but fail safe.
            return Err(Error::BadRequest(
                "capability_defaults must be an object".into(),
            ));
        };
        let defaults =
            merge_capability_defaults(current.get("capability_defaults"), patch_defaults);
        current.insert("capability_defaults".to_string(), defaults);
    }
    for (k, v) in patch {
        if k == "compression" || k == "capability_defaults" {
            continue;
        }
        current.insert(k, v);
    }
    let merged = Value::Object(current);
    write_blob(state, &merged).await?;
    apply_compression(&merged);
    apply_capability_defaults(&merged);
    if let Some(capture) = merged.get("capture_bodies").and_then(Value::as_bool) {
        CAPTURE_BODIES.store(capture, Ordering::Relaxed);
    }
    if let Some(logs) = merged.get("system_logs").and_then(Value::as_bool) {
        crate::syslog::set_enabled(logs);
    }
    if let Some(on) = merged.get("record_attempts").and_then(Value::as_bool) {
        RECORD_ATTEMPTS.store(on, Ordering::Relaxed);
    }
    if let Some(on) = merged.get("record_audit").and_then(Value::as_bool) {
        RECORD_AUDIT.store(on, Ordering::Relaxed);
    }
    Ok(merged)
}

/// Refuse a patch the sweep cannot honour: a switch that is not a boolean, or a retention
/// that is neither `null` nor a day count inside the ceiling. A silently clamped value
/// would report a policy the gateway is not running. `compression` is validated here too:
/// its booleans must stay booleans and the level must be one of the three names.
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
    if let Some(compression) = patch.get("compression") {
        let Value::Object(compression) = compression else {
            return Err(Error::BadRequest("compression must be an object".into()));
        };
        for key in COMPRESSION_BOOL_KEYS {
            match compression.get(*key) {
                None | Some(Value::Bool(_)) => {}
                Some(_) => {
                    return Err(Error::BadRequest(format!(
                        "compression.{key} must be true or false"
                    )));
                }
            }
        }
        match compression.get(CAVEMAN_LEVEL_KEY) {
            None => {}
            Some(Value::String(level)) if rrouter_caveman::Level::parse(level).is_some() => {}
            Some(_) => {
                return Err(Error::BadRequest(
                    "compression.caveman_level must be one of lite, full, ultra".into(),
                ));
            }
        }
    }
    if let Some(defaults) = patch.get("capability_defaults") {
        let Value::Object(defaults) = defaults else {
            return Err(Error::BadRequest(
                "capability_defaults must be an object".into(),
            ));
        };
        for cap in CapabilityDefault::ALL {
            match defaults.get(cap.key()) {
                None | Some(Value::Null) => {}
                Some(Value::String(value)) if is_valid_capability_default(value) => {}
                Some(_) => {
                    return Err(Error::BadRequest(format!(
                        "capability_defaults.{} must be null or a trimmed provider/model reference of 1-{MAX_CAPABILITY_DEFAULT_CHARS} characters",
                        cap.key()
                    )));
                }
            }
        }
    }
    Ok(())
}

/// The `ui` settings object: `{brand: {...}, panels: [...], compression: {...}}` with
/// defaults applied.
pub async fn ui_settings(state: &Arc<AppState>) -> Result<Value> {
    let stored = load(state).await?;
    let brand = brand_of(&stored);
    let panels = stored
        .get("panels")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let compression = stored
        .get("compression")
        .cloned()
        .unwrap_or_else(compression_defaults);
    Ok(json!({ "brand": brand, "panels": panels, "compression": compression }))
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
    use super::CapabilityDefault::{Embeddings, Rerank, Search};
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

    /// Runtime-only defaults live in the resolved block, never in the stored keys: the
    /// legacy top-level `rtk` switch keeps its meaning until the new block is written.
    #[test]
    fn compression_resolves_defaults_and_legacy_rtk() {
        assert!(defaults().get("rtk").is_none());
        assert!(defaults().get("compression").is_none());

        assert_eq!(
            compression_of(&json!({})),
            json!({"rtk": true, "caveman": false, "caveman_level": "full"})
        );
        assert_eq!(compression_of(&json!({"rtk": false}))["rtk"], json!(false));
        // A present block wins over the legacy key, including its own sub-key defaults.
        let both = compression_of(&json!({"rtk": false, "compression": {"caveman": true}}));
        assert_eq!(both["rtk"], json!(true));
        assert_eq!(both["caveman"], json!(true));
        assert_eq!(both["caveman_level"], json!("full"));
        // A junk stored block degrades to the defaults.
        assert_eq!(
            compression_of(&json!({"compression": 7})),
            json!({"rtk": true, "caveman": false, "caveman_level": "full"})
        );
    }

    #[test]
    fn compression_patches_are_validated_and_deep_merged() {
        for ok in [
            json!({"compression": {"caveman": true}}),
            json!({"compression": {"caveman_level": "ultra"}}),
            json!({"compression": {"unknown": 1}}),
        ] {
            assert!(validate(ok.as_object().expect("object")).is_ok(), "{ok}");
        }
        for bad in [
            json!({"compression": true}),
            json!({"compression": {"caveman": 1}}),
            json!({"compression": {"rtk": "yes"}}),
            json!({"compression": {"caveman_level": "max"}}),
            json!({"compression": {"caveman_level": 3}}),
        ] {
            assert!(validate(bad.as_object().expect("object")).is_err(), "{bad}");
        }

        let current = compression_of(&json!({}));
        let patch = json!({"caveman": true});
        assert_eq!(
            merge_compression(Some(&current), patch.as_object().expect("object")),
            json!({"rtk": true, "caveman": true, "caveman_level": "full"})
        );
        let patch = json!({"caveman_level": "lite", "junk": 3});
        assert_eq!(
            merge_compression(Some(&current), patch.as_object().expect("object")),
            json!({"rtk": true, "caveman": false, "caveman_level": "lite"})
        );
    }

    /// `GET /api/settings` always answers a full block, so the dashboard never has to
    /// distinguish "unset" from "absent".
    #[test]
    fn capability_defaults_are_present_null_and_validated() {
        assert_eq!(
            defaults()["capability_defaults"],
            json!({"search": null, "rerank": null, "embeddings": null})
        );
        for ok in [
            json!({"capability_defaults": {"search": "claude"}}),
            json!({"capability_defaults": {"rerank": "cohere/rerank-v3.5"}}),
            json!({"capability_defaults": {"embeddings": null, "unknown": 1}}),
            json!({"capability_defaults": {}}),
        ] {
            assert!(validate(ok.as_object().expect("object")).is_ok(), "{ok}");
        }
        let over = "x".repeat(MAX_CAPABILITY_DEFAULT_CHARS + 1);
        for bad in [
            json!({"capability_defaults": "claude"}),
            json!({"capability_defaults": {"search": 7}}),
            json!({"capability_defaults": {"search": ""}}),
            json!({"capability_defaults": {"rerank": " cohere"}}),
            json!({"capability_defaults": {"embeddings": over}}),
        ] {
            assert!(validate(bad.as_object().expect("object")).is_err(), "{bad}");
        }
    }

    /// A patch that names one capability keeps the other two, and the resolved block is
    /// what the request path reads back through `capability_default`.
    #[test]
    fn capability_defaults_deep_merge_and_reach_the_request_path() {
        let current = json!({"search": "claude", "rerank": "cohere/rerank-v3.5"});
        let patch = json!({"search": "openai/gpt-5-mini"});
        let merged = merge_capability_defaults(Some(&current), patch.as_object().expect("object"));
        assert_eq!(merged["search"], json!("openai/gpt-5-mini"));
        assert_eq!(
            merged["rerank"],
            json!("cohere/rerank-v3.5"),
            "an unmentioned endpoint keeps its default"
        );
        assert_eq!(merged["embeddings"], Value::Null);

        apply_capability_defaults(&json!({"capability_defaults": merged}));
        assert_eq!(
            capability_default(Search).as_deref(),
            Some("openai/gpt-5-mini")
        );
        assert_eq!(
            capability_default(Rerank).as_deref(),
            Some("cohere/rerank-v3.5")
        );
        assert_eq!(capability_default(Embeddings), None);

        apply_capability_defaults(&json!({}));
        assert_eq!(capability_default(Search), None);
    }
}
