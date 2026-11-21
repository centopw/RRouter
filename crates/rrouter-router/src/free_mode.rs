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
