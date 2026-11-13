//! Qoder's agent wire: the envelope qodercli posts to
//! `/algo/api/v2/service/pro/sse/agent_chat_generation?…&Encode=1`.
//!
//! This is a **provider dialect**, not an inbound one. A client still talks OpenAI Chat,
//! Anthropic Messages or Responses; the gateway translates that canonical request into the
//! envelope below, obfuscates and COSY-signs it (in `rrouter-auth::qoder`), and unwraps the
//! answer back into canonical [`StreamEvent`]s. [`Dialect::QoderAgent`] exists so the proxy
//! can name the upstream grammar without exposing a route.
//!
//! ```json
//! {
//!   "request_id": "<uuid>",
//!   "request_set_id": "<16 hex>",
//!   "chat_record_id": "<16 hex>",
//!   "session_id": "<16 hex>",
//!   "stream": true,
//!   "chat_task": "FREE_INPUT",
//!   "session_type": "qodercli",
//!   "system": "<hoisted system prompt>",
//!   "messages": [{"role": "user", "content": "hi"}],
//!   "tools": [{"type": "function", "function": {…}}],
//!   "parameters": {"max_tokens": 32768},
//!   "chat_context": {"text": "hi", "extra": {"modelConfig": {"key": "auto", …}}},
//!   "model_config": {"key": "auto", "is_reasoning": false, …},
//!   "business": {"product": "cli", "type": "agent", "stage": "start", …}
//! }
//! ```
//!
//! Three fields are load-bearing and easy to get wrong:
//!
//! - `model_config` is the account's **own catalog entry** for the model, fetched from
//!   `/algo/api/v2/model/list`. Sending another model's block (or inventing one) makes the
//!   backend silently answer with a different model, so a caller that has no entry must
//!   refuse the request instead of guessing: see `rrouter_auth::qoder::model_config`.
//! - `system` is hoisted out of `messages`; Qoder rejects `role:"system"` in the array.
//! - `request_set_id` / `chat_record_id` are stable for one prompt and `session_id` for one
//!   (account, model) pair: the upstream keys its model queue by them, and fresh ids per
//!   retry make a queued request look like a new one.
//!
//! # Responses
//!
//! Every SSE line is `data: {"statusCodeValue":200,"body":"<stringified OpenAI chunk>"}`,
//! interleaved with heartbeats and closed by a trailing `data: [DONE]`. [`classify`] unwraps
//! one line; an envelope whose status is not `200` becomes a gateway [`Error`], because the
//! HTTP status was already `200` by then:
//!
//! - code `10605` (`isQueued` / `subCode:RESOURCE_LIMIT`) is a transient capacity signal, so
//!   it maps to `503` with `retry_after` taken from `retryAfterSeconds`/`waitTime`: the
//!   dispatcher waits that long and re-signs, instead of benching the account.
//! - codes `110`/`112` (and any payload naming a `pricingUrl`) are billing/quota blocks and
//!   map to `429`, which is the gateway's existing quota-exhausted path: the candidate is
//!   cooled down and the next account is tried.

use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use rrouter_core::{ChatRequest, Error, Result, Role, StreamEvent};

use crate::openai_chat::{function_tools, messages_out, OpenAiChatDecoder};

/// Origin qodercli talks to. `api3` serves `dt-` device tokens; a deployment can point
/// a connection elsewhere with `base_url_override`.
pub const DEFAULT_BASE: &str = "https://api3.qoder.sh";

/// Inference path under `/algo`. Also the `Cosy-Sigpath` value (the signer strips `/algo`).
pub const CHAT_SIG_PATH: &str = "/api/v2/service/pro/sse/agent_chat_generation";

/// Model catalogue path under `/algo`.
pub const MODEL_LIST_SIG_PATH: &str = "/api/v2/model/list";

/// Query the inference endpoint requires: `Encode=1` tells the server the body is
/// obfuscated, the other two are what qodercli sends.
const CHAT_QUERY: &str = "?FetchKeys=llm_model_result&AgentId=agent_common&Encode=1";

/// Prefix whose absence/presence the signature path normalises away.
const ALGO_PREFIX: &str = "/algo";

/// Output cap used when the catalog entry names none and the client asked for none. Matches
/// qodercli's ceiling; the client's own value always wins when it is smaller.
pub const DEFAULT_MAX_TOKENS: u32 = 32_768;

/// `business.name` is a label, not a prompt: qodercli truncates it.
const BUSINESS_NAME_MAX: usize = 30;

/// Model-queue error code (`RESOURCE_LIMIT`).
pub const QUEUE_CODE: &str = "10605";
/// Billing daily-count / quota-exhausted codes.
pub const QUOTA_CODES: [&str; 2] = ["110", "112"];
/// Default queue wait when the refusal names none.
const DEFAULT_QUEUE_RETRY_SECS: f64 = 30.0;

/// Full model-catalogue URL for one connection.
pub fn models_url(base: &str) -> String {
    let mut url = String::with_capacity(base.len() + ALGO_PREFIX.len() + MODEL_LIST_SIG_PATH.len());
    url.push_str(base.trim_end_matches('/'));
    url.push_str(ALGO_PREFIX);
    url.push_str(MODEL_LIST_SIG_PATH);
    url
}

/// Full inference URL for one connection.
pub fn chat_url(base: &str) -> String {
    let mut url = String::with_capacity(base.len() + 96);
    url.push_str(base.trim_end_matches('/'));
    url.push_str(ALGO_PREFIX);
    url.push_str(CHAT_SIG_PATH);
    url.push_str(CHAT_QUERY);
    url
}

/// Path the signature covers: the URL's path with the leading `/algo` stripped, exactly as
/// qodercli computes it. Query and fragment are not part of it.
pub fn sig_path(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let path = rest.find('/').map_or("/", |i| &rest[i..]);
    let path = path.split(['?', '#']).next().unwrap_or(path);
    path.strip_prefix(ALGO_PREFIX).unwrap_or(path).to_string()
}

/// Stable per-(account, model) session id. The upstream tracks queue state per session, so
/// this must not change between requests.
pub fn session_id(user_id: &str, model: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(b"qoder-session\0");
    hash.update(user_id.as_bytes());
    hash.update(b"\0");
    hash.update(model.as_bytes());
    hex16(&hash.finalize())
}

fn hex16(digest: &[u8]) -> String {
    let mut out = String::with_capacity(16);
    for byte in &digest[..8] {
        out.push(char::from_digit(u32::from(byte >> 4), 16).unwrap_or('0'));
        out.push(char::from_digit(u32::from(byte & 0x0f), 16).unwrap_or('0'));
    }
    out
}

/// One request's model identity: the catalog key plus that account's own `model_config`.
pub struct Envelope<'a> {
    /// Canonical Qoder key (`auto`, `qmodel`, `smodel`, …), already stripped of any
    /// `qoder/` alias prefix.
    pub model: &'a str,
    /// The account's catalog entry for [`Self::model`], verbatim.
    pub model_config: &'a Value,
    /// [`session_id`] for this account and model.
    pub session_id: &'a str,
}

/// Build the envelope for one request.
///
/// A missing or non-object `model_config` is an error: the backend silently downgrades to a
/// different model when the block does not match the requested key, and a request that is
/// answered by the wrong model is worse than a request that fails loudly.
pub fn build_request(req: &ChatRequest, env: &Envelope<'_>) -> Result<Value> {
    let model = env.model.trim();
    if model.is_empty() {
        return Err(Error::BadRequest("a model id is required".into()));
    }
    if !env.model_config.is_object() {
        return Err(Error::Translation(format!(
            "qoder: no model_config for \"{model}\"; fetch the account catalog first"
        )));
    }

    // Qoder rejects `role:"system"` inside `messages`; every system turn joins the
    // dedicated field with a blank line, as qodercli does.
    let mut system_parts: Vec<String> = Vec::new();
    if let Some(system) = req.system.as_deref().filter(|s| !s.is_empty()) {
        system_parts.push(system.to_string());
    }
    let mut messages: Vec<Value> = Vec::with_capacity(req.messages.len());
    for msg in &req.messages {
        if msg.role == Role::System {
            let text = msg.joined_text();
            if !text.is_empty() {
                system_parts.push(text);
            }
            continue;
        }
        messages.extend(messages_out(msg)?);
    }

    let tools = function_tools(req);
    let max_tokens = max_tokens(req, env.model_config);
    let is_reasoning = env
        .model_config
        .get("is_reasoning")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let last_user = last_user_text(&messages);
    let record_id = record_id(model, &messages, &tools, max_tokens);

    Ok(json!({
        "request_id": Uuid::new_v4().to_string(),
        "request_set_id": record_id,
        "chat_record_id": record_id,
        "session_id": env.session_id,
        "stream": true,
        "chat_task": "FREE_INPUT",
        "is_reply": true,
        "is_retry": false,
        "source": 1,
        "version": "3",
        "session_type": "qodercli",
        "agent_id": "agent_common",
        "task_id": "common",
        "code_language": "",
        "chat_prompt": "",
        "image_urls": Value::Null,
        "aliyun_user_type": "",
        "system": system_parts.join("\n\n"),
        "messages": messages,
        "tools": tools,
        "parameters": {"max_tokens": max_tokens},
        "chat_context": {
            "chatPrompt": "",
            "imageUrls": Value::Null,
            "extra": {
                "context": [],
                "modelConfig": {"key": model, "is_reasoning": is_reasoning},
                "originalContent": last_user,
            },
            "features": [],
            "text": last_user,
        },
        "model_config": env.model_config,
        "business": {
            "product": "cli",
            "version": "1.0.0",
            "type": "agent",
            "stage": "start",
            "id": Uuid::new_v4().to_string(),
            "name": truncate(&last_user, BUSINESS_NAME_MAX),
            "begin_at": now_ms(),
        },
    }))
}

/// Output cap: the catalog entry's `max_output_tokens`, else [`DEFAULT_MAX_TOKENS`], capped
/// by a smaller client value.
fn max_tokens(req: &ChatRequest, model_config: &Value) -> u32 {
    let catalog = model_config
        .get("max_output_tokens")
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_MAX_TOKENS);
    match req.max_tokens {
        Some(n) if n > 0 && n < catalog => n,
        _ => catalog,
    }
}

/// Text of the last `user` message, which the envelope mirrors in `chat_context` and names
/// the `business` block after.
fn last_user_text(messages: &[Value]) -> String {
    for message in messages.iter().rev() {
        if message.get("role").and_then(Value::as_str) != Some("user") {
            continue;
        }
        return match message.get("content") {
            Some(Value::String(text)) => text.clone(),
            Some(Value::Array(parts)) => parts
                .iter()
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n"),
            _ => String::new(),
        };
    }
    String::new()
}

/// Stable id for one prompt: the same messages, tools and cap produce the same id, so a
/// re-signed retry lands on the same upstream chat record.
fn record_id(model: &str, messages: &[Value], tools: &[Value], max_tokens: u32) -> String {
    let mut hash = Sha256::new();
    hash.update(b"qoder-record\0");
    hash.update(model.as_bytes());
    for message in messages {
        hash.update(b"\0");
        hash.update(message.to_string().as_bytes());
    }
    if !tools.is_empty() {
        hash.update(b"\0");
        hash.update(Value::Array(tools.to_vec()).to_string().as_bytes());
    }
    hash.update(b"\0mt=");
    hash.update(max_tokens.to_string().as_bytes());
    hex16(&hash.finalize())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    text.chars().take(max).collect()
}

// ---------------------------------------------------------------------------
// Response in
// ---------------------------------------------------------------------------

/// One upstream line, classified.
#[derive(Debug)]
pub enum Frame {
    /// Keepalive, comment, blank separator, or an envelope with nothing to forward.
    Empty,
    /// End of stream (`[DONE]`, at either level).
    Done,
    /// An OpenAI payload unwrapped from the envelope's `body`.
    Payload(Value),
    /// Upstream reported a failure. The HTTP status was `200`, so the error is built here.
    Failed(Error),
}

/// Classify one SSE line's payload (everything after `data:`).
pub fn classify(data: &str) -> Frame {
    let data = data.trim();
    if data.is_empty() {
        return Frame::Empty;
    }
    if data == "[DONE]" {
        return Frame::Done;
    }
    let Ok(envelope) = serde_json::from_str::<Value>(data) else {
        return Frame::Empty;
    };
    let Some(status) = envelope.get("statusCodeValue") else {
        // A server that answers the bare OpenAI chunk: accept it rather than drop content.
        return if envelope.get("choices").is_some() {
            Frame::Payload(envelope)
        } else {
            Frame::Empty
        };
    };
    let status = status
        .as_u64()
        .or_else(|| status.as_str().and_then(|s| s.trim().parse().ok()))
        .and_then(|n| u16::try_from(n).ok())
        .unwrap_or(200);
    let body = match envelope.get("body") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    };
    if status != 200 {
        return Frame::Failed(inner_error(status, &body));
    }
    let body = body.trim();
    if body.is_empty() {
        return Frame::Empty;
    }
    if body == "[DONE]" {
        return Frame::Done;
    }
    match serde_json::from_str::<Value>(body) {
        Ok(payload) => Frame::Payload(payload),
        // A 200 envelope whose body is not JSON carries nothing a client can use.
        Err(_) => Frame::Empty,
    }
}

/// The payload of one buffered SSE line, or `None` when the line is not `data:`.
fn data_of(line: &[u8]) -> Option<&str> {
    let line = std::str::from_utf8(line).ok()?;
    let line = line.trim_end_matches(['\r', '\n']).trim();
    line.strip_prefix("data:").map(str::trim_start)
}

/// Classify the first complete line in `buf`, with the bytes that line occupies.
///
/// `None` means no newline is buffered yet. The gateway uses this to read the first frame
/// before it commits to a `200`: a queued model announces itself there.
pub fn first_line(buf: &[u8]) -> Option<(Frame, usize)> {
    let end = buf.iter().position(|byte| *byte == b'\n')?;
    let frame = match data_of(&buf[..end]) {
        Some(data) => classify(data),
        None => Frame::Empty,
    };
    Some((frame, end + 1))
}

/// Unwrap Qoder's `{statusCodeValue, body}` envelope stream into canonical events.
///
/// Qoder writes one `data:` line per frame (blank-line separators are optional and
/// heartbeats are frequent), so reading is line-oriented rather than frame-oriented; the
/// inner chunk vocabulary is the OpenAI one, decoded by [`OpenAiChatDecoder`].
#[derive(Debug, Default)]
pub struct QoderAgentDecoder {
    buf: Vec<u8>,
    inner: OpenAiChatDecoder,
    done: bool,
}

impl QoderAgentDecoder {
    pub fn new(model: &str) -> Self {
        Self {
            inner: OpenAiChatDecoder::new(model),
            ..Self::default()
        }
    }

    pub fn push(&mut self, chunk: &[u8], out: &mut Vec<StreamEvent>) -> Result<()> {
        // A terminal frame ends the stream even if the socket keeps sending keepalives.
        if self.done {
            return Ok(());
        }
        self.buf.extend_from_slice(chunk);
        while let Some(end) = self.buf.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=end).collect();
            self.line(&line, out)?;
            if self.done {
                self.buf.clear();
                return Ok(());
            }
        }
        Ok(())
    }

    pub fn finish(&mut self, out: &mut Vec<StreamEvent>) -> Result<()> {
        if !self.buf.is_empty() {
            let line = std::mem::take(&mut self.buf);
            self.line(&line, out)?;
        }
        if !self.done {
            self.done = true;
            self.inner.finish(out)?;
        }
        Ok(())
    }

    fn line(&mut self, line: &[u8], out: &mut Vec<StreamEvent>) -> Result<()> {
        let Some(data) = data_of(line) else {
            return Ok(());
        };
        match classify(data) {
            Frame::Empty => Ok(()),
            Frame::Payload(payload) => self.inner.push_value(&payload, out),
            Frame::Done => {
                self.done = true;
                self.inner.finish(out)
            }
            Frame::Failed(error) => Err(error),
        }
    }
}

/// Map a non-200 envelope body to the error the gateway should act on.
///
/// Qoder double- and triple-encodes its error payloads (a JSON object inside a JSON string
/// inside another), so every lookup walks nested JSON strings.
fn inner_error(status: u16, body: &str) -> Error {
    let parsed: Option<Value> = serde_json::from_str(body).ok();
    let nodes = flatten(parsed.as_ref());
    let code = nodes.iter().find_map(|node| scalar_text(node.get("code")));
    let queued = code.as_deref() == Some(QUEUE_CODE)
        || nodes
            .iter()
            .any(|node| scalar_text(node.get("subCode")).as_deref() == Some("RESOURCE_LIMIT"))
        || nodes.iter().any(|node| {
            node.get("isQueued").and_then(Value::as_bool) == Some(true)
                || node.get("serviceAvailable").and_then(Value::as_bool) == Some(false)
        });
    let quota = code
        .as_deref()
        .is_some_and(|code| QUOTA_CODES.contains(&code))
        || body.to_ascii_lowercase().contains("pricingurl");

    if queued {
        // A capacity signal, not a blocked account: bench nothing, wait the server's own
        // advertised interval and retry. `retry_after` is what the dispatcher reads.
        let seconds = nodes
            .iter()
            .find_map(|node| scalar_number(node.get("retryAfterSeconds")))
            .or_else(|| {
                nodes
                    .iter()
                    .find_map(|node| scalar_number(node.get("waitTime")))
            })
            .unwrap_or(DEFAULT_QUEUE_RETRY_SECS)
            .clamp(1.0, 600.0);
        return Error::Upstream {
            provider: "qoder".into(),
            status: 503,
            body: body.to_string(),
            payload: Some(json!({
                "code": QUEUE_CODE,
                "type": "rate_limit_error",
                "retry_after": seconds,
                "message": format!(
                    "qoder model is queued; retry in {}s",
                    seconds.ceil() as u64
                ),
            })),
        };
    }

    if quota {
        // The gateway's quota-exhausted path: a 429 benches this model on this account and
        // the dispatcher tries the next candidate.
        return Error::Upstream {
            provider: "qoder".into(),
            status: 429,
            body: body.to_string(),
            payload: Some(json!({
                "code": code,
                "type": "insufficient_quota",
                "message": if body.trim().is_empty() {
                    "qoder account is out of quota".to_string()
                } else {
                    body.to_string()
                },
            })),
        };
    }

    let status = if (400..=599).contains(&status) {
        status
    } else {
        502
    };
    Error::Upstream {
        provider: "qoder".into(),
        status,
        body: body.to_string(),
        payload: parsed,
    }
}

/// Every object reachable from `root`, walking the members Qoder nests payloads in and
/// parsing JSON-encoded strings as it goes. Bounded, so a hostile body cannot spin.
fn flatten(root: Option<&Value>) -> Vec<Value> {
    const NESTED: [&str; 7] = [
        "error", "data", "body", "message", "cause", "result", "payload",
    ];
    const MAX_NODES: usize = 32;
    let mut out: Vec<Value> = Vec::new();
    let mut queue: Vec<Value> = root.cloned().into_iter().collect();
    while let Some(node) = queue.pop() {
        if out.len() >= MAX_NODES {
            break;
        }
        match &node {
            Value::Object(map) => {
                for key in NESTED {
                    let Some(value) = map.get(key) else { continue };
                    match value {
                        Value::Object(_) | Value::Array(_) => queue.push(value.clone()),
                        Value::String(text) => {
                            if let Ok(parsed) = serde_json::from_str::<Value>(text) {
                                if parsed.is_object() || parsed.is_array() {
                                    queue.push(parsed);
                                }
                            }
                        }
                        _ => {}
                    }
                }
                out.push(node);
            }
            Value::Array(items) => queue.extend(items.iter().cloned()),
            _ => {}
        }
    }
    out
}

/// A scalar as text: providers use strings and numbers interchangeably for `code`.
fn scalar_text(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(text) => Some(text.trim().to_string()).filter(|t| !t.is_empty()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

fn scalar_number(value: Option<&Value>) -> Option<f64> {
    match value? {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => text.trim().parse().ok(),
        _ => None,
    }
}
