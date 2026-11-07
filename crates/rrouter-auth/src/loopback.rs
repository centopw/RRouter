//! One-shot loopback HTTP listener for the OAuth redirect.
//!
//! Scope is deliberately minimal: bind `127.0.0.1` on a chosen port, accept requests
//! until the provider redirects to the registered callback path, hand the query
//! parameters to the waiting flow, and shut down. No routing framework, no TLS, no
//! access from another host.
//!
//! Security properties this module is responsible for:
//! - binds `127.0.0.1` only (never `0.0.0.0`), so a LAN peer cannot inject a code;
//! - accepts exactly one callback and then stops listening;
//! - rejects a request whose `state` does not match, and reports it as an error
//!   rather than silently ignoring it (a mismatch means someone else completed a
//!   flow, and continuing would bind the wrong account);
//! - times out, so an abandoned login cannot hold a port forever.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;

use rrouter_core::{Error, Result};

/// Result of a completed redirect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Callback {
    pub code: String,
    pub state: String,
}

/// A listening loopback server; drop or [`LoopbackServer::close`] to release the port.
pub struct LoopbackServer {
    listener: TcpListener,
    /// `localhost` can resolve to ::1 first, so a named host listens on both loopback
    /// families; a browser that picks either one still reaches the callback.
    listener_v6: Option<TcpListener>,
    /// Host the provider registered for this client (usually `localhost` or 127.0.0.1).
    host: String,
    pub port: u16,
    pub callback_path: String,
    local_addr: SocketAddr,
}

impl LoopbackServer {
    /// Bind `127.0.0.1` on `port` (`0` picks an ephemeral port).
    pub async fn bind(port: u16, callback_path: &str, host: &str) -> Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", port)).await.map_err(|e| {
            Error::Transport(format!(
                "cannot bind 127.0.0.1:{port} for the OAuth callback: {e}"
            ))
        })?;
        let local_addr = listener
            .local_addr()
            .map_err(|e| Error::Transport(format!("loopback local_addr: {e}")))?;
        let listener_v6 = if host == "localhost" {
            TcpListener::bind(("::1", local_addr.port())).await.ok()
        } else {
            None
        };
        Ok(Self {
            listener,
            listener_v6,
            host: host.to_string(),
            port: local_addr.port(),
            callback_path: normalize_path(callback_path),
            local_addr,
        })
    }

    pub fn redirect_uri(&self) -> String {
        format!("http://{}:{}{}", self.host, self.port, self.callback_path)
    }

    /// Wait for the provider's redirect and return its query parameters.
    ///
    /// Requests to any other path get `404` and are consumed without ending the wait;
    /// the first request to the callback path with a matching `state` ends it.
    /// A callback whose `state` mismatches, or that carries `error=`, terminates the
    /// wait with an error — continuing would be indistinguishable from success.
    pub async fn wait(self, expected_state: &str, max_wait: Duration) -> Result<Callback> {
        let deadline = tokio::time::Instant::now() + max_wait;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err(Error::Timeout(
                    "timed out waiting for the OAuth redirect".into(),
                ));
            }
            let accepted = timeout(remaining, async {
                match &self.listener_v6 {
                    Some(v6) => tokio::select! {
                        accepted = self.listener.accept() => accepted,
                        accepted = v6.accept() => accepted,
                    },
                    None => self.listener.accept().await,
                }
            })
            .await;
            let (stream, peer) = match accepted {
                Err(_) => {
                    return Err(Error::Timeout(
                        "timed out waiting for the OAuth redirect".into(),
                    ))
                }
                Ok(Err(e)) => return Err(Error::Transport(format!("loopback accept: {e}"))),
                Ok(Ok(pair)) => pair,
            };
            // Defence in depth: the listener is already loopback-bound.
            if !peer.ip().is_loopback() {
                serve_reject(stream).await;
                continue;
            }
            match read_request(stream).await {
                Some((path, query, mut stream)) => {
                    let params = parse_query(&query);
                    if path != self.callback_path {
                        respond(&mut stream, 404, NOT_FOUND_PAGE).await;
                        continue;
                    }
                    match interpret(&params, expected_state) {
                        Ok(cb) => {
                            respond(&mut stream, 200, SUCCESS_PAGE).await;
                            return Ok(cb);
                        }
                        Err(outcome) => {
                            respond(&mut stream, 400, FAILURE_PAGE).await;
                            return Err(outcome);
                        }
                    }
                }
                None => continue,
            }
        }
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }
}

/// Turn callback query parameters into a [`Callback`] or a caller-facing error.
fn interpret(params: &HashMap<String, String>, expected_state: &str) -> Result<Callback> {
    if let Some(err) = params.get("error") {
        let description = params
            .get("error_description")
            .map(String::as_str)
            .unwrap_or("no description");
        return Err(Error::Unauthorized(format!(
            "authorization was refused: {err} ({description})"
        )));
    }
    let code = params
        .get("code")
        .filter(|c| !c.is_empty())
        .ok_or_else(|| Error::BadRequest("OAuth redirect carried no code".into()))?;
    let state = params
        .get("state")
        .ok_or_else(|| Error::BadRequest("OAuth redirect carried no state".into()))?;
    // Constant-time comparison is unnecessary here: `state` is a one-shot random
    // value consumed immediately, and an attacker who can read it already has it.
    if state != expected_state {
        return Err(Error::Unauthorized(
            "OAuth state mismatch: the redirect belongs to a different login attempt".into(),
        ));
    }
    Ok(Callback {
        code: code.clone(),
        state: state.clone(),
    })
}

/// Read one HTTP request head and return `(path, query, stream)`.
async fn read_request(mut stream: TcpStream) -> Option<(String, String, TcpStream)> {
    let mut buf = Vec::with_capacity(2048);
    let mut chunk = [0u8; 1024];
    // Bound the read so a slow or hostile peer cannot hold resources.
    let head_end = loop {
        if buf.len() > 16 * 1024 {
            return None;
        }
        let read = timeout(Duration::from_secs(10), stream.read(&mut chunk))
            .await
            .ok()?
            .ok()?;
        if read == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..read]);
        if let Some(pos) = find_head_end(&buf) {
            break pos;
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]);
    let mut lines = head.split("\r\n");
    let request_line = lines.next()?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next()?;
    let target = parts.next()?;
    if method != "GET" {
        return Some(("__non_get__".into(), String::new(), stream));
    }
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (target.to_string(), String::new()),
    };
    Some((percent_decode(&path), query, stream))
}

fn find_head_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4)
        .position(|w| w == b"\r\n\r\n")
        .or_else(|| buf.windows(2).position(|w| w == b"\n\n"))
}

async fn serve_reject(mut stream: TcpStream) {
    respond(&mut stream, 403, FAILURE_PAGE).await;
}

async fn respond(stream: &mut TcpStream, status: u16, body: &str) {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        _ => "Not Found",
    };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         Cache-Control: no-store\r\n\
         Referrer-Policy: no-referrer\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = timeout(
        Duration::from_secs(5),
        stream.write_all(response.as_bytes()),
    )
    .await;
    let _ = stream.flush().await;
}

/// Split a query string into decoded key/value pairs. The last value wins, which
/// matches how authorization servers mangle nothing in practice.
fn parse_query(query: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (k, v) = match pair.split_once('=') {
            Some(kv) => kv,
            None => (pair, ""),
        };
        out.insert(percent_decode(k), percent_decode(v));
    }
    out
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
                match hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    Some(byte) => {
                        out.push(byte);
                        i += 3;
                    }
                    None => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn normalize_path(path: &str) -> String {
    if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    }
}

const SUCCESS_PAGE: &str = "<!doctype html><meta charset=utf-8><title>RRouter</title>\
<body style=\"font-family:system-ui;max-width:32rem;margin:4rem auto;padding:0 1rem\">\
<h2>Signed in</h2><p>The account was connected. You can close this tab.</p>";

const FAILURE_PAGE: &str = "<!doctype html><meta charset=utf-8><title>RRouter</title>\
<body style=\"font-family:system-ui;max-width:32rem;margin:4rem auto;padding:0 1rem\">\
<h2>Sign-in failed</h2><p>Return to the RRouter dashboard and try again.</p>";

const NOT_FOUND_PAGE: &str = "<!doctype html><title>RRouter</title><p>Not found.</p>";

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpStream as Client;

    async fn get(port: u16, target: &str) -> String {
        let mut s = Client::connect(("127.0.0.1", port)).await.expect("connect");
        s.write_all(format!("GET {target} HTTP/1.1\r\nHost: x\r\n\r\n").as_bytes())
            .await
            .expect("write");
        let mut out = String::new();
        s.read_to_string(&mut out).await.expect("read");
        out
    }

    #[tokio::test]
    async fn returns_redirect_params_after_state_check() {
        let srv = LoopbackServer::bind(0, "/callback", "127.0.0.1")
            .await
            .unwrap();
        let port = srv.port;
        let uri = srv.redirect_uri();
        assert_eq!(uri, format!("http://127.0.0.1:{port}/callback"));
        let waiter = tokio::spawn(srv.wait("STATE123", Duration::from_secs(5)));
        let body = get(port, "/callback?code=abc%2Fdef&state=STATE123").await;
        assert!(body.contains("200 OK"), "{body}");
        let cb = waiter.await.unwrap().unwrap();
        assert_eq!(cb.code, "abc/def", "percent-decoded");
        assert_eq!(cb.state, "STATE123");
    }

    #[tokio::test]
    async fn state_mismatch_fails_instead_of_ignoring() {
        let srv = LoopbackServer::bind(0, "/callback", "127.0.0.1")
            .await
            .unwrap();
        let port = srv.port;
        let waiter = tokio::spawn(srv.wait("EXPECTED", Duration::from_secs(5)));
        let body = get(port, "/callback?code=x&state=ATTACKER").await;
        assert!(body.contains("400"), "{body}");
        let err = waiter.await.unwrap().unwrap_err();
        assert!(err.to_string().contains("state mismatch"), "{err}");
    }

    #[tokio::test]
    async fn provider_error_is_reported() {
        let srv = LoopbackServer::bind(0, "/callback", "127.0.0.1")
            .await
            .unwrap();
        let port = srv.port;
        let waiter = tokio::spawn(srv.wait("S", Duration::from_secs(5)));
        let _ = get(port, "/callback?error=access_denied&error_description=nope").await;
        let err = waiter.await.unwrap().unwrap_err();
        assert!(err.to_string().contains("access_denied"), "{err}");
    }

    #[tokio::test]
    async fn other_paths_are_ignored_and_wait_continues() {
        let srv = LoopbackServer::bind(0, "/callback", "127.0.0.1")
            .await
            .unwrap();
        let port = srv.port;
        let waiter = tokio::spawn(srv.wait("S", Duration::from_secs(5)));
        // Browser favicon/health probes must not end the wait.
        let body = get(port, "/favicon.ico").await;
        assert!(body.contains("404"), "{body}");
        let _ = get(port, "/callback?code=real&state=S").await;
        assert_eq!(waiter.await.unwrap().unwrap().code, "real");
    }

    #[tokio::test]
    async fn times_out_when_nothing_arrives() {
        let srv = LoopbackServer::bind(0, "/callback", "127.0.0.1")
            .await
            .unwrap();
        let err = srv.wait("S", Duration::from_millis(80)).await.unwrap_err();
        assert!(matches!(err, Error::Timeout(_)), "{err}");
    }

    #[tokio::test]
    async fn fixed_port_is_honoured_or_reported() {
        let first = LoopbackServer::bind(0, "/auth/callback", "127.0.0.1")
            .await
            .unwrap();
        let port = first.port;
        // The same port must be unavailable while the first listener lives.
        let second = LoopbackServer::bind(port, "/auth/callback", "127.0.0.1").await;
        assert!(second.is_err(), "port {port} should be taken");
        drop(first);
    }

    #[test]
    fn query_parsing_decodes_and_handles_missing_values() {
        let p = parse_query("a=1&b=two%20words&flag&c=x%2By");
        assert_eq!(p.get("a").unwrap(), "1");
        assert_eq!(p.get("b").unwrap(), "two words");
        assert_eq!(p.get("flag").unwrap(), "");
        assert_eq!(p.get("c").unwrap(), "x+y");
    }

    #[test]
    fn head_end_detection_accepts_crlf_and_lf() {
        assert_eq!(find_head_end(b"GET / HTTP/1.1\r\n\r\nbody"), Some(14));
        assert_eq!(find_head_end(b"GET / HTTP/1.1\n\nbody"), Some(14));
        assert_eq!(find_head_end(b"incomplete\r\n"), None);
    }
}
