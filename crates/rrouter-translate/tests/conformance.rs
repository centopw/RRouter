//! Cross-dialect conformance: the conversions the proxy actually performs.
//!
//! Every case here fails on a real client, not on a style rule: a Claude Code turn
//! translated to a Chat provider (tool results in the wrong position, `422` on the
//! output-cap key), an OpenAI client routed to Claude (silent 4k truncation), and a
//! tool-call stream that must survive a round trip through the Anthropic encoder.

use rrouter_core::sse::render_openai;
use rrouter_core::{Dialect, FinishReason, Part, Role, StreamEvent, Usage};
use rrouter_translate::{
    build_request_with, build_response, dropped_fields, error::upstream_error_body, parse_request,
    parse_response, MaxTokensKey, StreamDecoder, StreamEncoder, WireProfile, DEFAULT_MAX_TOKENS,
};
use serde_json::{json, Value};

/// A provider that documents `max_tokens` (Mistral, Together, DeepSeek, Gemini, Ollama …).
fn max_tokens_profile() -> WireProfile {
    WireProfile::new(MaxTokensKey::MaxTokens, None)
}

/// OpenAI's own surface: current gpt-5/o-series models reject `max_tokens`.
fn openai_profile() -> WireProfile {
    WireProfile::new(MaxTokensKey::MaxCompletionTokens, None)
}

/// Claude's registry entry: 32k default output, as `ProviderSpec` declares.
fn claude_profile() -> WireProfile {
    WireProfile::new(MaxTokensKey::MaxTokens, Some(32_000))
}

fn chat_request() -> Value {
    json!({
        "model": "claude-sonnet-4-5",
        "temperature": 0.2,
        "top_p": 0.9,
        "max_tokens": 512,
        "stop": ["END"],
        "tool_choice": {"type": "function", "function": {"name": "get_weather"}},
        "messages": [
            {"role": "system", "content": "be terse"},
            {"role": "user", "content": "weather in Hanoi?"},
            {"role": "assistant", "content": null, "reasoning_content": "need the tool",
             "tool_calls": [{"id": "call_1", "type": "function",
                "function": {"name": "get_weather", "arguments": "{\"city\":\"Hanoi\"}"}}]},
            {"role": "tool", "tool_call_id": "call_1", "content": "31C"}
        ],
        "tools": [{"type": "function", "function": {
            "name": "get_weather",
            "description": "weather lookup",
            "parameters": {"type": "object", "properties": {"city": {"type": "string"}}}
        }}]
    })
}

// ---------------------------------------------------------------------------
// Output cap
// ---------------------------------------------------------------------------

#[test]
fn a_max_tokens_provider_never_receives_max_completion_tokens() {
    let req = parse_request(
        Dialect::AnthropicMessages,
        &json!({
            "model": "claude-sonnet-4-5",
            "max_tokens": 128,
            "messages": [{"role": "user", "content": "hi"}],
        }),
    )
    .unwrap();

    // Mistral-class: `max_completion_tokens` is an unknown field and the API answers 422.
    let wire = build_request_with(Dialect::OpenAiChat, &req, false, &max_tokens_profile()).unwrap();
    assert_eq!(wire["max_tokens"], json!(128));
    assert!(
        wire.get("max_completion_tokens").is_none(),
        "a max_tokens provider must never see max_completion_tokens: {wire}"
    );

    // OpenAI's surface is the mirror image: `max_tokens` is what it rejects.
    let wire = build_request_with(Dialect::OpenAiChat, &req, false, &openai_profile()).unwrap();
    assert_eq!(wire["max_completion_tokens"], json!(128));
    assert!(wire.get("max_tokens").is_none(), "body: {wire}");
}

#[test]
fn the_provider_default_applies_only_when_the_client_names_no_cap() {
    let body = json!({
        "model": "gpt-4.1",
        "messages": [{"role": "user", "content": "hi"}],
    });
    let req = parse_request(Dialect::OpenAiChat, &body).unwrap();

    // Anthropic requires the field. It used to become a hardcoded 4096, truncating every
    // translated request that named no cap; Claude's configured default now applies.
    let wire =
        build_request_with(Dialect::AnthropicMessages, &req, false, &claude_profile()).unwrap();
    assert_eq!(wire["max_tokens"], json!(32_000));

    // An explicit client value is never overridden.
    let explicit = parse_request(
        Dialect::OpenAiChat,
        &json!({"model": "gpt-4.1", "max_tokens": 7,
                "messages": [{"role": "user", "content": "hi"}]}),
    )
    .unwrap();
    let wire = build_request_with(
        Dialect::AnthropicMessages,
        &explicit,
        false,
        &claude_profile(),
    )
    .unwrap();
    assert_eq!(wire["max_tokens"], json!(7));

    // A provider that declares no default still gets the mandatory field.
    let wire = build_request_with(
        Dialect::AnthropicMessages,
        &req,
        false,
        &max_tokens_profile(),
    )
    .unwrap();
    assert_eq!(wire["max_tokens"], json!(DEFAULT_MAX_TOKENS));
}

#[test]
fn the_responses_cap_follows_the_provider_default_too() {
    let req = parse_request(
        Dialect::AnthropicMessages,
        &json!({"model": "m", "max_tokens": 64,
                "messages": [{"role": "user", "content": "hi"}]}),
    )
    .unwrap();
    let wire =
        build_request_with(Dialect::OpenAiResponses, &req, false, &openai_profile()).unwrap();
    assert_eq!(wire["max_output_tokens"], json!(64));
    assert!(wire.get("max_completion_tokens").is_none());
    assert!(wire.get("max_tokens").is_none());
}

// ---------------------------------------------------------------------------
// Request round trips
// ---------------------------------------------------------------------------

/// Thinking effort survives Chat → Anthropic (budget) → Chat (effort), so a harness can
/// keep one level while the routed model changes dialect.
#[test]
fn reasoning_effort_crosses_chat_and_anthropic() {
    let chat = parse_request(
        Dialect::OpenAiChat,
        &json!({"model":"m","messages":[],"reasoning_effort":"high"}),
    )
    .unwrap();
    let anthropic =
        build_request_with(Dialect::AnthropicMessages, &chat, false, &claude_profile()).unwrap();
    assert_eq!(anthropic["thinking"]["type"], "enabled");
    let budget = anthropic["thinking"]["budget_tokens"]
        .as_u64()
        .expect("budget");
    let max = anthropic["max_tokens"].as_u64().expect("max");
    assert!(
        budget < max,
        "Claude requires max_tokens > budget: {anthropic}"
    );

    let back = parse_request(Dialect::AnthropicMessages, &anthropic).unwrap();
    let chat = build_request_with(Dialect::OpenAiChat, &back, false, &openai_profile()).unwrap();
    assert_eq!(chat["reasoning_effort"], "high");
    assert!(
        chat.get("temperature").is_none(),
        "no invented sampling: {chat}"
    );
}

#[test]
fn a_client_request_round_trips_through_anthropic_unchanged_on_the_wire() {
    let original = chat_request();
    let first = parse_request(Dialect::OpenAiChat, &original).unwrap();
    let messages = build_request_with(
        Dialect::AnthropicMessages,
        &first,
        false,
        &max_tokens_profile(),
    )
    .unwrap();

    // Anthropic shape: system hoisted, flat tools, translated tool_choice and stop.
    assert_eq!(messages["system"], json!("be terse"));
    assert_eq!(messages["max_tokens"], json!(512));
    assert_eq!(messages["stop_sequences"], json!(["END"]));
    assert_eq!(
        messages["tool_choice"],
        json!({"type": "tool", "name": "get_weather"})
    );
    assert_eq!(messages["tools"][0]["name"], json!("get_weather"));
    assert!(messages["tools"][0].get("function").is_none());

    // Assistant turn: reasoning first, then the call, in canonical order.
    let assistant = &messages["messages"][1]["content"];
    assert_eq!(assistant[0]["type"], json!("thinking"));
    assert_eq!(assistant[0]["thinking"], json!("need the tool"));
    assert_eq!(assistant[1]["type"], json!("tool_use"));
    assert_eq!(assistant[1]["input"], json!({"city": "Hanoi"}));
    // The tool result rides in the following user turn.
    assert_eq!(
        messages["messages"][2]["content"][0]["type"],
        json!("tool_result")
    );

    // Re-parsing and rebuilding must produce the same bytes the original chat request
    // would have produced: the cycle is lossless for everything the canonical model holds.
    let second = parse_request(Dialect::AnthropicMessages, &messages).unwrap();
    assert_eq!(second.model, first.model);
    assert_eq!(second.system, first.system);
    assert_eq!(second.max_tokens, first.max_tokens);
    assert_eq!(second.temperature, first.temperature);
    assert_eq!(second.top_p, first.top_p);
    assert_eq!(second.stop, first.stop);
    assert_eq!(second.tool_choice, first.tool_choice);
    assert_eq!(second.tools, first.tools);

    let chat_again =
        build_request_with(Dialect::OpenAiChat, &second, false, &max_tokens_profile()).unwrap();
    let chat_direct =
        build_request_with(Dialect::OpenAiChat, &first, false, &max_tokens_profile()).unwrap();
    assert_eq!(
        chat_again, chat_direct,
        "a full chat→messages→chat cycle must be wire-identical"
    );
}

#[test]
fn a_chat_request_round_trips_through_responses_with_only_reported_losses() {
    let first = parse_request(Dialect::OpenAiChat, &chat_request()).unwrap();
    let responses = build_request_with(
        Dialect::OpenAiResponses,
        &first,
        false,
        &max_tokens_profile(),
    )
    .unwrap();

    assert_eq!(responses["instructions"], json!("be terse"));
    assert_eq!(responses["max_output_tokens"], json!(512));
    assert_eq!(responses["store"], json!(false));
    assert_eq!(responses["tools"][0]["name"], json!("get_weather"));
    let input = responses["input"].as_array().expect("input");
    assert!(input
        .iter()
        .any(|item| item["type"] == json!("function_call")));
    assert!(input
        .iter()
        .any(|item| item["type"] == json!("function_call_output")));

    let second = parse_request(Dialect::OpenAiResponses, &responses).unwrap();
    let chat_again =
        build_request_with(Dialect::OpenAiChat, &second, false, &max_tokens_profile()).unwrap();
    let chat_direct =
        build_request_with(Dialect::OpenAiChat, &first, false, &max_tokens_profile()).unwrap();

    // Two canonical fields have no Responses spelling at all. They are reported rather
    // than lost quietly, and everything else must come back byte-identical.
    let mut expected = chat_direct;
    expected.as_object_mut().expect("object").remove("stop");
    let assistant = 2; // system, user, assistant, tool
    expected["messages"][assistant]
        .as_object_mut()
        .expect("assistant message")
        .remove("reasoning_content");
    assert_eq!(chat_again, expected);
    assert_eq!(
        dropped_fields(&first, Dialect::OpenAiResponses),
        vec!["reasoning".to_string(), "stop".to_string()]
    );
}

/// A user turn that mixes prose with a tool result (what Anthropic clients send) must be
/// written as `tool` *then* `user`, with no empty user message in between.
#[test]
fn a_translated_tool_turn_keeps_openais_required_ordering() {
    let canonical = parse_request(
        Dialect::AnthropicMessages,
        &json!({
            "model": "claude-sonnet-4-5",
            "max_tokens": 64,
            "messages": [
                {"role": "user", "content": "weather?"},
                {"role": "assistant", "content": [
                    {"type": "tool_use", "id": "toolu_1", "name": "get_weather", "input": {}},
                    {"type": "tool_use", "id": "toolu_2", "name": "get_time", "input": {}}
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "toolu_1", "content": "31C"},
                    {"type": "tool_result", "tool_use_id": "toolu_2", "content": "noon"},
                    {"type": "text", "text": "both done"}
                ]}
            ]
        }),
    )
    .unwrap();

    let chat = build_request_with(
        Dialect::OpenAiChat,
        &canonical,
        false,
        &max_tokens_profile(),
    )
    .unwrap();
    let messages = chat["messages"].as_array().expect("messages");
    let tail: Vec<&str> = messages
        .iter()
        .map(|m| m["role"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(
        tail,
        vec!["user", "assistant", "tool", "tool", "user"],
        "one tool message per call, immediately after the assistant turn: {chat}"
    );
    assert_eq!(messages[3]["tool_call_id"], json!("toolu_2"));
    assert_eq!(messages[4]["content"], json!("both done"));
    assert!(
        messages
            .iter()
            .all(|m| !(m["role"] == json!("user") && m["content"].is_null())),
        "no empty user message may be produced: {chat}"
    );
}

// ---------------------------------------------------------------------------
// Response round trips
// ---------------------------------------------------------------------------

fn chat_response_body() -> Value {
    json!({
        "id": "chatcmpl-9",
        "object": "chat.completion",
        "created": 1,
        "model": "mistral-large-latest",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": "checking",
                "tool_calls": [{"id": "call_z", "type": "function",
                    "function": {"name": "get_weather", "arguments": "{\"city\":\"Hue\"}"}}]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": {"prompt_tokens": 40, "completion_tokens": 12, "total_tokens": 52}
    })
}

#[test]
fn a_chat_response_round_trips_through_both_dialects() {
    let response = parse_response(Dialect::OpenAiChat, &chat_response_body()).unwrap();
    assert_eq!(response.finish_reason, FinishReason::ToolCalls);
    assert_eq!(
        response.usage,
        Usage {
            input_tokens: 40,
            output_tokens: 12,
            cache_read_tokens: 0,
            cache_write_tokens: 0
        }
    );

    for dialect in [Dialect::AnthropicMessages, Dialect::OpenAiResponses] {
        let body = build_response(dialect, &response).unwrap();
        let reparsed = parse_response(dialect, &body).unwrap();
        assert_eq!(reparsed.parts, response.parts, "{dialect}: parts");
        assert_eq!(
            reparsed.finish_reason, response.finish_reason,
            "{dialect}: finish"
        );
        assert_eq!(reparsed.usage, response.usage, "{dialect}: usage");

        // And back out to the client's dialect: the call identity must survive.
        let client = build_response(Dialect::OpenAiChat, &reparsed).unwrap();
        assert_eq!(client["choices"][0]["finish_reason"], json!("tool_calls"));
        assert_eq!(
            client["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"],
            json!("{\"city\":\"Hue\"}")
        );
        assert_eq!(client["usage"]["prompt_tokens"], json!(40));
    }
}

#[test]
fn reasoning_text_survives_an_anthropic_round_trip() {
    let response = parse_response(
        Dialect::OpenAiChat,
        &json!({
            "id": "c1",
            "model": "deepseek-reasoner",
            "choices": [{"index": 0, "message": {
                "role": "assistant", "content": "done", "reasoning_content": "step one, step two"
            }, "finish_reason": "stop"}],
        }),
    )
    .unwrap();
    let anthropic = build_response(Dialect::AnthropicMessages, &response).unwrap();
    assert_eq!(anthropic["content"][0]["type"], json!("thinking"));
    let back = parse_response(Dialect::AnthropicMessages, &anthropic).unwrap();
    assert_eq!(
        back.parts,
        vec![
            Part::Reasoning {
                text: "step one, step two".into(),
                signature: None
            },
            Part::Text("done".into()),
        ]
    );
}

// ---------------------------------------------------------------------------
// Streaming: the tool-call sequence must be stable
// ---------------------------------------------------------------------------

/// Chat SSE for a text-before-tool turn, with the argument JSON split mid-token.
fn chat_sse() -> String {
    let chunk = |delta: Value, finish: Option<&str>| {
        let mut payload = json!({
            "id": "chatcmpl-1",
            "object": "chat.completion.chunk",
            "created": 1,
            "model": "mistral-large-latest",
            "choices": [{"index": 0, "delta": delta, "finish_reason": finish}],
        });
        if finish.is_some() {
            payload["choices"][0]["delta"] = json!({});
        }
        render_openai(&payload.to_string(), None)
    };
    let mut wire = String::new();
    wire.push_str(&chunk(
        json!({"role": "assistant", "content": "Checking "}),
        None,
    ));
    wire.push_str(&chunk(
        json!({"tool_calls": [{"index": 0, "id": "call_a", "type": "function",
            "function": {"name": "get_weather", "arguments": ""}}]}),
        None,
    ));
    wire.push_str(&chunk(
        json!({"tool_calls": [{"index": 0, "function": {"arguments": "{\"city\":"}}]}),
        None,
    ));
    wire.push_str(&chunk(
        json!({"tool_calls": [{"index": 0, "function": {"arguments": "\"Hanoi\"}"}}]}),
        None,
    ));
    wire.push_str(&chunk(json!({}), Some("tool_calls")));
    wire.push_str(&render_openai(
        &json!({"id": "chatcmpl-1", "model": "mistral-large-latest", "choices": [],
                "usage": {"prompt_tokens": 11, "completion_tokens": 7, "total_tokens": 18}})
        .to_string(),
        None,
    ));
    wire.push_str(&render_openai("[DONE]", None));
    wire
}

fn decode(dialect: Dialect, bytes: &[u8], chunk: usize) -> Vec<StreamEvent> {
    let mut decoder = StreamDecoder::new(dialect, "client-model");
    let mut events = Vec::new();
    if chunk == 0 {
        events.extend(decoder.push(bytes).unwrap());
    } else {
        for part in bytes.chunks(chunk) {
            events.extend(decoder.push(part).unwrap());
        }
    }
    events.extend(decoder.finish().unwrap());
    events
}

fn encode_all(dialect: Dialect, events: &[StreamEvent]) -> Vec<u8> {
    let mut encoder = StreamEncoder::new(dialect, "client-model");
    let mut bytes = Vec::new();
    for event in events {
        bytes.extend_from_slice(&encoder.encode(event).unwrap());
    }
    bytes
}

#[test]
fn a_streamed_tool_call_sequence_is_stable_through_the_anthropic_encoder() {
    let upstream = chat_sse();
    let expected = decode(Dialect::OpenAiChat, upstream.as_bytes(), 0);

    // The fixture must exercise what it claims to.
    assert_eq!(
        expected,
        vec![
            StreamEvent::Start {
                id: "chatcmpl-1".into(),
                model: "mistral-large-latest".into()
            },
            StreamEvent::TextDelta("Checking ".into()),
            StreamEvent::ToolCallStart {
                index: 0,
                id: "call_a".into(),
                name: "get_weather".into()
            },
            StreamEvent::ToolCallArgsDelta {
                index: 0,
                partial_json: "{\"city\":".into()
            },
            StreamEvent::ToolCallArgsDelta {
                index: 0,
                partial_json: "\"Hanoi\"}".into()
            },
            StreamEvent::Done {
                reason: FinishReason::ToolCalls,
                usage: Some(Usage {
                    input_tokens: 11,
                    output_tokens: 7,
                    cache_read_tokens: 0,
                    cache_write_tokens: 0
                })
            },
        ]
    );

    let first = encode_all(Dialect::AnthropicMessages, &expected);
    let second = encode_all(Dialect::AnthropicMessages, &expected);
    assert_eq!(first, second, "encoding must be deterministic");

    // Byte-semantic stability: decoding the emitted bytes, whole or split at arbitrary
    // offsets, reconstructs exactly the same canonical event sequence.
    assert_eq!(decode(Dialect::AnthropicMessages, &first, 0), expected);
    assert_eq!(decode(Dialect::AnthropicMessages, &first, 7), expected);

    // The argument fragments the client receives reassemble into the original JSON.
    let joined: String = expected
        .iter()
        .filter_map(|event| match event {
            StreamEvent::ToolCallArgsDelta { partial_json, .. } => Some(partial_json.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        serde_json::from_str::<Value>(&joined).unwrap(),
        json!({"city": "Hanoi"})
    );
}

#[test]
fn a_streamed_tool_call_sequence_is_stable_through_the_responses_encoder() {
    let upstream = chat_sse();
    let expected = decode(Dialect::OpenAiChat, upstream.as_bytes(), 0);
    let bytes = encode_all(Dialect::OpenAiResponses, &expected);
    assert_eq!(decode(Dialect::OpenAiResponses, &bytes, 0), expected);
    assert_eq!(decode(Dialect::OpenAiResponses, &bytes, 5), expected);
}

#[test]
fn streamed_reasoning_and_usage_survive_a_cycle() {
    let upstream = {
        let frame = render_openai(
            &json!({
                "id": "chatcmpl-r",
                "model": "deepseek-reasoner",
                "choices": [{"index": 0, "delta": {"reasoning_content": "think "}}],
            })
            .to_string(),
            None,
        );
        let frame2 = render_openai(
            &json!({
                "id": "chatcmpl-r",
                "model": "deepseek-reasoner",
                "choices": [{"index": 0, "delta": {"content": "answer"}, "finish_reason": "stop"}],
                "usage": {"prompt_tokens": 3, "completion_tokens": 9},
            })
            .to_string(),
            None,
        );
        format!("{frame}{frame2}{}", render_openai("[DONE]", None))
    };
    let events = decode(Dialect::OpenAiChat, upstream.as_bytes(), 0);
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, StreamEvent::ReasoningDelta(_)))
            .count(),
        1
    );

    let bytes = encode_all(Dialect::AnthropicMessages, &events);
    let back = decode(Dialect::AnthropicMessages, &bytes, 11);
    assert_eq!(
        back, events,
        "reasoning, text and usage must survive the cycle"
    );
    assert_eq!(
        back.last(),
        Some(&StreamEvent::Done {
            reason: FinishReason::Stop,
            usage: Some(Usage {
                input_tokens: 3,
                output_tokens: 9,
                cache_read_tokens: 0,
                cache_write_tokens: 0
            })
        })
    );
}

// ---------------------------------------------------------------------------
// Field portability
// ---------------------------------------------------------------------------

#[test]
fn portable_openai_fields_cross_dialects_and_the_rest_are_reported() {
    let body = json!({
        "model": "gpt-4.1",
        "messages": [{"role": "user", "content": "hi"}],
        "service_tier": "flex",
        "prompt_cache_key": "conv-42",
        "response_format": {"type": "json_object"},
        "seed": 7,
        "logprobs": true,
    });
    let req = parse_request(Dialect::OpenAiChat, &body).unwrap();

    // Same dialect: everything travels, as before.
    let chat = build_request_with(Dialect::OpenAiChat, &req, false, &openai_profile()).unwrap();
    assert_eq!(chat["service_tier"], json!("flex"));
    assert_eq!(chat["response_format"], json!({"type": "json_object"}));
    assert_eq!(chat["seed"], json!(7));

    // Chat ↔ Responses: the three fields both APIs document under the same name travel.
    let responses =
        build_request_with(Dialect::OpenAiResponses, &req, false, &openai_profile()).unwrap();
    assert_eq!(responses["service_tier"], json!("flex"));
    assert_eq!(responses["prompt_cache_key"], json!("conv-42"));
    assert!(
        responses.get("response_format").is_none(),
        "Responses spells it text.format; a same-name rewrite would be an invented field"
    );
    assert!(responses.get("seed").is_none());
    assert!(responses.get("logprobs").is_none());

    // Nothing is lost silently: the caller can name what did not travel.
    let dropped = dropped_fields(&req, Dialect::OpenAiResponses);
    assert_eq!(
        dropped,
        vec![
            "logprobs".to_string(),
            "response_format".to_string(),
            "seed".to_string()
        ]
    );
    assert_eq!(
        dropped_fields(&req, Dialect::OpenAiChat),
        Vec::<String>::new(),
        "nothing is dropped in the dialect that produced the fields"
    );

    // Anthropic has no counterpart for any of them, so all four are reported.
    let dropped = dropped_fields(&req, Dialect::AnthropicMessages);
    assert!(dropped.contains(&"service_tier".to_string()));
    assert!(dropped.contains(&"prompt_cache_key".to_string()));
    let anthropic = build_request_with(
        Dialect::AnthropicMessages,
        &req,
        false,
        &max_tokens_profile(),
    )
    .unwrap();
    assert!(anthropic.get("service_tier").is_none());
    assert!(anthropic.get("prompt_cache_key").is_none());
}

#[test]
fn provider_native_tools_are_reported_when_the_dialect_changes() {
    let req = parse_request(
        Dialect::AnthropicMessages,
        &json!({
            "model": "claude-sonnet-4-5",
            "max_tokens": 16,
            "messages": [{"role": "user", "content": "hi"}],
            "tools": [{"type": "web_search_20250305", "name": "web_search"}],
        }),
    )
    .unwrap();
    assert!(dropped_fields(&req, Dialect::AnthropicMessages).is_empty());
    assert_eq!(
        dropped_fields(&req, Dialect::OpenAiChat),
        vec!["provider_tools".to_string()]
    );
}

// ---------------------------------------------------------------------------
// Upstream errors
// ---------------------------------------------------------------------------

#[test]
fn an_upstream_error_keeps_its_payload_and_status_envelope() {
    let payload = json!({"error": {
        "message": "Unsupported parameter: max_tokens",
        "type": "invalid_request_error",
        "param": "max_tokens",
        "code": "unsupported_parameter",
    }});

    // OpenAI-shaped client: status is the caller's, the structured fields are preserved.
    let body = upstream_error_body(Dialect::OpenAiChat, 400, Some(&payload), "ignored");
    assert_eq!(body["error"]["code"], json!("unsupported_parameter"));
    assert_eq!(body["error"]["param"], json!("max_tokens"));
    assert_eq!(body["error"]["type"], json!("invalid_request_error"));

    // Anthropic-shaped client: the same facts, reframed in its envelope.
    let body = upstream_error_body(Dialect::AnthropicMessages, 400, Some(&payload), "ignored");
    assert_eq!(body["type"], json!("error"));
    assert_eq!(body["error"]["type"], json!("invalid_request_error"));
    assert_eq!(
        body["error"]["message"],
        json!("Unsupported parameter: max_tokens")
    );

    // A provider that answered plain text still yields a typed body.
    let body = upstream_error_body(
        Dialect::OpenAiChat,
        429,
        None,
        "upstream mistral returned 429",
    );
    assert_eq!(body["error"]["type"], json!("rate_limit_error"));
    assert_eq!(
        body["error"]["message"],
        json!("upstream mistral returned 429")
    );
}

#[test]
fn a_tool_result_only_turn_is_one_tool_message() {
    // Guards the ordering fix from the other side: no stray user message.
    let req = parse_request(
        Dialect::OpenAiChat,
        &json!({
            "model": "m",
            "messages": [
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": null, "tool_calls": [
                    {"id": "c1", "type": "function", "function": {"name": "f", "arguments": "{}"}}]},
                {"role": "tool", "tool_call_id": "c1", "content": "ok"}
            ]
        }),
    )
    .unwrap();
    // A tool-only canonical turn (as an Anthropic client produces) must not gain an
    // empty user message on the way to Chat.
    let mut canonical = req.clone();
    canonical.messages[2].role = Role::User;
    let wire = build_request_with(
        Dialect::OpenAiChat,
        &canonical,
        false,
        &max_tokens_profile(),
    )
    .unwrap();
    let roles: Vec<&str> = wire["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .map(|m| m["role"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(roles, vec!["user", "assistant", "tool"]);
}
