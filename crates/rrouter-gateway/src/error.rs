//! Error bodies in the shape 9router's clients already parse, per inbound dialect.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use rrouter_core::{Dialect, Error};
use serde_json::{json, Value};

/// `(type, code)` pair 9router emits for a given HTTP status.
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
pub fn error_body(inbound: Dialect, err: &Error) -> Value {
    let status = err.status();
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
    fn status_maps_9router_kind_and_code() {
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
