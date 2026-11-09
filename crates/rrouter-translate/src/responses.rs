//! [OI] Responses API (`/v1/responses`): JSON bodies and `data:`-framed SSE
//! with `event:` names, but not Anthropic framing.

use serde_json::{json, Map, Value};

use rrouter_core::sse::{render_openai, SseCodec, SseFrame};
use rrouter_core::{
    ChatRequest, ChatResponse, Dialect, FinishReason, Message, Part, Result, Role, StreamEvent,
    ToolChoice, ToolDef, Usage,
};

use crate::{
    capture_extra, finish_from_responses, merge_extra, opt_f64, opt_u32, parse_err, req_str,
    req_str_field, tool_schema, usage_from_responses, usage_to_responses, WireProfile,
};

const D: Dialect = Dialect::OpenAiResponses;

const KNOWN_KEYS: &[&str] = &[
    "model",
    "instructions",
    "input",
    "tools",
    "tool_choice",
    "max_output_tokens",
    "temperature",
    "top_p",
    "stream",
];

// ---------------------------------------------------------------------------
// Request in
// ---------------------------------------------------------------------------

pub fn parse_request(body: &Value) -> Result<ChatRequest> {
    let model = req_str_field(body, "model", D)?;
    let mut req = ChatRequest::new(model);
    req.stream = body.get("stream").and_then(Value::as_bool).unwrap_or(false);
    req.max_tokens = opt_u32(body, "max_output_tokens");
    req.temperature = opt_f64(body, "temperature");
    req.top_p = opt_f64(body, "top_p");
    req.system = req_str(body, "instructions").map(str::to_owned);
    req.messages = parse_input(body)?;
    let (tools, raw_tools) = parse_tools(body);
    req.tools = tools;
    req.raw_tools = raw_tools.map(|t| (D, t));
    req.tool_choice = body.get("tool_choice").and_then(parse_tool_choice);
    capture_extra(body, KNOWN_KEYS, D, &mut req)?;
    Ok(req)
}

fn parse_input(body: &Value) -> Result<Vec<Message>> {
    match body.get("input") {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::String(s)) => Ok(vec![Message::text(Role::User, s)]),
        Some(Value::Array(items)) => {
            let mut out: Vec<Message> = Vec::with_capacity(items.len());
            for (i, item) in items.iter().enumerate() {
                let kind = req_str(item, "type").unwrap_or("message");
                match kind {
                    "function_call" => {
                        let call = Part::ToolUse {
                            id: req_str(item, "call_id")
                                .or_else(|| req_str(item, "id"))
                                .unwrap_or_default()
                                .to_owned(),
                            name: req_str(item, "name").unwrap_or_default().to_owned(),
                            input: item
                                .get("arguments")
                                .map(|a| match a {
                                    Value::String(s) => crate::parse_arguments(s),
                                    other => other.clone(),
                                })
                                .unwrap_or_else(|| json!({})),
                        };
                        push_part(&mut out, Role::Assistant, call);
                    }
                    "function_call_output" => {
                        let result = Part::ToolResult {
                            tool_use_id: req_str(item, "call_id").unwrap_or_default().to_owned(),
                            content: match item.get("output") {
                                Some(Value::String(s)) => s.clone(),
                                Some(other) => other.to_string(),
                                None => String::new(),
                            },
                            is_error: false,
                        };
                        push_part(&mut out, Role::Tool, result);
                    }
                    "reasoning" => {
                        let text = item
                            .get("summary")
                            .or_else(|| item.get("content"))
                            .map(summary_text)
                            .unwrap_or_default();
                        let signature = req_str(item, "encrypted_content")
                            .or_else(|| req_str(item, "id"))
                            .map(str::to_owned);
                        push_part(
                            &mut out,
                            Role::Assistant,
                            Part::Reasoning { text, signature },
                        );
                    }
                    "message" => {
                        let role = match req_str(item, "role") {
                            Some("assistant") => Role::Assistant,
                            Some("system") | Some("developer") => Role::System,
                            _ => Role::User,
                        };
                        let mut parts = Vec::new();
                        match item.get("content") {
                            Some(Value::String(s)) => parts.push(Part::Text(s.clone())),
                            Some(Value::Array(blocks)) => {
                                for b in blocks {
                                    match req_str(b, "type").unwrap_or("input_text") {
                                        "input_text" | "output_text" | "text" => {
                                            if let Some(t) = req_str(b, "text") {
                                                parts.push(Part::Text(t.to_owned()));
                                            }
                                        }
                                        "input_image" => {
                                            let url = req_str(b, "image_url").unwrap_or_default();
                                            parts.push(Part::Image {
                                                url: url.to_owned(),
                                                media_type: None,
                                            });
                                        }
                                        other => parts.push(Part::Opaque(json!({"type": other}))),
                                    }
                                }
                            }
                            _ => {}
                        }
                        if role == Role::System {
                            let text = parts
                                .iter()
                                .filter_map(|p| match p {
                                    Part::Text(t) => Some(t.as_str()),
                                    _ => None,
                                })
                                .collect::<Vec<_>>()
                                .join("\n\n");
                            req_system_extend(&mut out, text);
                        } else {
                            out.push(Message { role, parts });
                        }
                    }
                    other => {
                        return Err(parse_err(
                            D,
                            &format!("input[{i}] has unsupported type `{other}`"),
                        ))
                    }
                }
            }
            Ok(out)
        }
        Some(_) => Err(parse_err(D, "`input` must be a string or array")),
    }
}

/// Deterministic system-prompt accumulation without threading a second return
/// value through the item loop: a `System` message is appended in place and
/// `parse_request` folds it into `ChatRequest.system` afterwards.
fn req_system_extend(out: &mut Vec<Message>, text: String) {
    if text.is_empty() {
        return;
    }
    if let Some(Message {
        role: Role::System,
        parts,
    }) = out.last_mut()
    {
        if let Some(Part::Text(t)) = parts.first_mut() {
            t.push_str("\n\n");
            t.push_str(&text);
            return;
        }
    }
    out.push(Message::text(Role::System, text));
}

fn summary_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .filter_map(|i| req_str(i, "text"))
            .collect::<Vec<_>>()
            .join(""),
        other => other.to_string(),
    }
}

fn push_part(out: &mut Vec<Message>, role: Role, part: Part) {
    if let Some(last) = out.last_mut() {
        if last.role == role && !part_breaks_group(&part) {
            last.parts.push(part);
            return;
        }
    }
    out.push(Message {
        role,
        parts: vec![part],
    });
}

/// A tool result and a function call never share a message in Responses.
fn part_breaks_group(part: &Part) -> bool {
    matches!(part, Part::ToolResult { .. } | Part::ToolUse { .. })
}

/// Split the request's `tools` into function tools and provider-native tools.
///
/// Native tools (`web_search`, `file_search`, `code_interpreter`, `custom`, ...) have
/// no canonical form. Dropping them silently changes the request, so they are kept
/// verbatim and re-emitted byte-for-byte when the outbound dialect is Responses.
fn parse_tools(body: &Value) -> (Vec<ToolDef>, Option<Vec<Value>>) {
    let Some(items) = body.get("tools").and_then(Value::as_array) else {
        return (Vec::new(), None);
    };
    let mut out = Vec::with_capacity(items.len());
    let mut raw = Vec::new();
    for item in items {
        let kind = req_str(item, "type").unwrap_or("function");
        if kind != "function" {
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
                .get("parameters")
                .map(tool_schema)
                .unwrap_or_else(|| tool_schema(&Value::Null)),
            strict: item.get("strict").and_then(Value::as_bool).unwrap_or(false),
        });
    }
    (out, (!raw.is_empty()).then_some(raw))
}

fn parse_tool_choice(v: &Value) -> Option<ToolChoice> {
    if let Some(s) = v.as_str() {
        return Some(match s {
            "auto" => ToolChoice::Auto,
            "none" => ToolChoice::None,
            "required" => ToolChoice::Required,
            other => ToolChoice::Tool(other.to_owned()),
        });
    }
    req_str(v, "name")
        .or_else(|| req_str(v, "function").and_then(|_| req_str(v, "name")))
        .map(|n| ToolChoice::Tool(n.to_owned()))
        .or_else(|| match req_str(v, "type") {
            Some("function") => Some(ToolChoice::Required),
            _ => None,
        })
}

// ---------------------------------------------------------------------------
// Request out
// ---------------------------------------------------------------------------

pub fn build_request(req: &ChatRequest, stream: bool, wire: &WireProfile) -> Result<Value> {
    let mut input: Vec<Value> = Vec::with_capacity(req.messages.len());
    // System messages found inside `messages` fold into `instructions`.
    let mut instructions = req.system.clone().unwrap_or_default();
    for msg in &req.messages {
        if msg.role == Role::System {
            let text = msg.joined_text();
            if !text.is_empty() {
                if !instructions.is_empty() {
                    instructions.push_str("\n\n");
                }
                instructions.push_str(&text);
            }
            continue;
        }
        let role = match msg.role {
            Role::Assistant => "assistant",
            _ => "user",
        };
        let content = content_out(&msg.parts, msg.role)?;
        if !content.is_empty() {
            input.push(json!({"role": role, "content": content}));
        }
        for part in &msg.parts {
            match part {
                Part::ToolUse {
                    id,
                    name,
                    input: args,
                } => input.push(json!({
                    "type": "function_call",
                    "call_id": id,
                    "name": name,
                    "arguments": match args {
                        Value::String(s) => s.clone(),
                        other => crate::arguments_value(other)?
                            .as_str()
                            .unwrap_or_default()
                            .to_owned(),
                    },
                })),
                Part::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                } => input.push(json!({
                    "type": "function_call_output",
                    "call_id": tool_use_id,
                    "output": if *is_error { format!("error: {content}") } else { content.clone() },
                })),
                _ => {}
            }
        }
    }

    let mut body = Map::new();
    body.insert("model".into(), Value::from(req.model.clone()));
    body.insert("input".into(), Value::from(input));
    body.insert("stream".into(), Value::Bool(stream));
    // The Responses API only returns a response object when it stored it, and codex's
    // endpoints reject requests that ask for storage ("Store must be set to false").
    // RRouter never stores upstream state, so this is always false.
    body.insert("store".into(), Value::Bool(false));
    if !instructions.is_empty() {
        body.insert("instructions".into(), Value::from(instructions));
    }
    // Optional here: the client's value wins, then the provider's configured default.
    if let Some(n) = wire.max_tokens_opt(req.max_tokens) {
        body.insert("max_output_tokens".into(), Value::from(n));
    }
    if let Some(t) = req.temperature {
        body.insert("temperature".into(), Value::from(t));
    }
    if let Some(p) = req.top_p {
        body.insert("top_p".into(), Value::from(p));
    }
    let mut tools: Vec<Value> = req
        .tools
        .iter()
        .map(|t| {
            let mut m = Map::new();
            m.insert("type".into(), Value::from("function"));
            m.insert("name".into(), Value::from(t.name.clone()));
            if let Some(d) = &t.description {
                m.insert("description".into(), Value::from(d.clone()));
            }
            m.insert("parameters".into(), t.parameters.clone());
            if t.strict {
                m.insert("strict".into(), Value::Bool(true));
            }
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
        let value = match choice {
            ToolChoice::Auto => Value::from("auto"),
            ToolChoice::None => Value::from("none"),
            ToolChoice::Required => Value::from("required"),
            ToolChoice::Tool(name) => json!({"type": "function", "name": name}),
        };
        body.insert("tool_choice".into(), value);
    }
    let mut value = Value::Object(body);
    merge_extra(&mut value, req, D);
    Ok(value)
}

fn content_out(parts: &[Part], role: Role) -> Result<Vec<Value>> {
    let text_kind = if role == Role::Assistant {
        "output_text"
    } else {
        "input_text"
    };
    let mut out = Vec::with_capacity(parts.len());
    for part in parts {
        match part {
            Part::Text(t) => {
                if !t.is_empty() {
                    out.push(json!({"type": text_kind, "text": t}));
                }
            }
            Part::Image { url, .. } => out.push(json!({"type": "input_image", "image_url": url})),
            Part::Opaque(v) => out.push(v.clone()),
            _ => {}
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Response in
// ---------------------------------------------------------------------------

pub fn parse_response(body: &Value) -> Result<ChatResponse> {
    let id = req_str(body, "id").unwrap_or_default().to_owned();
    let model = req_str(body, "model").unwrap_or_default().to_owned();
    let mut parts = Vec::new();
    let mut saw_message = false;
    for item in body
        .get("output")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        match req_str(item, "type") {
            Some("message") => {
                saw_message = true;
                for b in item
                    .get("content")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    match req_str(b, "type") {
                        Some("output_text") | Some("text") => {
                            if let Some(t) = req_str(b, "text") {
                                parts.push(Part::Text(t.to_owned()));
                            }
                        }
                        Some("refusal") => {
                            if let Some(t) = req_str(b, "refusal") {
                                parts.push(Part::Text(t.to_owned()));
                            }
                        }
                        _ => {}
                    }
                }
            }
            Some("function_call") | Some("custom_tool_call") => {
                let args = item
                    .get("arguments")
                    .map(|a| match a {
                        Value::String(s) => crate::parse_arguments(s),
                        other => other.clone(),
                    })
                    .unwrap_or_else(|| json!({}));
                parts.push(Part::ToolUse {
                    id: req_str(item, "call_id")
                        .or_else(|| req_str(item, "id"))
                        .unwrap_or_default()
                        .to_owned(),
                    name: req_str(item, "name").unwrap_or_default().to_owned(),
                    input: args,
                });
            }
            Some("reasoning") => {
                let text = item
                    .get("summary")
                    .or_else(|| item.get("content"))
                    .map(summary_text)
                    .unwrap_or_default();
                let signature = req_str(item, "encrypted_content")
                    .or_else(|| req_str(item, "id"))
                    .map(str::to_owned);
                parts.push(Part::Reasoning { text, signature });
            }
            _ => {}
        }
    }
    if !saw_message {
        if let Some(text) = req_str(body, "output_text") {
            if !text.is_empty() {
                parts.insert(0, Part::Text(text.to_owned()));
            }
        }
    }

    let finish_reason = finish_from_responses(
        req_str(body, "status"),
        body.pointer("/incomplete_details/reason")
            .and_then(Value::as_str),
    );
    let finish_reason = if finish_reason == FinishReason::Stop
        && parts.iter().any(|p| matches!(p, Part::ToolUse { .. }))
    {
        FinishReason::ToolCalls
    } else {
        finish_reason
    };

    Ok(ChatResponse {
        id,
        model,
        parts,
        finish_reason,
        usage: usage_from_responses(body.get("usage")),
    })
}

// ---------------------------------------------------------------------------
// Response out
// ---------------------------------------------------------------------------

pub fn build_response(resp: &ChatResponse) -> Result<Value> {
    // Output items keep canonical part order: consecutive text parts form one
    // message item, and each reasoning/tool part is its own item.
    let mut output: Vec<Value> = Vec::with_capacity(resp.parts.len());
    let mut seq = 0usize;
    let mut text: Option<String> = None;
    let flush_text = |output: &mut Vec<Value>, text: &mut Option<String>, seq: &mut usize| {
        if let Some(t) = text.take() {
            output.push(json!({
                "type": "message",
                "id": format!("msg_{seq}"),
                "role": "assistant",
                "status": "completed",
                "content": [{"type": "output_text", "text": t, "annotations": []}],
            }));
            *seq += 1;
        }
    };

    for part in &resp.parts {
        match part {
            Part::Text(t) => {
                text.get_or_insert_with(String::new).push_str(t);
            }
            Part::Reasoning {
                text: reasoning,
                signature,
            } => {
                flush_text(&mut output, &mut text, &mut seq);
                let mut item = json!({
                    "type": "reasoning",
                    "id": format!("rs_{seq}"),
                    "summary": [{"type": "summary_text", "text": reasoning}],
                });
                if let Some(sig) = signature {
                    item["encrypted_content"] = Value::from(sig.clone());
                }
                output.push(item);
                seq += 1;
            }
            Part::ToolUse { id, name, input } => {
                flush_text(&mut output, &mut text, &mut seq);
                output.push(json!({
                    "type": "function_call",
                    "id": id,
                    "call_id": id,
                    "name": name,
                    "arguments": crate::arguments_value(input)?,
                }));
                seq += 1;
            }
            Part::Image { .. } | Part::ToolResult { .. } | Part::Opaque(_) => {}
        }
    }
    flush_text(&mut output, &mut text, &mut seq);

    let (status, incomplete) = match resp.finish_reason {
        FinishReason::Length => ("incomplete", Some("max_output_tokens")),
        FinishReason::ContentFilter => ("incomplete", Some("content_filter")),
        _ => ("completed", None),
    };
    Ok(json!({
        "id": resp.id,
        "object": "response",
        "created_at": 0,
        "model": resp.model,
        "status": status,
        "incomplete_details": incomplete.map(|r| json!({"reason": r})),
        "output": output,
        "usage": usage_to_responses(resp.usage),
    }))
}

// ---------------------------------------------------------------------------
// Streaming in
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct Pending {
    /// Canonical tool index.
    index: u32,
    id: String,
    name: String,
    started: bool,
}

#[derive(Debug, Default)]
pub struct ResponsesDecoder {
    codec: SseCodec,
    model: String,
    sent_start: bool,
    done: bool,
    id: String,
    /// Item id → canonical tool slot, so args deltas land on the right call.
    items: Vec<(String, Pending)>,
    tool_count: u32,
    usage: Option<Usage>,
    finish: Option<FinishReason>,
}

impl ResponsesDecoder {
    pub fn new(model: &str) -> Self {
        Self {
            model: model.to_owned(),
            ..Self::default()
        }
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
            self.emit_done(out);
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
            "response.created" | "response.in_progress" | "response.output_item.added" => {
                let inner = v.get("response").unwrap_or(&Value::Null);
                if !self.sent_start {
                    self.id = req_str(inner, "id").unwrap_or_default().to_owned();
                    let model = req_str(inner, "model")
                        .map(str::to_owned)
                        .unwrap_or_else(|| self.model.clone());
                    out.push(StreamEvent::Start {
                        id: self.id.clone(),
                        model,
                    });
                    self.sent_start = true;
                }
                if event == "response.output_item.added" {
                    self.item_added(v.get("item").unwrap_or(&Value::Null), out);
                }
            }
            "response.output_text.delta" | "response.refusal.delta" => {
                if let Some(t) = req_str(&v, "delta") {
                    out.push(StreamEvent::TextDelta(t.to_owned()));
                }
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                if let Some(t) = req_str(&v, "delta") {
                    out.push(StreamEvent::ReasoningDelta(t.to_owned()));
                }
            }
            "response.function_call_arguments.delta" | "response.custom_tool_call_input.delta" => {
                let Some(slot) = self.slot(&v) else { return };
                self.ensure_started(slot, out);
                if let Some(t) = req_str(&v, "delta") {
                    out.push(StreamEvent::ToolCallArgsDelta {
                        index: self.items[slot].1.index,
                        partial_json: t.to_owned(),
                    });
                }
            }
            "response.output_item.done" => {
                if let Some(slot) = self.slot(&v) {
                    self.ensure_started(slot, out);
                }
            }
            "response.incomplete" => {
                let inner = v.get("response").unwrap_or(&v);
                self.finish = Some(finish_from_responses(
                    Some("incomplete"),
                    inner
                        .pointer("/incomplete_details/reason")
                        .and_then(Value::as_str),
                ));
                if let Some(u) = inner.get("usage").filter(|u| u.is_object()) {
                    self.usage = Some(usage_from_responses(Some(u)));
                }
            }
            "response.completed" => {
                let inner = v.get("response").unwrap_or(&v);
                if let Some(u) = inner.get("usage").filter(|u| u.is_object()) {
                    self.usage = Some(usage_from_responses(Some(u)));
                }
                self.emit_done(out);
            }
            "response.failed" | "error" => {
                self.finish = Some(FinishReason::Other);
                self.emit_done(out);
            }
            _ => {}
        }
    }

    fn item_added(&mut self, item: &Value, out: &mut Vec<StreamEvent>) {
        match req_str(item, "type") {
            Some("function_call") | Some("custom_tool_call") => {
                let index = self.tool_count;
                self.tool_count += 1;
                let pending = Pending {
                    index,
                    id: req_str(item, "call_id")
                        .or_else(|| req_str(item, "id"))
                        .unwrap_or_default()
                        .to_owned(),
                    name: req_str(item, "name").unwrap_or_default().to_owned(),
                    started: false,
                };
                let item_id = req_str(item, "id").unwrap_or_default().to_owned();
                self.items.push((item_id, pending));
                let slot = self.items.len() - 1;
                self.ensure_started(slot, out);
            }
            Some("reasoning") => {
                // Reasoning summaries arrive as deltas; nothing to emit yet.
            }
            _ => {}
        }
    }

    fn slot(&self, v: &Value) -> Option<usize> {
        let item_id = req_str(v, "item_id")?;
        self.items.iter().position(|(id, _)| id == item_id)
    }

    fn ensure_started(&mut self, slot: usize, out: &mut Vec<StreamEvent>) {
        let Some((_, pending)) = self.items.get_mut(slot) else {
            return;
        };
        if pending.started {
            return;
        }
        pending.started = true;
        out.push(StreamEvent::ToolCallStart {
            index: pending.index,
            id: pending.id.clone(),
            name: pending.name.clone(),
        });
    }

    fn emit_done(&mut self, out: &mut Vec<StreamEvent>) {
        if self.done {
            return;
        }
        if !self.sent_start {
            self.sent_start = true;
            out.push(StreamEvent::Start {
                id: self.id.clone(),
                model: self.model.clone(),
            });
        }
        self.done = true;
        let reason = self.finish.unwrap_or(if self.tool_count > 0 {
            FinishReason::ToolCalls
        } else {
            FinishReason::Stop
        });
        out.push(StreamEvent::Done {
            reason,
            usage: self.usage,
        });
    }
}

// ---------------------------------------------------------------------------
// Streaming out
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct OutItem {
    id: String,
    seq: u32,
    output_index: u32,
}

#[derive(Debug, Default, PartialEq, Clone, Copy)]
enum Open {
    #[default]
    None,
    Text,
    Reasoning,
}

#[derive(Debug, Default)]
pub struct ResponsesEncoder {
    model: String,
    id: String,
    sent_start: bool,
    done: bool,
    seq: u32,
    next_output_index: u32,
    open: Option<Open>,
    open_item: Option<OutItem>,
    tools: Vec<Option<OutItem>>,
}

impl Open {
    fn is_open(self) -> bool {
        self != Open::None
    }
}

impl ResponsesEncoder {
    pub fn new(model: &str) -> Self {
        Self {
            model: model.to_owned(),
            open: None,
            ..Self::default()
        }
    }

    pub fn encode(&mut self, ev: &StreamEvent) -> bytes::Bytes {
        if self.done {
            return bytes::Bytes::new();
        }
        let mut out = String::new();
        if !self.sent_start {
            if let StreamEvent::Start { id, model } = ev {
                self.id = id.clone();
                self.model = model.clone();
            }
            self.sent_start = true;
            self.created(&mut out);
        }
        match ev {
            StreamEvent::Start { id, model } => {
                if self.id != *id || self.model != *model {
                    self.id = id.clone();
                    self.model = model.clone();
                }
            }
            StreamEvent::TextDelta(t) => {
                self.ensure_item(Open::Text, "message", 0, &mut out);
                self.emit(&mut out, "response.output_text.delta", json!({"delta": t}));
                self.seq += 1;
            }
            StreamEvent::ReasoningDelta(t) => {
                self.ensure_item(Open::Reasoning, "reasoning", 0, &mut out);
                self.emit(
                    &mut out,
                    "response.reasoning_summary_text.delta",
                    json!({"delta": t, "summary_index": 0}),
                );
                self.seq += 1;
            }
            StreamEvent::ToolCallStart { index, id, name } => {
                self.close_item(&mut out);
                let slot = *index as usize;
                if self.tools.len() <= slot {
                    self.tools.resize_with(slot + 1, || None);
                }
                if self.tools[slot].is_some() {
                    return bytes::Bytes::from(out);
                }
                let item = OutItem {
                    id: id.clone(),
                    seq: self.seq,
                    output_index: self.next_output_index,
                };
                self.seq += 1;
                self.next_output_index += 1;
                self.tools[slot] = Some(OutItem {
                    id: id.clone(),
                    seq: item.seq,
                    output_index: item.output_index,
                });
                let frame = self.envelope(
                    "response.output_item.added",
                    item.seq,
                    json!({
                        "output_index": item.output_index,
                        "item": {
                            "type": "function_call",
                            "id": id,
                            "call_id": id,
                            "name": name,
                            "arguments": "",
                            "status": "in_progress",
                        },
                    }),
                );
                out.push_str(&render_openai(
                    &frame.to_string(),
                    Some("response.output_item.added"),
                ));
            }
            StreamEvent::ToolCallArgsDelta {
                index,
                partial_json,
            } => {
                let Some(item) = self.tools.get(*index as usize).and_then(Option::as_ref) else {
                    return bytes::Bytes::from(out);
                };
                let frame = self.envelope(
                    "response.function_call_arguments.delta",
                    self.seq,
                    json!({"item_id": item.id, "output_index": item.output_index, "delta": partial_json}),
                );
                self.seq += 1;
                out.push_str(&render_openai(
                    &frame.to_string(),
                    Some("response.function_call_arguments.delta"),
                ));
            }
            StreamEvent::Done { reason, usage } => {
                self.close_item(&mut out);
                for slot in 0..self.tools.len() {
                    if let Some(item) = self.tools[slot].take() {
                        let frame = self.envelope(
                            "response.output_item.done",
                            item.seq,
                            json!({
                                "output_index": item.output_index,
                                "item": {
                                    "type": "function_call",
                                    "id": item.id,
                                    "call_id": item.id,
                                    "status": "completed",
                                },
                            }),
                        );
                        out.push_str(&render_openai(
                            &frame.to_string(),
                            Some("response.output_item.done"),
                        ));
                    }
                }
                let (status, incomplete) = match reason {
                    FinishReason::Length => ("incomplete", Some("max_output_tokens")),
                    FinishReason::ContentFilter => ("incomplete", Some("content_filter")),
                    _ => ("completed", None),
                };
                let usage = usage.map(usage_to_responses);
                let mut response = json!({
                    "id": self.id,
                    "object": "response",
                    "model": self.model,
                    "status": status,
                });
                if let Some(r) = incomplete {
                    response["incomplete_details"] = json!({"reason": r});
                }
                if let Some(u) = usage {
                    response["usage"] = u;
                }
                let event = if status == "completed" {
                    "response.completed"
                } else {
                    "response.incomplete"
                };
                let frame =
                    json!({"type": event, "sequence_number": self.seq, "response": response});
                out.push_str(&render_openai(&frame.to_string(), Some(event)));
                self.done = true;
            }
        }
        bytes::Bytes::from(out)
    }

    fn created(&mut self, out: &mut String) {
        let response = json!({
            "id": self.id,
            "object": "response",
            "model": self.model,
            "status": "in_progress",
            "output": [],
        });
        let frame =
            json!({"type": "response.created", "sequence_number": self.seq, "response": response});
        self.seq += 1;
        out.push_str(&render_openai(&frame.to_string(), Some("response.created")));
    }

    /// Start `kind` if it is not already the open item; closes any other one.
    fn ensure_item(&mut self, kind: Open, item_type: &str, output_index: u32, out: &mut String) {
        if self.open == Some(kind) && self.open_item.is_some() {
            return;
        }
        self.close_item(out);
        let idx = if output_index == 0 {
            self.next_output_index
        } else {
            output_index
        };
        self.next_output_index = self.next_output_index.max(idx + 1);
        let id = format!("{item_type}_{}", self.seq);
        let item = OutItem {
            id: id.clone(),
            seq: self.seq,
            output_index: idx,
        };
        self.seq += 1;
        let wire = match kind {
            Open::Text => json!({
                "type": "message", "id": id, "role": "assistant", "status": "in_progress", "content": [],
            }),
            _ => json!({"type": "reasoning", "id": id, "summary": []}),
        };
        let frame = self.envelope(
            "response.output_item.added",
            item.seq,
            json!({"output_index": item.output_index, "item": wire}),
        );
        out.push_str(&render_openai(
            &frame.to_string(),
            Some("response.output_item.added"),
        ));
        self.open = Some(kind);
        self.open_item = Some(item);
        if kind == Open::Text {
            let Some(item) = self.open_item.as_ref() else {
                return;
            };
            let frame = self.envelope(
                "response.content_part.added",
                self.seq,
                json!({
                    "item_id": item.id,
                    "output_index": item.output_index,
                    "content_index": 0,
                    "part": {"type": "output_text", "text": "", "annotations": []},
                }),
            );
            self.seq += 1;
            out.push_str(&render_openai(
                &frame.to_string(),
                Some("response.content_part.added"),
            ));
        }
    }

    fn close_item(&mut self, out: &mut String) {
        let Some(item) = self.open_item.take() else {
            self.open = None;
            return;
        };
        let kind = self.open.take().unwrap_or(Open::None);
        if kind.is_open() {
            let wire = match kind {
                Open::Text => json!({
                    "type": "message", "id": item.id, "role": "assistant", "status": "completed",
                    "content": [{"type": "output_text", "text": "", "annotations": []}],
                }),
                _ => json!({"type": "reasoning", "id": item.id, "summary": []}),
            };
            let frame = self.envelope(
                "response.output_item.done",
                item.seq,
                json!({"output_index": item.output_index, "item": wire}),
            );
            out.push_str(&render_openai(
                &frame.to_string(),
                Some("response.output_item.done"),
            ));
        }
    }

    fn envelope(&self, ty: &str, seq: u32, body: Value) -> Value {
        let mut m = match body {
            Value::Object(m) => m,
            _ => Map::new(),
        };
        m.insert("type".into(), Value::from(ty));
        m.insert("sequence_number".into(), Value::from(seq));
        Value::Object(m)
    }

    fn emit(&mut self, out: &mut String, event: &str, body: Value) {
        let mut m = match body {
            Value::Object(m) => m,
            _ => Map::new(),
        };
        if let Some(item) = &self.open_item {
            m.insert("item_id".into(), Value::from(item.id.clone()));
            m.insert("output_index".into(), Value::from(item.output_index));
            m.insert("content_index".into(), Value::from(0));
        }
        let frame = self.envelope(event, self.seq, Value::Object(m));
        out.push_str(&render_openai(&frame.to_string(), Some(event)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{build_request, parse_request, StreamEncoder};

    fn sse(event: &str, data: Value) -> String {
        render_openai(&data.to_string(), Some(event))
    }

    #[test]
    fn native_tools_survive_only_the_responses_dialect() {
        let body = json!({
            "model": "m",
            "input": "search the web",
            "tools": [
                {"type": "function", "name": "lookup", "parameters": {"type": "object"}},
                {"type": "web_search", "external_web_access": true},
                {"type": "file_search", "vector_store_ids": ["vs_1"]},
            ],
        });
        let req = parse_request(Dialect::OpenAiResponses, &body).unwrap();
        assert_eq!(req.tools.len(), 1, "only function tools are canonical");
        assert_eq!(req.tools[0].name, "lookup");

        // Same dialect: every native tool is re-emitted verbatim.
        let same = build_request(Dialect::OpenAiResponses, &req, false).unwrap();
        let tools = same["tools"].as_array().expect("tools");
        assert_eq!(tools.len(), 3);
        assert_eq!(
            tools[1],
            json!({"type": "web_search", "external_web_access": true})
        );
        assert_eq!(
            tools[2],
            json!({"type": "file_search", "vector_store_ids": ["vs_1"]})
        );

        // Different dialect: nothing the target cannot express is smuggled through.
        let chat = build_request(Dialect::OpenAiChat, &req, false).unwrap();
        assert_eq!(chat["tools"].as_array().expect("tools").len(), 1);
    }

    #[test]
    fn strict_schema_flag_is_preserved() {
        let body = json!({
            "model": "m",
            "input": "hi",
            "tools": [{"type": "function", "name": "f", "strict": true,
                       "parameters": {"type": "object", "properties": {}}}],
        });
        let req = parse_request(Dialect::OpenAiResponses, &body).unwrap();
        assert!(req.tools[0].strict);
        let same = build_request(Dialect::OpenAiResponses, &req, false).unwrap();
        assert_eq!(same["tools"][0]["strict"], true);
        let chat = build_request(Dialect::OpenAiChat, &req, false).unwrap();
        assert_eq!(chat["tools"][0]["function"]["strict"], true);
    }

    #[test]
    fn instructions_and_function_call_items_parse() {
        let body = json!({
            "model": "",
            "instructions": "be terse",
            "input": [
                {"role": "user", "content": [{"type": "input_text", "text": "weather?"}]},
                {"type": "function_call", "call_id": "call_1", "name": "get_weather", "arguments": "{\"city\":\"Hanoi\"}"},
                {"type": "function_call_output", "call_id": "call_1", "output": "21C"},
            ],
            "tools": [{"type": "function", "name": "get_weather", "parameters": {"type": "object"}}],
        });
        let req = parse_request(D, &body).unwrap();
        assert_eq!(req.system.as_deref(), Some("be terse"));
        assert_eq!(req.tools[0].name, "get_weather");
        let call = req
            .messages
            .iter()
            .flat_map(|m| &m.parts)
            .find_map(|p| match p {
                Part::ToolUse { id, name, input } => {
                    Some((id.clone(), name.clone(), input.clone()))
                }
                _ => None,
            })
            .expect("function_call item");
        assert_eq!(
            call,
            (
                "call_1".into(),
                "get_weather".into(),
                json!({"city": "Hanoi"})
            )
        );
        let result = req
            .messages
            .iter()
            .flat_map(|m| &m.parts)
            .find_map(|p| match p {
                Part::ToolResult {
                    tool_use_id,
                    content,
                    ..
                } => Some((tool_use_id.clone(), content.clone())),
                _ => None,
            })
            .expect("function_call_output item");
        assert_eq!(result, ("call_1".into(), "21C".into()));

        let built = build_request(D, &req, true).unwrap();
        assert_eq!(built["stream"], true);
        assert_eq!(built["instructions"], "be terse");
        let kinds: Vec<&str> = built["input"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i.get("type").and_then(Value::as_str).unwrap_or("message"))
            .collect();
        assert_eq!(
            kinds,
            vec!["message", "function_call", "function_call_output"]
        );
        assert!(built["input"][1]["arguments"].is_string());
    }

    #[test]
    fn reasoning_items_round_trip_signature() {
        let body = json!({
            "id": "resp_1",
            "model": "",
            "status": "completed",
            "output": [
                {"type": "reasoning", "id": "rs_1", "summary": [{"type": "summary_text", "text": "thought"}], "encrypted_content": "enc-9"},
                {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "done"}]},
            ],
            "usage": {"input_tokens": 20, "output_tokens": 5, "input_tokens_details": {"cached_tokens": 8}},
        });
        let resp = parse_response(&body).unwrap();
        assert_eq!(
            resp.parts[0],
            Part::Reasoning {
                text: "thought".into(),
                signature: Some("enc-9".into())
            }
        );
        assert_eq!(resp.parts[1], Part::Text("done".into()));
        assert_eq!(
            resp.usage,
            Usage {
                input_tokens: 20,
                output_tokens: 5,
                cache_read_tokens: 8,
                cache_write_tokens: 0
            }
        );
        assert_eq!(resp.finish_reason, FinishReason::Stop);

        let out = build_response(&resp).unwrap();
        assert_eq!(out["output"][0]["encrypted_content"], "enc-9");
        assert_eq!(out["output"][1]["content"][0]["type"], "output_text");
    }

    #[test]
    fn incomplete_max_output_tokens_maps_to_length() {
        let body = json!({
            "id": "r",
            "model": "",
            "status": "incomplete",
            "incomplete_details": {"reason": "max_output_tokens"},
            "output": [{"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "partial"}]}],
        });
        assert_eq!(
            parse_response(&body).unwrap().finish_reason,
            FinishReason::Length
        );
        let out = build_response(&parse_response(&body).unwrap()).unwrap();
        assert_eq!(out["status"], "incomplete");
        assert_eq!(out["incomplete_details"]["reason"], "max_output_tokens");
    }

    #[test]
    fn stream_decodes_text_and_function_args_then_completes_once() {
        let wire = [
            sse("response.created", json!({"type": "response.created", "response": {"id": "resp_2", "model": "m"}})),
            sse("response.output_text.delta", json!({"type": "response.output_text.delta", "delta": "Hel"})),
            sse("response.output_text.delta", json!({"type": "response.output_text.delta", "delta": "lo"})),
            sse("response.output_item.added", json!({"type": "response.output_item.added", "output_index": 1,
                "item": {"type": "function_call", "id": "fc_1", "call_id": "call_9", "name": "f", "arguments": ""}})),
            sse("response.function_call_arguments.delta", json!({"type": "response.function_call_arguments.delta", "item_id": "fc_1", "delta": "{\"x\":"})),
            sse("response.function_call_arguments.delta", json!({"type": "response.function_call_arguments.delta", "item_id": "fc_1", "delta": "7}"})),
            sse("response.completed", json!({"type": "response.completed", "response": {"id": "resp_2",
                "usage": {"input_tokens": 4, "output_tokens": 9, "input_tokens_details": {"cached_tokens": 1}}}})),
        ]
        .concat();

        let mut dec = ResponsesDecoder::new("m");
        let mut events = Vec::new();
        for chunk in wire.as_bytes().chunks(7) {
            dec.push(chunk, &mut events).unwrap();
        }
        dec.finish(&mut events).unwrap();

        let text: String = events
            .iter()
            .filter_map(|e| match e {
                StreamEvent::TextDelta(t) => Some(t.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(text, "Hello");
        let json: String = events
            .iter()
            .filter_map(|e| match e {
                StreamEvent::ToolCallArgsDelta { partial_json, .. } => Some(partial_json.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(json, "{\"x\":7}");
        assert!(events.contains(&StreamEvent::ToolCallStart {
            index: 0,
            id: "call_9".into(),
            name: "f".into()
        }));
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
                    input_tokens: 4,
                    output_tokens: 9,
                    cache_read_tokens: 1,
                    cache_write_tokens: 0
                }),
            }
        );
    }

    #[test]
    fn encoder_emits_required_event_names_and_completed_with_usage() {
        let mut enc = StreamEncoder::new(D, "");
        let mut wire = String::new();
        for ev in [
            StreamEvent::Start {
                id: "resp_3".into(),
                model: "".into(),
            },
            StreamEvent::TextDelta("hi".into()),
            StreamEvent::ToolCallStart {
                index: 0,
                id: "call_1".into(),
                name: "f".into(),
            },
            StreamEvent::ToolCallArgsDelta {
                index: 0,
                partial_json: "{}".into(),
            },
            StreamEvent::Done {
                reason: FinishReason::Stop,
                usage: Some(Usage {
                    input_tokens: 2,
                    output_tokens: 3,
                    cache_read_tokens: 0,
                    cache_write_tokens: 0,
                }),
            },
        ] {
            wire.push_str(std::str::from_utf8(&enc.encode(&ev).unwrap()).unwrap());
        }
        for required in [
            "event: response.created",
            "event: response.output_item.added",
            "event: response.output_text.delta",
            "event: response.function_call_arguments.delta",
            "event: response.completed",
        ] {
            assert!(wire.contains(required), "missing {required} in:\n{wire}");
        }
        assert_eq!(wire.matches("event: response.completed").count(), 1);
        assert!(wire.contains("\"usage\":{\"input_tokens\":2"));
        assert!(wire.contains("\"output_tokens\":3"));
    }
}
