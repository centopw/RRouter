//! Provider error bodies rendered back into the client's dialect.
//!
//! An upstream failure is the one response a client's *code* branches on: SDKs and
//! retry policies read `error.type`, `error.code` and `error.param`, not the prose.
//! Rebuilding the body from a formatted `Display` string throws those away and turns
//! every provider failure into the same opaque shape. These helpers read the provider's
//! own fields and re-frame them in the inbound envelope, leaving the HTTP status to the
//! caller (a `429` stays a `429`, so backoff keeps working).

use rrouter_core::Dialect;
use serde_json::{json, Map, Value};

/// What a provider's error body said, in either family's shape.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProviderError {
    pub message: String,
    /// Provider's own `type` (`invalid_request_error`, `overloaded_error`, …).
    pub kind: Option<String>,
    /// Provider's own `code` (`context_length_exceeded`, `insufficient_quota`, …).
    pub code: Option<String>,
    /// Offending parameter, when the provider names one.
    pub param: Option<String>,
}

/// Read an upstream error body.
///
/// OpenAI and Anthropic both put the fields under `error`; some OpenAI-compatible
/// servers send `{"error": "message"}`, and a few send the object at the top level.
/// A body with fields but no message still yields the fields (a `code` alone is worth
/// keeping); `None` means the body carried no error at all.
pub fn parse_provider_error(payload: &Value) -> Option<ProviderError> {
    let nested = payload.get("error");
    if let Some(Value::String(message)) = nested {
        return Some(ProviderError {
            message: message.clone(),
            ..ProviderError::default()
        });
    }
    let obj = match nested {
        Some(value @ Value::Object(_)) => value,
        _ => payload,
    };
    if !obj.is_object() {
        return None;
    }
    let text = |key: &str| obj.get(key).and_then(scalar_as_string);
    Some(ProviderError {
        // OpenAI and Cohere say `message`; Jina says `detail`.
        message: text("message")
            .or_else(|| text("detail"))
            .unwrap_or_default(),
        kind: text("type"),
        code: text("code"),
        param: text("param"),
    })
}

/// A string field, or a number/bool rendered as text: providers use both for `code`.
fn scalar_as_string(value: &Value) -> Option<String> {
    match value {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// `(type, code)` a status maps to when the provider said nothing more specific.
///
/// These are the standard pairs clients of the inbound dialect already parse, so a
/// translated failure is indistinguishable from a native one.
fn status_kind(status: u16) -> (&'static str, &'static str) {
    match status {
        400 => ("invalid_request_error", ""),
        401 => ("authentication_error", "invalid_api_key"),
        403 => ("permission_error", ""),
        404 => ("not_found_error", "model_not_found"),
        429 => ("rate_limit_error", "rate_limit_exceeded"),
        529 => ("overloaded_error", "proportional_overload"),
        _ => ("api_error", ""),
    }
}

/// Anthropic's `error.type` enum. A provider type outside it would make the client's
/// exception mapping fall through, so anything unrecognised becomes the status default.
fn anthropic_kind(status: u16, kind: Option<&str>) -> &'static str {
    match kind {
        Some("invalid_request_error") => "invalid_request_error",
        Some("authentication_error") => "authentication_error",
        Some("permission_error") => "permission_error",
        Some("not_found_error") => "not_found_error",
        Some("rate_limit_error") => "rate_limit_error",
        Some("overloaded_error") => "overloaded_error",
        Some("api_error") => "api_error",
        // OpenAI's own vocabulary that has an Anthropic counterpart.
        Some("insufficient_quota") => "rate_limit_error",
        Some("server_error") => "api_error",
        _ => status_kind(status).0,
    }
}

/// Build the client-facing body for a failed upstream call.
///
/// `status` is returned to the client unchanged by the caller. `payload` is the parsed
/// upstream body when the provider answered JSON; `fallback` is what the caller wants
/// said when it did not (a transport failure, a truncated body, a plain-text error).
pub fn upstream_error_body(
    inbound: Dialect,
    status: u16,
    payload: Option<&Value>,
    fallback: &str,
) -> Value {
    let provider = payload.and_then(parse_provider_error);
    let message = provider
        .as_ref()
        .map(|e| e.message.clone())
        .filter(|message| !message.is_empty())
        .unwrap_or_else(|| fallback.to_string());
    let (default_kind, default_code) = status_kind(status);
    let code = provider
        .as_ref()
        .and_then(|e| e.code.clone())
        .unwrap_or_else(|| default_code.to_string());

    match inbound {
        Dialect::AnthropicMessages => {
            let kind = anthropic_kind(status, provider.as_ref().and_then(|e| e.kind.as_deref()));
            json!({ "type": "error", "error": { "type": kind, "message": message, "code": code } })
        }
        _ => {
            let kind = provider
                .as_ref()
                .and_then(|e| e.kind.clone())
                .unwrap_or_else(|| default_kind.to_string());
            let mut error = Map::new();
            error.insert("message".into(), Value::from(message));
            error.insert("type".into(), Value::from(kind));
            error.insert("code".into(), Value::from(code));
            if let Some(param) = provider.as_ref().and_then(|e| e.param.clone()) {
                error.insert("param".into(), Value::from(param));
            }
            json!({ "error": Value::Object(error) })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openai_payload_keeps_type_code_and_param() {
        let payload = json!({"error": {
            "message": "This model's maximum context length is 128000 tokens",
            "type": "invalid_request_error",
            "param": "messages",
            "code": "context_length_exceeded",
        }});
        let body = upstream_error_body(Dialect::OpenAiChat, 400, Some(&payload), "ignored");
        assert_eq!(body["error"]["message"], json!(payload["error"]["message"]));
        assert_eq!(body["error"]["type"], json!("invalid_request_error"));
        assert_eq!(body["error"]["code"], json!("context_length_exceeded"));
        assert_eq!(body["error"]["param"], json!("messages"));
    }

    #[test]
    fn anthropic_payload_is_reframed_into_the_openai_envelope() {
        let payload = json!({
            "type": "error",
            "error": {"type": "overloaded_error", "message": "Overloaded"},
        });
        let body = upstream_error_body(Dialect::OpenAiChat, 529, Some(&payload), "ignored");
        assert_eq!(body["error"]["type"], json!("overloaded_error"));
        assert_eq!(body["error"]["message"], json!("Overloaded"));
        assert_eq!(body["error"]["code"], json!("proportional_overload"));
        assert!(
            body.get("type").is_none(),
            "openai envelope has no top level type"
        );
    }

    #[test]
    fn openai_payload_is_reframed_into_the_anthropic_envelope() {
        let payload = json!({"error": {
            "message": "Unsupported parameter: max_tokens",
            "type": "invalid_request_error",
            "param": "max_tokens",
            "code": "unsupported_parameter",
        }});
        let body = upstream_error_body(Dialect::AnthropicMessages, 400, Some(&payload), "ignored");
        assert_eq!(body["type"], json!("error"));
        assert_eq!(body["error"]["type"], json!("invalid_request_error"));
        assert_eq!(
            body["error"]["message"],
            json!("Unsupported parameter: max_tokens")
        );
        assert_eq!(body["error"]["code"], json!("unsupported_parameter"));
    }

    #[test]
    fn a_provider_type_outside_the_anthropic_enum_falls_back_to_the_status() {
        let payload = json!({"error": {"message": "nope", "type": "some_new_kind"}});
        let body = upstream_error_body(Dialect::AnthropicMessages, 429, Some(&payload), "ignored");
        assert_eq!(body["error"]["type"], json!("rate_limit_error"));
        assert_eq!(body["error"]["code"], json!("rate_limit_exceeded"));
    }

    #[test]
    fn a_body_without_a_message_keeps_the_callers_text() {
        let payload = json!({"error": {"code": "insufficient_quota"}});
        let body = upstream_error_body(
            Dialect::OpenAiChat,
            429,
            Some(&payload),
            "upstream openai returned 429",
        );
        assert_eq!(
            body["error"]["message"],
            json!("upstream openai returned 429")
        );
        assert_eq!(body["error"]["code"], json!("insufficient_quota"));
        assert_eq!(body["error"]["type"], json!("rate_limit_error"));
    }

    #[test]
    fn a_string_error_becomes_the_message() {
        let payload = json!({"error": "bad gateway"});
        let body = upstream_error_body(Dialect::OpenAiChat, 502, Some(&payload), "ignored");
        assert_eq!(body["error"]["message"], json!("bad gateway"));
        assert_eq!(body["error"]["type"], json!("api_error"));
    }

    #[test]
    fn no_payload_uses_the_status_defaults() {
        let body = upstream_error_body(Dialect::OpenAiChat, 529, None, "upstream overloaded");
        assert_eq!(body["error"]["message"], json!("upstream overloaded"));
        assert_eq!(body["error"]["type"], json!("overloaded_error"));
        assert_eq!(body["error"]["code"], json!("proportional_overload"));
    }

    #[test]
    fn cohere_and_jina_error_shapes_keep_their_message_and_code() {
        // Cohere answers `{message, id}` at the top level, with no `error` wrapper.
        let cohere = json!({"message": "invalid api token", "id": "abc"});
        let body = upstream_error_body(Dialect::OpenAiChat, 401, Some(&cohere), "ignored");
        assert_eq!(body["error"]["message"], json!("invalid api token"));
        assert_eq!(body["error"]["type"], json!("authentication_error"));

        // Jina answers `{detail, code, request_id}`.
        let jina = json!({
            "detail": "Invalid API key. Verify your API key.",
            "code": "AUTH_INVALID_API_KEY",
            "request_id": "03f58cd9da22fabf",
        });
        let body = upstream_error_body(Dialect::OpenAiChat, 401, Some(&jina), "ignored");
        assert_eq!(
            body["error"]["message"],
            json!("Invalid API key. Verify your API key.")
        );
        assert_eq!(body["error"]["code"], json!("AUTH_INVALID_API_KEY"));
    }

    #[test]
    fn a_numeric_code_is_preserved_as_text() {
        let payload = json!({"error": {"message": "boom", "code": 42}});
        let body = upstream_error_body(Dialect::OpenAiChat, 500, Some(&payload), "ignored");
        assert_eq!(body["error"]["code"], json!("42"));
    }
}
