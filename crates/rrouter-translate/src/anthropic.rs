//! Anthropic Messages API: JSON bodies and event-framed SSE.
//!
//! # Account refusals
//!
//! A refusal is not always about the request. The Claude subscription gate answers a
//! request that does not look like Claude Code with
//! `429 {"type":"error","error":{"type":"rate_limit_error","message":"Error"}}` — an
//! entitlement verdict wearing a throttle's status — and an account or organization the
//! provider disabled answers `400`/`403` with its own wording. [`refusal`] separates those
//! from ordinary failures and from real throttles (which carry `anthropic-ratelimit-*`
//! headers), so the gateway can retire a credential that can never succeed instead of
//! cooling it down and re-probing it forever.

use serde_json::{json, Map, Value};

use rrouter_core::sse::{render_anthropic, SseCodec, SseFrame};
use rrouter_core::{
    ChatRequest, ChatResponse, Dialect, Error, FinishReason, Message, Part, Refusal, Result, Role,
    StreamEvent, ToolChoice, ToolDef, Usage,
};

use crate::{
    capture_extra, finish_from_anthropic, merge_extra, opt_f64, opt_u32, parse_err, parse_stop,
    req_str, req_str_field, to_json, tool_schema, usage_from_anthropic, usage_to_anthropic,
    WireProfile,
};

const D: Dialect = Dialect::AnthropicMessages;

const KNOWN_KEYS: &[&str] = &[
    "model",
    "messages",
    "system",
    "tools",
    "tool_choice",
    "max_tokens",
    "temperature",
    "top_p",
    "stop_sequences",
    "stream",
    "thinking",
    "output_config",
];

// ---------------------------------------------------------------------------
// Request in
// ---------------------------------------------------------------------------

pub fn parse_request(body: &Value) -> Result<ChatRequest> {
    let model = req_str_field(body, "model", D)?;
    let mut req = ChatRequest::new(model);
    req.stream = body.get("stream").and_then(Value::as_bool).unwrap_or(false);
    req.max_tokens = opt_u32(body, "max_tokens");
    req.temperature = opt_f64(body, "temperature");
    req.top_p = opt_f64(body, "top_p");
    req.stop = body
        .get("stop_sequences")
        .map(parse_stop)
        .unwrap_or_default();
    req.system = body
        .get("system")
        .map(system_text)
        .filter(|s| !s.is_empty());
    req.messages = parse_messages(body)?;
    let (tools, raw_tools) = parse_tools(body);
    req.tools = tools;
    req.raw_tools = raw_tools.map(|t| (D, t));
    req.tool_choice = parse_tool_choice(body);
    req.reasoning = super::reasoning::parse_anthropic(body)?;

    capture_extra(body, KNOWN_KEYS, D, &mut req)?;
    Ok(req)
}

fn system_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(items) => {
            let mut out = String::new();
            for item in items {
                if let Some(t) = req_str(item, "text") {
                    if !out.is_empty() {
                        out.push_str("\n\n");
                    }
                    out.push_str(t);
                }
            }
            out
        }
        _ => String::new(),
    }
}

fn parse_messages(body: &Value) -> Result<Vec<Message>> {
    let Some(raw) = body.get("messages").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    let mut out: Vec<Message> = Vec::with_capacity(raw.len());
    for (i, msg) in raw.iter().enumerate() {
        let role = match req_str(msg, "role") {
            Some("user") => Role::User,
            Some("assistant") => Role::Assistant,
            Some(other) => return Err(parse_err(D, &format!("unknown role `{other}`"))),
            None => return Err(parse_err(D, &format!("messages[{i}] has no role"))),
        };
        let parts = match msg.get("content") {
            Some(Value::String(s)) => vec![Part::Text(s.clone())],
            Some(Value::Array(items)) => {
                let mut parts = Vec::with_capacity(items.len());
                for item in items {
                    parts.push(parse_block(item));
                }
                parts
            }
            _ => Vec::new(),
        };
        out.push(Message { role, parts });
    }
    Ok(out)
}

fn parse_block(block: &Value) -> Part {
    match req_str(block, "type") {
        Some("text") => Part::Text(req_str(block, "text").unwrap_or_default().to_owned()),
        Some("image") => {
            let source = block.get("source");
            let media_type = source
                .and_then(|s| req_str(s, "media_type"))
                .map(str::to_owned);
            let url = match source {
                Some(s) if req_str(s, "type") == Some("base64") => {
                    let data = req_str(s, "data").unwrap_or_default();
                    match &media_type {
                        Some(mt) => format!("data:{mt};base64,{data}"),
                        None => format!("data:;base64,{data}"),
                    }
                }
                Some(s) => req_str(s, "url").unwrap_or_default().to_owned(),
                None => String::new(),
            };
            Part::Image { url, media_type }
        }
        Some("tool_use") => Part::ToolUse {
            id: req_str(block, "id").unwrap_or_default().to_owned(),
            name: req_str(block, "name").unwrap_or_default().to_owned(),
            input: block.get("input").cloned().unwrap_or_else(|| json!({})),
        },
        Some("tool_result") => Part::ToolResult {
            tool_use_id: req_str(block, "tool_use_id").unwrap_or_default().to_owned(),
            content: block.get("content").map(result_text).unwrap_or_default(),
            is_error: block
                .get("is_error")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        },
        Some("thinking") => Part::Reasoning {
            text: req_str(block, "thinking").unwrap_or_default().to_owned(),
            signature: req_str(block, "signature").map(str::to_owned),
        },
        Some("redacted_thinking") => Part::Reasoning {
            text: String::new(),
            signature: req_str(block, "data").map(str::to_owned),
        },
        _ => Part::Opaque(block.clone()),
    }
}

fn result_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .filter_map(|i| req_str(i, "text").or_else(|| req_str(i, "content")))
            .collect::<Vec<_>>()
            .join(""),
        other => other.to_string(),
    }
}

/// Custom tools carry `input_schema` and no `type`; server tools (`web_search`,
/// `computer`, `bash`, `text_editor`) are identified by their versioned `type`.
/// Server tools have no canonical form, so they are kept verbatim and only sent
/// back to Anthropic — treating one as a function tool would strip the feature
/// and leave an empty schema behind.
fn parse_tools(body: &Value) -> (Vec<ToolDef>, Option<Vec<Value>>) {
    let Some(items) = body.get("tools").and_then(Value::as_array) else {
        return (Vec::new(), None);
    };
    let mut out = Vec::with_capacity(items.len());
    let mut raw = Vec::new();
    for item in items {
        if item.get("type").is_some() {
            raw.push(item.clone());
            continue;
        }
        let Some(name) = req_str(item, "name") else {
            continue;
        };
        out.push(ToolDef {
            name: name.to_owned(),
            description: req_str(item, "description").map(str::to_owned),
            parameters: item
                .get("input_schema")
                .map(tool_schema)
                .unwrap_or_else(|| tool_schema(&Value::Null)),
            // Anthropic has no strict flag; schemas are advisory only.
            strict: false,
        });
    }
    (out, (!raw.is_empty()).then_some(raw))
}

fn parse_tool_choice(body: &Value) -> Option<ToolChoice> {
    match body.get("tool_choice")? {
        Value::String(s) => Some(match s.as_str() {
            "auto" => ToolChoice::Auto,
            "any" => ToolChoice::Required,
            "none" => ToolChoice::None,
            other => ToolChoice::Tool(other.to_owned()),
        }),
        v => {
            let kind = req_str(v, "type")?;
            match kind {
                "auto" => Some(ToolChoice::Auto),
                "any" => Some(ToolChoice::Required),
                "none" => Some(ToolChoice::None),
                "tool" => req_str(v, "name").map(|n| ToolChoice::Tool(n.to_owned())),
                _ => None,
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Request out
// ---------------------------------------------------------------------------

pub fn build_request(req: &ChatRequest, stream: bool, wire: &WireProfile) -> Result<Value> {
    let mut messages: Vec<Value> = Vec::with_capacity(req.messages.len());
    for msg in &req.messages {
        let role = match msg.role {
            Role::Assistant => "assistant",
            _ => "user",
        };
        messages.push(json!({"role": role, "content": blocks_out(&msg.parts)?}));
    }

    let mut body = Map::new();
    body.insert("model".into(), Value::from(req.model.clone()));
    body.insert("messages".into(), Value::from(messages));
    // `max_tokens` is mandatory here. An omitted client value used to become a hardcoded
    // 4096, silently truncating every translated request that named no cap; the provider's
    // configured default (32k for Claude) now applies first.
    let mut max_tokens = wire.max_tokens(req.max_tokens);
    let thinking_enabled = req
        .reasoning
        .as_ref()
        .is_some_and(super::reasoning::is_enabled);
    if let Some(reasoning) = &req.reasoning {
        if !super::reasoning::is_enabled(reasoning) {
            body.insert("thinking".into(), json!({"type": "disabled"}));
        } else if let Some((budget, required)) =
            super::reasoning::anthropic_budget(reasoning, Some(max_tokens))
        {
            max_tokens = max_tokens.max(required);
            body.insert(
                "thinking".into(),
                json!({"type": "enabled", "budget_tokens": budget}),
            );
        }
    }
    body.insert("max_tokens".into(), Value::from(max_tokens));
    body.insert("stream".into(), Value::Bool(stream));
    if let Some(system) = &req.system {
        if !system.is_empty() {
            body.insert("system".into(), Value::from(system.clone()));
        }
    }
    let mut tools: Vec<Value> = req
        .tools
        .iter()
        .map(|t| {
            let mut m = Map::new();
            m.insert("name".into(), Value::from(t.name.clone()));
            if let Some(d) = &t.description {
                m.insert("description".into(), Value::from(d.clone()));
            }
            m.insert("input_schema".into(), t.parameters.clone());
            Value::Object(m)
        })
        .collect();
    if let Some((src, raw)) = &req.raw_tools {
        if *src == D {
            tools.extend(raw.iter().cloned());
        }
    }
    if !tools.is_empty() {
        body.insert("tools".into(), Value::from(tools));
    }
    if let Some(choice) = &req.tool_choice {
        body.insert("tool_choice".into(), tool_choice_out(choice));
    }
    // Anthropic rejects sampling controls while extended thinking is enabled (temperature
    // must be 1, and top_p is unsupported). Omitting both is the only valid request.
    if !thinking_enabled {
        if let Some(t) = req.temperature {
            body.insert("temperature".into(), Value::from(t));
        }
        if let Some(p) = req.top_p {
            body.insert("top_p".into(), Value::from(p));
        }
    }
    if !req.stop.is_empty() {
        body.insert("stop_sequences".into(), Value::from(req.stop.clone()));
    }
    let mut value = Value::Object(body);
    merge_extra(&mut value, req, D);
    Ok(value)
}

fn tool_choice_out(choice: &ToolChoice) -> Value {
    match choice {
        ToolChoice::Tool(name) => json!({"type": "tool", "name": name}),
        ToolChoice::None => json!({"type": "none"}),
        ToolChoice::Required => json!({"type": "any"}),
        ToolChoice::Auto => json!({"type": "auto"}),
    }
}

/// Anthropic splits content into typed blocks; `input` is a JSON *object*.
fn blocks_out(parts: &[Part]) -> Result<Value> {
    let mut out: Vec<Value> = Vec::with_capacity(parts.len());
    for part in parts {
        match part {
            Part::Text(t) => {
                if !t.is_empty() {
                    out.push(json!({"type": "text", "text": t}));
                }
            }
            Part::Image { url, media_type } => out.push(image_block(url, media_type)),
            Part::ToolUse { id, name, input } => out.push(json!({
                "type": "tool_use",
                "id": id,
                "name": name,
                "input": if input.is_object() { input.clone() } else { json!({}) },
            })),
            Part::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => {
                let mut block = Map::new();
                block.insert("type".into(), Value::from("tool_result"));
                block.insert("tool_use_id".into(), Value::from(tool_use_id.clone()));
                block.insert("content".into(), Value::from(content.clone()));
                if *is_error {
                    block.insert("is_error".into(), Value::Bool(true));
                }
                out.push(Value::Object(block));
            }
            Part::Reasoning { text, signature } => {
                if text.is_empty() {
                    if let Some(sig) = signature {
                        out.push(json!({"type": "redacted_thinking", "data": sig}));
                    }
                } else {
                    let mut block = Map::new();
                    block.insert("type".into(), Value::from("thinking"));
                    block.insert("thinking".into(), Value::from(text.clone()));
                    if let Some(sig) = signature {
                        block.insert("signature".into(), Value::from(sig.clone()));
                    }
                    out.push(Value::Object(block));
                }
            }
            Part::Opaque(v) => {
                let _ = to_json(v, D)?;
                out.push(v.clone());
            }
        }
    }
    Ok(Value::Array(out))
}

fn image_block(url: &str, media_type: &Option<String>) -> Value {
    if let Some(rest) = url.strip_prefix("data:") {
        if let Some((meta, data)) = rest.split_once(',') {
            let mt = meta
                .split(';')
                .next()
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .or_else(|| media_type.clone());
            return json!({
                "type": "image",
                "source": {"type": "base64", "media_type": mt.unwrap_or_else(|| "image/png".into()), "data": data},
            });
        }
    }
    json!({"type": "image", "source": {"type": "url", "url": url}})
}

// ---------------------------------------------------------------------------
// Response in
// ---------------------------------------------------------------------------

pub fn parse_response(body: &Value) -> Result<ChatResponse> {
    let id = req_str(body, "id").unwrap_or_default().to_owned();
    let model = req_str(body, "model").unwrap_or_default().to_owned();
    let blocks = body
        .get("content")
        .and_then(Value::as_array)
        .ok_or_else(|| parse_err(D, "response has no content array"))?;
    let parts = blocks.iter().map(parse_block).collect();

    Ok(ChatResponse {
        id,
        model,
        parts,
        finish_reason: finish_from_anthropic(req_str(body, "stop_reason")),
        usage: usage_from_anthropic(body.get("usage")),
    })
}

// ---------------------------------------------------------------------------
// Response out
// ---------------------------------------------------------------------------

pub fn build_response(resp: &ChatResponse) -> Result<Value> {
    Ok(json!({
        "id": resp.id,
        "type": "message",
        "role": "assistant",
        "model": resp.model,
        "content": blocks_out(&resp.parts)?,
        "stop_reason": resp.finish_reason.anthropic(),
        "stop_sequence": Value::Null,
        "usage": usage_to_anthropic(resp.usage),
    }))
}

// ---------------------------------------------------------------------------
// Account refusals
// ---------------------------------------------------------------------------

/// Provider label used in the translated error text and in a refusal's own sentence.
///
/// The dialect, not the credential: the gateway overrides it with the registry id when it
/// has one, and a refusal raised deep inside the stream decoder does not.
const PROVIDER: &str = "anthropic";

/// Wording that means the provider disabled the *account* or its *organization*.
///
/// Every phrase names one or the other: matching a bare `disabled` would retire a healthy
/// account on a coincidental word, and a false permanent verdict is far worse than a missed
/// one (the account keeps working under the status-quo path). Case-insensitive.
const TERMINATED_PHRASES: [&str; 5] = [
    "your account has been disabled",
    "account has been deactivated",
    "organization has been disabled",
    "organization has been suspended",
    "organization has been deactivated",
];

/// Wording that means the credential is real but not entitled to the request.
///
/// These never clear by waiting — the first is the subscription gate, the second is a
/// billing-routing refusal — so treating them as throttles is what leaves a dead account
/// cooling down and being re-probed forever.
const ENTITLEMENT_PHRASES: [&str; 2] = [
    "only authorized for use with claude code",
    "third-party apps now draw from your extra usage",
];

/// The message of an organization-level `403`. Gated on the status because the phrase is
/// short enough to appear inside an unrelated sentence.
const ENTITLEMENT_FORBIDDEN: &str = "request not allowed";

/// Wording that means the provider enforces a newer client than this build declares. A
/// configuration defect on our side, never a fact about the account, so it must not be
/// cooled down like a throttle nor retire anything.
const OUTDATED_PHRASES: [&str; 2] = [
    "or newer is required",
    "please upgrade to the latest version",
];

/// Error type and message of the subscription gate's refusal.
///
/// The gate answers `429 {"type":"error","error":{"type":"rate_limit_error","message":
/// "Error"}}`: an entitlement verdict wearing a throttle's status. The message is the whole
/// signature — a real `429` names the limit it exceeded — so a `429` is only ever read as
/// this class when the body says exactly this, and never when the answer carried the
/// provider's own rate-limit headers.
const GATE_TYPE: &str = "rate_limit_error";
const GATE_MESSAGE: &str = "Error";

/// Classify an upstream answer as a refusal of the *account* rather than of the request.
///
/// `http_status` is the status of the enclosing HTTP answer; `None` when the body arrived
/// inside a `200` (`event: error` on a stream, where the body's own `type` is the only
/// signal available). `throttle_headers` is `true` when that answer carried the provider's
/// `anthropic-ratelimit-*` headers.
///
/// `None` is the status-quo path and the safe default: nothing is classified unless the
/// answer carries one of the taxonomy's signals. Four guards keep it honest:
///
/// * a `5xx` — `529 overloaded` included — is transient by definition, so no wording in
///   such a body is evidence about the account;
/// * a `429` **with** rate-limit headers is a real throttle and is never classified,
///   whatever its message;
/// * a `429` without them is only classified when the body is the gate's exact signature,
///   because an ordinary `429` from an unknown proxy must stay a rate limit;
/// * `401` matches nothing on purpose: a revoked token is recoverable by a refresh, not a
///   ban, and terminalising it would retire an account that a refresh can fix.
pub fn refusal(http_status: Option<u16>, body: &[u8], throttle_headers: bool) -> Option<Refusal> {
    let value: Value = serde_json::from_slice(body).ok()?;
    refusal_in(http_status, &value, throttle_headers)
}

/// The body's own error message, verbatim, for an operator-facing detail line.
///
/// Empty when the body is not JSON or carries no message: the refusal classification is
/// what the caller acts on, and the detail is only ever appositional.
pub fn refusal_detail(body: &[u8]) -> String {
    serde_json::from_slice::<Value>(body)
        .ok()
        .map(|value| error_message(&value).to_string())
        .unwrap_or_default()
}

/// [`refusal`] over an already-parsed error, for the stream decoder, which has the error
/// object instead of the raw bytes.
fn refusal_in(http_status: Option<u16>, value: &Value, throttle_headers: bool) -> Option<Refusal> {
    // The provider's own rate-limit headers are sent on every real throttle and on none of
    // the gate's answers: with them present the answer is a rate limit, full stop.
    if throttle_headers {
        return None;
    }
    let error = value.get("error").unwrap_or(value);
    let kind = error
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    // An overloaded provider is the one 5xx-adjacent answer that can arrive with a `200`
    // envelope: it says nothing about entitlement.
    if kind.eq_ignore_ascii_case("overloaded_error") {
        return None;
    }
    let message = error_message(value);
    let lower = message.to_ascii_lowercase();
    match http_status {
        // The one refusal that wears a throttle's status, and only in its exact shape.
        Some(429) => {
            let gate = kind.eq_ignore_ascii_case(GATE_TYPE)
                && message.trim().eq_ignore_ascii_case(GATE_MESSAGE);
            return gate.then_some(Refusal::Entitlement);
        }
        // Transient by definition: a provider retry or an exhausted quota window can echo
        // any wording at all, so no phrase in such a body is evidence about the account.
        Some(500..=599) => return None,
        _ => {}
    }
    if TERMINATED_PHRASES
        .iter()
        .any(|phrase| lower.contains(phrase))
    {
        return Some(Refusal::Terminated);
    }
    if ENTITLEMENT_PHRASES
        .iter()
        .any(|phrase| lower.contains(phrase))
    {
        return Some(Refusal::Entitlement);
    }
    if http_status == Some(403) && lower.contains(ENTITLEMENT_FORBIDDEN) {
        return Some(Refusal::Entitlement);
    }
    if OUTDATED_PHRASES.iter().any(|phrase| lower.contains(phrase)) {
        return Some(Refusal::ClientOutdated);
    }
    None
}

/// The `message` an error object carries, or the bare string body itself.
fn error_message(value: &Value) -> &str {
    let error = value.get("error").unwrap_or(value);
    match error {
        Value::String(text) => text,
        _ => error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    }
}

/// A streamed `event: error` frame as the error the gateway sees.
///
/// The frame is a refusal when the taxonomy recognises it, so an account the provider
/// refuses mid-stream is retired exactly like one refused before the stream opened.
/// Otherwise the provider's wording becomes a translation failure: before this the frame
/// was dropped and the client got a truncated response recorded as a success.
fn error_from_value(error: &Value) -> Error {
    let detail = match error_message(error) {
        "" => "upstream error",
        text => text,
    };
    if let Some(refusal) = refusal_in(None, error, false) {
        return Error::account_refused(PROVIDER, refusal, detail);
    }
    Error::Translation(format!("{PROVIDER}: {detail}"))
}

// ---------------------------------------------------------------------------
// Streaming in
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct Block {
    /// Canonical tool index (counts `tool_use` blocks only).
    index: u32,
    kind: BlockKind,
}

#[derive(Debug, PartialEq, Clone, Copy)]
enum BlockKind {
    Text,
    Thinking,
    ToolUse,
    Other,
}

#[derive(Debug, Default)]
pub struct AnthropicDecoder {
    codec: SseCodec,
    model: String,
    sent_start: bool,
    done: bool,
    message_id: String,
    usage: Usage,
    finish: Option<FinishReason>,
    blocks: Vec<Option<Block>>,
    tool_count: u32,
}

impl AnthropicDecoder {
    pub fn new(model: &str) -> Self {
        Self {
            model: model.to_owned(),
            ..Self::default()
        }
    }

    pub fn push(&mut self, chunk: &[u8], out: &mut Vec<StreamEvent>) -> Result<()> {
        self.codec.push(chunk);
        while let Some(frame) = self.codec.next_frame() {
            self.frame(&frame, out)?;
        }
        Ok(())
    }

    pub fn finish(&mut self, out: &mut Vec<StreamEvent>) -> Result<()> {
        while let Some(frame) = self.codec.finish() {
            self.frame(&frame, out)?;
        }
        self.emit_done(out);
        Ok(())
    }

    fn frame(&mut self, frame: &SseFrame, out: &mut Vec<StreamEvent>) -> Result<()> {
        if frame.is_done() {
            return Ok(());
        }
        let Ok(v) = serde_json::from_str::<Value>(&frame.data) else {
            return Ok(());
        };
        // `event: error` (or an error object without the event name) is the provider
        // refusing after the `200` was already sent — the only place a stream can carry a
        // verdict, and the only chance to act on a refusal of the account.
        if let Some(error) = v.get("error") {
            return Err(error_from_value(error));
        }
        let event = frame
            .event
            .as_deref()
            .or_else(|| req_str(&v, "type"))
            .unwrap_or_default();
        match event {
            "message_start" => {
                let msg = v.get("message").unwrap_or(&Value::Null);
                self.message_id = req_str(msg, "id").unwrap_or_default().to_owned();
                let model = req_str(msg, "model")
                    .map(str::to_owned)
                    .unwrap_or_else(|| self.model.clone());
                self.usage = usage_from_anthropic(msg.get("usage"));
                self.emit_start(out, model);
            }
            "content_block_start" => {
                let slot = opt_u32(&v, "index").unwrap_or(0) as usize;
                let block = v.get("content_block").unwrap_or(&Value::Null);
                let (kind, entry) = match req_str(block, "type") {
                    Some("tool_use") => {
                        let index = self.tool_count;
                        self.tool_count += 1;
                        let id = req_str(block, "id").unwrap_or_default().to_owned();
                        let name = req_str(block, "name").unwrap_or_default().to_owned();
                        out.push(StreamEvent::ToolCallStart {
                            index,
                            id: id.clone(),
                            name: name.clone(),
                        });
                        (
                            BlockKind::ToolUse,
                            Block {
                                index,
                                kind: BlockKind::ToolUse,
                            },
                        )
                    }
                    Some("thinking") => (
                        BlockKind::Thinking,
                        Block {
                            index: 0,
                            kind: BlockKind::Thinking,
                        },
                    ),
                    Some("text") => {
                        if let Some(t) = req_str(block, "text") {
                            if !t.is_empty() {
                                out.push(StreamEvent::TextDelta(t.to_owned()));
                            }
                        }
                        (
                            BlockKind::Text,
                            Block {
                                index: 0,
                                kind: BlockKind::Text,
                            },
                        )
                    }
                    _ => (
                        BlockKind::Other,
                        Block {
                            index: 0,
                            kind: BlockKind::Other,
                        },
                    ),
                };
                debug_assert!(kind == entry.kind);
                if self.blocks.len() <= slot {
                    self.blocks.resize_with(slot + 1, || None);
                }
                self.blocks[slot] = Some(entry);
            }
            "content_block_delta" => {
                let slot = opt_u32(&v, "index").unwrap_or(0) as usize;
                let Some(Some(block)) = self.blocks.get(slot) else {
                    return Ok(());
                };
                let delta = v.get("delta").unwrap_or(&Value::Null);
                match req_str(delta, "type") {
                    Some("text_delta") => {
                        if let Some(t) = req_str(delta, "text") {
                            out.push(StreamEvent::TextDelta(t.to_owned()));
                        }
                    }
                    Some("thinking_delta") => {
                        if let Some(t) = req_str(delta, "thinking") {
                            out.push(StreamEvent::ReasoningDelta(t.to_owned()));
                        }
                    }
                    Some("input_json_delta") if block.kind == BlockKind::ToolUse => {
                        out.push(StreamEvent::ToolCallArgsDelta {
                            index: block.index,
                            partial_json: req_str(delta, "partial_json")
                                .unwrap_or_default()
                                .to_owned(),
                        });
                    }
                    _ => {}
                }
            }
            "content_block_stop" => {
                let slot = opt_u32(&v, "index").unwrap_or(0) as usize;
                if let Some(entry) = self.blocks.get_mut(slot) {
                    *entry = None;
                }
            }
            "message_delta" => {
                if let Some(d) = v.get("delta") {
                    if let Some(reason) = req_str(d, "stop_reason") {
                        self.finish = Some(finish_from_anthropic(Some(reason)));
                    }
                }
                if let Some(u) = v.get("usage").filter(|u| u.is_object()) {
                    let u = usage_from_anthropic(Some(u));
                    self.usage.output_tokens = u.output_tokens;
                    if u.input_tokens > 0 {
                        self.usage.input_tokens = u.input_tokens;
                    }
                    if u.cache_read_tokens > 0 {
                        self.usage.cache_read_tokens = u.cache_read_tokens;
                    }
                    if u.cache_write_tokens > 0 {
                        self.usage.cache_write_tokens = u.cache_write_tokens;
                    }
                }
            }
            "message_stop" => self.emit_done(out),
            _ => {}
        }
        Ok(())
    }

    fn emit_start(&mut self, out: &mut Vec<StreamEvent>, model: String) {
        if self.sent_start {
            return;
        }
        self.sent_start = true;
        out.push(StreamEvent::Start {
            id: self.message_id.clone(),
            model,
        });
    }

    fn emit_done(&mut self, out: &mut Vec<StreamEvent>) {
        if self.done {
            return;
        }
        self.emit_start(out, self.model.clone());
        self.done = true;
        let reason = self.finish.unwrap_or(if self.tool_count > 0 {
            FinishReason::ToolCalls
        } else {
            FinishReason::Stop
        });
        out.push(StreamEvent::Done {
            reason,
            usage: Some(self.usage),
        });
    }
}

// ---------------------------------------------------------------------------
// Streaming out
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
pub struct AnthropicEncoder {
    model: String,
    id: String,
    started: bool,
    done: bool,
    /// Wire content-block index; it differs from the canonical tool index
    /// because text/thinking blocks occupy slots too.
    next_index: u32,
    /// Currently open wire block, if any.
    open: Option<(u32, BlockKind)>,
    /// Canonical tool index → wire block index.
    tool_blocks: Vec<u32>,
}

impl AnthropicEncoder {
    pub fn new(model: &str) -> Self {
        Self {
            model: model.to_owned(),
            ..Self::default()
        }
    }

    pub fn encode(&mut self, ev: &StreamEvent) -> bytes::Bytes {
        if self.done {
            return bytes::Bytes::new();
        }
        let mut out = String::new();
        if !self.started {
            self.write_start(&mut out);
        }
        match ev {
            StreamEvent::Start { id, model } => {
                if self.id != *id || self.model != *model {
                    self.id = id.clone();
                    self.model = model.clone();
                    out.clear();
                    self.write_start(&mut out);
                }
            }
            StreamEvent::TextDelta(t) => {
                let idx = self.ensure_block(BlockKind::Text, &mut out);
                let data = json!({
                    "type": "content_block_delta",
                    "index": idx,
                    "delta": {"type": "text_delta", "text": t},
                });
                out.push_str(&render_anthropic("content_block_delta", &data.to_string()));
            }
            StreamEvent::ReasoningDelta(t) => {
                let idx = self.ensure_block(BlockKind::Thinking, &mut out);
                let data = json!({
                    "type": "content_block_delta",
                    "index": idx,
                    "delta": {"type": "thinking_delta", "thinking": t},
                });
                out.push_str(&render_anthropic("content_block_delta", &data.to_string()));
            }
            StreamEvent::ToolCallStart { index, id, name } => {
                if self.tool_block(*index).is_none() {
                    self.write_tool_start(*index, id, name, &mut out);
                }
            }
            StreamEvent::ToolCallArgsDelta {
                index,
                partial_json,
            } => {
                let idx = self
                    .tool_block(*index)
                    .unwrap_or_else(|| self.write_tool_start(*index, "", "", &mut out));
                let data = json!({
                    "type": "content_block_delta",
                    "index": idx,
                    "delta": {"type": "input_json_delta", "partial_json": partial_json},
                });
                out.push_str(&render_anthropic("content_block_delta", &data.to_string()));
            }
            // Anthropic has no envelope for a Responses-only opaque item.
            StreamEvent::OpaqueItem(_) => {}
            StreamEvent::Done { reason, usage } => {
                self.close_all(&mut out);
                let mut delta = json!({
                    "type": "message_delta",
                    "delta": {"stop_reason": reason.anthropic(), "stop_sequence": Value::Null},
                });
                if let Some(u) = usage {
                    delta["usage"] = usage_to_anthropic(*u);
                }
                out.push_str(&render_anthropic("message_delta", &delta.to_string()));
                out.push_str(&render_anthropic(
                    "message_stop",
                    "{\"type\":\"message_stop\"}",
                ));
                self.done = true;
            }
        }
        bytes::Bytes::from(out)
    }

    /// Open (or reuse) the block for `kind`, closing whatever else is open.
    fn ensure_block(&mut self, kind: BlockKind, out: &mut String) -> u32 {
        if let Some((idx, k)) = self.open {
            if k == kind {
                return idx;
            }
        }
        self.close_open(out);
        let idx = self.next_index;
        self.next_index += 1;
        let block = match kind {
            BlockKind::Thinking => json!({"type": "thinking", "thinking": "", "signature": ""}),
            _ => json!({"type": "text", "text": ""}),
        };
        let data = json!({"type": "content_block_start", "index": idx, "content_block": block});
        out.push_str(&render_anthropic("content_block_start", &data.to_string()));
        self.open = Some((idx, kind));
        idx
    }

    fn tool_block(&self, tool_index: u32) -> Option<u32> {
        self.tool_blocks
            .get(tool_index as usize)
            .copied()
            .filter(|i| *i != u32::MAX)
    }

    fn write_tool_start(&mut self, tool_index: u32, id: &str, name: &str, out: &mut String) -> u32 {
        self.close_open(out);
        let idx = self.next_index;
        self.next_index += 1;
        let slot = tool_index as usize;
        if self.tool_blocks.len() <= slot {
            self.tool_blocks.resize(slot + 1, u32::MAX);
        }
        self.tool_blocks[slot] = idx;
        self.open = Some((idx, BlockKind::ToolUse));
        let data = json!({
            "type": "content_block_start",
            "index": idx,
            "content_block": {"type": "tool_use", "id": id, "name": name, "input": {}},
        });
        out.push_str(&render_anthropic("content_block_start", &data.to_string()));
        idx
    }

    fn close_open(&mut self, out: &mut String) {
        if let Some((idx, _)) = self.open.take() {
            let data = json!({"type": "content_block_stop", "index": idx});
            out.push_str(&render_anthropic("content_block_stop", &data.to_string()));
        }
    }

    fn close_all(&mut self, out: &mut String) {
        self.close_open(out);
    }

    fn write_start(&mut self, out: &mut String) {
        self.started = true;
        let data = json!({
            "type": "message_start",
            "message": {
                "id": self.id,
                "type": "message",
                "role": "assistant",
                "model": self.model,
                "content": [],
                "stop_reason": Value::Null,
                "stop_sequence": Value::Null,
                "usage": {"input_tokens": 0, "output_tokens": 0},
            },
        });
        out.push_str(&render_anthropic("message_start", &data.to_string()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::openai_chat::{build_response as build_oi, parse_request as parse_oi};
    use crate::{build_request, parse_request, StreamEncoder};

    #[test]
    fn anthropic_server_tools_are_passed_through_unchanged() {
        let body = json!({
            "model": "m",
            "max_tokens": 16,
            "messages": [{"role": "user", "content": "hi"}],
            "tools": [
                {"name": "lookup", "description": "d", "input_schema": {"type": "object"}},
                {"type": "web_search_20250305", "name": "web_search", "max_uses": 5},
            ],
        });
        let req = super::parse_request(&body).unwrap();
        assert_eq!(req.tools.len(), 1, "the server tool is not a function tool");
        assert_eq!(req.tools[0].name, "lookup");

        let same = super::build_request(&req, false, &WireProfile::default()).unwrap();
        let tools = same["tools"].as_array().expect("tools");
        assert_eq!(tools.len(), 2);
        assert_eq!(
            tools[1],
            json!({"type": "web_search_20250305", "name": "web_search", "max_uses": 5})
        );

        let chat = build_request(Dialect::OpenAiChat, &req, false).unwrap();
        assert!(
            chat.get("tools")
                .and_then(|t| t.as_array())
                .map(|t| t.len())
                == Some(1),
            "a server tool must not be smuggled into another dialect: {chat}"
        );
    }

    #[test]
    fn thinking_omits_anthropic_sampling_controls() {
        let body = json!({
            "model": "claude-sonnet-4",
            "max_tokens": 8192,
            "thinking": {"type": "enabled", "budget_tokens": 4096},
            "temperature": 0.2,
            "top_p": 0.9,
            "messages": [{"role": "user", "content": "hi"}],
        });
        let req = super::parse_request(&body).expect("request");
        let out = super::build_request(&req, false, &WireProfile::default()).expect("body");
        assert!(out.get("temperature").is_none(), "{out}");
        assert!(out.get("top_p").is_none(), "{out}");
        assert_eq!(out["thinking"]["type"], "enabled");
    }

    fn sse(event: &str, data: Value) -> String {
        render_anthropic(event, &data.to_string())
    }

    #[test]
    fn tool_use_blocks_parse_into_canonical_tool_use() {
        let body = json!({
            "model": "claude-sonnet-4",
            "max_tokens": 1024,
            "messages": [
                {"role": "user", "content": "weather?"},
                {"role": "assistant", "content": [
                    {"type": "text", "text": "checking"},
                    {"type": "tool_use", "id": "toolu_1", "name": "get_weather", "input": {"city": "Hanoi"}}
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "toolu_1", "content": "21C"}
                ]}
            ]
        });
        let req = parse_request(D, &body).unwrap();
        assert_eq!(req.max_tokens, Some(1024));
        assert_eq!(
            req.messages[1].parts[1],
            Part::ToolUse {
                id: "toolu_1".into(),
                name: "get_weather".into(),
                input: json!({"city": "Hanoi"})
            }
        );
        assert_eq!(
            req.messages[2].parts,
            vec![Part::ToolResult {
                tool_use_id: "toolu_1".into(),
                content: "21C".into(),
                is_error: false
            }]
        );
    }

    #[test]
    fn builder_emits_object_input_and_tool_result_blocks() {
        let mut req = rrouter_core::ChatRequest::new("claude-sonnet-4");
        req.system = Some("be brief".into());
        req.messages = vec![
            Message::text(Role::User, "go"),
            Message {
                role: Role::Assistant,
                parts: vec![Part::ToolUse {
                    id: "t1".into(),
                    name: "f".into(),
                    input: json!({"a": 1}),
                }],
            },
            Message {
                role: Role::Tool,
                parts: vec![Part::ToolResult {
                    tool_use_id: "t1".into(),
                    content: "done".into(),
                    is_error: true,
                }],
            },
        ];
        let body = build_request(D, &req, true).unwrap();
        assert_eq!(body["system"], "be brief");
        assert_eq!(body["stream"], true);
        assert!(body["messages"][1]["content"][0]["input"].is_object());
        let result = &body["messages"][2]["content"][0];
        assert_eq!(result["type"], "tool_result");
        assert_eq!(result["tool_use_id"], "t1");
        assert_eq!(result["is_error"], true);
        assert_eq!(
            body["messages"][2]["role"], "user",
            "tool results ride in a user turn"
        );
    }

    #[test]
    fn system_string_and_block_array_both_parse() {
        let plain = parse_request(
            D,
            &json!({"model": "m", "max_tokens": 1, "system": "s1", "messages": []}),
        )
        .unwrap();
        assert_eq!(plain.system.as_deref(), Some("s1"));
        let blocks = parse_request(
            D,
            &json!({"model": "m", "max_tokens": 1, "system": [{"type": "text", "text": "a"}, {"type": "text", "text": "b"}], "messages": []}),
        )
        .unwrap();
        assert_eq!(blocks.system.as_deref(), Some("a\n\nb"));
    }

    #[test]
    fn thinking_blocks_keep_signature() {
        let body = json!({
            "id": "msg_1",
            "model": "claude-sonnet-4",
            "content": [
                {"type": "thinking", "thinking": "step 1", "signature": "sig-abc"},
                {"type": "redacted_thinking", "data": "redacted-blob"},
                {"type": "text", "text": "answer"}
            ],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 9, "output_tokens": 4, "cache_read_input_tokens": 6, "cache_creation_input_tokens": 2}
        });
        let resp = parse_response(&body).unwrap();
        assert_eq!(
            resp.parts[0],
            Part::Reasoning {
                text: "step 1".into(),
                signature: Some("sig-abc".into())
            }
        );
        assert_eq!(
            resp.parts[1],
            Part::Reasoning {
                text: String::new(),
                signature: Some("redacted-blob".into())
            }
        );
        assert_eq!(
            resp.usage,
            Usage {
                input_tokens: 9,
                output_tokens: 4,
                cache_read_tokens: 6,
                cache_write_tokens: 2,
                reasoning_tokens: 0,
            }
        );
        // Signature survives the way back out.
        let out = build_response(&resp).unwrap();
        assert_eq!(out["content"][0]["signature"], "sig-abc");
        assert_eq!(out["content"][1]["type"], "redacted_thinking");
        assert_eq!(out["content"][1]["data"], "redacted-blob");
        assert_eq!(out["usage"]["cache_read_input_tokens"], 6);
        assert_eq!(out["stop_reason"], "end_turn");
    }

    #[test]
    fn stop_reason_mapping_matches_openai() {
        let cases = [
            ("end_turn", FinishReason::Stop),
            ("max_tokens", FinishReason::Length),
            ("tool_use", FinishReason::ToolCalls),
        ];
        for (wire, want) in cases {
            let body =
                json!({"id": "x", "model": "m", "content": [], "stop_reason": wire, "usage": {}});
            assert_eq!(
                parse_response(&body).unwrap().finish_reason,
                want,
                "anthropic {wire}"
            );
            let oi = json!({"choices": [{"message": {"role": "assistant", "content": ""}, "finish_reason": want.openai()}]});
            assert_eq!(
                crate::openai_chat::parse_response(&oi)
                    .unwrap()
                    .finish_reason,
                want,
                "openai {}",
                want.openai()
            );
        }
    }

    #[test]
    fn stream_decodes_text_blocks_tool_input_deltas_usage_and_done_once() {
        let wire = [
            sse("message_start", json!({"type": "message_start", "message": {"id": "msg_9", "model": "claude-sonnet-4", "usage": {"input_tokens": 12}}})),
            sse("content_block_start", json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}})),
            sse("content_block_delta", json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "Hel"}})),
            sse("content_block_delta", json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "lo"}})),
            sse("content_block_stop", json!({"type": "content_block_stop", "index": 0})),
            sse("content_block_start", json!({"type": "content_block_start", "index": 1, "content_block": {"type": "tool_use", "id": "toolu_2", "name": "f", "input": {}}})),
            sse("content_block_delta", json!({"type": "content_block_delta", "index": 1, "delta": {"type": "input_json_delta", "partial_json": "{\"a\":"}})),
            sse("content_block_delta", json!({"type": "content_block_delta", "index": 1, "delta": {"type": "input_json_delta", "partial_json": "1}"}})),
            sse("content_block_stop", json!({"type": "content_block_stop", "index": 1})),
            sse("message_delta", json!({"type": "message_delta", "delta": {"stop_reason": "tool_use"}, "usage": {"output_tokens": 15}})),
            sse("message_stop", json!({"type": "message_stop"})),
        ]
        .concat();

        let mut dec = AnthropicDecoder::new("fallback");
        let mut events = Vec::new();
        // 7-byte chunks: every frame boundary lands mid-JSON.
        for chunk in wire.as_bytes().chunks(7) {
            dec.push(chunk, &mut events).unwrap();
        }
        dec.finish(&mut events).unwrap();
        dec.finish(&mut events).unwrap();

        assert_eq!(
            events[0],
            StreamEvent::Start {
                id: "msg_9".into(),
                model: "claude-sonnet-4".into()
            }
        );
        let text: String = events
            .iter()
            .filter_map(|e| match e {
                StreamEvent::TextDelta(t) => Some(t.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(text, "Hello");
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, StreamEvent::ToolCallStart { .. }))
                .count(),
            1
        );
        let json: String = events
            .iter()
            .filter_map(|e| match e {
                StreamEvent::ToolCallArgsDelta { partial_json, .. } => Some(partial_json.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(json, "{\"a\":1}");
        let done: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, StreamEvent::Done { .. }))
            .collect();
        assert_eq!(done.len(), 1);
        assert_eq!(
            *done[0],
            StreamEvent::Done {
                reason: FinishReason::ToolCalls,
                usage: Some(Usage {
                    input_tokens: 12,
                    output_tokens: 15,
                    cache_read_tokens: 0,
                    cache_write_tokens: 0,
                    reasoning_tokens: 0,
                }),
            }
        );
    }

    #[test]
    fn decoder_finish_emits_done_when_upstream_closes_early() {
        let mut dec = AnthropicDecoder::new("m");
        let mut events = Vec::new();
        dec.push(
            sse(
                "message_start",
                json!({"message": {"id": "x", "model": "m"}}),
            )
            .as_bytes(),
            &mut events,
        )
        .unwrap();
        dec.finish(&mut events).unwrap();
        assert_eq!(
            events.last(),
            Some(&StreamEvent::Done {
                reason: FinishReason::Stop,
                usage: Some(Usage::default())
            })
        );
    }

    #[test]
    fn encoder_frames_blocks_deltas_and_stop_reason() {
        let mut enc = StreamEncoder::new(D, "claude-sonnet-4");
        let mut wire = String::new();
        for ev in [
            StreamEvent::Start {
                id: "msg_1".into(),
                model: "claude-sonnet-4".into(),
            },
            StreamEvent::ReasoningDelta("think".into()),
            StreamEvent::TextDelta("hi".into()),
            StreamEvent::ToolCallStart {
                index: 0,
                id: "t0".into(),
                name: "f".into(),
            },
            StreamEvent::ToolCallArgsDelta {
                index: 0,
                partial_json: "{\"k\":".into(),
            },
            StreamEvent::ToolCallArgsDelta {
                index: 0,
                partial_json: "2}".into(),
            },
            StreamEvent::Done {
                reason: FinishReason::Length,
                usage: Some(Usage {
                    input_tokens: 3,
                    output_tokens: 4,
                    cache_read_tokens: 0,
                    cache_write_tokens: 0,
                    reasoning_tokens: 0,
                }),
            },
        ] {
            wire.push_str(std::str::from_utf8(&enc.encode(&ev).unwrap()).unwrap());
        }
        assert!(wire.contains("event: message_start"));
        assert!(wire.contains("\"thinking_delta\""));
        assert!(wire.contains("\"text_delta\""));
        assert_eq!(
            wire.matches("event: content_block_start").count(),
            3,
            "thinking, text, tool_use"
        );
        assert_eq!(wire.matches("event: content_block_stop").count(), 3);
        assert!(wire.contains("\"partial_json\":\"{\\\"k\\\":\""));
        assert!(wire.contains("\"stop_reason\":\"max_tokens\""));
        assert_eq!(wire.matches("event: message_stop").count(), 1);
        assert!(wire
            .trim_end()
            .ends_with("data: {\"type\":\"message_stop\"}"));
    }

    #[test]
    fn encoder_output_redecodes_into_the_same_events() {
        let events = vec![
            StreamEvent::Start {
                id: "msg_7".into(),
                model: "claude-sonnet-4".into(),
            },
            StreamEvent::TextDelta("abc".into()),
            StreamEvent::ToolCallStart {
                index: 0,
                id: "t0".into(),
                name: "f".into(),
            },
            StreamEvent::ToolCallArgsDelta {
                index: 0,
                partial_json: "{}".into(),
            },
            StreamEvent::Done {
                reason: FinishReason::ToolCalls,
                usage: Some(Usage::default()),
            },
        ];
        let mut enc = StreamEncoder::new(D, "claude-sonnet-4");
        let wire: Vec<u8> = events
            .iter()
            .flat_map(|e| enc.encode(e).unwrap().to_vec())
            .collect();
        let mut dec = AnthropicDecoder::new("claude-sonnet-4");
        let mut got = Vec::new();
        dec.push(&wire, &mut got).unwrap();
        dec.finish(&mut got).unwrap();
        assert_eq!(got, events);
    }

    // Full cross-dialect round trip: [OI] request → Anthropic body → Anthropic
    // response → [OI] response, preserving tool names/args/ids and finish reason.
    #[test]
    fn full_round_trip_openai_to_anthropic_and_back() {
        let client = json!({
            "model": "claude-sonnet-4",
            "messages": [
                {"role": "system", "content": "sys"},
                {"role": "user", "content": "weather in Hanoi?"},
                {"role": "assistant", "content": null, "tool_calls": [
                    {"id": "call_1", "type": "function",
                     "function": {"name": "get_weather", "arguments": "{\"city\":\"Hanoi\",\"unit\":\"c\"}"}}
                ]},
                {"role": "tool", "tool_call_id": "call_1", "content": "21C"}
            ],
            "tools": [{"type": "function", "function": {"name": "get_weather", "parameters": {"type": "object", "properties": {"city": {"type": "string"}}}}}]
        });
        let canonical = parse_oi(&client).unwrap();
        let anthropic_body = build_request(Dialect::AnthropicMessages, &canonical, false).unwrap();
        assert_eq!(anthropic_body["system"], "sys");
        assert_eq!(anthropic_body["tools"][0]["name"], "get_weather");
        assert!(
            anthropic_body["tools"][0].get("function").is_none(),
            "anthropic tools are flat"
        );

        let upstream = json!({
            "id": "msg_rt",
            "model": "claude-sonnet-4",
            "content": [
                {"type": "text", "text": "calling tool"},
                {"type": "tool_use", "id": "call_2", "name": "get_weather", "input": {"city": "Hue"}}
            ],
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 40, "output_tokens": 12}
        });
        let resp = parse_response(&upstream).unwrap();
        let oi_out = build_oi(&resp).unwrap();
        let call = &oi_out["choices"][0]["message"]["tool_calls"][0];
        assert_eq!(call["id"], "call_2");
        assert_eq!(call["function"]["name"], "get_weather");
        assert_eq!(
            call["function"]["arguments"].as_str().unwrap(),
            "{\"city\":\"Hue\"}"
        );
        assert_eq!(oi_out["choices"][0]["finish_reason"], "tool_calls");
        assert_eq!(oi_out["usage"]["prompt_tokens"], 40);
        assert_eq!(oi_out["usage"]["completion_tokens"], 12);

        // Re-entering through the client dialect must preserve identity.
        let reparsed = crate::openai_chat::parse_response(&oi_out).unwrap();
        assert_eq!(reparsed.id, "msg_rt");
        assert_eq!(reparsed.parts, resp.parts);
        assert_eq!(reparsed.finish_reason, FinishReason::ToolCalls);
        assert_eq!(reparsed.usage, resp.usage);
    }

    #[test]
    fn image_base64_maps_to_data_url_and_back() {
        let body = json!({
            "model": "",
            "max_tokens": 8,
            "messages": [{"role": "user", "content": [
                {"type": "text", "text": "what is this"},
                {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "QUJD"}}
            ]}]
        });
        let req = parse_request(D, &body).unwrap();
        assert_eq!(
            req.messages[0].parts[1],
            Part::Image {
                url: "data:image/png;base64,QUJD".into(),
                media_type: Some("image/png".into())
            }
        );
        let out = build_request(D, &req, false).unwrap();
        let source = &out["messages"][0]["content"][1]["source"];
        assert_eq!(source["type"], "base64");
        assert_eq!(source["media_type"], "image/png");
        assert_eq!(source["data"], "QUJD");
    }

    // ------------------------------------------------------------ account refusals

    /// An Anthropic error answer, in the shape both carriers use.
    fn answer(kind: &str, message: &str) -> Vec<u8> {
        json!({"type": "error", "error": {"type": kind, "message": message}})
            .to_string()
            .into_bytes()
    }

    /// The gate's exact body: a rate-limit status with a throttle's error type and no
    /// content at all in the message.
    fn gate() -> Vec<u8> {
        answer("rate_limit_error", "Error")
    }

    /// Every class is recognised from the answer the provider actually sends for it, so
    /// the operator reads the class's own reason instead of a generic upstream error.
    #[test]
    fn every_refusal_class_is_recognised_from_its_own_answer() {
        // A disabled account, and a disabled organization: the same class, two subjects.
        for (status, message) in [
            (400, "Your account has been disabled"),
            (403, "Your account has been disabled"),
            (403, "This organization has been suspended"),
            (400, "This account has been deactivated"),
        ] {
            assert_eq!(
                refusal(
                    Some(status),
                    &answer("invalid_request_error", message),
                    false
                ),
                Some(Refusal::Terminated),
                "{status} {message}"
            );
        }

        // Entitlements: the credential is alive but not authorized for this request.
        for (status, kind, message) in [
            (
                400,
                "invalid_request_error",
                "This credential is only authorized for use with Claude Code and cannot be used \
                 for other API requests.",
            ),
            (
                400,
                "invalid_request_error",
                "Third-party apps now draw from your extra usage, not your plan limits.",
            ),
            (403, "permission_error", "Request not allowed"),
            // The subscription gate, in its exact shape and with no rate-limit headers.
            (429, "rate_limit_error", "Error"),
        ] {
            assert_eq!(
                refusal(Some(status), &answer(kind, message), false),
                Some(Refusal::Entitlement),
                "{status} {message}"
            );
        }

        // Our own fingerprint being stale is a third, distinct class: nothing is wrong
        // with the account, so it must not read as one.
        assert_eq!(
            refusal(
                Some(400),
                &answer(
                    "invalid_request_error",
                    "Claude Code version 2.1.280 or newer is required"
                ),
                false
            ),
            Some(Refusal::ClientOutdated)
        );

        // The detail is the provider's own sentence, which is what the operator acts on.
        assert_eq!(
            refusal_detail(&answer("permission_error", "Request not allowed")),
            "Request not allowed"
        );
        assert_eq!(refusal_detail(b"not json"), "");
    }

    /// The gate wears a throttle's status, so the headers are the only thing that tells the
    /// two apart reliably: a real throttle always carries them and the gate never does.
    #[test]
    fn a_real_throttle_is_never_an_entitlement() {
        // The gate's own body, but this answer carried the provider's rate-limit headers:
        // a real throttle is a fact about the moment, not about the credential.
        assert_eq!(refusal(Some(429), &gate(), true), None);

        // A real limit, named as one, with and without headers.
        let real = answer(
            "rate_limit_error",
            "Number of request tokens has exceeded your per-minute rate limit",
        );
        assert_eq!(refusal(Some(429), &real, true), None);
        assert_eq!(
            refusal(Some(429), &real, false),
            None,
            "a 429 that names its limit is never the gate"
        );
        // A usage-limit answer with a reset time is a limit too.
        let window = answer(
            "rate_limit_error",
            "You have reached your 5-hour limit. Resets at 14:00 UTC.",
        );
        assert_eq!(refusal(Some(429), &window, false), None);
    }

    /// Guards: transient answers and ordinary request errors are never refusals, and a
    /// revoked token is refreshable rather than terminal.
    #[test]
    fn transient_and_ordinary_failures_are_not_refusals() {
        // Overloaded: 529, and the same class of frame inside a `200` stream envelope.
        assert_eq!(
            refusal(Some(529), &answer("overloaded_error", "Overloaded"), false),
            None
        );
        assert_eq!(
            refusal(
                Some(500),
                &answer("api_error", "Internal server error"),
                false
            ),
            None
        );
        // A 5xx can echo any wording; none of it is evidence about the account.
        assert_eq!(
            refusal(
                Some(503),
                &answer("api_error", "Your account has been disabled"),
                false
            ),
            None,
            "a 5xx is transient by definition"
        );
        // A plain request error: an unknown model must never retire anything.
        assert_eq!(
            refusal(
                Some(400),
                &answer("invalid_request_error", "model: unknown model claude-nope"),
                false
            ),
            None
        );
        // `Request not allowed` only classifies as an entitlement on the 403 it belongs to.
        assert_eq!(
            refusal(
                Some(400),
                &answer("invalid_request_error", "Request not allowed"),
                false
            ),
            None
        );
        // A revoked OAuth token is recoverable by a refresh, not a ban.
        for message in [
            "OAuth token has been revoked",
            "Invalid authentication credentials",
        ] {
            assert_eq!(
                refusal(Some(401), &answer("authentication_error", message), false),
                None,
                "{message}"
            );
        }
        assert_eq!(refusal(Some(429), b"not json", false), None);
        assert_eq!(refusal(Some(429), b"", false), None);
    }

    /// Anthropic streams errors as `event: error` after the `200` was sent, so the decoder
    /// is the last place a refusal can be recognised — and a dropped frame used to look
    /// like a successful truncated answer.
    #[test]
    fn a_streamed_refusal_becomes_an_account_refusal() {
        let mut decoder = AnthropicDecoder::new("m");
        let mut out = Vec::new();
        let wire = format!(
            "{}data: {}\n\n",
            sse(
                "message_start",
                json!({"type": "message_start", "message": {"id": "msg_1", "model": "m"}})
            ),
            json!({"type": "error", "error": {"type": "invalid_request_error", "message":
                "This credential is only authorized for use with Claude Code"}})
        );
        let err = decoder
            .push(wire.as_bytes(), &mut out)
            .expect_err("a refusal is not a successful stream");
        assert_eq!(err.refusal(), Some(Refusal::Entitlement));
        assert_eq!(err.code(), Some("account_not_entitled"));
        assert!(err.to_string().contains("only authorized"), "{err}");

        // An overloaded frame is an error the client must see, but not a refusal.
        let mut decoder = AnthropicDecoder::new("m");
        let mut out = Vec::new();
        let wire = format!(
            "data: {}\n\n",
            json!({"type": "error", "error": {"type": "overloaded_error", "message": "Overloaded"}})
        );
        let err = decoder
            .push(wire.as_bytes(), &mut out)
            .expect_err("an error frame is not success");
        assert_eq!(err.refusal(), None);
        assert!(err.to_string().contains("Overloaded"), "{err}");
    }
}
