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
    /// Google Code Assist (`/v1internal:generateContent`), the upstream-only wire used
    /// by the Antigravity desktop client. Never an inbound route: [`Self::from_path`]
    /// does not yield it.
    GoogleCodeAssist,
    /// Qoder's agent endpoint (`/algo/api/v2/service/pro/sse/agent_chat_generation`),
    /// the upstream-only wire qodercli speaks. `POST` is a COSY-signed, obfuscated
    /// envelope and the answer is a `{statusCodeValue, body}` SSE wrapper. Never an
    /// inbound route: [`Self::from_path`] does not yield it.
    QoderAgent,
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
            Self::GoogleCodeAssist => "google_code_assist",
            Self::QoderAgent => "qoder_agent",
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

    #[test]
    fn google_code_assist_is_upstream_only() {
        assert_eq!(Dialect::GoogleCodeAssist.as_str(), "google_code_assist");
        assert_eq!(Dialect::from_path("/v1internal:generateContent"), None);
        assert!(!Dialect::GoogleCodeAssist.is_event_framed());
    }

    #[test]
    fn qoder_agent_is_upstream_only() {
        assert_eq!(Dialect::QoderAgent.as_str(), "qoder_agent");
        assert_eq!(Dialect::from_path("/algo/api/v2/model/list"), None);
        assert!(!Dialect::QoderAgent.is_event_framed());
    }
}
