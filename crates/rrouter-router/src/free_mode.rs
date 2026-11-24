//! Free-mode rotation: the per-account request windows that make a pool of free-tier
//! accounts last longer than any single one of them.
//!
//! Free-tier limits are counted per account — and, for several providers, per egress
//! address — so the only way to spend a pool of them for longer is to *spread* requests
//! across the pool instead of draining one account at a time. `free_rpm` on the credential
//! is the operator's estimate of that account's own window; this module keeps a sliding
//! 60 s count per connection so the planner can prefer an account with room left.
//!
//! The window is a *budget, never a gate*: an account at its cap stays in the plan and
//! rotates by least-recent use, because serving from an account the provider may still
//! accept beats answering 429 from the gateway on the operator's behalf. The authoritative
//! cooldown stays where it already was — the per-`(connection, model)` 429 cooldown in
//! [`crate::breaker`] and the dispatcher's skip — so this module adds no second refusal
//! path and never needs to agree with one.
//!
//! State is shared, because the planner is: [`FreeRotation`] is a cheap handle over one
//! `Mutex`, so every request planned by the process sees the same windows and the same
//! rotation cursor.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use uuid::Uuid;

/// Width of the window an account's `free_rpm` is counted over. One minute is the unit
/// every free tier publishes, and reusing it here means the local estimate and the
/// provider's own counter age out together.
pub const WINDOW: Duration = Duration::from_secs(60);

/// Source tag stored in `free_models.source` for models discovered from a public keyless
/// catalog (OpenCode Zen, OpenRouter `:free`) that speak a dialect RRouter supports.
pub const SOURCE_PUBLIC_CATALOG: &str = "public-catalog";

/// Source tag stored in `free_models.source` for zero-priced public catalog models that
/// RRouter cannot route (for example `jev-1.13-free`, which only speaks `systemone`).
/// Stored with `free = false` so the planner never admits them, while the UI can still
/// disclose them under "Not usable here".
pub const SOURCE_PUBLIC_CATALOG_UNUSABLE: &str = "public-catalog-unusable";

/// Synthetic connection id representing an anonymous (keyless) free-tier send — currently
/// OpenCode Zen (`Authorization: Bearer public`).
///
/// Using the nil UUID keeps [`crate::Candidate`] wire-compatible across every existing
/// caller while giving the anonymous pool its own breaker/cooldown bucket. Callers that
/// persist usage or attempt rows **must** convert through [`logged_connection_id`] so this
/// marker never leaks into the database as a fake connection UUID.
pub const ANONYMOUS_CONNECTION: Uuid = Uuid::nil();

/// `true` when `connection` is the anonymous keyless free-tier marker rather than a stored
/// connection row.
pub fn is_anonymous_connection(connection: Uuid) -> bool {
    connection.is_nil()
}

/// Connection id suitable for durable `usage` / `attempts` rows (`None` for anonymous sends).
pub fn logged_connection_id(connection: Uuid) -> Option<Uuid> {
    (!connection.is_nil()).then_some(connection)
}

/// Providers whose verified-free public-catalog models can be dispatched anonymously
/// (keyless `Bearer public`) when the operator has not added an account on Providers.
pub fn supports_anonymous_free(provider: &str) -> bool {
    provider == "opencode"
}

/// `true` when `model` is one of the reserved free-mode refs (`free`, `free:<provider>`,
/// `free/<model>`, `free:<provider>/<model>`).
pub fn is_free_ref(model: &str) -> bool {
    model == "free" || model.starts_with("free:") || model.starts_with("free/")
}

/// Shared free-mode rotation state.
///
/// Cloneable by design: the handle wraps an `Arc`, so a second planner (a reload, a test)
/// configured with `Planner::with_free_rotation` continues the same windows instead of
/// starting from an empty one and hammering the first account again.
#[derive(Debug, Clone, Default)]
pub struct FreeRotation {
    inner: Arc<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    state: Mutex<State>,
}

#[derive(Debug, Default)]
struct State {
    /// Send timestamps per account, within [`WINDOW`].
    sends: HashMap<Uuid, Window>,
    /// Free-plan rotation cursor. Successive free plans start at successive models, so
    /// two requests in a row never spend the same account first.
    turn: u64,
    /// Provider-wide free-usage cooldowns keyed by `(connection, provider)`.
    ///
    /// OpenCode's `FreeUsageLimitError` applies across *every* free model for that IP or
    /// key: cooling only `(connection, model)` would let one request waste up to
    /// `max_attempts` sends against 8 different models on the same exhausted bucket.
    provider_limits: HashMap<(Uuid, String), Instant>,
}

#[derive(Debug, Default)]
struct Window {
    at: VecDeque<Instant>,
    /// The cap the window was last judged against. Kept so [`FreeRotation::pressure`] can
    /// answer a ratio without the caller re-reading the credential.
    rpm: Option<u32>,
}

impl Window {
    /// Drop timestamps that have left the window. The count is the whole state, so this
    /// is the only maintenance the map ever needs.
    fn prune(&mut self, now: Instant) {
        while let Some(oldest) = self.at.front() {
            if now.duration_since(*oldest) < WINDOW {
                break;
            }
            self.at.pop_front();
        }
    }

    fn pressure(&self) -> f64 {
        match self.rpm {
            Some(rpm) if rpm > 0 => (self.at.len() as f64 / f64::from(rpm)).clamp(0.0, 1.0),
            // No local cap: nothing is known to be spent, so nothing is pressured. The
            // provider's own 429 is the feedback in that case, not a guess.
            _ => 0.0,
        }
    }
}

impl FreeRotation {
    pub fn new() -> Self {
        Self::default()
    }

    /// True when `connection` still has room under `rpm` in the current window.
    ///
    /// `None` (or `0`) means the operator set no local cap: the account is always
    /// admissible, because inventing a cap would refuse a request the provider would
    /// have served. Also records the cap, which is what [`Self::pressure`] reports against.
    pub fn admit(&self, connection: Uuid, rpm: Option<u32>) -> bool {
        let now = Instant::now();
        let mut state = self.lock();
        let window = state.sends.entry(connection).or_default();
        window.prune(now);
        window.rpm = rpm;
        match rpm {
            Some(rpm) if rpm > 0 => window.at.len() < rpm as usize,
            _ => true,
        }
    }

    /// Record one send to `connection`.
    ///
    /// Called once per free plan, for the candidate the dispatcher will try first: the
    /// planner sees requests, not attempts, and an attempt that never left (a breaker
    /// skip, a fallback) is not this module's to count — the provider's 429 is.
    pub fn note(&self, connection: Uuid) {
        let now = Instant::now();
        let mut state = self.lock();
        let window = state.sends.entry(connection).or_default();
        window.prune(now);
        window.at.push_back(now);
    }

    /// How full `connection`'s window is, `0.0..=1.0`, against the cap the last
    /// [`Self::admit`] judged it with. `0.0` when no cap is known (never admitted, or the
    /// account has no local cap).
    pub fn pressure(&self, connection: Uuid) -> f64 {
        let now = Instant::now();
        let mut state = self.lock();
        let Some(window) = state.sends.get_mut(&connection) else {
            return 0.0;
        };
        window.prune(now);
        window.pressure()
    }

    /// When `connection` was last sent to, or `None` when it never was.
    ///
    /// The planner breaks ties with this: an account that has waited longest goes first,
    /// which is what keeps a capped pool rotating instead of retrying the same account.
    pub fn last_used(&self, connection: Uuid) -> Option<Instant> {
        let state = self.lock();
        state.sends.get(&connection)?.at.back().copied()
    }

    /// Take the next rotation slot. One call per free plan.
    pub fn turn(&self) -> u64 {
        let mut state = self.lock();
        let turn = state.turn;
        state.turn = state.turn.wrapping_add(1);
        turn
    }

    /// Record a provider-wide free-tier quota cooldown (for example OpenCode's
    /// `FreeUsageLimitError`) for `(connection, provider)`.
    pub fn record_provider_limit(&self, connection: Uuid, provider: &str, wait: Duration) {
        self.record_provider_limit_at(connection, provider, wait, Instant::now());
    }

    pub fn record_provider_limit_at(
        &self,
        connection: Uuid,
        provider: &str,
        wait: Duration,
        now: Instant,
    ) {
        let mut state = self.lock();
        let until = now + wait.max(Duration::from_secs(1));
        state
            .provider_limits
            .insert((connection, provider.to_string()), until);
    }

    /// Remaining provider-wide free-tier cooldown for `(connection, provider)`, if any.
    pub fn provider_limit_wait(&self, connection: Uuid, provider: &str) -> Option<Duration> {
        self.provider_limit_wait_at(connection, provider, Instant::now())
    }

    pub fn provider_limit_wait_at(
        &self,
        connection: Uuid,
        provider: &str,
        now: Instant,
    ) -> Option<Duration> {
        let mut state = self.lock();
        let key = (connection, provider.to_string());
        let until = *state.provider_limits.get(&key)?;
        if now >= until {
            state.provider_limits.remove(&key);
            return None;
        }
        Some(until.duration_since(now))
    }

    /// Clear any provider-wide free-tier cooldown (e.g. after a manual probe succeeds).
    pub fn clear_provider_limit(&self, connection: Uuid, provider: &str) {
        let mut state = self.lock();
        state
            .provider_limits
            .remove(&(connection, provider.to_string()));
    }

    /// A panic elsewhere must not disable free routing for the process lifetime, so a
    /// poisoned lock is recovered rather than propagated: the windows are a best-effort
    /// count, not an invariant.
    fn lock(&self) -> MutexGuard<'_, State> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anonymous_marker_never_leaks_a_uuid() {
        assert!(is_anonymous_connection(ANONYMOUS_CONNECTION));
        assert_eq!(logged_connection_id(ANONYMOUS_CONNECTION), None);
        let real = Uuid::new_v4();
        assert!(!is_anonymous_connection(real));
        assert_eq!(logged_connection_id(real), Some(real));
        assert!(supports_anonymous_free("opencode"));
        assert!(!supports_anonymous_free("opencode-go"));
        assert!(!supports_anonymous_free("openrouter"));
    }

    #[test]
    fn provider_limit_expires_and_can_be_cleared() {
        let rot = FreeRotation::new();
        let now = Instant::now();
        rot.record_provider_limit_at(
            ANONYMOUS_CONNECTION,
            "opencode",
            Duration::from_secs(60),
            now,
        );
        assert_eq!(
            rot.provider_limit_wait_at(
                ANONYMOUS_CONNECTION,
                "opencode",
                now + Duration::from_secs(10)
            ),
            Some(Duration::from_secs(50))
        );
        assert_eq!(
            rot.provider_limit_wait_at(
                ANONYMOUS_CONNECTION,
                "opencode",
                now + Duration::from_secs(60)
            ),
            None
        );

        rot.record_provider_limit_at(
            ANONYMOUS_CONNECTION,
            "opencode",
            Duration::from_secs(60),
            now,
        );
        rot.clear_provider_limit(ANONYMOUS_CONNECTION, "opencode");
        assert_eq!(
            rot.provider_limit_wait_at(ANONYMOUS_CONNECTION, "opencode", now),
            None
        );
    }
}
