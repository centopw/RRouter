//! The one call the proxy makes before it serializes a translated request:
//! `provider.wire_profile()` handed to `rrouter_translate::build_request_with`.
//!
//! This lives in `rrouter-auth` because it is the seam between the provider registry and
//! the translator; a registry entry that does not produce a usable profile is a routing
//! bug that only shows up as a provider-side `422`/`400` otherwise.

use rrouter_auth::registry;
use rrouter_core::Dialect;
use rrouter_translate::{build_request_with, parse_request, MaxTokensKey};
use serde_json::json;

#[test]
fn a_claude_code_turn_routed_to_mistral_is_sent_max_tokens() {
    let req = parse_request(
        Dialect::AnthropicMessages,
        &json!({
            "model": "mistral-large-latest",
            "max_tokens": 256,
            "messages": [{"role": "user", "content": "hi"}],
        }),
    )
    .unwrap();
    let spec = registry::get("mistral").expect("mistral");
    let body = build_request_with(Dialect::OpenAiChat, &req, false, &spec.wire_profile()).unwrap();

    assert_eq!(body["max_tokens"], json!(256));
    assert!(
        body.get("max_completion_tokens").is_none(),
        "mistral answers 422 to max_completion_tokens: {body}"
    );
}

#[test]
fn the_openai_surface_gets_max_completion_tokens_and_nobody_else_does() {
    let req = parse_request(
        Dialect::AnthropicMessages,
        &json!({
            "model": "m",
            "max_tokens": 64,
            "messages": [{"role": "user", "content": "hi"}],
        }),
    )
    .unwrap();

    for (id, expected) in [
        ("openai", "max_completion_tokens"),
        ("github", "max_completion_tokens"),
        ("mistral", "max_tokens"),
        ("together", "max_tokens"),
        ("deepseek", "max_tokens"),
        ("gemini", "max_tokens"),
    ] {
        let spec = registry::get(id).expect(id);
        let body =
            build_request_with(Dialect::OpenAiChat, &req, false, &spec.wire_profile()).unwrap();
        assert_eq!(body[expected], json!(64), "{id} body: {body}");
        let other = if expected == "max_tokens" {
            "max_completion_tokens"
        } else {
            "max_tokens"
        };
        assert!(body.get(other).is_none(), "{id} also sent {other}: {body}");
    }
}

#[test]
fn a_translated_request_to_claude_carries_the_registry_default_cap() {
    let req = parse_request(
        Dialect::OpenAiChat,
        &json!({
            "model": "claude-sonnet-4-5",
            "messages": [{"role": "user", "content": "hi"}],
        }),
    )
    .unwrap();
    let spec = registry::get("claude").expect("claude");
    let body = build_request_with(
        Dialect::AnthropicMessages,
        &req,
        false,
        &spec.wire_profile(),
    )
    .unwrap();

    // The registry's 32k, not the translator's historical 4096 fallback.
    assert_eq!(body["max_tokens"], json!(32_000));
}

#[test]
fn every_registered_provider_yields_a_usable_profile() {
    for spec in registry::all() {
        let profile = spec.wire_profile();
        assert!(
            matches!(
                profile.max_tokens_key,
                MaxTokensKey::MaxTokens | MaxTokensKey::MaxCompletionTokens
            ),
            "{}",
            spec.id
        );
        if spec.default_dialect == Dialect::AnthropicMessages {
            assert!(
                profile.default_max_tokens.is_some(),
                "{} speaks Anthropic, where max_tokens is mandatory",
                spec.id
            );
        }
    }
}
