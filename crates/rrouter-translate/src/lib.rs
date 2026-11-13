//! Wire dialects ↔ canonical model.
//!
//! Every supported wire protocol is parsed into `rrouter_core` types and
//! serialized back out of them, for both buffered bodies and streams. That keeps
//! the proxy at 2N conversions (dialect ↔ canonical) instead of N² pairwise
//! translators.

mod anthropic;
pub mod error;
pub mod google_cca;
mod openai_chat;
pub mod qoder;
mod reasoning;
pub mod rerank;
mod responses;
pub mod search;
mod wire;

pub use anthropic::{AnthropicDecoder, AnthropicEncoder};
pub use error::{parse_provider_error, upstream_error_body, ProviderError};
pub use openai_chat::{OpenAiChatDecoder, OpenAiChatEncoder};
pub use responses::{ResponsesDecoder, ResponsesEncoder};
pub use wire::{MaxTokensKey, WireProfile, DEFAULT_MAX_TOKENS};

use serde_json::{Map, Value};

use rrouter_core::{
    ChatRequest, ChatResponse, Dialect, Error, FinishReason, Part, Result, StreamEvent, Usage,
};

/// Parse an inbound client body into canonical form.
pub fn parse_request(dialect: Dialect, body: &Value) -> Result<ChatRequest> {
    match dialect {
        Dialect::OpenAiChat => openai_chat::parse_request(body),
        Dialect::AnthropicMessages => anthropic::parse_request(body),
        Dialect::OpenAiResponses => responses::parse_request(body),
        Dialect::OpenAiEmbeddings => Ok(openai_chat::parse_embeddings_request(body)),
        Dialect::GoogleCodeAssist => Err(Error::Translation(
            "google code assist is an upstream-only dialect".into(),
        )),
        Dialect::QoderAgent => Err(Error::Translation(
            "qoder agent is an upstream-only dialect".into(),
        )),
    }
}

/// Serialize a canonical request into the outbound provider body for `dialect`.
/// `stream` is authoritative over `req.stream`.
///
/// This is the provider-agnostic entry point: it assumes the widest-compatible wire
/// shape (see [`WireProfile::default`]). Callers that know which provider will answer
/// use [`build_request_with`] so the output cap is spelled the way that provider
/// accepts it.
pub fn build_request(dialect: Dialect, req: &ChatRequest, stream: bool) -> Result<Value> {
    build_request_with(dialect, req, stream, &WireProfile::default())
}

/// [`build_request`] with the provider's wire shape applied.
///
/// `wire` decides the output-cap key for the Chat dialect (`max_tokens` vs
/// `max_completion_tokens`) and the cap sent when the client named none. Dialects with
/// a fixed spelling (`max_tokens`, `max_output_tokens`) only use the default.
pub fn build_request_with(
    dialect: Dialect,
    req: &ChatRequest,
    stream: bool,
    wire: &WireProfile,
) -> Result<Value> {
    match dialect {
        Dialect::OpenAiChat => openai_chat::build_request(req, stream, wire),
        Dialect::AnthropicMessages => anthropic::build_request(req, stream, wire),
        Dialect::OpenAiResponses => responses::build_request(req, stream, wire),
        Dialect::OpenAiEmbeddings => Ok(openai_chat::build_embeddings_request(req)),
        Dialect::GoogleCodeAssist => Err(Error::Translation(
            "google code assist needs a project and request id; \
             call google_cca::build_request"
                .into(),
        )),
        Dialect::QoderAgent => Err(Error::Translation(
            "qoder agent needs the account's model_config; call qoder::build_request".into(),
        )),
    }
}

/// Parse a non-streaming upstream body into canonical form.
pub fn parse_response(dialect: Dialect, body: &Value) -> Result<ChatResponse> {
    match dialect {
        Dialect::OpenAiChat => openai_chat::parse_response(body),
        Dialect::AnthropicMessages => anthropic::parse_response(body),
        Dialect::OpenAiResponses => responses::parse_response(body),
        Dialect::OpenAiEmbeddings => Err(Error::Translation(
            "embeddings responses have no canonical chat form".into(),
        )),
        Dialect::GoogleCodeAssist => google_cca::parse_response(body),
        // The Qoder surface is SSE-only: a buffered body is not one of its shapes.
        Dialect::QoderAgent => Err(Error::Translation(
            "qoder agent answers as an SSE envelope, not a JSON body".into(),
        )),
    }
}

/// Serialize a canonical response into the inbound client dialect.
pub fn build_response(dialect: Dialect, resp: &ChatResponse) -> Result<Value> {
    match dialect {
        Dialect::OpenAiChat => openai_chat::build_response(resp),
        Dialect::AnthropicMessages => anthropic::build_response(resp),
        Dialect::OpenAiResponses => responses::build_response(resp),
        Dialect::OpenAiEmbeddings => Err(Error::Translation(
            "embeddings bodies have no canonical chat form".into(),
        )),
        Dialect::GoogleCodeAssist => Err(Error::Translation(
            "google code assist is an upstream-only dialect".into(),
        )),
        Dialect::QoderAgent => Err(Error::Translation(
            "qoder agent is an upstream-only dialect".into(),
        )),
    }
}

/// Stateful per-stream decoder: bytes → [`SseFrame`](rrouter_core::sse::SseFrame) → [`StreamEvent`].
pub enum StreamDecoder {
    OpenAiChat(OpenAiChatDecoder),
    Anthropic(AnthropicDecoder),
    Responses(ResponsesDecoder),
    GoogleCodeAssist(google_cca::GoogleCcaDecoder),
    QoderAgent(qoder::QoderAgentDecoder),
}

impl StreamDecoder {
    pub fn new(dialect: Dialect, model: &str) -> Self {
        match dialect {
            Dialect::OpenAiChat => Self::OpenAiChat(OpenAiChatDecoder::new(model)),
            Dialect::AnthropicMessages => Self::Anthropic(AnthropicDecoder::new(model)),
            Dialect::OpenAiResponses => Self::Responses(ResponsesDecoder::new(model)),
            Dialect::OpenAiEmbeddings => Self::OpenAiChat(OpenAiChatDecoder::new(model)),
            Dialect::GoogleCodeAssist => {
                Self::GoogleCodeAssist(google_cca::GoogleCcaDecoder::new(model))
            }
            Dialect::QoderAgent => Self::QoderAgent(qoder::QoderAgentDecoder::new(model)),
        }
    }

    /// Feed raw upstream bytes; returns the canonical events completed by them.
    /// Terminal `Done` is emitted exactly once.
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<StreamEvent>> {
        let mut out = Vec::new();
        match self {
            Self::OpenAiChat(d) => d.push(chunk, &mut out)?,
            Self::Anthropic(d) => d.push(chunk, &mut out)?,
            Self::Responses(d) => d.push(chunk, &mut out)?,
            Self::GoogleCodeAssist(d) => d.push(chunk, &mut out)?,
            Self::QoderAgent(d) => d.push(chunk, &mut out)?,
        }
        Ok(out)
    }

    /// Flush at EOF (upstream closed without a terminal signal).
    pub fn finish(&mut self) -> Result<Vec<StreamEvent>> {
        let mut out = Vec::new();
        match self {
            Self::OpenAiChat(d) => d.finish(&mut out)?,
            Self::Anthropic(d) => d.finish(&mut out)?,
            Self::Responses(d) => d.finish(&mut out)?,
            Self::GoogleCodeAssist(d) => d.finish(&mut out)?,
            Self::QoderAgent(d) => d.finish(&mut out)?,
        }
        Ok(out)
    }
}

/// Stateful per-stream encoder: [`StreamEvent`] → client-dialect SSE bytes.
pub enum StreamEncoder {
    OpenAiChat(OpenAiChatEncoder),
    Anthropic(AnthropicEncoder),
    Responses(ResponsesEncoder),
}

impl StreamEncoder {
    pub fn new(dialect: Dialect, model: &str) -> Self {
        match dialect {
            Dialect::OpenAiChat => Self::OpenAiChat(OpenAiChatEncoder::new(model)),
            Dialect::AnthropicMessages => Self::Anthropic(AnthropicEncoder::new(model)),
            Dialect::OpenAiResponses => Self::Responses(ResponsesEncoder::new(model)),
            Dialect::OpenAiEmbeddings => Self::OpenAiChat(OpenAiChatEncoder::new(model)),
            // CCA and Qoder are upstream-only; the gateway never encodes a client
            // response in either.
            Dialect::GoogleCodeAssist | Dialect::QoderAgent => {
                Self::OpenAiChat(OpenAiChatEncoder::new(model))
            }
        }
    }

    /// Wire bytes ready to write (empty for internal-only events).
    pub fn encode(&mut self, ev: &StreamEvent) -> Result<bytes::Bytes> {
        let out = match self {
            Self::OpenAiChat(e) => e.encode(ev),
            Self::Anthropic(e) => e.encode(ev),
            Self::Responses(e) => e.encode(ev),
        };
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// Shared helpers. Flat free functions: every translator needs the same few
// parsing primitives and there is no state to hang them off.
// ---------------------------------------------------------------------------

fn parse_err(dialect: Dialect, msg: &str) -> Error {
    Error::Translation(format!("{dialect}: {msg}"))
}

fn req_str<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

fn req_str_field(v: &Value, key: &str, dialect: Dialect) -> Result<String> {
    req_str(v, key)
        .map(str::to_owned)
        .ok_or_else(|| parse_err(dialect, &format!("missing string field `{key}`")))
}

fn opt_u32(v: &Value, key: &str) -> Option<u32> {
    v.get(key)
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
}

fn u32_at(v: &Value, key: &str) -> u32 {
    opt_u32(v, key).unwrap_or(0)
}

fn opt_f64(v: &Value, key: &str) -> Option<f64> {
    v.get(key).and_then(Value::as_f64)
}

/// Collect the body keys this crate does not model into `ChatRequest.extra`.
/// Unknown fields never cross dialects: they are merged back only when the
/// outbound dialect matches the dialect that produced them.
fn capture_extra(
    body: &Value,
    known: &[&str],
    dialect: Dialect,
    req: &mut ChatRequest,
) -> Result<()> {
    let Some(fields) = body.as_object() else {
        return Err(parse_err(dialect, "request body must be a JSON object"));
    };
    let mut unknown: Map<String, Value> = Map::new();
    for (k, v) in fields {
        if !known.contains(&k.as_str()) && !is_identity_key(k) {
            unknown.insert(k.clone(), v.clone());
        }
    }
    if !unknown.is_empty() {
        req.extra = Some((dialect, Value::Object(unknown)));
    }
    Ok(())
}

/// Model id keys are routing inputs, never passthrough: forwarding a stale
/// `model` from a different dialect would send the alias upstream verbatim.
fn is_identity_key(key: &str) -> bool {
    matches!(key, "model" | "stream")
}

/// Request fields whose **name and meaning** are identical in both OpenAI dialects, so
/// a client keeps them when the body is rebuilt for the other one.
///
/// Nothing is translated here: every key is written verbatim, which is only safe
/// because these keys are documented by both APIs. A field with a different name or
/// shape in the target (`response_format` vs `text.format`, `reasoning_effort` vs
/// `reasoning.effort`, `metadata` whose schema differs per vendor) is deliberately
/// absent — inventing a mapping would be worse than the drop.
const PORTABLE_OPENAI_FIELDS: &[&str] = &["service_tier", "prompt_cache_key", "safety_identifier"];

fn openai_family(dialect: Dialect) -> bool {
    matches!(dialect, Dialect::OpenAiChat | Dialect::OpenAiResponses)
}

/// Whether `key` may travel from `from` into `to` unchanged.
fn portable(key: &str, from: Dialect, to: Dialect) -> bool {
    from != to && openai_family(from) && openai_family(to) && PORTABLE_OPENAI_FIELDS.contains(&key)
}

/// Merge `req.extra` into `body`.
///
/// Everything travels when the outbound dialect is the one that produced the fields;
/// across dialects only [`PORTABLE_OPENAI_FIELDS`] do, because every other field was
/// captured precisely because this crate does not model its meaning.
fn merge_extra(body: &mut Value, req: &ChatRequest, dialect: Dialect) {
    let Some((src, extra)) = &req.extra else {
        return;
    };
    let (Some(dst), Some(fields)) = (body.as_object_mut(), extra.as_object()) else {
        return;
    };
    for (k, v) in fields {
        if is_identity_key(k) {
            continue;
        }
        if *src == dialect || portable(k, *src, dialect) {
            dst.insert(k.clone(), v.clone());
        }
    }
}

/// Fields [`build_request_with`] will not carry into `target`.
///
/// A translation drops what the target cannot express; the names are reported here so a
/// caller can log or annotate the request instead of losing the fact silently. Values
/// are never invented, and nothing in this list is a field the gateway models itself.
pub fn dropped_fields(req: &ChatRequest, target: Dialect) -> Vec<String> {
    let mut out = Vec::new();
    if let Some((src, extra)) = &req.extra {
        if let Some(fields) = extra.as_object() {
            for key in fields.keys() {
                if is_identity_key(key) || *src == target || portable(key, *src, target) {
                    continue;
                }
                out.push(key.clone());
            }
        }
    }
    if let Some((src, _)) = &req.raw_tools {
        if *src != target {
            out.push("provider_tools".to_string());
        }
    }
    if target == Dialect::OpenAiResponses {
        // The Responses API has no stop sequences, and a foreign turn's reasoning can
        // only be replayed with the provider's own encrypted payload.
        if !req.stop.is_empty() {
            out.push("stop".to_string());
        }
        let has_reasoning = req
            .messages
            .iter()
            .any(|m| m.parts.iter().any(|p| matches!(p, Part::Reasoning { .. })));
        if has_reasoning {
            out.push("reasoning".to_string());
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Concatenate stop sequences; accepts a string or an array of strings.
fn parse_stop(v: &Value) -> Vec<String> {
    match v {
        Value::String(s) => vec![s.clone()],
        Value::Array(items) => items
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

/// Parse a `tool_choice` value used by [OI] chat: the string form
/// (`auto|none|required|<name>`) or the object form
/// (`{"type":"function","function":{"name":…}}`).
///
/// The object form is what every OpenAI SDK sends for a forced tool; reading only `name`
/// at the top level dropped the forcing silently on the way to another dialect.
fn parse_openai_tool_choice(v: &Value) -> Option<rrouter_core::ToolChoice> {
    use rrouter_core::ToolChoice;
    if let Some(s) = v.as_str() {
        return Some(match s {
            "auto" => ToolChoice::Auto,
            "none" => ToolChoice::None,
            "required" => ToolChoice::Required,
            other => ToolChoice::Tool(other.to_owned()),
        });
    }
    if let Some(kind) = v.get("type").and_then(Value::as_str) {
        match kind {
            "auto" => return Some(ToolChoice::Auto),
            "none" => return Some(ToolChoice::None),
            "required" | "any" => return Some(ToolChoice::Required),
            "function" | "tool" => {
                return v
                    .pointer("/function/name")
                    .and_then(Value::as_str)
                    .or_else(|| v.get("name").and_then(Value::as_str))
                    .map(|n| ToolChoice::Tool(n.to_owned()))
            }
            _ => return None,
        }
    }
    v.get("name")
        .and_then(Value::as_str)
        .map(|n| ToolChoice::Tool(n.to_owned()))
}

fn openai_tool_choice_value(choice: &rrouter_core::ToolChoice) -> Value {
    use rrouter_core::ToolChoice;
    match choice {
        ToolChoice::Tool(name) => {
            serde_json::json!({"type": "function", "function": {"name": name}})
        }
        other => Value::String(
            match other {
                ToolChoice::Auto => "auto",
                ToolChoice::None => "none",
                _ => "required",
            }
            .to_owned(),
        ),
    }
}

/// `arguments` fragments arrive as strings but some providers send an object.
fn arguments_to_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

fn parse_arguments(s: &str) -> Value {
    if s.trim().is_empty() {
        return Value::Object(Map::new());
    }
    serde_json::from_str(s).unwrap_or_else(|_| Value::String(s.to_owned()))
}

/// `arguments` is a JSON-encoded *string* in both [OI] dialects.
fn arguments_value(input: &Value) -> Result<Value> {
    Ok(Value::String(match input {
        Value::String(s) => s.clone(),
        Value::Null => "{}".to_owned(),
        other => serde_json::to_string(other)
            .map_err(|e| Error::Translation(format!("tool arguments: {e}")))?,
    }))
}

fn to_json<T: serde::Serialize>(v: &T, dialect: Dialect) -> Result<Value> {
    serde_json::to_value(v).map_err(|e| Error::Translation(format!("{dialect}: {e}")))
}

fn finish_from_openai(s: Option<&str>) -> FinishReason {
    match s {
        Some("stop") | Some("end_turn") => FinishReason::Stop,
        Some("length") | Some("max_tokens") => FinishReason::Length,
        Some("tool_calls") | Some("function_call") | Some("tool_use") => FinishReason::ToolCalls,
        Some("content_filter") | Some("refusal") => FinishReason::ContentFilter,
        _ => FinishReason::Other,
    }
}

fn finish_from_anthropic(s: Option<&str>) -> FinishReason {
    match s {
        Some("end_turn") | Some("stop_sequence") => FinishReason::Stop,
        Some("max_tokens") => FinishReason::Length,
        Some("tool_use") => FinishReason::ToolCalls,
        Some("refusal") => FinishReason::ContentFilter,
        _ => FinishReason::Other,
    }
}

fn finish_from_responses(status: Option<&str>, incomplete_reason: Option<&str>) -> FinishReason {
    match status {
        Some("incomplete") => match incomplete_reason {
            Some("max_output_tokens") => FinishReason::Length,
            Some("content_filter") => FinishReason::ContentFilter,
            _ => FinishReason::Other,
        },
        Some("failed") => FinishReason::Other,
        _ => FinishReason::Stop,
    }
}

fn usage_from_openai(u: Option<&Value>) -> Usage {
    let Some(u) = u else { return Usage::default() };
    Usage {
        input_tokens: u32_at(u, "prompt_tokens"),
        output_tokens: u32_at(u, "completion_tokens"),
        cache_read_tokens: u
            .pointer("/prompt_tokens_details/cached_tokens")
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok())
            .unwrap_or(0),
        cache_write_tokens: 0,
    }
}

fn usage_to_openai(usage: Usage) -> Value {
    let mut details = Map::new();
    details.insert("cached_tokens".into(), Value::from(usage.cache_read_tokens));
    serde_json::json!({
        "prompt_tokens": usage.input_tokens,
        "completion_tokens": usage.output_tokens,
        "total_tokens": usage.total(),
        "prompt_tokens_details": Value::Object(details),
    })
}

fn usage_from_anthropic(u: Option<&Value>) -> Usage {
    let Some(u) = u else { return Usage::default() };
    Usage {
        input_tokens: u32_at(u, "input_tokens"),
        output_tokens: u32_at(u, "output_tokens"),
        cache_read_tokens: u32_at(u, "cache_read_input_tokens"),
        cache_write_tokens: u32_at(u, "cache_creation_input_tokens"),
    }
}

fn usage_to_anthropic(usage: Usage) -> Value {
    serde_json::json!({
        "input_tokens": usage.input_tokens,
        "output_tokens": usage.output_tokens,
        "cache_read_input_tokens": usage.cache_read_tokens,
        "cache_creation_input_tokens": usage.cache_write_tokens,
    })
}

fn usage_from_responses(u: Option<&Value>) -> Usage {
    let Some(u) = u else { return Usage::default() };
    Usage {
        input_tokens: u32_at(u, "input_tokens"),
        output_tokens: u32_at(u, "output_tokens"),
        cache_read_tokens: u
            .pointer("/input_tokens_details/cached_tokens")
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok())
            .unwrap_or(0),
        cache_write_tokens: 0,
    }
}

fn usage_to_responses(usage: Usage) -> Value {
    serde_json::json!({
        "input_tokens": usage.input_tokens,
        "output_tokens": usage.output_tokens,
        "total_tokens": usage.total(),
        "input_tokens_details": {"cached_tokens": usage.cache_read_tokens},
        "output_tokens_details": {"reasoning_tokens": 0},
    })
}

fn tool_schema(params: &Value) -> Value {
    if params.is_null() {
        serde_json::json!({"type": "object", "properties": {}})
    } else {
        params.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_fields_are_captured_and_only_merged_back_for_the_same_dialect() {
        let body = serde_json::json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "hi"}],
            "service_tier": "flex",
            "metadata": {"trace": "abc"},
            "stream": false
        });
        let req = parse_request(Dialect::OpenAiChat, &body).unwrap();
        assert_eq!(req.model, "gpt-4o");
        let (src, extra) = req.extra.clone().unwrap();
        assert_eq!(src, Dialect::OpenAiChat);
        assert_eq!(extra.get("service_tier"), Some(&Value::from("flex")));
        assert!(
            extra.get("model").is_none(),
            "identity keys are not passthrough"
        );

        let same = build_request(Dialect::OpenAiChat, &req, false).unwrap();
        assert_eq!(same.get("service_tier"), Some(&Value::from("flex")));
        assert_eq!(same.pointer("/metadata/trace"), Some(&Value::from("abc")));

        let cross = build_request(Dialect::AnthropicMessages, &req, false).unwrap();
        assert!(cross.get("service_tier").is_none());
        assert!(cross.get("metadata").is_none());
    }

    #[test]
    fn embeddings_passthrough_keeps_input_in_extra() {
        let body = serde_json::json!({
            "model": "text-embedding-3-small",
            "input": ["one", "two"],
            "encoding_format": "float"
        });
        let req = parse_request(Dialect::OpenAiEmbeddings, &body).unwrap();
        assert_eq!(req.model, "text-embedding-3-small");
        let (src, extra) = req.extra.clone().unwrap();
        assert_eq!(src, Dialect::OpenAiEmbeddings);
        assert_eq!(extra["input"], serde_json::json!(["one", "two"]));
        let out = build_request(Dialect::OpenAiEmbeddings, &req, false).unwrap();
        assert_eq!(out["input"], serde_json::json!(["one", "two"]));
        assert_eq!(out["encoding_format"], "float");
        assert_eq!(out["model"], "text-embedding-3-small");
    }

    #[test]
    fn embeddings_have_no_canonical_response() {
        let err = parse_response(Dialect::OpenAiEmbeddings, &serde_json::json!({})).unwrap_err();
        assert!(matches!(err, Error::Translation(_)));
    }

    #[test]
    fn fragmented_300_byte_frame_parses_in_7_byte_chunks() {
        let text = "x".repeat(240);
        let payload = serde_json::json!({
            "id": "chatcmpl-chunk",
            "model": "gpt-4o",
            "choices": [{"index": 0, "delta": {"content": text}}]
        });
        let frame = rrouter_core::sse::render_openai(&payload.to_string(), None);
        assert!(
            frame.len() > 300,
            "frame should exceed 300 bytes: {}",
            frame.len()
        );

        let decode = |chunk_size: usize| {
            let mut dec = StreamDecoder::new(Dialect::OpenAiChat, "m");
            let mut events = Vec::new();
            for chunk in frame.as_bytes().chunks(chunk_size) {
                events.extend(dec.push(chunk).unwrap());
            }
            events.extend(dec.finish().unwrap());
            events
        };
        let whole = decode(frame.len());
        assert_eq!(
            decode(7),
            whole,
            "frame split at arbitrary offsets must decode identically"
        );
        assert_eq!(
            whole,
            vec![
                StreamEvent::Start {
                    id: "chatcmpl-chunk".into(),
                    model: "gpt-4o".into()
                },
                StreamEvent::TextDelta(text),
                StreamEvent::Done {
                    reason: FinishReason::Stop,
                    usage: None
                },
            ]
        );
    }
}
