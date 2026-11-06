//! The fallback engine for non-streaming attempts.
//!
//! Rules, in order:
//! 1. A candidate whose breaker is open is skipped, not attempted.
//! 2. If *nothing* is eligible but some candidate opens within
//!    [`MAX_SKIP_WAIT`], sleep at most that long and retry it once. This keeps a
//!    client from paying a 30 s breaker window for a 300 ms blip, and it is the
//!    only place this module ever sleeps — a 429/5xx mid-flight never waits.
//! 3. Retryable failures (`Error::is_retryable_upstream`) record a breaker
//!    failure and move to the next candidate.
//! 4. Non-retryable failures return immediately: retrying a 400 on another
//!    account just burns requests.
//! 5. Success closes the breaker and returns the winning candidate.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use rrouter_core::{ChatResponse, Error, Result};
use uuid::Uuid;

use crate::breaker::Breaker;
use crate::candidate::{Candidate, Plan};

/// Total time we are willing to sleep before a single retry of a skipped candidate.
pub(crate) const MAX_SKIP_WAIT: Duration = Duration::from_secs(2);
/// Default number of upstream sends per request.
pub const DEFAULT_MAX_ATTEMPTS: usize = 4;

/// One attempt's result, tagged with the candidate it was sent to.
#[derive(Debug)]
pub struct AttemptOutcome {
    pub candidate: Candidate,
    pub result: Result<ChatResponse>,
}

/// Drives [`Plan`]s against a caller-supplied transport.
pub struct Dispatcher {
    pub(crate) breaker: Arc<Breaker>,
    max_attempts: usize,
}

impl Dispatcher {
    pub fn new(breaker: Breaker) -> Self {
        Self { breaker: Arc::new(breaker), max_attempts: DEFAULT_MAX_ATTEMPTS }
    }

    /// Build from an already-shared breaker (e.g. one the planner also inspects).
    pub fn with_shared_breaker(breaker: Arc<Breaker>) -> Self {
        Self { breaker, max_attempts: DEFAULT_MAX_ATTEMPTS }
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

    pub fn breaker(&self) -> &Breaker {
        &self.breaker
    }

    pub fn max_attempts(&self) -> usize {
        self.max_attempts
    }

    /// Non-streaming fallback run. `send` performs exactly one upstream attempt.
    pub async fn run<F, Fut>(&self, plan: &Plan, send: F) -> Result<AttemptOutcome>
    where
        F: Fn(Candidate) -> Fut,
        Fut: Future<Output = Result<ChatResponse>>,
    {
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
            sends += 1;
            let result = send(candidate.clone()).await;
            // Disarm before settling so `Drop` cannot double-report.
            claim.disarm();
            match result {
                Ok(resp) => {
                    self.breaker.record_success(candidate.connection);
                    return Ok(AttemptOutcome { candidate, result: Ok(resp) });
                }
                Err(err) if !err.is_retryable_upstream() => {
                    // A non-retryable error (bad client request, translation failure) says
                    // nothing about the account: the upstream answered sanely. Releasing the
                    // probe keeps it eligible instead of counting a client 400 toward a trip.
                    self.breaker.record_success(candidate.connection);
                    return Ok(AttemptOutcome { candidate, result: Err(err) });
                }
                Err(err) => {
                    self.breaker
                        .record_failure(candidate.connection, status_of(&err), retry_after(&err));
                    last = Some(err);
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
            if self.breaker.claim(candidate.connection).is_none() {
                let claim = ProbeClaim {
                    breaker: Arc::clone(&self.breaker),
                    conn: candidate.connection,
                    armed: true,
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
    armed: bool,
}

impl ProbeClaim {
    /// Mark the claim as already settled by an explicit `record_success` /
    /// `record_failure`. Must be called before those, so a panic cannot double-report.
    pub(crate) fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for ProbeClaim {
    fn drop(&mut self) {
        if self.armed {
            self.breaker.release_probe(self.conn);
        }
    }
}

pub(crate) fn no_route(plan: &Plan, detail: &str) -> Error {
    Error::NoRoute { model: plan.model.clone(), detail: detail.to_string() }
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
