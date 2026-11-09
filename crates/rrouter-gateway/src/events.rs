//! Webhook event primitives: the envelope, its signature, the retry schedule, redaction.
//!
//! Nothing here opens a socket, touches the database, or spawns a task. The delivery
//! worker and the outbox transaction live in `rrouter-store`/`state.rs`; this module is
//! the contract they share with receivers, kept pure so the wire format and the crypto can
//! be tested without a running gateway. `docs/EVENTS.md` is the integration handoff.

use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use hmac::{Hmac, Mac};
use serde::ser::SerializeStruct;
use serde::{Serialize, Serializer};
use serde_json::Value;
use sha2::Sha256;
use uuid::Uuid;

type HmacSha256 = Hmac<Sha256>;

/// Written over every redacted leaf. Receivers see the key still present, so "absent"
/// and "present but secret" stay distinguishable.
pub const REDACTED: &str = "[redacted]";

/// `x-rrouter-event`: the event type (`key.created`), for routing without parsing.
pub const HEADER_EVENT: &str = "x-rrouter-event";
/// `x-rrouter-delivery`: the event id, stable across every retry — the idempotency key.
pub const HEADER_DELIVERY: &str = "x-rrouter-delivery";
/// `x-rrouter-signature`: `t=<unix>,v1=<hex hmac>` over `"<t>.<body>"`.
pub const HEADER_SIGNATURE: &str = "x-rrouter-signature";

/// How far a signature timestamp may be from the receiver's clock.
pub const DEFAULT_TOLERANCE_SECS: i64 = 300;

// ---------------------------------------------------------------------------------
// Envelope
// ---------------------------------------------------------------------------------

/// The stable set of events a deployment can subscribe to.
///
/// Adding a variant is backward compatible (receivers ignore unknown types); renaming or
/// removing one is not. The wire string is the contract, not the Rust name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EventType {
    KeyCreated,
    KeyUpdated,
    KeyDeleted,
    TeamCreated,
    TeamUpdated,
    TeamDeleted,
    TeamMemberAdded,
    TeamMemberRemoved,
    ConnectionCreated,
    ConnectionUpdated,
    ConnectionDeleted,
    ConnectionDegraded,
    ConnectionRecovered,
    AuditRecorded,
}

impl EventType {
    /// Every type, in documentation order. Also what a receiver may subscribe to.
    pub const ALL: [EventType; 14] = [
        EventType::KeyCreated,
        EventType::KeyUpdated,
        EventType::KeyDeleted,
        EventType::TeamCreated,
        EventType::TeamUpdated,
        EventType::TeamDeleted,
        EventType::TeamMemberAdded,
        EventType::TeamMemberRemoved,
        EventType::ConnectionCreated,
        EventType::ConnectionUpdated,
        EventType::ConnectionDeleted,
        EventType::ConnectionDegraded,
        EventType::ConnectionRecovered,
        EventType::AuditRecorded,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::KeyCreated => "key.created",
            Self::KeyUpdated => "key.updated",
            Self::KeyDeleted => "key.deleted",
            Self::TeamCreated => "team.created",
            Self::TeamUpdated => "team.updated",
            Self::TeamDeleted => "team.deleted",
            Self::TeamMemberAdded => "team.member.added",
            Self::TeamMemberRemoved => "team.member.removed",
            Self::ConnectionCreated => "connection.created",
            Self::ConnectionUpdated => "connection.updated",
            Self::ConnectionDeleted => "connection.deleted",
            Self::ConnectionDegraded => "connection.degraded",
            Self::ConnectionRecovered => "connection.recovered",
            Self::AuditRecorded => "audit.recorded",
        }
    }

    /// Parse the wire string; `None` for a type this build does not know.
    pub fn parse(raw: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|kind| kind.as_str() == raw)
    }
}

impl fmt::Display for EventType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for EventType {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

/// One event as it is stored and delivered.
///
/// The payload is redacted at construction ([`Event::new`]), so an event that exists can
/// be serialized, logged or written to the outbox without a second review. The field
/// order below is the wire order: `id`, `type`, `created_at`, `data`.
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    id: String,
    kind: EventType,
    created_at: i64,
    data: Value,
}

impl Event {
    /// A new event with a fresh UUIDv4 id.
    pub fn new(kind: EventType, created_at: i64, data: Value) -> Self {
        Self::with_id(Uuid::new_v4().to_string(), kind, created_at, data)
    }

    /// An event with an explicit id (tests, replay, or an id chosen by the outbox).
    pub fn with_id(
        id: impl Into<String>,
        kind: EventType,
        created_at: i64,
        mut data: Value,
    ) -> Self {
        redact(&mut data);
        Self {
            id: id.into(),
            kind,
            created_at,
            data,
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn kind(&self) -> EventType {
        self.kind
    }

    pub fn created_at(&self) -> i64 {
        self.created_at
    }

    pub fn data(&self) -> &Value {
        &self.data
    }

    /// The canonical body: exactly the bytes that are signed and delivered.
    pub fn to_json(&self) -> Vec<u8> {
        // A `Value` and four fixed fields cannot fail to serialize.
        serde_json::to_vec(self).expect("an event always serializes")
    }
}

impl Serialize for Event {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut st = serializer.serialize_struct("Event", 4)?;
        st.serialize_field("id", &self.id)?;
        st.serialize_field("type", self.kind.as_str())?;
        st.serialize_field("created_at", &self.created_at)?;
        st.serialize_field("data", &self.data)?;
        st.end()
    }
}

// ---------------------------------------------------------------------------------
// Redaction
// ---------------------------------------------------------------------------------

/// Key-name suffixes that always carry secret material (case-insensitive, `_`/`-`
/// ignored): `api_key`, `access_token`, `client_secret`, `password`, `credentials`, …
const SECRET_SUFFIXES: &[&str] = &[
    "password",
    "secret",
    "token",
    "apikey",
    "credential",
    "credentials",
];

/// Replace every secret-looking leaf of `value` with [`REDACTED`], in place.
///
/// Key-based redaction is the primary rule because it cannot be fooled by an encoding;
/// a value scan is the second line, for a credential pasted into a free-text field
/// (`"message": "Authorization: Bearer …"`, a PEM block, a provider key prefix).
/// Over-redaction is deliberate: a webhook receiver needs the shape of an event, not its
/// credentials, and a false positive costs a `[redacted]` while a false negative leaks.
pub fn redact(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, child) in map.iter_mut() {
                if is_secret_key(key) {
                    // Replace the whole value, so a credential *blob* cannot leak one
                    // field at a time.
                    *child = Value::String(REDACTED.to_string());
                } else {
                    redact(child);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                redact(item);
            }
        }
        Value::String(text) if looks_like_secret(text) => {
            *text = REDACTED.to_string();
        }
        _ => {}
    }
}

/// Is this JSON key a credential container? Normalised so `api-key`, `api_key` and
/// `APIKEY` are one answer.
fn is_secret_key(key: &str) -> bool {
    let normalized: String = key
        .chars()
        .filter(|c| *c != '_' && *c != '-')
        .flat_map(char::to_lowercase)
        .collect();
    matches!(
        normalized.as_str(),
        "authorization" | "masterkey" | "keyhash"
    ) || SECRET_SUFFIXES
        .iter()
        .any(|suffix| normalized.ends_with(suffix))
}

/// Does a free-text value look like a credential? Conservative prefixes only, so ordinary
/// prose is not rewritten.
fn looks_like_secret(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.starts_with("bearer ")
        || lower.contains("authorization: bearer")
        || lower.contains("sk-rr-")
        || lower.contains("ghp_")
        || lower.contains("xoxb-")
        || lower.contains("-----begin ")
        || lower.contains("api_key=")
        || lower.contains("apikey=")
        || lower.contains("password=")
}

// ---------------------------------------------------------------------------------
// Signing
// ---------------------------------------------------------------------------------

/// Failures that are safe to log: they name references and never echo material.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventError {
    /// The secret reference could not be resolved; the message names only the reference.
    Secret(String),
    /// The signature header is not `t=<unix>,v1=<64 hex>`.
    MalformedSignature,
    /// The timestamp is outside the accepted clock window.
    StaleSignature,
    /// The body does not match the signature.
    BadSignature,
}

impl fmt::Display for EventError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Secret(message) => write!(f, "secret: {message}"),
            Self::MalformedSignature => f.write_str("malformed signature header"),
            Self::StaleSignature => f.write_str("signature timestamp outside the window"),
            Self::BadSignature => f.write_str("signature does not match the body"),
        }
    }
}

impl std::error::Error for EventError {}

/// Where the signing secret comes from. Resolved per delivery, so rotating the environment
/// variable or the file takes effect on the next attempt without a restart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretRef {
    Env(String),
    File(PathBuf),
}

impl SecretRef {
    pub fn env(name: impl Into<String>) -> Self {
        Self::Env(name.into())
    }

    pub fn file(path: impl Into<PathBuf>) -> Self {
        Self::File(path.into())
    }

    /// Read the secret. The error names the variable or path, never the value.
    pub fn resolve(&self) -> Result<Vec<u8>, EventError> {
        let raw = match self {
            Self::Env(name) => std::env::var(name)
                .map_err(|_| {
                    EventError::Secret(format!("env {name:?} is not set (or is not UTF-8)"))
                })?
                .into_bytes(),
            Self::File(path) => std::fs::read(path).map_err(|e| {
                EventError::Secret(format!("file {} cannot be read: {e}", path.display()))
            })?,
        };
        let trimmed = raw.trim_ascii();
        if trimmed.is_empty() {
            return Err(EventError::Secret(format!("{} is empty", self.describe())));
        }
        if trimmed.contains(&b'\n') || trimmed.contains(&b'\r') {
            return Err(EventError::Secret(format!(
                "{} must contain a single line",
                self.describe()
            )));
        }
        Ok(trimmed.to_vec())
    }

    fn describe(&self) -> String {
        match self {
            Self::Env(name) => format!("env {name:?}"),
            Self::File(path) => format!("file {}", path.display()),
        }
    }
}

/// `t=<timestamp>,v1=<hex>` for `body`.
///
/// The covered string is `"<timestamp>.<body>"`: the timestamp is inside the MAC, so a
/// receiver can reject a replayed delivery without trusting the body.
pub fn signature(secret: &[u8], timestamp: i64, body: &[u8]) -> Result<String, EventError> {
    let mut mac = new_mac(secret)?;
    mac.update(&signed_payload(timestamp, body));
    let digest = mac.finalize().into_bytes();
    Ok(format!(
        "t={timestamp},v1={}",
        hex::encode(digest.as_slice())
    ))
}

/// Verify a signature over the exact bytes received. Constant-time comparison.
pub fn verify(secret: &[u8], signature: &str, body: &[u8], now: i64) -> Result<(), EventError> {
    verify_with_window(secret, signature, body, now, DEFAULT_TOLERANCE_SECS)
}

/// [`verify`] with an explicit clock window (tests, clock-skewed deployments).
pub fn verify_with_window(
    secret: &[u8],
    signature: &str,
    body: &[u8],
    now: i64,
    tolerance_secs: i64,
) -> Result<(), EventError> {
    let (timestamp, expected) = parse_signature(signature)?;
    if timestamp.abs_diff(now) > tolerance_secs.max(0) as u64 {
        return Err(EventError::StaleSignature);
    }
    let bytes = hex::decode(&expected).map_err(|_| EventError::MalformedSignature)?;
    if bytes.len() != 32 {
        return Err(EventError::MalformedSignature);
    }
    let mut mac = new_mac(secret)?;
    mac.update(&signed_payload(timestamp, body));
    // `verify_slice` compares in constant time; a length mismatch does not short-circuit
    // on the secret's contents.
    mac.verify_slice(&bytes)
        .map_err(|_| EventError::BadSignature)
}

/// One delivery attempt: the exact body plus the three headers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delivery {
    pub body: Vec<u8>,
    pub headers: Vec<(&'static str, String)>,
}

impl Delivery {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(header, _)| *header == name)
            .map(|(_, value)| value.as_str())
    }
}

/// Build a delivery for `event` at `timestamp`.
///
/// The body is serialized once and the signature covers exactly those bytes, so a caller
/// cannot sign one payload and send another.
pub fn prepare(event: &Event, secret: &[u8], timestamp: i64) -> Result<Delivery, EventError> {
    let body = event.to_json();
    let signature = signature(secret, timestamp, &body)?;
    Ok(Delivery {
        headers: vec![
            (HEADER_EVENT, event.kind().as_str().to_string()),
            (HEADER_DELIVERY, event.id().to_string()),
            (HEADER_SIGNATURE, signature),
        ],
        body,
    })
}

/// The MAC input: `"<timestamp>.<body>"`.
fn signed_payload(timestamp: i64, body: &[u8]) -> Vec<u8> {
    let mut out = timestamp.to_string().into_bytes();
    out.push(b'.');
    out.extend_from_slice(body);
    out
}

fn new_mac(secret: &[u8]) -> Result<HmacSha256, EventError> {
    // HMAC accepts keys of any length; the error branch is unreachable but handled.
    HmacSha256::new_from_slice(secret)
        .map_err(|_| EventError::Secret("the signing secret is not usable".to_string()))
}

/// Parse `t=<unix>,v1=<hex>` strictly: exactly one of each, in any order, nothing else.
fn parse_signature(raw: &str) -> Result<(i64, String), EventError> {
    let mut timestamp = None;
    let mut digest = None;
    for part in raw.trim().split(',') {
        let part = part.trim();
        if let Some(value) = part.strip_prefix("t=") {
            let parsed: i64 = value.parse().map_err(|_| EventError::MalformedSignature)?;
            if timestamp.replace(parsed).is_some() {
                return Err(EventError::MalformedSignature);
            }
        } else if let Some(value) = part.strip_prefix("v1=") {
            if digest.replace(value.to_string()).is_some() {
                return Err(EventError::MalformedSignature);
            }
        } else {
            return Err(EventError::MalformedSignature);
        }
    }
    match (timestamp, digest) {
        (Some(timestamp), Some(digest)) => Ok((timestamp, digest)),
        _ => Err(EventError::MalformedSignature),
    }
}

// ---------------------------------------------------------------------------------
// Retry policy
// ---------------------------------------------------------------------------------

/// Why a delivery will never be attempted again.
///
/// The reason is a log and `last_error` token, not a column of its own: the outbox records
/// the terminal outcome as `status`, and the dispatcher moves the row to `dead` when the
/// retries are spent (`delivered` and `unmatched` are the queue's other terminal statuses).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DropReason {
    /// `max_attempts` attempts were spent; the dispatcher marks the row `dead`.
    Exhausted,
}

impl DropReason {
    /// Stable value for logs; the row this reason terminates is recorded as `status = dead`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Exhausted => "exhausted",
        }
    }
}

/// What the worker does after a failed attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NextAttempt {
    /// Wait this long, then send attempt `attempt + 1`.
    RetryIn(Duration),
    /// Stop for good; record the reason and release the row.
    Drop(DropReason),
}

/// Bounded exponential backoff with deterministic jitter.
///
/// Attempts are numbered from 1 (the first send). `now` is unix seconds and seeds the
/// jitter, so the schedule is a pure function of `(policy, now, attempt)` — replayable in
/// tests, and still spread across a fleet because every worker calls it at a different
/// second.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Attempts allowed in total, including the first send. Values below 1 mean one send.
    pub max_attempts: u32,
    pub base_delay_ms: u64,
    pub max_delay_ms: u64,
    /// Jitter band, as a percentage of the computed delay. Clamped to 0–50.
    pub jitter_percent: u8,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 8,
            base_delay_ms: 1_000,
            max_delay_ms: 300_000,
            jitter_percent: 20,
        }
    }
}

impl RetryPolicy {
    /// The decision after attempt number `attempt` (1-based) failed.
    pub fn next_attempt(&self, now: i64, attempt: u32) -> NextAttempt {
        let max_attempts = self.max_attempts.max(1);
        if attempt >= max_attempts {
            return NextAttempt::Drop(DropReason::Exhausted);
        }
        let base = self.base_delay_ms.max(1);
        let cap = self.max_delay_ms.max(base).min(i64::MAX as u64);
        // 2^(attempt-1), bounded well before the shift can overflow.
        let step = attempt.saturating_sub(1).min(32);
        let delay = base.saturating_mul(1u64 << step).min(cap);
        let jittered = jitter(delay, self.jitter_percent.min(50), now, attempt).clamp(1, cap);
        NextAttempt::RetryIn(Duration::from_millis(jittered))
    }
}

/// Deterministic jitter in `-percent..=+percent` of `delay`, derived from `(now, attempt)`.
fn jitter(delay: u64, percent: u8, now: i64, attempt: u32) -> u64 {
    if percent == 0 {
        return delay;
    }
    let span = ((u128::from(delay) * u128::from(percent)) / 100) as u64;
    if span == 0 {
        return delay;
    }
    let spread = (mix(now, attempt) % (2 * span + 1)) as i64 - span as i64;
    (delay as i64).saturating_add(spread).max(1) as u64
}

/// FNV-1a over `(now, attempt)`: deterministic, allocation-free, and good enough to spread
/// retries — this is jitter, not key material.
fn mix(now: i64, attempt: u32) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in now.to_le_bytes().iter().chain(attempt.to_le_bytes().iter()) {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ----- envelope ---------------------------------------------------------------

    #[test]
    fn the_json_shape_is_stable_and_field_order_is_fixed() {
        let event = Event::with_id(
            "00000000-0000-4000-8000-000000000000",
            EventType::KeyCreated,
            1_700_000_000,
            json!({"name": "laptop", "key_id": "k1"}),
        );
        // Object keys inside `data` are sorted (serde_json's BTreeMap), so the body is
        // byte-identical across processes — which is what the signature covers.
        assert_eq!(
            String::from_utf8(event.to_json()).expect("utf-8"),
            r#"{"id":"00000000-0000-4000-8000-000000000000","type":"key.created","created_at":1700000000,"data":{"key_id":"k1","name":"laptop"}}"#
        );
        assert_eq!(event.id(), "00000000-0000-4000-8000-000000000000");
        assert_eq!(event.kind(), EventType::KeyCreated);
        assert_eq!(event.created_at(), 1_700_000_000);
    }

    #[test]
    fn a_new_event_gets_a_uuid_id() {
        let event = Event::new(EventType::AuditRecorded, 1, json!({"kind": "sign_in"}));
        assert_eq!(event.id().len(), 36, "UUIDv4 string");
        assert_eq!(
            Uuid::parse_str(event.id()).expect("uuid").get_version_num(),
            4
        );
    }

    #[test]
    fn the_event_type_list_is_stable_and_round_trips() {
        let mut seen = std::collections::HashSet::new();
        for kind in EventType::ALL {
            assert!(seen.insert(kind.as_str()), "duplicate {}", kind.as_str());
            assert_eq!(EventType::parse(kind.as_str()), Some(kind));
            assert_eq!(kind.to_string(), kind.as_str());
        }
        assert_eq!(seen.len(), EventType::ALL.len());
        assert_eq!(EventType::parse("key.exploded"), None);
        // The strings themselves are the receiver-facing contract.
        assert_eq!(EventType::KeyCreated.as_str(), "key.created");
        assert_eq!(EventType::TeamMemberAdded.as_str(), "team.member.added");
        assert_eq!(EventType::AuditRecorded.as_str(), "audit.recorded");
    }

    // ----- redaction --------------------------------------------------------------

    #[test]
    fn payload_secrets_are_redacted_and_shape_is_kept() {
        let mut data = json!({
            "key_id": "k1",
            "model": "claude/haiku",
            "api_key": "sk-rr-0123456789abcdef",
            "Authorization": "Bearer eyJhbGciOi",
            "nested": {
                "refresh_token": "rt-secret",
                "name": "kept"
            },
            "credentials": {"api_key": "sk-rr-other", "base_url": "https://api.example"},
            "items": [{"password": "hunter2"}, {"name": "kept-too"}]
        });
        redact(&mut data);

        assert_eq!(data["api_key"], REDACTED);
        assert_eq!(data["Authorization"], REDACTED);
        assert_eq!(data["nested"]["refresh_token"], REDACTED);
        assert_eq!(data["nested"]["name"], "kept");
        assert_eq!(
            data["credentials"], REDACTED,
            "a credential blob is replaced whole"
        );
        assert_eq!(data["items"][0]["password"], REDACTED);
        assert_eq!(data["items"][1]["name"], "kept-too");
        assert_eq!(data["key_id"], "k1", "ids are not secrets");
        assert_eq!(data["model"], "claude/haiku");
        let serialized = data.to_string();
        for secret in [
            "sk-rr-0123456789abcdef",
            "Bearer eyJhbGciOi",
            "rt-secret",
            "hunter2",
        ] {
            assert!(
                !serialized.contains(secret),
                "{secret} leaked: {serialized}"
            );
        }
    }

    #[test]
    fn representative_secret_shapes_are_caught() {
        let mut data = json!({
            "api-key": "a",
            "APIKEY": "b",
            "access_token": "c",
            "idToken": "d",
            "client_secret": "e",
            "password": "f",
            "master_key": "g",
            "key_hash": "h",
            "connection_credentials": {"anything": 1},
            "message": "Authorization: Bearer sk-rr-leaked",
            "pem": "-----BEGIN PRIVATE KEY-----",
            "dsn": "postgres://user:password=secret@db"
        });
        redact(&mut data);
        for key in [
            "api-key",
            "APIKEY",
            "access_token",
            "idToken",
            "client_secret",
            "password",
            "master_key",
            "key_hash",
            "connection_credentials",
            "message",
            "pem",
            "dsn",
        ] {
            assert_eq!(data[key], REDACTED, "{key} must be redacted");
        }
    }

    #[test]
    fn events_are_redacted_at_construction() {
        let event = Event::with_id(
            "e1",
            EventType::KeyUpdated,
            1,
            json!({"api_key": "sk-rr-construction-secret", "name": "laptop"}),
        );
        assert_eq!(event.data()["api_key"], REDACTED);
        let body = String::from_utf8(event.to_json()).expect("utf-8");
        assert!(!body.contains("sk-rr-construction-secret"), "{body}");
        assert!(body.contains("\"name\":\"laptop\""), "{body}");
    }

    // ----- signing ----------------------------------------------------------------

    const SECRET: &[u8] = b"whsec_test_secret";

    fn event() -> Event {
        Event::with_id(
            "evt_1",
            EventType::ConnectionDegraded,
            1_700_000_000,
            json!({"connection_id": "c1", "provider": "openai"}),
        )
    }

    #[test]
    fn a_signature_has_the_documented_shape() {
        let value = signature(SECRET, 1_700_000_000, b"{}").expect("sign");
        let (timestamp, digest) = value
            .strip_prefix("t=")
            .and_then(|rest| rest.split_once(",v1="))
            .expect("t=…,v1=…");
        assert_eq!(timestamp, "1700000000");
        assert_eq!(digest.len(), 64, "HMAC-SHA256 hex");
        assert!(digest.bytes().all(|b| b.is_ascii_hexdigit()));
        // Stable for the same inputs, different for a different body.
        assert_eq!(
            value,
            signature(SECRET, 1_700_000_000, b"{}").expect("sign")
        );
        assert_ne!(
            value,
            signature(SECRET, 1_700_000_000, b"[]").expect("sign")
        );
    }

    #[test]
    fn delivery_headers_carry_type_id_and_signature() {
        let event = event();
        let delivery = prepare(&event, SECRET, 1_700_000_000).expect("prepare");
        assert_eq!(delivery.header(HEADER_EVENT), Some("connection.degraded"));
        assert_eq!(delivery.header(HEADER_DELIVERY), Some("evt_1"));
        assert!(delivery
            .header(HEADER_SIGNATURE)
            .expect("signature")
            .starts_with("t=1700000000,v1="));
        assert_eq!(
            delivery.body,
            event.to_json(),
            "signed body is the sent body"
        );
        // And the delivery verifies end to end.
        verify(
            SECRET,
            delivery.header(HEADER_SIGNATURE).expect("signature"),
            &delivery.body,
            1_700_000_000,
        )
        .expect("verify");
    }

    #[test]
    fn tampering_with_the_body_fails_verification() {
        let delivery = prepare(&event(), SECRET, 1_700_000_000).expect("prepare");
        let signature = delivery.header(HEADER_SIGNATURE).expect("signature");

        let mut body = delivery.body.clone();
        let last = body.len() - 1;
        body[last] ^= 0x01; // flip one bit inside the JSON
        assert_eq!(
            verify(SECRET, signature, &body, 1_700_000_000),
            Err(EventError::BadSignature)
        );
    }

    #[test]
    fn tampering_with_the_signature_fails_verification() {
        let delivery = prepare(&event(), SECRET, 1_700_000_000).expect("prepare");
        let signature = delivery.header(HEADER_SIGNATURE).expect("signature");

        let (prefix, digest) = signature.split_once("v1=").expect("v1");
        let mut chars: Vec<char> = digest.chars().collect();
        chars[0] = if chars[0] == 'a' { 'b' } else { 'a' };
        let flipped: String = chars.into_iter().collect();
        assert_eq!(
            verify(
                SECRET,
                &format!("{prefix}v1={flipped}"),
                &delivery.body,
                1_700_000_000
            ),
            Err(EventError::BadSignature)
        );
        // A different secret fails the same way.
        assert_eq!(
            verify(b"other-secret", signature, &delivery.body, 1_700_000_000),
            Err(EventError::BadSignature)
        );
    }

    #[test]
    fn malformed_signature_headers_are_refused() {
        for raw in [
            "",
            "t=1700000000",
            "v1=aa",
            "t=abc,v1=aa",
            "t=1700000000,v1=not-hex",
            "t=1700000000,v1=aa,v2=bb",
            "1700000000,v1=aa",
        ] {
            assert!(
                matches!(
                    verify(SECRET, raw, b"{}", 1_700_000_000),
                    Err(EventError::MalformedSignature)
                ),
                "{raw:?} should be malformed"
            );
        }
        // Right shape, wrong digest length: still malformed, not a bad signature.
        let short = "t=1700000000,v1=aa";
        assert_eq!(
            verify(SECRET, short, b"{}", 1_700_000_000),
            Err(EventError::MalformedSignature)
        );
    }

    #[test]
    fn a_stale_or_future_timestamp_is_refused() {
        let delivery = prepare(&event(), SECRET, 1_700_000_000).expect("prepare");
        let signature = delivery.header(HEADER_SIGNATURE).expect("signature");

        // Inside the default window on both sides.
        verify(SECRET, signature, &delivery.body, 1_700_000_299).expect("fresh");
        verify(SECRET, signature, &delivery.body, 1_699_999_701).expect("slightly early");

        assert_eq!(
            verify(SECRET, signature, &delivery.body, 1_700_000_301),
            Err(EventError::StaleSignature)
        );
        assert_eq!(
            verify_with_window(SECRET, signature, &delivery.body, 1_700_000_000, 0),
            Ok(())
        );
        assert_eq!(
            verify_with_window(SECRET, signature, &delivery.body, 1_700_000_001, 0),
            Err(EventError::StaleSignature)
        );
    }

    #[test]
    fn secret_refs_resolve_from_the_environment_and_a_file() {
        let name = format!("RROUTER_TEST_WEBHOOK_SECRET_{}", Uuid::new_v4());
        std::env::set_var(&name, "  env-secret\n");
        let resolved = SecretRef::env(&name).resolve().expect("env secret");
        std::env::remove_var(&name);
        assert_eq!(resolved, b"env-secret");

        let path = std::env::temp_dir().join(format!("rrouter-hook-{}.key", Uuid::new_v4()));
        std::fs::write(&path, "file-secret\n\n").expect("write");
        assert_eq!(
            SecretRef::file(&path).resolve().expect("file secret"),
            b"file-secret"
        );
        std::fs::write(&path, "line-one\nline-two\n").expect("write");
        let error = SecretRef::file(&path).resolve().expect_err("multi-line");
        std::fs::remove_file(&path).ok();
        assert!(error.to_string().contains("single line"), "{error}");
        assert!(!error.to_string().contains("line-one"), "{error}");

        let missing = SecretRef::env(format!("RROUTER_ABSENT_{}", Uuid::new_v4()));
        let error = missing.resolve().expect_err("missing");
        assert!(error.to_string().contains("is not set"), "{error}");
    }

    // ----- retry policy -----------------------------------------------------------

    fn base_policy(jitter_percent: u8) -> RetryPolicy {
        RetryPolicy {
            max_attempts: 10,
            base_delay_ms: 100,
            max_delay_ms: 1_000,
            jitter_percent,
        }
    }

    fn delay(policy: &RetryPolicy, attempt: u32) -> u64 {
        match policy.next_attempt(1_700_000_000, attempt) {
            NextAttempt::RetryIn(delay) => delay.as_millis() as u64,
            NextAttempt::Drop(reason) => panic!("attempt {attempt} dropped: {reason:?}"),
        }
    }

    #[test]
    fn backoff_doubles_until_the_cap_without_jitter() {
        let policy = base_policy(0);
        assert_eq!(delay(&policy, 1), 100);
        assert_eq!(delay(&policy, 2), 200);
        assert_eq!(delay(&policy, 3), 400);
        assert_eq!(delay(&policy, 4), 800);
        assert_eq!(delay(&policy, 5), 1_000, "capped");
        assert_eq!(delay(&policy, 9), 1_000, "still capped");
    }

    #[test]
    fn attempts_stop_at_the_maximum_and_report_a_terminal_reason() {
        let policy = RetryPolicy {
            max_attempts: 3,
            ..base_policy(0)
        };
        assert!(matches!(policy.next_attempt(1, 1), NextAttempt::RetryIn(_)));
        assert!(matches!(policy.next_attempt(1, 2), NextAttempt::RetryIn(_)));
        assert_eq!(
            policy.next_attempt(1, 3),
            NextAttempt::Drop(DropReason::Exhausted)
        );
        assert_eq!(
            policy.next_attempt(1, 4),
            NextAttempt::Drop(DropReason::Exhausted)
        );
        assert_eq!(DropReason::Exhausted.as_str(), "exhausted");

        // One attempt means no retry at all.
        let single = RetryPolicy {
            max_attempts: 1,
            ..base_policy(0)
        };
        assert_eq!(
            single.next_attempt(1, 1),
            NextAttempt::Drop(DropReason::Exhausted)
        );
    }

    #[test]
    fn jitter_is_bounded_and_deterministic() {
        let policy = base_policy(20);
        let first = delay(&policy, 3); // 400 ms ± 20% => 320–480
        assert!((320..=480).contains(&first), "{first} outside the band");
        assert_eq!(first, delay(&policy, 3), "same inputs, same schedule");

        // The cap bounds the upper side even when jitter would push past it; jitter may
        // still come in below the cap.
        let capped = RetryPolicy {
            max_attempts: 10,
            base_delay_ms: 1_000,
            max_delay_ms: 1_000,
            jitter_percent: 50,
        };
        for attempt in 1..10 {
            let value = delay(&capped, attempt);
            assert!((500..=1_000).contains(&value), "attempt {attempt}: {value}");
        }

        // Different seconds spread the retries, which is the point of jitter.
        let mut seen = std::collections::HashSet::new();
        for now in 1_700_000_000..1_700_000_040 {
            if let NextAttempt::RetryIn(delay) = policy.next_attempt(now, 3) {
                seen.insert(delay.as_millis());
            }
        }
        assert!(seen.len() > 1, "jitter must vary with time: {seen:?}");
    }

    #[test]
    fn a_zero_base_or_negative_window_still_produces_a_safe_schedule() {
        let zero_base = RetryPolicy {
            max_attempts: 2,
            base_delay_ms: 0,
            max_delay_ms: 0,
            jitter_percent: 50,
        };
        // Clamped to 1 ms rather than 0 (a hot retry loop) and to the cap.
        assert_eq!(delay(&zero_base, 1), 1);
    }
}
