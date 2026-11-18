//! The Claude Code fingerprint Anthropic's subscription gate checks.
//!
//! An OAuth ("Claude subscription") credential is authorized to drive Sonnet and Opus
//! only when the request looks like it came from Claude Code. The check is the first
//! system block: without it the provider answers `429` with
//! `{"type":"error","error":{"type":"rate_limit_error","message":"Error"}}` — an
//! entitlement refusal wearing a rate limit's status. Haiku is not gated, which is why
//! the same account answers cheap models and fails the ones that matter, and why the
//! failure looks like a throttle that is not real.
//!
//! The line below is Anthropic's own, verbatim. It is prepended, never merged: the gate
//! reads the first block's text.

use rrouter_auth::{AuthStyle, ProviderKind, ProviderSpec};
use serde_json::{json, Value};

pub const IDENTITY: &str = "You are Claude Code, Anthropic's official CLI for Claude.";

/// True for the Anthropic subscription connection (OAuth bearer, provider `claude`).
/// An API-key connection to the same host is a different credential and takes no
/// fingerprint.
pub fn is_subscription(spec: &ProviderSpec) -> bool {
    spec.id == "claude"
        && spec.kind == ProviderKind::OAuth
        && spec.auth == AuthStyle::Bearer
}

/// Ensure `body["system"]` opens with the identity block, in place.
///
/// A body that already carries the line (a Claude Code client speaking to us directly)
/// is left byte-for-byte alone. `system` may arrive absent, as a string, or as blocks;
/// Anthropic accepts both shapes, so a string is widened to blocks rather than
/// rejected.
pub fn inject_identity(spec: &ProviderSpec, body: &mut Value) {
    if !is_subscription(spec) {
        return;
    }
    let carries = |text: &str| text.starts_with(IDENTITY);
    match body.get_mut("system") {
        Some(Value::String(text)) if carries(text) => {}
        Some(Value::String(text)) => {
            let existing = std::mem::take(text);
            body["system"] = json!([
                { "type": "text", "text": IDENTITY },
                { "type": "text", "text": existing }
            ]);
        }
        Some(Value::Array(blocks)) => {
            let present = blocks.iter().any(|block| {
                block
                    .get("text")
                    .and_then(Value::as_str)
                    .is_some_and(carries)
            });
            if !present {
                blocks.insert(0, json!({ "type": "text", "text": IDENTITY }));
            }
        }
        _ => {
            body["system"] = json!([{ "type": "text", "text": IDENTITY }]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rrouter_auth::registry;

    fn claude() -> &'static ProviderSpec {
        registry::get("claude").expect("claude spec")
    }

    #[test]
    fn a_string_system_is_widened_and_gains_the_identity_first() {
        let mut body = json!({ "system": "you are terse", "messages": [] });
        inject_identity(claude(), &mut body);
        let blocks = body["system"].as_array().expect("blocks");
        assert_eq!(blocks[0]["text"], IDENTITY);
        assert_eq!(blocks[1]["text"], "you are terse");
    }

    #[test]
    fn an_absent_system_becomes_the_identity_alone() {
        let mut body = json!({ "messages": [] });
        inject_identity(claude(), &mut body);
        assert_eq!(body["system"], json!([{ "type": "text", "text": IDENTITY }]));
    }

    #[test]
    fn a_body_that_already_carries_the_line_is_untouched() {
        let original = json!({
            "system": [{ "type": "text", "text": IDENTITY }, { "type": "text", "text": "rest" }]
        });
        let mut body = original.clone();
        inject_identity(claude(), &mut body);
        assert_eq!(body, original);
    }

    #[test]
    fn an_api_key_connection_takes_no_fingerprint() {
        let spec = registry::get("anthropic").expect("anthropic spec");
        let mut body = json!({ "messages": [] });
        inject_identity(spec, &mut body);
        assert!(body.get("system").is_none());
    }
}
