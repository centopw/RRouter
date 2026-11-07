//! Shared application state handed to every handler.
//!
//! It also owns the virtual-key governance path: resolving the presented key, the
//! allowlist, and the per-minute caps. Everything the hot path needs to refuse a
//! request lives here so [`crate::proxy`] only has to call it.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::http::HeaderMap;
use rrouter_core::{Dialect, Error, Result};
use rrouter_router::Catalog;
use rrouter_store::{ApiKey, Guardrails, KeyScope, Store};
use uuid::Uuid;

use crate::config::Config;

/// Width of the sliding window behind `rpm` and `tpm`.
const MINUTE: Duration = Duration::from_secs(60);

/// How long the first retention sweep waits after startup: long enough for the bind and
/// catalog refresh to finish, so a large first delete never delays `serve`.
const FIRST_SWEEP_DELAY: Duration = Duration::from_secs(10);

/// Retention period: history is measured in days, so one pass a day is the useful rate.
const SWEEP_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

const SECS_PER_DAY: i64 = 86_400;

pub struct AppState {
    pub store: Arc<Store>,
    pub catalog: Arc<Catalog>,
    pub refresher: Arc<rrouter_auth::Refresher>,
    pub planner: rrouter_router::Planner,
    pub dispatcher: Arc<rrouter_router::Dispatcher>,
    pub http: reqwest::Client,
    pub config: Config,
    pub started: Instant,
    /// Live OAuth sign-in sessions (one-time browser/device logins).
    pub oauth: rrouter_auth::OAuthFlows,
    /// Per-key request/token windows for the caps that cannot be answered from SQL.
    pub counters: KeyCounters,
    /// Caps concurrent `/v1/*` proxy requests; `None` when `RROUTER_MAX_CONCURRENCY`
    /// is 0. One permit spans a whole request, streamed body included.
    pub concurrency: Option<Arc<tokio::sync::Semaphore>>,
}

/// One `/v1/*` concurrency slot handed out by [`AppState::try_slot`].
pub enum Slot {
    /// The request may proceed. The permit (when a cap is configured) must stay alive
    /// until the response is built — or, for a stream, until the body ends or is
    /// dropped; dropping it is what returns the slot.
    Granted(Option<tokio::sync::OwnedSemaphorePermit>),
    /// Every permit is taken. Refusing at once beats queueing a request the client will
    /// give up on, and the caller answers `503 overloaded`.
    Full,
}

/// The fallback engine, configured from the environment: the total send cap, the
/// per-provider retry curve, and the headers-phase budget those retries may spend.
///
/// A 503 or reset is usually transient, so one retry on the same account is cheaper
/// than spending a fallback; the shared request timeout is what bounds the whole thing.
pub fn dispatcher(config: &Config) -> rrouter_router::Dispatcher {
    rrouter_router::Dispatcher::new(rrouter_router::Breaker::new())
        .with_max_attempts(config.max_attempts)
        .with_retry_policy(rrouter_router::dispatch::RetryPolicy {
            attempts: config.retry_attempts,
            base: Duration::from_millis(config.retry_base_ms),
            max: Duration::from_millis(config.retry_max_ms),
            budget: Some(Duration::from_secs(config.request_timeout_secs)),
        })
}

/// The semaphore for `RROUTER_MAX_CONCURRENCY` (`0` = none).
///
/// Any cap above tokio's permit ceiling is effectively unlimited, but constructing a
/// semaphore with it would panic at startup — clamp instead.
pub fn concurrency_for(max_concurrency: usize) -> Option<Arc<tokio::sync::Semaphore>> {
    (max_concurrency > 0).then(|| {
        Arc::new(tokio::sync::Semaphore::new(
            max_concurrency.min(tokio::sync::Semaphore::MAX_PERMITS),
        ))
    })
}

impl AppState {
    /// Start the daily usage-retention sweep.
    ///
    /// The task owns an `Arc<Self>` for the life of the process; it waits
    /// [`FIRST_SWEEP_DELAY`] before the first pass so the bind is never delayed, then
    /// runs every [`SWEEP_INTERVAL`]. No join handle is kept: no sweep can stop the
    /// server, and the task ends with the process. Exercised by the live binary rather
    /// than a unit test — a 24 h timer is not worth faking.
    pub fn spawn_retention(self: &Arc<Self>) {
        let state = Arc::clone(self);
        tokio::spawn(async move {
            tokio::time::sleep(FIRST_SWEEP_DELAY).await;
            loop {
                state.retention_sweep().await;
                tokio::time::sleep(SWEEP_INTERVAL).await;
            }
        });
    }

    /// One sweep: drop history past the retention cutoff, always drop expired cache
    /// rows, then refresh planner statistics. A failed phase is logged and the next
    /// pass retries; nothing here is visible to a client.
    async fn retention_sweep(&self) {
        let now = crate::now_unix();
        let days = i64::try_from(self.config.usage_retention_days).unwrap_or(i64::MAX);
        let (deleted_usage, deleted_attempts) = if days == 0 {
            (0, 0)
        } else {
            let cutoff = now.saturating_sub(days.saturating_mul(SECS_PER_DAY));
            match self.store.prune_usage(cutoff).await {
                Ok(report) => (report.usage, report.attempts),
                Err(e) => {
                    tracing::warn!(error = %e, "usage retention delete failed");
                    (0, 0)
                }
            }
        };
        let deleted_cache = match self.store.prune_expired_cache(now).await {
            Ok(deleted) => deleted,
            Err(e) => {
                tracing::warn!(error = %e, "expired cache delete failed");
                0
            }
        };
        if let Err(e) = self.store.optimize().await {
            tracing::warn!(error = %e, "pragma optimize failed");
        }
        tracing::info!(deleted_usage, deleted_attempts, deleted_cache, "retention sweep");
    }

    pub fn request_timeout(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.config.request_timeout_secs)
    }

    pub fn uptime_secs(&self) -> u64 {
        self.started.elapsed().as_secs()
    }

    /// Take one `/v1/*` concurrency slot.
    ///
    /// Nothing is queued: with every permit in flight the caller answers `503`, which is
    /// the only backpressure a streaming gateway can apply without lying to the client.
    pub fn try_slot(&self) -> Slot {
        let Some(semaphore) = &self.concurrency else {
            return Slot::Granted(None);
        };
        match Arc::clone(semaphore).try_acquire_owned() {
            Ok(permit) => Slot::Granted(Some(permit)),
            Err(_) => Slot::Full,
        }
    }

    /// Resolve the presented credential and validate it.
    ///
    /// - a known key is returned even when it is disabled or expired, because that is a
    ///   `403` governance decision (`api_key_inactive` / `api_key_expired`), not a `401`
    ///   — a client that has the right secret should never be told it is missing;
    /// - an unknown or missing key is anonymous when `require_api_key` is off, so dev
    ///   clients that send a placeholder token keep working;
    /// - an unknown or missing key is a `401` when keys are required;
    /// - an `admin`-scoped key is refused with `403 key_scope`: it administers the
    ///   gateway, it does not spend tokens, so it has no business on `/v1/*`.
    ///
    /// Scope is checked after activity and expiry, so a disabled or expired admin key
    /// still gets the more specific refusal.
    pub async fn authenticate(&self, headers: &HeaderMap) -> Result<Caller> {
        let presented = crate::auth_header(headers);
        let key = match &presented {
            Some(token) => self.store.verify_api_key(token).await?,
            None => None,
        };
        let Some(key) = key else {
            if !self.config.require_api_key {
                return Ok(Caller::Anonymous);
            }
            return Err(Error::Unauthorized(
                if presented.is_some() {
                    "invalid API key"
                } else {
                    "missing API key"
                }
                .to_string(),
            ));
        };
        if !key.is_active {
            return Err(Error::Forbidden {
                code: "api_key_inactive",
                message: format!("api key {} is disabled", key.id),
            });
        }
        if let Some(expires_at) = key.expires_at {
            if crate::now_unix() >= expires_at {
                return Err(Error::Forbidden {
                    code: "api_key_expired",
                    message: format!("api key {} expired at {expires_at}", key.id),
                });
            }
        }
        if key.scope == KeyScope::Admin {
            return Err(Error::Forbidden {
                code: "key_scope",
                message: "admin keys cannot call /v1".to_string(),
            });
        }
        Ok(Caller::Key(key))
    }

    /// The key `presented` names, when it is usable right now: it exists, is active and
    /// has not expired. `None` covers both "no such key" and "not a live credential",
    /// because the admin gate has nothing different to say about them.
    ///
    /// Scope is deliberately not filtered here: the gate needs an inference key to
    /// answer `403 admin_required` rather than plain `401`.
    pub async fn live_api_key(&self, presented: &str) -> Result<Option<ApiKey>> {
        let Some(key) = self.store.verify_api_key(presented).await? else {
            return Ok(None);
        };
        Ok(key.is_live(crate::now_unix()).then_some(key))
    }

    /// Apply this key's model mapping to the client's model: one hop, before alias and
    /// combo resolution. Only a key can carry a mapping, so anonymous traffic and
    /// unmapped models pass through unchanged, and a key with no mapping keeps exactly
    /// the pre-phase-4 behaviour.
    ///
    /// Runs on the request path, so it is one indexed row rather than the whole map.
    pub async fn mapped_model(&self, key: Option<Uuid>, model: &str) -> Result<String> {
        let Some(id) = key else {
            return Ok(model.to_string());
        };
        Ok(self
            .store
            .key_model_target(id, model)
            .await?
            .unwrap_or_else(|| model.to_string()))
    }

    /// Allowlist and per-key caps for an authenticated caller. Must run after the
    /// client's model is known and before any dispatch.
    ///
    /// Caps are checked in the order the contract names them: the allowlist (403), the
    /// durable budget (429), then the in-memory per-minute windows (429). Nothing is
    /// counted for a request that is refused.
    pub async fn admit(&self, caller: &Caller, model: &str) -> Result<()> {
        let Caller::Key(key) = caller else {
            return Ok(());
        };
        if let Some(allowed) = &key.allowed_models {
            // The allowlist is written against resolved refs; compare after the same one
            // alias hop the planner performs, and only for keys that have a list.
            let resolved = self.resolve_alias(model).await?;
            if !model_allowed(allowed, &resolved) {
                return Err(Error::Forbidden {
                    code: "model_not_allowed",
                    message: format!("api key {} is not allowed to use model {model}", key.id),
                });
            }
        }
        // Both budgets are measured over the key's own window; outside-window spend
        // does not count, so a daily/weekly/monthly key renews at the boundary.
        let since = rrouter_store::window_start(key.budget_window, crate::now_unix());
        if key.budget_tokens > 0 {
            let spent = self.store.api_key_spend(key.id, since).await?;
            if spent >= key.budget_tokens {
                return Err(Error::Limited {
                    code: "budget_exhausted",
                    message: format!(
                        "api key {} has spent {spent} of its {} token budget",
                        key.id, key.budget_tokens
                    ),
                    // A lifetime budget needs an operator, not patience.
                    retry_after_secs: 3600,
                });
            }
        }
        if key.budget_micros > 0 {
            // Same durable source as the token budget: the summed cost of this key's
            // usage rows inside the window, so a restart never refunds spend.
            let spent = self.store.api_key_cost_spend(key.id, since).await?;
            if spent >= key.budget_micros {
                return Err(Error::Limited {
                    code: "budget_exhausted",
                    message: {
                        // Sub-cent amounts need six decimals or a real spend reads as
                        // "$0.0000" next to a "$0.0000" budget and looks like a bug.
                        let money = |micros: u64| {
                            let usd = micros_to_usd(micros);
                            if usd < 0.01 {
                                format!("{usd:.6}")
                            } else {
                                format!("{usd:.4}")
                            }
                        };
                        format!(
                            "api key {} has spent ${} of its ${} budget",
                            key.id,
                            money(spent),
                            money(key.budget_micros)
                        )
                    },
                    retry_after_secs: 3600,
                });
            }
        }
        match self.counters.try_admit(key.id, key.rpm, key.tpm) {
            Ok(()) => Ok(()),
            Err(Cap::Rpm { limit }) => Err(Error::Limited {
                code: "rate_limited",
                message: format!("api key {} is limited to {limit} requests per minute", key.id),
                retry_after_secs: 1,
            }),
            Err(Cap::Tpm { limit, used }) => Err(Error::Limited {
                code: "rate_limited",
                message: format!(
                    "api key {} has used {used} of {limit} tokens per minute",
                    key.id
                ),
                retry_after_secs: 1,
            }),
        }
    }

    /// Attribute `tokens` to a key's sliding window (TPM is measured after the fact:
    /// tokens are only known once a response reports its usage).
    pub fn record_tokens(&self, key: Option<Uuid>, tokens: u64) {
        if let Some(id) = key {
            self.counters.record_tokens(id, tokens);
        }
    }

    /// One alias hop, exactly as the planner resolves it before routing.
    async fn resolve_alias(&self, model: &str) -> Result<String> {
        let aliases = self.store.list_aliases().await?;
        Ok(aliases
            .iter()
            .find(|a| a.alias == model)
            .map(|a| a.target.clone())
            .unwrap_or_else(|| model.to_string()))
    }
}

/// Who pays for this request.
#[derive(Debug, Clone)]
pub enum Caller {
    /// No credential presented, or an unknown one while keys are optional.
    Anonymous,
    Key(ApiKey),
}

impl Caller {
    /// The key to attribute usage to; `None` for anonymous traffic.
    pub fn key_id(&self) -> Option<Uuid> {
        match self {
            Self::Anonymous => None,
            Self::Key(key) => Some(key.id),
        }
    }

    /// This caller's guardrail policy, if it has one. Anonymous traffic never does.
    pub fn guardrails(&self) -> Option<&Guardrails> {
        match self {
            Self::Anonymous => None,
            Self::Key(key) => key.guardrails.as_ref(),
        }
    }

    /// The policy that governs the response side: `scan_output` with no blocked terms
    /// is a no-op, so it is reported as "no policy" rather than paying for a scan.
    pub fn output_guardrails(&self) -> Option<&Guardrails> {
        self.guardrails().filter(|g| g.scans_output())
    }

    /// The key id for a refusal message; anonymous traffic cannot be refused here.
    fn key_id_for_refusal(&self) -> String {
        match self {
            Self::Anonymous => "-".to_string(),
            Self::Key(key) => key.id.to_string(),
        }
    }
}

/// The prompt text a guardrail counts and searches: the system prompt plus every
/// message's text parts, joined by a newline.
///
/// Tool payloads are ignored on purpose — a tool result is machine data (JSON
/// arguments, function output), not prose a team typed, and scanning it would block
/// requests for data the client itself produced. Images carry no text. The newline
/// separator keeps a blocked term from matching across two unrelated parts, and it is
/// part of the count so the number an operator sees is the number that was measured.
pub fn prompt_text(request: &rrouter_core::ChatRequest) -> String {
    let mut segments: Vec<&str> = Vec::new();
    if let Some(system) = &request.system {
        segments.push(system);
    }
    for message in &request.messages {
        for part in &message.parts {
            if let rrouter_core::Part::Text(text) = part {
                segments.push(text);
            }
        }
    }
    segments.join("\n")
}

/// Input side of a key's guardrails. Runs after authentication and before planning, so
/// a refused prompt never reaches a provider, never consumes a rate-limit slot and
/// never fills the response cache. No policy → no work.
pub fn check_prompt(caller: &Caller, prompt: &str) -> Result<()> {
    let Some(policy) = caller.guardrails() else {
        return Ok(());
    };
    if let Some(max) = policy.max_prompt_chars {
        let chars = prompt.chars().count();
        if chars > max {
            crate::metrics::guardrail_blocked(crate::metrics::Stage::Input);
            return Err(Error::Forbidden {
                code: "prompt_too_long",
                message: format!(
                    "api key {} limits prompts to {max} characters; this one has {chars}",
                    caller.key_id_for_refusal()
                ),
            });
        }
    }
    if let Some(term) = policy.hit(prompt) {
        crate::metrics::guardrail_blocked(crate::metrics::Stage::Input);
        return Err(Error::Forbidden {
            code: "guardrail_blocked",
            message: format!(
                "api key {} refused the prompt: it contains the blocked term {:?}",
                caller.key_id_for_refusal(),
                term
            ),
        });
    }
    Ok(())
}

/// Micro-USD as dollars, for the message an operator reads. Four decimals is the
/// resolution the schema stores: one micro-dollar is $0.000001, so the fourth decimal
/// is where a budget refusal stays readable without lying about precision.
fn micros_to_usd(micros: u64) -> f64 {
    micros as f64 / 1_000_000.0
}

/// Does an allowlist cover `model`?
///
/// `allowed` comes from the database: `None` (NULL) allows every model, while an
/// explicitly empty list allows none. An entry matches when it is
///
/// - `*`,
/// - the model string itself,
/// - a `provider/*` glob covering the model's `provider/` prefix,
/// - the same model in its other form: a bare id matches the allowlist's full ref
///   (`openai/gpt-4.1` allows `gpt-4.1`), because the planner adds the provider and a
///   client should not be denied for spelling the model the shorter way.
///
/// A bare id is never covered by a `provider/*` glob: the provider is only known after
/// planning, and guessing it would widen the allowlist.
fn model_allowed(allowed: &[String], model: &str) -> bool {
    let bare = model.rsplit_once('/').map(|(_, id)| id).unwrap_or(model);
    allowed.iter().any(|raw| {
        let entry = raw.trim();
        if entry == "*" || entry == model {
            return true;
        }
        if let Some(provider) = entry.strip_suffix("/*") {
            return model.len() > provider.len()
                && model.starts_with(provider)
                && model.as_bytes()[provider.len()] == b'/';
        }
        entry.rsplit('/').next() == Some(bare)
    })
}

/// Sliding per-minute windows, one per key.
///
/// In memory by design: an `rpm`/`tpm` counter is a burst-control device, and a restart
/// clearing it is acceptable in phase 1. Spend is *not* here — the budget is summed from
/// `usage`, so it survives restarts.
#[derive(Default)]
pub struct KeyCounters {
    inner: Mutex<HashMap<Uuid, Window>>,
}

#[derive(Default)]
struct Window {
    /// Request arrival times, oldest first.
    requests: VecDeque<Instant>,
    /// `(arrival, tokens)` for finished responses, oldest first.
    tokens: VecDeque<(Instant, u64)>,
}

/// Which cap refused a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cap {
    Rpm { limit: u32 },
    Tpm { limit: u32, used: u64 },
}

impl Window {
    fn prune(&mut self, now: Instant) {
        while self
            .requests
            .front()
            .is_some_and(|at| now.saturating_duration_since(*at) >= MINUTE)
        {
            self.requests.pop_front();
        }
        while self
            .tokens
            .front()
            .is_some_and(|(at, _)| now.saturating_duration_since(*at) >= MINUTE)
        {
            self.tokens.pop_front();
        }
    }

    fn token_total(&self) -> u64 {
        self.tokens.iter().map(|(_, n)| *n).sum()
    }
}

impl KeyCounters {
    /// Check `rpm`/`tpm` and, when both allow it, count this request — one critical
    /// section, so two concurrent requests cannot both observe an empty window.
    pub fn try_admit(&self, id: Uuid, rpm: u32, tpm: u32) -> std::result::Result<(), Cap> {
        let now = Instant::now();
        let mut map = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let window = map.entry(id).or_default();
        window.prune(now);
        if rpm > 0 && window.requests.len() as u32 >= rpm {
            return Err(Cap::Rpm { limit: rpm });
        }
        let used = window.token_total();
        if tpm > 0 && used >= u64::from(tpm) {
            return Err(Cap::Tpm { limit: tpm, used });
        }
        window.requests.push_back(now);
        Ok(())
    }

    /// Record the tokens one finished response consumed.
    pub fn record_tokens(&self, id: Uuid, tokens: u64) {
        if tokens == 0 {
            return;
        }
        let now = Instant::now();
        let mut map = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let window = map.entry(id).or_default();
        window.prune(now);
        window.tokens.push_back((now, tokens));
    }

    /// Drop a deleted key's windows so a recreated id cannot inherit traffic.
    pub fn forget(&self, id: Uuid) {
        self.inner.lock().unwrap_or_else(|p| p.into_inner()).remove(&id);
    }
}

/// Dialect implied by an inbound URI path; unknown paths fall back to [OI] chat.
pub fn dialect_of(uri: &axum::http::Uri) -> Dialect {
    Dialect::from_path(uri.path()).unwrap_or(Dialect::OpenAiChat)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refs(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn allowlist_accepts_exact_refs_and_provider_globs() {
        let exact = refs(&["claude/claude-haiku-4-5-20251001"]);
        assert!(model_allowed(&exact, "claude/claude-haiku-4-5-20251001"));
        // Same model, shorter spelling.
        assert!(model_allowed(&exact, "claude-haiku-4-5-20251001"));
        assert!(!model_allowed(&exact, "claude/claude-sonnet-4-5-20250929"));

        let glob = refs(&["claude/*"]);
        assert!(model_allowed(&glob, "claude/anything"));
        assert!(!model_allowed(&glob, "openai/gpt-4.1"));
        // A glob is not widened by an unknown provider.
        assert!(!model_allowed(&glob, "anything"));

        assert!(model_allowed(&refs(&["*"]), "whatever"));
        assert!(!model_allowed(&[], "claude/haiku"));
    }

    #[test]
    fn concurrency_cap_is_built_only_when_requested() {
        assert!(concurrency_for(0).is_none(), "0 means unlimited");
        assert_eq!(concurrency_for(3).expect("cap").available_permits(), 3);
        // A huge value must clamp instead of panicking at startup.
        assert_eq!(
            concurrency_for(usize::MAX).expect("cap").available_permits(),
            tokio::sync::Semaphore::MAX_PERMITS
        );
    }

    #[test]
    fn sliding_window_admits_up_to_the_limit() {
        let counters = KeyCounters::default();
        let id = Uuid::new_v4();
        assert_eq!(counters.try_admit(id, 2, 0), Ok(()));
        assert_eq!(counters.try_admit(id, 2, 0), Ok(()));
        assert_eq!(counters.try_admit(id, 2, 0), Err(Cap::Rpm { limit: 2 }));

        // Unlimited (0) never refuses.
        assert_eq!(counters.try_admit(Uuid::new_v4(), 0, 0), Ok(()));
    }

    #[test]
    fn tpm_counts_recorded_tokens_only() {
        let counters = KeyCounters::default();
        let id = Uuid::new_v4();
        counters.record_tokens(id, 40);
        assert_eq!(counters.try_admit(id, 0, 100), Ok(()));
        counters.record_tokens(id, 40);
        assert_eq!(counters.try_admit(id, 0, 100), Ok(()));
        counters.record_tokens(id, 40);
        assert_eq!(
            counters.try_admit(id, 0, 100),
            Err(Cap::Tpm { limit: 100, used: 120 })
        );
        // A different key is unaffected.
        assert_eq!(counters.try_admit(Uuid::new_v4(), 0, 100), Ok(()));
    }
}
