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
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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

/// Normalize a model id for free-tier limit/counter lookup.
///
/// Gemini's OpenAI-compatible `/models` endpoint returns ids prefixed with `"models/"`
/// (for example `"models/gemini-2.5-flash"`), whereas the vendored catalog and clients
/// name `"gemini-2.5-flash"`. Stripping the prefix on `gemini` keeps discovery, vendored
/// limits, learned limits, and daily counters on the same key.
pub fn normalize_model_key<'a>(provider: &str, model: &'a str) -> &'a str {
    if provider == "gemini" {
        model.strip_prefix("models/").unwrap_or(model)
    } else {
        model
    }
}

/// One kind of per-model rate limit a provider names in an error body or vendored row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitKind {
    RequestsPerMinute,
    RequestsPerDay,
    TokensPerMinute,
    TokensPerDay,
}

impl LimitKind {
    /// Short lowercase token for display/logging.
    pub fn code(self) -> &'static str {
        match self {
            Self::RequestsPerMinute => "rpm",
            Self::RequestsPerDay => "rpd",
            Self::TokensPerMinute => "tpm",
            Self::TokensPerDay => "tpd",
        }
    }
}

/// Effective per-model limits (vendored seed tightened by anything learned from provider
/// error bodies). `None` means unknown — never invent a cap.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ModelLimits {
    pub rpm: Option<u64>,
    pub rpd: Option<u64>,
    pub tpm: Option<u64>,
    pub tpd: Option<u64>,
}

impl ModelLimits {
    /// True when none of the four limits is known.
    pub fn is_empty(&self) -> bool {
        self.rpm.is_none() && self.rpd.is_none() && self.tpm.is_none() && self.tpd.is_none()
    }

    /// Lower one limit when `value > 0` and tighter than the stored value. Learned limits
    /// only ever tighten — never raise — a cap. Returns `true` if the stored value changed.
    pub fn tighten(&mut self, kind: LimitKind, value: u64) -> bool {
        if value == 0 {
            return false;
        }
        let slot = match kind {
            LimitKind::RequestsPerMinute => &mut self.rpm,
            LimitKind::RequestsPerDay => &mut self.rpd,
            LimitKind::TokensPerMinute => &mut self.tpm,
            LimitKind::TokensPerDay => &mut self.tpd,
        };
        match *slot {
            Some(existing) if existing <= value => false,
            _ => {
                *slot = Some(value);
                true
            }
        }
    }

    /// Combine a vendored baseline with learned overrides, always taking the tighter value.
    pub fn merge_tighter(base: Self, learned: Self) -> Self {
        let min_opt = |a: Option<u64>, b: Option<u64>| match (a, b) {
            (Some(x), Some(y)) => Some(x.min(y)),
            (Some(x), None) | (None, Some(x)) => Some(x),
            (None, None) => None,
        };
        Self {
            rpm: min_opt(base.rpm, learned.rpm),
            rpd: min_opt(base.rpd, learned.rpd),
            tpm: min_opt(base.tpm, learned.tpm),
            tpd: min_opt(base.tpd, learned.tpd),
        }
    }
}

/// Reason a proactive per-model free-tier check skipped a candidate before dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelCapHit {
    /// Already parked until UTC midnight after a daily exhaustion.
    DailyPark { retry_after_secs: u64 },
    /// Per-model daily request cap (RPD) reached; parked until UTC midnight.
    Rpd {
        used: u32,
        limit: u64,
        retry_after_secs: u64,
    },
    /// Per-model daily token cap (TPD) reached; parked until UTC midnight.
    Tpd {
        used: u64,
        limit: u64,
        retry_after_secs: u64,
    },
    /// Per-model minute request cap (RPM) reached.
    Rpm {
        used: u32,
        limit: u64,
        retry_after_secs: u64,
    },
    /// Per-model minute token cap (TPM) reached.
    Tpm {
        used: u64,
        limit: u64,
        retry_after_secs: u64,
    },
}

impl ModelCapHit {
    pub fn retry_after_secs(self) -> u64 {
        match self {
            Self::DailyPark { retry_after_secs }
            | Self::Rpd {
                retry_after_secs, ..
            }
            | Self::Tpd {
                retry_after_secs, ..
            }
            | Self::Rpm {
                retry_after_secs, ..
            }
            | Self::Tpm {
                retry_after_secs, ..
            } => retry_after_secs.max(1),
        }
    }

    pub fn describe(self, provider: &str, model: &str) -> String {
        match self {
            Self::DailyPark { retry_after_secs } => format!(
                "{provider}/{model} daily free-tier quota exhausted; parked until UTC midnight ({retry_after_secs}s remaining)"
            ),
            Self::Rpd { used, limit, .. } => format!(
                "{provider}/{model} free-tier daily request cap ({used}/{limit} RPD) reached; parked until UTC midnight"
            ),
            Self::Tpd { used, limit, .. } => format!(
                "{provider}/{model} free-tier daily token cap ({used}/{limit} TPD) reached; parked until UTC midnight"
            ),
            Self::Rpm {
                used,
                limit,
                retry_after_secs,
            } => format!(
                "{provider}/{model} free-tier minute request cap ({used}/{limit} RPM) reached; retry in {retry_after_secs}s"
            ),
            Self::Tpm {
                used,
                limit,
                retry_after_secs,
            } => format!(
                "{provider}/{model} free-tier minute token cap ({used}/{limit} TPM) reached; retry in {retry_after_secs}s"
            ),
        }
    }
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
    /// Proactive provider-wide request counts per `(connection, provider)` over the current
    /// UTC calendar day, so an account approaching a provider cap (OpenRouter 1000/day) is
    /// skipped *before* it spends an attempt, not after a 429.
    provider_days: HashMap<(Uuid, String), ProviderDay>,
    /// Per-model daily request and token counts per `(connection, model)` over the current
    /// UTC calendar day (RPD + TPD, gap 2).
    model_days: HashMap<(Uuid, String), ModelDay>,
    /// Per-model 60 s sliding request and token windows per `(connection, model)` (RPM + TPM).
    model_minutes: HashMap<(Uuid, String), ModelMinute>,
    /// Per-model daily exhaustion parks keyed by `(connection, model)`. Unlike the 1 h
    /// transient 429 cap in `Breaker`, a daily exhaustion parks until the next UTC midnight.
    model_daily_parks: HashMap<(Uuid, String), Instant>,
    /// Tightened per-model limits learned from upstream 429/413 error bodies, keyed by
    /// `(provider, normalized_model)`.
    learned_limits: HashMap<(String, String), ModelLimits>,
    /// True once today's durable `usage` rows have been loaded into the UTC-day counters.
    seeded_from_usage: bool,
}

/// One provider-wide day count: the UTC day it was counted over and how many sends it saw.
#[derive(Debug, Default)]
struct ProviderDay {
    /// The UTC midnight epoch-second the window started at; a stale entry rolls over.
    day_start: u64,
    count: u32,
}

/// One per-model UTC-day bucket: request count (RPD) and token count (TPD).
#[derive(Debug, Default)]
struct ModelDay {
    day_start: u64,
    requests: u32,
    tokens: u64,
}

impl ModelDay {
    fn ensure_day(&mut self, day_start: u64) {
        if self.day_start != day_start {
            self.day_start = day_start;
            self.requests = 0;
            self.tokens = 0;
        }
    }
}

/// Per-model 60 s sliding window for RPM and TPM.
#[derive(Debug, Default)]
struct ModelMinute {
    requests: VecDeque<Instant>,
    tokens: VecDeque<(Instant, u64)>,
}

impl ModelMinute {
    fn prune(&mut self, now: Instant) {
        while let Some(oldest) = self.requests.front() {
            if now.duration_since(*oldest) < WINDOW {
                break;
            }
            self.requests.pop_front();
        }
        while let Some((oldest, _)) = self.tokens.front() {
            if now.duration_since(*oldest) < WINDOW {
                break;
            }
            self.tokens.pop_front();
        }
    }

    fn request_count(&self) -> u32 {
        self.requests.len().min(u32::MAX as usize) as u32
    }

    fn request_reset_secs(&self, now: Instant) -> u64 {
        self.requests
            .front()
            .map(|oldest| {
                WINDOW
                    .saturating_sub(now.duration_since(*oldest))
                    .as_secs()
                    .max(1)
            })
            .unwrap_or(1)
    }

    fn token_sum(&self) -> u64 {
        self.tokens
            .iter()
            .fold(0u64, |acc, (_, tok)| acc.saturating_add(*tok))
    }

    fn token_reset_secs(&self, now: Instant) -> u64 {
        self.tokens
            .front()
            .map(|(oldest, _)| {
                WINDOW
                    .saturating_sub(now.duration_since(*oldest))
                    .as_secs()
                    .max(1)
            })
            .unwrap_or(1)
    }
}

/// UTC-midnight epoch second for `now` (or the test clock): the provider day boundary.
pub fn utc_day_start(now_unix: u64) -> u64 {
    now_unix - (now_unix % 86_400)
}

/// Whole seconds from `now_unix` until the next UTC midnight (the reset of every provider
/// daily window). Used to park a daily-exhausted model/account until the window rolls over,
/// instead of the 1 h cap a transient 429 earns.
pub fn seconds_until_utc_midnight(now_unix: u64) -> u64 {
    utc_day_start(now_unix)
        .saturating_add(86_400)
        .saturating_sub(now_unix)
        .max(1)
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
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

    /// Record one send to `(connection, provider)` for the proactive provider-wide day cap.
    pub fn note_provider_day(&self, connection: Uuid, provider: &str) {
        self.note_provider_day_at(connection, provider, now_unix_secs());
    }

    pub fn note_provider_day_at(&self, connection: Uuid, provider: &str, now_unix: u64) {
        let mut state = self.lock();
        let day_start = utc_day_start(now_unix);
        let entry = state
            .provider_days
            .entry((connection, provider.to_string()))
            .or_default();
        if entry.day_start != day_start {
            entry.day_start = day_start;
            entry.count = 0;
        }
        entry.count = entry.count.saturating_add(1);
    }

    /// Count of sends to `(connection, provider)` in the current UTC day (rolling over a
    /// stale entry to zero first).
    pub fn provider_day_count(&self, connection: Uuid, provider: &str) -> u32 {
        self.provider_day_count_at(connection, provider, now_unix_secs())
    }

    pub fn provider_day_count_at(&self, connection: Uuid, provider: &str, now_unix: u64) -> u32 {
        let mut state = self.lock();
        let day_start = utc_day_start(now_unix);
        let entry = state
            .provider_days
            .entry((connection, provider.to_string()))
            .or_default();
        if entry.day_start != day_start {
            entry.day_start = day_start;
            entry.count = 0;
        }
        entry.count
    }

    /// Record one send to `(connection, model)` toward per-model RPM and RPD windows.
    /// Also marks `(connection, model)` as tracked so response token accounting records
    /// into its TPM/TPD windows without allocating entries for paid connections.
    pub fn note_model_send(&self, connection: Uuid, model: &str) {
        self.note_model_send_at(connection, model, Instant::now(), now_unix_secs());
    }

    pub fn note_model_send_at(
        &self,
        connection: Uuid,
        model: &str,
        now_instant: Instant,
        now_unix: u64,
    ) {
        let mut state = self.lock();
        let day_start = utc_day_start(now_unix);
        let key = (connection, model.to_string());
        let day = state.model_days.entry(key.clone()).or_default();
        day.ensure_day(day_start);
        day.requests = day.requests.saturating_add(1);

        let minute = state.model_minutes.entry(key).or_default();
        minute.prune(now_instant);
        minute.requests.push_back(now_instant);
    }

    /// Record one send to `(connection, model)` toward a per-model daily request cap (RPD).
    pub fn note_model_day(&self, connection: Uuid, model: &str) {
        self.note_model_day_at(connection, model, now_unix_secs());
    }

    pub fn note_model_day_at(&self, connection: Uuid, model: &str, now_unix: u64) {
        let mut state = self.lock();
        let day_start = utc_day_start(now_unix);
        let entry = state
            .model_days
            .entry((connection, model.to_string()))
            .or_default();
        entry.ensure_day(day_start);
        entry.requests = entry.requests.saturating_add(1);
    }

    /// Count of sends to `(connection, model)` in the current UTC day (rolling over a stale
    /// entry to zero first).
    pub fn model_day_count(&self, connection: Uuid, model: &str) -> u32 {
        self.model_day_count_at(connection, model, now_unix_secs())
    }

    pub fn model_day_count_at(&self, connection: Uuid, model: &str, now_unix: u64) -> u32 {
        let mut state = self.lock();
        let day_start = utc_day_start(now_unix);
        let entry = state
            .model_days
            .entry((connection, model.to_string()))
            .or_default();
        entry.ensure_day(day_start);
        entry.requests
    }

    /// True when `(connection, model)` has been tracked as a free-tier send (or seeded from
    /// today's usage), so completion token recording and daily-exhaustion parking apply to
    /// it without touching paid accounts.
    pub fn is_tracking_model(&self, connection: Uuid, model: &str) -> bool {
        let state = self.lock();
        state
            .model_days
            .contains_key(&(connection, model.to_string()))
    }

    /// Record completed tokens for `(connection, model)` toward TPM and TPD only if this
    /// `(connection, model)` is already tracked as a free-tier candidate.
    pub fn note_model_tokens_if_tracked(&self, connection: Uuid, model: &str, tokens: u64) {
        if tokens == 0 {
            return;
        }
        self.note_model_tokens_if_tracked_at(
            connection,
            model,
            tokens,
            Instant::now(),
            now_unix_secs(),
        );
    }

    pub fn note_model_tokens_if_tracked_at(
        &self,
        connection: Uuid,
        model: &str,
        tokens: u64,
        now_instant: Instant,
        now_unix: u64,
    ) {
        if tokens == 0 {
            return;
        }
        let mut state = self.lock();
        let key = (connection, model.to_string());
        let Some(day) = state.model_days.get_mut(&key) else {
            return;
        };
        day.ensure_day(utc_day_start(now_unix));
        day.tokens = day.tokens.saturating_add(tokens);

        let minute = state.model_minutes.entry(key).or_default();
        minute.prune(now_instant);
        minute.tokens.push_back((now_instant, tokens));
    }

    /// Unconditionally record tokens for `(connection, model)` toward TPM and TPD.
    pub fn note_model_tokens(&self, connection: Uuid, model: &str, tokens: u64) {
        self.note_model_tokens_at(connection, model, tokens, Instant::now(), now_unix_secs());
    }

    pub fn note_model_tokens_at(
        &self,
        connection: Uuid,
        model: &str,
        tokens: u64,
        now_instant: Instant,
        now_unix: u64,
    ) {
        if tokens == 0 {
            return;
        }
        let mut state = self.lock();
        let key = (connection, model.to_string());
        let day = state.model_days.entry(key.clone()).or_default();
        day.ensure_day(utc_day_start(now_unix));
        day.tokens = day.tokens.saturating_add(tokens);

        let minute = state.model_minutes.entry(key).or_default();
        minute.prune(now_instant);
        minute.tokens.push_back((now_instant, tokens));
    }

    /// Tokens spent on `(connection, model)` in the current UTC day.
    pub fn model_day_tokens(&self, connection: Uuid, model: &str) -> u64 {
        self.model_day_tokens_at(connection, model, now_unix_secs())
    }

    pub fn model_day_tokens_at(&self, connection: Uuid, model: &str, now_unix: u64) -> u64 {
        let mut state = self.lock();
        let day_start = utc_day_start(now_unix);
        let Some(entry) = state.model_days.get_mut(&(connection, model.to_string())) else {
            return 0;
        };
        entry.ensure_day(day_start);
        entry.tokens
    }

    /// Park `(connection, model)` until UTC midnight (or the stated reset wait) after a
    /// daily exhaustion. Unlike `Breaker`'s 1 h transient cap, daily parks last up to 24 h.
    pub fn record_model_daily_park(&self, connection: Uuid, model: &str, wait: Duration) {
        self.record_model_daily_park_at(connection, model, wait, Instant::now());
    }

    pub fn record_model_daily_park_at(
        &self,
        connection: Uuid,
        model: &str,
        wait: Duration,
        now: Instant,
    ) {
        let mut state = self.lock();
        let until = now + wait.max(Duration::from_secs(1));
        state
            .model_daily_parks
            .insert((connection, model.to_string()), until);
    }

    /// Remaining daily-exhaustion park duration for `(connection, model)`, if active.
    pub fn model_daily_park_wait(&self, connection: Uuid, model: &str) -> Option<Duration> {
        self.model_daily_park_wait_at(connection, model, Instant::now())
    }

    pub fn model_daily_park_wait_at(
        &self,
        connection: Uuid,
        model: &str,
        now: Instant,
    ) -> Option<Duration> {
        let mut state = self.lock();
        let key = (connection, model.to_string());
        let until = *state.model_daily_parks.get(&key)?;
        if now >= until {
            state.model_daily_parks.remove(&key);
            return None;
        }
        Some(until.duration_since(now))
    }

    /// Clear a per-model daily park (for example after a manual probe succeeds).
    pub fn clear_model_daily_park(&self, connection: Uuid, model: &str) {
        let mut state = self.lock();
        state
            .model_daily_parks
            .remove(&(connection, model.to_string()));
    }

    /// Record a learned per-model limit from a provider 429/413 error body. Only ever
    /// tightens the stored limit (never raises it).
    pub fn learn_model_limit(
        &self,
        provider: &str,
        model: &str,
        kind: LimitKind,
        value: u64,
    ) -> bool {
        if value == 0 {
            return false;
        }
        let mut state = self.lock();
        let key = (
            provider.to_string(),
            normalize_model_key(provider, model).to_string(),
        );
        state
            .learned_limits
            .entry(key)
            .or_default()
            .tighten(kind, value)
    }

    /// Previously learned limits for `(provider, model)` (empty when none were observed).
    pub fn learned_model_limits(&self, provider: &str, model: &str) -> ModelLimits {
        let state = self.lock();
        let key = (
            provider.to_string(),
            normalize_model_key(provider, model).to_string(),
        );
        state.learned_limits.get(&key).copied().unwrap_or_default()
    }

    /// Proactive check of all per-model caps (daily park, RPD, TPD, RPM, TPM) for
    /// `(connection, model)`. When RPD or TPD is exhausted, parks `(connection, model)`
    /// until the next UTC midnight so subsequent requests skip without spending an attempt.
    pub fn check_model_caps(
        &self,
        connection: Uuid,
        model: &str,
        limits: ModelLimits,
    ) -> Option<ModelCapHit> {
        self.check_model_caps_at(connection, model, limits, Instant::now(), now_unix_secs())
    }

    pub fn check_model_caps_at(
        &self,
        connection: Uuid,
        model: &str,
        limits: ModelLimits,
        now_instant: Instant,
        now_unix: u64,
    ) -> Option<ModelCapHit> {
        let mut state = self.lock();
        let key = (connection, model.to_string());

        // 1. Existing daily park (from an earlier daily 429 or proactive RPD/TPD hit).
        if let Some(&until) = state.model_daily_parks.get(&key) {
            if now_instant >= until {
                state.model_daily_parks.remove(&key);
            } else {
                return Some(ModelCapHit::DailyPark {
                    retry_after_secs: until.duration_since(now_instant).as_secs().max(1),
                });
            }
        }

        if limits.is_empty() {
            return None;
        }

        // 2. UTC-day RPD and TPD caps: reaching either parks until next UTC midnight.
        let day_start = utc_day_start(now_unix);
        let (day_requests, day_tokens) = if let Some(day) = state.model_days.get_mut(&key) {
            day.ensure_day(day_start);
            (day.requests, day.tokens)
        } else {
            (0, 0)
        };
        if let Some(rpd) = limits.rpd.filter(|&v| v > 0) {
            if u64::from(day_requests) >= rpd {
                let wait_secs = seconds_until_utc_midnight(now_unix);
                state
                    .model_daily_parks
                    .insert(key, now_instant + Duration::from_secs(wait_secs));
                return Some(ModelCapHit::Rpd {
                    used: day_requests,
                    limit: rpd,
                    retry_after_secs: wait_secs,
                });
            }
        }
        if let Some(tpd) = limits.tpd.filter(|&v| v > 0) {
            if day_tokens >= tpd {
                let wait_secs = seconds_until_utc_midnight(now_unix);
                state
                    .model_daily_parks
                    .insert(key, now_instant + Duration::from_secs(wait_secs));
                return Some(ModelCapHit::Tpd {
                    used: day_tokens,
                    limit: tpd,
                    retry_after_secs: wait_secs,
                });
            }
        }

        // 3. Sliding 60 s RPM and TPM caps.
        if let Some(minute) = state.model_minutes.get_mut(&key) {
            minute.prune(now_instant);
            if let Some(rpm) = limits.rpm.filter(|&v| v > 0) {
                let used = minute.request_count();
                if u64::from(used) >= rpm {
                    return Some(ModelCapHit::Rpm {
                        used,
                        limit: rpm,
                        retry_after_secs: minute.request_reset_secs(now_instant),
                    });
                }
            }
            if let Some(tpm) = limits.tpm.filter(|&v| v > 0) {
                let used = minute.token_sum();
                if used >= tpm {
                    return Some(ModelCapHit::Tpm {
                        used,
                        limit: tpm,
                        retry_after_secs: minute.token_reset_secs(now_instant),
                    });
                }
            }
        }

        None
    }

    /// True until [`Self::seed_from_usage_at`] has run once after boot.
    pub fn needs_usage_seed(&self) -> bool {
        !self.lock().seeded_from_usage
    }

    /// Rebuild today's UTC-day request and token counts from durable `usage` aggregates
    /// (`ts >= today 00:00 UTC`). Idempotent: only the first caller after boot applies.
    pub fn seed_from_usage_at(&self, rows: &[(Uuid, String, String, u32, u64)], now_unix: u64) {
        let mut state = self.lock();
        if state.seeded_from_usage {
            return;
        }
        state.seeded_from_usage = true;
        let day_start = utc_day_start(now_unix);
        for (conn, provider, model, requests, tokens) in rows {
            if *requests > 0 {
                let pday = state
                    .provider_days
                    .entry((*conn, provider.clone()))
                    .or_default();
                if pday.day_start != day_start {
                    pday.day_start = day_start;
                    pday.count = 0;
                }
                pday.count = pday.count.saturating_add(*requests);
            }
            let norm = normalize_model_key(provider, model).to_string();
            let mday = state.model_days.entry((*conn, norm)).or_default();
            mday.ensure_day(day_start);
            mday.requests = mday.requests.saturating_add(*requests);
            mday.tokens = mday.tokens.saturating_add(*tokens);
        }
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

    /// The provider-wide day counter counts sends and rolls over at the next UTC midnight.
    #[test]
    fn provider_day_count_rolls_over_at_utc_midnight() {
        let rot = FreeRotation::new();
        let conn = Uuid::new_v4();
        // Pick a fixed instant: 2026-01-02 00:30 UTC = 1767312000 + 1800.
        let day_start = 1_767_312_000; // 2026-01-02 00:00 UTC
        let t0 = day_start + 1800;

        assert_eq!(rot.provider_day_count_at(conn, "openrouter", t0), 0);
        rot.note_provider_day_at(conn, "openrouter", t0);
        rot.note_provider_day_at(conn, "openrouter", t0 + 60);
        assert_eq!(rot.provider_day_count_at(conn, "openrouter", t0 + 120), 2);

        // The next day, the count resets to zero.
        let next_day = day_start + 86_400;
        assert_eq!(
            rot.provider_day_count_at(conn, "openrouter", next_day + 60),
            0,
            "the day window rolls over at UTC midnight"
        );
    }

    /// Per-model RPD, TPD, RPM, and TPM caps gate accurately, park daily exhaustions until
    /// UTC midnight (beyond 1 h), and roll over cleanly after midnight / 60 s.
    #[test]
    fn per_model_rpd_tpd_rpm_tpm_and_daily_park_to_utc_midnight() {
        let rot = FreeRotation::new();
        let conn = Uuid::new_v4();
        let day_start = 1_767_312_000; // 2026-01-02 00:00 UTC
        let now_unix = day_start + 3_600; // 01:00 UTC -> 23 h (82,800 s) until midnight
        let t0 = Instant::now();

        let limits = ModelLimits {
            rpm: Some(2),
            rpd: Some(3),
            tpm: Some(1_000),
            tpd: Some(2_000),
        };

        // Two sends hit the 2 RPM cap (still under 3 RPD).
        rot.note_model_send_at(conn, "gemini-2.5-flash", t0, now_unix);
        rot.note_model_send_at(
            conn,
            "gemini-2.5-flash",
            t0 + Duration::from_secs(5),
            now_unix + 5,
        );
        assert!(matches!(
            rot.check_model_caps_at(
                conn,
                "gemini-2.5-flash",
                limits,
                t0 + Duration::from_secs(10),
                now_unix + 10,
            ),
            Some(ModelCapHit::Rpm {
                used: 2,
                limit: 2,
                ..
            })
        ));

        // After 61 s the RPM window clears, but recording 1,200 tokens trips TPM (1,000).
        let t1 = t0 + Duration::from_secs(61);
        let u1 = now_unix + 61;
        assert_eq!(
            rot.check_model_caps_at(conn, "gemini-2.5-flash", limits, t1, u1),
            None
        );
        rot.note_model_tokens_if_tracked_at(conn, "gemini-2.5-flash", 1_200, t1, u1);
        assert!(matches!(
            rot.check_model_caps_at(
                conn,
                "gemini-2.5-flash",
                limits,
                t1 + Duration::from_secs(1),
                u1 + 1
            ),
            Some(ModelCapHit::Tpm {
                used: 1_200,
                limit: 1_000,
                ..
            })
        ));

        // After another 61 s TPM clears; a 3rd send reaches 3/3 RPD and parks until UTC midnight
        // (> 3600 s, proving the 1 h transient cap does not truncate daily parks).
        let t2 = t1 + Duration::from_secs(61);
        let u2 = u1 + 61;
        rot.note_model_send_at(conn, "gemini-2.5-flash", t2, u2);
        let hit = rot.check_model_caps_at(conn, "gemini-2.5-flash", limits, t2, u2);
        let expected_wait = seconds_until_utc_midnight(u2);
        assert!(expected_wait > 3_600, "test expects > 1 h left in day");
        assert_eq!(
            hit,
            Some(ModelCapHit::Rpd {
                used: 3,
                limit: 3,
                retry_after_secs: expected_wait,
            })
        );
        // Subsequent checks see the active daily park without re-evaluating counts.
        assert!(matches!(
            rot.check_model_caps_at(conn, "gemini-2.5-flash", limits, t2 + Duration::from_secs(3600), u2 + 3600),
            Some(ModelCapHit::DailyPark { retry_after_secs }) if retry_after_secs > 70_000
        ));

        // After UTC midnight, both the park and the day counters clear.
        let t_next = t2 + Duration::from_secs(expected_wait + 1);
        let u_next = u2 + expected_wait + 1;
        assert_eq!(
            rot.check_model_caps_at(conn, "gemini-2.5-flash", limits, t_next, u_next),
            None
        );
        assert_eq!(rot.model_day_count_at(conn, "gemini-2.5-flash", u_next), 0);
        assert_eq!(rot.model_day_tokens_at(conn, "gemini-2.5-flash", u_next), 0);
    }

    /// TPD exhaustion parks until UTC midnight, and untracked (paid) connections never
    /// allocate token counters via `note_model_tokens_if_tracked`.
    #[test]
    fn tpd_exhaustion_parks_until_midnight_and_untracked_models_stay_untouched() {
        let rot = FreeRotation::new();
        let free_conn = Uuid::new_v4();
        let paid_conn = Uuid::new_v4();
        let day_start = 1_767_312_000;
        let now_unix = day_start + 7_200;
        let t0 = Instant::now();

        rot.note_model_tokens_if_tracked_at(
            paid_conn,
            "llama-3.3-70b-versatile",
            500_000,
            t0,
            now_unix,
        );
        assert!(!rot.is_tracking_model(paid_conn, "llama-3.3-70b-versatile"));
        assert_eq!(
            rot.model_day_tokens_at(paid_conn, "llama-3.3-70b-versatile", now_unix),
            0
        );

        rot.note_model_send_at(free_conn, "llama-3.3-70b-versatile", t0, now_unix);
        rot.note_model_tokens_if_tracked_at(
            free_conn,
            "llama-3.3-70b-versatile",
            100_000,
            t0,
            now_unix,
        );
        let limits = ModelLimits {
            tpd: Some(100_000),
            ..Default::default()
        };
        assert_eq!(
            rot.check_model_caps_at(free_conn, "llama-3.3-70b-versatile", limits, t0, now_unix),
            Some(ModelCapHit::Tpd {
                used: 100_000,
                limit: 100_000,
                retry_after_secs: seconds_until_utc_midnight(now_unix),
            })
        );
    }

    /// Learned limits only tighten (never raise), normalize Gemini's `models/` prefix, and
    /// `seed_from_usage_at` rebuilds day counts once after boot without double-counting.
    #[test]
    fn learned_limits_only_tighten_and_usage_seed_is_idempotent() {
        let rot = FreeRotation::new();
        assert!(rot.learn_model_limit(
            "gemini",
            "models/gemini-2.5-flash",
            LimitKind::RequestsPerDay,
            15
        ));
        assert!(!rot.learn_model_limit(
            "gemini",
            "gemini-2.5-flash",
            LimitKind::RequestsPerDay,
            50
        ));
        assert!(rot.learn_model_limit("gemini", "gemini-2.5-flash", LimitKind::RequestsPerDay, 10));
        assert_eq!(
            rot.learned_model_limits("gemini", "models/gemini-2.5-flash")
                .rpd,
            Some(10)
        );
        let merged = ModelLimits::merge_tighter(
            ModelLimits {
                rpd: Some(20),
                tpm: Some(250_000),
                ..Default::default()
            },
            rot.learned_model_limits("gemini", "gemini-2.5-flash"),
        );
        assert_eq!(merged.rpd, Some(10));
        assert_eq!(merged.tpm, Some(250_000));

        let conn = Uuid::new_v4();
        let now_unix = 1_767_312_000 + 600;
        let rows = vec![(
            conn,
            "gemini".to_string(),
            "models/gemini-2.5-flash".to_string(),
            20,
            42_000,
        )];
        assert!(rot.needs_usage_seed());
        rot.seed_from_usage_at(&rows, now_unix);
        assert!(!rot.needs_usage_seed());
        // Calling a second time is a no-op (no double-count).
        rot.seed_from_usage_at(&rows, now_unix);
        assert_eq!(rot.provider_day_count_at(conn, "gemini", now_unix), 20);
        assert_eq!(
            rot.model_day_count_at(conn, "gemini-2.5-flash", now_unix),
            20
        );
        assert_eq!(
            rot.model_day_tokens_at(conn, "gemini-2.5-flash", now_unix),
            42_000
        );
    }
}
