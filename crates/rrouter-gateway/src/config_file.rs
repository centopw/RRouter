//! `RROUTER_CONFIG` — declarative configuration file, v1.
//!
//! JSON, not YAML: the gateway already depends on `serde_json`, and one format is one
//! grammar to keep honest. The schema is deliberately flat and each key maps to exactly
//! one environment variable, so [`super::Config::resolve`] validates both sources through
//! the same code path instead of forking a second schema with its own defaults.
//!
//! Rules that make the file safe to keep in version control:
//!
//! - **no secrets, only references** — `{"key_env": "NAME"}` or `{"key_file": "PATH"}`;
//!   an inline string is rejected at parse time;
//! - **unknown keys are errors** (`deny_unknown_fields`) so a typo cannot silently keep
//!   the default;
//! - **duplicate keys are errors** — JSON parsers keep the last one, which turns a typo
//!   into a surprise;
//! - **bind address stays in the environment** — `host`/`port` are recognised keys with a
//!   targeted refusal, because a reload can never apply them (v1 file is the reloadable
//!   surface).

use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};

use serde::de::{self, Deserializer, MapAccess, SeqAccess, Visitor};
use serde::Deserialize;

/// Environment lookup, threaded in so tests never touch the process environment.
pub type EnvLookup<'a> = &'a dyn Fn(&str) -> Option<String>;

/// Where a secret comes from. Inline values are rejected: a file in git must not be able
/// to hold a token, and an error is better than an operator believing it worked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretRef {
    pub key_env: Option<String>,
    pub key_file: Option<String>,
}

impl<'de> Deserialize<'de> for SecretRef {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct RefVisitor;

        impl<'de> Visitor<'de> for RefVisitor {
            type Value = SecretRef;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(
                    "a secret reference: {\"key_env\": \"NAME\"} or {\"key_file\": \"PATH\"}",
                )
            }

            fn visit_str<E: de::Error>(self, _value: &str) -> Result<Self::Value, E> {
                Err(E::custom(
                    "inline secrets are not allowed; use {\"key_env\": \"NAME\"} \
                     or {\"key_file\": \"PATH\"}",
                ))
            }

            fn visit_string<E: de::Error>(self, value: String) -> Result<Self::Value, E> {
                self.visit_str(&value)
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut key_env: Option<String> = None;
                let mut key_file: Option<String> = None;
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "key_env" => {
                            if key_env.is_some() {
                                return Err(de::Error::custom("duplicate key \"key_env\""));
                            }
                            key_env = Some(map.next_value::<String>()?);
                        }
                        "key_file" => {
                            if key_file.is_some() {
                                return Err(de::Error::custom("duplicate key \"key_file\""));
                            }
                            key_file = Some(map.next_value::<String>()?);
                        }
                        other => {
                            return Err(de::Error::unknown_field(other, &["key_env", "key_file"]))
                        }
                    }
                }
                Ok(SecretRef { key_env, key_file })
            }
        }

        deserializer.deserialize_any(RefVisitor)
    }
}

/// The file's keys. Every field is optional: absent means "no opinion", which is what
/// lets the environment and the defaults fill the gap.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    host: Option<String>,
    port: Option<u16>,
    data_dir: Option<String>,
    master_key: Option<SecretRef>,
    require_api_key: Option<bool>,
    request_timeout_secs: Option<u64>,
    connect_timeout_secs: Option<u64>,
    log_filter: Option<String>,
    static_dir: Option<String>,
    cors_origins: Option<Vec<String>>,
    allow_remote_admin: Option<bool>,
    admin_token: Option<SecretRef>,
    max_concurrency: Option<usize>,
    max_attempts: Option<usize>,
    retry_attempts: Option<usize>,
    retry_base_ms: Option<u64>,
    retry_max_ms: Option<u64>,
    shutdown_grace_secs: Option<u64>,
    strategy: Option<String>,
    cache: Option<String>,
    cache_ttl_secs: Option<u64>,
    cache_max_entries: Option<usize>,
    usage_retention_days: Option<usize>,
    log_format: Option<String>,
    access_log: Option<bool>,
    hedge_ms: Option<u64>,
    canary: Option<String>,
    subscription_reserve_percent: Option<u8>,
    semantic_cache: Option<String>,
    semantic_cache_threshold: Option<f64>,
    semantic_embedding_model: Option<String>,
    semantic_cache_ttl_secs: Option<u64>,
    semantic_cache_max_entries: Option<usize>,
}

/// A parsed, secrets-resolved file, addressable by the same field names the resolver
/// uses. Values are kept in their textual form so the environment's parsers (which
/// already produce precise errors) are the only parsers.
#[derive(Debug)]
pub struct FileLayer {
    pub path: PathBuf,
    values: BTreeMap<&'static str, String>,
}

impl FileLayer {
    /// Read and validate a file. The returned error is already operator-facing and
    /// names the file; the caller only prefixes its own context.
    pub fn load(path: &Path, env: EnvLookup<'_>) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("config file {}: cannot read: {e}", path.display()))?;
        Self::parse(path, &text, env)
    }

    /// Parse text as if it lived at `path` (tests use this; the path only labels errors).
    pub(crate) fn parse(path: &Path, text: &str, env: EnvLookup<'_>) -> Result<Self, String> {
        let label = |message: String| format!("config file {}: {message}", path.display());

        // Pass 1: syntax and duplicate keys. serde_json keeps the last duplicate
        // silently, so a config typo would otherwise change behaviour without a word.
        if let Err(error) = serde_json::from_str::<StrictValue>(text) {
            let message = error.to_string();
            if let Some(key) = duplicate_key(&message) {
                let line = last_line_containing(text, &key);
                let at = line.map_or(String::new(), |line| format!(" (line {line})"));
                return Err(label(format!("duplicate key {key:?}{at}; remove one")));
            }
            return Err(label(message));
        }

        // Pass 2: the typed schema. Unknown keys, wrong types and out-of-range numbers
        // all come back with a line and column from serde.
        let file: FileConfig = serde_json::from_str(text).map_err(|e| label(e.to_string()))?;

        // Bind settings are recognised so the refusal is specific instead of "unknown
        // field", but they are environment-only: a reload cannot rebind a listener.
        let mut problems: Vec<String> = Vec::new();
        if file.host.is_some() {
            problems.push(
                "\"host\" is fixed at startup and cannot come from the file; \
                 set RROUTER_HOST instead"
                    .to_string(),
            );
        }
        if file.port.is_some() {
            problems.push(
                "\"port\" is fixed at startup and cannot come from the file; \
                 set RROUTER_PORT instead"
                    .to_string(),
            );
        }
        if !problems.is_empty() {
            return Err(label(problems.join("; ")));
        }

        let mut values = BTreeMap::new();
        insert(&mut values, "data_dir", file.data_dir);
        insert(&mut values, "log_filter", file.log_filter);
        insert(&mut values, "static_dir", file.static_dir);
        insert(&mut values, "strategy", file.strategy);
        insert(&mut values, "cache", file.cache);
        insert(&mut values, "log_format", file.log_format);
        insert_num(&mut values, "require_api_key", file.require_api_key);
        insert_num(&mut values, "allow_remote_admin", file.allow_remote_admin);
        insert_num(&mut values, "access_log", file.access_log);
        insert_num(&mut values, "hedge_ms", file.hedge_ms);
        insert(&mut values, "canary", file.canary);
        insert_num(
            &mut values,
            "subscription_reserve_percent",
            file.subscription_reserve_percent,
        );
        insert(&mut values, "semantic_cache", file.semantic_cache);
        insert_num(
            &mut values,
            "semantic_cache_threshold",
            file.semantic_cache_threshold,
        );
        insert(
            &mut values,
            "semantic_embedding_model",
            file.semantic_embedding_model,
        );
        insert_num(
            &mut values,
            "semantic_cache_ttl_secs",
            file.semantic_cache_ttl_secs,
        );
        insert_num(
            &mut values,
            "semantic_cache_max_entries",
            file.semantic_cache_max_entries,
        );
        insert_num(
            &mut values,
            "request_timeout_secs",
            file.request_timeout_secs,
        );
        insert_num(
            &mut values,
            "connect_timeout_secs",
            file.connect_timeout_secs,
        );
        insert_num(&mut values, "max_concurrency", file.max_concurrency);
        insert_num(&mut values, "max_attempts", file.max_attempts);
        insert_num(&mut values, "retry_attempts", file.retry_attempts);
        insert_num(&mut values, "retry_base_ms", file.retry_base_ms);
        insert_num(&mut values, "retry_max_ms", file.retry_max_ms);
        insert_num(&mut values, "shutdown_grace_secs", file.shutdown_grace_secs);
        insert_num(&mut values, "cache_ttl_secs", file.cache_ttl_secs);
        insert_num(&mut values, "cache_max_entries", file.cache_max_entries);
        insert_num(
            &mut values,
            "usage_retention_days",
            file.usage_retention_days,
        );
        if let Some(origins) = file.cors_origins {
            // The environment parses a comma-separated list; joining keeps one parser
            // and one wildcard check for both sources.
            values.insert("cors_origins", origins.join(","));
        }

        for (field, secret) in [
            ("master_key", file.master_key),
            ("admin_token", file.admin_token),
        ] {
            if let Some(secret) = secret {
                let value = resolve_secret(path, field, &secret, env)?;
                values.insert(field, value);
            }
        }

        Ok(Self {
            path: path.to_path_buf(),
            values,
        })
    }

    /// The file's value for one config field, already validated for presence.
    pub fn get(&self, field: &str) -> Option<&str> {
        self.values.get(field).map(String::as_str)
    }

    /// How many settings the file actually sets (for `--check-config`).
    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
}

fn insert(values: &mut BTreeMap<&'static str, String>, field: &'static str, value: Option<String>) {
    if let Some(value) = value {
        values.insert(field, value);
    }
}

fn insert_num<T: ToString>(
    values: &mut BTreeMap<&'static str, String>,
    field: &'static str,
    value: Option<T>,
) {
    if let Some(value) = value {
        values.insert(field, value.to_string());
    }
}

/// Turn a reference into its value. The value is only ever held in memory; the error
/// names the reference, never the material.
fn resolve_secret(
    path: &Path,
    field: &str,
    secret: &SecretRef,
    env: EnvLookup<'_>,
) -> Result<String, String> {
    let label =
        |message: String| format!("config file {}: field {field:?}: {message}", path.display());
    match (&secret.key_env, &secret.key_file) {
        (Some(name), None) => {
            let value = env(name)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
                .ok_or_else(|| {
                    label(format!(
                        "key_env {name:?} is not set in the environment (or is empty)"
                    ))
                })?;
            Ok(value)
        }
        (None, Some(source)) => {
            let raw = std::fs::read_to_string(source)
                .map_err(|e| label(format!("key_file {source:?} cannot be read: {e}")))?;
            // Surrounding whitespace and a trailing newline are normal for a secret file
            // (`echo … > key`); an *embedded* newline is not a secret, it is the wrong
            // file — a JSON/YAML blob or a multi-key dump — and silently using its first
            // line would authenticate with something the operator never intended.
            let raw = raw.trim().to_string();
            if raw.is_empty() {
                return Err(label(format!("key_file {source:?} is empty")));
            }
            if raw.contains(['\n', '\r']) {
                return Err(label(format!(
                    "key_file {source:?} must contain a single line; found embedded \
                     newlines (point it at a file holding only the secret)"
                )));
            }
            Ok(raw)
        }
        (Some(_), Some(_)) => Err(label(
            "key_env and key_file are mutually exclusive; keep one".to_string(),
        )),
        (None, None) => Err(label(
            "needs \"key_env\" or \"key_file\"; inline values are not allowed".to_string(),
        )),
    }
}

/// The duplicate key a strict parse reported, if it was that kind of error.
///
/// The marker is internal: serde's error type cannot carry structured data, so the key
/// travels in the message and this strips it back out.
fn duplicate_key(message: &str) -> Option<String> {
    let rest = message.split(StrictValue::DUP_MARKER).nth(1)?;
    let key = rest.trim().trim_end_matches('"').trim_start_matches('"');
    let key = key.split_whitespace().next().unwrap_or(key);
    (!key.is_empty()).then(|| key.to_string())
}

/// Line of the last occurrence of `"key"`, used to point at the duplicate.
fn last_line_containing(text: &str, key: &str) -> Option<usize> {
    let needle = format!("\"{key}\"");
    text.lines()
        .enumerate()
        .filter(|(_, line)| line.contains(&needle))
        .map(|(index, _)| index + 1)
        .last()
}

/// A JSON value parsed only to reject duplicate object keys at any depth.
///
/// Scalar values are accepted and discarded; maps carry the duplicate check. This runs
/// before the typed parse so the message can say "duplicate key" instead of letting the
/// last occurrence win.
struct StrictValue;

impl StrictValue {
    const DUP_MARKER: &'static str = "\u{1}duplicate-key:";
}

impl<'de> Deserialize<'de> for StrictValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct StrictVisitor;

        impl<'de> Visitor<'de> for StrictVisitor {
            type Value = StrictValue;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a JSON object without duplicate keys")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut seen: HashSet<String> = HashSet::new();
                while let Some(key) = map.next_key::<String>()? {
                    if !seen.insert(key.clone()) {
                        return Err(de::Error::custom(format!(
                            "{}{key}",
                            StrictValue::DUP_MARKER
                        )));
                    }
                    map.next_value::<StrictValue>()?;
                }
                Ok(StrictValue)
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                while seq.next_element::<StrictValue>()?.is_some() {}
                Ok(StrictValue)
            }

            fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> {
                Ok(StrictValue)
            }
            fn visit_i64<E: de::Error>(self, _: i64) -> Result<Self::Value, E> {
                Ok(StrictValue)
            }
            fn visit_u64<E: de::Error>(self, _: u64) -> Result<Self::Value, E> {
                Ok(StrictValue)
            }
            fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> {
                Ok(StrictValue)
            }
            fn visit_str<E: de::Error>(self, _: &str) -> Result<Self::Value, E> {
                Ok(StrictValue)
            }
            fn visit_string<E: de::Error>(self, _: String) -> Result<Self::Value, E> {
                Ok(StrictValue)
            }
            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(StrictValue)
            }
            fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(StrictValue)
            }
            fn visit_some<D: Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
                StrictValue::deserialize(d)
            }
        }

        deserializer.deserialize_any(StrictVisitor)
    }
}
