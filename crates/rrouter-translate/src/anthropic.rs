//! Anthropic Messages API: JSON bodies and event-framed SSE.

use serde_json::{json, Map, Value};

use rrouter_core::sse::{render_anthropic, SseCodec, SseFrame};
use rrouter_core::{
    ChatRequest, ChatResponse, Dialect, FinishReason, Message, Part, Result, Role, StreamEvent,
    ToolDef, ToolChoice, Usage,
};

use crate::{
    capture_extra, finish_from_anthropic, merge_extra, opt_f64, opt_u32, parse_err, parse_stop,
    req_str, req_str_field, to_json, tool_schema, usage_from_anthropic, usage_to_anthropic,
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
    req.stop = body.get("stop_sequences").map(parse_stop).unwrap_or_default();
    req.system = body.get("system").map(system_text).filter(|s| !s.is_empty());
    req.messages = parse_messages(body)?;
    req.tools = parse_tools(body);
    req.tool_choice = parse_tool_choice(body);

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
            let media_type = source.and_then(|s| req_str(s, "media_type")).map(str::to_owned);
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
            is_error: block.get("is_error").and_then(Value::as_bool).unwrap_or(false),
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

fn parse_tools(body: &Value) -> Vec<ToolDef> {
    let Some(items) = body.get("tools").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let Some(name) = req_str(item, "name") else { continue };
        out.push(ToolDef {
            name: name.to_owned(),
            description: req_str(item, "description").map(str::to_owned),
            parameters: item.get("input_schema").map(tool_schema).unwrap_or_else(|| tool_schema(&Value::Null)),
            // Anthropic has no strict flag; schemas are advisory only.
            strict: false,
        });
    }
    out
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

pub fn build_request(req: &ChatRequest, stream: bool) -> Result<Value> {
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
    body.insert("max_tokens".into(), Value::from(req.max_tokens.unwrap_or(4096)));
    body.insert("stream".into(), Value::Bool(stream));
    if let Some(system) = &req.system {
        if !system.is_empty() {
            body.insert("system".into(), Value::from(system.clone()));
        }
    }
    if !req.tools.is_empty() {
        let tools: Vec<Value> = req
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
        body.insert("tools".into(), Value::from(tools));
    }
    if let Some(choice) = &req.tool_choice {
        body.insert("tool_choice".into(), tool_choice_out(choice));
    }
    if let Some(t) = req.temperature {
        body.insert("temperature".into(), Value::from(t));
    }
    if let Some(p) = req.top_p {
        body.insert("top_p".into(), Value::from(p));
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
            Part::ToolResult { tool_use_id, content, is_error } => {
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
            let mt = meta.split(';').next().filter(|s| !s.is_empty()).map(str::to_owned).or_else(|| media_type.clone());
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
        Self { model: model.to_owned(), ..Self::default() }
    }

    pub fn push(&mut self, chunk: &[u8], out: &mut Vec<StreamEvent>) -> Result<()> {
        self.codec.push(chunk);
        while let Some(frame) = self.codec.next_frame() {
            self.frame(&frame, out);
        }
        Ok(())
    }

    pub fn finish(&mut self, out: &mut Vec<StreamEvent>) -> Result<()> {
        while let Some(frame) = self.codec.finish() {
            self.frame(&frame, out);
        }
        self.emit_done(out);
        Ok(())
    }

    fn frame(&mut self, frame: &SseFrame, out: &mut Vec<StreamEvent>) {
        if frame.is_done() {
            return;
        }
        let Ok(v) = serde_json::from_str::<Value>(&frame.data) else {
            return;
        };
        let event = frame
            .event
            .as_deref()
            .or_else(|| req_str(&v, "type"))
            .unwrap_or_default();
        match event {
            "message_start" => {
                let msg = v.get("message").unwrap_or(&Value::Null);
                self.message_id = req_str(msg, "id").unwrap_or_default().to_owned();
                let model = req_str(msg, "model").map(str::to_owned).unwrap_or_else(|| self.model.clone());
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
                        (BlockKind::ToolUse, Block { index, kind: BlockKind::ToolUse })
                    }
                    Some("thinking") => (BlockKind::Thinking, Block { index: 0, kind: BlockKind::Thinking }),
                    Some("text") => {
                        if let Some(t) = req_str(block, "text") {
                            if !t.is_empty() {
                                out.push(StreamEvent::TextDelta(t.to_owned()));
                            }
                        }
                        (BlockKind::Text, Block { index: 0, kind: BlockKind::Text })
                    }
                    _ => (BlockKind::Other, Block { index: 0, kind: BlockKind::Other }),
                };
                debug_assert!(kind == entry.kind);
                if self.blocks.len() <= slot {
                    self.blocks.resize_with(slot + 1, || None);
                }
                self.blocks[slot] = Some(entry);
            }
            "content_block_delta" => {
                let slot = opt_u32(&v, "index").unwrap_or(0) as usize;
                let Some(Some(block)) = self.blocks.get(slot) else { return };
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
                            partial_json: req_str(delta, "partial_json").unwrap_or_default().to_owned(),
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
    }

    fn emit_start(&mut self, out: &mut Vec<StreamEvent>, model: String) {
        if self.sent_start {
            return;
        }
        self.sent_start = true;
        out.push(StreamEvent::Start { id: self.message_id.clone(), model });
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
        out.push(StreamEvent::Done { reason, usage: Some(self.usage) });
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
        Self { model: model.to_owned(), ..Self::default() }
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
            StreamEvent::ToolCallArgsDelta { index, partial_json } => {
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
                out.push_str(&render_anthropic("message_stop", "{\"type\":\"message_stop\"}"));
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
        self.tool_blocks.get(tool_index as usize).copied().filter(|i| *i != u32::MAX)
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
            Part::ToolUse { id: "toolu_1".into(), name: "get_weather".into(), input: json!({"city": "Hanoi"}) }
        );
        assert_eq!(
            req.messages[2].parts,
            vec![Part::ToolResult { tool_use_id: "toolu_1".into(), content: "21C".into(), is_error: false }]
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
                parts: vec![Part::ToolUse { id: "t1".into(), name: "f".into(), input: json!({"a": 1}) }],
            },
            Message {
                role: Role::Tool,
                parts: vec![Part::ToolResult { tool_use_id: "t1".into(), content: "done".into(), is_error: true }],
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
        assert_eq!(body["messages"][2]["role"], "user", "tool results ride in a user turn");
    }

    #[test]
    fn system_string_and_block_array_both_parse() {
        let plain = parse_request(D, &json!({"model": "m", "max_tokens": 1, "system": "s1", "messages": []})).unwrap();
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
            Part::Reasoning { text: "step 1".into(), signature: Some("sig-abc".into()) }
        );
        assert_eq!(resp.parts[1], Part::Reasoning { text: String::new(), signature: Some("redacted-blob".into()) });
        assert_eq!(resp.usage, Usage { input_tokens: 9, output_tokens: 4, cache_read_tokens: 6, cache_write_tokens: 2 });
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
            let body = json!({"id": "x", "model": "m", "content": [], "stop_reason": wire, "usage": {}});
            assert_eq!(parse_response(&body).unwrap().finish_reason, want, "anthropic {wire}");
            let oi = json!({"choices": [{"message": {"role": "assistant", "content": ""}, "finish_reason": want.openai()}]});
            assert_eq!(crate::openai_chat::parse_response(&oi).unwrap().finish_reason, want, "openai {}", want.openai());
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

        assert_eq!(events[0], StreamEvent::Start { id: "msg_9".into(), model: "claude-sonnet-4".into() });
        let text: String = events
            .iter()
            .filter_map(|e| match e {
                StreamEvent::TextDelta(t) => Some(t.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(text, "Hello");
        assert_eq!(
            events.iter().filter(|e| matches!(e, StreamEvent::ToolCallStart { .. })).count(),
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
        let done: Vec<_> = events.iter().filter(|e| matches!(e, StreamEvent::Done { .. })).collect();
        assert_eq!(done.len(), 1);
        assert_eq!(
            *done[0],
            StreamEvent::Done {
                reason: FinishReason::ToolCalls,
                usage: Some(Usage { input_tokens: 12, output_tokens: 15, cache_read_tokens: 0, cache_write_tokens: 0 }),
            }
        );
    }

    #[test]
    fn decoder_finish_emits_done_when_upstream_closes_early() {
        let mut dec = AnthropicDecoder::new("m");
        let mut events = Vec::new();
        dec.push(
            sse("message_start", json!({"message": {"id": "x", "model": "m"}})).as_bytes(),
            &mut events,
        )
        .unwrap();
        dec.finish(&mut events).unwrap();
        assert_eq!(events.last(), Some(&StreamEvent::Done { reason: FinishReason::Stop, usage: Some(Usage::default()) }));
    }

    #[test]
    fn encoder_frames_blocks_deltas_and_stop_reason() {
        let mut enc = StreamEncoder::new(D, "claude-sonnet-4");
        let mut wire = String::new();
        for ev in [
            StreamEvent::Start { id: "msg_1".into(), model: "claude-sonnet-4".into() },
            StreamEvent::ReasoningDelta("think".into()),
            StreamEvent::TextDelta("hi".into()),
            StreamEvent::ToolCallStart { index: 0, id: "t0".into(), name: "f".into() },
            StreamEvent::ToolCallArgsDelta { index: 0, partial_json: "{\"k\":".into() },
            StreamEvent::ToolCallArgsDelta { index: 0, partial_json: "2}".into() },
            StreamEvent::Done {
                reason: FinishReason::Length,
                usage: Some(Usage { input_tokens: 3, output_tokens: 4, cache_read_tokens: 0, cache_write_tokens: 0 }),
            },
        ] {
            wire.push_str(std::str::from_utf8(&enc.encode(&ev).unwrap()).unwrap());
        }
        assert!(wire.contains("event: message_start"));
        assert!(wire.contains("\"thinking_delta\""));
        assert!(wire.contains("\"text_delta\""));
        assert_eq!(wire.matches("event: content_block_start").count(), 3, "thinking, text, tool_use");
        assert_eq!(wire.matches("event: content_block_stop").count(), 3);
        assert!(wire.contains("\"partial_json\":\"{\\\"k\\\":\""));
        assert!(wire.contains("\"stop_reason\":\"max_tokens\""));
        assert_eq!(wire.matches("event: message_stop").count(), 1);
        assert!(wire.trim_end().ends_with("data: {\"type\":\"message_stop\"}"));
    }

    #[test]
    fn encoder_output_redecodes_into_the_same_events() {
        let events = vec![
            StreamEvent::Start { id: "msg_7".into(), model: "claude-sonnet-4".into() },
            StreamEvent::TextDelta("abc".into()),
            StreamEvent::ToolCallStart { index: 0, id: "t0".into(), name: "f".into() },
            StreamEvent::ToolCallArgsDelta { index: 0, partial_json: "{}".into() },
            StreamEvent::Done { reason: FinishReason::ToolCalls, usage: Some(Usage::default()) },
        ];
        let mut enc = StreamEncoder::new(D, "claude-sonnet-4");
        let wire: Vec<u8> = events.iter().flat_map(|e| enc.encode(e).unwrap().to_vec()).collect();
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
        assert!(anthropic_body["tools"][0].get("function").is_none(), "anthropic tools are flat");

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
        assert_eq!(call["function"]["arguments"].as_str().unwrap(), "{\"city\":\"Hue\"}");
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
            Part::Image { url: "data:image/png;base64,QUJD".into(), media_type: Some("image/png".into()) }
        );
        let out = build_request(D, &req, false).unwrap();
        let source = &out["messages"][0]["content"][1]["source"];
        assert_eq!(source["type"], "base64");
        assert_eq!(source["media_type"], "image/png");
        assert_eq!(source["data"], "QUJD");
    }
}
