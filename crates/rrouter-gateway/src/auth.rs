//! Dashboard authentication plumbing: the one-time setup code, session cookies, login
//! throttling and the CSRF header.
//!
//! `/v1` traffic is untouched by this module; gateway keys authenticate model clients.
//! Here "auth" always means a person signed in to the dashboard.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::http::header::{COOKIE, SET_COOKIE};
use axum::http::{HeaderMap, HeaderValue};
use axum::response::Response;
use rrouter_store::{hash_secret, random_setup_code};

/// The only cookie the dashboard sets.
pub const SESSION_COOKIE: &str = "rrouter_session";
/// Custom header required on cookie-authenticated state-changing calls. A cross-site
/// form cannot set it, so `SameSite=Lax` plus this header closes CSRF.
pub const CSRF_HEADER: &str = "x-rrouter-csrf";
/// Server-side session lifetime without "keep me signed in": one browser session, capped.
pub const SESSION_SECS: i64 = 12 * 60 * 60;
/// Lifetime with "keep me signed in", also the cookie's `Max-Age`.
pub const REMEMBER_SECS: i64 = 30 * 24 * 60 * 60;

const SETUP_FILE: &str = "setup-code.txt";
const WINDOW: Duration = Duration::from_secs(60);
const LIMIT: u32 = 5;

struct Attempt {
    window_start: Instant,
    count: u32,
}

/// Per-instance auth state: the setup code (in memory and on disk) and the sign-in
/// throttle. Sessions themselves live in the store.
pub struct Auth {
    setup_path: PathBuf,
    setup_code: Mutex<Option<String>>,
    /// Set once a user exists (or setup consumed the code): a code must never be minted
    /// again after that, or setup would silently reopen.
    retired: Mutex<bool>,
    attempts: Mutex<HashMap<String, Attempt>>,
}

impl Auth {
    /// Load or create the setup code. When users already exist the file is removed:
    /// setup is over and the code must not be usable.
    pub fn load(data_dir: &Path, users_exist: bool) -> Self {
        let auth = Self {
            setup_path: data_dir.join(SETUP_FILE),
            setup_code: Mutex::new(read_code(&data_dir.join(SETUP_FILE))),
            retired: Mutex::new(false),
            attempts: Mutex::new(HashMap::new()),
        };
        if users_exist {
            auth.retire();
        } else if let Some(code) = auth.code() {
            tracing::info!("Setup code: {code}");
        }
        auth
    }

    /// The current code, creating and persisting one on first use.
    pub fn code(&self) -> Option<String> {
        let retired = self.retired.lock().unwrap_or_else(|e| e.into_inner());
        if *retired {
            return None;
        }
        let mut slot = self.setup_code.lock().unwrap_or_else(|e| e.into_inner());
        if slot.is_none() {
            *slot = Some(random_setup_code());
        }
        let code = slot.clone()?;
        write_code(&self.setup_path, &code);
        Some(code)
    }

    /// Constant-time comparison against the current code. No code means "setup done".
    pub fn verify(&self, presented: &str) -> bool {
        let Some(expected) = self.code() else {
            return false;
        };
        constant_time_eq(presented.trim(), &expected)
    }

    /// Consume the code: used once, then setup is closed.
    pub fn retire(&self) {
        *self.retired.lock().unwrap_or_else(|e| e.into_inner()) = true;
        let mut slot = self.setup_code.lock().unwrap_or_else(|e| e.into_inner());
        *slot = None;
        std::fs::remove_file(&self.setup_path).ok();
    }

    /// `Some(wait_secs)` when this key may not try again yet. One key per email and one
    /// per client IP, so neither a single account nor a single host can be hammered.
    pub fn throttle(&self, key: &str) -> Option<u64> {
        let mut map = self.attempts.lock().unwrap_or_else(|e| e.into_inner());
        let entry = map.get_mut(key)?;
        if entry.window_start.elapsed() >= WINDOW {
            map.remove(key);
            return None;
        }
        if entry.count >= LIMIT {
            return Some((WINDOW - entry.window_start.elapsed()).as_secs().max(1));
        }
        None
    }

    pub fn note_failure(&self, key: &str) {
        let mut map = self.attempts.lock().unwrap_or_else(|e| e.into_inner());
        let entry = map.entry(key.to_string()).or_insert(Attempt {
            window_start: Instant::now(),
            count: 0,
        });
        if entry.window_start.elapsed() >= WINDOW {
            entry.window_start = Instant::now();
            entry.count = 0;
        }
        entry.count += 1;
    }

    pub fn clear_failures(&self, key: &str) {
        self.attempts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(key);
    }
}

fn read_code(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|raw| raw.trim().to_string())
        .filter(|raw| !raw.is_empty())
}

fn write_code(path: &Path, code: &str) {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).ok();
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
        {
            use std::io::Write;
            let _ = file.write_all(format!("{code}\n").as_bytes());
        }
    }
    #[cfg(not(unix))]
    {
        let _ = std::fs::write(path, format!("{code}\n"));
    }
}

/// The peer address, when axum was served with connect info (it is). In-process
/// `oneshot` calls carry none, so this is an `Option` an extractor can always satisfy.
pub struct Peer(pub Option<SocketAddr>);

impl<S: Send + Sync> axum::extract::FromRequestParts<S> for Peer {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        _state: &S,
    ) -> Result<Self, Self::Rejection> {
        Ok(Peer(
            parts
                .extensions
                .get::<axum::extract::ConnectInfo<SocketAddr>>()
                .map(|info| info.0),
        ))
    }
}

/// The session token from the `Cookie` header, if present.
pub fn session_token(headers: &HeaderMap) -> Option<String> {
    let raw = headers.get(COOKIE)?.to_str().ok()?;
    for part in raw.split(';') {
        let part = part.trim();
        if let Some(value) = part.strip_prefix(&format!("{SESSION_COOKIE}=")) {
            let value = value.trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

/// SHA-256 of the cookie token: the only form the database stores.
pub fn session_key(token: &str) -> String {
    hash_secret(token)
}

/// Whether the request arrived over HTTPS (directly or through a proxy that told us).
pub fn is_secure(headers: &HeaderMap) -> bool {
    headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|proto| proto.eq_ignore_ascii_case("https"))
}

pub fn set_session_cookie(resp: &mut Response, token: &str, remember: bool, secure: bool) {
    let mut cookie = format!("{SESSION_COOKIE}={token}; Path=/; HttpOnly; SameSite=Lax");
    if remember {
        cookie.push_str(&format!("; Max-Age={REMEMBER_SECS}"));
    }
    if secure {
        cookie.push_str("; Secure");
    }
    if let Ok(value) = HeaderValue::from_str(&cookie) {
        resp.headers_mut().append(SET_COOKIE, value);
    }
}

pub fn clear_session_cookie(resp: &mut Response, secure: bool) {
    let mut cookie = format!("{SESSION_COOKIE}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0");
    if secure {
        cookie.push_str("; Secure");
    }
    if let Ok(value) = HeaderValue::from_str(&cookie) {
        resp.headers_mut().append(SET_COOKIE, value);
    }
}

/// Cookie-authenticated writes must carry `x-rrouter-csrf: 1`.
pub fn csrf_ok(headers: &HeaderMap) -> bool {
    headers
        .get(CSRF_HEADER)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|value| value == "1")
}

/// The peer address for throttling. Falls back to a proxy header, then to a shared key.
pub fn client_ip(headers: &HeaderMap, peer: Option<SocketAddr>) -> String {
    if let Some(forwarded) = headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|raw| raw.split(',').next())
        .map(|raw| raw.trim().to_string())
        .filter(|raw| !raw.is_empty())
    {
        return forwarded;
    }
    peer.map(|addr| addr.ip().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

/// Length-independent comparison: the length check leaks only the length, which the
/// setup-code format already fixes.
pub fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_code_lives_on_disk_until_retired() {
        let dir = std::env::temp_dir().join(format!("rrouter-auth-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("dir");
        let auth = Auth::load(&dir, false);
        let code = auth.code().expect("generated");
        assert_eq!(code.len(), 9, "XXXX-XXXX");
        assert!(auth.verify(&code));
        assert!(!auth.verify("0000-0000"));
        // A restart reads the same code back instead of minting a new one.
        let again = Auth::load(&dir, false);
        assert_eq!(again.code().as_deref(), Some(code.as_str()));
        again.retire();
        assert!(!again.verify(&code));
        assert!(!dir.join(SETUP_FILE).exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn login_throttle_is_five_per_minute_per_key() {
        let dir = std::env::temp_dir().join(format!("rrouter-auth-{}", uuid::Uuid::new_v4()));
        let auth = Auth::load(&dir, true);
        for _ in 0..5 {
            assert!(auth.throttle("a@b.c|1.2.3.4").is_none());
            auth.note_failure("a@b.c|1.2.3.4");
        }
        assert!(auth.throttle("a@b.c|1.2.3.4").is_some());
        assert!(auth.throttle("other@b.c|1.2.3.4").is_none());
        auth.clear_failures("a@b.c|1.2.3.4");
        assert!(auth.throttle("a@b.c|1.2.3.4").is_none());
    }

    #[test]
    fn cookie_round_trip() {
        use axum::body::Body;
        let mut resp = Response::new(Body::empty());
        set_session_cookie(&mut resp, "tok", true, true);
        let header = resp
            .headers()
            .get(SET_COOKIE)
            .and_then(|v| v.to_str().ok())
            .expect("cookie");
        assert!(header.starts_with("rrouter_session=tok"));
        assert!(header.contains("HttpOnly"));
        assert!(header.contains("SameSite=Lax"));
        assert!(header.contains("Max-Age=2592000"));
        assert!(header.contains("Secure"));

        let mut headers = HeaderMap::new();
        headers.insert(
            COOKIE,
            HeaderValue::from_static("a=1; rrouter_session=tok; b=2"),
        );
        assert_eq!(session_token(&headers).as_deref(), Some("tok"));
        assert!(!csrf_ok(&headers));
        headers.insert("x-rrouter-csrf", HeaderValue::from_static("1"));
        assert!(csrf_ok(&headers));
    }
}
