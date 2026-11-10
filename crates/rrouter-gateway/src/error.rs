//! Error bodies in the shape each inbound dialect's clients expect.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use rrouter_core::{Dialect, Error};
use serde_json::{json, Value};

/// `(type, code)` pair emitted for a given HTTP status.
pub fn error_kind(status: u16) -> (&'static str, &'static str) {
    match status {
        400 => ("invalid_request_error", ""),
        401 => ("authentication_error", "invalid_api_key"),
        403 => ("permission_error", ""),
        404 => ("not_found_error", "model_not_found"),
        429 => ("rate_limit_error", "rate_limit_exceeded"),
        529 => ("overloaded_error", "proportional_overload"),
        500..=599 => ("api_error", ""),
        _ => ("api_error", ""),
    }
}

/// The `error` object common to both shapes.
pub fn error_object(status: u16, message: &str) -> Value {
    error_object_with(status, message, None)
}

/// [`error_object`] with an explicit `code`, used by governance errors whose machine
/// reason (`api_key_inactive`, `model_not_allowed`, …) is not the status default.
pub fn error_object_with(status: u16, message: &str, code: Option<&str>) -> Value {
    let (kind, default_code) = error_kind(status);
    json!({ "message": message, "type": kind, "code": code.unwrap_or(default_code) })
}

/// Body for a non-streaming failure. Anthropic clients expect the top-level
/// `{"type":"error","error":{...}}` envelope; everyone else gets `{"error":{...}}`.
///
/// An upstream failure keeps the provider's own `type`, `code` and `param`: client SDKs
/// and retry policies branch on those fields, and rebuilding the body from a formatted
/// message turned every provider failure into the same opaque shape. Governance errors
/// (our own 403/429 refusals) keep their stable codes, because there is no upstream body
/// to preserve.
pub fn error_body(inbound: Dialect, err: &Error) -> Value {
    let status = err.status();
    if let Error::Upstream { body, payload, .. } = err {
        return rrouter_translate::upstream_error_body(inbound, status, payload.as_ref(), body);
    }
    let message = err.to_string();
    let obj = error_object_with(status, &message, err.code());
    match inbound {
        Dialect::AnthropicMessages => json!({ "type": "error", "error": obj }),
        _ => json!({ "error": obj }),
    }
}

/// Complete HTTP response for a failed request.
pub fn error_response(inbound: Dialect, err: &Error) -> Response {
    let status = StatusCode::from_u16(err.status()).unwrap_or(StatusCode::BAD_GATEWAY);
    let mut resp = (status, axum::Json(error_body(inbound, err))).into_response();
    if let Some(secs) = err.retry_after_secs() {
        if let Ok(value) = axum::http::HeaderValue::from_str(&secs.to_string()) {
            resp.headers_mut()
                .insert(axum::http::header::RETRY_AFTER, value);
        }
    }
    resp
}

/// One SSE frame carrying a mid-stream error, in the inbound wire shape.
pub fn error_frame(inbound: Dialect, err: &Error) -> String {
    let body = error_body(inbound, err);
    let data = body.to_string();
    match inbound {
        Dialect::AnthropicMessages => rrouter_core::sse::render_anthropic("error", &data),
        _ => rrouter_core::sse::render_openai(&data, None),
    }
}

/// A `501` for endpoints RRouter intentionally does not implement.
pub fn not_implemented(inbound: Dialect, what: &str) -> Response {
    let message = format!("{what} is not supported by RRouter");
    let obj = error_object(StatusCode::NOT_IMPLEMENTED.as_u16(), &message);
    let body = match inbound {
        Dialect::AnthropicMessages => json!({ "type": "error", "error": obj }),
        _ => json!({ "error": obj }),
    };
    (StatusCode::NOT_IMPLEMENTED, axum::Json(body)).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The status, the body's code and the `Retry-After` header describe one fact; a
    /// client must not have to reconcile three different answers.
    #[test]
    fn rate_limited_body_and_retry_after_agree() {
        let err = Error::Limited {
            code: "rate_limited",
            message: "api key is limited to 1 request per minute".into(),
            retry_after_secs: 1,
        };
        let resp = error_response(Dialect::OpenAiChat, &err);
        assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            resp.headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok()),
            Some("1")
        );

        let body = error_body(Dialect::OpenAiChat, &err);
        assert_eq!(body["error"]["code"], "rate_limited", "{body}");
        assert_eq!(body["error"]["type"], "rate_limit_error", "{body}");
        assert!(
            body["error"]["message"]
                .as_str()
                .is_some_and(|m| m.contains("limited to 1 request")),
            "{body}"
        );

        // The Anthropic envelope says the same thing in its own shape.
        let body = error_body(Dialect::AnthropicMessages, &err);
        assert_eq!(body["type"], "error", "{body}");
        assert_eq!(body["error"]["code"], "rate_limited", "{body}");
        assert_eq!(body["error"]["type"], "rate_limit_error", "{body}");
    }

    #[test]
    fn anthropic_gets_envelope_openai_gets_plain_error() {
        let err = Error::BadRequest("nope".into());
        let a = error_body(Dialect::AnthropicMessages, &err);
        assert_eq!(a["type"], "error");
        assert_eq!(a["error"]["type"], "invalid_request_error");
        assert_eq!(a["error"]["message"], "bad request: nope");
        let o = error_body(Dialect::OpenAiChat, &err);
        assert!(o.get("type").is_none());
        assert_eq!(o["error"]["type"], "invalid_request_error");
    }

    #[test]
    fn status_maps_kind_and_code() {
        assert_eq!(error_kind(401), ("authentication_error", "invalid_api_key"));
        assert_eq!(error_kind(404), ("not_found_error", "model_not_found"));
        assert_eq!(error_kind(429), ("rate_limit_error", "rate_limit_exceeded"));
        assert_eq!(error_kind(503), ("api_error", ""));
        assert_eq!(
            error_kind(529),
            ("overloaded_error", "proportional_overload")
        );
    }

    #[test]
    fn mid_stream_frames_are_wire_shaped() {
        let err = Error::Upstream {
            provider: "p".into(),
            status: 503,
            body: "down".into(),
            payload: None,
        };
        let anthropic = error_frame(Dialect::AnthropicMessages, &err);
        assert!(anthropic.starts_with("event: error\ndata: {\"error\":"));
        assert!(anthropic.ends_with("\n\n"));
        let openai = error_frame(Dialect::OpenAiChat, &err);
        assert!(openai.starts_with("data: {\"error\":"));
    }
}
