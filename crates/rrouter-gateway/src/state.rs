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
use rrouter_store::{ApiKey, Store};
use uuid::Uuid;

use crate::config::Config;

/// Width of the sliding window behind `rpm` and `tpm`.
const MINUTE: Duration = Duration::from_secs(60);

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
    /// - an unknown or missing key is a `401` when keys are required.
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
        Ok(Caller::Key(key))
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
        if key.budget_tokens > 0 {
            let spent = self.store.api_key_spend(key.id).await?;
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
