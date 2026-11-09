//! Webhook delivery worker and the audit→event mapping.
//!
//! The queue lives in SQLite (`events_outbox`, see `rrouter-store`); this module owns the
//! network side. Nothing here runs on a request path: handlers only enqueue, and the
//! worker started by [`spawn`] polls due rows, signs each body with its receiver's secret,
//! POSTs it, and records the outcome.
//!
//! # Delivery contract
//!
//! The body is the exact bytes stored in the outbox (already redacted by
//! [`events::Event::new`]). Headers are `x-rrouter-event`, `x-rrouter-delivery` and
//! `x-rrouter-signature`; the signature is `t=<unix>,v1=<hex hmac-sha256>` over
//! `"<timestamp>.<body>"`, verified by receivers with [`events::verify`]. The timestamp is
//! the attempt time, so rotating an environment secret takes effect on the next try.
//!
//! # Retries
//!
//! [`events::RetryPolicy`] decides: bounded exponential backoff with deterministic jitter,
//! then a terminal `dead` row. Attempts are counted when a row is claimed, so a crash
//! mid-delivery still consumes an attempt once the lease is reclaimed.
//!
//! # Fan-out
//!
//! One event may match several endpoints (and `*`). Every matching endpoint is tried; the
//! row is completed only when all of them answered 2xx. A retry re-sends to endpoints that
//! already succeeded — receivers dedupe on `x-rrouter-delivery`, which is the outbox id.

use std::sync::Arc;
use std::time::Duration;

use futures_util::stream::{self, StreamExt};
use serde_json::{json, Value};
use tokio::sync::Notify;

use rrouter_core::{Error, Result};
use rrouter_store::{OutboxEvent, Store};

use crate::events::{
    self, Event, EventType, NextAttempt, RetryPolicy, SecretRef, HEADER_DELIVERY, HEADER_EVENT,
    HEADER_SIGNATURE,
};

/// Events delivered per poll tick, and the cap on simultaneous deliveries.
pub const DELIVERY_CONCURRENCY: usize = 4;
/// How often the worker looks for due rows.
pub const POLL_INTERVAL: Duration = Duration::from_secs(1);
/// Upper bound on one receiver's response.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// Terminal rows are pruned once an hour.
const PRUNE_EVERY_SECS: i64 = 3_600;

/// What one poll tick did, for logs and tests.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DeliveryReport {
    pub claimed: usize,
    pub delivered: usize,
    /// Rows that had no enabled subscriber; terminal, and never counted as delivered.
    pub unmatched: usize,
    pub retried: usize,
    pub dead: usize,
}

/// Start the delivery worker. It exits when `stop` is notified; the caller keeps the
/// handle and may abort it on shutdown, so no task outlives the server.
pub fn spawn(
    store: Arc<Store>,
    http: reqwest::Client,
    stop: Arc<Notify>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move { run(store, http, stop).await })
}

async fn run(store: Arc<Store>, http: reqwest::Client, stop: Arc<Notify>) {
    let mut now = crate::now_unix();
    // A crash mid-delivery leaves rows leased; reclaim them before serving traffic.
    match store.recover_stale_events(now).await {
        Ok(0) => {}
        Ok(n) => tracing::info!(rows = n, "webhook worker reclaimed interrupted deliveries"),
        Err(e) => tracing::warn!(error = %e, "webhook worker could not reclaim deliveries"),
    }
    let mut prune_at = now;
    loop {
        tokio::select! {
            _ = stop.notified() => break,
            _ = tokio::time::sleep(POLL_INTERVAL) => {}
        }
        now = crate::now_unix();
        match deliver_due(&store, &http, now, DELIVERY_CONCURRENCY).await {
            Ok(report) if report.claimed > 0 => tracing::debug!(
                claimed = report.claimed,
                delivered = report.delivered,
                unmatched = report.unmatched,
                retried = report.retried,
                dead = report.dead,
                "webhook deliveries processed"
            ),
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "webhook poll failed"),
        }
        if now >= prune_at {
            prune_at = now.saturating_add(PRUNE_EVERY_SECS);
            let cutoff = now.saturating_sub(rrouter_store::OUTBOX_RETENTION_SECS);
            match store.prune_events(cutoff).await {
                Ok(0) => {}
                Ok(n) => tracing::debug!(rows = n, "pruned delivered webhook rows"),
                Err(e) => tracing::warn!(error = %e, "webhook prune failed"),
            }
        }
    }
    tracing::info!("webhook worker stopped");
}

/// Claim and deliver up to `limit` due rows at `now`. Public so tests drive delivery with a
/// fixed clock instead of sleeping.
pub async fn deliver_due(
    store: &Store,
    http: &reqwest::Client,
    now: i64,
    limit: usize,
) -> Result<DeliveryReport> {
    let events = store.claim_due_events(now, limit).await?;
    let mut report = DeliveryReport {
        claimed: events.len(),
        ..DeliveryReport::default()
    };
    if events.is_empty() {
        return Ok(report);
    }
    // Bounded fan-out: at most `limit` deliveries in flight, and the tick does not return
    // until they settle.
    let outcomes = stream::iter(events)
        .map(|event| deliver_one(store, http, event, now))
        .buffer_unordered(limit.max(1))
        .collect::<Vec<_>>()
        .await;
    for outcome in outcomes {
        match outcome {
            AttemptOutcome::Delivered => report.delivered += 1,
            AttemptOutcome::Unmatched => report.unmatched += 1,
            AttemptOutcome::Retried => report.retried += 1,
            AttemptOutcome::Dead => report.dead += 1,
        }
    }
    Ok(report)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AttemptOutcome {
    Delivered,
    /// Terminal: nothing subscribed, so nothing was sent.
    Unmatched,
    Retried,
    Dead,
}

async fn deliver_one(
    store: &Store,
    http: &reqwest::Client,
    event: OutboxEvent,
    now: i64,
) -> AttemptOutcome {
    match post_event(store, http, &event, now).await {
        Ok(0) => {
            // No enabled subscriber: terminal, but not a delivery. `delivered_at` stays
            // NULL so the status screen and counters cannot claim an event was sent.
            let note = "no enabled subscriber for this event type";
            if let Err(e) = store.unmatched_event(&event.id, note).await {
                tracing::warn!(event = %event.id, error = %e, "webhook unmatched but not recorded");
            }
            tracing::debug!(event = %event.id, kind = %event.event_type, "webhook unmatched");
            AttemptOutcome::Unmatched
        }
        Ok(sent) => {
            if let Err(e) = store.complete_event(&event.id, now).await {
                tracing::warn!(event = %event.id, error = %e, "webhook delivered but not recorded");
            }
            tracing::debug!(event = %event.id, kind = %event.event_type, endpoints = sent, "webhook delivered");
            AttemptOutcome::Delivered
        }
        Err(error) => {
            let policy = RetryPolicy::default();
            match policy.next_attempt(now, event.attempts) {
                NextAttempt::RetryIn(delay) => {
                    // Seconds granularity: round up so sub-second jitter never becomes a
                    // zero-delay retry that spins the poll loop.
                    let delay_secs = delay.as_secs().max(1) as i64;
                    let next_at = now.saturating_add(delay_secs);
                    match store
                        .fail_event(&event.id, next_at, &error, policy.max_attempts)
                        .await
                    {
                        Ok(Some(true)) => AttemptOutcome::Dead,
                        Ok(_) => AttemptOutcome::Retried,
                        Err(e) => {
                            tracing::warn!(event = %event.id, error = %e, "could not record webhook failure");
                            AttemptOutcome::Retried
                        }
                    }
                }
                NextAttempt::Drop(reason) => {
                    tracing::debug!(event = %event.id, reason = reason.as_str(), "webhook dropped");
                    // Attempts were already counted at claim; `max_attempts = 1` makes the
                    // transition terminal in the same statement.
                    let note = format!("{error}; dropped: {}", reason.as_str());
                    if let Err(e) = store.fail_event(&event.id, now, &note, 1).await {
                        tracing::warn!(event = %event.id, error = %e, "could not record webhook drop");
                    }
                    AttemptOutcome::Dead
                }
            }
        }
    }
}

/// POST one event to every enabled subscriber. Returns how many endpoints accepted it;
/// the first failure aborts the rest, because the row will be retried as a unit.
async fn post_event(
    store: &Store,
    http: &reqwest::Client,
    event: &OutboxEvent,
    now: i64,
) -> std::result::Result<usize, String> {
    let endpoints = store
        .webhooks_for(&event.event_type)
        .await
        .map_err(|e| e.to_string())?;
    let mut sent = 0usize;
    for endpoint in &endpoints {
        let Some(secret_env) = endpoint.secret_env.as_deref() else {
            return Err(format!(
                "endpoint {} has no secret_env; refusing to send unsigned",
                endpoint.id
            ));
        };
        let secret = SecretRef::env(secret_env)
            .resolve()
            .map_err(|e| e.to_string())?;
        let signature =
            events::signature(&secret, now, event.payload.as_bytes()).map_err(|e| e.to_string())?;
        let response = http
            .post(&endpoint.url)
            .timeout(REQUEST_TIMEOUT)
            .header("content-type", "application/json")
            .header(HEADER_EVENT, &event.event_type)
            .header(HEADER_DELIVERY, &event.id)
            .header(HEADER_SIGNATURE, signature)
            .body(event.payload.clone())
            .send()
            .await
            .map_err(|e| format!("transport: {}", e.without_url()))?;
        let status = response.status();
        if !status.is_success() {
            return Err(format!(
                "endpoint {} answered {}",
                endpoint.id,
                status.as_u16()
            ));
        }
        sent += 1;
    }
    Ok(sent)
}

/// Whether the signing secret named by `secret_env` can be resolved right now. The value
/// is read and discarded, so the UI can warn before the first delivery without the API
/// ever carrying the secret.
pub fn secret_configured(secret_env: &str) -> bool {
    SecretRef::env(secret_env).resolve().is_ok()
}

/// Map an audit kind onto the event vocabulary. Unknown kinds still reach receivers as
/// `audit.recorded`, so the wire contract never silently drops an administrative action.
pub fn event_type_for_audit(kind: &str) -> EventType {
    match kind {
        "key_created" => EventType::KeyCreated,
        "key_updated" => EventType::KeyUpdated,
        "key_deleted" => EventType::KeyDeleted,
        "team_created" => EventType::TeamCreated,
        "team_updated" => EventType::TeamUpdated,
        "team_deleted" => EventType::TeamDeleted,
        "team_member_added" => EventType::TeamMemberAdded,
        "team_member_removed" => EventType::TeamMemberRemoved,
        "connection_created" => EventType::ConnectionCreated,
        "connection_updated" => EventType::ConnectionUpdated,
        "connection_deleted" => EventType::ConnectionDeleted,
        "connection_degraded" => EventType::ConnectionDegraded,
        "connection_recovered" => EventType::ConnectionRecovered,
        _ => EventType::AuditRecorded,
    }
}

/// Queue the audit row's event. The payload is built through [`Event::new`], which redacts
/// free text before it can reach the outbox.
pub async fn enqueue_audit(
    store: &Store,
    kind: &str,
    actor: Option<&str>,
    ip: &str,
    detail: &str,
) -> Result<bool> {
    let event_type = event_type_for_audit(kind);
    let data = json!({
        "kind": kind,
        "actor": actor,
        "ip": ip,
        "detail": detail,
    });
    enqueue_event(store, event_type, data).await
}

/// Enqueue an event with a fresh id. Returns `false` when the id was already queued.
pub async fn enqueue_event(store: &Store, kind: EventType, data: Value) -> Result<bool> {
    let event = Event::new(kind, crate::now_unix(), data);
    let payload = String::from_utf8(event.to_json())
        .map_err(|_| Error::BadRequest("event payload is not utf-8".into()))?;
    store
        .enqueue_event(
            event.id(),
            event.kind().as_str(),
            &payload,
            event.created_at(),
        )
        .await
}
