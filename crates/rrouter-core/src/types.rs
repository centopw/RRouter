//! Canonical internal model. Every translator converts between one wire dialect
//! and these types, so N dialects cost 2N conversions instead of N².

use serde_json::Value;

/// One content part inside a message.
#[derive(Debug, Clone, PartialEq)]
pub enum Part {
    Text(String),
    /// Base64 image or remote URL, already normalized to a data-URL or URL string.
    Image { url: String, media_type: Option<String> },
    /// A completed or streaming tool invocation requested by the model.
    ToolUse { id: String, name: String, input: Value },
    /// A tool result fed back by the client.
    ToolResult { tool_use_id: String, content: String, is_error: bool },
    /// Provider-specific reasoning/thinking block. `payload` is the native shape
    /// and is passed through unchanged when the outbound dialect matches.
    Reasoning { text: String, signature: Option<String> },
    /// Anything a translator could not map; preserved so round-trips lose nothing.
    Opaque(Value),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    pub role: Role,
    pub parts: Vec<Part>,
}

impl Message {
    pub fn text(role: Role, text: impl Into<String>) -> Self {
        Self { role, parts: vec![Part::Text(text.into())] }
    }

    /// Concatenated text of all `Part::Text` entries.
    pub fn joined_text(&self) -> String {
        let mut out = String::new();
        for p in &self.parts {
            if let Part::Text(t) = p {
                out.push_str(t);
            }
        }
        out
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// Tool declaration in canonical (JSON-Schema) form.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolDef {
    pub name: String,
    pub description: Option<String>,
    /// JSON Schema object for the arguments.
    pub parameters: Value,
    /// OpenAI's `strict` schema-conformance flag. Kept per tool because it changes
    /// how the provider validates arguments; dropping it silently loosens the call.
    pub strict: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ToolChoice {
    Auto,
    None,
    Required,
    Tool(String),
}

/// Everything needed to send one non-streaming or streaming completion upstream.
#[derive(Debug, Clone)]
pub struct ChatRequest {
    /// Model id as the client wrote it (may be an alias or a `provider/model` combo).
    pub model: String,
    /// System prompt, hoisted out of `messages` for dialects that have a
    /// dedicated field (Anthropic, Responses `instructions`).
    pub system: Option<String>,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolDef>,
    /// Tools the dialects model as opaque provider features (`web_search`,
    /// `file_search`, `custom`, ...). They cannot be expressed as function tools, so
    /// they travel verbatim and only when the outbound dialect matches the source.
    pub raw_tools: Option<(crate::Dialect, Vec<Value>)>,
    pub tool_choice: Option<ToolChoice>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub stop: Vec<String>,
    pub stream: bool,
    /// Dialect-level passthrough for fields RRouter does not model (e.g. `metadata`,
    /// `service_tier`). Merged into the outbound body only when the outbound dialect
    /// equals `passthrough_dialect`, so unknown fields never leak across dialects.
    pub extra: Option<(crate::Dialect, Value)>,
}

impl ChatRequest {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            system: None,
            messages: Vec::new(),
            tools: Vec::new(),
            raw_tools: None,
            tool_choice: None,
            max_tokens: None,
            temperature: None,
            top_p: None,
            stop: Vec::new(),
            stream: false,
            extra: None,
        }
    }
}

/// One incremental piece of a streamed response, dialect-independent.
#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    Start { id: String, model: String },
    TextDelta(String),
    ReasoningDelta(String),
    ToolCallStart { index: u32, id: String, name: String },
    ToolCallArgsDelta { index: u32, partial_json: String },
    /// Terminal signal; `reason` is the canonical reason (see [`FinishReason`]).
    Done { reason: FinishReason, usage: Option<Usage> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinishReason {
    Stop,
    Length,
    ToolCalls,
    ContentFilter,
    Other,
}

impl FinishReason {
    pub fn openai(self) -> &'static str {
        match self {
            Self::Stop => "stop",
            Self::Length => "length",
            Self::ToolCalls => "tool_calls",
            Self::ContentFilter => "content_filter",
            Self::Other => "stop",
        }
    }

    pub fn anthropic(self) -> &'static str {
        match self {
            Self::Stop => "end_turn",
            Self::Length => "max_tokens",
            Self::ToolCalls => "tool_use",
            Self::ContentFilter => "refusal",
            Self::Other => "end_turn",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_read_tokens: u32,
    pub cache_write_tokens: u32,
}

impl Usage {
    pub fn total(&self) -> u32 {
        self.input_tokens + self.output_tokens
    }
}

/// Full response body for non-streaming calls.
#[derive(Debug, Clone)]
pub struct ChatResponse {
    pub id: String,
    pub model: String,
    pub parts: Vec<Part>,
    pub finish_reason: FinishReason,
    pub usage: Usage,
}
