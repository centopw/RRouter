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

/// What the browser callback page may show. Never holds credential material.
#[derive(Debug, Clone, Default)]
pub struct PageContext {
    /// Provider display name ("Claude (Anthropic OAuth)").
    pub provider: Option<String>,
    /// Account label the provider reported, when the caller already knows it.
    pub account: Option<String>,
    /// Dashboard root (absolute URL) when the operator configured a safe public URL.
    pub dashboard_url: Option<String>,
}

impl PageContext {
    /// Context for a provider sign-in, with the dashboard link derived from
    /// `RROUTER_PUBLIC_URL` when it is a safe absolute URL.
    pub fn for_provider(display: &str) -> Self {
        Self {
            provider: Some(display.to_string()),
            account: None,
            dashboard_url: dashboard_url_from_env(),
        }
    }
}

/// Dashboard root from `RROUTER_PUBLIC_URL`, or `None` when unset/unsafe.
///
/// Only `https://` and loopback `http://` are accepted: the callback page is served by
/// the gateway, but a link derived from an operator-supplied value must never become an
/// open redirect. Query, fragment and embedded credentials are discarded.
fn dashboard_url_from_env() -> Option<String> {
    let raw = std::env::var("RROUTER_PUBLIC_URL").ok()?;
    let url = reqwest::Url::parse(raw.trim()).ok()?;
    if !url.username().is_empty() || url.password().is_some() {
        return None;
    }
    if url.query().is_some() || url.fragment().is_some() {
        return None;
    }
    let host = url.host_str()?;
    let loopback = matches!(host, "localhost" | "127.0.0.1" | "::1");
    if url.scheme() != "https" && !(url.scheme() == "http" && loopback) {
        return None;
    }
    let mut out = url.origin().ascii_serialization();
    out.push_str(url.path().trim_end_matches('/'));
    out.push_str("/app");
    Some(out)
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
        self.wait_with(expected_state, max_wait, &PageContext::default())
            .await
    }

    /// [`wait`] rendering `context.provider`/`context.dashboard_url` on its pages.
    pub async fn wait_with(
        self,
        expected_state: &str,
        max_wait: Duration,
        context: &PageContext,
    ) -> Result<Callback> {
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
                serve_reject(stream, context).await;
                continue;
            }
            match read_request(stream).await {
                Some((path, query, mut stream)) => {
                    let params = parse_query(&query);
                    if path != self.callback_path {
                        respond(&mut stream, 404, &not_found_page(context)).await;
                        continue;
                    }
                    match interpret(&params, expected_state) {
                        Ok(cb) => {
                            respond(&mut stream, 200, &success_page(context)).await;
                            return Ok(cb);
                        }
                        Err(outcome) => {
                            respond(&mut stream, 400, &failure_page(context)).await;
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

/// Parse a callback URL pasted by an operator: a full redirect URL, or just its query.
///
/// The `state` check is the same one the loopback listener applies, so a pasted URL is
/// bound to exactly one pending login. A callback carrying `error=` is refused with the
/// provider's description, never treated as success.
pub fn parse_callback_url(input: &str, expected_state: &str) -> Result<Callback> {
    let input = input.trim();
    if input.is_empty() {
        return Err(Error::BadRequest("callback URL is empty".into()));
    }
    // A pasted browser URL is short; anything larger is not a callback.
    if input.len() > 8 * 1024 {
        return Err(Error::BadRequest("callback URL is too long".into()));
    }
    let query = match reqwest::Url::parse(input) {
        Ok(url) => url.query().unwrap_or("").to_string(),
        // Tolerate an operator pasting only `?code=…&state=…`.
        Err(_) => input.trim_start_matches('?').to_string(),
    };
    if query.is_empty() {
        return Err(Error::BadRequest(
            "callback URL carried no query parameters".into(),
        ));
    }
    interpret(&parse_query(&query), expected_state)
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

async fn serve_reject(mut stream: TcpStream, context: &PageContext) {
    respond(&mut stream, 403, &failure_page(context)).await;
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

/// Shared card shell: no external assets, light/dark safe, focus-visible affordances.
fn page(title: &str, content: &str) -> String {
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
<meta name=\"color-scheme\" content=\"light dark\">\
<title>{title} · RRouter</title><style>{CSS}</style></head>\
<body><main class=\"card\">\
<div class=\"brand\"><svg class=\"mark\" viewBox=\"0 0 32 32\" aria-hidden=\"true\" focusable=\"false\">\
<rect x=\"3\" y=\"3\" width=\"26\" height=\"26\" rx=\"8\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"2.2\"/>\
<path d=\"M11 22V10h6.5a4 4 0 0 1 0 8H13\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"2.4\" stroke-linecap=\"round\" stroke-linejoin=\"round\"/>\
<circle cx=\"21\" cy=\"21\" r=\"2\" fill=\"currentColor\"/></svg>\
<span>RRouter</span></div>{content}</main></body></html>"
    )
}

/// Inline SVG check, tinted by the stylesheet.
const ICON_SUCCESS: &str = "<div class=\"icon\" role=\"img\" aria-label=\"Success\">\
<svg viewBox=\"0 0 48 48\" aria-hidden=\"true\" focusable=\"false\">\
<circle cx=\"24\" cy=\"24\" r=\"22\" class=\"okfill\"/>\
<path d=\"M15 24.5l6 6L34 18\" fill=\"none\" stroke=\"#fff\" stroke-width=\"4\" \
stroke-linecap=\"round\" stroke-linejoin=\"round\"/></svg></div>";

/// Inline SVG alert for failures.
const ICON_FAILURE: &str = "<div class=\"icon\" role=\"img\" aria-label=\"Failure\">\
<svg viewBox=\"0 0 48 48\" aria-hidden=\"true\" focusable=\"false\">\
<circle cx=\"24\" cy=\"24\" r=\"22\" class=\"failfill\"/>\
<path d=\"M24 14v13\" stroke=\"#fff\" stroke-width=\"4\" stroke-linecap=\"round\"/>\
<circle cx=\"24\" cy=\"34\" r=\"2.4\" fill=\"#fff\"/></svg></div>";

const CSS: &str = r#":root{color-scheme:light dark;--bg:#f3f5f8;--card:#ffffff;--fg:#14171d;--muted:#5b6472;--line:#e2e6ec;--btn-bg:#161a21;--btn-fg:#ffffff;--focus:#2563eb;--ok:#12864b;--fail:#c02636}
@media (prefers-color-scheme: dark){:root{--bg:#0c0f14;--card:#161a21;--fg:#eef1f6;--muted:#a6afbc;--line:#2a303b;--btn-bg:#eef1f6;--btn-fg:#101319;--focus:#7ba7ff;--ok:#35c07a;--fail:#ff6b7a}}
*{box-sizing:border-box}
body{margin:0;min-height:100vh;min-height:100svh;display:flex;align-items:center;justify-content:center;padding:1.5rem;background:var(--bg);color:var(--fg);font-family:ui-sans-serif,-apple-system,BlinkMacSystemFont,"Segoe UI",Roboto,Helvetica,Arial,sans-serif;font-size:16px;line-height:1.5}
.card{width:100%;max-width:26rem;padding:2rem 1.75rem;background:var(--card);border:1px solid var(--line);border-radius:18px;box-shadow:0 18px 48px rgba(12,22,40,.10);text-align:center}
.brand{display:flex;align-items:center;justify-content:center;gap:.55rem;font-weight:650;letter-spacing:-.02em;font-size:1rem}
.mark{width:28px;height:28px;color:var(--fg);flex:none}
h1{margin:1.1rem 0 .4rem;font-size:1.22rem;line-height:1.3;font-weight:650}
p{margin:.32rem 0;color:var(--muted);font-size:.95rem}
.provider{margin-top:.7rem;color:var(--fg);font-weight:600;font-size:.95rem}
.account{margin-top:.2rem;font-family:ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;font-size:.82rem;color:var(--muted);word-break:break-all}
.icon{display:flex;justify-content:center;margin-top:1.25rem}
.icon svg{width:56px;height:56px}
.okfill{fill:var(--ok)}
.failfill{fill:var(--fail)}
a.return{display:inline-block;margin-top:1.35rem;padding:.62rem 1.05rem;border-radius:10px;background:var(--btn-bg);color:var(--btn-fg);text-decoration:none;font-weight:600;font-size:.92rem}
a.return:hover{opacity:.9}
a.return:focus-visible{outline:3px solid var(--focus);outline-offset:3px;border-radius:10px}
.close{margin-top:1rem;font-size:.8rem;color:var(--muted)}
@media (prefers-reduced-motion:no-preference){.card{transition:background-color .2s,color .2s}}"#;

fn html_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            other => out.push(other),
        }
    }
    out
}

fn provider_line(context: &PageContext) -> String {
    match context.provider.as_deref().filter(|p| !p.is_empty()) {
        Some(provider) => format!("<p class=\"provider\">{}</p>", html_escape(provider)),
        None => String::new(),
    }
}

fn account_line(context: &PageContext) -> String {
    match context.account.as_deref().filter(|a| !a.is_empty()) {
        Some(account) => format!("<p class=\"account\">{}</p>", html_escape(account)),
        None => String::new(),
    }
}

fn dashboard_link(context: &PageContext) -> String {
    match context.dashboard_url.as_deref() {
        Some(url) => format!(
            "<a class=\"return\" href=\"{}\" rel=\"noreferrer\">Return to dashboard</a>",
            html_escape(url)
        ),
        None => String::new(),
    }
}

fn success_page(context: &PageContext) -> String {
    page(
        "Signed in",
        &format!(
            "{ICON_SUCCESS}<h1>Signed in</h1>{}<p>The account was connected.</p>\
{}<p class=\"close\">You can close this tab.</p>{}",
            provider_line(context),
            account_line(context),
            dashboard_link(context)
        ),
    )
}

fn failure_page(context: &PageContext) -> String {
    page(
        "Sign-in failed",
        &format!(
            "{ICON_FAILURE}<h1>Sign-in failed</h1>{}<p>The provider did not complete the \
sign-in.</p>{}<p class=\"close\">Return to the dashboard and start again.</p>",
            provider_line(context),
            dashboard_link(context)
        ),
    )
}

fn not_found_page(context: &PageContext) -> String {
    page(
        "Page not found",
        &format!(
            "<h1>Page not found</h1><p>This callback URL does not belong to a sign-in in \
progress.</p>{}",
            dashboard_link(context)
        ),
    )
}

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
    fn pasted_callback_urls_are_state_bound() {
        let cb = parse_callback_url(
            "http://127.0.0.1:1455/auth/callback?code=abc%2Fdef&state=S1",
            "S1",
        )
        .expect("callback");
        assert_eq!(cb.code, "abc/def");
        assert_eq!(cb.state, "S1");

        // Query-only paste is tolerated.
        assert_eq!(
            parse_callback_url("?code=x&state=S1", "S1")
                .expect("query")
                .code,
            "x"
        );
        let err = parse_callback_url("http://localhost/cb?code=x&state=OTHER", "S1")
            .expect_err("wrong state");
        assert!(err.to_string().contains("state mismatch"), "{err}");
        let err = parse_callback_url(
            "http://localhost/cb?error=access_denied&error_description=nope&state=S1",
            "S1",
        )
        .expect_err("denied");
        assert!(err.to_string().contains("access_denied"), "{err}");
        assert!(matches!(
            parse_callback_url("", "S1"),
            Err(Error::BadRequest(_))
        ));
        assert!(matches!(
            parse_callback_url("http://localhost/cb", "S1"),
            Err(Error::BadRequest(_))
        ));
    }

    #[test]
    fn pages_carry_brand_context_and_no_secret() {
        let context = PageContext {
            provider: Some("Mock Provider".into()),
            account: Some("user@example.test".into()),
            dashboard_url: Some("https://rrouter.example.test/app".into()),
        };
        let success = success_page(&context);
        for needle in [
            "RRouter",
            "Signed in",
            "Mock Provider",
            "user@example.test",
            "https://rrouter.example.test/app",
            "prefers-color-scheme: dark",
            "focus-visible",
            "You can close this tab.",
        ] {
            assert!(success.contains(needle), "success page missing {needle}");
        }
        assert!(
            !success.contains("SECRET"),
            "pages must never carry a token"
        );

        let failure = failure_page(&context);
        assert!(failure.contains("RRouter"));
        assert!(failure.contains("Sign-in failed"));
        assert!(failure.contains("https://rrouter.example.test/app"));
        assert!(!failure.contains("SECRET"));

        let missing = not_found_page(&context);
        assert!(missing.contains("RRouter"));
        assert!(missing.contains("Page not found"));

        // Unconfigured context: no link, still branded and escaped.
        let plain = success_page(&PageContext {
            provider: Some("<script>".into()),
            ..PageContext::default()
        });
        assert!(!plain.contains("href="), "{plain}");
        assert!(plain.contains("&lt;script&gt;"), "{plain}");
    }

    #[test]
    fn dashboard_url_requires_a_safe_public_origin() {
        let _guard = crate::TEST_ENV
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        std::env::remove_var("RROUTER_PUBLIC_URL");
        assert_eq!(dashboard_url_from_env(), None);

        std::env::set_var("RROUTER_PUBLIC_URL", "https://rrouter.example.test");
        assert_eq!(
            dashboard_url_from_env().as_deref(),
            Some("https://rrouter.example.test/app")
        );

        std::env::set_var("RROUTER_PUBLIC_URL", "http://rrouter.example.test");
        assert_eq!(
            dashboard_url_from_env(),
            None,
            "public plain HTTP is unsafe"
        );

        std::env::set_var(
            "RROUTER_PUBLIC_URL",
            "https://user:pass@rrouter.example.test/?x=1",
        );
        assert_eq!(
            dashboard_url_from_env(),
            None,
            "credentials and query are unsafe"
        );

        std::env::set_var("RROUTER_PUBLIC_URL", "http://localhost:20128");
        assert_eq!(
            dashboard_url_from_env().as_deref(),
            Some("http://localhost:20128/app")
        );
        std::env::remove_var("RROUTER_PUBLIC_URL");
    }

    #[tokio::test]
    async fn wait_with_renders_provider_context() {
        let srv = LoopbackServer::bind(0, "/callback", "127.0.0.1")
            .await
            .unwrap();
        let port = srv.port;
        let context = PageContext {
            provider: Some("Mock Provider".into()),
            account: None,
            dashboard_url: None,
        };
        let waiter =
            tokio::spawn(async move { srv.wait_with("S", Duration::from_secs(5), &context).await });
        let body = get(port, "/callback?code=c&state=S").await;
        assert!(body.contains("200 OK"), "{body}");
        assert!(body.contains("RRouter"), "{body}");
        assert!(body.contains("Mock Provider"), "{body}");
        assert!(waiter.await.unwrap().is_ok());
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
