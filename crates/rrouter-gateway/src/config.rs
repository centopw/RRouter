//! Resolved runtime configuration: one `Config`, built from an optional declarative
//! file (`RROUTER_CONFIG`) and the environment, in that order of increasing precedence.
//!
//! There is exactly one schema: [`config_file::FileLayer`] is a thin JSON reader that
//! hands this module the same textual values the environment provides, so defaults,
//! ranges, clamping and error wording are shared rather than duplicated.

use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use rrouter_router::{CanaryRule, Strategy};
use serde::Serialize;

#[path = "config_file.rs"]
pub mod config_file;

use config_file::{EnvLookup, FileLayer};

/// Default listen port used by existing client configurations.
pub const DEFAULT_PORT: u16 = 20128;
pub const DEFAULT_HOST: &str = "127.0.0.1";
pub const DEFAULT_DATA_DIR: &str = "./data";
const DEFAULT_REQUEST_TIMEOUT_SECS: u64 = 600;
const DEFAULT_CONNECT_TIMEOUT_SECS: u64 = 15;
const DEFAULT_LOG: &str = "rrouter=info";
/// Hedge delay when `RROUTER_HEDGE_MS` is unset: off. A hedge spends a second account,
/// so it is opt-in per deployment.
const DEFAULT_HEDGE_MS: usize = 0;
/// Upper bound on `RROUTER_HEDGE_MS`: a hedge past a minute would be a fallback in its
/// own right, and the request timeout already bounds the client's wait.
const MAX_HEDGE_MS: usize = 60_000;
/// Upstream sends per client request when `RROUTER_MAX_ATTEMPTS` is unset.
const DEFAULT_MAX_ATTEMPTS: usize = 4;
/// Upper bound on `RROUTER_MAX_ATTEMPTS`: more sends than this per request turn a
/// provider outage into a slow client-visible failure for no diagnostic gain.
const MAX_ATTEMPTS_CEILING: usize = 8;
/// Extra tries per candidate when `RROUTER_RETRY_ATTEMPTS` is unset: one retry keeps a
/// transient 503 from spending a second account, which is the whole point.
const DEFAULT_RETRY_ATTEMPTS: usize = 1;
/// Upper bound on per-candidate retries. Together with `RROUTER_MAX_ATTEMPTS` this is
/// what stops an outage from turning one request into a retry storm.
const MAX_RETRY_ATTEMPTS: usize = 5;
/// First backoff when `RROUTER_RETRY_BASE_MS` is unset.
const DEFAULT_RETRY_BASE_MS: usize = 200;
/// Upper bound on the first backoff: a longer one would exceed the request timeout
/// before any fallback is tried.
const MAX_RETRY_BASE_MS: usize = 5_000;
/// Backoff ceiling when `RROUTER_RETRY_MAX_MS` is unset.
const DEFAULT_RETRY_MAX_MS: usize = 5_000;
/// Upper bound on the backoff ceiling.
const MAX_RETRY_MAX_MS: usize = 60_000;
const DEFAULT_SHUTDOWN_GRACE_SECS: u64 = 20;
/// Response cache lifetime when `RROUTER_CACHE_TTL_SECS` is unset.
const DEFAULT_CACHE_TTL_SECS: usize = 300;
/// Upper bound on a cache entry's life: a day is the longest an operator can keep
/// serving a response that a model/provider may since have changed.
const MAX_CACHE_TTL_SECS: usize = 86_400;
/// Cache rows kept when `RROUTER_CACHE_MAX_ENTRIES` is unset.
const DEFAULT_CACHE_MAX_ENTRIES: usize = 1_000;
/// Hard ceiling on cache rows: the whole point is a bounded amount of memory/disk.
const MAX_CACHE_ENTRIES: usize = 100_000;
/// Semantic-cache lifetime when `RROUTER_SEMANTIC_CACHE_TTL_SECS` is unset.
const DEFAULT_SEMANTIC_TTL_SECS: usize = 3_600;
/// Default cosine similarity a semantic hit must reach (0.92 = close paraphrase).
const DEFAULT_SEMANTIC_THRESHOLD: f64 = 0.92;
/// Subscription reserve when `RROUTER_SUBSCRIPTION_RESERVE_PERCENT` is unset. 90 leaves
/// one tenth of a plan window for ad-hoc traffic before an account is avoided.
const DEFAULT_SUBSCRIPTION_RESERVE_PERCENT: u8 = 90;

/// Days of `usage`/`attempts` history kept when `RROUTER_USAGE_RETENTION_DAYS` is
/// unset. `0` keeps everything, which is the pre-phase-14 behaviour.
const DEFAULT_USAGE_RETENTION_DAYS: usize = 0;
/// Upper bound on retention: ten years, well past any history a local gateway can use.
const MAX_USAGE_RETENTION_DAYS: usize = 3650;
/// The retention setting's env var, named once so parsing and tests agree.
const USAGE_RETENTION_VAR: &str = "RROUTER_USAGE_RETENTION_DAYS";

/// Where the exact-match response cache keeps its entries.
///
/// `Off` (the default) means the gateway never reads or writes one; `Sqlite` reuses the
/// gateway database, so a restart keeps cached responses and an operator can inspect
/// them with `sqlite3`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CacheMode {
    #[default]
    Off,
    Sqlite,
}

impl CacheMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Sqlite => "sqlite",
        }
    }

    /// Case-insensitive, trimmed. `None` for an unknown name.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "off" => Some(Self::Off),
            "sqlite" => Some(Self::Sqlite),
            _ => None,
        }
    }
}

/// Shape of every log line. `Text` (default) suits a terminal; `Json` suits Loki or
/// CloudWatch, where one object per line is queryable by field.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LogFormat {
    #[default]
    Text,
    Json,
}

impl LogFormat {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Json => "json",
        }
    }

    /// Case-insensitive, trimmed. `None` for an unknown name.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "text" => Some(Self::Text),
            "json" => Some(Self::Json),
            _ => None,
        }
    }
}

/// Everything the binary needs, resolved and validated once.
#[derive(Debug, Clone)]
pub struct Config {
    pub host: IpAddr,
    pub port: u16,
    pub data_dir: PathBuf,
    /// Hex-encoded 32-byte AES key; `None` means `<data_dir>/.key`.
    pub master_key: Option<String>,
    pub require_api_key: bool,
    pub request_timeout_secs: u64,
    pub connect_timeout_secs: u64,
    pub log_filter: String,
    /// Extra static files served under `/ext/*` (enterprise overlay assets).
    pub static_dir: Option<PathBuf>,
    /// Allowed CORS origins. Empty means loopback origins only; `*` allows any.
    pub cors_origins: Vec<String>,
    /// When false (default) the admin API rejects non-loopback peers.
    pub allow_remote_admin: bool,
    /// Bearer token the admin API (and `/metrics`) requires. `None` keeps the
    /// loopback rule; `Some` replaces it, so every admin request must present it.
    pub admin_token: Option<String>,
    /// Maximum concurrent `/v1/*` proxy requests; `0` (default) is unlimited.
    pub max_concurrency: usize,
    /// Upstream sends per client request, clamped to `1..=8` (default 4).
    pub max_attempts: usize,
    /// Extra tries per provider before falling back, clamped to `0..=5` (default 1).
    pub retry_attempts: usize,
    /// First retry backoff in milliseconds, clamped to `0..=5000` (default 200).
    pub retry_base_ms: u64,
    /// Retry backoff ceiling in milliseconds, clamped to `0..=60000` (default 5000).
    pub retry_max_ms: u64,
    /// Seconds graceful shutdown may spend draining live streams; `0` waits forever.
    pub shutdown_grace_secs: u64,
    /// Ordering of equal-priority connections. Priority itself always dominates.
    pub strategy: Strategy,
    /// Exact-match response cache backend; `Off` by default.
    pub cache: CacheMode,
    /// Seconds a cached response stays servable, clamped to `1..=86400`.
    pub cache_ttl_secs: u64,
    /// Cached responses kept, clamped to `1..=100000`; oldest evicted first.
    pub cache_max_entries: usize,
    /// Days of `usage`/`attempts` history to keep, clamped to `0..=3650`. `0` keeps
    /// everything; expired `response_cache` rows are swept either way.
    pub usage_retention_days: usize,
    /// Shape of every log line: `text` (default) or `json`.
    pub log_format: LogFormat,
    /// Emit one `request` info line per finished request. `false` silences only that line.
    pub access_log: bool,
    /// Hedge delay for non-streaming requests; `0` (default) keeps sequential fallback.
    /// After this long without an answer, the next eligible candidate of the same
    /// priority starts in parallel. Restart-only: the dispatcher is built at startup.
    pub hedge_ms: u64,
    /// Operator A/B rule (`RROUTER_CANARY`), or `None`. Reloadable: it is read per plan.
    pub canary: Option<CanaryRule>,
    /// Subscription reserve (`RROUTER_SUBSCRIPTION_RESERVE_PERCENT`), `0..=100`, default 90.
    /// A provider account at or above this percentage of any plan window is skipped while
    /// at least one healthier candidate remains; `0` disables subscription-aware filtering
    /// and ordering. Reloadable: it is read per plan.
    pub subscription_reserve_percent: u8,
    /// Semantic response cache backend (`RROUTER_SEMANTIC_CACHE`); `Off` by default.
    /// Reloadable: every read consults the snapshot.
    pub semantic_cache: CacheMode,
    /// Minimum cosine similarity for a semantic hit, `0..=1` (`RROUTER_SEMANTIC_CACHE_THRESHOLD`).
    pub semantic_cache_threshold: f64,
    /// Embedding model as a `provider/model` ref; required when the cache is on.
    pub semantic_embedding_model: Option<String>,
    /// Semantic entry lifetime in seconds, clamped `1..=86400`.
    pub semantic_cache_ttl_secs: u64,
    /// Semantic rows kept, clamped `1..=100000`; oldest evicted first.
    pub semantic_cache_max_entries: usize,
}

impl Config {
    /// The `Access-Control-Allow-Origin` value to echo, if any.
    ///
    /// There is deliberately no wildcard: a `*` grant would let any web origin read a
    /// loopback-bound, unauthenticated admin API. `RROUTER_CORS_ORIGINS=*` is rejected
    /// at parse time; see [`parse_env`].
    pub fn cors_origin_for(&self, origin: &str) -> Option<String> {
        if self.cors_origins.iter().any(|o| o == origin) {
            return Some(origin.to_string());
        }
        // Loopback tooling (curl, local dev servers) always works.
        let host = origin
            .split("://")
            .nth(1)
            .and_then(|rest| rest.split([':', '/']).next())
            .unwrap_or("");
        matches!(host, "localhost" | "127.0.0.1" | "[::1]").then(|| origin.to_string())
    }
}

/// Where an effective setting came from. Precedence is `Env` > `File` > `Default`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    Default,
    File,
    Env,
}

/// One setting's value before parsing, carrying the label its error message must use so
/// a bad file value is never reported as an environment variable (or vice versa).
#[derive(Debug, Clone)]
struct Raw {
    value: String,
    origin: Origin,
    /// `RROUTER_PORT` or `config file /etc/rrouter.json: field "port"`.
    head: String,
}

impl Raw {
    fn env(name: &str, value: String) -> Self {
        Self {
            value,
            origin: Origin::Env,
            head: name.to_string(),
        }
    }

    fn file(path: &Path, field: &str, value: &str) -> Self {
        Self {
            value: value.to_string(),
            origin: Origin::File,
            head: format!("config file {}: field {field:?}", path.display()),
        }
    }

    /// What an operator reads: `RROUTER_PORT="x"` or
    /// `config file /p: field "port" = "x"`. The environment spelling stays exactly what
    /// it always was, so existing messages and greps keep working.
    fn describe(&self) -> String {
        match self.origin {
            Origin::Env => format!("{}={:?}", self.head, self.value),
            _ => format!("{} = {:?}", self.head, self.value),
        }
    }

    fn head(&self) -> &str {
        &self.head
    }
}

/// Provenance for every setting, printed by `--print-config` and summarised by
/// `--check-config`.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(transparent)]
pub struct SourceMap(BTreeMap<&'static str, Origin>);

impl SourceMap {
    pub fn origin(&self, field: &str) -> Option<Origin> {
        self.0.get(field).copied()
    }

    /// `(env, file, default)` counts.
    pub fn counts(&self) -> (usize, usize, usize) {
        let mut counts = (0, 0, 0);
        for origin in self.0.values() {
            match origin {
                Origin::Env => counts.0 += 1,
                Origin::File => counts.1 += 1,
                Origin::Default => counts.2 += 1,
            }
        }
        counts
    }
}

/// The resolved configuration plus where each value came from.
#[derive(Debug, Clone)]
pub struct Loaded {
    pub config: Config,
    pub sources: SourceMap,
    pub config_file: Option<PathBuf>,
    /// Resolved OTLP exporter settings; present only when the `otel` feature is built
    /// and `RROUTER_OTEL_ENDPOINT` (or `otel_endpoint` in the config file) is set.
    #[cfg(feature = "otel")]
    pub otel: Option<OtelConfig>,
}

/// Optional OTLP/HTTP span export (`otel` feature, default off).
///
/// The endpoint is the **full** traces URL (for example
/// `http://collector:4318/v1/traces`); nothing is guessed. `sample_ratio` is a trace
/// ratio in `0.0..=1.0` applied only to the local root decision — an inbound
/// `traceparent`'s sampling flag wins, so a sampled client trace is never dropped by a
/// lower local ratio. Values are validated at startup: an invalid setting stops the
/// process instead of exporting nowhere. Read from the environment only — the config
/// file schema rejects unknown keys, so `otel_endpoint` there is an error, not silently
/// ignored.
#[cfg(feature = "otel")]
#[derive(Debug, Clone, PartialEq)]
pub struct OtelConfig {
    pub endpoint: String,
    pub service_name: String,
    pub sample_ratio: f64,
}

#[cfg(feature = "otel")]
impl OtelConfig {
    pub const DEFAULT_SERVICE_NAME: &'static str = "rrouter";
    pub const DEFAULT_SAMPLE_RATIO: f64 = 1.0;

    /// Pure validation shared by startup and tests.
    pub fn validate(endpoint: &str, service_name: &str, sample_ratio: f64) -> Result<Self, String> {
        let endpoint = endpoint.trim();
        if endpoint.is_empty() {
            return Err("otel endpoint is empty".into());
        }
        if endpoint.len() > 2048 {
            return Err("otel endpoint is longer than 2048 bytes".into());
        }
        if endpoint.chars().any(char::is_whitespace) {
            return Err("otel endpoint must not contain whitespace".into());
        }
        let Some((scheme, rest)) = endpoint.split_once("://") else {
            return Err("otel endpoint must be an http(s) URL".into());
        };
        if !matches!(scheme, "http" | "https") {
            return Err(format!(
                "otel endpoint scheme {scheme:?} is not http or https"
            ));
        }
        if rest.is_empty() || rest.starts_with('/') {
            return Err("otel endpoint has no host".into());
        }
        let service_name = service_name.trim();
        if service_name.is_empty() {
            return Err("otel service name is empty".into());
        }
        if service_name.len() > 128 {
            return Err("otel service name is longer than 128 bytes".into());
        }
        if service_name.chars().any(|c| c.is_control()) {
            return Err("otel service name must not contain control characters".into());
        }
        if !sample_ratio.is_finite() || !(0.0..=1.0).contains(&sample_ratio) {
            return Err(format!(
                "otel sample ratio {sample_ratio} is outside 0.0..=1.0"
            ));
        }
        Ok(Self {
            endpoint: endpoint.to_string(),
            service_name: service_name.to_string(),
            sample_ratio,
        })
    }
}

/// Read the OTel settings from one `Source` (environment first, then the config file),
/// recording origin-aware problems. `None` means the exporter is disabled.
#[cfg(feature = "otel")]
fn otel_setting(source: &mut Source, problems: &mut Vec<String>) -> Option<OtelConfig> {
    // Disabled means absent: do not add `otel_endpoint` to the source map as a default,
    // which would make the setting look tracked while nothing is exported.
    if !source.present("otel_endpoint", "RROUTER_OTEL_ENDPOINT") {
        return None;
    }
    let endpoint = source.raw("otel_endpoint", "RROUTER_OTEL_ENDPOINT")?;
    let service_name = source
        .raw("otel_service_name", "RROUTER_OTEL_SERVICE_NAME")
        .map_or_else(
            || OtelConfig::DEFAULT_SERVICE_NAME.to_string(),
            |raw| raw.value,
        );
    let ratio = match source.raw("otel_sample_ratio", "RROUTER_OTEL_SAMPLE_RATIO") {
        None => OtelConfig::DEFAULT_SAMPLE_RATIO,
        Some(raw) => raw.value.parse::<f64>().unwrap_or_else(|_| {
            problems.push(format!(
                "{} is not a ratio between 0.0 and 1.0",
                raw.describe()
            ));
            OtelConfig::DEFAULT_SAMPLE_RATIO
        }),
    };
    match OtelConfig::validate(&endpoint.value, &service_name, ratio) {
        Ok(config) => Some(config),
        Err(message) => {
            problems.push(format!("{}: {message}", endpoint.describe()));
            None
        }
    }
}

impl Loaded {
    /// Secrets-free view for `--print-config`, with per-field provenance.
    pub fn redacted(&self) -> Redacted<'_> {
        Redacted {
            config: &self.config,
            sources: Some(&self.sources),
            config_file: self.config_file.as_deref(),
        }
    }

    /// What adopting this configuration would do to a process running `old`.
    pub fn plan_against(&self, old: &Config) -> ReloadPlan {
        ReloadPlan::between(old, &self.config)
    }
}

/// A configuration that can be replaced while the gateway runs.
///
/// A reader takes one [`Arc`] snapshot per request ([`ConfigSnapshot::load`]), so a reload
/// can never leave a request reading half of the old and half of the new configuration.
#[derive(Debug)]
pub struct ConfigSnapshot(RwLock<Arc<Config>>);

impl ConfigSnapshot {
    pub fn new(config: Config) -> Self {
        Self(RwLock::new(Arc::new(config)))
    }

    /// The configuration in effect right now. Cheap: one read lock plus an `Arc` clone.
    pub fn load(&self) -> Arc<Config> {
        let guard = self
            .0
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        Arc::clone(&guard)
    }

    /// Replace the snapshot, returning the previous one.
    pub fn store(&self, config: Arc<Config>) -> Arc<Config> {
        std::mem::replace(
            &mut *self
                .0
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
            config,
        )
    }

    /// Compute and apply a reload against `candidate`.
    ///
    /// Only [`ReloadPlan::changed`] fields move; restart-only and rejected fields keep their
    /// live value, and the swap happens under one write lock, so a reader sees either the
    /// whole old configuration or the whole new one.
    pub fn apply(&self, candidate: &Config) -> ReloadOutcome {
        let current = self.load();
        let plan = ReloadPlan::between(&current, candidate);
        if plan.changed.is_empty() {
            return ReloadOutcome {
                plan,
                config: current,
                applied: false,
            };
        }
        let next = Arc::new(current.reloaded(candidate, &plan));
        let _previous = self.store(Arc::clone(&next));
        ReloadOutcome {
            plan,
            config: next,
            applied: true,
        }
    }
}

impl From<Config> for ConfigSnapshot {
    fn from(config: Config) -> Self {
        Self::new(config)
    }
}

/// What a reload attempt decided and did. Safe to log: a plan carries no secret values.
#[derive(Debug, Clone)]
pub struct ReloadOutcome {
    pub plan: ReloadPlan,
    /// The snapshot now in effect (the previous one when `applied` is false).
    pub config: Arc<Config>,
    pub applied: bool,
}

impl ReloadOutcome {
    /// Fields the caller must push into subsystems it owns (the tracing subscriber).
    pub fn changed_fields(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.plan.changed_fields()
    }
}

impl Config {
    /// Apply the plan's changes from `candidate` onto `self`.
    ///
    /// Every other field — restart-only, rejected, or unchanged — keeps its live value, so
    /// a partial reload can never introduce a value the plan refused.
    pub fn reloaded(&self, candidate: &Config, plan: &ReloadPlan) -> Config {
        let mut next = self.clone();
        for change in &plan.changed {
            match change.field {
                "admin_token" => next.admin_token.clone_from(&candidate.admin_token),
                "allow_remote_admin" => next.allow_remote_admin = candidate.allow_remote_admin,
                "access_log" => next.access_log = candidate.access_log,
                "canary" => next.canary.clone_from(&candidate.canary),
                "subscription_reserve_percent" => {
                    next.subscription_reserve_percent = candidate.subscription_reserve_percent;
                }
                "semantic_cache" => next.semantic_cache = candidate.semantic_cache,
                "semantic_cache_threshold" => {
                    next.semantic_cache_threshold = candidate.semantic_cache_threshold;
                }
                "semantic_embedding_model" => next
                    .semantic_embedding_model
                    .clone_from(&candidate.semantic_embedding_model),
                "semantic_cache_ttl_secs" => {
                    next.semantic_cache_ttl_secs = candidate.semantic_cache_ttl_secs;
                }
                "semantic_cache_max_entries" => {
                    next.semantic_cache_max_entries = candidate.semantic_cache_max_entries;
                }
                "cache" => next.cache = candidate.cache,
                "cache_max_entries" => next.cache_max_entries = candidate.cache_max_entries,
                "cache_ttl_secs" => next.cache_ttl_secs = candidate.cache_ttl_secs,
                "cors_origins" => next.cors_origins.clone_from(&candidate.cors_origins),
                "log_filter" => next.log_filter.clone_from(&candidate.log_filter),
                "log_format" => next.log_format = candidate.log_format,
                "max_concurrency" => next.max_concurrency = candidate.max_concurrency,
                "request_timeout_secs" => {
                    next.request_timeout_secs = candidate.request_timeout_secs;
                }
                "require_api_key" => next.require_api_key = candidate.require_api_key,
                "strategy" => next.strategy = candidate.strategy,
                "usage_retention_days" => {
                    next.usage_retention_days = candidate.usage_retention_days;
                }
                // `ReloadPlan::between` emits only the names above in `changed`; anything
                // else is a plan bug, not a value to apply.
                _ => {}
            }
        }
        next
    }
}

/// One setting a reload can adopt now. Secret material is reduced to its presence
/// (`set`/`unset`/`rotated`); the value never appears in a plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Changed {
    pub field: &'static str,
    pub from: String,
    pub to: String,
}

/// One setting that differs but cannot apply to the running process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RestartRequired {
    pub field: &'static str,
    pub reason: &'static str,
}

/// One setting the planner refused. The running configuration keeps its old value; the
/// field is deliberately absent from [`ReloadPlan::changed`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Rejected {
    pub field: &'static str,
    pub reason: &'static str,
}

/// What a reload would do, computed without touching the running process.
///
/// The contract for Wave 3B: apply exactly [`ReloadPlan::changed`]; report
/// `restart_required` and `rejected` to the operator and apply **nothing** from either.
/// An empty plan means the new file still describes the running configuration.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ReloadPlan {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub changed: Vec<Changed>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub restart_required: Vec<RestartRequired>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub rejected: Vec<Rejected>,
}

impl ReloadPlan {
    /// Compare two resolved configurations, field by field, in a fixed order.
    ///
    /// Pure: nothing here reads the environment or the file system, and no field can
    /// move between the three lists because of an unrelated failure.
    pub fn between(old: &Config, new: &Config) -> Self {
        let mut plan = Self::default();

        // Startup-only: a reload can never rebind a listener or relocate the data dir.
        for (field, differs) in [
            ("host", old.host != new.host),
            ("port", old.port != new.port),
            ("data_dir", old.data_dir != new.data_dir),
        ] {
            if differs {
                plan.restart_required.push(RestartRequired {
                    field,
                    reason: "fixed when the process starts; restart to apply",
                });
            }
        }

        // Key material. The master key is bound into the store's cipher when the store
        // opens, so rotating it means decrypting and re-encrypting every credential — a
        // migration, not a reload. The admin token is compared per admin request and has
        // no stored state, so it can be swapped (dashboard sessions are JWTs and do not
        // depend on it).
        if old.master_key != new.master_key {
            plan.restart_required.push(RestartRequired {
                field: "master_key",
                reason: "the store's cipher is bound at open; rotation needs re-encryption",
            });
        }
        if old.admin_token != new.admin_token {
            let (from, to) =
                secret_transition(old.admin_token.is_some(), new.admin_token.is_some());
            push_changed(&mut plan, "admin_token", from, to);
        }

        // A reload must uphold the startup invariant that the admin API is never
        // silently unreachable. `serve()` refuses a non-loopback bind without
        // `allow_remote_admin`; adopting that value live would do the same damage quietly.
        if old.allow_remote_admin != new.allow_remote_admin {
            if !new.allow_remote_admin && !new.host.is_loopback() {
                plan.rejected.push(Rejected {
                    field: "allow_remote_admin",
                    reason: "would leave the admin API unreachable on a non-loopback bind; \
                             a restart with this value is refused too",
                });
            } else {
                push_changed(
                    &mut plan,
                    "allow_remote_admin",
                    old.allow_remote_admin.to_string(),
                    new.allow_remote_admin.to_string(),
                );
            }
        }

        // `static_dir` is served per request, but v1 does not promise a live asset-root
        // swap: restarting is cheap and the failure mode (half-served overlay) is not.
        if old.static_dir != new.static_dir {
            plan.restart_required.push(RestartRequired {
                field: "static_dir",
                reason: "not in the v1 reloadable set; restart to apply",
            });
        }

        if old.require_api_key != new.require_api_key {
            push_changed(
                &mut plan,
                "require_api_key",
                old.require_api_key.to_string(),
                new.require_api_key.to_string(),
            );
        }
        if old.strategy != new.strategy {
            push_changed(
                &mut plan,
                "strategy",
                old.strategy.as_str(),
                new.strategy.as_str(),
            );
        }
        if old.cache != new.cache {
            push_changed(&mut plan, "cache", old.cache.as_str(), new.cache.as_str());
        }
        if old.cache_ttl_secs != new.cache_ttl_secs {
            push_changed(
                &mut plan,
                "cache_ttl_secs",
                old.cache_ttl_secs.to_string(),
                new.cache_ttl_secs.to_string(),
            );
        }
        if old.cache_max_entries != new.cache_max_entries {
            push_changed(
                &mut plan,
                "cache_max_entries",
                old.cache_max_entries.to_string(),
                new.cache_max_entries.to_string(),
            );
        }
        // Retry policy and the HTTP client are built once from the startup config, and the
        // breaker that owns failure state lives inside the dispatcher. Swapping either
        // mid-flight would discard state or silently keep using the old values, so these
        // are restart-only rather than "applied but ineffective".
        for (field, differs) in [
            ("max_attempts", old.max_attempts != new.max_attempts),
            ("retry_attempts", old.retry_attempts != new.retry_attempts),
            ("retry_base_ms", old.retry_base_ms != new.retry_base_ms),
            ("retry_max_ms", old.retry_max_ms != new.retry_max_ms),
            (
                "connect_timeout_secs",
                old.connect_timeout_secs != new.connect_timeout_secs,
            ),
            ("hedge_ms", old.hedge_ms != new.hedge_ms),
        ] {
            if differs {
                plan.restart_required.push(RestartRequired {
                    field,
                    reason: "built at startup (dispatcher / HTTP client); restart to apply",
                });
            }
        }
        if old.max_concurrency != new.max_concurrency {
            push_changed(
                &mut plan,
                "max_concurrency",
                old.max_concurrency.to_string(),
                new.max_concurrency.to_string(),
            );
        }
        if old.request_timeout_secs != new.request_timeout_secs {
            push_changed(
                &mut plan,
                "request_timeout_secs",
                old.request_timeout_secs.to_string(),
                new.request_timeout_secs.to_string(),
            );
        }
        if old.log_filter != new.log_filter {
            push_changed(
                &mut plan,
                "log_filter",
                old.log_filter.clone(),
                new.log_filter.clone(),
            );
        }
        if old.log_format != new.log_format {
            push_changed(
                &mut plan,
                "log_format",
                old.log_format.as_str(),
                new.log_format.as_str(),
            );
        }
        if old.access_log != new.access_log {
            push_changed(
                &mut plan,
                "access_log",
                old.access_log.to_string(),
                new.access_log.to_string(),
            );
        }
        if old.subscription_reserve_percent != new.subscription_reserve_percent {
            push_changed(
                &mut plan,
                "subscription_reserve_percent",
                old.subscription_reserve_percent.to_string(),
                new.subscription_reserve_percent.to_string(),
            );
        }
        if old.semantic_cache != new.semantic_cache {
            push_changed(
                &mut plan,
                "semantic_cache",
                old.semantic_cache.as_str(),
                new.semantic_cache.as_str(),
            );
        }
        if old.semantic_cache_threshold.to_bits() != new.semantic_cache_threshold.to_bits() {
            push_changed(
                &mut plan,
                "semantic_cache_threshold",
                old.semantic_cache_threshold.to_string(),
                new.semantic_cache_threshold.to_string(),
            );
        }
        if old.semantic_embedding_model != new.semantic_embedding_model {
            push_changed(
                &mut plan,
                "semantic_embedding_model",
                old.semantic_embedding_model.as_deref().unwrap_or_default(),
                new.semantic_embedding_model.as_deref().unwrap_or_default(),
            );
        }
        if old.semantic_cache_ttl_secs != new.semantic_cache_ttl_secs {
            push_changed(
                &mut plan,
                "semantic_cache_ttl_secs",
                old.semantic_cache_ttl_secs.to_string(),
                new.semantic_cache_ttl_secs.to_string(),
            );
        }
        if old.semantic_cache_max_entries != new.semantic_cache_max_entries {
            push_changed(
                &mut plan,
                "semantic_cache_max_entries",
                old.semantic_cache_max_entries.to_string(),
                new.semantic_cache_max_entries.to_string(),
            );
        }
        if old.canary != new.canary {
            push_changed(
                &mut plan,
                "canary",
                old.canary
                    .as_ref()
                    .map(CanaryRule::as_wire)
                    .unwrap_or_default(),
                new.canary
                    .as_ref()
                    .map(CanaryRule::as_wire)
                    .unwrap_or_default(),
            );
        }
        if old.usage_retention_days != new.usage_retention_days {
            push_changed(
                &mut plan,
                "usage_retention_days",
                old.usage_retention_days.to_string(),
                new.usage_retention_days.to_string(),
            );
        }
        if old.cors_origins != new.cors_origins {
            push_changed(
                &mut plan,
                "cors_origins",
                old.cors_origins.join(","),
                new.cors_origins.join(","),
            );
        }

        plan
    }

    /// Nothing differs: adopting this file would be a no-op.
    pub fn is_empty(&self) -> bool {
        self.changed.is_empty() && self.restart_required.is_empty() && self.rejected.is_empty()
    }

    /// Fields a Wave 3B swap must apply, in plan order.
    pub fn changed_fields(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.changed.iter().map(|change| change.field)
    }
}

/// Record one changed setting.
fn push_changed(
    plan: &mut ReloadPlan,
    field: &'static str,
    from: impl Into<String>,
    to: impl Into<String>,
) {
    plan.changed.push(Changed {
        field,
        from: from.into(),
        to: to.into(),
    });
}

/// How a secret changed, without the secret: `set`/`unset`/`rotated`.
fn secret_transition(old_set: bool, new_set: bool) -> (&'static str, &'static str) {
    match (old_set, new_set) {
        (false, true) => ("unset", "set"),
        (true, false) => ("set", "unset"),
        (true, true) => ("set", "rotated"),
        // Callers only reach here when the values differ, so both-unset cannot happen.
        (false, false) => ("unset", "unset"),
    }
}

/// Precedence lookup: environment first, then the file, then the caller's default.
struct Source<'a> {
    env: EnvLookup<'a>,
    file: Option<&'a FileLayer>,
    sources: SourceMap,
}

impl Source<'_> {
    fn raw(&mut self, field: &'static str, env_name: &str) -> Option<Raw> {
        self.pick(field, env_name, true)
    }

    /// Like [`Self::raw`], but an empty value counts as unset. That is the historical
    /// rule for secrets and paths (`RROUTER_ADMIN_TOKEN=` must not enable auth), and it
    /// lets a file value apply when the environment only exports an empty variable.
    fn raw_set(&mut self, field: &'static str, env_name: &str) -> Option<Raw> {
        self.pick(field, env_name, false)
    }

    /// Whether a setting is present at all, without recording provenance. Used by
    /// feature-gated settings (OTel) so a disabled feature leaves the source map exactly
    /// as it was.
    #[cfg_attr(not(feature = "otel"), allow(dead_code))]
    fn present(&self, field: &str, env_name: &str) -> bool {
        let usable = |value: String| !value.is_empty();
        (self.env)(env_name).is_some_and(usable)
            || self
                .file
                .and_then(|file| file.get(field))
                .is_some_and(|value| usable(value.to_string()))
    }

    fn pick(&mut self, field: &'static str, env_name: &str, allow_empty: bool) -> Option<Raw> {
        let usable = |value: String| {
            if allow_empty || !value.is_empty() {
                Some(value)
            } else {
                None
            }
        };
        if let Some(value) = (self.env)(env_name).and_then(usable) {
            self.sources.0.insert(field, Origin::Env);
            return Some(Raw::env(env_name, value));
        }
        if let Some(file) = self.file {
            if let Some(value) = file.get(field).and_then(|v| usable(v.to_string())) {
                self.sources.0.insert(field, Origin::File);
                return Some(Raw::file(&file.path, field, &value));
            }
        }
        self.sources.0.insert(field, Origin::Default);
        None
    }
}

impl Config {
    /// Read `RROUTER_CONFIG` (when set) and the environment, the environment winning.
    /// Returns an error message instead of panicking so `--check-config`,
    /// `--print-config` and the binary can all report it cleanly.
    pub fn from_env() -> Result<Self, String> {
        Self::load_env().map(|loaded| loaded.config)
    }

    /// Resolve the file and the environment, keeping provenance for the CLI.
    pub fn load_env() -> Result<Loaded, String> {
        let lookup = |name: &str| var(name);
        let path = var("RROUTER_CONFIG").filter(|path| !path.is_empty());
        let file = match &path {
            Some(path) => Some(FileLayer::load(Path::new(path), &lookup)?),
            None => None,
        };
        Self::resolve(&lookup, file)
    }

    /// Load a *candidate* configuration for a reload: exactly `path`, merged with the
    /// supplied environment using the startup precedence (environment wins).
    ///
    /// Pure and self-contained: it reads that file and the closure only. It never looks
    /// at `RROUTER_CONFIG`, never mutates the process environment, and returns an error
    /// without side effects, so a dry run cannot disturb the running gateway.
    pub fn load_reloadable(path: &Path, env: EnvLookup<'_>) -> Result<Loaded, String> {
        Self::resolve(env, Some(FileLayer::load(path, env)?))
    }

    /// [`Self::load_reloadable`] against the process environment — the Wave 3B entry
    /// point, sharing `var` with startup so trimming and precedence cannot drift.
    pub fn load_reloadable_env(path: &Path) -> Result<Loaded, String> {
        let lookup = |name: &str| var(name);
        Self::load_reloadable(path, &lookup)
    }

    /// Pure resolver: `env` is the environment surface, `file` the parsed layer.
    ///
    /// Each setting is looked up once, validated once, and recorded with its origin, so
    /// there is no second schema whose defaults could drift from the environment's.
    pub fn resolve(env: EnvLookup<'_>, file: Option<FileLayer>) -> Result<Loaded, String> {
        let mut problems: Vec<String> = Vec::new();
        let mut source = Source {
            env,
            file: file.as_ref(),
            sources: SourceMap::default(),
        };

        let port = match source.raw("port", "RROUTER_PORT") {
            None => DEFAULT_PORT,
            Some(raw) => raw.value.parse::<u16>().unwrap_or_else(|_| {
                problems.push(format!("{} is not a port number", raw.describe()));
                0
            }),
        };
        let host = match source.raw("host", "RROUTER_HOST") {
            None => DEFAULT_HOST
                .parse()
                .unwrap_or(std::net::Ipv4Addr::LOCALHOST.into()),
            Some(raw) => raw.value.parse::<IpAddr>().unwrap_or_else(|_| {
                problems.push(format!("{} is not an IP address", raw.describe()));
                std::net::Ipv4Addr::LOCALHOST.into()
            }),
        };
        let data_dir = source.raw("data_dir", "RROUTER_DATA_DIR").map_or_else(
            || PathBuf::from(DEFAULT_DATA_DIR),
            |raw| PathBuf::from(raw.value),
        );
        let master_key = source
            .raw_set("master_key", "RROUTER_MASTER_KEY")
            .map(|raw| {
                if raw.value.len() != 64 || !raw.value.bytes().all(|b| b.is_ascii_hexdigit()) {
                    problems.push(format!(
                        "{} must be 64 hex characters (32 bytes)",
                        raw.head()
                    ));
                }
                raw.value
            });
        let require_api_key = bool_var(
            source.raw("require_api_key", "RROUTER_REQUIRE_API_KEY"),
            false,
            &mut problems,
        );
        let request_timeout_secs = secs_var(
            source.raw("request_timeout_secs", "RROUTER_REQUEST_TIMEOUT_SECS"),
            DEFAULT_REQUEST_TIMEOUT_SECS,
            &mut problems,
        );
        let connect_timeout_secs = secs_var(
            source.raw("connect_timeout_secs", "RROUTER_CONNECT_TIMEOUT_SECS"),
            DEFAULT_CONNECT_TIMEOUT_SECS,
            &mut problems,
        );
        let log_filter = source
            .raw("log_filter", "RUST_LOG")
            .map_or_else(|| DEFAULT_LOG.to_string(), |raw| raw.value);
        let static_dir = source
            .raw_set("static_dir", "RROUTER_STATIC_DIR")
            .map(|raw| PathBuf::from(raw.value));
        let cors_origins: Vec<String> = match source.raw("cors_origins", "RROUTER_CORS_ORIGINS") {
            None => Vec::new(),
            Some(raw) => {
                let origins: Vec<String> = raw
                    .value
                    .split(',')
                    .map(|origin| origin.trim().trim_end_matches('/').to_string())
                    .filter(|origin| !origin.is_empty())
                    .collect();
                // A wildcard would expose the (unauthenticated by default, loopback-bound)
                // admin API to every web origin. Refuse it loudly, whichever source asked.
                if origins.iter().any(|origin| origin == "*") {
                    problems.push(format!(
                        "{}: \"*\" is not allowed; list explicit origins instead",
                        raw.head()
                    ));
                }
                origins
            }
        };
        let allow_remote_admin = bool_var(
            source.raw("allow_remote_admin", "RROUTER_ALLOW_REMOTE_ADMIN"),
            false,
            &mut problems,
        );
        // An admin token replaces the loopback rule rather than adding to it: behind a
        // reverse proxy the peer is always the proxy, so loopback proves nothing.
        let admin_token = source
            .raw_set("admin_token", "RROUTER_ADMIN_TOKEN")
            .map(|raw| raw.value);
        let max_concurrency = usize_var(
            source.raw("max_concurrency", "RROUTER_MAX_CONCURRENCY"),
            0,
            &mut problems,
        );
        let max_attempts = clamp_attempts(usize_var(
            source.raw("max_attempts", "RROUTER_MAX_ATTEMPTS"),
            DEFAULT_MAX_ATTEMPTS,
            &mut problems,
        ));
        // Retries are clamped, never refused: an operator writing 99 wants "retry hard",
        // and garbage still stops startup like every other numeric knob.
        let retry_attempts = clamped_var(
            source.raw("retry_attempts", "RROUTER_RETRY_ATTEMPTS"),
            DEFAULT_RETRY_ATTEMPTS,
            0,
            MAX_RETRY_ATTEMPTS,
            &mut problems,
        );
        let retry_base_ms = clamped_var(
            source.raw("retry_base_ms", "RROUTER_RETRY_BASE_MS"),
            DEFAULT_RETRY_BASE_MS,
            0,
            MAX_RETRY_BASE_MS,
            &mut problems,
        ) as u64;
        let retry_max_ms = clamped_var(
            source.raw("retry_max_ms", "RROUTER_RETRY_MAX_MS"),
            DEFAULT_RETRY_MAX_MS,
            0,
            MAX_RETRY_MAX_MS,
            &mut problems,
        ) as u64;
        let shutdown_grace_secs = secs_var_allow_zero(
            source.raw("shutdown_grace_secs", "RROUTER_SHUTDOWN_GRACE_SECS"),
            DEFAULT_SHUTDOWN_GRACE_SECS,
            &mut problems,
        );
        // An unknown strategy is refused at startup rather than silently treated as
        // `priority`: the operator asked for different routing and must know it did not
        // take effect.
        let strategy = strategy_setting(source.raw("strategy", "RROUTER_STRATEGY"), &mut problems);

        // An unknown cache backend is refused for the same reason: silently staying off
        // would look like the cache is working.
        let cache = match source.raw("cache", "RROUTER_CACHE") {
            None => CacheMode::Off,
            Some(raw) => CacheMode::parse(&raw.value).unwrap_or_else(|| {
                problems.push(format!("{} is not one of off, sqlite", raw.describe()));
                CacheMode::Off
            }),
        };
        let cache_ttl_secs = clamp_cache_ttl(clamped_var(
            source.raw("cache_ttl_secs", "RROUTER_CACHE_TTL_SECS"),
            DEFAULT_CACHE_TTL_SECS,
            1,
            MAX_CACHE_TTL_SECS,
            &mut problems,
        )) as u64;
        let cache_max_entries = clamp_cache_entries(clamped_var(
            source.raw("cache_max_entries", "RROUTER_CACHE_MAX_ENTRIES"),
            DEFAULT_CACHE_MAX_ENTRIES,
            1,
            MAX_CACHE_ENTRIES,
            &mut problems,
        ));
        let log_format = log_format_setting(
            source.raw("log_format", "RROUTER_LOG_FORMAT"),
            &mut problems,
        );
        let access_log = access_log_setting(
            source.raw("access_log", "RROUTER_ACCESS_LOG"),
            &mut problems,
        );
        let hedge_ms = clamped_var(
            source.raw("hedge_ms", "RROUTER_HEDGE_MS"),
            DEFAULT_HEDGE_MS,
            0,
            MAX_HEDGE_MS,
            &mut problems,
        ) as u64;
        let canary = canary_setting(source.raw("canary", "RROUTER_CANARY"), &mut problems);
        // The semantic cache is opt-in twice: a backend *and* an embedding model. An
        // enabled cache with no model would have to silently embed with something the
        // operator never chose, so startup refuses it instead.
        let semantic_cache = match source.raw("semantic_cache", "RROUTER_SEMANTIC_CACHE") {
            None => CacheMode::Off,
            Some(raw) => CacheMode::parse(&raw.value).unwrap_or_else(|| {
                problems.push(format!("{} is not one of off, sqlite", raw.describe()));
                CacheMode::Off
            }),
        };
        let semantic_cache_threshold = threshold_setting(
            source.raw(
                "semantic_cache_threshold",
                "RROUTER_SEMANTIC_CACHE_THRESHOLD",
            ),
            &mut problems,
        );
        let semantic_embedding_model = source
            .raw_set(
                "semantic_embedding_model",
                "RROUTER_SEMANTIC_EMBEDDING_MODEL",
            )
            .map(|raw| raw.value.trim().to_string())
            .filter(|value| !value.is_empty());
        if semantic_cache != CacheMode::Off {
            match semantic_embedding_model.as_deref() {
                None => problems.push(
                    "RROUTER_SEMANTIC_EMBEDDING_MODEL is required when RROUTER_SEMANTIC_CACHE is on"
                        .to_string(),
                ),
                Some(model) => {
                    let valid = model.split_once('/').is_some_and(|(provider, id)| {
                        !id.is_empty() && rrouter_auth::get(provider).is_some()
                    });
                    if !valid {
                        problems.push(format!(
                            "RROUTER_SEMANTIC_EMBEDDING_MODEL must be provider/model with a \
                             registered provider (got {model:?})"
                        ));
                    }
                }
            }
        }
        let semantic_cache_ttl_secs = clamp_cache_ttl(clamped_var(
            source.raw("semantic_cache_ttl_secs", "RROUTER_SEMANTIC_CACHE_TTL_SECS"),
            DEFAULT_SEMANTIC_TTL_SECS,
            1,
            MAX_CACHE_TTL_SECS,
            &mut problems,
        )) as u64;
        let semantic_cache_max_entries = clamp_cache_entries(clamped_var(
            source.raw(
                "semantic_cache_max_entries",
                "RROUTER_SEMANTIC_CACHE_MAX_ENTRIES",
            ),
            DEFAULT_CACHE_MAX_ENTRIES,
            1,
            MAX_CACHE_ENTRIES,
            &mut problems,
        ));
        let subscription_reserve_percent = match source.raw(
            "subscription_reserve_percent",
            "RROUTER_SUBSCRIPTION_RESERVE_PERCENT",
        ) {
            None => DEFAULT_SUBSCRIPTION_RESERVE_PERCENT,
            Some(raw) => match raw.value.parse::<u8>() {
                Ok(value) if value <= 100 => value,
                _ => {
                    problems.push(format!(
                        "{} must be a percentage between 0 and 100",
                        raw.describe()
                    ));
                    DEFAULT_SUBSCRIPTION_RESERVE_PERCENT
                }
            },
        };
        // Same rule as the cache bounds: an oversized number is clamped, garbage stops
        // startup. `0` disables history deletion, not the sweep itself.
        let usage_retention_days = clamped_var(
            source.raw("usage_retention_days", USAGE_RETENTION_VAR),
            DEFAULT_USAGE_RETENTION_DAYS,
            0,
            MAX_USAGE_RETENTION_DAYS,
            &mut problems,
        );

        #[cfg(feature = "otel")]
        let otel = otel_setting(&mut source, &mut problems);

        if !problems.is_empty() {
            return Err(problems.join("; "));
        }
        let sources = std::mem::take(&mut source.sources);
        drop(source);
        Ok(Loaded {
            #[cfg(feature = "otel")]
            otel,
            config: Self {
                host,
                port,
                data_dir,
                master_key,
                require_api_key,
                request_timeout_secs,
                connect_timeout_secs,
                log_filter,
                static_dir,
                cors_origins,
                allow_remote_admin,
                admin_token,
                max_concurrency,
                max_attempts,
                retry_attempts,
                retry_base_ms,
                retry_max_ms,
                shutdown_grace_secs,
                strategy,
                cache,
                cache_ttl_secs,
                cache_max_entries,
                usage_retention_days,
                log_format,
                access_log,
                hedge_ms,
                canary,
                subscription_reserve_percent,
                semantic_cache,
                semantic_cache_threshold,
                semantic_embedding_model,
                semantic_cache_ttl_secs,
                semantic_cache_max_entries,
            },
            sources,
            config_file: file.map(|file| file.path),
        })
    }

    pub fn socket_addr(&self) -> SocketAddr {
        SocketAddr::new(self.host, self.port)
    }

    pub fn db_path(&self) -> PathBuf {
        self.data_dir.join("rrouter.db")
    }

    /// The secrets-free view printed by `--print-config` (no provenance).
    pub fn redacted(&self) -> Redacted<'_> {
        Redacted {
            config: self,
            sources: None,
            config_file: None,
        }
    }
}

fn var(name: &str) -> Option<String> {
    std::env::var(name).ok().map(|v| v.trim().to_string())
}

fn parse_bool(raw: &str) -> Option<bool> {
    match raw.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

/// Boolean setting. `default` is the built-in when neither source set it.
fn bool_var(raw: Option<Raw>, default: bool, problems: &mut Vec<String>) -> bool {
    match raw {
        None => default,
        Some(raw) => parse_bool(&raw.value).unwrap_or_else(|| {
            problems.push(format!("{} is not a boolean", raw.describe()));
            default
        }),
    }
}

fn secs_var(raw: Option<Raw>, default: u64, problems: &mut Vec<String>) -> u64 {
    secs_var_in(raw, default, false, problems)
}

/// Like [`secs_var`] but `0` is a meaningful value (wait indefinitely).
fn secs_var_allow_zero(raw: Option<Raw>, default: u64, problems: &mut Vec<String>) -> u64 {
    secs_var_in(raw, default, true, problems)
}

fn secs_var_in(
    raw: Option<Raw>,
    default: u64,
    allow_zero: bool,
    problems: &mut Vec<String>,
) -> u64 {
    match raw {
        None => default,
        Some(raw) => match raw.value.parse::<u64>() {
            Ok(v) if v > 0 || (allow_zero && v == 0) => v,
            _ => {
                let head = raw.head();
                problems.push(if allow_zero {
                    format!(
                        "{head}={:?} is not a number of seconds (0 disables the wait)",
                        raw.value
                    )
                } else {
                    format!("{head}={:?} is not a positive number of seconds", raw.value)
                });
                default
            }
        },
    }
}

/// Non-negative count. `0` means "unlimited" for the callers that do not clamp.
fn usize_var(raw: Option<Raw>, default: usize, problems: &mut Vec<String>) -> usize {
    match raw {
        None => default,
        Some(raw) => raw.value.parse::<usize>().unwrap_or_else(|_| {
            problems.push(format!("{} is not a non-negative integer", raw.describe()));
            default
        }),
    }
}

/// Attempts are clamped, not refused: an operator who writes 100 wants "more retries",
/// and failing startup over it would only take the gateway down.
fn clamp_attempts(raw: usize) -> usize {
    raw.clamp(1, MAX_ATTEMPTS_CEILING)
}

/// Numeric setting clamped into `min..=max`; a non-numeric value is a startup error.
///
/// Clamping is for "more than allowed", which is an understandable operator request;
/// garbage is not, and must be reported before the gateway starts serving.
fn clamped_var(
    raw: Option<Raw>,
    default: usize,
    min: usize,
    max: usize,
    problems: &mut Vec<String>,
) -> usize {
    match raw {
        None => default,
        Some(raw) => match raw.value.parse::<usize>() {
            Ok(v) => v.clamp(min, max),
            Err(_) => {
                problems.push(format!("{} is not a non-negative integer", raw.describe()));
                default
            }
        },
    }
}

/// Cache TTL is clamped, never refused: a longer life than a day only risks serving a
/// stale answer, while failing startup would take a working gateway down.
fn clamp_cache_ttl(raw: usize) -> usize {
    raw.clamp(1, MAX_CACHE_TTL_SECS)
}

/// Same reasoning as [`clamp_cache_ttl`]: the entry cap is a bound, not a contract.
fn clamp_cache_entries(raw: usize) -> usize {
    raw.clamp(1, MAX_CACHE_ENTRIES)
}

/// Log line shape: `text` for a terminal, `json` for Loki/CloudWatch. Unknown is refused.
fn log_format_setting(raw: Option<Raw>, problems: &mut Vec<String>) -> LogFormat {
    match raw {
        None => LogFormat::Text,
        Some(raw) => LogFormat::parse(&raw.value).unwrap_or_else(|| {
            problems.push(format!("{} is not one of text, json", raw.describe()));
            LogFormat::Text
        }),
    }
}

/// Equal-priority ordering. Unset means `priority`; a name the router does not know is a
/// startup error, because silently routing differently than asked is the worst outcome.
fn strategy_setting(raw: Option<Raw>, problems: &mut Vec<String>) -> Strategy {
    match raw {
        None => Strategy::Priority,
        Some(raw) => Strategy::parse(&raw.value).unwrap_or_else(|| {
            problems.push(format!(
                "{} is not one of priority, weighted, least_latency, cheapest",
                raw.describe()
            ));
            Strategy::Priority
        }),
    }
}

/// The per-request access line is on unless the operator explicitly turns it off.
fn access_log_setting(raw: Option<Raw>, problems: &mut Vec<String>) -> bool {
    match raw {
        None => true,
        Some(raw) => parse_bool(&raw.value).unwrap_or_else(|| {
            problems.push(format!("{} is not a boolean", raw.describe()));
            true
        }),
    }
}

/// Parse `RROUTER_CANARY`'s `model=provider:percent` form.
///
/// An empty value (or `off`) means no canary. A malformed value, an unregistered
/// provider, or a percentage above 100 stops startup: silently running an A/B the
/// operator did not ask for — or not running the one they did — is the worse outcome.
fn canary_setting(raw: Option<Raw>, problems: &mut Vec<String>) -> Option<CanaryRule> {
    let raw = raw?;
    let value = raw.value.trim();
    if value.is_empty() || value.eq_ignore_ascii_case("off") {
        return None;
    }
    let parsed = value.split_once('=').and_then(|(model, rest)| {
        rest.rsplit_once(':')
            .map(|(provider, percent)| (model, provider, percent))
    });
    let Some((model, provider, percent)) = parsed else {
        problems.push(format!(
            "{} must be model=provider:percent (for example gpt-4.1=azure:10)",
            raw.describe()
        ));
        return None;
    };
    let model = model.trim();
    let provider = provider.trim();
    if model.is_empty() || rrouter_auth::get(provider).is_none() {
        problems.push(format!(
            "{} must name a model ref and a registered provider (got {value:?})",
            raw.describe()
        ));
        return None;
    }
    let Ok(percent) = percent.trim().parse::<u8>() else {
        problems.push(format!(
            "{} has a percentage that is not 0..=100",
            raw.describe()
        ));
        return None;
    };
    if percent > 100 {
        problems.push(format!(
            "{} has a percentage that is not 0..=100",
            raw.describe()
        ));
        return None;
    }
    Some(CanaryRule {
        model: model.to_string(),
        provider: provider.to_string(),
        percent,
    })
}

/// Cosine threshold for a semantic hit. Anything outside `0..=1` is a startup error: a
/// threshold of 1.5 would disable the cache while looking enabled.
fn threshold_setting(raw: Option<Raw>, problems: &mut Vec<String>) -> f64 {
    match raw {
        None => DEFAULT_SEMANTIC_THRESHOLD,
        Some(raw) => match raw.value.trim().parse::<f64>() {
            Ok(value) if (0.0..=1.0).contains(&value) => value,
            _ => {
                problems.push(format!("{} must be a number in 0..=1", raw.describe()));
                DEFAULT_SEMANTIC_THRESHOLD
            }
        },
    }
}

/// Serializes configuration for `--print-config`, never echoing the master key or the
/// admin token. When built from [`Loaded`] it also reports `config_file` and a `sources`
/// map, so an operator can see which layer won without seeing any value that is secret.
pub struct Redacted<'a> {
    config: &'a Config,
    sources: Option<&'a SourceMap>,
    config_file: Option<&'a Path>,
}

impl Serialize for Redacted<'_> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let c = self.config;
        let extra = if self.sources.is_some() { 2 } else { 0 };
        let mut st = s.serialize_struct("Config", 33 + extra)?;
        st.serialize_field("host", &c.host.to_string())?;
        st.serialize_field("port", &c.port)?;
        st.serialize_field("data_dir", &c.data_dir.to_string_lossy())?;
        st.serialize_field("db_path", &c.db_path().to_string_lossy())?;
        st.serialize_field(
            "master_key",
            &if c.master_key.is_some() {
                "env"
            } else {
                "file"
            },
        )?;
        st.serialize_field("require_api_key", &c.require_api_key)?;
        st.serialize_field("request_timeout_secs", &c.request_timeout_secs)?;
        st.serialize_field("connect_timeout_secs", &c.connect_timeout_secs)?;
        st.serialize_field(
            "static_dir",
            &c.static_dir.as_ref().map(|p| p.to_string_lossy()),
        )?;
        st.serialize_field("cors_origins", &c.cors_origins)?;
        st.serialize_field("allow_remote_admin", &c.allow_remote_admin)?;
        st.serialize_field("log_filter", &c.log_filter)?;
        st.serialize_field(
            "admin_token",
            &if c.admin_token.is_some() {
                "set"
            } else {
                "unset"
            },
        )?;
        st.serialize_field("max_concurrency", &c.max_concurrency)?;
        st.serialize_field("max_attempts", &c.max_attempts)?;
        st.serialize_field("retry_attempts", &c.retry_attempts)?;
        st.serialize_field("retry_base_ms", &c.retry_base_ms)?;
        st.serialize_field("retry_max_ms", &c.retry_max_ms)?;
        st.serialize_field("shutdown_grace_secs", &c.shutdown_grace_secs)?;
        st.serialize_field("strategy", c.strategy.as_str())?;
        st.serialize_field("cache", c.cache.as_str())?;
        st.serialize_field("cache_ttl_secs", &c.cache_ttl_secs)?;
        st.serialize_field("cache_max_entries", &c.cache_max_entries)?;
        st.serialize_field("usage_retention_days", &c.usage_retention_days)?;
        st.serialize_field("log_format", c.log_format.as_str())?;
        st.serialize_field("access_log", &c.access_log)?;
        st.serialize_field("hedge_ms", &c.hedge_ms)?;
        st.serialize_field("canary", &c.canary.as_ref().map(CanaryRule::as_wire))?;
        st.serialize_field("semantic_cache", c.semantic_cache.as_str())?;
        st.serialize_field("semantic_cache_threshold", &c.semantic_cache_threshold)?;
        st.serialize_field("semantic_embedding_model", &c.semantic_embedding_model)?;
        st.serialize_field("semantic_cache_ttl_secs", &c.semantic_cache_ttl_secs)?;
        st.serialize_field("semantic_cache_max_entries", &c.semantic_cache_max_entries)?;
        if let Some(sources) = self.sources {
            st.serialize_field(
                "config_file",
                &self.config_file.map(|path| path.to_string_lossy()),
            )?;
            st.serialize_field("sources", sources)?;
        }
        st.end()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_booleans_and_rejects_garbage() {
        assert_eq!(parse_bool("TRUE"), Some(true));
        assert_eq!(parse_bool("0"), Some(false));
        assert_eq!(parse_bool("maybe"), None);
    }

    fn base() -> Config {
        Config {
            host: "127.0.0.1".parse().expect("ip"),
            port: DEFAULT_PORT,
            data_dir: PathBuf::from("/tmp/x"),
            master_key: None,
            require_api_key: false,
            request_timeout_secs: 600,
            connect_timeout_secs: 15,
            log_filter: DEFAULT_LOG.into(),
            static_dir: None,
            cors_origins: vec![],
            allow_remote_admin: false,
            admin_token: None,
            max_concurrency: 0,
            max_attempts: DEFAULT_MAX_ATTEMPTS,
            retry_attempts: DEFAULT_RETRY_ATTEMPTS,
            retry_base_ms: DEFAULT_RETRY_BASE_MS as u64,
            retry_max_ms: DEFAULT_RETRY_MAX_MS as u64,
            shutdown_grace_secs: DEFAULT_SHUTDOWN_GRACE_SECS,
            strategy: Strategy::Priority,
            cache: CacheMode::Off,
            cache_ttl_secs: DEFAULT_CACHE_TTL_SECS as u64,
            cache_max_entries: DEFAULT_CACHE_MAX_ENTRIES,
            usage_retention_days: DEFAULT_USAGE_RETENTION_DAYS,
            log_format: LogFormat::Text,
            access_log: true,
            hedge_ms: 0,
            canary: None,
            subscription_reserve_percent: 90,
            semantic_cache: CacheMode::Off,
            semantic_cache_threshold: 0.92,
            semantic_embedding_model: None,
            semantic_cache_ttl_secs: 3600,
            semantic_cache_max_entries: 1000,
        }
    }

    #[test]
    fn canary_setting_parses_and_refuses_garbage() {
        let mut problems = Vec::new();
        assert_eq!(
            canary_setting(raw_env("RROUTER_CANARY", "gpt-4.1=azure:10"), &mut problems),
            Some(CanaryRule {
                model: "gpt-4.1".to_string(),
                provider: "azure".to_string(),
                percent: 10,
            })
        );
        assert!(canary_setting(raw_env("RROUTER_CANARY", "off"), &mut problems).is_none());
        assert!(canary_setting(None, &mut problems).is_none());
        assert!(problems.is_empty(), "{problems:?}");

        for bad in [
            "nonsense",
            "gpt-4.1=nope:10",
            "gpt-4.1=azure:101",
            "gpt-4.1=azure:x",
            "=azure:10",
        ] {
            let mut problems = Vec::new();
            assert!(
                canary_setting(raw_env("RROUTER_CANARY", bad), &mut problems).is_none(),
                "{bad} must be refused"
            );
            assert_eq!(problems.len(), 1, "{bad}");
            assert!(
                problems[0].contains("RROUTER_CANARY"),
                "{bad}: {:?}",
                problems[0]
            );
        }
    }

    #[test]
    fn hedge_delay_is_clamped_and_garbage_is_refused() {
        let mut problems = Vec::new();
        assert_eq!(
            clamped_var(
                raw_env("RROUTER_HEDGE_MS", "50"),
                DEFAULT_HEDGE_MS,
                0,
                MAX_HEDGE_MS,
                &mut problems
            ),
            50
        );
        assert_eq!(
            clamped_var(
                raw_env("RROUTER_HEDGE_MS", "999999"),
                DEFAULT_HEDGE_MS,
                0,
                MAX_HEDGE_MS,
                &mut problems
            ),
            MAX_HEDGE_MS,
            "an oversized delay is clamped, not refused"
        );
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(
            clamped_var(
                raw_env("RROUTER_HEDGE_MS", "soon"),
                DEFAULT_HEDGE_MS,
                0,
                MAX_HEDGE_MS,
                &mut problems
            ),
            DEFAULT_HEDGE_MS
        );
        assert_eq!(problems.len(), 1);
    }

    #[test]
    fn semantic_cache_settings_are_validated_at_startup() {
        let disabled = Config::resolve(&env_map(&[]), None).expect("defaults");
        assert_eq!(disabled.config.semantic_cache, CacheMode::Off);
        assert_eq!(disabled.config.semantic_cache_threshold, 0.92);
        assert!(disabled.config.semantic_embedding_model.is_none());

        let valid = Config::resolve(
            &env_map(&[
                ("RROUTER_SEMANTIC_CACHE", "sqlite"),
                ("RROUTER_SEMANTIC_CACHE_THRESHOLD", "0.8"),
                (
                    "RROUTER_SEMANTIC_EMBEDDING_MODEL",
                    "openai/text-embedding-3-small",
                ),
                ("RROUTER_SEMANTIC_CACHE_TTL_SECS", "120"),
                ("RROUTER_SEMANTIC_CACHE_MAX_ENTRIES", "10"),
            ]),
            None,
        )
        .expect("a complete configuration resolves");
        assert_eq!(valid.config.semantic_cache, CacheMode::Sqlite);
        assert!((valid.config.semantic_cache_threshold - 0.8).abs() < 1e-9);
        assert_eq!(
            valid.config.semantic_embedding_model.as_deref(),
            Some("openai/text-embedding-3-small")
        );
        assert_eq!(valid.config.semantic_cache_ttl_secs, 120);
        assert_eq!(valid.config.semantic_cache_max_entries, 10);

        for (env, expected) in [
            (
                vec![("RROUTER_SEMANTIC_CACHE", "sqlite")],
                "RROUTER_SEMANTIC_EMBEDDING_MODEL is required",
            ),
            (
                vec![
                    ("RROUTER_SEMANTIC_CACHE", "sqlite"),
                    ("RROUTER_SEMANTIC_EMBEDDING_MODEL", "not-a-ref"),
                ],
                "must be provider/model",
            ),
            (
                vec![
                    ("RROUTER_SEMANTIC_CACHE", "sqlite"),
                    ("RROUTER_SEMANTIC_EMBEDDING_MODEL", "nope/model"),
                ],
                "must be provider/model",
            ),
            (
                vec![("RROUTER_SEMANTIC_CACHE", "banana")],
                "is not one of off, sqlite",
            ),
            (
                vec![("RROUTER_SEMANTIC_CACHE_THRESHOLD", "1.5")],
                "must be a number in 0..=1",
            ),
        ] {
            let error = Config::resolve(&env_map(&env), None)
                .expect_err(&format!("{env:?} must be refused"));
            assert!(
                error.contains(expected),
                "{env:?}: {error} is missing {expected:?}"
            );
        }
    }

    #[test]
    fn strategy_names_parse_and_unknown_values_are_refused() {
        assert_eq!(Strategy::parse("priority"), Some(Strategy::Priority));
        assert_eq!(Strategy::parse(" Weighted "), Some(Strategy::Weighted));
        assert_eq!(
            Strategy::parse("least_latency"),
            Some(Strategy::LeastLatency)
        );
        assert_eq!(Strategy::parse("CHEAPEST"), Some(Strategy::Cheapest));
        assert_eq!(Strategy::parse("least-latency"), None);
        assert_eq!(Strategy::parse(""), None);
        // The startup message is the only place an operator learns the accepted names,
        // so it must list every one of them.
        let mut problems = Vec::new();
        assert_eq!(strategy_setting(None, &mut problems), Strategy::Priority);
        assert_eq!(
            strategy_setting(raw_env("RROUTER_STRATEGY", "cheapest"), &mut problems),
            Strategy::Cheapest
        );
        assert!(
            problems.is_empty(),
            "valid settings must not report problems"
        );
        assert_eq!(
            strategy_setting(raw_env("RROUTER_STRATEGY", "round_robin"), &mut problems),
            Strategy::Priority
        );
        assert_eq!(problems.len(), 1);
        for name in ["priority", "weighted", "least_latency", "cheapest"] {
            assert!(
                problems[0].contains(name),
                "{name} missing from {:?}",
                problems[0]
            );
        }
        assert!(
            problems[0].contains("RROUTER_STRATEGY"),
            "the error must name the source: {:?}",
            problems[0]
        );
    }

    #[test]
    fn cache_names_parse_and_unknown_values_are_refused() {
        assert_eq!(CacheMode::parse("off"), Some(CacheMode::Off));
        assert_eq!(CacheMode::parse(" SQLite "), Some(CacheMode::Sqlite));
        assert_eq!(CacheMode::parse("redis"), None);
        assert_eq!(CacheMode::parse(""), None);
        assert_eq!(CacheMode::default(), CacheMode::Off, "the cache is opt-in");
        assert_eq!(CacheMode::Off.as_str(), "off");
        assert_eq!(CacheMode::Sqlite.as_str(), "sqlite");
    }

    /// TTL and entry caps are clamped into the documented windows; the values are
    /// operator intent, so an oversized number must not take the gateway down.
    #[test]
    fn cache_numbers_are_clamped_into_the_supported_range() {
        assert_eq!(
            clamp_cache_ttl(0),
            1,
            "zero would make every entry expire at once"
        );
        assert_eq!(clamp_cache_ttl(300), 300);
        assert_eq!(clamp_cache_ttl(86_400), 86_400);
        assert_eq!(clamp_cache_ttl(usize::MAX), MAX_CACHE_TTL_SECS);
        assert_eq!(clamp_cache_entries(0), 1);
        assert_eq!(clamp_cache_entries(1_000), 1_000);
        assert_eq!(clamp_cache_entries(100_000), 100_000);
        assert_eq!(clamp_cache_entries(usize::MAX), MAX_CACHE_ENTRIES);
    }

    #[test]
    fn attempts_are_clamped_into_the_supported_range() {
        assert_eq!(clamp_attempts(0), 1, "zero sends would fail every request");
        assert_eq!(clamp_attempts(1), 1);
        assert_eq!(clamp_attempts(4), 4);
        assert_eq!(clamp_attempts(8), 8);
        assert_eq!(clamp_attempts(100), MAX_ATTEMPTS_CEILING);
        assert_eq!(clamp_attempts(usize::MAX), MAX_ATTEMPTS_CEILING);
    }

    /// Retry knobs: unset means the documented defaults, oversized values clamp into
    /// the windows, `0` disables a stage, and garbage stops startup like every other
    /// numeric setting.
    #[test]
    fn retry_settings_default_clamp_and_refuse_garbage() {
        static ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = ENV.lock().unwrap_or_else(|p| p.into_inner());
        let read = |name: &str, default: usize, max: usize, problems: &mut Vec<String>| {
            // The resolver builds a `Raw` per setting; this reproduces that for the
            // environment-only case the test exercises.
            clamped_var(
                var(name).map(|value| Raw::env(name, value)),
                default,
                0,
                max,
                problems,
            )
        };

        for name in [
            "RROUTER_RETRY_ATTEMPTS",
            "RROUTER_RETRY_BASE_MS",
            "RROUTER_RETRY_MAX_MS",
        ] {
            std::env::remove_var(name);
        }
        let mut problems = Vec::new();
        assert_eq!(
            read(
                "RROUTER_RETRY_ATTEMPTS",
                DEFAULT_RETRY_ATTEMPTS,
                MAX_RETRY_ATTEMPTS,
                &mut problems
            ),
            1,
            "one retry per candidate is the default"
        );
        assert_eq!(
            read(
                "RROUTER_RETRY_BASE_MS",
                DEFAULT_RETRY_BASE_MS,
                MAX_RETRY_BASE_MS,
                &mut problems
            ),
            200
        );
        assert_eq!(
            read(
                "RROUTER_RETRY_MAX_MS",
                DEFAULT_RETRY_MAX_MS,
                MAX_RETRY_MAX_MS,
                &mut problems
            ),
            5_000
        );
        assert!(problems.is_empty());

        std::env::set_var("RROUTER_RETRY_ATTEMPTS", "2");
        std::env::set_var("RROUTER_RETRY_BASE_MS", "1");
        std::env::set_var("RROUTER_RETRY_MAX_MS", "50");
        let mut problems = Vec::new();
        assert_eq!(
            read(
                "RROUTER_RETRY_ATTEMPTS",
                DEFAULT_RETRY_ATTEMPTS,
                MAX_RETRY_ATTEMPTS,
                &mut problems
            ),
            2
        );
        assert_eq!(
            read(
                "RROUTER_RETRY_BASE_MS",
                DEFAULT_RETRY_BASE_MS,
                MAX_RETRY_BASE_MS,
                &mut problems
            ),
            1
        );
        assert_eq!(
            read(
                "RROUTER_RETRY_MAX_MS",
                DEFAULT_RETRY_MAX_MS,
                MAX_RETRY_MAX_MS,
                &mut problems
            ),
            50
        );
        assert!(problems.is_empty());

        std::env::set_var("RROUTER_RETRY_ATTEMPTS", "0");
        std::env::set_var("RROUTER_RETRY_BASE_MS", "0");
        std::env::set_var("RROUTER_RETRY_MAX_MS", "0");
        let mut problems = Vec::new();
        assert_eq!(
            read(
                "RROUTER_RETRY_ATTEMPTS",
                DEFAULT_RETRY_ATTEMPTS,
                MAX_RETRY_ATTEMPTS,
                &mut problems
            ),
            0,
            "zero retries is a legitimate off switch"
        );
        assert_eq!(
            read(
                "RROUTER_RETRY_BASE_MS",
                DEFAULT_RETRY_BASE_MS,
                MAX_RETRY_BASE_MS,
                &mut problems
            ),
            0
        );
        assert_eq!(
            read(
                "RROUTER_RETRY_MAX_MS",
                DEFAULT_RETRY_MAX_MS,
                MAX_RETRY_MAX_MS,
                &mut problems
            ),
            0
        );
        assert!(problems.is_empty());

        std::env::set_var("RROUTER_RETRY_ATTEMPTS", "99");
        std::env::set_var("RROUTER_RETRY_BASE_MS", "999999");
        std::env::set_var("RROUTER_RETRY_MAX_MS", "999999");
        let mut problems = Vec::new();
        assert_eq!(
            read(
                "RROUTER_RETRY_ATTEMPTS",
                DEFAULT_RETRY_ATTEMPTS,
                MAX_RETRY_ATTEMPTS,
                &mut problems
            ),
            MAX_RETRY_ATTEMPTS
        );
        assert_eq!(
            read(
                "RROUTER_RETRY_BASE_MS",
                DEFAULT_RETRY_BASE_MS,
                MAX_RETRY_BASE_MS,
                &mut problems
            ),
            MAX_RETRY_BASE_MS
        );
        assert_eq!(
            read(
                "RROUTER_RETRY_MAX_MS",
                DEFAULT_RETRY_MAX_MS,
                MAX_RETRY_MAX_MS,
                &mut problems
            ),
            MAX_RETRY_MAX_MS
        );
        assert!(
            problems.is_empty(),
            "an oversized bound is clamped, not refused"
        );

        std::env::set_var("RROUTER_RETRY_ATTEMPTS", "many");
        std::env::set_var("RROUTER_RETRY_BASE_MS", "-1");
        std::env::set_var("RROUTER_RETRY_MAX_MS", "5s");
        let mut problems = Vec::new();
        assert_eq!(
            read(
                "RROUTER_RETRY_ATTEMPTS",
                DEFAULT_RETRY_ATTEMPTS,
                MAX_RETRY_ATTEMPTS,
                &mut problems
            ),
            DEFAULT_RETRY_ATTEMPTS,
            "garbage falls back to the default"
        );
        assert_eq!(
            read(
                "RROUTER_RETRY_BASE_MS",
                DEFAULT_RETRY_BASE_MS,
                MAX_RETRY_BASE_MS,
                &mut problems
            ),
            DEFAULT_RETRY_BASE_MS
        );
        assert_eq!(
            read(
                "RROUTER_RETRY_MAX_MS",
                DEFAULT_RETRY_MAX_MS,
                MAX_RETRY_MAX_MS,
                &mut problems
            ),
            DEFAULT_RETRY_MAX_MS
        );
        assert_eq!(
            problems.len(),
            3,
            "each bad value must fail startup: {problems:?}"
        );

        for name in [
            "RROUTER_RETRY_ATTEMPTS",
            "RROUTER_RETRY_BASE_MS",
            "RROUTER_RETRY_MAX_MS",
        ] {
            std::env::remove_var(name);
        }
    }

    /// Retention is a bound, not a promise: unset keeps everything, an oversized number
    /// is clamped to ten years, and garbage stops startup like the other numeric knobs.
    #[test]
    fn usage_retention_defaults_to_zero_and_is_clamped() {
        static ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = ENV.lock().unwrap_or_else(|p| p.into_inner());
        let read = |problems: &mut Vec<String>| {
            clamped_var(
                var(USAGE_RETENTION_VAR).map(|value| Raw::env(USAGE_RETENTION_VAR, value)),
                DEFAULT_USAGE_RETENTION_DAYS,
                0,
                MAX_USAGE_RETENTION_DAYS,
                problems,
            )
        };

        std::env::remove_var(USAGE_RETENTION_VAR);
        let mut problems = Vec::new();
        assert_eq!(read(&mut problems), 0, "unset keeps history forever");
        assert!(problems.is_empty());

        std::env::set_var(USAGE_RETENTION_VAR, "30");
        let mut problems = Vec::new();
        assert_eq!(read(&mut problems), 30);
        assert!(problems.is_empty());

        std::env::set_var(USAGE_RETENTION_VAR, "99999");
        let mut problems = Vec::new();
        assert_eq!(read(&mut problems), MAX_USAGE_RETENTION_DAYS);
        assert!(
            problems.is_empty(),
            "an oversized bound is clamped, not refused"
        );

        std::env::set_var(USAGE_RETENTION_VAR, "-1");
        let mut problems = Vec::new();
        assert_eq!(
            read(&mut problems),
            0,
            "a bad value falls back to the default"
        );
        assert_eq!(problems.len(), 1, "garbage must fail startup: {problems:?}");

        std::env::remove_var(USAGE_RETENTION_VAR);
    }

    #[test]
    fn redacted_view_hides_the_key_material() {
        let mut cfg = base();
        cfg.master_key = Some("ab".repeat(32));
        let json = serde_json::to_string(&cfg.redacted()).expect("serialize");
        assert!(json.contains("\"port\":20128"));
        assert!(!json.contains(&"ab".repeat(32)));
        assert!(json.contains("\"master_key\":\"env\""));
        assert!(json.contains("\"admin_token\":\"unset\""));
    }

    #[test]
    fn redacted_view_says_whether_an_admin_token_is_configured_without_showing_it() {
        let token = "adm-0123456789abcdef";
        let mut cfg = base();
        cfg.admin_token = Some(token.into());
        cfg.max_concurrency = 7;
        cfg.max_attempts = 2;
        cfg.shutdown_grace_secs = 0;
        cfg.cache = CacheMode::Sqlite;
        cfg.cache_ttl_secs = 60;
        cfg.cache_max_entries = 5;
        let json = serde_json::to_string(&cfg.redacted()).expect("serialize");
        assert!(json.contains("\"admin_token\":\"set\""));
        assert!(
            !json.contains(token),
            "the token itself must never be printed: {json}"
        );
        assert!(json.contains("\"max_concurrency\":7"));
        assert!(json.contains("\"max_attempts\":2"));
        assert!(json.contains("\"shutdown_grace_secs\":0"));
        assert!(json.contains("\"strategy\":\"priority\""));
        assert!(json.contains("\"cache\":\"sqlite\""));
        assert!(json.contains("\"cache_ttl_secs\":60"));
        assert!(json.contains("\"cache_max_entries\":5"));
    }

    #[test]
    fn cors_allows_configured_and_loopback_origins_only() {
        let mut cfg = base();
        cfg.cors_origins = vec!["https://console.example".into()];
        assert_eq!(
            cfg.cors_origin_for("https://console.example"),
            Some("https://console.example".into())
        );
        assert_eq!(
            cfg.cors_origin_for("http://localhost:3000"),
            Some("http://localhost:3000".into())
        );
        assert_eq!(cfg.cors_origin_for("https://evil.example"), None);
        cfg.cors_origins = vec!["*".into()];
        // A stored wildcard must never grant a foreign origin: an admin API that is
        // unauthenticated by default must not be readable from any web page.
        assert_eq!(cfg.cors_origin_for("https://evil.example"), None);
    }

    #[test]
    fn log_format_names_parse_and_unknown_values_are_refused() {
        assert_eq!(LogFormat::parse("text"), Some(LogFormat::Text));
        assert_eq!(LogFormat::parse(" JSON "), Some(LogFormat::Json));
        assert_eq!(LogFormat::parse("pretty"), None);
        assert_eq!(LogFormat::parse(""), None);
        assert_eq!(
            LogFormat::default(),
            LogFormat::Text,
            "text stays the default"
        );
        assert_eq!(LogFormat::Json.as_str(), "json");
    }

    #[test]
    fn log_settings_default_and_refuse_garbage() {
        let mut problems = Vec::new();
        assert_eq!(log_format_setting(None, &mut problems), LogFormat::Text);
        assert!(
            access_log_setting(None, &mut problems),
            "the access line is on by default"
        );
        assert!(problems.is_empty());
        assert_eq!(
            log_format_setting(raw_env("RROUTER_LOG_FORMAT", "json"), &mut problems),
            LogFormat::Json
        );
        assert!(!access_log_setting(
            raw_env("RROUTER_ACCESS_LOG", "false"),
            &mut problems
        ));
        assert!(problems.is_empty());
        log_format_setting(raw_env("RROUTER_LOG_FORMAT", "yaml"), &mut problems);
        access_log_setting(raw_env("RROUTER_ACCESS_LOG", "maybe"), &mut problems);
        assert_eq!(
            problems.len(),
            2,
            "both bad values must fail startup: {problems:?}"
        );
        assert!(
            problems.iter().all(|p| p.contains("RROUTER_")),
            "errors name their source: {problems:?}"
        );
    }

    // ----- config file v1 ---------------------------------------------------------

    /// A raw environment value, as the resolver builds it.
    fn raw_env(name: &str, value: &str) -> Option<Raw> {
        Some(Raw::env(name, value.to_string()))
    }

    /// An environment lookup backed by a map, so these tests never mutate the process
    /// environment (and can run in parallel with everything else).
    fn env_map(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: BTreeMap<String, String> = pairs
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect();
        move |name| map.get(name).cloned()
    }

    /// Parse inline JSON as if it lived at `path`.
    fn file_at(path: &str, json: &str) -> Result<FileLayer, String> {
        FileLayer::parse(Path::new(path), json, &env_map(&[]))
    }

    #[test]
    fn defaults_are_unchanged_without_a_file_or_environment() {
        let loaded = Config::resolve(&env_map(&[]), None).expect("defaults must always resolve");
        let expected = base();
        assert_eq!(loaded.config.host, expected.host);
        assert_eq!(loaded.config.port, DEFAULT_PORT);
        assert_eq!(loaded.config.data_dir, PathBuf::from(DEFAULT_DATA_DIR));
        assert!(!loaded.config.require_api_key);
        assert_eq!(loaded.config.max_attempts, DEFAULT_MAX_ATTEMPTS);
        assert_eq!(loaded.config.retry_attempts, DEFAULT_RETRY_ATTEMPTS);
        assert_eq!(loaded.config.strategy, Strategy::Priority);
        assert_eq!(loaded.config.cache, CacheMode::Off);
        assert_eq!(loaded.config.cache_ttl_secs, DEFAULT_CACHE_TTL_SECS as u64);
        assert_eq!(loaded.config.log_format, LogFormat::Text);
        assert!(loaded.config.access_log);
        assert!(loaded.config.master_key.is_none());
        assert!(loaded.config.admin_token.is_none());
        assert!(loaded.config.cors_origins.is_empty());
        // Every field is accounted for, and all of them are defaults.
        assert_eq!(loaded.sources.counts(), (0, 0, 33));
        assert!(loaded.config_file.is_none());
    }

    #[test]
    fn the_environment_overrides_the_file() {
        let file = file_at(
            "/etc/rrouter.json",
            r#"{"cache":"sqlite","max_concurrency":7,"max_attempts":2,"strategy":"weighted"}"#,
        )
        .expect("file parses");
        let env = env_map(&[("RROUTER_CACHE", "off"), ("RROUTER_MAX_CONCURRENCY", "9")]);
        let loaded = Config::resolve(&env, Some(file)).expect("merge");

        assert_eq!(loaded.config.cache, CacheMode::Off, "env wins over file");
        assert_eq!(loaded.config.max_concurrency, 9);
        assert_eq!(
            loaded.config.max_attempts, 2,
            "the file still applies where the environment is silent"
        );
        assert_eq!(loaded.config.strategy, Strategy::Weighted);
        assert_eq!(loaded.sources.origin("cache"), Some(Origin::Env));
        assert_eq!(loaded.sources.origin("max_concurrency"), Some(Origin::Env));
        assert_eq!(loaded.sources.origin("max_attempts"), Some(Origin::File));
        assert_eq!(loaded.sources.origin("port"), Some(Origin::Default));
        assert_eq!(
            loaded.config_file.as_deref().and_then(|p| p.to_str()),
            Some("/etc/rrouter.json")
        );
    }

    #[test]
    fn an_unknown_file_key_names_the_key_and_its_line() {
        let error = file_at("/etc/rrouter.json", "{\n  \"prot\": 8080\n}")
            .expect_err("unknown keys must be refused");
        assert!(error.contains("/etc/rrouter.json"), "{error}");
        assert!(error.contains("prot"), "the key must be named: {error}");
        assert!(error.contains("line 2"), "the line must be named: {error}");
    }

    #[test]
    fn a_file_value_with_the_wrong_type_names_the_line() {
        let error = file_at("/etc/rrouter.json", "{\n  \"port\": \"8080\"\n}")
            .expect_err("a string port must be refused");
        assert!(error.contains("invalid type"), "{error}");
        assert!(error.contains("line 2"), "{error}");
        // The port refusal (file cannot set bind settings) is not what fired here: the
        // message must be about the type, so the operator fixes the right thing.
        assert!(!error.contains("RROUTER_PORT"), "{error}");
    }

    #[test]
    fn duplicate_file_keys_are_refused_with_the_second_line() {
        let error = file_at(
            "/etc/rrouter.json",
            "{\"cache\":\"off\",\n\"cache\":\"sqlite\"}",
        )
        .expect_err("duplicate keys must be refused");
        assert!(error.contains("duplicate key \"cache\""), "{error}");
        assert!(error.contains("line 2"), "{error}");
    }

    #[test]
    fn an_inline_secret_is_refused() {
        let error = file_at(
            "/etc/rrouter.json",
            r#"{"admin_token":"adm-inline-secret"}"#,
        )
        .expect_err("inline secrets must be refused");
        assert!(error.contains("inline secrets are not allowed"), "{error}");
        assert!(error.contains("key_env"), "{error}");
        assert!(error.contains("key_file"), "{error}");
        assert!(
            !error.contains("adm-inline-secret"),
            "the rejected value must not be echoed: {error}"
        );
    }

    #[test]
    fn secret_references_resolve_from_the_environment_and_a_file() {
        let key_file =
            std::env::temp_dir().join(format!("rrouter-master-{}.key", uuid::Uuid::new_v4()));
        std::fs::write(&key_file, format!("{}\n", "ab".repeat(32))).expect("write key");
        let json = format!(
            r#"{{"admin_token":{{"key_env":"ADMIN_TOKEN"}},"master_key":{{"key_file":"{}"}}}}"#,
            key_file.display()
        );
        // A `key_env` reference is resolved while the file loads, against the same
        // environment the rest of the merge reads.
        let env = env_map(&[("ADMIN_TOKEN", "token-from-env")]);
        let layer = FileLayer::parse(Path::new("/etc/rrouter.json"), &json, &env)
            .expect("secret refs parse");
        let loaded = Config::resolve(&env, Some(layer)).expect("resolve");
        std::fs::remove_file(&key_file).ok();

        let expected_master = "ab".repeat(32);
        assert_eq!(loaded.config.admin_token.as_deref(), Some("token-from-env"));
        assert_eq!(
            loaded.config.master_key.as_deref(),
            Some(expected_master.as_str())
        );
        assert_eq!(loaded.sources.origin("admin_token"), Some(Origin::File));
        assert_eq!(loaded.sources.origin("master_key"), Some(Origin::File));
    }

    #[test]
    fn a_missing_secret_reference_fails_with_its_name() {
        let error = file_at(
            "/etc/rrouter.json",
            r#"{"admin_token":{"key_env":"ABSENT_FOR_THE_TEST"}}"#,
        )
        .expect_err("an unset key_env must fail");
        assert!(error.contains("ABSENT_FOR_THE_TEST"), "{error}");
        assert!(error.contains("not set"), "{error}");

        let error = file_at(
            "/etc/rrouter.json",
            r#"{"admin_token":{"key_env":"A","key_file":"/tmp/b"}}"#,
        )
        .expect_err("both references must fail");
        assert!(error.contains("mutually exclusive"), "{error}");

        let error = file_at("/etc/rrouter.json", r#"{"admin_token":{}}"#)
            .expect_err("an empty reference must fail");
        assert!(error.contains("key_env"), "{error}");
    }

    #[test]
    fn the_file_cannot_set_the_bind_address() {
        let error = file_at("/etc/rrouter.json", r#"{"host":"0.0.0.0","port":8081}"#)
            .expect_err("bind settings are environment-only");
        assert!(error.contains("RROUTER_HOST"), "{error}");
        assert!(error.contains("RROUTER_PORT"), "{error}");
        assert!(error.contains("startup"), "{error}");
    }

    #[test]
    fn a_wildcard_origin_from_the_file_is_refused() {
        let layer = file_at("/etc/rrouter.json", r#"{"cors_origins":["*"]}"#).expect("file parses");
        let error = Config::resolve(&env_map(&[]), Some(layer)).expect_err("wildcard refused");
        assert!(error.contains("not allowed"), "{error}");
        assert!(error.contains("/etc/rrouter.json"), "{error}");
        assert!(error.contains("cors_origins"), "{error}");
    }

    #[test]
    fn print_config_reports_sources_and_redacts_secrets() {
        let env = env_map(&[
            ("ADMIN_TOKEN", "super-secret-admin-token"),
            ("RROUTER_STRATEGY", "weighted"),
        ]);
        let layer = FileLayer::parse(
            Path::new("/etc/rrouter.json"),
            r#"{"admin_token":{"key_env":"ADMIN_TOKEN"},"max_concurrency":3}"#,
            &env,
        )
        .expect("file parses");
        let loaded = Config::resolve(&env, Some(layer)).expect("resolve");
        let sources = loaded.sources.clone();
        let rendered = serde_json::to_string(&loaded.redacted()).expect("serialize");

        assert!(!rendered.contains("super-secret-admin-token"), "{rendered}");
        assert!(rendered.contains("\"admin_token\":\"set\""), "{rendered}");
        assert!(rendered.contains("\"max_concurrency\":3"), "{rendered}");
        assert!(rendered.contains("\"strategy\":\"weighted\""), "{rendered}");
        assert!(
            rendered.contains("\"config_file\":\"/etc/rrouter.json\""),
            "{rendered}"
        );
        assert!(rendered.contains("\"sources\":{"), "{rendered}");
        assert!(
            rendered.contains("\"max_concurrency\":\"file\""),
            "file provenance missing: {rendered}"
        );
        assert!(
            rendered.contains("\"strategy\":\"env\""),
            "env provenance missing: {rendered}"
        );
        assert!(
            rendered.contains("\"port\":\"default\""),
            "default provenance missing: {rendered}"
        );
        assert_eq!(sources.counts(), (1, 2, 30));
    }

    #[test]
    fn check_config_counts_every_setting() {
        let layer = file_at(
            "/etc/rrouter.json",
            r#"{"cache":"sqlite","cache_ttl_secs":60}"#,
        )
        .expect("file parses");
        let env = env_map(&[("RROUTER_STRATEGY", "cheapest")]);
        let loaded = Config::resolve(&env, Some(layer)).expect("resolve");
        // env + file + default must add up to the settings the resolver tracks.
        let (env, file, defaults) = loaded.sources.counts();
        assert_eq!((env, file, defaults), (1, 2, 30));
        assert_eq!(env + file + defaults, 33);
    }

    // ----- reload planning --------------------------------------------------------

    /// Write `body` to a fresh temp file and return its path.
    fn temp_file(tag: &str, body: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("rrouter-{tag}-{}.tmp", uuid::Uuid::new_v4()));
        std::fs::write(&path, body).expect("write temp file");
        path
    }

    #[test]
    fn a_changed_strategy_is_a_reload_plan_not_a_restart() {
        let old = Config::resolve(&env_map(&[]), None).expect("old");
        let new =
            Config::resolve(&env_map(&[("RROUTER_STRATEGY", "weighted")]), None).expect("new");

        let plan = ReloadPlan::between(&old.config, &new.config);

        assert_eq!(plan.changed.len(), 1, "{plan:?}");
        assert_eq!(plan.changed[0].field, "strategy");
        assert_eq!(plan.changed[0].from, "priority");
        assert_eq!(plan.changed[0].to, "weighted");
        assert!(plan.restart_required.is_empty(), "{plan:?}");
        assert!(plan.rejected.is_empty(), "{plan:?}");
        assert!(!plan.is_empty());
        // Deterministic: the same inputs produce the same plan and order.
        assert_eq!(plan, ReloadPlan::between(&old.config, &new.config));
    }

    #[test]
    fn a_changed_subscription_reserve_is_reloadable() {
        let old = Config::resolve(&env_map(&[]), None).expect("old");
        let new = Config::resolve(
            &env_map(&[("RROUTER_SUBSCRIPTION_RESERVE_PERCENT", "50")]),
            None,
        )
        .expect("new");
        assert_eq!(new.config.subscription_reserve_percent, 50);
        let plan = ReloadPlan::between(&old.config, &new.config);
        assert_eq!(plan.changed.len(), 1, "{plan:?}");
        assert_eq!(plan.changed[0].field, "subscription_reserve_percent");
        assert_eq!(plan.changed[0].to, "50");
        assert_eq!(
            old.config
                .reloaded(&new.config, &plan)
                .subscription_reserve_percent,
            50
        );
    }

    #[test]
    fn subscription_reserve_rejects_an_out_of_range_percentage() {
        for value in ["101", "-1", "ninety"] {
            let env = env_map(&[("RROUTER_SUBSCRIPTION_RESERVE_PERCENT", value)]);
            let error = Config::resolve(&env, None).expect_err("invalid reserve");
            assert!(
                error.contains("SUBSCRIPTION_RESERVE_PERCENT")
                    || error.contains("subscription_reserve_percent"),
                "{value}: {error}"
            );
        }
        let env = env_map(&[("RROUTER_SUBSCRIPTION_RESERVE_PERCENT", "0")]);
        assert_eq!(
            Config::resolve(&env, None)
                .expect("0 disables")
                .config
                .subscription_reserve_percent,
            0
        );
    }

    #[test]
    fn host_port_and_data_dir_require_a_restart_and_never_change_live() {
        let old = Config::resolve(&env_map(&[]), None).expect("old");
        let new = Config::resolve(
            &env_map(&[
                ("RROUTER_HOST", "0.0.0.0"),
                ("RROUTER_PORT", "20999"),
                ("RROUTER_DATA_DIR", "/srv/rrouter"),
            ]),
            None,
        )
        .expect("new");

        let plan = ReloadPlan::between(&old.config, &new.config);

        let fields: Vec<&str> = plan
            .restart_required
            .iter()
            .map(|entry| entry.field)
            .collect();
        assert_eq!(fields, vec!["host", "port", "data_dir"], "{plan:?}");
        assert!(
            plan.changed.is_empty(),
            "startup-only settings must never be applied by a reload: {plan:?}"
        );
        // Planning has no side effects: the running config is exactly what it was.
        assert_eq!(old.config.port, DEFAULT_PORT);
        assert_eq!(old.config.host.to_string(), DEFAULT_HOST);
        assert_eq!(old.config.data_dir, PathBuf::from(DEFAULT_DATA_DIR));
    }

    #[test]
    fn an_invalid_reload_file_returns_an_error_and_leaves_the_old_config_alone() {
        let old = Config::resolve(&env_map(&[]), None).expect("old");
        let before = old.config.clone();
        let path = temp_file("reload-invalid", "{\"cache\":\"redis\"}");

        let error = Config::load_reloadable(&path, &env_map(&[])).expect_err("invalid file");
        std::fs::remove_file(&path).ok();

        assert!(error.contains("cache"), "{error}");
        assert!(error.contains("off, sqlite"), "{error}");
        // Same values, and an empty plan against itself: nothing was applied.
        assert_eq!(old.config.cache, before.cache);
        assert_eq!(old.config.port, before.port);
        assert!(ReloadPlan::between(&before, &old.config).is_empty());
    }

    #[test]
    fn an_identical_file_produces_an_empty_plan() {
        let path = temp_file(
            "reload-same",
            r#"{"cache":"sqlite","cache_ttl_secs":60,"strategy":"weighted"}"#,
        );
        let env = env_map(&[]);
        let first = Config::load_reloadable(&path, &env).expect("first");
        let second = Config::load_reloadable(&path, &env).expect("second");
        std::fs::remove_file(&path).ok();

        let plan = second.plan_against(&first.config);
        assert!(plan.is_empty(), "{plan:?}");
        assert_eq!(plan.changed_fields().count(), 0);
    }

    #[test]
    fn a_reload_reads_the_named_file_and_still_lets_the_environment_win() {
        let path = temp_file(
            "reload-env",
            r#"{"cache":"sqlite","log_filter":"rrouter=debug"}"#,
        );
        // `RROUTER_CONFIG` in the environment must be irrelevant: the caller named the
        // file explicitly, and a stale variable must not redirect the reload.
        let env = env_map(&[
            ("RROUTER_CACHE", "off"),
            ("RROUTER_CONFIG", "/nonexistent/ignored.json"),
        ]);

        let loaded = Config::load_reloadable(&path, &env).expect("reload");
        std::fs::remove_file(&path).ok();

        assert_eq!(loaded.config.cache, CacheMode::Off, "environment wins");
        assert_eq!(
            loaded.config.log_filter, "rrouter=debug",
            "the file applies where the environment is silent"
        );
        assert_eq!(loaded.sources.origin("cache"), Some(Origin::Env));
        assert_eq!(loaded.sources.origin("log_filter"), Some(Origin::File));
    }

    #[test]
    fn a_secret_change_is_reported_without_its_value() {
        let old = Config::resolve(
            &env_map(&[("RROUTER_ADMIN_TOKEN", "old-secret-token")]),
            None,
        )
        .expect("old");
        let new = Config::resolve(
            &env_map(&[("RROUTER_ADMIN_TOKEN", "new-secret-token")]),
            None,
        )
        .expect("new");

        let plan = ReloadPlan::between(&old.config, &new.config);
        let rendered = serde_json::to_string(&plan).expect("serialize");

        assert!(!rendered.contains("old-secret-token"), "{rendered}");
        assert!(!rendered.contains("new-secret-token"), "{rendered}");
        assert_eq!(plan.changed.len(), 1, "{plan:?}");
        assert_eq!(plan.changed[0].field, "admin_token");
        assert_eq!(plan.changed[0].from, "set");
        assert_eq!(plan.changed[0].to, "rotated");
    }

    #[test]
    fn master_key_rotation_requires_a_restart_and_prints_no_key() {
        let old_key = "aa".repeat(32);
        let new_key = "bb".repeat(32);
        let old =
            Config::resolve(&env_map(&[("RROUTER_MASTER_KEY", &old_key)]), None).expect("old");
        let new =
            Config::resolve(&env_map(&[("RROUTER_MASTER_KEY", &new_key)]), None).expect("new");

        let plan = ReloadPlan::between(&old.config, &new.config);
        let rendered = serde_json::to_string(&plan).expect("serialize");

        assert!(plan.changed.is_empty(), "{plan:?}");
        assert_eq!(plan.restart_required.len(), 1, "{plan:?}");
        assert_eq!(plan.restart_required[0].field, "master_key");
        assert!(!rendered.contains(&old_key), "{rendered}");
        assert!(!rendered.contains(&new_key), "{rendered}");
    }

    #[test]
    fn a_reload_cannot_silently_make_the_admin_api_unreachable() {
        let old = Config::resolve(
            &env_map(&[
                ("RROUTER_HOST", "0.0.0.0"),
                ("RROUTER_ALLOW_REMOTE_ADMIN", "true"),
            ]),
            None,
        )
        .expect("old");
        let new = Config::resolve(&env_map(&[("RROUTER_HOST", "0.0.0.0")]), None).expect("new");

        let plan = ReloadPlan::between(&old.config, &new.config);

        assert_eq!(plan.rejected.len(), 1, "{plan:?}");
        assert_eq!(plan.rejected[0].field, "allow_remote_admin");
        assert!(
            !plan
                .changed_fields()
                .any(|field| field == "allow_remote_admin"),
            "a rejected field must not also be applied: {plan:?}"
        );
    }

    // ----- live snapshot and reload ----------------------------------------------

    fn candidate_from(live: &Config) -> Config {
        live.clone()
    }

    #[test]
    fn a_reload_swaps_only_reloadable_fields() {
        let mut old = base();
        old.data_dir = PathBuf::from("/live");
        old.master_key = Some("ab".repeat(32));
        let snapshot = ConfigSnapshot::new(old.clone());
        let held = snapshot.load();

        let mut candidate = candidate_from(&old);
        candidate.strategy = Strategy::Weighted;
        candidate.cache = CacheMode::Sqlite;
        candidate.cache_ttl_secs = 60;
        candidate.cache_max_entries = 5;
        candidate.max_concurrency = 4;
        candidate.request_timeout_secs = 120;
        candidate.log_filter = "rrouter=debug".into();
        candidate.log_format = LogFormat::Json;
        candidate.access_log = false;
        candidate.usage_retention_days = 7;
        candidate.cors_origins = vec!["https://console.example".into()];
        candidate.require_api_key = true;
        candidate.allow_remote_admin = true;
        candidate.admin_token = Some("new-admin-token".into());
        // Restart-only differences must be ignored by the swap.
        candidate.host = "0.0.0.0".parse().expect("ip");
        candidate.port = 20_999;
        candidate.data_dir = PathBuf::from("/other");
        candidate.static_dir = Some(PathBuf::from("/ext"));
        candidate.max_attempts = 8;
        candidate.retry_attempts = 3;
        candidate.retry_base_ms = 50;
        candidate.retry_max_ms = 900;
        candidate.connect_timeout_secs = 5;
        candidate.master_key = Some("cd".repeat(32));

        let outcome = snapshot.apply(&candidate);
        assert!(outcome.applied, "{:?}", outcome.plan);
        let live = snapshot.load();

        assert_eq!(live.strategy, Strategy::Weighted);
        assert_eq!(live.cache, CacheMode::Sqlite);
        assert_eq!(live.cache_ttl_secs, 60);
        assert_eq!(live.cache_max_entries, 5);
        assert_eq!(live.max_concurrency, 4);
        assert_eq!(live.request_timeout_secs, 120);
        assert_eq!(live.log_filter, "rrouter=debug");
        assert_eq!(live.log_format, LogFormat::Json);
        assert!(!live.access_log);
        assert_eq!(live.usage_retention_days, 7);
        assert_eq!(
            live.cors_origins,
            vec!["https://console.example".to_string()]
        );
        assert!(live.require_api_key);
        assert!(live.allow_remote_admin);
        assert_eq!(live.admin_token.as_deref(), Some("new-admin-token"));

        assert_eq!(live.host, old.host);
        assert_eq!(live.port, old.port);
        assert_eq!(live.data_dir, PathBuf::from("/live"));
        assert_eq!(live.static_dir, None);
        assert_eq!(live.max_attempts, 4);
        assert_eq!(live.retry_attempts, 1);
        assert_eq!(live.connect_timeout_secs, 15);
        assert_eq!(live.master_key, old.master_key);

        // A request that already took a snapshot keeps it, whole.
        assert_eq!(held.strategy, Strategy::Priority);
        assert_eq!(held.port, old.port);

        let restart: Vec<&str> = outcome
            .plan
            .restart_required
            .iter()
            .map(|entry| entry.field)
            .collect();
        for field in [
            "host",
            "port",
            "data_dir",
            "static_dir",
            "master_key",
            "max_attempts",
            "retry_attempts",
            "retry_base_ms",
            "retry_max_ms",
            "connect_timeout_secs",
        ] {
            assert!(restart.contains(&field), "{field} missing from {restart:?}");
        }
    }

    #[test]
    fn an_unchanged_candidate_keeps_the_same_arc_and_reports_no_change() {
        let snapshot = ConfigSnapshot::new(base());
        let before = snapshot.load();

        let outcome = snapshot.apply(&before);

        assert!(!outcome.applied);
        assert!(outcome.plan.is_empty());
        assert!(std::sync::Arc::ptr_eq(&before, &outcome.config));
        assert!(std::sync::Arc::ptr_eq(&before, &snapshot.load()));
    }

    #[test]
    fn a_rejected_field_is_reported_and_not_applied() {
        let old = Config::resolve(
            &env_map(&[
                ("RROUTER_HOST", "0.0.0.0"),
                ("RROUTER_ALLOW_REMOTE_ADMIN", "true"),
            ]),
            None,
        )
        .expect("old");
        let snapshot = ConfigSnapshot::new(old.config.clone());
        let mut candidate = candidate_from(&old.config);
        candidate.allow_remote_admin = false;

        let outcome = snapshot.apply(&candidate);

        assert!(!outcome.applied);
        assert_eq!(outcome.plan.rejected.len(), 1, "{:?}", outcome.plan);
        assert_eq!(outcome.plan.rejected[0].field, "allow_remote_admin");
        assert!(!outcome
            .plan
            .changed_fields()
            .any(|f| f == "allow_remote_admin"));
        assert!(
            snapshot.load().allow_remote_admin,
            "live value must not change"
        );
    }

    #[test]
    fn a_reload_outcome_never_carries_secret_values() {
        let mut old = base();
        old.admin_token = Some("secret-old-0123456789".into());
        let snapshot = ConfigSnapshot::new(old.clone());
        let mut candidate = candidate_from(&old);
        candidate.admin_token = Some("secret-new-abcdefghij".into());
        candidate.master_key = Some("ef".repeat(32));

        let outcome = snapshot.apply(&candidate);
        assert!(outcome.applied, "{:?}", outcome.plan);

        let rendered = format!("{:?}", outcome.plan);
        for secret in ["secret-old-0123456789", "secret-new-abcdefghij"] {
            assert!(!rendered.contains(secret), "{rendered}");
        }
        assert!(rendered.contains("rotated"), "{rendered}");
    }

    #[test]
    fn concurrent_readers_never_observe_a_mixed_snapshot() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let mut priority = base();
        priority.strategy = Strategy::Priority;
        priority.cache_ttl_secs = 10;
        let mut weighted = base();
        weighted.strategy = Strategy::Weighted;
        weighted.cache_ttl_secs = 20;

        let snapshot = std::sync::Arc::new(ConfigSnapshot::new(priority.clone()));
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        // Readers and the writer start together; otherwise a loaded machine can finish
        // all 500 writes before any reader thread is scheduled, making the total zero.
        let start = std::sync::Arc::new(std::sync::Barrier::new(5));
        let readers: Vec<_> = (0..4)
            .map(|_| {
                let snapshot = std::sync::Arc::clone(&snapshot);
                let stop = std::sync::Arc::clone(&stop);
                let start = std::sync::Arc::clone(&start);
                std::thread::spawn(move || {
                    start.wait();
                    let mut observed = 0u64;
                    while !stop.load(Ordering::Relaxed) {
                        let config = snapshot.load();
                        let pair = (config.strategy, config.cache_ttl_secs);
                        assert!(
                            pair == (Strategy::Priority, 10) || pair == (Strategy::Weighted, 20),
                            "a reader observed a mixed snapshot: {pair:?}"
                        );
                        observed += 1;
                    }
                    observed
                })
            })
            .collect();

        start.wait();
        for _ in 0..500 {
            snapshot.apply(&weighted);
            snapshot.apply(&priority);
        }
        stop.store(true, Ordering::Relaxed);
        let total: u64 = readers
            .into_iter()
            .map(|reader| reader.join().expect("reader"))
            .sum();
        assert!(total > 0, "readers must have observed a snapshot");
    }

    // ----- secret files -----------------------------------------------------------

    #[test]
    fn a_multi_line_key_file_is_refused() {
        let material = "deadbeef".repeat(8);
        let key = temp_file("multiline-key", &format!("{material}\n{material}\n"));
        let json = format!(r#"{{"master_key":{{"key_file":"{}"}}}}"#, key.display());
        let error = file_at("/etc/rrouter.json", &json).expect_err("multi-line key file");
        std::fs::remove_file(&key).ok();

        assert!(error.contains("single line"), "{error}");
        assert!(
            !error.contains(&material),
            "the material must never be echoed: {error}"
        );
    }

    #[test]
    fn a_key_file_may_end_with_whitespace() {
        let material = "cafe".repeat(16);
        let key = temp_file("newline-key", &format!("{material}\n\n"));
        let json = format!(r#"{{"master_key":{{"key_file":"{}"}}}}"#, key.display());
        let layer = file_at("/etc/rrouter.json", &json).expect("a trailing newline is normal");
        std::fs::remove_file(&key).ok();

        assert_eq!(layer.get("master_key"), Some(material.as_str()));
    }
}

#[cfg(all(test, feature = "otel"))]
mod otel_config_tests {
    use super::OtelConfig;

    #[test]
    fn a_valid_endpoint_service_and_ratio_round_trip() {
        let config = OtelConfig::validate(
            "  https://collector.example:4318/v1/traces  ",
            " rrouter ",
            0.25,
        )
        .expect("valid");
        assert_eq!(config.endpoint, "https://collector.example:4318/v1/traces");
        assert_eq!(config.service_name, "rrouter");
        assert_eq!(config.sample_ratio, 0.25);
    }

    #[test]
    fn invalid_endpoints_are_rejected() {
        for endpoint in [
            "",
            "collector:4318/v1/traces",
            "ftp://collector/v1/traces",
            "https:///v1/traces",
            "https://collector example/v1/traces",
        ] {
            assert!(
                OtelConfig::validate(endpoint, "rrouter", 1.0).is_err(),
                "{endpoint:?} must be rejected"
            );
        }
    }

    #[test]
    fn service_name_and_ratio_bounds_are_enforced() {
        assert!(OtelConfig::validate("https://c/v1/traces", "", 1.0).is_err());
        assert!(OtelConfig::validate("https://c/v1/traces", "a\u{0}b", 1.0).is_err());
        assert!(OtelConfig::validate("https://c/v1/traces", "rrouter", -0.1).is_err());
        assert!(OtelConfig::validate("https://c/v1/traces", "rrouter", 1.1).is_err());
        assert!(OtelConfig::validate("https://c/v1/traces", "rrouter", f64::INFINITY).is_err());
        assert!(OtelConfig::validate("https://c/v1/traces", "rrouter", 0.0).is_ok());
        assert!(OtelConfig::validate("https://c/v1/traces", "rrouter", 1.0).is_ok());
    }
}
