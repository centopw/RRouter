//! Resolved runtime configuration, read once from the environment at startup.

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;

use serde::Serialize;

/// Default listen port: 9router's, so existing client configs keep working.
pub const DEFAULT_PORT: u16 = 20128;
pub const DEFAULT_HOST: &str = "127.0.0.1";
pub const DEFAULT_DATA_DIR: &str = "./data";
const DEFAULT_REQUEST_TIMEOUT_SECS: u64 = 600;
const DEFAULT_CONNECT_TIMEOUT_SECS: u64 = 15;
const DEFAULT_LOG: &str = "rrouter=info";

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
}

impl Config {
    /// The `Access-Control-Allow-Origin` value to echo, if any.
    pub fn cors_origin_for(&self, origin: &str) -> Option<String> {
        if self.cors_origins.iter().any(|o| o == "*") {
            return Some("*".to_string());
        }
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
            None => DEFAULT_HOST.parse().unwrap_or(std::net::Ipv4Addr::LOCALHOST.into()),
            Some(raw) => raw.parse::<IpAddr>().unwrap_or_else(|_| {
                problems.push(format!("RROUTER_HOST={raw:?} is not an IP address"));
                std::net::Ipv4Addr::LOCALHOST.into()
            }),
        };
        let data_dir = PathBuf::from(var("RROUTER_DATA_DIR").unwrap_or_else(|| DEFAULT_DATA_DIR.into()));
        let master_key = var("RROUTER_MASTER_KEY").filter(|s| !s.is_empty());
        if let Some(key) = &master_key {
            if key.len() != 64 || !key.bytes().all(|b| b.is_ascii_hexdigit()) {
                problems.push(
                    "RROUTER_MASTER_KEY must be 64 hex characters (32 bytes)".to_string(),
                );
            }
        }
        let require_api_key = match var("RROUTER_REQUIRE_API_KEY") {
            None => false,
            Some(raw) => parse_bool(&raw).unwrap_or_else(|| {
                problems.push(format!("RROUTER_REQUIRE_API_KEY={raw:?} is not a boolean"));
                false
            }),
        };
        let request_timeout_secs =
            secs_var("RROUTER_REQUEST_TIMEOUT_SECS", DEFAULT_REQUEST_TIMEOUT_SECS, &mut problems);
        let connect_timeout_secs =
            secs_var("RROUTER_CONNECT_TIMEOUT_SECS", DEFAULT_CONNECT_TIMEOUT_SECS, &mut problems);
        let log_filter = var("RUST_LOG").unwrap_or_else(|| DEFAULT_LOG.into());
        let static_dir = var("RROUTER_STATIC_DIR").filter(|s| !s.is_empty()).map(PathBuf::from);
        let cors_origins = var("RROUTER_CORS_ORIGINS")
            .map(|raw| {
                raw.split(',')
                    .map(|o| o.trim().trim_end_matches('/').to_string())
                    .filter(|o| !o.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        let allow_remote_admin = match var("RROUTER_ALLOW_REMOTE_ADMIN") {
            None => false,
            Some(raw) => parse_bool(&raw).unwrap_or_else(|| {
                problems.push(format!("RROUTER_ALLOW_REMOTE_ADMIN={raw:?} is not a boolean"));
                false
            }),
        };

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
    match var(name) {
        None => default,
        Some(raw) => match raw.parse::<u64>() {
            Ok(v) if v > 0 => v,
            _ => {
                problems.push(format!("{name}={raw:?} is not a positive number of seconds"));
                default
            }
        },
    }
}

/// Serializes configuration for `--print-config`, never echoing the master key.
pub struct Redacted<'a>(&'a Config);

impl Serialize for Redacted<'_> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let c = self.0;
        let mut st = s.serialize_struct("Config", 12)?;
        st.serialize_field("host", &c.host.to_string())?;
        st.serialize_field("port", &c.port)?;
        st.serialize_field("data_dir", &c.data_dir.to_string_lossy())?;
        st.serialize_field("db_path", &c.db_path().to_string_lossy())?;
        st.serialize_field("master_key", &if c.master_key.is_some() { "env" } else { "file" })?;
        st.serialize_field("require_api_key", &c.require_api_key)?;
        st.serialize_field("request_timeout_secs", &c.request_timeout_secs)?;
        st.serialize_field("connect_timeout_secs", &c.connect_timeout_secs)?;
        st.serialize_field("static_dir", &c.static_dir.as_ref().map(|p| p.to_string_lossy()))?;
        st.serialize_field("cors_origins", &c.cors_origins)?;
        st.serialize_field("allow_remote_admin", &c.allow_remote_admin)?;
        st.serialize_field("log_filter", &c.log_filter)?;
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

    #[test]
    fn redacted_view_hides_the_key_material() {
        let cfg = Config {
            host: "127.0.0.1".parse().expect("ip"),
            port: DEFAULT_PORT,
            data_dir: PathBuf::from("/tmp/x"),
            master_key: Some("ab".repeat(32)),
            require_api_key: false,
            request_timeout_secs: 600,
            connect_timeout_secs: 15,
            log_filter: DEFAULT_LOG.into(),
            static_dir: None,
            cors_origins: vec![],
            allow_remote_admin: false,
        };
        let json = serde_json::to_string(&cfg.redacted()).expect("serialize");
        assert!(json.contains("\"port\":20128"));
        assert!(!json.contains(&"ab".repeat(32)));
        assert!(json.contains("\"master_key\":\"env\""));
    }

    #[test]
    fn cors_allows_configured_and_loopback_origins_only() {
        let mut cfg = Config {
            host: "127.0.0.1".parse().expect("ip"),
            port: DEFAULT_PORT,
            data_dir: PathBuf::from("/tmp/x"),
            master_key: None,
            require_api_key: false,
            request_timeout_secs: 600,
            connect_timeout_secs: 15,
            log_filter: DEFAULT_LOG.into(),
            static_dir: None,
            cors_origins: vec!["https://console.example".into()],
            allow_remote_admin: false,
        };
        assert_eq!(cfg.cors_origin_for("https://console.example"), Some("https://console.example".into()));
        assert_eq!(cfg.cors_origin_for("http://localhost:3000"), Some("http://localhost:3000".into()));
        assert_eq!(cfg.cors_origin_for("https://evil.example"), None);
        cfg.cors_origins = vec!["*".into()];
        assert_eq!(cfg.cors_origin_for("https://evil.example"), Some("*".into()));
    }
}
