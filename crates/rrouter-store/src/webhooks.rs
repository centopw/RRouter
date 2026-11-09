//! Webhook endpoints and the delivery outbox.
//!
//! Two tables from schema v25:
//!
//! * `webhook_endpoints` is the receiver registry. A row names an `https://` URL, the
//!   event types it wants, an environment variable that holds its signing secret (the
//!   secret itself is never stored), and an operator switch.
//! * `events_outbox` is a queue, not an audit trail: one row per event, keyed by the
//!   event id, so a retried enqueue cannot duplicate a delivery. The dispatcher claims a
//!   row in a transaction, tries it, then either completes it or records the next attempt
//!   (or the terminal `dead` state).
//!
//! # Clock
//!
//! Every method that depends on time takes `now` (unix seconds) from the caller. The
//! worker passes the wall clock; tests pass a fixed clock, so retries and leases are
//! deterministic without sleeping.
//!
//! # Validation
//!
//! The store enforces the invariants it can state without the gateway's event vocabulary:
//! the URL scheme and shape, the endpoint/event-type limits, and the secret reference
//! shape. Whether an event type exists is the API's rule (it owns [`EventType`]).
//!
//! [`EventType`]: https://docs.rs/rrouter-gateway

use rusqlite::{params, OptionalExtension, Row};
use uuid::Uuid;

use rrouter_core::{Error, Result};

use crate::migrations::db_err;
use crate::store::Store;

/// Endpoints one instance may register. Bounded so a runaway script cannot turn every
/// mutation into an unbounded fan-out.
pub const MAX_WEBHOOKS: i64 = 32;
/// Subscriptions one endpoint may carry (the event vocabulary is smaller than this).
pub const MAX_EVENT_TYPES: usize = 32;
/// Longest accepted receiver URL.
pub const MAX_URL_LEN: usize = 2048;
/// Longest accepted environment-variable name for a signing secret.
pub const MAX_SECRET_ENV_LEN: usize = 128;
/// Largest stored event body. Events are administrative facts, not documents.
pub const MAX_PAYLOAD_LEN: usize = 256 * 1024;
/// How long a claimed row stays leased before another worker may reclaim it.
pub const DELIVERY_LEASE_SECS: i64 = 120;
/// Terminal rows are kept this long for the operator's status screen, then swept.
pub const OUTBOX_RETENTION_SECS: i64 = 7 * 86_400;

/// The queue's state vocabulary.
pub const STATUS_PENDING: &str = "pending";
pub const STATUS_DELIVERING: &str = "delivering";
/// Every matching endpoint accepted the event.
pub const STATUS_DELIVERED: &str = "delivered";
/// Terminal: no enabled endpoint subscribed when the dispatch ran. Nothing was sent, so
/// this is *not* a delivery and must not be counted as one; it is terminal so the queue
/// does not retry an event nobody wants.
pub const STATUS_UNMATCHED: &str = "unmatched";
/// Terminal: retries were spent (or the attempt cap was reached) without a 2xx.
pub const STATUS_DEAD: &str = "dead";

/// One registered receiver. `secret_env` names where the signing secret lives; the secret
/// value is never part of this struct, so no API can accidentally return it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebhookEndpoint {
    pub id: Uuid,
    pub url: String,
    /// Wire names such as `key.created`; `*` subscribes to every type.
    pub event_types: Vec<String>,
    pub secret_env: Option<String>,
    pub enabled: bool,
    pub created_at: i64,
    pub updated_at: Option<i64>,
}

/// Fields an edit may change. `None` leaves the field alone; `secret_env: Some(None)`
/// clears the reference.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WebhookPatch {
    pub url: Option<String>,
    pub event_types: Option<Vec<String>>,
    pub secret_env: Option<Option<String>>,
    pub enabled: Option<bool>,
}

/// One queued event. `payload` is the exact body that is signed and delivered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxEvent {
    pub id: String,
    pub event_type: String,
    pub payload: String,
    pub created_at: i64,
    pub next_attempt_at: i64,
    pub attempts: u32,
    pub status: String,
    pub last_error: Option<String>,
    pub delivered_at: Option<i64>,
}

/// Queue depth by state, for the admin status screen.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OutboxStatus {
    pub pending: i64,
    pub delivering: i64,
    pub delivered: i64,
    /// Terminal rows that matched no enabled subscriber; never counted as delivered.
    pub unmatched: i64,
    pub dead: i64,
}

fn row_to_endpoint(r: &Row<'_>) -> rusqlite::Result<WebhookEndpoint> {
    let raw_types: String = r.get(1)?;
    let event_types: Vec<String> = serde_json::from_str(&raw_types).unwrap_or_else(|_| {
        raw_types
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect()
    });
    let raw_id: String = r.get(0)?;
    Ok(WebhookEndpoint {
        id: Uuid::parse_str(&raw_id).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })?,
        url: r.get(2)?,
        event_types,
        secret_env: r.get(3)?,
        enabled: r.get::<_, i64>(4)? != 0,
        created_at: r.get(5)?,
        updated_at: r.get(6)?,
    })
}

const ENDPOINT_COLUMNS: &str = "id, event_types, url, secret_env, enabled, created_at, updated_at";

fn row_to_event(r: &Row<'_>) -> rusqlite::Result<OutboxEvent> {
    Ok(OutboxEvent {
        id: r.get(0)?,
        event_type: r.get(1)?,
        payload: r.get(2)?,
        created_at: r.get(3)?,
        next_attempt_at: r.get(4)?,
        attempts: r.get::<_, i64>(5)? as u32,
        status: r.get(6)?,
        last_error: r.get(7)?,
        delivered_at: r.get(8)?,
    })
}

const EVENT_COLUMNS: &str = "id, type, payload, created_at, next_attempt_at, attempts, status, \
                             last_error, delivered_at";

/// `https://` plus a host, no userinfo, no whitespace. The store does not resolve the
/// host: an operator may deliberately point a webhook at an internal receiver.
fn validate_url(raw: &str) -> Result<String> {
    let url = raw.trim();
    if url.is_empty() {
        return Err(Error::BadRequest("webhook url is required".into()));
    }
    if url.len() > MAX_URL_LEN {
        return Err(Error::BadRequest(format!(
            "webhook url is longer than {MAX_URL_LEN} bytes"
        )));
    }
    // `https://` is the rule. Plain `http://` is accepted only for a loopback receiver
    // (a local sidecar or the test mock): the traffic never leaves the host, and refusing
    // it would make every deployment need a TLS terminator just to watch events locally.
    let rest = if let Some(rest) = url.strip_prefix("https://") {
        rest
    } else if let Some(rest) = url.strip_prefix("http://") {
        let host = rest
            .split(['/', '?', '#'])
            .next()
            .unwrap_or_default()
            .split(':')
            .next()
            .unwrap_or_default();
        if !matches!(host, "127.0.0.1" | "localhost" | "[::1]") {
            return Err(Error::BadRequest(
                "webhook url must use https:// (http:// is allowed only for loopback)".into(),
            ));
        }
        rest
    } else {
        return Err(Error::BadRequest(
            "webhook url must start with https://".into(),
        ));
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.is_empty() {
        return Err(Error::BadRequest("webhook url has no host".into()));
    }
    if authority.contains('@') {
        return Err(Error::BadRequest(
            "webhook url must not embed credentials".into(),
        ));
    }
    if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(Error::BadRequest(
            "webhook url must not contain whitespace or control characters".into(),
        ));
    }
    Ok(url.to_string())
}

/// Event-type wire names: non-empty, bounded, unique, ASCII `[a-z0-9._*-]`. `*` is kept
/// as the whole-subscription wildcard.
fn validate_event_types(raw: Vec<String>) -> Result<Vec<String>> {
    let mut out: Vec<String> = Vec::with_capacity(raw.len());
    for entry in raw {
        let name = entry.trim();
        if name.is_empty() {
            return Err(Error::BadRequest(
                "webhook event types must not be empty".into(),
            ));
        }
        if name.len() > 64 {
            return Err(Error::BadRequest(
                "webhook event type is longer than 64 bytes".into(),
            ));
        }
        if !name.chars().all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-' | '*')
        }) {
            return Err(Error::BadRequest(format!(
                "webhook event type {name:?} is not a known shape"
            )));
        }
        if !out.iter().any(|seen| seen == name) {
            out.push(name.to_string());
        }
    }
    if out.is_empty() {
        return Err(Error::BadRequest(
            "a webhook needs at least one event type".into(),
        ));
    }
    if out.len() > MAX_EVENT_TYPES {
        return Err(Error::BadRequest(format!(
            "a webhook may subscribe to at most {MAX_EVENT_TYPES} event types"
        )));
    }
    Ok(out)
}

fn validate_secret_env(raw: Option<&str>) -> Result<Option<String>> {
    let Some(name) = raw else { return Ok(None) };
    let name = name.trim();
    if name.is_empty() {
        return Err(Error::BadRequest(
            "webhook secret_env must name an environment variable".into(),
        ));
    }
    if name.len() > MAX_SECRET_ENV_LEN {
        return Err(Error::BadRequest(format!(
            "webhook secret_env is longer than {MAX_SECRET_ENV_LEN} bytes"
        )));
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(Error::BadRequest(format!(
            "webhook secret_env {name:?} is not an environment variable name"
        )));
    }
    Ok(Some(name.to_string()))
}

fn encode_types(types: &[String]) -> String {
    serde_json::to_string(types).expect("a list of strings always serializes")
}

impl Store {
    /// Register a receiver. The URL and subscriptions are validated here; the caller
    /// (the API) additionally rejects types this build does not know.
    pub async fn create_webhook(
        &self,
        url: &str,
        event_types: Vec<String>,
        secret_env: Option<&str>,
        now: i64,
    ) -> Result<WebhookEndpoint> {
        let url = validate_url(url)?;
        let event_types = validate_event_types(event_types)?;
        let secret_env = validate_secret_env(secret_env)?;
        let id = Uuid::new_v4();
        let mut conn = self.conn();
        let tx = conn.transaction().map_err(db_err)?;
        let count: i64 = tx
            .query_row("SELECT COUNT(*) FROM webhook_endpoints", [], |r| r.get(0))
            .map_err(db_err)?;
        if count >= MAX_WEBHOOKS {
            return Err(Error::BadRequest(format!(
                "at most {MAX_WEBHOOKS} webhook endpoints may be registered"
            )));
        }
        let clash: Option<String> = tx
            .query_row(
                "SELECT id FROM webhook_endpoints WHERE url = ?1",
                params![url],
                |r| r.get(0),
            )
            .optional()
            .map_err(db_err)?;
        if clash.is_some() {
            return Err(Error::BadRequest(
                "a webhook with this url is already registered".into(),
            ));
        }
        tx.execute(
            "INSERT INTO webhook_endpoints (id, url, event_types, secret_env, enabled, created_at) \
             VALUES (?1, ?2, ?3, ?4, 1, ?5)",
            params![id.to_string(), &url, encode_types(&event_types), secret_env.as_deref(), now],
        )
        .map_err(db_err)?;
        tx.commit().map_err(db_err)?;
        Ok(WebhookEndpoint {
            id,
            url,
            event_types,
            secret_env,
            enabled: true,
            created_at: now,
            updated_at: None,
        })
    }

    pub async fn list_webhooks(&self) -> Result<Vec<WebhookEndpoint>> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {ENDPOINT_COLUMNS} FROM webhook_endpoints ORDER BY created_at, rowid"
            ))
            .map_err(db_err)?;
        let rows = stmt
            .query_map([], row_to_endpoint)
            .map_err(db_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(db_err)?;
        Ok(rows)
    }

    pub async fn webhook(&self, id: Uuid) -> Result<Option<WebhookEndpoint>> {
        self.conn()
            .query_row(
                &format!("SELECT {ENDPOINT_COLUMNS} FROM webhook_endpoints WHERE id = ?1"),
                params![id.to_string()],
                row_to_endpoint,
            )
            .optional()
            .map_err(db_err)
    }

    /// Apply an edit; `Ok(None)` when the endpoint does not exist. Validation runs on the
    /// merged row, so a partial edit cannot leave an invalid endpoint behind.
    pub async fn update_webhook(
        &self,
        id: Uuid,
        patch: &WebhookPatch,
        now: i64,
    ) -> Result<Option<WebhookEndpoint>> {
        let mut conn = self.conn();
        let tx = conn.transaction().map_err(db_err)?;
        let current = tx
            .query_row(
                &format!("SELECT {ENDPOINT_COLUMNS} FROM webhook_endpoints WHERE id = ?1"),
                params![id.to_string()],
                row_to_endpoint,
            )
            .optional()
            .map_err(db_err)?;
        let Some(current) = current else {
            return Ok(None);
        };
        let url = match &patch.url {
            Some(url) => validate_url(url)?,
            None => current.url.clone(),
        };
        let event_types = match &patch.event_types {
            Some(types) => validate_event_types(types.clone())?,
            None => current.event_types,
        };
        let secret_env = match &patch.secret_env {
            Some(value) => validate_secret_env(value.as_deref())?,
            None => current.secret_env,
        };
        let enabled = patch.enabled.unwrap_or(current.enabled);
        if url != current.url {
            let clash: Option<String> = tx
                .query_row(
                    "SELECT id FROM webhook_endpoints WHERE url = ?1 AND id <> ?2",
                    params![&url, id.to_string()],
                    |r| r.get(0),
                )
                .optional()
                .map_err(db_err)?;
            if clash.is_some() {
                return Err(Error::BadRequest(
                    "a webhook with this url is already registered".into(),
                ));
            }
        }
        tx.execute(
            "UPDATE webhook_endpoints SET url = ?2, event_types = ?3, secret_env = ?4, \
             enabled = ?5, updated_at = ?6 WHERE id = ?1",
            params![
                id.to_string(),
                &url,
                encode_types(&event_types),
                secret_env.as_deref(),
                i64::from(enabled),
                now
            ],
        )
        .map_err(db_err)?;
        tx.commit().map_err(db_err)?;
        Ok(Some(WebhookEndpoint {
            id,
            url,
            event_types,
            secret_env,
            enabled,
            created_at: current.created_at,
            updated_at: Some(now),
        }))
    }

    pub async fn delete_webhook(&self, id: Uuid) -> Result<bool> {
        let changed = self
            .conn()
            .execute(
                "DELETE FROM webhook_endpoints WHERE id = ?1",
                params![id.to_string()],
            )
            .map_err(db_err)?;
        Ok(changed > 0)
    }

    /// Enabled receivers subscribed to `event_type`, in registration order.
    pub async fn webhooks_for(&self, event_type: &str) -> Result<Vec<WebhookEndpoint>> {
        let all = self.list_webhooks().await?;
        Ok(all
            .into_iter()
            .filter(|endpoint| {
                endpoint.enabled
                    && endpoint
                        .event_types
                        .iter()
                        .any(|wanted| wanted == "*" || wanted == event_type)
            })
            .collect())
    }

    /// Queue one event. The id is the idempotency key: re-enqueuing the same event (a
    /// caller retry, a replay) inserts nothing and returns `false`.
    pub async fn enqueue_event(
        &self,
        id: &str,
        event_type: &str,
        payload: &str,
        now: i64,
    ) -> Result<bool> {
        let id = id.trim();
        if id.is_empty() || id.len() > 64 {
            return Err(Error::BadRequest("event id is required".into()));
        }
        let event_type = event_type.trim();
        if event_type.is_empty() {
            return Err(Error::BadRequest("event type is required".into()));
        }
        if payload.is_empty() {
            return Err(Error::BadRequest("event payload is required".into()));
        }
        if payload.len() > MAX_PAYLOAD_LEN {
            return Err(Error::BadRequest(format!(
                "event payload is longer than {MAX_PAYLOAD_LEN} bytes"
            )));
        }
        let changed = self
            .conn()
            .execute(
                "INSERT OR IGNORE INTO events_outbox \
                 (id, type, payload, created_at, next_attempt_at, attempts, status) \
                 VALUES (?1, ?2, ?3, ?4, ?4, 0, 'pending')",
                params![id, event_type, payload, now],
            )
            .map_err(db_err)?;
        Ok(changed > 0)
    }

    /// Lease up to `limit` due rows in one transaction: select, mark `delivering`, bump
    /// `attempts`, and move the lease marker forward. Another worker (or this one after a
    /// crash) can reclaim the row once the lease expires via [`Store::recover_stale_events`].
    pub async fn claim_due_events(&self, now: i64, limit: usize) -> Result<Vec<OutboxEvent>> {
        let limit = i64::try_from(limit.clamp(1, 64)).unwrap_or(8);
        let mut conn = self.conn();
        let tx = conn.transaction().map_err(db_err)?;
        let mut events = {
            let mut stmt = tx
                .prepare(&format!(
                    "SELECT {EVENT_COLUMNS} FROM events_outbox \
                     WHERE status = 'pending' AND next_attempt_at <= ?1 \
                     ORDER BY next_attempt_at, created_at LIMIT ?2"
                ))
                .map_err(db_err)?;
            let rows = stmt
                .query_map(params![now, limit], row_to_event)
                .map_err(db_err)?;
            rows.collect::<rusqlite::Result<Vec<_>>>().map_err(db_err)?
        };
        if !events.is_empty() {
            let lease = now.saturating_add(DELIVERY_LEASE_SECS);
            let mut update = tx
                .prepare(
                    "UPDATE events_outbox SET status = 'delivering', \
                     attempts = attempts + 1, next_attempt_at = ?2 WHERE id = ?1",
                )
                .map_err(db_err)?;
            for event in &mut events {
                update.execute(params![&event.id, lease]).map_err(db_err)?;
                event.status = STATUS_DELIVERING.to_string();
                event.attempts = event.attempts.saturating_add(1);
                event.next_attempt_at = lease;
            }
        }
        tx.commit().map_err(db_err)?;
        Ok(events)
    }

    /// Finish a claimed row as delivered.
    pub async fn complete_event(&self, id: &str, now: i64) -> Result<bool> {
        let changed = self
            .conn()
            .execute(
                "UPDATE events_outbox SET status = 'delivered', delivered_at = ?2, \
                 last_error = NULL WHERE id = ?1 AND status = 'delivering'",
                params![id, now],
            )
            .map_err(db_err)?;
        Ok(changed > 0)
    }

    /// Finish a claimed row that had no enabled subscriber. Terminal and explicitly *not*
    /// delivered: `delivered_at` stays NULL so the status screen cannot confuse the two.
    pub async fn unmatched_event(&self, id: &str, reason: &str) -> Result<bool> {
        let changed = self
            .conn()
            .execute(
                "UPDATE events_outbox SET status = 'unmatched', last_error = ?2 \
                 WHERE id = ?1 AND status = 'delivering'",
                params![id, reason],
            )
            .map_err(db_err)?;
        Ok(changed > 0)
    }

    /// Record a failed attempt in one atomic statement. When the row has already spent
    /// `max_attempts` it becomes `dead`; otherwise it returns to `pending` at
    /// `next_attempt_at`. `None` means the row was not in the `delivering` state (another
    /// worker settled it, or it was never claimed); `Some(true)` means the row is now dead.
    pub async fn fail_event(
        &self,
        id: &str,
        next_attempt_at: i64,
        error: &str,
        max_attempts: u32,
    ) -> Result<Option<bool>> {
        let max_attempts = i64::from(max_attempts.max(1));
        let dead: Option<i64> = self
            .conn()
            .query_row(
                "UPDATE events_outbox SET \
                   status = CASE WHEN attempts >= ?3 THEN 'dead' ELSE 'pending' END, \
                   next_attempt_at = CASE WHEN attempts >= ?3 THEN next_attempt_at ELSE ?2 END, \
                   last_error = ?4 \
                 WHERE id = ?1 AND status = 'delivering' \
                 RETURNING status = 'dead'",
                params![id, next_attempt_at, max_attempts, error],
                |r| r.get(0),
            )
            .optional()
            .map_err(db_err)?;
        Ok(dead.map(|value| value != 0))
    }

    /// Return leased rows whose worker died to `pending`. Safe to run at boot and on every
    /// poll tick: a live lease is never earlier than `now`.
    pub async fn recover_stale_events(&self, now: i64) -> Result<usize> {
        self.conn()
            .execute(
                "UPDATE events_outbox SET status = 'pending' \
                 WHERE status = 'delivering' AND next_attempt_at <= ?1",
                params![now],
            )
            .map_err(db_err)
    }

    /// Delete terminal rows older than `cutoff`. Pending work is never swept.
    pub async fn prune_events(&self, cutoff: i64) -> Result<usize> {
        self.conn()
            .execute(
                "DELETE FROM events_outbox \
                 WHERE status IN ('delivered', 'unmatched', 'dead') AND created_at < ?1",
                params![cutoff],
            )
            .map_err(db_err)
    }

    pub async fn outbox_status(&self) -> Result<OutboxStatus> {
        self.conn()
            .query_row(
                "SELECT \
                   COALESCE(SUM(status = 'pending'), 0), \
                   COALESCE(SUM(status = 'delivering'), 0), \
                   COALESCE(SUM(status = 'delivered'), 0), \
                   COALESCE(SUM(status = 'unmatched'), 0), \
                   COALESCE(SUM(status = 'dead'), 0) \
                 FROM events_outbox",
                [],
                |r| {
                    Ok(OutboxStatus {
                        pending: r.get(0)?,
                        delivering: r.get(1)?,
                        delivered: r.get(2)?,
                        unmatched: r.get(3)?,
                        dead: r.get(4)?,
                    })
                },
            )
            .map_err(db_err)
    }

    /// Newest queued rows, for the operator's delivery screen. The caller decides whether
    /// to expose `payload`.
    pub async fn recent_events(&self, limit: usize) -> Result<Vec<OutboxEvent>> {
        let limit = i64::try_from(limit.clamp(1, 200)).unwrap_or(50);
        let conn = self.conn();
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {EVENT_COLUMNS} FROM events_outbox ORDER BY created_at DESC, rowid DESC LIMIT ?1"
            ))
            .map_err(db_err)?;
        let rows = stmt
            .query_map(params![limit], row_to_event)
            .map_err(db_err)?;
        let rows = rows.collect::<rusqlite::Result<Vec<_>>>().map_err(db_err)?;
        Ok(rows)
    }
}
