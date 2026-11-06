//! Static provider registry: transport (base URL, dialect), auth style, extra headers and
//! refresh recipe for every provider RRouter can route to.
//!
//! The table is `const` data; the id → spec index is a `LazyLock<HashMap>` built once.
//! These transports are live 9router transports and are copied verbatim; changing one is a
//! routing change, not a refactor.

use std::collections::HashMap;
use std::sync::LazyLock;

use rrouter_core::Dialect;

/// How an account for this provider is obtained and refreshed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProviderKind {
    /// OAuth account: expiring access token plus refresh token.
    OAuth,
    /// User-supplied API key, non-expiring.
    ApiKey,
    /// Local server (ollama): no credential.
    Local,
}

/// How the credential is presented on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AuthStyle {
    /// `authorization: Bearer <token>`.
    Bearer,
    /// `x-api-key: <key>`.
    XApiKey,
    /// No auth header (local servers).
    None,
}

/// Encoding of a refresh request body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RefreshBody {
    /// `application/json`.
    Json,
    /// `application/x-www-form-urlencoded`.
    Form,
}

/// Everything needed to exchange a refresh token for a fresh access token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RefreshSpec {
    /// OAuth token endpoint.
    pub token_url: &'static str,
    /// Public OAuth client id of the CLI whose flow issued the token.
    pub client_id: &'static str,
    /// Body encoding the endpoint expects.
    pub body: RefreshBody,
}

/// One provider's static transport description.
#[derive(Debug, Clone, Copy)]
pub struct ProviderSpec {
    /// Registry key ("claude", "codex", "openai", ...).
    pub id: &'static str,
    /// Display name for the dashboard.
    pub display: &'static str,
    /// Credential kind.
    pub kind: ProviderKind,
    /// Full chat endpoint URL, no trailing slash.
    pub base_url: &'static str,
    /// Native Responses endpoint, when the provider speaks it.
    pub responses_url: Option<&'static str>,
    /// Native Anthropic Messages endpoint, when the provider speaks it.
    pub messages_url: Option<&'static str>,
    /// Dialect `base_url` speaks.
    pub default_dialect: Dialect,
    /// How the credential is presented.
    pub auth: AuthStyle,
    /// Headers every request to this provider must carry.
    pub extra_headers: &'static [(&'static str, &'static str)],
    /// Provider-imposed output cap default.
    pub default_max_tokens: Option<u32>,
    /// Refresh recipe; `None` for `ApiKey`/`Local`.
    pub refresh: Option<RefreshSpec>,
}

impl ProviderSpec {
    /// `true` when this provider hands out short-lived OAuth access tokens.
    pub const fn is_oauth(&self) -> bool {
        matches!(self.kind, ProviderKind::OAuth)
    }

    /// Base URL to use for this account: the credential's override wins unconditionally.
    pub fn base_url_for<'a>(&'a self, override_url: Option<&'a str>) -> &'a str {
        override_url.unwrap_or(self.base_url)
    }
}

const ANTHROPIC_OAUTH_CLIENT: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const CODEX_OAUTH_CLIENT: &str = "app_EMoamEEZ73f0CkXaXp7hrann";

const CLAUDE_REFRESH: RefreshSpec = RefreshSpec {
    token_url: "https://api.anthropic.com/v1/oauth/token",
    client_id: ANTHROPIC_OAUTH_CLIENT,
    body: RefreshBody::Json,
};

const CODEX_REFRESH: RefreshSpec = RefreshSpec {
    token_url: "https://auth.openai.com/oauth/token",
    client_id: CODEX_OAUTH_CLIENT,
    body: RefreshBody::Form,
};

const CLAUDE_HEADERS: &[(&str, &str)] = &[
    ("anthropic-version", "2023-06-01"),
    (
        "anthropic-beta",
        "oauth-2025-04-20,interleaved-thinking-2025-05-14",
    ),
    ("user-agent", "claude-cli/2.0.0 (external, cli)"),
];

const CODEX_HEADERS: &[(&str, &str)] = &[
    ("originator", "codex_cli_rs"),
    ("version", "0.159.0"),
];

const GITHUB_HEADERS: &[(&str, &str)] = &[
    ("copilot-integration-id", "vscode-chat"),
    ("editor-version", "vscode/1.110.0"),
    ("X-Initiator", "user"),
];

const ANTHROPIC_HEADERS: &[(&str, &str)] = &[("anthropic-version", "2023-06-01")];

/// Every provider this build can route to.
pub static PROVIDERS: &[ProviderSpec] = &[
    ProviderSpec {
        id: "claude",
        display: "Claude (Anthropic OAuth)",
        kind: ProviderKind::OAuth,
        base_url: "https://api.anthropic.com/v1/messages",
        responses_url: None,
        messages_url: Some("https://api.anthropic.com/v1/messages"),
        default_dialect: Dialect::AnthropicMessages,
        auth: AuthStyle::Bearer,
        extra_headers: CLAUDE_HEADERS,
        default_max_tokens: Some(32_000),
        refresh: Some(CLAUDE_REFRESH),
    },
    ProviderSpec {
        id: "codex",
        display: "Codex (ChatGPT OAuth)",
        kind: ProviderKind::OAuth,
        base_url: "https://chatgpt.com/backend-api/codex/responses",
        responses_url: Some("https://chatgpt.com/backend-api/codex/responses"),
        messages_url: None,
        default_dialect: Dialect::OpenAiResponses,
        auth: AuthStyle::Bearer,
        extra_headers: CODEX_HEADERS,
        default_max_tokens: None,
        refresh: Some(CODEX_REFRESH),
    },
    ProviderSpec {
        id: "github",
        display: "GitHub Copilot",
        kind: ProviderKind::OAuth,
        base_url: "https://api.githubcopilot.com/chat/completions",
        responses_url: Some("https://api.githubcopilot.com/responses"),
        messages_url: Some("https://api.githubcopilot.com/v1/messages"),
        default_dialect: Dialect::OpenAiChat,
        auth: AuthStyle::Bearer,
        extra_headers: GITHUB_HEADERS,
        default_max_tokens: None,
        refresh: None,
    },
    ProviderSpec {
        id: "openai",
        display: "OpenAI",
        kind: ProviderKind::ApiKey,
        base_url: "https://api.openai.com/v1/chat/completions",
        responses_url: Some("https://api.openai.com/v1/responses"),
        messages_url: None,
        default_dialect: Dialect::OpenAiChat,
        auth: AuthStyle::Bearer,
        extra_headers: &[],
        default_max_tokens: None,
        refresh: None,
    },
    ProviderSpec {
        id: "anthropic",
        display: "Anthropic",
        kind: ProviderKind::ApiKey,
        base_url: "https://api.anthropic.com/v1/messages",
        responses_url: None,
        messages_url: Some("https://api.anthropic.com/v1/messages"),
        default_dialect: Dialect::AnthropicMessages,
        auth: AuthStyle::XApiKey,
        extra_headers: ANTHROPIC_HEADERS,
        default_max_tokens: Some(32_000),
        refresh: None,
    },
    ProviderSpec {
        id: "deepseek",
        display: "DeepSeek",
        kind: ProviderKind::ApiKey,
        base_url: "https://api.deepseek.com/chat/completions",
        responses_url: None,
        messages_url: None,
        default_dialect: Dialect::OpenAiChat,
        auth: AuthStyle::Bearer,
        extra_headers: &[],
        default_max_tokens: None,
        refresh: None,
    },
    ProviderSpec {
        id: "gemini",
        display: "Google Gemini",
        kind: ProviderKind::ApiKey,
        base_url: "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions",
        responses_url: None,
        messages_url: None,
        default_dialect: Dialect::OpenAiChat,
        auth: AuthStyle::Bearer,
        extra_headers: &[],
        default_max_tokens: None,
        refresh: None,
    },
    ProviderSpec {
        id: "groq",
        display: "Groq",
        kind: ProviderKind::ApiKey,
        base_url: "https://api.groq.com/openai/v1/chat/completions",
        responses_url: None,
        messages_url: None,
        default_dialect: Dialect::OpenAiChat,
        auth: AuthStyle::Bearer,
        extra_headers: &[],
        default_max_tokens: None,
        refresh: None,
    },
    ProviderSpec {
        id: "openrouter",
        display: "OpenRouter",
        kind: ProviderKind::ApiKey,
        base_url: "https://openrouter.ai/api/v1/chat/completions",
        responses_url: None,
        messages_url: None,
        default_dialect: Dialect::OpenAiChat,
        auth: AuthStyle::Bearer,
        extra_headers: &[],
        default_max_tokens: None,
        refresh: None,
    },
    ProviderSpec {
        id: "xai",
        display: "xAI",
        kind: ProviderKind::ApiKey,
        base_url: "https://api.x.ai/v1/chat/completions",
        responses_url: None,
        messages_url: None,
        default_dialect: Dialect::OpenAiChat,
        auth: AuthStyle::Bearer,
        extra_headers: &[],
        default_max_tokens: None,
        refresh: None,
    },
    ProviderSpec {
        id: "mistral",
        display: "Mistral",
        kind: ProviderKind::ApiKey,
        base_url: "https://api.mistral.ai/v1/chat/completions",
        responses_url: None,
        messages_url: None,
        default_dialect: Dialect::OpenAiChat,
        auth: AuthStyle::Bearer,
        extra_headers: &[],
        default_max_tokens: None,
        refresh: None,
    },
    ProviderSpec {
        id: "together",
        display: "Together AI",
        kind: ProviderKind::ApiKey,
        base_url: "https://api.together.xyz/v1/chat/completions",
        responses_url: None,
        messages_url: None,
        default_dialect: Dialect::OpenAiChat,
        auth: AuthStyle::Bearer,
        extra_headers: &[],
        default_max_tokens: None,
        refresh: None,
    },
    ProviderSpec {
        id: "cerebras",
        display: "Cerebras",
        kind: ProviderKind::ApiKey,
        base_url: "https://api.cerebras.ai/v1/chat/completions",
        responses_url: None,
        messages_url: None,
        default_dialect: Dialect::OpenAiChat,
        auth: AuthStyle::Bearer,
        extra_headers: &[],
        default_max_tokens: None,
        refresh: None,
    },
    ProviderSpec {
        id: "ollama",
        display: "Ollama (local)",
        kind: ProviderKind::Local,
        base_url: "http://localhost:11434/v1/chat/completions",
        responses_url: None,
        messages_url: None,
        default_dialect: Dialect::OpenAiChat,
        auth: AuthStyle::None,
        extra_headers: &[],
        default_max_tokens: None,
        refresh: None,
    },
];

static BY_ID: LazyLock<HashMap<&'static str, &'static ProviderSpec>> = LazyLock::new(|| {
    let mut map = HashMap::with_capacity(PROVIDERS.len());
    for spec in PROVIDERS {
        map.insert(spec.id, spec);
    }
    map
});

/// Look up a provider by registry id.
pub fn get(id: &str) -> Option<&'static ProviderSpec> {
    BY_ID.get(id).copied()
}

/// `true` when `id` names a registered OAuth provider.
pub fn is_oauth(id: &str) -> bool {
    get(id).is_some_and(ProviderSpec::is_oauth)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_and_reachable() {
        for (i, spec) in PROVIDERS.iter().enumerate() {
            assert!(
                get(spec.id).is_some_and(|s| std::ptr::eq(s, spec)),
                "{} not reachable",
                spec.id
            );
            assert_eq!(BY_ID.len(), PROVIDERS.len());
            assert!(
                PROVIDERS[..i].iter().all(|p| p.id != spec.id),
                "duplicate id {}",
                spec.id
            );
        }
    }

    #[test]
    fn no_trailing_slash_and_expected_styles() {
        for spec in PROVIDERS {
            assert!(!spec.base_url.ends_with('/'), "{} base_url", spec.id);
            assert_eq!(
                spec.is_oauth(),
                matches!(spec.kind, ProviderKind::OAuth),
                "{}",
                spec.id
            );
        }
        assert!(is_oauth("claude"));
        assert!(is_oauth("codex"));
        assert!(is_oauth("github"));
        assert!(!is_oauth("openai"));
        assert!(!is_oauth("anthropic"));
        assert!(!is_oauth("ollama"));
        assert!(get("nope").is_none());
    }

    #[test]
    fn transport_table_matches_spec() {
        let claude = get("claude").expect("claude");
        assert_eq!(claude.base_url, "https://api.anthropic.com/v1/messages");
        assert_eq!(claude.default_dialect, Dialect::AnthropicMessages);
        assert_eq!(claude.refresh.map(|r| r.body), Some(RefreshBody::Json));

        let codex = get("codex").expect("codex");
        assert_eq!(
            codex.base_url,
            "https://chatgpt.com/backend-api/codex/responses"
        );
        assert_eq!(codex.default_dialect, Dialect::OpenAiResponses);
        assert_eq!(codex.refresh.map(|r| r.body), Some(RefreshBody::Form));

        let github = get("github").expect("github");
        assert_eq!(
            github.responses_url,
            Some("https://api.githubcopilot.com/responses")
        );
        assert_eq!(
            github.messages_url,
            Some("https://api.githubcopilot.com/v1/messages")
        );

        let ollama = get("ollama").expect("ollama");
        assert_eq!(ollama.kind, ProviderKind::Local);
        assert_eq!(ollama.auth, AuthStyle::None);

        for spec in PROVIDERS {
            match spec.kind {
                // `github` is OAuth but derives its copilot token instead of running an
                // RFC 6749 refresh, so it is the one OAuth entry without a `RefreshSpec`.
                ProviderKind::OAuth if spec.id == "github" => assert!(spec.refresh.is_none()),
                ProviderKind::OAuth => assert!(spec.refresh.is_some(), "{}", spec.id),
                ProviderKind::ApiKey | ProviderKind::Local => {
                    assert!(spec.refresh.is_none(), "{}", spec.id)
                }
            }
        }
    }
}
