//! The fallback engine for non-streaming attempts.
//!
//! Rules, in order:
//! 1. A candidate whose breaker is open is skipped, not attempted.
//! 2. If *nothing* is eligible but some candidate opens within
//!    [`MAX_SKIP_WAIT`], sleep at most that long and retry it once. This keeps a
//!    client from paying a 30 s breaker window for a 300 ms blip, and it is the
//!    only ordering wait this module performs; a 429/5xx mid-flight never waits
//!    here beyond the retry backoff in rule 3.
//! 3. A retryable failure is retried on the **same** candidate up to
//!    [`RetryPolicy::attempts`] extra times, sleeping an exponentially backed-off,
//!    jittered `min(base * 2^n, max)` between sends (capped by an upstream
//!    `Retry-After` and by the time the caller gave the headers phase). A 503 is
//!    usually a blip; falling back on the first one spends a healthy account for
//!    nothing. [`crate::Dispatcher::run_streaming`] retries through the same
//!    decision ([`retry_wait`]), so a streamed request pays the same price.
//! 4. Only once a candidate's retries are exhausted does it record **one** breaker
//!    failure and the next candidate get tried, so a flaky retry cannot trip an
//!    account faster than a hard failure.
//! 5. Non-retryable failures return immediately: retrying a 400 on another
//!    account just burns requests.
//! 6. Success closes the breaker and returns the winning candidate.

use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rrouter_core::{Error, Result};
use uuid::Uuid;

use crate::breaker::{Breaker, ProbeTicket};
use crate::candidate::{Candidate, Plan};

/// Total time we are willing to sleep before a single retry of a skipped candidate.
pub(crate) const MAX_SKIP_WAIT: Duration = Duration::from_secs(2);
/// Default number of upstream sends per request.
pub const DEFAULT_MAX_ATTEMPTS: usize = 4;
/// Extra tries per candidate after the first, when no policy is supplied.
pub const DEFAULT_RETRY_ATTEMPTS: usize = 1;
/// The most extra tries per candidate a policy may ask for.
pub const MAX_RETRY_ATTEMPTS: usize = 5;
/// First backoff when no policy is supplied.
pub const DEFAULT_RETRY_BASE: Duration = Duration::from_millis(200);
/// Backoff ceiling when no policy is supplied.
pub const DEFAULT_RETRY_MAX: Duration = Duration::from_millis(5_000);

/// How often one candidate is retried, and how long to wait between tries.
///
/// The total number of sends still belongs to [`Dispatcher::max_attempts`]: retries
/// spend that budget like any other send, so `attempts` can never turn one request
/// into an unbounded stream of upstream calls.
#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    /// Extra tries per candidate after the first, clamped to `0..=`[`MAX_RETRY_ATTEMPTS`].
    pub attempts: usize,
    /// First backoff. Each further retry of the same candidate doubles it.
    pub base: Duration,
    /// Backoff ceiling; also the longest an upstream `Retry-After` may make us sleep.
    pub max: Duration,
    /// Time the caller granted the headers phase. A retry whose sleep would run past
    /// it is abandoned instead of slept; `None` means the caller enforces its own
    /// deadline (tests, admin probes).
    pub budget: Option<Duration>,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            attempts: DEFAULT_RETRY_ATTEMPTS,
            base: DEFAULT_RETRY_BASE,
            max: DEFAULT_RETRY_MAX,
            budget: None,
        }
    }
}

impl RetryPolicy {
    /// Sleep before retry `retry_index` (0-based) of one candidate.
    ///
    /// `min(base * 2^retry_index, max)` plus ±20% jitter, capped by `max` and — when the
    /// upstream sent one — by its `Retry-After`, which is itself capped by `max` so a
    /// `Retry-After: 60` cannot stall a request whose ceiling is milliseconds.
    ///
    /// `fraction` is the jitter source in `[0, 1]`. Production passes
    /// [`clock_fraction`]; tests pass a fixed value, which keeps the curve exact.
    pub fn delay(&self, retry_index: usize, retry_after: Option<Duration>, fraction: f64) -> Duration {
        // `2^n` saturates well before it can overflow the duration multiply.
        let doublings = 1u32 << retry_index.min(16);
        let capped = self.base.saturating_mul(doublings).min(self.max);
        // 800..=1200 thousandths: -20% .. +20%, computed in integers so a fixed
        // fraction produces an exact duration.
        let thousandths = 800 + (400.0 * fraction.clamp(0.0, 1.0)).round() as u64;
        let jittered_nanos = capped.as_nanos() * u128::from(thousandths) / 1_000;
        let jittered =
            Duration::from_nanos(u64::try_from(jittered_nanos).unwrap_or(u64::MAX)).min(self.max);
        match retry_after {
            Some(wait) => jittered.min(wait).min(self.max),
            None => jittered,
        }
    }

    /// Absolute deadline of the caller's headers budget, when one was granted.
    pub(crate) fn deadline(&self) -> Option<Instant> {
        self.budget.and_then(|budget| Instant::now().checked_add(budget))
    }
}

/// Jitter fraction in `[0, 1)` from the system clock. No `rand` dependency, and the
/// nanosecond field is enough: the point is that clients failing at the same instant do
/// not retry in lockstep.
fn clock_fraction() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| f64::from(since.subsec_nanos() % 10_000) / 10_000.0)
        .unwrap_or(0.5)
}

/// One attempt's result, tagged with the candidate it was sent to.
#[derive(Debug)]
pub struct AttemptOutcome<T> {
    pub candidate: Candidate,
    pub result: Result<T>,
}

/// Drives [`Plan`]s against a caller-supplied transport.
pub struct Dispatcher {
    pub(crate) breaker: Arc<Breaker>,
    max_attempts: usize,
    /// Shared with [`crate::Dispatcher::run_streaming`], which retries on the same curve.
    pub(crate) retry: RetryPolicy,
}

impl Dispatcher {
    pub fn new(breaker: Breaker) -> Self {
        Self {
            breaker: Arc::new(breaker),
            max_attempts: DEFAULT_MAX_ATTEMPTS,
            retry: RetryPolicy::default(),
        }
    }

    /// Build from an already-shared breaker (e.g. one the planner also inspects).
    pub fn with_shared_breaker(breaker: Arc<Breaker>) -> Self {
        Self {
            breaker,
            max_attempts: DEFAULT_MAX_ATTEMPTS,
            retry: RetryPolicy::default(),
        }
    }

    /// The breaker itself, for callers that want to inspect state.
    pub fn breaker_arc(&self) -> Arc<Breaker> {
        Arc::clone(&self.breaker)
    }

    /// Override the send cap (default [`DEFAULT_MAX_ATTEMPTS`]). Zero is clamped to 1.
    pub fn with_max_attempts(mut self, max_attempts: usize) -> Self {
        self.max_attempts = max_attempts.max(1);
        self
    }

    /// Override the per-candidate retry policy (default [`RetryPolicy::default`]).
    /// `attempts` is clamped to [`MAX_RETRY_ATTEMPTS`].
    pub fn with_retry_policy(mut self, retry: RetryPolicy) -> Self {
        self.retry = RetryPolicy { attempts: retry.attempts.min(MAX_RETRY_ATTEMPTS), ..retry };
        self
    }

    pub fn breaker(&self) -> &Breaker {
        &self.breaker
    }

    pub fn max_attempts(&self) -> usize {
        self.max_attempts
    }

    /// Non-streaming fallback run. `send` performs exactly one upstream attempt.
    ///
    /// A retryable failure is sent to the **same** candidate again after
    /// [`RetryPolicy::delay`], up to its `attempts` extra tries and the
    /// [`Dispatcher::max_attempts`] send cap. Only an exhausted candidate counts as a
    /// breaker failure, so retries do not trip an account faster than a hard failure.
    pub async fn run<T, F, Fut>(&self, plan: &Plan, send: F) -> Result<AttemptOutcome<T>>
    where
        F: Fn(Candidate) -> Fut,
        Fut: Future<Output = Result<T>>,
    {
        // The budget covers ordering too: the skipped-candidate wait below is part of
        // the headers phase the caller timed.
        let deadline = self.retry.deadline();
        let mut order = self.attempt_order(plan).await;
        if order.is_empty() {
            return Err(no_route(plan, "every candidate is blocked by its circuit breaker"));
        }

        let mut last: Option<Error> = None;
        let mut sends = 0usize;
        while sends < self.max_attempts {
            let Some((candidate, mut claim)) = self.next_claim(plan, &mut order).await else {
                break;
            };
            let mut retries = 0usize;
            loop {
                sends += 1;
                let result = send(candidate.clone()).await;
                match result {
                    Ok(resp) => {
                        // Disarm before settling so `Drop` cannot double-report.
                        claim.disarm();
                        self.breaker.record_success(candidate.connection);
                        return Ok(AttemptOutcome { candidate, result: Ok(resp) });
                    }
                    Err(err) if !err.is_retryable_upstream() => {
                        // A non-retryable error (bad client request, translation failure) says
                        // nothing about the account: the upstream answered sanely. Releasing the
                        // probe keeps it eligible instead of counting a client 400 toward a trip.
                        claim.disarm();
                        self.breaker.record_success(candidate.connection);
                        return Ok(AttemptOutcome { candidate, result: Err(err) });
                    }
                    Err(err) => {
                        if let Some(wait) =
                            retry_wait(&self.retry, &err, retries, sends, self.max_attempts, deadline)
                        {
                            tracing::debug!(
                                connection = %candidate.connection,
                                retry = retries + 1,
                                wait_ms = wait.as_millis() as u64,
                                "retrying candidate after transient upstream failure"
                            );
                            tokio::time::sleep(wait).await;
                            retries += 1;
                            continue;
                        }
                        // Retries exhausted (or no time left): one breaker failure for
                        // the candidate, then the next one.
                        claim.disarm();
                        self.breaker.record_failure(
                            candidate.connection,
                            status_of(&err),
                            retry_after(&err),
                        );
                        last = Some(err);
                        break;
                    }
                }
            }
        }

        // The last upstream error is the most informative thing to return: the
        // cap and the skip rules only decide *when* to stop, not what failed.
        Err(last.unwrap_or_else(|| no_route(plan, "no attempt could be sent")))
    }

    /// Pop the next candidate whose send is permitted *now*, claiming its
    /// half-open probe slot in the same step so no slot is taken and dropped.
    ///
    /// The returned [`ProbeClaim`] settles the slot on drop, so a cancelled request
    /// (client disconnect) can never park the account.
    pub(crate) async fn next_claim(
        &self,
        plan: &Plan,
        order: &mut std::collections::VecDeque<usize>,
    ) -> Option<(Candidate, ProbeClaim)> {
        while let Some(idx) = order.pop_front() {
            let candidate = plan.candidates[idx].clone();
            if let Ok(ticket) = self.breaker.claim(candidate.connection) {
                let claim = ProbeClaim {
                    breaker: Arc::clone(&self.breaker),
                    conn: candidate.connection,
                    // Only a guard that actually took a half-open slot may give one
                    // back, and only within the episode that granted it.
                    ticket,
                };
                return Some((candidate, claim));
            }
        }
        None
    }

    /// Candidates in send order, honoring the bounded pre-sleep rule.
    ///
    /// This is a read-only ranking: probe slots are claimed in [`Dispatcher::next_claim`],
    /// immediately before the attempt, so an unsent candidate never holds one.
    pub(crate) async fn attempt_order(&self, plan: &Plan) -> std::collections::VecDeque<usize> {
        let mut eligible = std::collections::VecDeque::with_capacity(plan.candidates.len());
        let mut skipped: Vec<(usize, Duration)> = Vec::new();
        for (idx, c) in plan.candidates.iter().enumerate() {
            match self.breaker.check(c.connection) {
                None => eligible.push_back(idx),
                Some(wait) if wait <= MAX_SKIP_WAIT => skipped.push((idx, wait)),
                Some(_) => {}
            }
        }
        if eligible.is_empty() && !skipped.is_empty() {
            // Nothing eligible: wait for the earliest skipped candidate, once,
            // bounded by the total sleep budget. Re-check afterwards — another
            // request may have started its probe during the sleep.
            skipped.sort_by_key(|(_, wait)| *wait);
            let (idx, wait) = skipped[0];
            tokio::time::sleep(wait.min(MAX_SKIP_WAIT)).await;
            if self.breaker.check(plan.candidates[idx].connection).is_none() {
                eligible.push_back(idx);
            }
        }
        eligible
    }
}

/// RAII settlement for one claimed attempt.
///
/// `Dispatcher` claims a half-open probe slot immediately before sending. Every
/// documented outcome settles it explicitly, but a **cancelled** request — the
/// axum handler future dropped on client disconnect — never reaches that code.
/// Dropping this guard releases the slot instead of parking the account for
/// `PROBE_TAKEN`, and is a no-op when the slot was already settled.
pub(crate) struct ProbeClaim {
    breaker: Arc<Breaker>,
    conn: Uuid,
    ticket: ProbeTicket,
}

impl ProbeClaim {
    /// Mark the claim as already settled by an explicit `record_success` /
    /// `record_failure`. Must be called before those, so a panic cannot double-report.
    pub(crate) fn disarm(&mut self) {
        self.ticket = ProbeTicket::None;
    }
}

impl Drop for ProbeClaim {
    fn drop(&mut self) {
        self.breaker.release_probe(self.conn, self.ticket);
    }
}

pub(crate) fn no_route(plan: &Plan, detail: &str) -> Error {
    Error::NoRoute { model: plan.model.clone(), detail: detail.to_string() }
}


/// May a retry sleep this long without running past the caller's deadline?
///
/// No deadline means the caller enforces its own (tests and probes pass `None`).
fn within_budget(deadline: Option<Instant>, wait: Duration) -> bool {
    let Some(deadline) = deadline else { return true };
    Instant::now().checked_add(wait).is_some_and(|at| at <= deadline)
}

/// The one retry decision both [`Dispatcher::run`] and
/// [`Dispatcher::run_streaming`] make: should `err` be retried on the **same**
/// candidate, and if so after how long?
///
/// `None` when the candidate has no try left (`retries` reached
/// [`RetryPolicy::attempts`]), when the request has spent its sends
/// (`max_attempts`), or when the [`RetryPolicy::delay`] sleep would overrun the
/// caller's headers budget. Keeping the arithmetic in `delay` and the gating here
/// means a streamed request backs off exactly like a JSON one.
pub(crate) fn retry_wait(
    retry: &RetryPolicy,
    err: &Error,
    retries: usize,
    sends: usize,
    max_attempts: usize,
    deadline: Option<Instant>,
) -> Option<Duration> {
    if retries >= retry.attempts || sends >= max_attempts {
        return None;
    }
    let wait = retry.delay(retries, retry_after(err), clock_fraction());
    within_budget(deadline, wait).then_some(wait)
}

/// Upstream status for breaker bookkeeping; transport/timeout failures have no
/// HTTP status and are recorded as 0 (generic trip).
pub(crate) fn status_of(err: &Error) -> u16 {
    match err {
        Error::Upstream { status, .. } => *status,
        Error::Timeout(_) | Error::Transport(_) => 0,
        _ => err.status(),
    }
}

/// `Retry-After` from an upstream error payload, when the upstream sent one.
pub(crate) fn retry_after(err: &Error) -> Option<Duration> {
    let Error::Upstream { payload, .. } = err else { return None };
    let value = payload.as_ref()?.get("retry_after")?;
    if let Some(secs) = value.as_f64() {
        return Duration::try_from_secs_f64(secs.max(0.0)).ok();
    }
    value.as_str().and_then(|s| s.trim().parse::<u64>().ok()).map(Duration::from_secs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// A guard created for a closed-breaker attempt (`armed = false`) must not hand
    /// back a probe slot. The race: request A starts while closed (takes nothing),
    /// another request trips the account and holds a probe, then A is cancelled.
    /// If A's guard released anyway it would free B's probe and admit two at once.
    #[test]
    fn closed_breaker_claim_arms_no_release() {
        use std::time::Instant;
        let breaker = Arc::new(Breaker::with_policy(1, Duration::from_secs(30), 1));
        let conn = Uuid::new_v4();

        // A starts while closed: no slot taken.
        assert_eq!(breaker.claim(conn), Ok(ProbeTicket::None));
        let a_guard = ProbeClaim { breaker: Arc::clone(&breaker), conn, ticket: ProbeTicket::None };

        // Meanwhile B trips the account and takes the single half-open probe.
        breaker.record_failure(conn, 500, None);
        let later = Instant::now() + Duration::from_secs(31);
        assert!(matches!(breaker.claim_at(conn, later), Ok(ProbeTicket::Probe { .. })), "B holds the only probe");

        // A is now cancelled.
        drop(a_guard);

        // B's probe must still be held: exactly one probe is allowed.
        assert!(
            breaker.claim_at(conn, later).is_err(),
            "a cancelled closed-state attempt must not free another request's probe"
        );
    }

    /// A guard that *did* take the slot returns it when cancelled.
    #[test]
    fn probe_claim_guard_releases_only_its_own_slot() {
        let breaker = Arc::new(Breaker::with_policy(1, Duration::ZERO, 1));
        let conn = Uuid::new_v4();
        breaker.record_failure(conn, 500, None);
        assert!(matches!(breaker.claim(conn), Ok(ProbeTicket::Probe { .. })));
        drop(ProbeClaim { breaker: Arc::clone(&breaker), conn, ticket: ProbeTicket::Probe { epoch: 1 } });
        assert!(matches!(breaker.claim(conn), Ok(ProbeTicket::Probe { .. })), "the slot was returned");
    }

    /// A stale guard must not free a probe belonging to a *newer* half-open episode.
    /// Sequence: probe episode N is taken and held; the account trips again and a new
    /// episode N+1 is entered; the stale N guard is dropped. N+1's slot must survive.
    #[test]
    fn stale_guard_cannot_release_a_newer_episode_probe() {
        use std::time::Instant;
        let breaker = Arc::new(Breaker::with_policy(1, Duration::ZERO, 1));
        let conn = Uuid::new_v4();

        // Episode N: trip, then claim the probe past the window.
        breaker.record_failure(conn, 500, None);
        let later = Instant::now() + Duration::from_secs(31);
        let Ok(old_ticket) = breaker.claim_at(conn, later) else { panic!("probe N must be taken") };

        // The account trips again, opening a new window, which then elapses.
        breaker.record_failure(conn, 500, None);
        let later2 = later + Duration::from_secs(31);
        assert!(
            matches!(breaker.claim_at(conn, later2), Ok(ProbeTicket::Probe { .. })),
            "probe N+1 taken"
        );

        // The stale guard from episode N is finally dropped: it must not free N+1.
        breaker.release_probe(conn, old_ticket);
        assert!(
            breaker.claim_at(conn, later2).is_err(),
            "a stale guard must not release a newer episode's probe"
        );
    }
}
