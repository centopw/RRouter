use serde::{Deserialize, Serialize};

/// Wire protocol used on a given side of the proxy.
///
/// Inbound: shape the client (Claude Code, Codex CLI, Cline, Cursor, ...) speaks.
/// Upstream: shape the provider's HTTP API expects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Dialect {
    /// OpenAI Chat Completions (`/v1/chat/completions`).
    OpenAiChat,
    /// OpenAI Responses API (`/v1/responses`).
    OpenAiResponses,
    /// Anthropic Messages API (`/v1/messages`).
    AnthropicMessages,
    /// OpenAI Embeddings (`/v1/embeddings`).
    OpenAiEmbeddings,
}

impl Dialect {
    /// Dialect implied by the inbound HTTP path.
    pub fn from_path(path: &str) -> Option<Self> {
        let path = path.split('?').next().unwrap_or(path);
        match path {
            "/v1/chat/completions" => Some(Self::OpenAiChat),
            "/v1/responses" | "/v1/responses/compact" => Some(Self::OpenAiResponses),
            "/v1/messages" => Some(Self::AnthropicMessages),
            "/v1/embeddings" => Some(Self::OpenAiEmbeddings),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OpenAiChat => "openai_chat",
            Self::OpenAiResponses => "openai_responses",
            Self::AnthropicMessages => "anthropic_messages",
            Self::OpenAiEmbeddings => "openai_embeddings",
        }
    }

    /// True when the dialect streams as Anthropic-style named SSE events.
    pub const fn is_event_framed(self) -> bool {
        matches!(self, Self::AnthropicMessages)
    }
}

impl std::fmt::Display for Dialect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_paths_with_query_and_trailing_segments() {
        assert_eq!(
            Dialect::from_path("/v1/messages?beta=1"),
            Some(Dialect::AnthropicMessages)
        );
        assert_eq!(
            Dialect::from_path("/v1/chat/completions"),
            Some(Dialect::OpenAiChat)
        );
        assert_eq!(Dialect::from_path("/v1/models"), None);
    }
}
