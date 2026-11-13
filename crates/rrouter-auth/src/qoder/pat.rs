//! Personal access token (`pt-…`) → job token (`jt-…`) exchange.
//!
//! A `pt-` PAT cannot sign COSY requests directly, so it is exchanged once per process for
//! a short-lived job token. Two endpoints are involved, neither of them COSY-signed:
//!
//! 1. `POST {openapi}/api/v1/jobToken/exchange` with `{"personal_token": "pt-…"}` answers
//!    `{"token": "jt-…", "refresh_token"?, "expires_at"|"expires_in"?}`.
//! 2. `GET {openapi}/api/v1/userinfo` with `Authorization: Bearer <jt-…>` answers the
//!    account record; its `id` / `userId` / `user_id` is the `Cosy-User` value signing
//!    needs. When that call fails the connection's `credentials.extra.user_id` is used.
//!
//! The resolved credential is cached per (PAT, openapi origin). A cached entry is reused
//! until it is within [`REFRESH_BUFFER`] of its expiry, so normal traffic never blocks on
//! the exchange; concurrent first requests share one flight instead of each exchanging.
//! Neither the PAT nor the job token is logged, echoed in errors or persisted: they live
//! only in this process's cache and in the connection's existing `api_key` column.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::sync::Mutex as AsyncMutex;

use rrouter_core::{Error, Result};

use super::cosy::{self, QoderCredentials};
use super::{PAT_PREFIX, PROVIDER_ID};

/// Origin hosting the exchange and userinfo endpoints for the default (international)
/// region. A CN deployment points [`resolve_at`] at `https://openapi.qoder.com.cn`.
pub const OPENAPI_BASE: &str = "https://openapi.qoder.sh";

/// The client the reference implementation presents to the exchange. The endpoint is not
/// COSY-signed, but the impersonation headers are still part of the contract.
const USER_AGENT: &str = "qodercli/1.0.0";

/// How long a job token is reused before it is exchanged again. The response usually
/// carries [`expires_at`](https://en.wikipedia.org/wiki/ISO_8601); this is the fallback for
/// a response that carries no expiry at all.
const DEFAULT_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// Re-exchange once the cached token is this close to expiring, so a request never has to
/// fail on a job token that expires between the cache check and the upstream call.
const REFRESH_BUFFER: Duration = Duration::from_secs(5 * 60);

/// Deadline for one exchange or userinfo call.
const TIMEOUT: Duration = Duration::from_secs(15);

/// One resolved PAT: the job token, the account's user id, and when the token stops being
/// usable.
#[derive(Debug, Clone)]
struct Resolved {
    token: String,
    user_id: String,
    expires: Instant,
}

/// Resolve a connection's COSY-signable credential.
///
/// A `dt-` device token is returned unchanged (and costs no network call). A `pt-` PAT is
/// exchanged for a job token, and its user id is resolved from `userinfo` with
/// `credentials.extra.user_id` as the fallback when that call fails.
pub async fn resolve(
    http: &reqwest::Client,
    source: &crate::credentials::Credentials,
) -> Result<QoderCredentials> {
    resolve_at(http, source, OPENAPI_BASE).await
}

/// [`resolve`] against an explicit openapi origin.
///
/// The origin is part of the cache identity, so the same PAT used against two regions
/// (intl and CN) never shares a job token. Tests use this to point at a mock; it is also
/// the hook a CN deployment needs.
pub async fn resolve_at(
    http: &reqwest::Client,
    source: &crate::credentials::Credentials,
    openapi_base: &str,
) -> Result<QoderCredentials> {
    let parsed = cosy::signer(source)?;
    if !parsed.token.starts_with(PAT_PREFIX) {
        return Ok(parsed);
    }
    let resolved = resolve_pat(http, &parsed.token, &parsed.user_id, openapi_base).await?;
    let machine_id = if parsed.machine_id.is_empty() {
        // A PAT-only connection has no user id until userinfo answers, so the machine id
        // could not be derived by `cosy::signer`; derive it now that the id is known.
        cosy::derive_machine_id(&resolved.user_id)
    } else {
        parsed.machine_id
    };
    Ok(QoderCredentials {
        token: resolved.token,
        user_id: resolved.user_id,
        machine_id,
        name: parsed.name,
        email: parsed.email,
    })
}

/// Resolve one PAT to a job token plus user id, through the cache and one shared flight.
async fn resolve_pat(
    http: &reqwest::Client,
    pat: &str,
    fallback_user_id: &str,
    openapi_base: &str,
) -> Result<Resolved> {
    let key = cache_key(openapi_base, pat);
    if let Some(hit) = fresh(&key) {
        return Ok(hit);
    }
    // Every concurrent caller for this (PAT, origin) waits on the same flight, so exactly
    // one exchange goes out; the others read the value it stores.
    let flight = flight(&key);
    let _guard = flight.lock().await;
    if let Some(hit) = fresh(&key) {
        return Ok(hit);
    }

    let exchanged = exchange(http, pat, openapi_base).await?;
    let user_id = match user_info(http, &exchanged.token, openapi_base).await {
        Some(id) => id,
        None => fallback_user_id.trim().to_string(),
    };
    if user_id.is_empty() {
        return Err(Error::Unauthorized(
            "qoder: could not resolve the account's user id for this `pt-` token; \
             supply credentials.extra.user_id"
                .into(),
        ));
    }
    let resolved = Resolved {
        token: exchanged.token,
        user_id,
        expires: exchanged.expires,
    };
    store(&key, &resolved);
    Ok(resolved)
}

struct Exchanged {
    token: String,
    expires: Instant,
}

/// One exchange. The request carries the PAT in the body only; neither the response body
/// nor this error ever echoes a token.
async fn exchange(http: &reqwest::Client, pat: &str, openapi_base: &str) -> Result<Exchanged> {
    let url = format!("{}/api/v1/jobToken/exchange", trim(openapi_base));
    let response = http
        .post(&url)
        .timeout(TIMEOUT)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .header(reqwest::header::ACCEPT, "application/json")
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .header("cosy-version", cosy::IDE_VERSION)
        .header("cosy-clienttype", cosy::CLIENT_TYPE)
        .json(&serde_json::json!({ "personal_token": pat }))
        .send()
        .await
        .map_err(|e| {
            if e.is_timeout() {
                Error::Timeout(format!("qoder job-token exchange: {e}"))
            } else {
                Error::Transport(format!("qoder job-token exchange: {e}"))
            }
        })?;

    let status = response.status();
    let body = response
        .bytes()
        .await
        .map_err(|e| Error::Transport(format!("qoder job-token exchange body: {e}")))?;
    if !status.is_success() {
        // The upstream body is deliberately not forwarded: it is not needed to diagnose a
        // refused PAT, and a mistyped token in the request must not come back anywhere.
        return Err(Error::Upstream {
            provider: PROVIDER_ID.into(),
            status: status.as_u16(),
            body: "qoder: the personal access token was refused by the job-token exchange; \
                   check that credentials.api_key holds a current `pt-` token"
                .into(),
            payload: None,
        });
    }
    let payload: Value = serde_json::from_slice(&body)
        .map_err(|_| Error::Translation("qoder: job-token exchange is not JSON".into()))?;
    let token = payload
        .get("token")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .ok_or_else(|| {
            Error::Unauthorized("qoder: the job-token exchange answered without a job token".into())
        })?;
    Ok(Exchanged {
        token: token.to_string(),
        expires: expiry(&payload),
    })
}

/// The account's user id for a job token, or `None` on any failure (the caller falls back
/// to the connection's stored id).
async fn user_info(http: &reqwest::Client, token: &str, openapi_base: &str) -> Option<String> {
    let url = format!("{}/api/v1/userinfo", trim(openapi_base));
    let response = http
        .get(&url)
        .timeout(TIMEOUT)
        .header(reqwest::header::AUTHORIZATION, format!("Bearer {token}"))
        .header(reqwest::header::ACCEPT, "application/json")
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let payload: Value = response.json().await.ok()?;
    ["id", "userId", "user_id"]
        .iter()
        .find_map(|field| payload.get(*field).and_then(Value::as_str))
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
}

/// Absolute expiry of an exchange response.
///
/// `expires_at` is RFC 3339 (or unix seconds); `expires_in` is milliseconds, matching the
/// reference client's `Date.now() + expires_in`. A response with neither gets
/// [`DEFAULT_TTL`].
fn expiry(payload: &Value) -> Instant {
    if let Some(at) = payload.get("expires_at").and_then(unix_of) {
        let now = unix_now();
        return Instant::now() + Duration::from_secs(at.saturating_sub(now).max(0) as u64);
    }
    if let Some(ms) = payload.get("expires_in").and_then(Value::as_u64) {
        if ms > 0 {
            return Instant::now() + Duration::from_millis(ms);
        }
    }
    Instant::now() + DEFAULT_TTL
}

/// Unix seconds from a number (seconds, or milliseconds when implausibly large) or from an
/// RFC 3339 string.
fn unix_of(value: &Value) -> Option<i64> {
    if let Some(number) = value.as_i64() {
        // A millisecond stamp is ~1000x a seconds stamp; 10^11 seconds is year 5138, so
        // anything larger is a millisecond value.
        return Some(if number > 100_000_000_000 {
            number / 1_000
        } else {
            number
        });
    }
    parse_rfc3339(value.as_str()?)
}

/// Minimal RFC 3339 (`2026-04-08T18:59:59Z`); an explicit offset is parsed as UTC, which is
/// only used to decide when to re-exchange.
fn parse_rfc3339(raw: &str) -> Option<i64> {
    let bytes = raw.as_bytes();
    if bytes.len() < 19 {
        return None;
    }
    let number = |range: std::ops::Range<usize>| raw.get(range)?.parse::<i64>().ok();
    let (year, month, day) = (number(0..4)?, number(5..7)?, number(8..10)?);
    let (hour, minute, second) = (number(11..13)?, number(14..16)?, number(17..19)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    Some(days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second)
}

/// Days since 1970-01-01 (H. Hinnant).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or_default()
}

fn trim(base: &str) -> &str {
    base.trim().trim_end_matches('/')
}

/// Cache identity: one entry per (PAT, origin). The PAT is hashed, never stored as a key.
fn cache_key(openapi_base: &str, pat: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(trim(openapi_base).as_bytes());
    hash.update(b"\0");
    hash.update(pat.as_bytes());
    let digest = hash.finalize();
    let mut out = String::with_capacity(32);
    for byte in &digest[..16] {
        use std::fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

type Cache = Mutex<HashMap<String, Resolved>>;

fn cache() -> &'static Cache {
    static CACHE: LazyLock<Cache> = LazyLock::new(|| Mutex::new(HashMap::new()));
    &CACHE
}

fn fresh(key: &str) -> Option<Resolved> {
    let mut cache = cache().lock().unwrap_or_else(|e| e.into_inner());
    let entry = cache.get(key)?;
    if entry.expires.saturating_duration_since(Instant::now()) <= REFRESH_BUFFER {
        cache.remove(key);
        return None;
    }
    Some(entry.clone())
}

fn store(key: &str, resolved: &Resolved) {
    cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(key.to_string(), resolved.clone());
}

type Flights = Mutex<HashMap<String, Arc<AsyncMutex<()>>>>;

fn flights() -> &'static Flights {
    static FLIGHTS: LazyLock<Flights> = LazyLock::new(|| Mutex::new(HashMap::new()));
    &FLIGHTS
}

/// The lock one (PAT, origin) resolves under.
fn flight(key: &str) -> Arc<AsyncMutex<()>> {
    Arc::clone(
        flights()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(key.to_string())
            .or_default(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn an_rfc3339_expiry_is_understood() {
        assert_eq!(parse_rfc3339("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_rfc3339("2026-04-08T18:59:59Z"), Some(1_775_674_799));
        assert_eq!(parse_rfc3339("not a date"), None);
    }

    #[test]
    fn an_expiry_prefers_a_timestamp_then_milliseconds_then_the_default() {
        let far = Instant::now() + DEFAULT_TTL - Duration::from_secs(60);
        let absolute = json!({"expires_at": "2999-01-01T00:00:00Z"});
        assert!(expiry(&absolute) > far);

        let millis = json!({"expires_in": 60_000});
        let remaining = expiry(&millis).saturating_duration_since(Instant::now());
        assert!(remaining > Duration::from_secs(50) && remaining <= Duration::from_secs(60));

        let none = json!({});
        assert!(expiry(&none) > far);
    }

    #[test]
    fn the_cache_key_separates_pats_and_origins_without_embedding_the_pat() {
        let key = cache_key(OPENAPI_BASE, "pt-secret");
        assert_eq!(key, cache_key(&format!("{OPENAPI_BASE}/"), "pt-secret"));
        assert_ne!(key, cache_key("https://openapi.qoder.com.cn", "pt-secret"));
        assert_ne!(key, cache_key(OPENAPI_BASE, "pt-other"));
        assert_eq!(key.len(), 32);
        assert!(!key.contains("pt-"), "the PAT is hashed, not embedded");
    }
}
