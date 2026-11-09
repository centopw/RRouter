//! Google Code Assist (Cloud Code Assist): the upstream wire the Antigravity desktop
//! client speaks.
//!
//! This is a **provider dialect**, not an inbound one. A client still talks OpenAI Chat,
//! Anthropic Messages or Responses; the gateway translates that canonical request into
//! the envelope below and translates the answer back. [`Dialect::GoogleCodeAssist`] exists
//! so the proxy can name the upstream grammar without exposing a route.
//!
//! ```json
//! {
//!   "model": "gemini-3-pro",
//!   "userAgent": "antigravity",
//!   "requestType": "agent",
//!   "project": "<cloud project id>",
//!   "requestId": "agent-<uuid>",
//!   "request": {
//!     "contents": [{"role": "user", "parts": [{"text": "hi"}]}],
//!     "systemInstruction": {"parts": [{"text": "…"}]},
//!     "generationConfig": {"maxOutputTokens": 4096}
//!   }
//! }
//! ```
//!
//! `request` deliberately carries no `model` and no `safetySettings`: neither belongs in
//! the inner Gemini request, and the Antigravity backend rejects a nested model.
//!
//! # Responses
//!
//! Every buffered body and every SSE `data:` payload wraps the Gemini response:
//! `{"response": {"candidates": …, "usageMetadata": …}, "traceId": "…"}`. Both readers
//! unwrap `response` and also accept a bare Gemini body, so a mock or a future server that
//! drops the wrapper still works.
//!
//! # Streams
//!
//! `streamGenerateContent?alt=sse` emits `data: {json}` frames with no `event:` name.
//! Text, reasoning (`thought: true`), function calls and usage all arrive incrementally;
//! the decoder folds them into the canonical [`StreamEvent`] vocabulary.

use serde_json::{json, Map, Value};
use uuid::Uuid;

use rrouter_core::sse::{SseCodec, SseFrame};
use rrouter_core::{
    ChatRequest, ChatResponse, Error, FinishReason, Message, Part, Result, Role, StreamEvent,
    ToolChoice, Usage,
};

/// Daily build host. Antigravity desktop points here first.
pub const DAILY_HOST: &str = "https://daily-cloudcode-pa.googleapis.com";
/// Production fallback host, used when the daily build transport-fails or answers 5xx.
pub const PROD_HOST: &str = "https://cloudcode-pa.googleapis.com";
/// Streaming path, with the SSE selector the endpoint requires.
pub const STREAM_PATH: &str = "/v1internal:streamGenerateContent?alt=sse";
/// Buffered path.
pub const NON_STREAM_PATH: &str = "/v1internal:generateContent";
/// `userAgent` in the envelope and `User-Agent` on the wire.
pub const USER_AGENT: &str = "antigravity";
/// `X-Goog-Api-Client` value the Antigravity desktop sends.
pub const CLIENT_HEADER: &str = "google-cloud-sdk vscode_cloudshelleditor/0.1";
/// Env override for the daily host (a deployment pin or a test server).
pub const ENV_DAILY_HOST: &str = "RROUTER_ANTIGRAVITY_DAILY_HOST";
/// Env override for the production fallback host.
pub const ENV_PROD_HOST: &str = "RROUTER_ANTIGRAVITY_PROD_HOST";

/// Daily host: the env override wins, then the provider's configured base URL.
pub fn daily_host(configured_base: &str) -> String {
    env_host(ENV_DAILY_HOST).unwrap_or_else(|| configured_base.trim_end_matches('/').to_string())
}

/// Production fallback host: the env override wins, else [`PROD_HOST`].
pub fn prod_host() -> String {
    env_host(ENV_PROD_HOST).unwrap_or_else(|| PROD_HOST.to_string())
}

fn env_host(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
}

/// Full URL for one CCA call. `base` is an origin, not a path.
pub fn endpoint(base: &str, stream: bool) -> String {
    let mut url = String::with_capacity(base.trim_end_matches('/').len() + NON_STREAM_PATH.len());
    url.push_str(base.trim_end_matches('/'));
    url.push_str(if stream { STREAM_PATH } else { NON_STREAM_PATH });
    url
}

/// Build one envelope with a fresh `agent-<uuid>` request id.
///
/// `model` is the resolved model id (the candidate's), and `project` is the credential's
/// `project_id`. An empty project is a client-visible configuration error, not a panic.
pub fn build_request(req: &ChatRequest, model: &str, project: &str) -> Result<Value> {
    let request_id = format!("agent-{}", Uuid::new_v4());
    build_request_with(req, model, project, &request_id)
}

/// [`build_request`] with a caller-supplied request id, for retries that must stay
/// idempotent and for deterministic tests.
pub fn build_request_with(
    req: &ChatRequest,
    model: &str,
    project: &str,
    request_id: &str,
) -> Result<Value> {
    let project = project.trim();
    if project.is_empty() {
        return Err(Error::BadRequest(
            "a google-antigravity connection needs credentials.extra.project_id".into(),
        ));
    }
    if model.trim().is_empty() {
        return Err(Error::BadRequest("a model id is required".into()));
    }

    let mut system_parts: Vec<Value> = Vec::new();
    if let Some(system) = req.system.as_deref().filter(|s| !s.is_empty()) {
        system_parts.push(json!({ "text": system }));
    }
    let contents = contents_in(&req.messages, &mut system_parts)?;

    let mut request = Map::new();
    if !contents.is_empty() {
        request.insert("contents".into(), Value::Array(contents));
    }
    if !system_parts.is_empty() {
        request.insert("systemInstruction".into(), json!({ "parts": system_parts }));
    }
    let generation = generation_config(req);
    if !generation.is_empty() {
        request.insert("generationConfig".into(), Value::Object(generation));
    }
    if !req.tools.is_empty() {
        let declarations: Vec<Value> = req
            .tools
            .iter()
            .map(|tool| {
                let mut declaration = Map::new();
                declaration.insert("name".into(), Value::from(tool.name.clone()));
                if let Some(description) = tool.description.as_deref().filter(|d| !d.is_empty()) {
                    declaration.insert("description".into(), Value::from(description.to_string()));
                }
                declaration.insert(
                    "parameters".into(),
                    if tool.parameters.is_null() {
                        json!({"type": "object", "properties": {}})
                    } else {
                        tool.parameters.clone()
                    },
                );
                Value::Object(declaration)
            })
            .collect();
        request.insert(
            "tools".into(),
            json!([{"functionDeclarations": declarations}]),
        );
    }
    if let Some(choice) = &req.tool_choice {
        let mut config = Map::new();
        match choice {
            ToolChoice::Auto => {
                config.insert("mode".into(), Value::from("AUTO"));
            }
            ToolChoice::None => {
                config.insert("mode".into(), Value::from("NONE"));
            }
            ToolChoice::Required => {
                config.insert("mode".into(), Value::from("ANY"));
            }
            ToolChoice::Tool(name) => {
                config.insert("mode".into(), Value::from("ANY"));
                config.insert(
                    "allowedFunctionNames".into(),
                    Value::Array(vec![Value::from(name.clone())]),
                );
            }
        }
        request.insert(
            "toolConfig".into(),
            json!({"functionCallingConfig": Value::Object(config)}),
        );
    }

    let mut envelope = Map::new();
    envelope.insert("model".into(), Value::from(model.to_string()));
    envelope.insert("userAgent".into(), Value::from(USER_AGENT));
    envelope.insert("requestType".into(), Value::from("agent"));
    envelope.insert("project".into(), Value::from(project.to_string()));
    envelope.insert("requestId".into(), Value::from(request_id.to_string()));
    if let Some(session_id) = session_id(req) {
        envelope.insert("sessionId".into(), Value::from(session_id));
    }
    envelope.insert("request".into(), Value::Object(request));
    Ok(Value::Object(envelope))
}

fn generation_config(req: &ChatRequest) -> Map<String, Value> {
    let mut config = Map::new();
    if let Some(max_tokens) = req.max_tokens {
        config.insert("maxOutputTokens".into(), Value::from(max_tokens));
    }
    if let Some(temperature) = req.temperature {
        config.insert("temperature".into(), Value::from(temperature));
    }
    if let Some(top_p) = req.top_p {
        config.insert("topP".into(), Value::from(top_p));
    }
    if !req.stop.is_empty() {
        config.insert("stopSequences".into(), json!(req.stop));
    }
    config
}

/// Stable conversation id derived from the system prompt and the first user turn.
///
/// The Antigravity envelope treats `sessionId` as a conversation handle; nothing in the
/// canonical request carries one, so it is derived from the part of the conversation that
/// does not change between turns. Requests with neither field omit it.
fn session_id(req: &ChatRequest) -> Option<String> {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut wrote = false;
    let feed = |text: &str, hash: &mut u64, wrote: &mut bool| {
        if text.is_empty() {
            return;
        }
        *wrote = true;
        for byte in text.as_bytes() {
            *hash ^= u64::from(*byte);
            *hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        // A separator keeps ("ab","c") and ("a","bc") distinct.
        *hash ^= 0xff;
        *hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    };
    if let Some(system) = req.system.as_deref() {
        feed(system, &mut hash, &mut wrote);
    }
    if let Some(first_user) = req
        .messages
        .iter()
        .find(|message| message.role == Role::User)
    {
        feed(&first_user.joined_text(), &mut hash, &mut wrote);
    }
    wrote.then(|| format!("{hash:016x}"))
}

fn contents_in(messages: &[Message], system_parts: &mut Vec<Value>) -> Result<Vec<Value>> {
    let tool_names = tool_names(messages);
    let mut contents: Vec<Value> = Vec::with_capacity(messages.len());
    for message in messages {
        if message.role == Role::System {
            let text = message.joined_text();
            if !text.is_empty() {
                system_parts.push(json!({ "text": text }));
            }
            continue;
        }
        let role = match message.role {
            Role::Assistant => "model",
            Role::User | Role::Tool => "user",
            Role::System => unreachable!("handled above"),
        };
        let mut parts: Vec<Value> = Vec::with_capacity(message.parts.len());
        for part in &message.parts {
            parts.push(part_in(part, &tool_names)?);
        }
        parts.retain(|part| !part.is_null());
        if parts.is_empty() {
            continue;
        }
        if let Some(last) = contents.last_mut() {
            if last.get("role").and_then(Value::as_str) == Some(role) {
                if let Some(existing) = last.get_mut("parts").and_then(Value::as_array_mut) {
                    existing.extend(parts);
                    continue;
                }
            }
        }
        contents.push(json!({ "role": role, "parts": parts }));
    }
    Ok(contents)
}

fn tool_names(messages: &[Message]) -> std::collections::HashMap<&str, &str> {
    let mut names = std::collections::HashMap::new();
    for message in messages {
        for part in &message.parts {
            if let Part::ToolUse { id, name, .. } = part {
                names.insert(id.as_str(), name.as_str());
            }
        }
    }
    names
}

fn part_in(part: &Part, tool_names: &std::collections::HashMap<&str, &str>) -> Result<Value> {
    Ok(match part {
        Part::Text(text) => {
            if text.is_empty() {
                Value::Null
            } else {
                json!({ "text": text })
            }
        }
        Part::Image { url, media_type } => image_part(url, media_type),
        Part::ToolUse { name, input, .. } => json!({
            "functionCall": {"name": name, "args": if input.is_null() { json!({}) } else { input.clone() }},
        }),
        Part::ToolResult {
            tool_use_id,
            content,
            is_error,
        } => {
            let name = tool_names
                .get(tool_use_id.as_str())
                .copied()
                .unwrap_or(tool_use_id.as_str());
            let response = if *is_error {
                json!({"error": content})
            } else {
                json!({"result": content})
            };
            json!({
                "functionResponse": {"name": name, "response": response},
            })
        }
        Part::Reasoning { text, signature } => {
            if text.is_empty() {
                Value::Null
            } else {
                let mut block = Map::new();
                block.insert("text".into(), Value::from(text.clone()));
                block.insert("thought".into(), Value::Bool(true));
                if let Some(signature) = signature {
                    block.insert("thoughtSignature".into(), Value::from(signature.clone()));
                }
                Value::Object(block)
            }
        }
        Part::Opaque(value) => value.clone(),
    })
}

fn image_part(url: &str, media_type: &Option<String>) -> Value {
    if let Some(rest) = url.strip_prefix("data:") {
        if let Some((meta, data)) = rest.split_once(',') {
            let mime = meta
                .split(';')
                .next()
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .or_else(|| media_type.clone())
                .unwrap_or_else(|| "image/png".to_string());
            return json!({"inlineData": {"mimeType": mime, "data": data}});
        }
    }
    let mut file = Map::new();
    file.insert("fileUri".into(), Value::from(url.to_string()));
    if let Some(mime) = media_type.as_deref().filter(|value| !value.is_empty()) {
        file.insert("mimeType".into(), Value::from(mime.to_string()));
    }
    json!({ "fileData": Value::Object(file) })
}

// ---------------------------------------------------------------------------
// Response in
// ---------------------------------------------------------------------------

/// The Gemini payload: the `response` member when the CCA wrapper is present, else the
/// body itself.
fn payload(body: &Value) -> &Value {
    body.get("response")
        .filter(|value| value.is_object())
        .unwrap_or(body)
}

fn response_id(body: &Value, payload: &Value) -> String {
    if let Some(id) = payload.get("responseId").and_then(Value::as_str) {
        return id.to_string();
    }
    body.get("traceId")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn response_model(payload: &Value) -> String {
    payload
        .get("modelVersion")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn candidate(body: &Value) -> Option<&Value> {
    payload(body)
        .get("candidates")
        .and_then(Value::as_array)
        .and_then(|candidates| candidates.first())
}

/// Parse one buffered CCA response into the canonical form.
pub fn parse_response(body: &Value) -> Result<ChatResponse> {
    let payload = payload(body);
    if let Some(error) = payload.get("error") {
        return Err(error_from_value(error));
    }
    let candidate = candidate(body);
    let mut tool_index = 0;
    let parts = candidate
        .map(|candidate| parts_from_candidate(candidate, &mut tool_index))
        .unwrap_or_default();
    let saw_tool = parts
        .iter()
        .any(|part| matches!(part, Part::ToolUse { .. }));
    let finish_reason = match candidate
        .and_then(|c| c.get("finishReason"))
        .and_then(Value::as_str)
    {
        Some(reason) => finish_from_gemini(reason, saw_tool),
        None if saw_tool => FinishReason::ToolCalls,
        None => {
            if payload.get("promptFeedback").is_some() {
                FinishReason::ContentFilter
            } else {
                FinishReason::Stop
            }
        }
    };
    Ok(ChatResponse {
        id: response_id(body, payload),
        model: response_model(payload),
        parts,
        finish_reason,
        usage: usage_from(payload.get("usageMetadata")),
    })
}

fn parts_from_candidate(candidate: &Value, tool_index: &mut u32) -> Vec<Part> {
    let Some(blocks) = candidate
        .pointer("/content/parts")
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    let mut parts = Vec::with_capacity(blocks.len());
    for block in blocks {
        if let Some(text) = block.get("text").and_then(Value::as_str) {
            if text.is_empty() {
                continue;
            }
            if block.get("thought").and_then(Value::as_bool) == Some(true) {
                parts.push(Part::Reasoning {
                    text: text.to_string(),
                    signature: block
                        .get("thoughtSignature")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                });
            } else {
                parts.push(Part::Text(text.to_string()));
            }
            continue;
        }
        if let Some(call) = block.get("functionCall") {
            let index = *tool_index;
            *tool_index += 1;
            parts.push(Part::ToolUse {
                id: format!("call_{index}"),
                name: call
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                input: call.get("args").cloned().unwrap_or_else(|| json!({})),
            });
            continue;
        }
        parts.push(Part::Opaque(block.clone()));
    }
    parts
}

fn finish_from_gemini(reason: &str, saw_tool: bool) -> FinishReason {
    match reason {
        "STOP" if saw_tool => FinishReason::ToolCalls,
        "STOP" => FinishReason::Stop,
        "MAX_TOKENS" => FinishReason::Length,
        "SAFETY" | "RECITATION" | "BLOCKLIST" | "PROHIBITED_CONTENT" | "SPII" => {
            FinishReason::ContentFilter
        }
        _ => FinishReason::Other,
    }
}

fn usage_from(usage: Option<&Value>) -> Usage {
    let Some(usage) = usage else {
        return Usage::default();
    };
    let count = |key: &str| {
        usage
            .get(key)
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .unwrap_or(0)
    };
    Usage {
        input_tokens: count("promptTokenCount"),
        output_tokens: count("candidatesTokenCount").saturating_add(count("thoughtsTokenCount")),
        cache_read_tokens: count("cachedContentTokenCount"),
        cache_write_tokens: 0,
    }
}

fn error_from_value(error: &Value) -> Error {
    if let Some(message) = error.as_str() {
        return Error::Translation(format!("google code assist: {message}"));
    }
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .or_else(|| error.get("status").and_then(Value::as_str))
        .unwrap_or("upstream error");
    Error::Translation(format!("google code assist: {message}"))
}

// ---------------------------------------------------------------------------
// Streaming in
// ---------------------------------------------------------------------------

/// Incremental decoder for `streamGenerateContent?alt=sse`.
#[derive(Debug, Default)]
pub struct GoogleCcaDecoder {
    codec: SseCodec,
    model: String,
    id: String,
    sent_start: bool,
    done: bool,
    usage: Option<Usage>,
    finish: Option<FinishReason>,
    tool_index: u32,
    saw_tool: bool,
}

impl GoogleCcaDecoder {
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
        if self.done {
            return Ok(());
        }
        if frame.is_done() {
            self.emit_done(out);
            return Ok(());
        }
        let Ok(value) = serde_json::from_str::<Value>(&frame.data) else {
            return Ok(()); // keep-alive or non-JSON payload
        };
        if let Some(error) = value
            .get("error")
            .or_else(|| value.get("response").and_then(|r| r.get("error")))
        {
            return Err(error_from_value(error));
        }
        self.chunk(&value, out);
        Ok(())
    }

    fn chunk(&mut self, body: &Value, out: &mut Vec<StreamEvent>) {
        let payload = payload(body);
        if !self.sent_start {
            self.sent_start = true;
            self.id = response_id(body, payload);
            let model = response_model(payload);
            if !model.is_empty() {
                self.model = model;
            }
            out.push(StreamEvent::Start {
                id: self.id.clone(),
                model: self.model.clone(),
            });
        }
        if let Some(usage) = payload.get("usageMetadata") {
            self.usage = Some(usage_from(Some(usage)));
        }
        if let Some(candidate) = payload
            .get("candidates")
            .and_then(Value::as_array)
            .and_then(|candidates| candidates.first())
        {
            self.candidate(candidate, out);
        }
    }

    fn candidate(&mut self, candidate: &Value, out: &mut Vec<StreamEvent>) {
        if let Some(blocks) = candidate
            .pointer("/content/parts")
            .and_then(Value::as_array)
        {
            for block in blocks {
                if let Some(text) = block.get("text").and_then(Value::as_str) {
                    if text.is_empty() {
                        continue;
                    }
                    if block.get("thought").and_then(Value::as_bool) == Some(true) {
                        out.push(StreamEvent::ReasoningDelta(text.to_string()));
                    } else {
                        out.push(StreamEvent::TextDelta(text.to_string()));
                    }
                    continue;
                }
                if let Some(call) = block.get("functionCall") {
                    let index = self.tool_index;
                    self.tool_index += 1;
                    let name = call
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let args = call.get("args").cloned().unwrap_or_else(|| json!({}));
                    out.push(StreamEvent::ToolCallStart {
                        index,
                        id: format!("call_{index}"),
                        name,
                    });
                    let partial = serde_json::to_string(&args).unwrap_or_else(|_| "{}".into());
                    out.push(StreamEvent::ToolCallArgsDelta {
                        index,
                        partial_json: partial,
                    });
                    self.saw_tool = true;
                }
            }
        }
        if let Some(reason) = candidate.get("finishReason").and_then(Value::as_str) {
            self.finish = Some(finish_from_gemini(reason, self.saw_tool));
        }
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
        let reason = self.finish.unwrap_or(if self.saw_tool {
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
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use rrouter_core::{Dialect, Message, Part, ToolDef};

    fn request() -> ChatRequest {
        let mut req = ChatRequest::new("gemini-3-pro");
        req.system = Some("Be brief.".into());
        req.messages = vec![
            Message::text(Role::User, "weather in Paris?"),
            Message {
                role: Role::Assistant,
                parts: vec![Part::ToolUse {
                    id: "call_0".into(),
                    name: "get_weather".into(),
                    input: json!({"city": "Paris"}),
                }],
            },
            Message {
                role: Role::Tool,
                parts: vec![Part::ToolResult {
                    tool_use_id: "call_0".into(),
                    content: "18C".into(),
                    is_error: false,
                }],
            },
        ];
        req.tools = vec![ToolDef {
            name: "get_weather".into(),
            description: Some("Look up weather".into()),
            parameters: json!({"type": "object", "properties": {"city": {"type": "string"}}}),
            strict: false,
        }];
        req.tool_choice = Some(ToolChoice::Tool("get_weather".into()));
        req.max_tokens = Some(1024);
        req.temperature = Some(0.2);
        req
    }

    #[test]
    fn builds_the_antigravity_envelope() {
        let envelope =
            build_request_with(&request(), "gemini-3-pro", "proj-1", "agent-test").unwrap();
        assert_eq!(envelope["model"], "gemini-3-pro");
        assert_eq!(envelope["userAgent"], "antigravity");
        assert_eq!(envelope["requestType"], "agent");
        assert_eq!(envelope["project"], "proj-1");
        assert_eq!(envelope["requestId"], "agent-test");
        let inner = &envelope["request"];
        assert!(inner.get("model").is_none(), "model is top-level only");
        assert!(
            inner.get("safetySettings").is_none(),
            "CCA must not send safetySettings"
        );
        assert_eq!(inner["contents"][0]["role"], "user");
        assert_eq!(
            inner["contents"][0]["parts"][0]["text"],
            "weather in Paris?"
        );
        assert_eq!(inner["contents"][1]["role"], "model");
        assert_eq!(
            inner["contents"][1]["parts"][0]["functionCall"]["name"],
            "get_weather"
        );
        assert_eq!(
            inner["contents"][2]["parts"][0]["functionResponse"]["name"], "get_weather",
            "a tool result is named from the assistant's call"
        );
        assert_eq!(inner["systemInstruction"]["parts"][0]["text"], "Be brief.");
        assert_eq!(inner["generationConfig"]["maxOutputTokens"], 1024);
        assert_eq!(inner["generationConfig"]["temperature"], 0.2);
        assert_eq!(
            inner["tools"][0]["functionDeclarations"][0]["name"],
            "get_weather"
        );
        assert_eq!(
            inner["toolConfig"]["functionCallingConfig"]["allowedFunctionNames"][0],
            "get_weather"
        );
        assert!(
            envelope["sessionId"].is_string(),
            "a conversation id is stable"
        );
        assert_eq!(
            build_request_with(&request(), "gemini-3-pro", "proj-1", "agent-test").unwrap()
                ["sessionId"],
            envelope["sessionId"]
        );
    }

    #[test]
    fn an_empty_project_is_a_client_error() {
        let err = build_request_with(&request(), "m", "  ", "agent-test").unwrap_err();
        assert!(matches!(err, Error::BadRequest(_)), "{err}");
    }

    #[test]
    fn parses_a_wrapped_response_with_usage_and_tools() {
        let body: Value =
            serde_json::from_str(include_str!("../tests/fixtures/google_cca_response.json"))
                .expect("fixture");
        let response = parse_response(&body).unwrap();
        assert_eq!(response.id, "resp-fixture-1");
        assert_eq!(response.model, "gemini-3-pro");
        assert_eq!(response.finish_reason, FinishReason::ToolCalls);
        assert_eq!(response.usage.input_tokens, 10);
        assert_eq!(response.usage.output_tokens, 25);
        assert_eq!(response.usage.cache_read_tokens, 3);
        assert!(matches!(&response.parts[0], Part::Reasoning { text, .. } if text == "think"));
        assert!(matches!(&response.parts[1], Part::Text(text) if text == "18C"));
        assert!(matches!(
            &response.parts[2],
            Part::ToolUse { name, input, .. } if name == "get_weather" && input["city"] == "Paris"
        ));
    }

    #[test]
    fn accepts_a_bare_gemini_body() {
        let body = json!({
            "candidates": [{"content": {"parts": [{"text": "hi"}]}, "finishReason": "MAX_TOKENS"}]
        });
        let response = parse_response(&body).unwrap();
        assert_eq!(response.finish_reason, FinishReason::Length);
        assert!(matches!(&response.parts[0], Part::Text(text) if text == "hi"));
    }

    #[test]
    fn decodes_sse_with_fragmented_frames() {
        let wire = include_str!("../tests/fixtures/google_cca_stream.sse");

        let decode = |chunk: usize| {
            let mut decoder = GoogleCcaDecoder::new("gemini-3-pro");
            let mut events = Vec::new();
            for piece in wire.as_bytes().chunks(chunk) {
                decoder.push(piece, &mut events).unwrap();
            }
            decoder.finish(&mut events).unwrap();
            events
        };
        let events = decode(5);
        assert_eq!(
            events,
            vec![
                StreamEvent::Start {
                    id: "r".into(),
                    model: "gemini-3-pro".into()
                },
                StreamEvent::TextDelta("Hel".into()),
                StreamEvent::TextDelta("lo".into()),
                StreamEvent::Done {
                    reason: FinishReason::Stop,
                    usage: Some(Usage {
                        input_tokens: 7,
                        output_tokens: 2,
                        ..Usage::default()
                    })
                },
            ]
        );
        assert_eq!(
            events,
            decode(wire.len()),
            "chunk boundaries cannot change events"
        );
    }

    #[test]
    fn decodes_streamed_function_calls() {
        let frame = format!(
            "data: {}\n\n",
            json!({"response": {"candidates": [{
                "content": {"parts": [{"functionCall": {"name": "get_weather", "args": {"city": "Paris"}}}]},
                "finishReason": "STOP"
            }]}})
        );
        let mut decoder = GoogleCcaDecoder::new("m");
        let mut events = Vec::new();
        decoder.push(frame.as_bytes(), &mut events).unwrap();
        decoder.finish(&mut events).unwrap();
        assert!(matches!(
            &events[1],
            StreamEvent::ToolCallStart { index: 0, name, .. } if name == "get_weather"
        ));
        assert!(matches!(
            &events[2],
            StreamEvent::ToolCallArgsDelta { index: 0, partial_json }
                if partial_json.contains("Paris")
        ));
        assert_eq!(
            events.last(),
            Some(&StreamEvent::Done {
                reason: FinishReason::ToolCalls,
                usage: None
            })
        );
    }

    #[test]
    fn openai_client_sees_text_tools_and_usage() {
        let inbound = json!({
            "model": "gemini-3-pro",
            "messages": [{"role": "user", "content": "weather?"}],
            "tools": [{"type": "function", "function": {
                "name": "get_weather",
                "parameters": {"type": "object", "properties": {"city": {"type": "string"}}}
            }}],
            "stream": false
        });
        let request = crate::parse_request(Dialect::OpenAiChat, &inbound).unwrap();
        let envelope = build_request_with(&request, "gemini-3-pro", "proj", "agent-1").unwrap();
        assert_eq!(
            envelope["request"]["tools"][0]["functionDeclarations"][0]["name"],
            "get_weather"
        );

        let upstream = json!({"response": {"candidates": [{
            "content": {"parts": [
                {"text": "18C"},
                {"functionCall": {"name": "get_weather", "args": {"city": "Paris"}}}
            ]},
            "finishReason": "STOP"
        }], "usageMetadata": {"promptTokenCount": 4, "candidatesTokenCount": 5}}});
        let response = crate::parse_response(Dialect::GoogleCodeAssist, &upstream).unwrap();
        let out = crate::build_response(Dialect::OpenAiChat, &response).unwrap();
        assert_eq!(out["choices"][0]["message"]["content"], "18C");
        assert_eq!(
            out["choices"][0]["message"]["tool_calls"][0]["function"]["name"],
            "get_weather"
        );
        assert_eq!(
            out["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"],
            "{\"city\":\"Paris\"}"
        );
        assert_eq!(out["choices"][0]["finish_reason"], "tool_calls");
        assert_eq!(out["usage"]["prompt_tokens"], 4);
        assert_eq!(out["usage"]["completion_tokens"], 5);
    }

    #[test]
    fn anthropic_client_sees_reasoning_tools_and_usage() {
        let inbound = json!({
            "model": "gemini-3-pro",
            "max_tokens": 256,
            "system": "be terse",
            "messages": [{"role": "user", "content": "weather?"}]
        });
        let request = crate::parse_request(Dialect::AnthropicMessages, &inbound).unwrap();
        let envelope = build_request_with(&request, "gemini-3-pro", "proj", "agent-2").unwrap();
        assert_eq!(
            envelope["request"]["systemInstruction"]["parts"][0]["text"],
            "be terse"
        );
        assert_eq!(
            envelope["request"]["generationConfig"]["maxOutputTokens"],
            256
        );

        let upstream = json!({"response": {"candidates": [{
            "content": {"parts": [
                {"text": "thinking", "thought": true},
                {"text": "18C"},
                {"functionCall": {"name": "get_weather", "args": {"city": "Paris"}}}
            ]},
            "finishReason": "STOP"
        }], "usageMetadata": {"promptTokenCount": 9, "candidatesTokenCount": 3, "thoughtsTokenCount": 2}}});
        let response = crate::parse_response(Dialect::GoogleCodeAssist, &upstream).unwrap();
        let out = crate::build_response(Dialect::AnthropicMessages, &response).unwrap();
        assert_eq!(out["content"][0]["type"], "thinking");
        assert_eq!(out["content"][0]["thinking"], "thinking");
        assert_eq!(out["content"][1]["type"], "text");
        assert_eq!(out["content"][1]["text"], "18C");
        assert_eq!(out["content"][2]["type"], "tool_use");
        assert_eq!(out["content"][2]["name"], "get_weather");
        assert_eq!(out["stop_reason"], "tool_use");
        assert_eq!(out["usage"]["input_tokens"], 9);
        assert_eq!(out["usage"]["output_tokens"], 5);
    }

    #[test]
    fn streaming_decodes_into_the_clients_own_frames() {
        let wire = "data: {\"response\":{\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"Hi\"}]}}]}}\n\n\
                    data: {\"response\":{\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"!\"}]},\"finishReason\":\"STOP\"}]}}\n\n";
        let mut decoder = crate::StreamDecoder::new(Dialect::GoogleCodeAssist, "m");
        let mut events = decoder.push(wire.as_bytes()).unwrap();
        events.extend(decoder.finish().unwrap());

        let mut openai = crate::StreamEncoder::new(Dialect::OpenAiChat, "m");
        let mut openai_wire = String::new();
        for event in &events {
            openai_wire.push_str(&String::from_utf8_lossy(&openai.encode(event).unwrap()));
        }
        assert!(openai_wire.contains("Hi"), "{openai_wire}");
        assert!(openai_wire.contains("!"), "both deltas reach the client");
        assert!(openai_wire.contains("[DONE]"), "{openai_wire}");

        let mut anthropic = crate::StreamEncoder::new(Dialect::AnthropicMessages, "m");
        let mut anthropic_wire = String::new();
        for event in &events {
            anthropic_wire.push_str(&String::from_utf8_lossy(&anthropic.encode(event).unwrap()));
        }
        assert!(
            anthropic_wire.contains("content_block_delta"),
            "{anthropic_wire}"
        );
        assert!(anthropic_wire.contains("message_stop"), "{anthropic_wire}");
    }

    #[test]
    fn stream_errors_are_reported_not_swallowed() {
        let frame = format!(
            "data: {}\n\n",
            json!({"error": {"code": 429, "message": "quota exhausted", "status": "RESOURCE_EXHAUSTED"}})
        );
        let mut decoder = GoogleCcaDecoder::new("m");
        let mut events = Vec::new();
        let err = decoder.push(frame.as_bytes(), &mut events).unwrap_err();
        assert!(err.to_string().contains("quota exhausted"), "{err}");
    }
}
