//! Per-connection circuit breaker.
//!
//! A binary per-account lock either hammers a dead account or locks a healthy one
//! out for a fixed period. This is a real closed → open → half-open breaker: consecutive
//! failures trip it, the open
//! window is time-based, and reopening admits exactly one probe at a time so a
//! recovered account is picked up without a thundering herd.
//!
//! # Two scopes
//!
//! Auth failures (401/403), server errors (5xx) and transport failures say the
//! *account* is unhealthy: they open the connection-wide breaker above. A 429 is
//! different — a free-tier or per-model limit belongs to `(connection, model)`, so it
//! starts a model cooldown and leaves the connection and every other model eligible.
//! Dispatchers must record through [`Breaker::record_failure_for`] (which carries the
//! candidate's model), never the low-level `record_failure` for a 429.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use uuid::Uuid;

/// Open window after a generic server error (5xx).
const OPEN_SERVER_ERROR: Duration = Duration::from_secs(30);
/// Open window after an auth failure: the credential is dead until an operator
/// (or the auth refresher) replaces it, so back off hard.
const OPEN_AUTH_ERROR: Duration = Duration::from_secs(300);
/// Rate-limit backoff is exponential in the consecutive-429 count, starting here.
/// Subscription quota windows outlive a one-second retry, so the first cooldown is
/// already long enough to send the next request to another sub-account.
const BACKOFF_BASE: Duration = Duration::from_secs(30);
/// Never park an account longer than this on 429s; repeated strikes escalate to an
/// hour instead of hammering an exhausted quota every four minutes.
const BACKOFF_CAP: Duration = Duration::from_secs(3600);
/// Reported wait when a half-open connection's single probe slot is in use.
const PROBE_TAKEN: Duration = Duration::from_secs(3600);

/// What [`Breaker::claim`] granted: a half-open slot from one specific episode,
/// or nothing (breaker closed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeTicket {
    None,
    Probe { epoch: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakerState {
    Closed,
    Open { until: Instant },
    HalfOpen { probes: u8 },
}

struct Policy {
    failures: u32,
    open_for: Duration,
    probes: u8,
}

/// One model's 429 cooldown on one connection. Model limits are independent: a
/// free-tier exhaustion is a fact about that model, not the account's transport.
struct ModelEntry {
    until: Instant,
    strikes: u32,
}

struct Entry {
    state: BreakerState,
    consecutive_failures: u32,
    /// Consecutive 429s; drives exponential backoff.
    rate_limit_strikes: u32,
    /// Incremented every time the breaker enters a new half-open episode. A probe
    /// claim records it so a later cancellation can only release *its own* slot,
    /// never one taken by a newer episode.
    epoch: u64,
}

impl Entry {
    fn new() -> Self {
        Self {
            state: BreakerState::Closed,
            consecutive_failures: 0,
            rate_limit_strikes: 0,
            epoch: 0,
        }
    }
}

/// Failure history per connection. Share it by wrapping in `Arc`; the handle is
/// what streams and spawned tasks hold.
pub struct Breaker {
    policy: Policy,
    entries: Mutex<HashMap<Uuid, Entry>>,
    /// `(connection, model)` → the 429 cooldown for that model on that account.
    model_entries: Mutex<HashMap<(Uuid, String), ModelEntry>>,
}

impl Default for Breaker {
    fn default() -> Self {
        Self::new()
    }
}

impl Breaker {
    /// 3 consecutive failures tripping, 30 s open, 1 half-open probe.
    pub fn new() -> Self {
        Self::with_policy(3, OPEN_SERVER_ERROR, 1)
    }

    pub fn with_policy(failures: u32, open_for: Duration, probes: u8) -> Self {
        Self {
            policy: Policy {
                failures: failures.max(1),
                open_for,
                probes: probes.max(1),
            },
            entries: Mutex::new(HashMap::new()),
            model_entries: Mutex::new(HashMap::new()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<Uuid, Entry>> {
        // A poisoned map only means another task panicked mid-update; the map is
        // still structurally sound, so recover instead of propagating the panic.
        self.entries.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn model_lock(&self) -> std::sync::MutexGuard<'_, HashMap<(Uuid, String), ModelEntry>> {
        self.model_entries.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Model-scoped cooldown, independent of the connection breaker.
    pub fn check_model(&self, conn: Uuid, model: &str) -> Option<Duration> {
        self.check_model_at(conn, model, Instant::now())
    }

    /// Clock-injected form of [`Breaker::check_model`]. An expired cooldown is
    /// removed so a long-lived process does not accumulate stale keys.
    pub fn check_model_at(&self, conn: Uuid, model: &str, now: Instant) -> Option<Duration> {
        let mut map = self.model_lock();
        let key = (conn, model.to_string());
        let entry = map.get(&key)?;
        if now >= entry.until {
            map.remove(&key);
            return None;
        }
        Some(entry.until.duration_since(now))
    }

    /// Earliest time both the connection and this model are eligible.
    pub fn check_candidate(&self, conn: Uuid, model: &str) -> Option<Duration> {
        self.check_candidate_at(conn, model, Instant::now())
    }

    pub fn check_candidate_at(&self, conn: Uuid, model: &str, now: Instant) -> Option<Duration> {
        let connection = self.check_at(conn, now);
        let model = self.check_model_at(conn, model, now);
        match (connection, model) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (Some(wait), None) | (None, Some(wait)) => Some(wait),
            (None, None) => None,
        }
    }

    /// Claim a candidate: a model cooldown is a plain skip (no probe slot), while the
    /// connection-wide half-open probe is claimed exactly as before.
    pub fn claim_candidate(&self, conn: Uuid, model: &str) -> Result<ProbeTicket, Duration> {
        self.claim_candidate_at(conn, model, Instant::now())
    }

    pub fn claim_candidate_at(
        &self,
        conn: Uuid,
        model: &str,
        now: Instant,
    ) -> Result<ProbeTicket, Duration> {
        if let Some(wait) = self.check_model_at(conn, model, now) {
            return Err(wait);
        }
        self.claim_at(conn, now)
    }

    /// `None` → eligible now. `Some(wait)` → skip this candidate, earliest retry
    /// after `wait`.
    pub fn check(&self, conn: Uuid) -> Option<Duration> {
        self.check_at(conn, Instant::now())
    }

    /// Clock-injected form of [`Breaker::check`] (deterministic tests).
    pub fn check_at(&self, conn: Uuid, now: Instant) -> Option<Duration> {
        let map = self.lock();
        match map.get(&conn).map(|e| e.state) {
            None | Some(BreakerState::Closed) => None,
            Some(BreakerState::Open { until }) if now >= until => None,
            Some(BreakerState::Open { until }) => Some(until.duration_since(now)),
            Some(BreakerState::HalfOpen { probes }) if probes < self.policy.probes => None,
            Some(BreakerState::HalfOpen { .. }) => Some(PROBE_TAKEN),
        }
    }

    /// Claim the right to send one request, consuming a half-open probe slot.
    ///
    /// `Err(wait)` means do not send. `Ok(true)` means a half-open probe slot was
    /// **taken** and the caller must give it back with [`Breaker::release_probe`] if
    /// the attempt is cancelled without a verdict. `Ok(false)` means the breaker is
    /// closed and no slot was consumed — releasing then would hand back a slot
    /// another task holds.
    pub fn claim(&self, conn: Uuid) -> Result<ProbeTicket, Duration> {
        self.claim_at(conn, Instant::now())
    }

    /// Clock-injected form of [`Breaker::claim`].
    ///
    /// On success the ticket says whether a half-open slot was taken and, if so,
    /// which episode it belongs to. Pass it back to [`Breaker::release_probe`] if
    /// the attempt is cancelled without a verdict.
    pub fn claim_at(&self, conn: Uuid, now: Instant) -> Result<ProbeTicket, Duration> {
        let mut map = self.lock();
        let entry = map.entry(conn).or_insert_with(Entry::new);
        // An elapsed open window is *reported* as half-open, and only an actual
        // probe claim advances it. Entering a window starts a new episode.
        if let BreakerState::Open { until } = entry.state {
            if now >= until {
                entry.state = BreakerState::HalfOpen { probes: 0 };
                entry.epoch = entry.epoch.wrapping_add(1);
            }
        }
        match entry.state {
            BreakerState::Closed => Ok(ProbeTicket::None),
            BreakerState::HalfOpen { probes } if probes < self.policy.probes => {
                // Exactly one caller wins the slot; the rest see it taken.
                entry.state = BreakerState::HalfOpen { probes: probes + 1 };
                Ok(ProbeTicket::Probe { epoch: entry.epoch })
            }
            BreakerState::HalfOpen { .. } => Err(PROBE_TAKEN),
            BreakerState::Open { until } => Err(until.duration_since(now)),
        }
    }

    pub fn record_success(&self, conn: Uuid) {
        let mut map = self.lock();
        let entry = map.entry(conn).or_insert_with(Entry::new);
        entry.state = BreakerState::Closed;
        entry.consecutive_failures = 0;
        entry.rate_limit_strikes = 0;
    }

    /// Success for one model: clears that model's 429 cooldown **and** applies the
    /// existing connection-wide success reset.
    pub fn record_success_for(&self, conn: Uuid, model: &str) {
        self.model_lock().remove(&(conn, model.to_string()));
        self.record_success(conn);
    }

    /// Record a failure with the candidate's model.
    ///
    /// 429 is model-scoped: it starts an exponential cooldown for `(conn, model)` and
    /// does not open the connection-wide breaker. Every other status keeps the
    /// connection-wide behaviour, including the half-open semantics.
    pub fn record_failure_for(
        &self,
        conn: Uuid,
        model: &str,
        status: u16,
        retry_after: Option<Duration>,
    ) {
        self.record_failure_for_at(conn, model, status, retry_after, Instant::now());
    }

    pub fn record_failure_for_at(
        &self,
        conn: Uuid,
        model: &str,
        status: u16,
        retry_after: Option<Duration>,
        now: Instant,
    ) {
        if status == 429 {
            let mut map = self.model_lock();
            let entry = map.entry((conn, model.to_string())).or_insert(ModelEntry {
                until: now,
                strikes: 0,
            });
            entry.strikes = entry.strikes.saturating_add(1);
            let exponential = BACKOFF_BASE
                .checked_mul(1u32 << (entry.strikes - 1).min(16))
                .unwrap_or(BACKOFF_CAP)
                .min(BACKOFF_CAP);
            let wait = retry_after.unwrap_or(exponential).min(BACKOFF_CAP);
            entry.until = now + wait;
            drop(map);
            // The account itself answered: a half-open probe is not re-opened by a
            // model-specific limit. Other models on this connection stay eligible.
            self.record_success(conn);
            return;
        }
        self.record_failure_at(conn, status, retry_after, now);
    }

    /// Give back a claimed half-open probe slot without judging the attempt.
    ///
    /// Used when a request is cancelled before it produced a verdict (client
    /// disconnect): the account is neither proven healthy nor proven broken, so
    /// the slot is returned and the state otherwise left alone.
    pub fn release_probe(&self, conn: Uuid, ticket: ProbeTicket) {
        let ProbeTicket::Probe { epoch } = ticket else {
            return;
        };
        let mut map = self.lock();
        if let Some(entry) = map.get_mut(&conn) {
            // Only the episode that granted this ticket may be rolled back: a stale
            // guard must never free a probe belonging to a newer half-open window.
            if entry.epoch == epoch {
                if let BreakerState::HalfOpen { probes } = entry.state {
                    entry.state = BreakerState::HalfOpen {
                        probes: probes.saturating_sub(1),
                    };
                }
            }
        }
    }

    /// Connection-wide failure recording. 5xx → 30 s; 401/403 → 300 s; anything else
    /// counts toward tripping. A 429 passed here keeps the historical connection-wide
    /// behaviour for direct callers; dispatchers use [`Breaker::record_failure_for`],
    /// which scopes a 429 to the model.
    pub fn record_failure(&self, conn: Uuid, status: u16, retry_after: Option<Duration>) {
        self.record_failure_at(conn, status, retry_after, Instant::now());
    }

    /// Clock-injected form of [`Breaker::record_failure`].
    pub fn record_failure_at(
        &self,
        conn: Uuid,
        status: u16,
        retry_after: Option<Duration>,
        now: Instant,
    ) {
        let mut map = self.lock();
        let entry = map.entry(conn).or_insert_with(Entry::new);

        // Any failure from a probe re-opens immediately for a full window.
        let was_half_open = matches!(entry.state, BreakerState::HalfOpen { .. });
        entry.consecutive_failures = entry.consecutive_failures.saturating_add(1);

        let open_for = match status {
            429 => {
                entry.rate_limit_strikes = entry.rate_limit_strikes.saturating_add(1);
                let exp = BACKOFF_BASE
                    .checked_mul(1u32 << (entry.rate_limit_strikes - 1).min(16))
                    .unwrap_or(BACKOFF_CAP)
                    .min(BACKOFF_CAP);
                retry_after.unwrap_or(exp).min(BACKOFF_CAP)
            }
            401 | 403 => OPEN_AUTH_ERROR,
            _ => self.policy.open_for,
        };

        let tripped = status == 429
            || status == 401
            || status == 403
            || entry.consecutive_failures >= self.policy.failures;
        if tripped || was_half_open {
            entry.state = BreakerState::Open {
                until: now + open_for,
            };
        }
    }

    /// True when a candidate could be attempted right now.
    pub fn is_available(&self, conn: Uuid) -> bool {
        self.check(conn).is_none()
    }

    /// Raw state, for introspection and tests. Does not advance the breaker.
    pub fn state(&self, conn: Uuid) -> BreakerState {
        self.state_at(conn, Instant::now())
    }

    pub fn state_at(&self, conn: Uuid, now: Instant) -> BreakerState {
        let map = self.lock();
        match map.get(&conn) {
            None => BreakerState::Closed,
            Some(e) => match e.state {
                BreakerState::Open { until } if now >= until => {
                    BreakerState::HalfOpen { probes: 0 }
                }
                other => other,
            },
        }
    }
}
