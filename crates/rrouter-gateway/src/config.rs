//! Resolved runtime configuration, read once from the environment at startup.

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;

use rrouter_router::Strategy;
use serde::Serialize;

/// Default listen port: 9router's, so existing client configs keep working.
pub const DEFAULT_PORT: u16 = 20128;
pub const DEFAULT_HOST: &str = "127.0.0.1";
pub const DEFAULT_DATA_DIR: &str = "./data";
const DEFAULT_REQUEST_TIMEOUT_SECS: u64 = 600;
const DEFAULT_CONNECT_TIMEOUT_SECS: u64 = 15;
const DEFAULT_LOG: &str = "rrouter=info";
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

/// Env var as parsed values. Always produced even when the env is malformed:
/// `parse` records the problem and callers `expect_valid` instead of panicking.
#[derive(Debug, Default)]
pub struct ParseOutcome {
    pub config: Option<Config>,
    pub error: Option<String>,
}

impl Config {
    /// Read all `RROUTER_*` variables. Returns an error message instead of panicking
    /// so `--print-config` and the binary can report it cleanly.
    pub fn from_env() -> Result<Self, String> {
        let mut problems: Vec<String> = Vec::new();

        let port = match var("RROUTER_PORT") {
            None => DEFAULT_PORT,
            Some(raw) => raw.parse::<u16>().unwrap_or_else(|_| {
                problems.push(format!("RROUTER_PORT={raw:?} is not a port number"));
                0
            }),
        };
        let host = match var("RROUTER_HOST") {
            None => DEFAULT_HOST
                .parse()
                .unwrap_or(std::net::Ipv4Addr::LOCALHOST.into()),
            Some(raw) => raw.parse::<IpAddr>().unwrap_or_else(|_| {
                problems.push(format!("RROUTER_HOST={raw:?} is not an IP address"));
                std::net::Ipv4Addr::LOCALHOST.into()
            }),
        };
        let data_dir =
            PathBuf::from(var("RROUTER_DATA_DIR").unwrap_or_else(|| DEFAULT_DATA_DIR.into()));
        let master_key = var("RROUTER_MASTER_KEY").filter(|s| !s.is_empty());
        if let Some(key) = &master_key {
            if key.len() != 64 || !key.bytes().all(|b| b.is_ascii_hexdigit()) {
                problems
                    .push("RROUTER_MASTER_KEY must be 64 hex characters (32 bytes)".to_string());
            }
        }
        let require_api_key = match var("RROUTER_REQUIRE_API_KEY") {
            None => false,
            Some(raw) => parse_bool(&raw).unwrap_or_else(|| {
                problems.push(format!("RROUTER_REQUIRE_API_KEY={raw:?} is not a boolean"));
                false
            }),
        };
        let request_timeout_secs = secs_var(
            "RROUTER_REQUEST_TIMEOUT_SECS",
            DEFAULT_REQUEST_TIMEOUT_SECS,
            &mut problems,
        );
        let connect_timeout_secs = secs_var(
            "RROUTER_CONNECT_TIMEOUT_SECS",
            DEFAULT_CONNECT_TIMEOUT_SECS,
            &mut problems,
        );
        let log_filter = var("RUST_LOG").unwrap_or_else(|| DEFAULT_LOG.into());
        let static_dir = var("RROUTER_STATIC_DIR")
            .filter(|s| !s.is_empty())
            .map(PathBuf::from);
        let cors_origins: Vec<String> = var("RROUTER_CORS_ORIGINS")
            .map(|raw| {
                raw.split(',')
                    .map(|o| o.trim().trim_end_matches('/').to_string())
                    .filter(|o| !o.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        // A wildcard would expose the (unauthenticated by default, loopback-bound)
        // admin API to every web origin. Refuse it loudly rather than honour it.
        if cors_origins.iter().any(|o| o == "*") {
            problems.push(
                "RROUTER_CORS_ORIGINS=* is not allowed: list explicit origins instead".into(),
            );
        }
        let allow_remote_admin = match var("RROUTER_ALLOW_REMOTE_ADMIN") {
            None => false,
            Some(raw) => parse_bool(&raw).unwrap_or_else(|| {
                problems.push(format!(
                    "RROUTER_ALLOW_REMOTE_ADMIN={raw:?} is not a boolean"
                ));
                false
            }),
        };
        // An admin token replaces the loopback rule rather than adding to it: behind a
        // reverse proxy the peer is always the proxy, so loopback proves nothing.
        let admin_token = var("RROUTER_ADMIN_TOKEN").filter(|s| !s.is_empty());
        let max_concurrency = usize_var("RROUTER_MAX_CONCURRENCY", 0, &mut problems);
        let max_attempts = clamp_attempts(usize_var(
            "RROUTER_MAX_ATTEMPTS",
            DEFAULT_MAX_ATTEMPTS,
            &mut problems,
        ));
        // Retries are clamped, never refused: an operator writing 99 wants "retry hard",
        // and garbage still stops startup like every other numeric knob.
        let retry_attempts = clamped_var(
            "RROUTER_RETRY_ATTEMPTS",
            DEFAULT_RETRY_ATTEMPTS,
            0,
            MAX_RETRY_ATTEMPTS,
            &mut problems,
        );
        let retry_base_ms = clamped_var(
            "RROUTER_RETRY_BASE_MS",
            DEFAULT_RETRY_BASE_MS,
            0,
            MAX_RETRY_BASE_MS,
            &mut problems,
        ) as u64;
        let retry_max_ms = clamped_var(
            "RROUTER_RETRY_MAX_MS",
            DEFAULT_RETRY_MAX_MS,
            0,
            MAX_RETRY_MAX_MS,
            &mut problems,
        ) as u64;
        let shutdown_grace_secs = secs_var_allow_zero(
            "RROUTER_SHUTDOWN_GRACE_SECS",
            DEFAULT_SHUTDOWN_GRACE_SECS,
            &mut problems,
        );
        // An unknown strategy is refused at startup rather than silently treated as
        // `priority`: the operator asked for different routing and must know it did not
        // take effect.
        let strategy = strategy_setting(var("RROUTER_STRATEGY"), &mut problems);

        // An unknown cache backend is refused at startup for the same reason as an
        // unknown strategy: silently staying off would look like the cache is working.
        let cache = match var("RROUTER_CACHE") {
            None => CacheMode::Off,
            Some(raw) => match CacheMode::parse(&raw) {
                Some(mode) => mode,
                None => {
                    problems.push(format!("RROUTER_CACHE={raw:?} is not one of off, sqlite"));
                    CacheMode::Off
                }
            },
        };
        let cache_ttl_secs = clamp_cache_ttl(clamped_var(
            "RROUTER_CACHE_TTL_SECS",
            DEFAULT_CACHE_TTL_SECS,
            1,
            MAX_CACHE_TTL_SECS,
            &mut problems,
        )) as u64;
        let cache_max_entries = clamp_cache_entries(clamped_var(
            "RROUTER_CACHE_MAX_ENTRIES",
            DEFAULT_CACHE_MAX_ENTRIES,
            1,
            MAX_CACHE_ENTRIES,
            &mut problems,
        ));
        let log_format = log_format_setting(var("RROUTER_LOG_FORMAT"), &mut problems);
        let access_log = access_log_setting(var("RROUTER_ACCESS_LOG"), &mut problems);
        // Same rule as the cache bounds: an oversized number is clamped, garbage stops
        // startup. `0` disables history deletion, not the sweep itself.
        let usage_retention_days = clamped_var(
            USAGE_RETENTION_VAR,
            DEFAULT_USAGE_RETENTION_DAYS,
            0,
            MAX_USAGE_RETENTION_DAYS,
            &mut problems,
        );

        if problems.is_empty() {
            Ok(Self {
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
            })
        } else {
            Err(problems.join("; "))
        }
    }

    pub fn socket_addr(&self) -> SocketAddr {
        SocketAddr::new(self.host, self.port)
    }

    pub fn db_path(&self) -> PathBuf {
        self.data_dir.join("rrouter.db")
    }

    /// The secrets-free view printed by `--print-config`.
    pub fn redacted(&self) -> Redacted<'_> {
        Redacted(self)
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

fn secs_var(name: &str, default: u64, problems: &mut Vec<String>) -> u64 {
    secs_var_in(name, default, false, problems)
}

/// Like [`secs_var`] but `0` is a meaningful value (wait indefinitely).
fn secs_var_allow_zero(name: &str, default: u64, problems: &mut Vec<String>) -> u64 {
    secs_var_in(name, default, true, problems)
}

fn secs_var_in(name: &str, default: u64, allow_zero: bool, problems: &mut Vec<String>) -> u64 {
    match var(name) {
        None => default,
        Some(raw) => match raw.parse::<u64>() {
            Ok(v) if v > 0 || (allow_zero && v == 0) => v,
            _ => {
                problems.push(if allow_zero {
                    format!("{name}={raw:?} is not a number of seconds (0 disables the wait)")
                } else {
                    format!("{name}={raw:?} is not a positive number of seconds")
                });
                default
            }
        },
    }
}

/// Non-negative count. `0` means "unlimited" for the callers that do not clamp.
fn usize_var(name: &str, default: usize, problems: &mut Vec<String>) -> usize {
    match var(name) {
        None => default,
        Some(raw) => raw.parse::<usize>().unwrap_or_else(|_| {
            problems.push(format!("{name}={raw:?} is not a non-negative integer"));
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
    name: &str,
    default: usize,
    min: usize,
    max: usize,
    problems: &mut Vec<String>,
) -> usize {
    match var(name) {
        None => default,
        Some(raw) => match raw.parse::<usize>() {
            Ok(v) => v.clamp(min, max),
            Err(_) => {
                problems.push(format!("{name}={raw:?} is not a non-negative integer"));
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
fn log_format_setting(raw: Option<String>, problems: &mut Vec<String>) -> LogFormat {
    match raw {
        None => LogFormat::Text,
        Some(raw) => LogFormat::parse(&raw).unwrap_or_else(|| {
            problems.push(format!(
                "RROUTER_LOG_FORMAT={raw:?} is not one of text, json"
            ));
            LogFormat::Text
        }),
    }
}

/// Equal-priority ordering. Unset means `priority`; a name the router does not know is a
/// startup error, because silently routing differently than asked is the worst outcome.
fn strategy_setting(raw: Option<String>, problems: &mut Vec<String>) -> Strategy {
    match raw {
        None => Strategy::Priority,
        Some(raw) => Strategy::parse(&raw).unwrap_or_else(|| {
            problems.push(format!(
                "RROUTER_STRATEGY={raw:?} is not one of priority, weighted, least_latency, \
                 cheapest"
            ));
            Strategy::Priority
        }),
    }
}

/// The per-request access line is on unless the operator explicitly turns it off.
fn access_log_setting(raw: Option<String>, problems: &mut Vec<String>) -> bool {
    match raw {
        None => true,
        Some(raw) => parse_bool(&raw).unwrap_or_else(|| {
            problems.push(format!("RROUTER_ACCESS_LOG={raw:?} is not a boolean"));
            true
        }),
    }
}

/// Serializes configuration for `--print-config`, never echoing the master key or the
/// admin token.
pub struct Redacted<'a>(&'a Config);

impl Serialize for Redacted<'_> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let c = self.0;
        let mut st = s.serialize_struct("Config", 26)?;
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
            strategy_setting(Some("cheapest".into()), &mut problems),
            Strategy::Cheapest
        );
        assert!(
            problems.is_empty(),
            "valid settings must not report problems"
        );
        assert_eq!(
            strategy_setting(Some("round_robin".into()), &mut problems),
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
            clamped_var(name, default, 0, max, problems)
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
                USAGE_RETENTION_VAR,
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
            log_format_setting(Some("json".into()), &mut problems),
            LogFormat::Json
        );
        assert!(!access_log_setting(Some("false".into()), &mut problems));
        assert!(problems.is_empty());
        log_format_setting(Some("yaml".into()), &mut problems);
        access_log_setting(Some("maybe".into()), &mut problems);
        assert_eq!(
            problems.len(),
            2,
            "both bad values must fail startup: {problems:?}"
        );
    }
}
