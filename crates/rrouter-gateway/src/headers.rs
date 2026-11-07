//! Response hardening, applied once to every response at the router edge.
//!
//! A deployment behind a TLS-terminating proxy should not have to add the basic
//! response headers itself. This middleware sets them after the handler ran, so
//! it sees the final response (including CORS and the admin gate's rejections) and
//! never has to touch the body: nothing is buffered, streamed responses keep
//! streaming.
//!
//! Every header is only *inserted*: a value a handler or an upstream hop already
//! set wins, so a proxy in front of this process can tighten a policy without the
//! gateway overriding it.
//!
//! HSTS is deliberately absent: TLS terminates at the proxy, and that hop owns
//! `strict-transport-security`.

use axum::extract::Request;
use axum::http::header::{Entry, CONTENT_TYPE};
use axum::http::{HeaderMap, HeaderValue};
use axum::middleware::Next;
use axum::response::Response;

/// Headers every response carries. They describe the response rather than the
/// body, so they are safe on JSON, SSE, binary downloads and redirects alike.
const BASE: [(&str, &str); 4] = [
    ("x-content-type-options", "nosniff"),
    ("x-frame-options", "DENY"),
    ("referrer-policy", "no-referrer"),
    ("permissions-policy", "camera=(), microphone=(), geolocation=()"),
];

const CSP_HEADER: &str = "content-security-policy";

/// Dashboard policy: same-origin scripts and fetches, inline styles for the shell's style
/// attributes, https images and extension-panel iframes (operator-configured), no framing by
/// others, no cross-origin form posts.
pub const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; \
img-src 'self' data: https:; font-src 'self' data:; connect-src 'self'; \
frame-src 'self' https:; frame-ancestors 'none'; base-uri 'self'; form-action 'self'";

/// Add the hardening headers to the response the rest of the stack produced.
///
/// Insert-only: `entry(..).or_insert` semantics, because an existing value is a
/// decision made closer to the data.
pub async fn harden(req: Request, next: Next) -> Response {
    let mut res = next.run(req).await;
    let html = is_html(res.headers());
    let headers = res.headers_mut();
    for (name, value) in BASE {
        if let Entry::Vacant(slot) = headers.entry(name) {
            slot.insert(HeaderValue::from_static(value));
        }
    }
    // A CSP only means something for a document, and a JSON error body with one
    // would just be noise in the response dump.
    if html {
        if let Entry::Vacant(slot) = headers.entry(CSP_HEADER) {
            slot.insert(HeaderValue::from_static(CSP));
        }
    }
    res
}

/// `true` for `text/html` and `Text/HTML; charset=utf-8` alike; `text/htmlx` is not
/// HTML, so the comparison is on the full media type.
fn is_html(headers: &HeaderMap) -> bool {
    let Some(value) = headers.get(CONTENT_TYPE).and_then(|v| v.to_str().ok()) else {
        return false;
    };
    let bytes = value.as_bytes();
    let media_type = bytes.split(|b| *b == b';').next().unwrap_or(bytes);
    media_type.trim_ascii().eq_ignore_ascii_case(b"text/html")
}
