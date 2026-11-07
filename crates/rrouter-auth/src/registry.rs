//! Static provider registry: transport (base URL, dialect), auth style, extra headers and
//! refresh recipe for every provider RRouter can route to.
//!
//! The table is `const` data; the id → spec index is a `LazyLock<HashMap>` built once.
//! These transports are live 9router transports and are copied verbatim; changing one is a
//! routing change, not a refactor.

use std::collections::HashMap;
use std::sync::LazyLock;

use rrouter_core::Dialect;

use crate::overrides::{override_for, OverrideKind};

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

/// How a user signs in to obtain an OAuth account in the first place.
///
/// Distinct from [`RefreshSpec`], which keeps an existing account alive: this is
/// the one-time human step (browser consent or device code) that yields the first
/// refresh token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginFlow {
    /// Authorization Code + PKCE against a loopback redirect (RFC 7636).
    Pkce(PkceFlow),
    /// OAuth 2.0 Device Authorization Grant (RFC 8628).
    DeviceCode(DeviceFlow),
}

/// Authorization Code + PKCE parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PkceFlow {
    pub client_id: &'static str,
    pub authorize_url: &'static str,
    pub token_url: &'static str,
    /// Space-joined scope string.
    pub scope: &'static str,
    /// Fixed loopback port the provider's registered redirect URI requires, if any.
    /// `None` means bind an ephemeral port.
    pub fixed_port: Option<u16>,
    /// Host spelled in the redirect URI. Anthropic's client registration only accepts
    /// `localhost`, not `127.0.0.1`; OpenAI's Codex registration is the other way round.
    pub redirect_host: &'static str,
    /// Path of the loopback callback (`/callback` or `/auth/callback`).
    pub callback_path: &'static str,
    /// Extra query params the provider's own CLI sends.
    pub extra_params: &'static [(&'static str, &'static str)],
    /// Echo `state` in the token exchange. Anthropic binds the exchange to the
    /// authorization request and answers "Invalid request format" without it.
    pub send_state: bool,
    /// Token-endpoint request encoding.
    pub body: RefreshBody,
}

/// Device Authorization Grant parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceFlow {
    pub client_id: &'static str,
    pub device_code_url: &'static str,
    pub token_url: &'static str,
    /// Space-joined scope string.
    pub scope: &'static str,
}

impl LoginFlow {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Pkce(_) => "pkce",
            Self::DeviceCode(_) => "device_code",
        }
    }
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
    /// Native image-generation endpoint, when the provider documents one.
    pub images_url: Option<&'static str>,
    /// Native text-to-speech endpoint, when the provider documents one.
    pub audio_speech_url: Option<&'static str>,
    /// Native speech-to-text endpoint, when the provider documents one.
    pub audio_transcriptions_url: Option<&'static str>,
    /// Native voice-listing endpoint. No provider here documents one, so every entry
    /// leaves it `None` and `/v1/audio/voices` answers `501` instead of guessing a path.
    pub audio_voices_url: Option<&'static str>,
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
    /// Profile endpoint used as a best-effort account-label fallback.
    pub profile_url: Option<&'static str>,
    /// One-time sign-in recipe; `None` when the provider has no browser/device login
    /// (API-key providers) or RRouter does not implement it.
    pub login: Option<LoginFlow>,
    /// `true` when the upstream rejects non-streaming requests (codex answers
    /// "Stream must be set to true"). The gateway then streams upstream and
    /// reassembles the result for a non-streaming client.
    pub stream_only: bool,
}

impl ProviderSpec {
    /// The sign-in flow for this provider, when one is wired.
    pub const fn login_flow(&self) -> Option<LoginFlow> {
        self.login
    }

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

/// Anthropic gates model access on the client version it sees: a request claiming an
/// old Claude Code is answered with "version 2.1.280 or newer is required" instead of
/// a completion. Keep this at or above that floor when new models land.
const CLAUDE_HEADERS: &[(&str, &str)] = &[
    ("anthropic-version", "2023-06-01"),
    (
        "anthropic-beta",
        "oauth-2025-04-20,interleaved-thinking-2025-05-14",
    ),
    ("user-agent", "claude-cli/2.1.318 (external, cli)"),
];

const CODEX_HEADERS: &[(&str, &str)] = &[("originator", "codex_cli_rs"), ("version", "0.159.0")];

const GITHUB_HEADERS: &[(&str, &str)] = &[
    ("copilot-integration-id", "vscode-chat"),
    ("editor-version", "vscode/1.110.0"),
    ("X-Initiator", "user"),
];

const ANTHROPIC_HEADERS: &[(&str, &str)] = &[("anthropic-version", "2023-06-01")];

// ---- sign-in flows ------------------------------------------------------------------
//
// These mirror the login parameters of the official CLIs so a token obtained here is
// an ordinary account token that the same refresh recipe keeps alive.

/// [CC] sign-in: authorization code + PKCE on an ephemeral loopback port.
pub const CLAUDE_LOGIN: LoginFlow = LoginFlow::Pkce(PkceFlow {
    client_id: ANTHROPIC_OAUTH_CLIENT,
    authorize_url: "https://claude.ai/oauth/authorize",
    token_url: "https://api.anthropic.com/v1/oauth/token",
    scope: "org:create_api_key user:profile user:inference",
    fixed_port: None,
    // Claude Code's own CLI registers `http://localhost:<port>/callback`; sending
    // 127.0.0.1 fails with "Redirect URI ... is not supported by client".
    redirect_host: "localhost",
    callback_path: "/callback",
    extra_params: &[("code", "true")],
    send_state: true,
    body: RefreshBody::Json,
});

/// Codex CLI sign-in: authorization code + PKCE on the port its client registered.
pub const CODEX_LOGIN: LoginFlow = LoginFlow::Pkce(PkceFlow {
    client_id: CODEX_OAUTH_CLIENT,
    authorize_url: "https://auth.openai.com/oauth/authorize",
    token_url: "https://auth.openai.com/oauth/token",
    scope: "openid profile email offline_access",
    // The registered redirect URI is bound to this exact port; a dynamic port
    // fails the exchange with a redirect_uri mismatch.
    fixed_port: Some(1455),
    redirect_host: "127.0.0.1",
    callback_path: "/auth/callback",
    extra_params: &[
        ("id_token_add_organizations", "true"),
        ("codex_cli_simplified_flow", "true"),
        ("originator", "codex_cli_rs"),
    ],
    send_state: false,
    body: RefreshBody::Form,
});

/// GitHub Copilot sign-in: device authorization grant.
pub const GITHUB_LOGIN: LoginFlow = LoginFlow::DeviceCode(DeviceFlow {
    client_id: "Iv1.b507a08c87ecfe98",
    device_code_url: "https://github.com/login/device/code",
    token_url: "https://github.com/login/oauth/access_token",
    scope: "read:user",
});

/// Every provider this build can route to.
pub static PROVIDERS: &[ProviderSpec] = &[
    ProviderSpec {
        id: "claude",
        display: "Claude (Anthropic OAuth)",
        kind: ProviderKind::OAuth,
        base_url: "https://api.anthropic.com/v1/messages",
        responses_url: None,
        messages_url: Some("https://api.anthropic.com/v1/messages"),
        images_url: None,
        audio_speech_url: None,
        audio_transcriptions_url: None,
        audio_voices_url: None,
        default_dialect: Dialect::AnthropicMessages,
        auth: AuthStyle::Bearer,
        extra_headers: CLAUDE_HEADERS,
        default_max_tokens: Some(32_000),
        refresh: Some(CLAUDE_REFRESH),
        profile_url: Some("https://api.anthropic.com/api/oauth/profile"),
        stream_only: false,
        login: Some(CLAUDE_LOGIN),
    },
    ProviderSpec {
        id: "codex",
        display: "Codex (ChatGPT OAuth)",
        kind: ProviderKind::OAuth,
        base_url: "https://chatgpt.com/backend-api/codex/responses",
        responses_url: Some("https://chatgpt.com/backend-api/codex/responses"),
        messages_url: None,
        images_url: None,
        audio_speech_url: None,
        audio_transcriptions_url: None,
        audio_voices_url: None,
        default_dialect: Dialect::OpenAiResponses,
        auth: AuthStyle::Bearer,
        extra_headers: CODEX_HEADERS,
        default_max_tokens: None,
        refresh: Some(CODEX_REFRESH),
        login: Some(CODEX_LOGIN),
        profile_url: Some("https://auth.openai.com/userinfo"),
        stream_only: true,
    },
    ProviderSpec {
        id: "github",
        display: "GitHub Copilot",
        kind: ProviderKind::OAuth,
        base_url: "https://api.githubcopilot.com/chat/completions",
        responses_url: Some("https://api.githubcopilot.com/responses"),
        messages_url: Some("https://api.githubcopilot.com/v1/messages"),
        images_url: None,
        audio_speech_url: None,
        audio_transcriptions_url: None,
        audio_voices_url: None,
        default_dialect: Dialect::OpenAiChat,
        auth: AuthStyle::Bearer,
        extra_headers: GITHUB_HEADERS,
        default_max_tokens: None,
        refresh: None,
        login: Some(GITHUB_LOGIN),
        profile_url: Some("https://api.github.com/user"),
        stream_only: false,
    },
    ProviderSpec {
        id: "openai",
        display: "OpenAI",
        kind: ProviderKind::ApiKey,
        base_url: "https://api.openai.com/v1/chat/completions",
        responses_url: Some("https://api.openai.com/v1/responses"),
        messages_url: None,
        images_url: Some("https://api.openai.com/v1/images/generations"),
        audio_speech_url: Some("https://api.openai.com/v1/audio/speech"),
        audio_transcriptions_url: Some("https://api.openai.com/v1/audio/transcriptions"),
        // OpenAI publishes a fixed voice list, not a listing endpoint.
        audio_voices_url: None,
        default_dialect: Dialect::OpenAiChat,
        auth: AuthStyle::Bearer,
        extra_headers: &[],
        default_max_tokens: None,
        refresh: None,
        login: None,
        profile_url: None,
        stream_only: false,
    },
    ProviderSpec {
        id: "anthropic",
        display: "Anthropic",
        kind: ProviderKind::ApiKey,
        base_url: "https://api.anthropic.com/v1/messages",
        responses_url: None,
        messages_url: Some("https://api.anthropic.com/v1/messages"),
        images_url: None,
        audio_speech_url: None,
        audio_transcriptions_url: None,
        audio_voices_url: None,
        default_dialect: Dialect::AnthropicMessages,
        auth: AuthStyle::XApiKey,
        extra_headers: ANTHROPIC_HEADERS,
        default_max_tokens: Some(32_000),
        refresh: None,
        login: None,
        profile_url: None,
        stream_only: false,
    },
    ProviderSpec {
        id: "deepseek",
        display: "DeepSeek",
        kind: ProviderKind::ApiKey,
        base_url: "https://api.deepseek.com/chat/completions",
        responses_url: None,
        messages_url: None,
        images_url: None,
        audio_speech_url: None,
        audio_transcriptions_url: None,
        audio_voices_url: None,
        default_dialect: Dialect::OpenAiChat,
        auth: AuthStyle::Bearer,
        extra_headers: &[],
        default_max_tokens: None,
        refresh: None,
        login: None,
        profile_url: None,
        stream_only: false,
    },
    ProviderSpec {
        id: "gemini",
        display: "Google Gemini",
        kind: ProviderKind::ApiKey,
        base_url: "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions",
        responses_url: None,
        messages_url: None,
        images_url: None,
        audio_speech_url: None,
        audio_transcriptions_url: None,
        audio_voices_url: None,
        default_dialect: Dialect::OpenAiChat,
        auth: AuthStyle::Bearer,
        extra_headers: &[],
        default_max_tokens: None,
        refresh: None,
        login: None,
        profile_url: None,
        stream_only: false,
    },
    ProviderSpec {
        id: "groq",
        display: "Groq",
        kind: ProviderKind::ApiKey,
        base_url: "https://api.groq.com/openai/v1/chat/completions",
        responses_url: None,
        messages_url: None,
        images_url: None,
        audio_speech_url: Some("https://api.groq.com/openai/v1/audio/speech"),
        audio_transcriptions_url: Some("https://api.groq.com/openai/v1/audio/transcriptions"),
        audio_voices_url: None,
        default_dialect: Dialect::OpenAiChat,
        auth: AuthStyle::Bearer,
        extra_headers: &[],
        default_max_tokens: None,
        refresh: None,
        login: None,
        profile_url: None,
        stream_only: false,
    },
    ProviderSpec {
        id: "openrouter",
        display: "OpenRouter",
        kind: ProviderKind::ApiKey,
        base_url: "https://openrouter.ai/api/v1/chat/completions",
        responses_url: None,
        messages_url: None,
        images_url: None,
        audio_speech_url: None,
        audio_transcriptions_url: None,
        audio_voices_url: None,
        default_dialect: Dialect::OpenAiChat,
        auth: AuthStyle::Bearer,
        extra_headers: &[],
        default_max_tokens: None,
        refresh: None,
        login: None,
        profile_url: None,
        stream_only: false,
    },
    ProviderSpec {
        id: "xai",
        display: "xAI",
        kind: ProviderKind::ApiKey,
        base_url: "https://api.x.ai/v1/chat/completions",
        responses_url: None,
        messages_url: None,
        images_url: Some("https://api.x.ai/v1/images/generations"),
        audio_speech_url: None,
        audio_transcriptions_url: None,
        audio_voices_url: None,
        default_dialect: Dialect::OpenAiChat,
        auth: AuthStyle::Bearer,
        extra_headers: &[],
        default_max_tokens: None,
        refresh: None,
        login: None,
        profile_url: None,
        stream_only: false,
    },
    ProviderSpec {
        id: "mistral",
        display: "Mistral",
        kind: ProviderKind::ApiKey,
        base_url: "https://api.mistral.ai/v1/chat/completions",
        responses_url: None,
        messages_url: None,
        images_url: None,
        audio_speech_url: None,
        audio_transcriptions_url: None,
        audio_voices_url: None,
        default_dialect: Dialect::OpenAiChat,
        auth: AuthStyle::Bearer,
        extra_headers: &[],
        default_max_tokens: None,
        refresh: None,
        login: None,
        profile_url: None,
        stream_only: false,
    },
    ProviderSpec {
        id: "together",
        display: "Together AI",
        kind: ProviderKind::ApiKey,
        base_url: "https://api.together.xyz/v1/chat/completions",
        responses_url: None,
        messages_url: None,
        images_url: Some("https://api.together.xyz/v1/images/generations"),
        audio_speech_url: Some("https://api.together.xyz/v1/audio/speech"),
        audio_transcriptions_url: Some("https://api.together.xyz/v1/audio/transcriptions"),
        audio_voices_url: None,
        default_dialect: Dialect::OpenAiChat,
        auth: AuthStyle::Bearer,
        extra_headers: &[],
        default_max_tokens: None,
        refresh: None,
        login: None,
        profile_url: None,
        stream_only: false,
    },
    ProviderSpec {
        id: "cerebras",
        display: "Cerebras",
        kind: ProviderKind::ApiKey,
        base_url: "https://api.cerebras.ai/v1/chat/completions",
        responses_url: None,
        messages_url: None,
        images_url: None,
        audio_speech_url: None,
        audio_transcriptions_url: None,
        audio_voices_url: None,
        default_dialect: Dialect::OpenAiChat,
        auth: AuthStyle::Bearer,
        extra_headers: &[],
        default_max_tokens: None,
        refresh: None,
        login: None,
        profile_url: None,
        stream_only: false,
    },
    ProviderSpec {
        id: "ollama",
        display: "Ollama (local)",
        kind: ProviderKind::Local,
        base_url: "http://localhost:11434/v1/chat/completions",
        responses_url: None,
        messages_url: None,
        images_url: None,
        audio_speech_url: None,
        audio_transcriptions_url: None,
        audio_voices_url: None,
        default_dialect: Dialect::OpenAiChat,
        auth: AuthStyle::None,
        extra_headers: &[],
        default_max_tokens: None,
        refresh: None,
        login: None,
        profile_url: None,
        stream_only: false,
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

/// Return a provider's login flow with configured endpoint overrides applied.
pub fn login_flow_for(provider: &str) -> Option<LoginFlow> {
    let flow = get(provider)?.login?;
    Some(match flow {
        LoginFlow::Pkce(mut p) => {
            if let Some(url) = override_for(provider, OverrideKind::Authorize) {
                p.authorize_url = url;
            }
            if let Some(url) = override_for(provider, OverrideKind::Token) {
                p.token_url = url;
            }
            LoginFlow::Pkce(p)
        }
        LoginFlow::DeviceCode(mut d) => {
            if let Some(url) = override_for(provider, OverrideKind::Token) {
                d.token_url = url;
            }
            if let Some(url) = override_for(provider, OverrideKind::Device) {
                d.device_code_url = url;
            }
            LoginFlow::DeviceCode(d)
        }
    })
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

    #[test]
    fn endpoint_overrides_apply_and_reject_insecure_hosts() {
        let output = std::process::Command::new(std::env::current_exe().expect("test executable"))
            .arg("--exact")
            .arg("registry::tests::override_probe")
            .arg("--nocapture")
            .env(
                "RROUTER_OAUTH_OVERRIDE_CLAUDE_AUTHORIZE",
                "https://oauth.example.test/authorize",
            )
            .env(
                "RROUTER_OAUTH_OVERRIDE_CODEX_TOKEN",
                "http://oauth.example.test/token",
            )
            .env_remove("RROUTER_ALLOW_INSECURE_OAUTH")
            .output()
            .expect("run override probe");
        assert!(
            output.status.success(),
            "override probe failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn override_probe() {
        if std::env::var("RROUTER_OAUTH_OVERRIDE_CLAUDE_AUTHORIZE").as_deref()
            != Ok("https://oauth.example.test/authorize")
        {
            return;
        }
        let flow = login_flow_for("claude").expect("claude login flow");
        let LoginFlow::Pkce(flow) = flow else {
            panic!("claude must use PKCE");
        };
        assert_eq!(flow.authorize_url, "https://oauth.example.test/authorize");
        assert_eq!(flow.token_url, "https://api.anthropic.com/v1/oauth/token");
        assert!(crate::invalid_overrides()
            .iter()
            .any(|entry| entry.contains("RROUTER_OAUTH_OVERRIDE_CODEX_TOKEN")));
    }
}
