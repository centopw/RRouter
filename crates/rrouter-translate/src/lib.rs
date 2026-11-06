//! Wire dialects ↔ canonical model.
//!
//! Every supported wire protocol is parsed into `rrouter_core` types and
//! serialized back out of them, for both buffered bodies and streams. That keeps
//! the proxy at 2N conversions (dialect ↔ canonical) instead of N² pairwise
//! translators.

mod anthropic;
mod openai_chat;
mod responses;

pub use anthropic::{AnthropicDecoder, AnthropicEncoder};
pub use openai_chat::{OpenAiChatDecoder, OpenAiChatEncoder};
pub use responses::{ResponsesDecoder, ResponsesEncoder};

use serde_json::{Map, Value};

use rrouter_core::{
    ChatRequest, ChatResponse, Dialect, Error, FinishReason, Result, StreamEvent, Usage,
};



/// Parse an inbound client body into canonical form.
pub fn parse_request(dialect: Dialect, body: &Value) -> Result<ChatRequest> {
    match dialect {
        Dialect::OpenAiChat => openai_chat::parse_request(body),
        Dialect::AnthropicMessages => anthropic::parse_request(body),
        Dialect::OpenAiResponses => responses::parse_request(body),
        Dialect::OpenAiEmbeddings => Ok(openai_chat::parse_embeddings_request(body)),
    }
}

/// Serialize a canonical request into the outbound provider body for `dialect`.
/// `stream` is authoritative over `req.stream`.
pub fn build_request(dialect: Dialect, req: &ChatRequest, stream: bool) -> Result<Value> {
    match dialect {
        Dialect::OpenAiChat => openai_chat::build_request(req, stream),
        Dialect::AnthropicMessages => anthropic::build_request(req, stream),
        Dialect::OpenAiResponses => responses::build_request(req, stream),
        Dialect::OpenAiEmbeddings => Ok(openai_chat::build_embeddings_request(req)),
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
    }
}

/// Stateful per-stream decoder: bytes → [`SseFrame`](rrouter_core::sse::SseFrame) → [`StreamEvent`].
pub enum StreamDecoder {
    OpenAiChat(OpenAiChatDecoder),
    Anthropic(AnthropicDecoder),
    Responses(ResponsesDecoder),
}

impl StreamDecoder {
    pub fn new(dialect: Dialect, model: &str) -> Self {
        match dialect {
            Dialect::OpenAiChat => Self::OpenAiChat(OpenAiChatDecoder::new(model)),
            Dialect::AnthropicMessages => Self::Anthropic(AnthropicDecoder::new(model)),
            Dialect::OpenAiResponses => Self::Responses(ResponsesDecoder::new(model)),
            Dialect::OpenAiEmbeddings => Self::OpenAiChat(OpenAiChatDecoder::new(model)),
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

/// Merge `req.extra` into `body` when it was captured from `dialect`.
fn merge_extra(body: &mut Value, req: &ChatRequest, dialect: Dialect) {
    let Some((src, extra)) = &req.extra else { return };
    if *src != dialect {
        return;
    }
    let (Some(dst), Some(src)) = (body.as_object_mut(), extra.as_object()) else {
        return;
    };
    for (k, v) in src {
        if !is_identity_key(k) {
            dst.insert(k.clone(), v.clone());
        }
    }
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

/// Parse a `tool_choice` string used by [OI] chat: auto|none|required|<name>.
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
    details.insert(
        "cached_tokens".into(),
        Value::from(usage.cache_read_tokens),
    );
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
        let (src, extra) = req.extra.clone().unwrap();
        assert_eq!(src, Dialect::OpenAiChat);
        assert_eq!(extra.get("service_tier"), Some(&Value::from("flex")));
        assert!(extra.get("model").is_none(), "identity keys are not passthrough");

        let same = build_request(Dialect::OpenAiChat, &req, false).unwrap();
        assert_eq!(same.get("service_tier"), Some(&Value::from("flex")));
        assert_eq!(same.pointer("/metadata/trace"), Some(&Value::from("abc")));

        let cross = build_request(Dialect::AnthropicMessages, &req, false).unwrap();
        assert!(cross.get("service_tier").is_none());
        assert!(cross.get("metadata").is_none());
    }
}
