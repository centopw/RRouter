//! Shared application state handed to every handler.
//!
//! It also owns the virtual-key governance path: resolving the presented key, the
//! allowlist, and the per-minute caps. Everything the hot path needs to refuse a
//! request lives here so [`crate::proxy`] only has to call it.

use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use tokio::sync::OwnedMutexGuard;

use axum::http::HeaderMap;
use rrouter_core::{Dialect, Error, Result};
use rrouter_router::{model_allowed, Catalog};
use rrouter_store::{ApiKey, Guardrails, KeyKind, KeyScope, Store};
use uuid::Uuid;

use crate::config::{config_file::EnvLookup, Config, ConfigSnapshot, ReloadOutcome};

/// Width of the sliding window behind `rpm` and `tpm`.
const MINUTE: Duration = Duration::from_secs(60);

/// Request header the dashboard playground sets on its `/v1` calls; see
/// [`AppState::authenticate`].
pub const PLAYGROUND_HEADER: &str = "x-rrouter-playground";

/// How long the first retention sweep waits after startup: long enough for the bind and
/// catalog refresh to finish, so a large first delete never delays `serve`.
const FIRST_SWEEP_DELAY: Duration = Duration::from_secs(10);

/// Retention period: history is measured in days, so one pass a day is the useful rate.
const SWEEP_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

const SECS_PER_DAY: i64 = 86_400;

/// One per-minute window as the client is told about it: the cap, what is left of it,
/// and when the oldest counted event expires.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowUsage {
    pub limit: u64,
    pub remaining: u64,
    pub reset_in: Duration,
}

/// What this request may still spend, as of admission.
///
/// Reported back as `x-ratelimit-*` (and `anthropic-ratelimit-*`) headers so a client can
/// pace itself instead of discovering a cap through a 429. An `None` field means "this
/// kind of limit does not exist for the key", never "nothing left": an unlimited key gets
/// no header rather than a fictional one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimits {
    /// The per-minute request cap, when the key has one.
    pub requests: Option<WindowUsage>,
    /// The per-minute token cap, when the key has one.
    pub tokens: Option<WindowUsage>,
    /// Tokens left in the key's durable budget window; `None` when unlimited.
    pub budget_tokens_remaining: Option<u64>,
    /// Micro-USD left in the key's durable budget window; `None` when unlimited.
    pub budget_micros_remaining: Option<u64>,
}

/// Guards held for one request against every budget it can spend (its key and its team).
///
/// Budget checks are read-then-spend, so two concurrent requests could both pass a
/// preflight check and overspend. A budgeted request takes these locks before admission
/// and holds them until its usage row is durable; unbudgeted traffic never touches them.
/// The tradeoff is deliberate: a key with an explicit budget serializes its own requests.
pub(crate) struct BudgetHold {
    _guards: Vec<OwnedMutexGuard<()>>,
}

pub struct AppState {
    pub store: Arc<Store>,
    pub catalog: Arc<Catalog>,
    pub refresher: Arc<rrouter_auth::Refresher>,
    pub planner: rrouter_router::Planner,
    pub dispatcher: Arc<rrouter_router::Dispatcher>,
    pub http: reqwest::Client,
    /// The live configuration. Read one snapshot per request with [`ConfigSnapshot::load`].
    pub config: ConfigSnapshot,
    pub started: Instant,
    /// Live OAuth sign-in sessions (one-time browser/device logins).
    pub oauth: rrouter_auth::OAuthFlows,
    /// Per-key request/token windows for the caps that cannot be answered from SQL.
    pub counters: KeyCounters,
    /// Caps concurrent `/v1/*` proxy requests; `None` when `RROUTER_MAX_CONCURRENCY`
    /// is 0. One permit spans a whole request, streamed body included. Reloadable: the
    /// semaphore is swapped atomically, and in-flight requests keep the old permits.
    pub concurrency: RwLock<Option<Arc<tokio::sync::Semaphore>>>,
    /// Dashboard auth: the setup code and sign-in throttle.
    pub auth: crate::auth::Auth,
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
        .with_hedge((config.hedge_ms > 0).then(|| Duration::from_millis(config.hedge_ms)))
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
        // One snapshot for the whole sweep: a reload mid-pass applies to the next pass.
        let config = self.config.load();
        let days = i64::try_from(config.usage_retention_days).unwrap_or(i64::MAX);
        let (deleted_usage, deleted_attempts) = if days == 0 {
            (0, 0)
        } else {
            let cutoff = now.saturating_sub(days.saturating_mul(SECS_PER_DAY));
            if let Err(e) = self.store.prune_audit(cutoff).await {
                tracing::warn!(error = %e, "audit retention delete failed");
            }
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
        let deleted_sessions = match self.store.prune_sessions(now).await {
            Ok(deleted) => deleted,
            Err(e) => {
                tracing::warn!(error = %e, "expired session delete failed");
                0
            }
        };
        if let Err(e) = self.store.optimize().await {
            tracing::warn!(error = %e, "pragma optimize failed");
        }
        tracing::info!(
            deleted_usage,
            deleted_attempts,
            deleted_cache,
            deleted_sessions,
            "retention sweep"
        );
    }

    pub fn request_timeout(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.config.load().request_timeout_secs)
    }

    pub fn uptime_secs(&self) -> u64 {
        self.started.elapsed().as_secs()
    }

    /// Take one `/v1/*` concurrency slot.
    ///
    /// Nothing is queued: with every permit in flight the caller answers `503`, which is
    /// the only backpressure a streaming gateway can apply without lying to the client.
    pub fn try_slot(&self) -> Slot {
        // Clone the current semaphore out of the lock: a reload may replace it, and a
        // request must keep the one it started with until its permit is released.
        let concurrency = self
            .concurrency
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let Some(semaphore) = concurrency else {
            return Slot::Granted(None);
        };
        match semaphore.try_acquire_owned() {
            Ok(permit) => Slot::Granted(Some(permit)),
            Err(_) => Slot::Full,
        }
    }

    /// Re-read the config file at `path` (environment wins, as at startup) and apply the
    /// reloadable fields. Nothing is applied when the file is invalid.
    pub fn reload_from_file(&self, path: &Path) -> Result<ReloadOutcome, String> {
        let loaded = Config::load_reloadable_env(path)?;
        Ok(self.apply_reload(&loaded.config))
    }

    /// [`Self::reload_from_file`] with an injected environment, for tests and dry runs.
    pub fn reload_from(&self, path: &Path, env: EnvLookup<'_>) -> Result<ReloadOutcome, String> {
        let loaded = Config::load_reloadable(path, env)?;
        Ok(self.apply_reload(&loaded.config))
    }

    /// Apply an already-loaded candidate. Deterministic: no file and no environment.
    ///
    /// Only reloadable fields move; restart-only and rejected fields keep their live
    /// values and are reported in the plan. The swap is one write lock, so no reader sees a
    /// half-applied configuration.
    pub fn apply_reload(&self, candidate: &Config) -> ReloadOutcome {
        let outcome = self.config.apply(candidate);
        if outcome.applied {
            self.sync_subsystems(&outcome.config);
            tracing::info!(
                changed = ?outcome.plan.changed_fields().collect::<Vec<_>>(),
                restart_required = ?outcome
                    .plan
                    .restart_required
                    .iter()
                    .map(|entry| entry.field)
                    .collect::<Vec<_>>(),
                rejected = ?outcome
                    .plan
                    .rejected
                    .iter()
                    .map(|entry| entry.field)
                    .collect::<Vec<_>>(),
                "configuration reloaded"
            );
        } else if outcome.plan.is_empty() {
            tracing::debug!("configuration unchanged; reload was a no-op");
        } else {
            tracing::warn!(
                restart_required = ?outcome
                    .plan
                    .restart_required
                    .iter()
                    .map(|entry| entry.field)
                    .collect::<Vec<_>>(),
                rejected = ?outcome
                    .plan
                    .rejected
                    .iter()
                    .map(|entry| entry.field)
                    .collect::<Vec<_>>(),
                "configuration reload not applied; restart required or field rejected"
            );
        }
        outcome
    }

    /// Push reloadable values into the pieces built from config at startup.
    ///
    /// Only the concurrency cap lives here: `request_timeout`, cache, auth and logging
    /// settings are read from the snapshot per use. The dispatcher and the HTTP client stay
    /// restart-only (see `ReloadPlan`).
    fn sync_subsystems(&self, config: &Config) {
        let next = concurrency_for(config.max_concurrency);
        *self
            .concurrency
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = next;
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
    ///   gateway, it does not spend tokens, so it has no business on `/v1/*`;
    /// - the system `playground` key is refused with `403 key_scope` unless the request
    ///   carries [`PLAYGROUND_HEADER`]`: 1` **and** a valid admin dashboard session. Its
    ///   plaintext is visible on the Access page, so the bearer alone must never be
    ///   enough to spend through it from another client.
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
            if !self.config.load().require_api_key {
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
        if key.kind == KeyKind::Playground && !self.is_playground_request(headers).await {
            return Err(Error::Forbidden {
                code: "key_scope",
                message: "the playground key is accepted only from the dashboard playground"
                    .to_string(),
            });
        }
        Ok(Caller::Key(key))
    }

    /// The playground marker plus a signed-in admin: the only context the playground key
    /// is valid in. The header is checked first so ordinary traffic never pays for a
    /// session lookup.
    async fn is_playground_request(&self, headers: &HeaderMap) -> bool {
        let marked = headers
            .get(PLAYGROUND_HEADER)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.trim() == "1");
        marked
            && crate::routes::auth::current_user(self, headers)
                .await
                .is_some_and(|user| user.is_admin())
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

    /// Take the budget locks a request needs before checking its spend. Returns `None`
    /// for anonymous or unbudgeted traffic; a budgeted key/team serializes requests until
    /// the previous request's usage row is committed.
    pub(crate) async fn budget_hold(&self, caller: &Caller) -> Result<Option<Arc<BudgetHold>>> {
        let Caller::Key(key) = caller else {
            return Ok(None);
        };
        let mut ids = Vec::new();
        if key.budget_tokens > 0 || key.budget_micros > 0 {
            ids.push(key.id);
        }
        if let Some(team) = self.store.key_team_row(key.id).await? {
            if team.budget_tokens > 0 || team.budget_micros > 0 {
                ids.push(team.id);
            }
        }
        if ids.is_empty() {
            return Ok(None);
        }
        ids.sort_unstable();
        ids.dedup();
        let mut guards = Vec::with_capacity(ids.len());
        for id in ids {
            guards.push(self.counters.budget_lock(id).lock_owned().await);
        }
        Ok(Some(Arc::new(BudgetHold { _guards: guards })))
    }

    /// Allowlist and per-key caps for an authenticated caller. Must run after the
    /// client's model is known and before any dispatch.
    ///
    /// Caps are checked in the order the contract names them: the allowlist (403), the
    /// durable budgets (429), then the in-memory per-minute windows (429). Nothing is
    /// counted for a request that is refused.
    ///
    /// The returned [`RateLimits`] is the state *after* this request's slot was taken, for
    /// the response headers: a client that reads `remaining-requests: 0` has actually
    /// spent its last slot. Anonymous traffic has no key policy and therefore no limits.
    pub async fn admit(&self, caller: &Caller, model: &str) -> Result<Option<RateLimits>> {
        // A free-form group prefix is a routing alias with its own grant list. Resolve
        // one alias hop first, or `fast -> main/gpt-4.1` would bypass the group check.
        let resolved = self.resolve_alias(model).await?;
        if let Some((head, _)) = resolved.split_once('/') {
            if rrouter_auth::registry::get(head).is_none() {
                if let Some(preset) = self.store.default_model_preset().await? {
                    if let Some(group) = preset.layout.group_by_name(head) {
                        if !rrouter_store::LayoutDocument::allows(
                            group,
                            caller.key_id(),
                            caller.team_id(),
                            caller.owner_user_id(),
                        ) {
                            return Err(Error::Forbidden {
                                code: "model_not_allowed",
                                message: format!(
                                    "this caller is not allowed to use model group {}",
                                    group.name
                                ),
                            });
                        }
                    }
                }
            }
        }
        let Caller::Key(key) = caller else {
            return Ok(None);
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
        let mut budget_tokens_remaining = None;
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
            budget_tokens_remaining = Some(key.budget_tokens - spent);
        }
        let mut budget_micros_remaining = None;
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
            budget_micros_remaining = Some(key.budget_micros - spent);
        }
        // Team budgets sit above the key's: a key that bills a team spends the team's
        // allowance, and the team's period is its own (`total` by default). A key with no
        // team is instance-wide and stays unlimited, exactly as it was before v23.
        if let Some(team) = self.store.key_team_row(key.id).await? {
            // No budget means no query: an unlimited team costs nothing to admit.
            if team.budget_tokens > 0 || team.budget_micros > 0 {
                let since = rrouter_store::window_start(team.budget_window, crate::now_unix());
                let spend = self.store.team_spend(team.id, since).await?;
                if team.budget_tokens > 0 && spend.tokens >= team.budget_tokens {
                    return Err(Error::Limited {
                        code: "team_budget_exhausted",
                        message: format!(
                            "team {} has spent {} of its {} token budget",
                            team.name, spend.tokens, team.budget_tokens
                        ),
                        // A lifetime budget needs an operator, not patience.
                        retry_after_secs: 3600,
                    });
                }
                if team.budget_micros > 0 && spend.cost_micros >= team.budget_micros {
                    return Err(Error::Limited {
                        code: "team_budget_exhausted",
                        message: format!(
                            "team {} has spent ${} of its ${} budget",
                            team.name,
                            dollars(spend.cost_micros),
                            dollars(team.budget_micros)
                        ),
                        retry_after_secs: 3600,
                    });
                }
            }
        }
        match self.counters.try_admit(key.id, key.rpm, key.tpm) {
            Ok(()) => Ok(Some(self.rate_limits(
                key,
                budget_tokens_remaining,
                budget_micros_remaining,
            ))),
            Err(Cap::Rpm { limit }) => Err(Error::Limited {
                code: "rate_limited",
                message: format!(
                    "api key {} is limited to {limit} requests per minute",
                    key.id
                ),
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

    /// The headers a *refused* request should carry: the windows as they stand, with no
    /// budget figures (the refusal message already names the budget). `None` for
    /// anonymous traffic, which has no limits to describe.
    pub(crate) fn refused_limits(&self, caller: &Caller) -> Option<RateLimits> {
        let Caller::Key(key) = caller else {
            return None;
        };
        let window = self.counters.snapshot(key.id);
        let usage = |limit: u32, used: u64| {
            (limit > 0).then(|| WindowUsage {
                limit: u64::from(limit),
                remaining: u64::from(limit).saturating_sub(used),
                reset_in: window.reset_in,
            })
        };
        Some(RateLimits {
            requests: usage(key.rpm, window.requests_used),
            tokens: usage(key.tpm, window.tokens_used),
            budget_tokens_remaining: None,
            budget_micros_remaining: None,
        })
    }

    /// The headers' view of a key's limits, read after its slot was taken.
    fn rate_limits(
        &self,
        key: &ApiKey,
        budget_tokens_remaining: Option<u64>,
        budget_micros_remaining: Option<u64>,
    ) -> RateLimits {
        let window = self.counters.snapshot(key.id);
        let usage = |limit: u32, used: u64| {
            (limit > 0).then(|| WindowUsage {
                limit: u64::from(limit),
                remaining: u64::from(limit).saturating_sub(used),
                reset_in: window.reset_in,
            })
        };
        RateLimits {
            requests: usage(key.rpm, window.requests_used),
            tokens: usage(key.tpm, window.tokens_used),
            budget_tokens_remaining,
            budget_micros_remaining,
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
    pub(crate) async fn resolve_alias(&self, model: &str) -> Result<String> {
        let aliases = self.store.list_aliases().await?;
        Ok(aliases
            .iter()
            .find(|a| a.alias == model)
            .map(|a| a.target.clone())
            .unwrap_or_else(|| model.to_string()))
    }
}

/// Who pays for this request.
// `ApiKey` grew with team/owner attribution and now dominates the enum. Boxing it would
// add an allocation to every authenticated request, and `Caller` is a short-lived local
// (never stored in a collection), so the size difference is deliberate.
#[allow(clippy::large_enum_variant)]
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

    /// The paying key's `allowed_models`, for the planner's post-expansion check.
    /// `None` means every model is allowed (and for anonymous traffic there is no list).
    pub fn allowed_models(&self) -> Option<&[String]> {
        match self {
            Self::Anonymous => None,
            Self::Key(key) => key.allowed_models.as_deref(),
        }
    }

    /// The paying key's team, or `None` for an instance-wide key and anonymous traffic.
    /// The planner restricts a team's candidates to that team's own accounts.
    pub fn team_id(&self) -> Option<Uuid> {
        match self {
            Self::Anonymous => None,
            Self::Key(key) => key.team_id,
        }
    }

    /// The paying key's owner, when it has one. User-targeted group grants match this.
    pub fn owner_user_id(&self) -> Option<Uuid> {
        match self {
            Self::Anonymous => None,
            Self::Key(key) => key.owner_user_id,
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

/// Micro-USD as the dollars an operator reads, at the precision the stored unit justifies:
/// six decimals below a cent (a real spend must not read "$0.0000"), four above it.
fn dollars(micros: u64) -> String {
    let usd = micros_to_usd(micros);
    if usd < 0.01 {
        format!("{usd:.6}")
    } else {
        format!("{usd:.4}")
    }
}

/// Sliding per-minute windows, one per key.
///
/// In memory by design: an `rpm`/`tpm` counter is a burst-control device, and a restart
/// clearing it is acceptable in phase 1. Spend is *not* here — the budget is summed from
/// `usage`, so it survives restarts.
#[derive(Default)]
pub struct KeyCounters {
    inner: Mutex<HashMap<Uuid, Window>>,
    /// One async mutex per budgeted key/team. Entries are small and never removed while
    /// an in-flight request may still hold the old lock.
    budget_locks: Mutex<HashMap<Uuid, Arc<tokio::sync::Mutex<()>>>>,
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

/// What a key's windows hold right now, for the response headers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowSnapshot {
    /// Requests admitted in the current window.
    pub requests_used: u64,
    /// Tokens recorded in the current window.
    pub tokens_used: u64,
    /// Time until the oldest counted event leaves the window; a full minute when the
    /// window is empty, so a caller always has a usable reset hint.
    pub reset_in: Duration,
}

impl WindowSnapshot {
    fn empty() -> Self {
        Self {
            requests_used: 0,
            tokens_used: 0,
            reset_in: MINUTE,
        }
    }
}

impl KeyCounters {
    /// Read the window without consuming a slot.
    ///
    /// Expired entries are pruned first — that is bookkeeping, not accounting: the
    /// snapshot must report exactly what the next [`KeyCounters::try_admit`] would see.
    /// Nothing is pushed, so headers can be read on every response (including errors)
    /// without inflating the request or token counts.
    pub fn snapshot(&self, id: Uuid) -> WindowSnapshot {
        let now = Instant::now();
        let mut map = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let Some(window) = map.get_mut(&id) else {
            return WindowSnapshot::empty();
        };
        window.prune(now);
        let oldest = window
            .requests
            .front()
            .copied()
            .into_iter()
            .chain(window.tokens.front().map(|(at, _)| *at))
            .min();
        WindowSnapshot {
            requests_used: window.requests.len() as u64,
            tokens_used: window.token_total(),
            reset_in: oldest
                .map(|at| MINUTE.saturating_sub(now.saturating_duration_since(at)))
                .unwrap_or(MINUTE),
        }
    }

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
        self.inner
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&id);
    }

    fn budget_lock(&self, id: Uuid) -> Arc<tokio::sync::Mutex<()>> {
        self.budget_locks
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .entry(id)
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
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

    /// The snapshot is what the headers report: used and remaining are complementary,
    /// and the reset hint is inside the one-minute window.
    #[test]
    fn snapshot_reports_remaining_and_resets_within_a_minute() {
        let counters = KeyCounters::default();
        let id = Uuid::new_v4();
        assert_eq!(counters.try_admit(id, 3, 0), Ok(()));

        let first = counters.snapshot(id);
        assert_eq!(first.requests_used, 1);
        assert!(
            first.reset_in > Duration::ZERO && first.reset_in <= MINUTE,
            "reset hint was {:?}",
            first.reset_in
        );

        assert_eq!(counters.try_admit(id, 3, 0), Ok(()));
        assert_eq!(counters.try_admit(id, 3, 0), Ok(()));
        let full = counters.snapshot(id);
        assert_eq!(full.requests_used, 3);
        assert_eq!(u64::from(3u32) - full.requests_used, 0, "nothing is left");
        assert_eq!(counters.try_admit(id, 3, 0), Err(Cap::Rpm { limit: 3 }));
    }

    /// Reading the window must not consume a slot: headers are rendered on every
    /// response, including errors, and a read that counted would rate-limit itself.
    #[test]
    fn snapshot_does_not_count_a_request() {
        let counters = KeyCounters::default();
        let id = Uuid::new_v4();

        // An unknown key is never given an entry just by being observed.
        for _ in 0..5 {
            assert_eq!(counters.snapshot(id), WindowSnapshot::empty());
        }

        assert_eq!(counters.try_admit(id, 2, 0), Ok(()));
        let before = counters.snapshot(id);
        assert_eq!(before.requests_used, 1);
        for _ in 0..5 {
            let again = counters.snapshot(id);
            assert_eq!(again.requests_used, before.requests_used);
            assert_eq!(again.tokens_used, before.tokens_used);
            // The hint only shrinks with the clock; it never counts a new event.
            assert!(again.reset_in <= before.reset_in);
        }

        // Both admitted slots are still available: one used, one left.
        assert_eq!(counters.try_admit(id, 2, 0), Ok(()));
        assert_eq!(counters.try_admit(id, 2, 0), Err(Cap::Rpm { limit: 2 }));

        // Tokens move only when a response reports them, never when they are read.
        counters.record_tokens(id, 10);
        assert_eq!(counters.snapshot(id).tokens_used, 10);
        for _ in 0..3 {
            assert_eq!(counters.snapshot(id).tokens_used, 10);
        }
    }

    /// Budget locks are per id: two requests for one budgeted key/team cannot both hold
    /// the gate, while a different id is independent.
    #[tokio::test]
    async fn budget_lock_serializes_the_same_key_or_team() {
        let counters = KeyCounters::default();
        let id = Uuid::new_v4();
        let other = Uuid::new_v4();
        let first = counters.budget_lock(id).lock_owned().await;
        assert!(
            counters.budget_lock(id).try_lock_owned().is_err(),
            "the same budget id must wait for the in-flight request"
        );
        assert!(
            counters.budget_lock(other).try_lock_owned().is_ok(),
            "an unrelated budget id is independent"
        );
        drop(first);
        assert!(
            counters.budget_lock(id).try_lock_owned().is_ok(),
            "the lock is released with the request guard"
        );
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
            concurrency_for(usize::MAX)
                .expect("cap")
                .available_permits(),
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
            Err(Cap::Tpm {
                limit: 100,
                used: 120
            })
        );
        // A different key is unaffected.
        assert_eq!(counters.try_admit(Uuid::new_v4(), 0, 100), Ok(()));
    }
}
