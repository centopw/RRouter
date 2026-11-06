//! Per-connection circuit breaker.
//!
//! 9router kept a single binary lock per account; that either hammers a dead
//! account or locks a healthy one out for a fixed period. This is a real
//! closed → open → half-open breaker: consecutive failures trip it, the open
//! window is time-based, and reopening admits exactly one probe at a time so a
//! recovered account is picked up without a thundering herd.

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
const BACKOFF_BASE: Duration = Duration::from_secs(1);
/// Never park an account longer than this on 429s.
const BACKOFF_CAP: Duration = Duration::from_secs(240);
/// Reported wait when a half-open connection's single probe slot is in use.
const PROBE_TAKEN: Duration = Duration::from_secs(3600);

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

struct Entry {
    state: BreakerState,
    consecutive_failures: u32,
    /// Consecutive 429s; drives exponential backoff.
    rate_limit_strikes: u32,
}

impl Entry {
    fn new() -> Self {
        Self { state: BreakerState::Closed, consecutive_failures: 0, rate_limit_strikes: 0 }
    }
}

/// Failure history per connection. Share it by wrapping in `Arc`; the handle is
/// what streams and spawned tasks hold.
pub struct Breaker {
    policy: Policy,
    entries: Mutex<HashMap<Uuid, Entry>>,
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
            policy: Policy { failures: failures.max(1), open_for, probes: probes.max(1) },
            entries: Mutex::new(HashMap::new()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<Uuid, Entry>> {
        // A poisoned map only means another task panicked mid-update; the map is
        // still structurally sound, so recover instead of propagating the panic.
        self.entries.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// `None` → eligible now. `Some(wait)` → skip this candidate, earliest retry
    /// after `wait`.
    pub fn check(&self, conn: Uuid) -> Option<Duration> {
        self.check_at(conn, Instant::now())
    }

    /// Clock-injected form of [`Breaker::check`] (deterministic tests).
    pub fn check_at(&self, conn: Uuid, now: Instant) -> Option<Duration> {
        let mut map = self.lock();
        let entry = map.entry(conn).or_insert_with(Entry::new);
        // Observe the state without mutating it first: an elapsed open window is
        // *reported* as half-open, and only an actual probe claim advances it.
        let state = match entry.state {
            BreakerState::Open { until } if now >= until => BreakerState::HalfOpen { probes: 0 },
            other => other,
        };
        entry.state = state;
        match state {
            BreakerState::Closed => None,
            BreakerState::HalfOpen { probes } if probes < self.policy.probes => {
                // Claim the probe slot. Exactly one caller wins it; concurrent
                // callers see `probes == policy.probes` and are turned away.
                entry.state = BreakerState::HalfOpen { probes: probes + 1 };
                None
            }
            BreakerState::HalfOpen { .. } => Some(PROBE_TAKEN),
            BreakerState::Open { until } => Some(until.duration_since(now)),
        }
    }

    pub fn record_success(&self, conn: Uuid) {
        let mut map = self.lock();
        let entry = map.entry(conn).or_insert_with(Entry::new);
        entry.state = BreakerState::Closed;
        entry.consecutive_failures = 0;
        entry.rate_limit_strikes = 0;
    }

    /// 429 → exponential backoff (1,2,4,8,… capped 240 s, honoring `Retry-After`);
    /// 5xx → 30 s; 401/403 → 300 s; anything else counts toward tripping.
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
            entry.state = BreakerState::Open { until: now + open_for };
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
                BreakerState::Open { until } if now >= until => BreakerState::HalfOpen { probes: 0 },
                other => other,
            },
        }
    }
}
