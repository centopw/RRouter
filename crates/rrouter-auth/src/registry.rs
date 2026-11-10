//! Static provider registry: transport (base URL, dialect), auth style, extra headers and
//! refresh recipe for every provider RRouter can route to.
//!
//! The table is `const` data; the id → spec index is a `LazyLock<HashMap>` built once.
//! These transports are verified against the providers' public APIs; changing one is a
//! routing change, not a refactor.

use std::collections::HashMap;
use std::sync::LazyLock;

use rrouter_core::Dialect;

use crate::overrides::{override_for, OverrideKind};
use rrouter_translate::search::SearchMode;
use rrouter_translate::{MaxTokensKey, WireProfile};

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
    /// `<ProviderSpec::auth_header>: <key>`. Azure OpenAI uses `api-key`; a manifest names
    /// the header, and `authorization` is refused there because `Bearer` already covers it.
    ApiKeyHeader,
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
    /// Client secret for endpoints that authenticate a confidential client (Google).
    /// `None` for public PKCE-only clients. `GOOGLE_ANTIGRAVITY_CLIENT_SECRET` overrides
    /// the registered value at runtime.
    pub client_secret: Option<&'static str>,
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
    /// Client secret sent with the code exchange, when the registered client is
    /// confidential (Google). `None` for public PKCE clients.
    pub client_secret: Option<&'static str>,
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
    /// Full chat endpoint URL, no trailing slash. When `base_url_requires_override` is set
    /// this is a documented placeholder, not a callable endpoint.
    pub base_url: &'static str,
    /// The host above names a tenant the manifest cannot know (Azure's
    /// `RESOURCE.openai.azure.com`), so every connection supplies its own base URL.
    /// [`crate::base_url`] refuses to dispatch or save one that does not.
    pub base_url_requires_override: bool,
    /// Native Responses endpoint, when the provider speaks it.
    pub responses_url: Option<&'static str>,
    /// Explicit model-list URL when it cannot be derived from the inference base.
    pub models_url: Option<&'static str>,
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
    /// Header that carries the key when `auth` is [`AuthStyle::ApiKeyHeader`]; empty for
    /// every other style. Azure OpenAI names it `api-key`.
    pub auth_header: &'static str,
    /// Static query parameters every request to this provider carries, reviewed with the
    /// transport. Azure's `api-version` is the case this exists for; a parameter the
    /// connection's base URL already sets wins (see [`crate::query::with_query`]).
    pub query: &'static [(&'static str, &'static str)],
    /// Key the Chat dialect uses for the output cap. `max_completion_tokens` only on
    /// OpenAI's own surface; everything else `max_tokens`, which Mistral-class APIs
    /// require and OpenAI's current models reject.
    pub max_tokens_key: MaxTokensKey,
    /// Client protocol beta headers this provider understands. The gateway forwards only
    /// these names, so a client's credential headers can never travel with them.
    pub passthrough_headers: &'static [&'static str],
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

    /// Native rerank endpoint, when the provider serves `/v1/rerank` itself.
    ///
    /// Kept out of the struct because it is a separate capability surface, not part of the
    /// chat transport: the manifest declares it and [`rerank_endpoint`] resolves it.
    pub fn rerank_url(&self) -> Option<&'static str> {
        rerank_endpoint(self.id)
    }

    /// OpenAI-compatible file API endpoint, when the provider serves one.
    pub fn files_url(&self) -> Option<&'static str> {
        files_endpoint(self.id)
    }

    /// OpenAI-compatible batch API endpoint, when the provider serves one.
    pub fn batches_url(&self) -> Option<&'static str> {
        batches_endpoint(self.id)
    }

    /// Request shaping this provider's wire surface demands.
    ///
    /// Read by the proxy before it serializes a translated request, so the output cap is
    /// spelled the way this provider accepts it and an omitted cap becomes the provider's
    /// configured default instead of a hardcoded one.
    pub fn wire_profile(&self) -> WireProfile {
        WireProfile::new(self.max_tokens_key, self.default_max_tokens)
    }
}

const ANTHROPIC_OAUTH_CLIENT: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const CODEX_OAUTH_CLIENT: &str = "app_EMoamEEZ73f0CkXaXp7hrann";

const CLAUDE_REFRESH: RefreshSpec = RefreshSpec {
    token_url: "https://api.anthropic.com/v1/oauth/token",
    client_id: ANTHROPIC_OAUTH_CLIENT,
    client_secret: None,
    body: RefreshBody::Json,
};

const CODEX_REFRESH: RefreshSpec = RefreshSpec {
    token_url: "https://auth.openai.com/oauth/token",
    client_id: CODEX_OAUTH_CLIENT,
    client_secret: None,
    body: RefreshBody::Form,
};

/// Google Antigravity: the Google token endpoint authenticates both the code exchange
/// and the refresh with an operator-supplied confidential client. The static fields are
/// a non-secret placeholder; the runtime values come from `GOOGLE_ANTIGRAVITY_*`.
const GOOGLE_ANTIGRAVITY_REFRESH: RefreshSpec = RefreshSpec {
    token_url: crate::antigravity::TOKEN_URL,
    client_id: crate::antigravity::PLACEHOLDER_CLIENT,
    client_secret: Some(crate::antigravity::PLACEHOLDER_CLIENT),
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
    client_secret: None,
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
    client_secret: None,
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

/// Google Antigravity sign-in: authorization code + PKCE on the registered fixed port.
///
/// The client id/secret are operator-supplied (`GOOGLE_ANTIGRAVITY_CLIENT_ID`,
/// `GOOGLE_ANTIGRAVITY_CLIENT_SECRET`) and replace these placeholders at runtime; a
/// missing value is a 400 before any listener or request exists. `access_type=offline`
/// is what makes Google issue a refresh token; `prompt=consent` forces it on a repeat
/// sign-in.
pub const GOOGLE_ANTIGRAVITY_LOGIN: LoginFlow = LoginFlow::Pkce(PkceFlow {
    client_id: crate::antigravity::PLACEHOLDER_CLIENT,
    client_secret: Some(crate::antigravity::PLACEHOLDER_CLIENT),
    authorize_url: crate::antigravity::AUTHORIZE_URL,
    token_url: crate::antigravity::TOKEN_URL,
    scope: crate::antigravity::SCOPES,
    fixed_port: Some(crate::antigravity::CALLBACK_PORT),
    redirect_host: "localhost",
    callback_path: crate::antigravity::CALLBACK_PATH,
    extra_params: &[("access_type", "offline"), ("prompt", "consent")],
    send_state: false,
    body: RefreshBody::Form,
});

/// Every provider this build can route to.
pub static PROVIDERS: &[ProviderSpec] = &[
    ProviderSpec {
        id: "claude",
        display: "Claude (Anthropic OAuth)",
        kind: ProviderKind::OAuth,
        base_url: "https://api.anthropic.com/v1/messages",
        base_url_requires_override: false,
        responses_url: None,
        models_url: None,
        messages_url: Some("https://api.anthropic.com/v1/messages"),
        images_url: None,
        audio_speech_url: None,
        audio_transcriptions_url: None,
        audio_voices_url: None,
        default_dialect: Dialect::AnthropicMessages,
        auth: AuthStyle::Bearer,
        extra_headers: CLAUDE_HEADERS,
        auth_header: "",
        query: &[],
        max_tokens_key: MaxTokensKey::MaxTokens,
        // 1M context, interleaved thinking and fine-grained tool streaming are `beta`
        // flags only the client knows it needs.
        passthrough_headers: &["anthropic-beta"],
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
        base_url_requires_override: false,
        responses_url: Some("https://chatgpt.com/backend-api/codex/responses"),
        models_url: None,
        messages_url: None,
        images_url: None,
        audio_speech_url: None,
        audio_transcriptions_url: None,
        audio_voices_url: None,
        default_dialect: Dialect::OpenAiResponses,
        auth: AuthStyle::Bearer,
        extra_headers: CODEX_HEADERS,
        auth_header: "",
        query: &[],
        max_tokens_key: MaxTokensKey::MaxCompletionTokens,
        passthrough_headers: &["openai-beta"],
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
        base_url_requires_override: false,
        responses_url: Some("https://api.githubcopilot.com/responses"),
        models_url: None,
        messages_url: Some("https://api.githubcopilot.com/v1/messages"),
        images_url: None,
        audio_speech_url: None,
        audio_transcriptions_url: None,
        audio_voices_url: None,
        default_dialect: Dialect::OpenAiChat,
        auth: AuthStyle::Bearer,
        extra_headers: GITHUB_HEADERS,
        auth_header: "",
        query: &[],
        // Copilot proxies OpenAI models, so it needs the same cap key — but its beta header
        // vocabulary is undocumented, and guessing one is worse than forwarding none.
        max_tokens_key: MaxTokensKey::MaxCompletionTokens,
        passthrough_headers: &[],
        default_max_tokens: None,
        refresh: None,
        login: Some(GITHUB_LOGIN),
        profile_url: Some("https://api.github.com/user"),
        stream_only: false,
    },
    ProviderSpec {
        id: crate::antigravity::PROVIDER_ID,
        display: "Google Antigravity",
        kind: ProviderKind::OAuth,
        // Daily origin. The proxy appends the CCA streaming/buffered path and retries on
        // the production host; `RROUTER_ANTIGRAVITY_DAILY_HOST` overrides it.
        base_url: rrouter_translate::google_cca::DAILY_HOST,
        base_url_requires_override: false,
        responses_url: None,
        // Model discovery is `POST /v1internal:fetchAvailableModels` behind a project and
        // an OAuth token, not a `GET /models` surface, so no URL can be derived here.
        // Model ids come from the router catalog; see docs/PROVIDERS.md.
        models_url: None,
        messages_url: None,
        images_url: None,
        audio_speech_url: None,
        audio_transcriptions_url: None,
        audio_voices_url: None,
        default_dialect: Dialect::GoogleCodeAssist,
        auth: AuthStyle::Bearer,
        // The proxy sets `user-agent: antigravity` and `x-goog-api-client` on every CCA
        // call; the OAuth endpoints need no extra headers.
        extra_headers: &[],
        auth_header: "",
        query: &[],
        // CCA spells the cap `maxOutputTokens` inside `generationConfig`; WireProfile is
        // only read for canonical chat dialects, so this value never reaches the wire.
        max_tokens_key: MaxTokensKey::MaxTokens,
        passthrough_headers: &[],
        default_max_tokens: None,
        refresh: Some(GOOGLE_ANTIGRAVITY_REFRESH),
        login: Some(GOOGLE_ANTIGRAVITY_LOGIN),
        profile_url: Some(crate::antigravity::PROFILE_URL),
        stream_only: false,
    },
    ProviderSpec {
        id: "openai",
        display: "OpenAI",
        kind: ProviderKind::ApiKey,
        base_url: "https://api.openai.com/v1/chat/completions",
        base_url_requires_override: false,
        responses_url: Some("https://api.openai.com/v1/responses"),
        models_url: None,
        messages_url: None,
        images_url: Some("https://api.openai.com/v1/images/generations"),
        audio_speech_url: Some("https://api.openai.com/v1/audio/speech"),
        audio_transcriptions_url: Some("https://api.openai.com/v1/audio/transcriptions"),
        // OpenAI publishes a fixed voice list, not a listing endpoint.
        audio_voices_url: None,
        default_dialect: Dialect::OpenAiChat,
        auth: AuthStyle::Bearer,
        extra_headers: &[],
        auth_header: "",
        query: &[],
        // OpenAI's own surface: current gpt-5/o-series models reject `max_tokens`.
        max_tokens_key: MaxTokensKey::MaxCompletionTokens,
        passthrough_headers: &["openai-beta"],
        default_max_tokens: None,
        refresh: None,
        login: None,
        profile_url: None,
        stream_only: false,
    },
    ProviderSpec {
        id: "opencode",
        display: "OpenCode",
        kind: ProviderKind::ApiKey,
        // OpenCode translates every text model through its OpenAI-compatible surface.
        base_url: "https://opencode.ai/inference/openai/v1/chat/completions",
        base_url_requires_override: false,
        responses_url: Some("https://opencode.ai/inference/openai/v1/responses"),
        models_url: Some("https://opencode.ai/inference/v1/models"),
        messages_url: None,
        images_url: None,
        audio_speech_url: None,
        audio_transcriptions_url: None,
        audio_voices_url: None,
        default_dialect: Dialect::OpenAiChat,
        auth: AuthStyle::Bearer,
        extra_headers: &[],
        auth_header: "",
        query: &[],
        // An aggregator: the widest-common cap spelling, and it passes betas through.
        max_tokens_key: MaxTokensKey::MaxTokens,
        passthrough_headers: &["openai-beta"],
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
        base_url_requires_override: false,
        responses_url: None,
        models_url: None,
        messages_url: Some("https://api.anthropic.com/v1/messages"),
        images_url: None,
        audio_speech_url: None,
        audio_transcriptions_url: None,
        audio_voices_url: None,
        default_dialect: Dialect::AnthropicMessages,
        auth: AuthStyle::XApiKey,
        extra_headers: ANTHROPIC_HEADERS,
        auth_header: "",
        query: &[],
        max_tokens_key: MaxTokensKey::MaxTokens,
        passthrough_headers: &["anthropic-beta"],
        default_max_tokens: Some(32_000),
        refresh: None,
        login: None,
        profile_url: None,
        stream_only: false,
    },
    ProviderSpec {
        id: "opencode-go",
        display: "OpenCode Go",
        kind: ProviderKind::ApiKey,
        // Go is a separate plan served from its own base URL. The paid OpenCode base
        // cannot serve Go models, so this is a distinct provider rather than a model
        // special case in the chat transport.
        base_url: "https://opencode.ai/inference/go/openai/v1/chat/completions",
        base_url_requires_override: false,
        responses_url: Some("https://opencode.ai/inference/go/openai/v1/responses"),
        models_url: Some("https://opencode.ai/inference/go/v1/models"),
        // The Go Anthropic lane authenticates with x-api-key, not the Bearer style this
        // spec uses; leave it out and let a Messages client translate through Chat.
        messages_url: None,
        images_url: None,
        audio_speech_url: None,
        audio_transcriptions_url: None,
        audio_voices_url: None,
        default_dialect: Dialect::OpenAiChat,
        auth: AuthStyle::Bearer,
        extra_headers: &[],
        auth_header: "",
        query: &[],
        max_tokens_key: MaxTokensKey::MaxTokens,
        passthrough_headers: &["openai-beta"],
        default_max_tokens: None,
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
        base_url_requires_override: false,
        responses_url: None,
        models_url: None,
        messages_url: None,
        images_url: None,
        audio_speech_url: None,
        audio_transcriptions_url: None,
        audio_voices_url: None,
        default_dialect: Dialect::OpenAiChat,
        auth: AuthStyle::Bearer,
        extra_headers: &[],
        auth_header: "",
        query: &[],
        max_tokens_key: MaxTokensKey::MaxTokens,
        passthrough_headers: &[],
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
        base_url_requires_override: false,
        responses_url: None,
        models_url: None,
        messages_url: None,
        images_url: None,
        audio_speech_url: None,
        audio_transcriptions_url: None,
        audio_voices_url: None,
        default_dialect: Dialect::OpenAiChat,
        auth: AuthStyle::Bearer,
        extra_headers: &[],
        auth_header: "",
        query: &[],
        max_tokens_key: MaxTokensKey::MaxTokens,
        passthrough_headers: &[],
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
        base_url_requires_override: false,
        responses_url: None,
        models_url: None,
        messages_url: None,
        images_url: None,
        audio_speech_url: Some("https://api.groq.com/openai/v1/audio/speech"),
        audio_transcriptions_url: Some("https://api.groq.com/openai/v1/audio/transcriptions"),
        audio_voices_url: None,
        default_dialect: Dialect::OpenAiChat,
        auth: AuthStyle::Bearer,
        extra_headers: &[],
        auth_header: "",
        query: &[],
        max_tokens_key: MaxTokensKey::MaxTokens,
        passthrough_headers: &[],
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
        base_url_requires_override: false,
        responses_url: None,
        models_url: None,
        messages_url: None,
        images_url: None,
        audio_speech_url: None,
        audio_transcriptions_url: None,
        audio_voices_url: None,
        default_dialect: Dialect::OpenAiChat,
        auth: AuthStyle::Bearer,
        extra_headers: &[],
        auth_header: "",
        query: &[],
        max_tokens_key: MaxTokensKey::MaxTokens,
        passthrough_headers: &[],
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
        base_url_requires_override: false,
        responses_url: Some("https://api.x.ai/v1/responses"),
        models_url: None,
        messages_url: None,
        images_url: Some("https://api.x.ai/v1/images/generations"),
        audio_speech_url: None,
        audio_transcriptions_url: None,
        audio_voices_url: None,
        default_dialect: Dialect::OpenAiChat,
        auth: AuthStyle::Bearer,
        extra_headers: &[],
        auth_header: "",
        query: &[],
        max_tokens_key: MaxTokensKey::MaxTokens,
        passthrough_headers: &[],
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
        base_url_requires_override: false,
        responses_url: None,
        models_url: None,
        messages_url: None,
        images_url: None,
        audio_speech_url: None,
        audio_transcriptions_url: None,
        audio_voices_url: None,
        default_dialect: Dialect::OpenAiChat,
        auth: AuthStyle::Bearer,
        extra_headers: &[],
        auth_header: "",
        query: &[],
        max_tokens_key: MaxTokensKey::MaxTokens,
        passthrough_headers: &[],
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
        base_url_requires_override: false,
        responses_url: None,
        models_url: None,
        messages_url: None,
        images_url: Some("https://api.together.xyz/v1/images/generations"),
        audio_speech_url: Some("https://api.together.xyz/v1/audio/speech"),
        audio_transcriptions_url: Some("https://api.together.xyz/v1/audio/transcriptions"),
        audio_voices_url: None,
        default_dialect: Dialect::OpenAiChat,
        auth: AuthStyle::Bearer,
        extra_headers: &[],
        auth_header: "",
        query: &[],
        max_tokens_key: MaxTokensKey::MaxTokens,
        passthrough_headers: &[],
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
        base_url_requires_override: false,
        responses_url: None,
        models_url: None,
        messages_url: None,
        images_url: None,
        audio_speech_url: None,
        audio_transcriptions_url: None,
        audio_voices_url: None,
        default_dialect: Dialect::OpenAiChat,
        auth: AuthStyle::Bearer,
        extra_headers: &[],
        auth_header: "",
        query: &[],
        max_tokens_key: MaxTokensKey::MaxTokens,
        passthrough_headers: &[],
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
        base_url_requires_override: false,
        responses_url: None,
        models_url: None,
        messages_url: None,
        images_url: None,
        audio_speech_url: None,
        audio_transcriptions_url: None,
        audio_voices_url: None,
        default_dialect: Dialect::OpenAiChat,
        auth: AuthStyle::None,
        extra_headers: &[],
        auth_header: "",
        query: &[],
        max_tokens_key: MaxTokensKey::MaxTokens,
        passthrough_headers: &[],
        default_max_tokens: None,
        refresh: None,
        login: None,
        profile_url: None,
        stream_only: false,
    },
];

// Reviewed community manifests, generated at build time from `providers/community`.
include!(concat!(env!("OUT_DIR"), "/community.rs"));

/// Every provider, core and community. OAuth flows only exist in the core slice.
pub fn all() -> impl Iterator<Item = &'static ProviderSpec> {
    PROVIDERS.iter().chain(COMMUNITY_PROVIDERS.iter())
}

static BY_ID: LazyLock<HashMap<&'static str, &'static ProviderSpec>> = LazyLock::new(|| {
    let all: Vec<_> = all().collect();
    let mut map = HashMap::with_capacity(all.len());
    for spec in all {
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

/// One provider's native web-search surface.
///
/// Search is a separate capability, not part of the chat transport: the provider runs it
/// inside an existing model endpoint, and the gateway only needs to know which wire body
/// that endpoint expects and which model to use when the client names the provider alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchSpec {
    pub mode: SearchMode,
    /// Model used when the client sends a provider id (`claude`) instead of
    /// `provider/model`. Chosen from the provider's catalog and overridable per request.
    pub default_model: &'static str,
}

/// Native web search for `provider`, when its model API can search the web.
///
/// The list is deliberately explicit. A provider is added here only after its API
/// documents a hosted search tool; an unknown provider answers `501` on `/v1/search`
/// instead of receiving a guessed tool or a different endpoint.
pub fn search_spec(provider: &str) -> Option<SearchSpec> {
    match provider {
        "claude" | "anthropic" => Some(SearchSpec {
            mode: SearchMode::Anthropic,
            default_model: "claude-sonnet-4-5",
        }),
        "openai" => Some(SearchSpec {
            mode: SearchMode::OpenAiResponses,
            default_model: "gpt-5-mini",
        }),
        "codex" => Some(SearchSpec {
            mode: SearchMode::OpenAiResponses,
            default_model: "gpt-5.6-luna",
        }),
        "xai" => Some(SearchSpec {
            mode: SearchMode::OpenAiResponses,
            default_model: "grok-4",
        }),
        "gemini" => Some(SearchSpec {
            mode: SearchMode::Gemini,
            default_model: "gemini-2.5-flash",
        }),
        _ => None,
    }
}

/// Core providers that serve `/v1/rerank`. Empty today: the capability ships through
/// community manifests, and this table is where a built-in provider would join.
const CORE_RERANK: &[(&str, &str)] = &[];

/// Native rerank endpoint for `provider`, when it serves `/v1/rerank`.
///
/// The manifest is the single source: `build.rs` compiles every `rerank_url` into
/// `COMMUNITY_RERANK`, and the round-trip test requires the two to agree. A provider
/// without one answers `501` on that route rather than having a path guessed for it.
pub fn rerank_endpoint(provider: &str) -> Option<&'static str> {
    CORE_RERANK
        .iter()
        .chain(COMMUNITY_RERANK.iter())
        .find(|(id, _)| *id == provider)
        .map(|(_, url)| *url)
}

/// Core providers with an OpenAI-compatible file API.
///
/// OpenAI is the only one: Together serves uploads at `/v1/files/upload` (not `/v1/files`),
/// Mistral's batch API is `/v1/batch` with a different body, and Azure's files/batches are
/// host- and version-scoped. Every other reviewed provider publishes no such API, so none
/// is listed — a missing endpoint answers `501` instead of a guessed path.
const CORE_FILES: &[(&str, &str)] = &[("openai", "https://api.openai.com/v1/files")];

/// Core providers with an OpenAI-compatible batch API. OpenAI only, for the reasons on
/// [`CORE_FILES`].
const CORE_BATCHES: &[(&str, &str)] = &[("openai", "https://api.openai.com/v1/batches")];

fn capability(
    table: &'static [(&'static str, &'static str)],
    provider: &str,
) -> Option<&'static str> {
    table
        .iter()
        .find(|(id, _)| *id == provider)
        .map(|(_, url)| *url)
}

/// OpenAI-compatible file endpoint for `provider`, when it serves one.
///
/// A manifest is the source for every community provider: `build.rs` compiles each
/// `files_url` into `COMMUNITY_FILES`.
pub fn files_endpoint(provider: &str) -> Option<&'static str> {
    capability(CORE_FILES, provider).or_else(|| capability(COMMUNITY_FILES, provider))
}

/// OpenAI-compatible batch endpoint for `provider`, when it serves one.
pub fn batches_endpoint(provider: &str) -> Option<&'static str> {
    capability(CORE_BATCHES, provider).or_else(|| capability(COMMUNITY_BATCHES, provider))
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
        let all: Vec<_> = all().collect();
        for (i, spec) in all.iter().enumerate() {
            assert!(
                get(spec.id).is_some_and(|s| std::ptr::eq(s, *spec)),
                "{} not reachable",
                spec.id
            );
            assert_eq!(BY_ID.len(), all.len());
            assert!(
                all[..i].iter().all(|p| p.id != spec.id),
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
        assert!(is_oauth("google-antigravity"));
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
    fn opencode_go_is_a_separate_transport_from_paid_opencode() {
        let go = get("opencode-go").expect("opencode-go");
        assert_eq!(go.display, "OpenCode Go");
        assert_eq!(
            go.base_url,
            "https://opencode.ai/inference/go/openai/v1/chat/completions"
        );
        assert_eq!(
            go.responses_url,
            Some("https://opencode.ai/inference/go/openai/v1/responses")
        );
        assert_eq!(
            go.models_url,
            Some("https://opencode.ai/inference/go/v1/models")
        );
        assert_eq!(go.messages_url, None);
        assert_eq!(go.auth, AuthStyle::Bearer);
        assert_eq!(go.default_dialect, Dialect::OpenAiChat);

        let paid = get("opencode").expect("opencode");
        assert_eq!(
            paid.base_url,
            "https://opencode.ai/inference/openai/v1/chat/completions"
        );
        assert_ne!(paid.base_url, go.base_url);
    }

    #[test]
    fn native_search_specs_name_an_endpoint_and_a_default_model() {
        for provider in ["claude", "anthropic", "openai", "codex", "xai", "gemini"] {
            let spec = get(provider).unwrap_or_else(|| panic!("{provider}"));
            let search = search_spec(provider).unwrap_or_else(|| panic!("{provider} search"));
            assert!(!search.default_model.is_empty(), "{provider}");
            match search.mode {
                SearchMode::Anthropic => assert!(
                    spec.messages_url.is_some(),
                    "{provider} searches through Messages"
                ),
                SearchMode::OpenAiResponses => assert!(
                    spec.responses_url.is_some(),
                    "{provider} searches through Responses"
                ),
                SearchMode::Gemini => {}
            }
        }
        assert!(search_spec("groq").is_none());
        assert_eq!(
            get("xai").expect("xai").responses_url,
            Some("https://api.x.ai/v1/responses")
        );
    }

    #[test]
    fn google_antigravity_is_a_cca_oauth_provider() {
        let spec = get("google-antigravity").expect("google-antigravity");
        assert_eq!(spec.display, "Google Antigravity");
        assert_eq!(spec.kind, ProviderKind::OAuth);
        assert_eq!(spec.auth, AuthStyle::Bearer);
        assert_eq!(spec.default_dialect, Dialect::GoogleCodeAssist);
        assert_eq!(spec.base_url, "https://daily-cloudcode-pa.googleapis.com");
        assert_eq!(spec.profile_url, Some(crate::antigravity::PROFILE_URL));
        assert_eq!(spec.models_url, None);

        let LoginFlow::Pkce(login) = spec.login.expect("login") else {
            panic!("antigravity signs in with PKCE");
        };
        assert_eq!(
            login.authorize_url,
            "https://accounts.google.com/o/oauth2/v2/auth"
        );
        assert_eq!(login.token_url, "https://oauth2.googleapis.com/token");
        assert_eq!(login.fixed_port, Some(51121));
        assert_eq!(login.redirect_host, "localhost");
        assert_eq!(login.callback_path, "/oauth-callback");
        assert_eq!(login.body, RefreshBody::Form);
        // The static row must never carry a real credential.
        assert_eq!(login.client_id, crate::antigravity::PLACEHOLDER_CLIENT);
        assert_eq!(
            login.client_secret,
            Some(crate::antigravity::PLACEHOLDER_CLIENT)
        );
        for scope in [
            "cloud-platform",
            "userinfo.email",
            "userinfo.profile",
            "cclog",
            "experimentsandconfigs",
        ] {
            assert!(
                login.scope.contains(scope),
                "missing scope {scope}: {}",
                login.scope
            );
        }
        assert!(login.extra_params.contains(&("access_type", "offline")));
        assert!(login.extra_params.contains(&("prompt", "consent")));

        let refresh = spec.refresh.expect("refresh");
        assert_eq!(refresh.token_url, "https://oauth2.googleapis.com/token");
        assert_eq!(refresh.client_id, crate::antigravity::PLACEHOLDER_CLIENT);
        assert_eq!(
            refresh.client_secret,
            Some(crate::antigravity::PLACEHOLDER_CLIENT)
        );
        assert_eq!(refresh.body, RefreshBody::Form);
    }

    #[test]
    fn wire_shape_is_declared_for_every_provider() {
        for spec in all() {
            if spec.auth == AuthStyle::ApiKeyHeader {
                assert!(
                    !spec.auth_header.is_empty(),
                    "{} uses api-key auth but names no header",
                    spec.id
                );
                assert!(
                    spec.auth_header
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
                    "{} auth_header `{}` is not a header name",
                    spec.id,
                    spec.auth_header
                );
                assert_ne!(
                    spec.auth_header, "authorization",
                    "{} must use bearer auth for Authorization",
                    spec.id
                );
            } else {
                assert!(
                    spec.auth_header.is_empty(),
                    "{} names an auth header without api-key auth",
                    spec.id
                );
            }
            for (name, value) in spec.query {
                assert!(
                    !name.is_empty()
                        && !value.is_empty()
                        && value.chars().all(
                            |c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '~' | '-')
                        ),
                    "{} query `{name}` is not a literal parameter",
                    spec.id
                );
            }
            // The allowlist is the only way a client header reaches a provider, so it must
            // never name a credential or hop-by-hop header.
            for name in spec.passthrough_headers {
                assert!(
                    matches!(*name, "anthropic-beta" | "openai-beta"),
                    "{} would forward {name}",
                    spec.id
                );
            }
        }
    }

    #[test]
    fn only_the_openai_surface_uses_max_completion_tokens() {
        for spec in all() {
            let expected = match spec.id {
                "openai" | "codex" | "github" | "azure" => MaxTokensKey::MaxCompletionTokens,
                _ => MaxTokensKey::MaxTokens,
            };
            assert_eq!(spec.max_tokens_key, expected, "{}", spec.id);
        }
        // The two facts the proxy needs come from the same place.
        let claude = get("claude").expect("claude").wire_profile();
        assert_eq!(claude.max_tokens_key, MaxTokensKey::MaxTokens);
        assert_eq!(claude.default_max_tokens, Some(32_000));
        assert_eq!(
            get("mistral")
                .expect("mistral")
                .wire_profile()
                .default_max_tokens,
            None
        );
    }

    #[test]
    fn passthrough_headers_are_protocol_betas_only() {
        assert_eq!(
            get("claude").expect("claude").passthrough_headers,
            &["anthropic-beta"]
        );
        assert_eq!(
            get("openai").expect("openai").passthrough_headers,
            &["openai-beta"]
        );
        assert!(get("mistral")
            .expect("mistral")
            .passthrough_headers
            .is_empty());
        assert!(get("github")
            .expect("github")
            .passthrough_headers
            .is_empty());
    }

    #[test]
    fn only_openai_declares_files_and_batches() {
        assert_eq!(
            get("openai").expect("openai").files_url(),
            Some("https://api.openai.com/v1/files")
        );
        assert_eq!(
            get("openai").expect("openai").batches_url(),
            Some("https://api.openai.com/v1/batches")
        );
        assert_eq!(
            files_endpoint("openai"),
            Some("https://api.openai.com/v1/files")
        );
        assert_eq!(
            batches_endpoint("openai"),
            Some("https://api.openai.com/v1/batches")
        );

        for id in ["mistral", "together", "claude", "azure", "cohere", "jina"] {
            let spec = get(id).expect(id);
            assert_eq!(spec.files_url(), None, "{id}");
            assert_eq!(spec.batches_url(), None, "{id}");
            assert_eq!(files_endpoint(id), None, "{id}");
            assert_eq!(batches_endpoint(id), None, "{id}");
        }

        // Every table entry names a registered provider, and the URL is exact.
        for (id, url) in CORE_FILES.iter().chain(CORE_BATCHES) {
            assert!(get(id).is_some(), "{id} is not a provider");
            assert!(url.starts_with("https://"), "{id}");
            assert!(!url.ends_with('/') && !url.contains('?'), "{id}");
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
