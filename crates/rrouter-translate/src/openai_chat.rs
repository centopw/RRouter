//! [OI] Chat Completions: JSON bodies and `data:`-framed SSE.

use serde_json::{json, Map, Value};

use rrouter_core::sse::{render_openai, SseCodec, SseFrame};
use rrouter_core::{
    ChatRequest, ChatResponse, Dialect, FinishReason, Message, Part, Result, Role, StreamEvent,
    ToolDef, Usage,
};

use crate::{
    arguments_to_string, arguments_value, capture_extra, finish_from_openai, merge_extra, openai_tool_choice_value,
    opt_f64, opt_u32, parse_arguments, parse_err, parse_openai_tool_choice, parse_stop, req_str,
    req_str_field, tool_schema, usage_from_openai, usage_to_openai,
};

const D: Dialect = Dialect::OpenAiChat;

const KNOWN_KEYS: &[&str] = &[
    "model",
    "messages",
    "tools",
    "tool_choice",
    "max_tokens",
    "max_completion_tokens",
    "temperature",
    "top_p",
    "stop",
    "stream",
    "parallel_tool_calls",
];

// ---------------------------------------------------------------------------
// Request in
// ---------------------------------------------------------------------------

pub fn parse_request(body: &Value) -> Result<ChatRequest> {
    let model = req_str_field(body, "model", D)?;
    let mut req = ChatRequest::new(model);
    req.stream = body.get("stream").and_then(Value::as_bool).unwrap_or(false);
    req.max_tokens =
        opt_u32(body, "max_tokens").or_else(|| opt_u32(body, "max_completion_tokens"));
    req.temperature = opt_f64(body, "temperature");
    req.top_p = opt_f64(body, "top_p");
    req.stop = body.get("stop").map(parse_stop).unwrap_or_default();
    req.tool_choice = body.get("tool_choice").and_then(parse_openai_tool_choice);

    let (system, messages) = parse_messages(body)?;
    req.system = system;
    req.messages = messages;
    req.tools = parse_tools(body);
    if req.tool_choice.is_some() && req.tools.is_empty() {
        req.tool_choice = None;
    }

    capture_extra(body, KNOWN_KEYS, D, &mut req)?;
    Ok(req)
}

/// Split leading `role:"system"` messages into `ChatRequest.system`; several
/// system messages concatenate with a blank line.
fn parse_messages(body: &Value) -> Result<(Option<String>, Vec<Message>)> {
    let raw = match body.get("messages") {
        None | Some(Value::Null) => return Ok((None, Vec::new())),
        Some(Value::Array(items)) => items,
        Some(_) => return Err(parse_err(D, "`messages` must be an array")),
    };
    let mut system: Vec<String> = Vec::new();
    let mut out: Vec<Message> = Vec::with_capacity(raw.len());
    for (i, msg) in raw.iter().enumerate() {
        let role = req_str(msg, "role")
            .ok_or_else(|| parse_err(D, &format!("messages[{i}] has no role")))?;
        let parts = parse_parts(msg, i)?;
        match role {
            "system" | "developer" => {
                let text = msg.get("content").map(content_text).unwrap_or_default();
                system.push(text);
                // Keep non-text parts (rare) as a system message rather than losing them.
                if parts.iter().any(|p| !matches!(p, Part::Text(_))) {
                    out.push(Message { role: Role::System, parts });
                }
            }
            "user" => out.push(Message { role: Role::User, parts }),
            "assistant" => out.push(Message { role: Role::Assistant, parts }),
            "tool" | "function" => out.push(Message { role: Role::Tool, parts }),
            other => return Err(parse_err(D, &format!("unknown role `{other}`"))),
        }
    }
    let system = if system.is_empty() { None } else { Some(system.join("\n\n")) };
    Ok((system, out))
}

fn content_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .filter_map(|i| i.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(""),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn parse_parts(msg: &Value, index: usize) -> Result<Vec<Part>> {
    let mut parts: Vec<Part> = Vec::new();
    match msg.get("content") {
        Some(Value::String(s)) => {
            if !s.is_empty() {
                parts.push(Part::Text(s.clone()));
            }
        }
        Some(Value::Array(items)) => {
            for item in items {
                let kind = req_str(item, "type").unwrap_or("text");
                match kind {
                    "text" | "input_text" | "output_text" | "refusal" => {
                        if let Some(t) = req_str(item, "text") {
                            parts.push(Part::Text(t.to_owned()));
                        }
                    }
                    "image_url" | "input_image" => {
                        let (url, media_type) = parse_image(item);
                        parts.push(Part::Image { url, media_type });
                    }
                    other => parts.push(Part::Opaque(json!({"type": other}))),
                }
            }
        }
        Some(Value::Null) | None => {}
        Some(_) => return Err(parse_err(D, &format!("messages[{index}].content must be string or array"))),
    }

    // A [OI] `role:"tool"` message carries the result in `content` + tool_call_id.
    if matches!(req_str(msg, "role"), Some("tool") | Some("function")) {
        let id = req_str(msg, "tool_call_id")
            .or_else(|| req_str(msg, "name"))
            .unwrap_or_default()
            .to_owned();
        let text = parts
            .iter()
            .filter_map(|p| match p {
                Part::Text(t) => Some(t.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("");
        return Ok(vec![Part::ToolResult { tool_use_id: id, content: text, is_error: false }]);
    }

    if let Some(calls) = msg.get("tool_calls").and_then(Value::as_array) {
        for call in calls {
            let id = req_str(call, "id").unwrap_or_default().to_owned();
            let func = call.get("function");
            let name = func.and_then(|f| req_str(f, "name")).unwrap_or_default().to_owned();
            let args = func
                .and_then(|f| f.get("arguments"))
                .map(arguments_to_string)
                .unwrap_or_default();
            if name.is_empty() && id.is_empty() {
                continue;
            }
            parts.push(Part::ToolUse { id, name, input: parse_arguments(&args) });
        }
    }
    if let Some(text) = req_str(msg, "reasoning_content") {
        if !text.is_empty() {
            parts.push(Part::Reasoning { text: text.to_owned(), signature: None });
        }
    }
    Ok(parts)
}

fn parse_image(item: &Value) -> (String, Option<String>) {
    let holder = item.get("image_url").unwrap_or(item);
    let url = req_str(holder, "url")
        .map(str::to_owned)
        .or_else(|| req_str(item, "image_url").map(str::to_owned))
        .unwrap_or_default();
    let media_type = req_str(holder, "media_type")
        .map(str::to_owned)
        .or_else(|| data_url_media_type(&url));
    (url, media_type)
}

/// `data:image/png;base64,...` → `image/png`.
fn data_url_media_type(url: &str) -> Option<String> {
    let rest = url.strip_prefix("data:")?;
    let (meta, _) = rest.split_once(',')?;
    Some(meta.split(';').next().unwrap_or(meta).to_owned())
}

fn parse_tools(body: &Value) -> Vec<ToolDef> {
    let Some(items) = body.get("tools").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let func = item.get("function").unwrap_or(item);
        let Some(name) = req_str(func, "name") else { continue };
        out.push(ToolDef {
            name: name.to_owned(),
            description: req_str(func, "description").map(str::to_owned),
            parameters: func.get("parameters").map(tool_schema).unwrap_or_else(|| tool_schema(&Value::Null)),
        });
    }
    out
}

/// Embeddings passthrough: keep the whole body in `extra` so `build_request`
/// reproduces it for the same dialect.
pub fn parse_embeddings_request(body: &Value) -> ChatRequest {
    let model = req_str(body, "model").unwrap_or_default().to_owned();
    let mut req = ChatRequest::new(model);
    req.extra = Some((Dialect::OpenAiEmbeddings, body.clone()));
    req
}

pub fn build_embeddings_request(req: &ChatRequest) -> Value {
    let mut body = match &req.extra {
        Some((Dialect::OpenAiEmbeddings, v)) if v.is_object() => v.clone(),
        _ => json!({}),
    };
    if let Some(obj) = body.as_object_mut() {
        obj.insert("model".into(), Value::from(req.model.clone()));
    }
    body
}

// ---------------------------------------------------------------------------
// Request out
// ---------------------------------------------------------------------------

pub fn build_request(req: &ChatRequest, stream: bool) -> Result<Value> {
    let mut messages: Vec<Value> = Vec::with_capacity(req.messages.len() + 1);
    if let Some(system) = &req.system {
        if !system.is_empty() {
            messages.push(json!({"role": "system", "content": system}));
        }
    }
    for msg in &req.messages {
        for m in messages_out(msg)? {
            messages.push(m);
        }
    }

    let mut body = Map::new();
    body.insert("model".into(), Value::from(req.model.clone()));
    body.insert("messages".into(), Value::from(messages));
    body.insert("stream".into(), Value::Bool(stream));
    if stream {
        body.insert("stream_options".into(), json!({"include_usage": true}));
    }
    if !req.tools.is_empty() {
        let tools: Vec<Value> = req
            .tools
            .iter()
            .map(|t| {
                let mut func = Map::new();
                func.insert("name".into(), Value::from(t.name.clone()));
                if let Some(desc) = &t.description {
                    func.insert("description".into(), Value::from(desc.clone()));
                }
                func.insert("parameters".into(), t.parameters.clone());
                json!({"type": "function", "function": Value::Object(func)})
            })
            .collect();
        body.insert("tools".into(), Value::from(tools));
    }
    if let Some(choice) = &req.tool_choice {
        body.insert("tool_choice".into(), openai_tool_choice_value(choice));
    }
    if let Some(n) = req.max_tokens {
        body.insert("max_completion_tokens".into(), Value::from(n));
    }
    if let Some(t) = req.temperature {
        body.insert("temperature".into(), Value::from(t));
    }
    if let Some(p) = req.top_p {
        body.insert("top_p".into(), Value::from(p));
    }
    if !req.stop.is_empty() {
        body.insert("stop".into(), Value::from(req.stop.clone()));
    }
    let mut value = Value::Object(body);
    merge_extra(&mut value, req, D);
    Ok(value)
}

/// `[OI]` has no tool part: each tool result becomes its own `role:"tool"`
/// message, so one canonical message may expand to several wire messages.
fn messages_out(msg: &Message) -> Result<Vec<Value>> {
    let mut tool_msgs: Vec<Value> = Vec::new();
    let mut texts: Vec<Value> = Vec::new();
    let mut calls: Vec<Value> = Vec::new();
    let mut reasoning = String::new();
    let mut tool_content: Option<String> = None;

    for part in &msg.parts {
        match part {
            Part::Text(t) => texts.push(json!({"type": "text", "text": t})),
            Part::Image { url, .. } => {
                texts.push(json!({"type": "image_url", "image_url": {"url": url}}))
            }
            Part::ToolUse { id, name, input } => calls.push(json!({
                "id": id,
                "type": "function",
                "function": {"name": name, "arguments": arguments_value(input)?},
            })),
            Part::ToolResult { tool_use_id, content, is_error } => {
                if msg.role == Role::Tool {
                    tool_content.get_or_insert_with(String::new).push_str(content);
                } else {
                    tool_msgs.push(json!({
                        "role": "tool",
                        "tool_call_id": tool_use_id,
                        "content": if *is_error { format!("error: {content}") } else { content.clone() },
                    }));
                }
            }
            Part::Reasoning { text, .. } => reasoning.push_str(text),
            Part::Opaque(v) => texts.push(v.clone()),
        }
    }

    if msg.role == Role::Tool {
        return Ok(vec![json!({
            "role": "tool",
            "tool_call_id": msg
                .parts
                .iter()
                .find_map(|p| match p {
                    Part::ToolResult { tool_use_id, .. } => Some(tool_use_id.clone()),
                    _ => None,
                })
                .unwrap_or_default(),
            "content": tool_content.unwrap_or_default(),
        })]);
    }

    let mut out = Map::new();
    out.insert(
        "role".into(),
        Value::from(match msg.role {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        }),
    );
    if texts.is_empty() && calls.is_empty() {
        out.insert("content".into(), Value::Null);
    } else if texts.len() == 1 && texts[0].get("type") == Some(&Value::from("text")) {
        out.insert("content".into(), texts[0].get("text").cloned().unwrap_or(Value::Null));
    } else if !texts.is_empty() {
        out.insert("content".into(), Value::Array(texts));
    }
    if !calls.is_empty() {
        out.insert("tool_calls".into(), Value::Array(calls));
    }
    if !reasoning.is_empty() {
        out.insert("reasoning_content".into(), Value::from(reasoning));
    }
    let mut all = vec![Value::Object(out)];
    all.extend(tool_msgs);
    Ok(all)
}

// ---------------------------------------------------------------------------
// Response in
// ---------------------------------------------------------------------------

pub fn parse_response(body: &Value) -> Result<ChatResponse> {
    let id = req_str(body, "id").unwrap_or_default().to_owned();
    let model = req_str(body, "model").unwrap_or_default().to_owned();
    let choice = body
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|c| c.first())
        .ok_or_else(|| parse_err(D, "response has no choices"))?;
    let msg = choice.get("message").ok_or_else(|| parse_err(D, "choice has no message"))?;

    let mut parts = Vec::new();
    if let Some(text) = msg.get("content") {
        let text = content_text(text);
        if !text.is_empty() {
            parts.push(Part::Text(text));
        }
    }
    if let Some(reasoning) = req_str(msg, "reasoning_content").or_else(|| req_str(msg, "reasoning")) {
        if !reasoning.is_empty() {
            parts.push(Part::Reasoning { text: reasoning.to_owned(), signature: None });
        }
    }
    if let Some(calls) = msg.get("tool_calls").and_then(Value::as_array) {
        for call in calls {
            let id = req_str(call, "id").unwrap_or_default().to_owned();
            let func = call.get("function");
            let name = func.and_then(|f| req_str(f, "name")).unwrap_or_default().to_owned();
            let args = func
                .and_then(|f| f.get("arguments"))
                .map(arguments_to_string)
                .unwrap_or_default();
            parts.push(Part::ToolUse { id, name, input: parse_arguments(&args) });
        }
    }

    Ok(ChatResponse {
        id,
        model,
        parts,
        finish_reason: finish_from_openai(req_str(choice, "finish_reason")),
        usage: usage_from_openai(body.get("usage")),
    })
}

// ---------------------------------------------------------------------------
// Response out
// ---------------------------------------------------------------------------

pub fn build_response(resp: &ChatResponse) -> Result<Value> {
    let mut content = String::new();
    let mut reasoning = String::new();
    let mut calls: Vec<Value> = Vec::new();
    for part in &resp.parts {
        match part {
            Part::Text(t) => content.push_str(t),
            Part::Reasoning { text, .. } => reasoning.push_str(text),
            Part::ToolUse { id, name, input } => calls.push(json!({
                "id": id,
                "type": "function",
                "function": {"name": name, "arguments": arguments_value(input)?},
            })),
            Part::Image { .. } | Part::ToolResult { .. } | Part::Opaque(_) => {}
        }
    }
    let mut message = Map::new();
    message.insert("role".into(), Value::from("assistant"));
    message.insert("content".into(), if content.is_empty() { Value::Null } else { Value::from(content) });
    if !calls.is_empty() {
        message.insert("tool_calls".into(), Value::Array(calls));
    }
    if !reasoning.is_empty() {
        message.insert("reasoning_content".into(), Value::from(reasoning));
    }

    Ok(json!({
        "id": resp.id,
        "object": "chat.completion",
        "model": resp.model,
        "choices": [{
            "index": 0,
            "message": Value::Object(message),
            "finish_reason": resp.finish_reason.openai(),
        }],
        "usage": usage_to_openai(resp.usage),
    }))
}

// ---------------------------------------------------------------------------
// Streaming in
// ---------------------------------------------------------------------------

/// Per-index tool-call bookkeeping while decoding.
#[derive(Debug, Default)]
struct CallSlot {
    id: String,
    name: String,
    started: bool,
}

#[derive(Debug, Default)]
pub struct OpenAiChatDecoder {
    codec: SseCodec,
    model: String,
    id: String,
    sent_start: bool,
    done: bool,
    slots: Vec<CallSlot>,
    usage: Option<Usage>,
    finish: Option<FinishReason>,
}

impl OpenAiChatDecoder {
    pub fn new(model: &str) -> Self {
        Self { model: model.to_owned(), ..Self::default() }
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
            self.emit_done(out);
            return Ok(());
        }
        let Ok(v) = serde_json::from_str::<Value>(&frame.data) else {
            return Ok(()); // keep-alive or non-JSON comment payload
        };
        self.chunk(&v, out)
    }

    fn chunk(&mut self, v: &Value, out: &mut Vec<StreamEvent>) -> Result<()> {
        if !self.sent_start {
            self.sent_start = true;
            self.id = req_str(v, "id").unwrap_or_default().to_owned();
            let model = req_str(v, "model").map(str::to_owned).unwrap_or_else(|| self.model.clone());
            out.push(StreamEvent::Start { id: self.id.clone(), model });
        }
        if let Some(usage) = v.get("usage") {
            if !usage.is_null() {
                self.usage = Some(usage_from_openai(Some(usage)));
            }
        }
        let Some(choices) = v.get("choices").and_then(Value::as_array) else {
            return Ok(());
        };
        for choice in choices {
            if let Some(delta) = choice.get("delta") {
                if let Some(text) = req_str(delta, "content") {
                    if !text.is_empty() {
                        out.push(StreamEvent::TextDelta(text.to_owned()));
                    }
                }
                if let Some(r) = req_str(delta, "reasoning_content").or_else(|| req_str(delta, "reasoning")) {
                    if !r.is_empty() {
                        out.push(StreamEvent::ReasoningDelta(r.to_owned()));
                    }
                }
                if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
                    for (pos, call) in calls.iter().enumerate() {
                        let index = opt_u32(call, "index").unwrap_or(pos as u32);
                        let func = call.get("function");
                        let id = req_str(call, "id").unwrap_or_default();
                        let name = func.and_then(|f| req_str(f, "name")).unwrap_or_default();
                        let args = func
                            .and_then(|f| f.get("arguments"))
                            .map(arguments_to_string)
                            .unwrap_or_default();

                        let idx = index as usize;
                        if self.slots.len() <= idx {
                            self.slots.resize_with(idx + 1, CallSlot::default);
                        }
                        let slot = &mut self.slots[idx];
                        if !id.is_empty() {
                            slot.id = id.to_owned();
                        }
                        if !name.is_empty() {
                            slot.name.push_str(name);
                        }
                        let ready = !slot.started && !(slot.id.is_empty() && slot.name.is_empty());
                        if ready {
                            slot.started = true;
                            out.push(StreamEvent::ToolCallStart {
                                index,
                                id: slot.id.clone(),
                                name: slot.name.clone(),
                            });
                        }
                        if !args.is_empty() {
                            out.push(StreamEvent::ToolCallArgsDelta { index, partial_json: args });
                        }
                    }
                }
            }
            if let Some(reason) = req_str(choice, "finish_reason") {
                self.finish = Some(finish_from_openai(Some(reason)));
            }
        }
        Ok(())
    }

    fn emit_done(&mut self, out: &mut Vec<StreamEvent>) {
        if self.done {
            return;
        }
        // Some upstreams close the stream without any frame being parsed at all.
        if !self.sent_start {
            self.sent_start = true;
            out.push(StreamEvent::Start { id: self.id.clone(), model: self.model.clone() });
        }
        self.done = true;
        let reason = self.finish.unwrap_or_else(|| {
            if self.slots.iter().any(|s| !s.name.is_empty()) {
                FinishReason::ToolCalls
            } else {
                FinishReason::Stop
            }
        });
        out.push(StreamEvent::Done { reason, usage: self.usage });
    }
}

// ---------------------------------------------------------------------------
// Streaming out
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
pub struct OpenAiChatEncoder {
    model: String,
    id: String,
    sent_start: bool,
    done: bool,
    created: u64,
}

impl OpenAiChatEncoder {
    pub fn new(model: &str) -> Self {
        Self { model: model.to_owned(), created: 1, ..Self::default() }
    }

    pub fn encode(&mut self, ev: &StreamEvent) -> bytes::Bytes {
        if self.done {
            return bytes::Bytes::new();
        }
        let mut frames = String::new();
        match ev {
            StreamEvent::Start { id, model } => {
                if self.sent_start {
                    return bytes::Bytes::new();
                }
                self.sent_start = true;
                self.id = id.clone();
                self.model = model.clone();
            }
            StreamEvent::TextDelta(t) => {
                if t.is_empty() {
                    return bytes::Bytes::new();
                }
                self.delta(&mut frames, json!({"content": t}));
            }
            StreamEvent::ReasoningDelta(t) => {
                if t.is_empty() {
                    return bytes::Bytes::new();
                }
                self.delta(&mut frames, json!({"reasoning_content": t}));
            }
            StreamEvent::ToolCallStart { index, id, name } => {
                self.delta(
                    &mut frames,
                    json!({"tool_calls": [{
                        "index": index,
                        "id": id,
                        "type": "function",
                        "function": {"name": name, "arguments": ""},
                    }]}),
                );
            }
            StreamEvent::ToolCallArgsDelta { index, partial_json } => {
                self.delta(
                    &mut frames,
                    json!({"tool_calls": [{
                        "index": index,
                        "function": {"arguments": partial_json},
                    }]}),
                );
            }
            StreamEvent::Done { reason, usage } => {
                self.done = true;
                let mut chunk = self.envelope();
                chunk["choices"] = json!([{
                    "index": 0,
                    "delta": {},
                    "finish_reason": reason.openai(),
                }]);
                if let Some(u) = usage {
                    chunk["usage"] = usage_to_openai(*u);
                }
                frames.push_str(&render_openai(&chunk.to_string(), None));
                frames.push_str(&render_openai("[DONE]", None));
                return bytes::Bytes::from(frames);
            }
        }
        bytes::Bytes::from(frames)
    }

    fn envelope(&self) -> Value {
        json!({
            "id": self.id,
            "object": "chat.completion.chunk",
            "created": self.created,
            "model": self.model,
        })
    }

    fn delta(&self, frames: &mut String, delta: Value) {
        let mut chunk = self.envelope();
        chunk["choices"] = json!([{"index": 0, "delta": delta, "finish_reason": null}]);
        frames.push_str(&render_openai(&chunk.to_string(), None));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{build_request, parse_request, StreamEncoder};

    fn drain(d: &mut OpenAiChatDecoder, chunks: &[&str]) -> Vec<StreamEvent> {
        let mut out = Vec::new();
        for c in chunks {
            d.push(c.as_bytes(), &mut out).unwrap();
        }
        d.finish(&mut out).unwrap();
        out
    }

    fn frame(event: Option<&str>, data: &str) -> String {
        render_openai(data, event)
    }

    #[test]
    fn tool_calls_parse_arguments_string_into_object() {
        let body = json!({
            "model": "gpt-4o",
            "messages": [{
                "role": "assistant",
                "content": null,
                "tool_calls": [
                    {"id": "call_a", "type": "function",
                     "function": {"name": "get_weather", "arguments": "{\"city\":\"Hanoi\"}"}}
                ]
            }]
        });
        let req = parse_request(D, &body).unwrap();
        assert_eq!(
            req.messages[0].parts,
            vec![Part::ToolUse {
                id: "call_a".into(),
                name: "get_weather".into(),
                input: json!({"city": "Hanoi"}),
            }]
        );
        // And back out: arguments must be a JSON *string*, not an object.
        let built = build_request(D, &req, false).unwrap();
        let args = &built["messages"][0]["tool_calls"][0]["function"]["arguments"];
        assert!(args.is_string(), "arguments must serialize as a string: {args}");
        assert_eq!(args.as_str().unwrap(), "{\"city\":\"Hanoi\"}");
    }

    #[test]
    fn tool_role_messages_map_to_tool_results() {
        let body = json!({
            "model": "gpt-4o",
            "messages": [
                {"role": "user", "content": "weather?"},
                {"role": "tool", "tool_call_id": "call_a", "content": "21C"},
            ]
        });
        let req = parse_request(D, &body).unwrap();
        assert_eq!(
            req.messages[1].parts,
            vec![Part::ToolResult { tool_use_id: "call_a".into(), content: "21C".into(), is_error: false }]
        );
        let built = build_request(D, &req, false).unwrap();
        assert_eq!(built["messages"][1]["role"], "tool");
        assert_eq!(built["messages"][1]["tool_call_id"], "call_a");
    }

    #[test]
    fn system_messages_hoist_and_concatenate() {
        let body = json!({
            "model": "gpt-4o",
            "messages": [
                {"role": "system", "content": "one"},
                {"role": "system", "content": "two"},
                {"role": "user", "content": "hi"},
            ]
        });
        let req = parse_request(D, &body).unwrap();
        assert_eq!(req.system.as_deref(), Some("one\n\ntwo"));
        assert_eq!(req.messages.len(), 1, "system messages are not kept in messages");
        let built = build_request(D, &req, false).unwrap();
        assert_eq!(built["messages"][0]["role"], "system");
        assert_eq!(built["messages"][0]["content"], "one\n\ntwo");
    }

    #[test]
    fn response_usage_and_stop_reason_normalize() {
        let body = json!({
            "id": "c1",
            "model": "gpt-4o",
            "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"}, "finish_reason": "length"}],
            "usage": {"prompt_tokens": 11, "completion_tokens": 7, "prompt_tokens_details": {"cached_tokens": 3}}
        });
        let resp = parse_response(&body).unwrap();
        assert_eq!(resp.finish_reason, FinishReason::Length);
        assert_eq!(resp.usage, Usage { input_tokens: 11, output_tokens: 7, cache_read_tokens: 3, cache_write_tokens: 0 });
        let back = build_response(&resp).unwrap();
        assert_eq!(back["usage"]["prompt_tokens"], 11);
        assert_eq!(back["usage"]["completion_tokens"], 7);
        assert_eq!(back["choices"][0]["finish_reason"], "length");
    }

    #[test]
    fn stream_tool_call_fragments_concatenate_by_index() {
        let chunks = [
            frame(None, r#"{"id":"c1","model":"gpt-4o","choices":[{"index":0,"delta":{"role":"assistant","tool_calls":[{"index":0,"id":"call_a","type":"function","function":{"name":"lookup","arguments":"{\"q\":"}}]}}]}"#),
            frame(None, r#"{"id":"c1","model":"gpt-4o","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"rust\"}"}}]}}]}"#),
            frame(None, r#"{"id":"c1","model":"gpt-4o","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":5,"completion_tokens":2}}"#),
        ];
        let refs: Vec<&str> = chunks.iter().map(String::as_str).collect();
        let events = drain(&mut OpenAiChatDecoder::new("gpt-4o"), &refs);
        let mut args = String::new();
        let mut starts = 0;
        let mut done = 0;
        for ev in &events {
            match ev {
                StreamEvent::ToolCallStart { id, name, index } => {
                    starts += 1;
                    assert_eq!((*index, id.as_str(), name.as_str()), (0, "call_a", "lookup"));
                }
                StreamEvent::ToolCallArgsDelta { index, partial_json } => {
                    assert_eq!(*index, 0);
                    args.push_str(partial_json);
                }
                StreamEvent::Done { reason, usage } => {
                    done += 1;
                    assert_eq!(*reason, FinishReason::ToolCalls);
                    assert_eq!(usage.expect("usage").output_tokens, 2);
                }
                _ => {}
            }
        }
        assert_eq!((starts, done), (1, 1));
        assert_eq!(args, "{\"q\":\"rust\"}");
        assert_eq!(crate::parse_arguments(&args), json!({"q": "rust"}));
    }

    #[test]
    fn encoder_emits_done_exactly_once_with_done_marker() {
        let mut enc = StreamEncoder::new(D, "gpt-4o");
        let mut wire = String::new();
        for ev in [
            StreamEvent::Start { id: "c9".into(), model: "gpt-4o".into() },
            StreamEvent::TextDelta("hi".into()),
            StreamEvent::Done { reason: FinishReason::Stop, usage: Some(Usage::default()) },
            StreamEvent::TextDelta("late".into()),
        ] {
            let bytes = enc.encode(&ev).unwrap();
            wire.push_str(std::str::from_utf8(&bytes).unwrap());
        }
        assert_eq!(wire.matches("data: [DONE]").count(), 1);
        assert!(!wire.contains("late"));
        assert!(wire.contains(r#""content":"hi""#));
    }
}
