//! The outbound wire shape a provider demands, as opposed to the canonical model.
//!
//! The canonical request is provider-agnostic, but two facts are not: how the output
//! cap is spelled, and what to send when the client names no cap. OpenAI's own surface
//! spells it `max_completion_tokens` and rejects `max_tokens` on current gpt-5/o-series
//! models, while Mistral's API answers `422` to `max_completion_tokens` and expects
//! `max_tokens` (the spelling every other OpenAI-compatible provider documents).
//!
//! [`WireProfile`] carries those facts from the provider registry
//! (`rrouter_auth::ProviderSpec::wire_profile`) to the serializer, so the translators
//! stay provider-agnostic.

/// Cap sent when neither the client nor the provider states one.
///
/// Anthropic's Messages API requires the field, so a translated request must always
/// carry a number; this is the historical fallback and is only reached when the
/// provider declares no `default_max_tokens`.
pub const DEFAULT_MAX_TOKENS: u32 = 4096;

/// Request key that carries the output cap in the Chat dialect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MaxTokensKey {
    /// `max_tokens`: documented by Mistral, Together, DeepSeek, Gemini, Groq, xAI,
    /// Cerebras, Ollama, OpenRouter and vLLM/TGI — and the safe choice for an
    /// aggregator whose upstream is unknown.
    #[default]
    MaxTokens,
    /// `max_completion_tokens`: OpenAI's own surface (API, Codex, Copilot), where
    /// current gpt-5/o-series models reject `max_tokens` outright.
    MaxCompletionTokens,
}

impl MaxTokensKey {
    /// The JSON field name this key is written as.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MaxTokens => "max_tokens",
            Self::MaxCompletionTokens => "max_completion_tokens",
        }
    }
}

/// Provider-side request shaping the canonical model cannot express.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WireProfile {
    /// Key the Chat dialect uses for the output cap.
    pub max_tokens_key: MaxTokensKey,
    /// Provider-imposed cap applied when the client sends none.
    pub default_max_tokens: Option<u32>,
}

impl WireProfile {
    pub const fn new(max_tokens_key: MaxTokensKey, default_max_tokens: Option<u32>) -> Self {
        Self {
            max_tokens_key,
            default_max_tokens,
        }
    }

    /// Cap the provider will accept when it requires one: the client's value wins, then
    /// the provider default, then [`DEFAULT_MAX_TOKENS`].
    ///
    /// Used by dialects where the field is mandatory (Anthropic Messages).
    pub fn max_tokens(&self, requested: Option<u32>) -> u32 {
        self.max_tokens_opt(requested).unwrap_or(DEFAULT_MAX_TOKENS)
    }

    /// Cap to send, or `None` when neither side stated one.
    ///
    /// Used by dialects where the field is optional: an explicit client value always
    /// wins, and nothing is invented when the provider declares no default.
    pub fn max_tokens_opt(&self, requested: Option<u32>) -> Option<u32> {
        requested.or(self.default_max_tokens)
    }
}
