//! Header set for one outbound call, already including auth.

use crate::credentials::Credentials;
use crate::registry::{AuthStyle, ProviderKind, ProviderSpec};

/// Build the full header list for one upstream request to `provider` with `c`.
///
/// Auth selection: `github` → `copilot_token`; OAuth → `access_token`; `XApiKey` → `api_key`
/// in `x-api-key`; `None` → nothing. A missing credential is an error, never an unauthenticated
/// request: sending one would burn a retryable upstream error and mask a local bug.
pub fn build_headers(provider: &ProviderSpec, c: &Credentials) -> rrouter_core::Result<Vec<(String, String)>> {
    let mut headers: Vec<(String, String)> = Vec::with_capacity(provider.extra_headers.len() + 2);

    match provider.auth {
        AuthStyle::Bearer => {
            let token = match provider.kind {
                ProviderKind::OAuth if provider.id == "github" => c.copilot_token.as_deref(),
                ProviderKind::OAuth => c.access_token.as_deref(),
                // API-key provider with Bearer auth: the key is the bearer token.
                ProviderKind::ApiKey | ProviderKind::Local => c
                    .api_key
                    .as_deref()
                    .or(c.access_token.as_deref()),
            };
            let token = token.ok_or_else(|| missing(provider, "bearer token"))?;
            headers.push(("authorization".to_string(), format!("Bearer {token}")));
        }
        AuthStyle::XApiKey => {
            let key = c
                .api_key
                .as_deref()
                .or(c.access_token.as_deref())
                .ok_or_else(|| missing(provider, "api key"))?;
            headers.push(("x-api-key".to_string(), key.to_string()));
        }
        AuthStyle::None => {}
    }

    for (name, value) in provider.extra_headers {
        headers.push(((*name).to_string(), (*value).to_string()));
    }
    Ok(headers)
}

/// Endpoint to call for this account: the credential's `base_url_override` wins.
pub fn resolve_base_url<'a>(provider: &'a ProviderSpec, c: &'a Credentials) -> &'a str {
    provider.base_url_for(c.base_url_override.as_deref())
}

fn missing(provider: &ProviderSpec, what: &str) -> rrouter_core::Error {
    rrouter_core::Error::Unauthorized(format!(
        "provider {} credential has no {what}",
        provider.id
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry;

    fn full() -> Credentials {
        Credentials {
            access_token: Some("at".to_string()),
            api_key: Some("sk-key".to_string()),
            copilot_token: Some("cp".to_string()),
            ..Credentials::default()
        }
    }

    fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
        headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    #[test]
    fn github_uses_copilot_token() {
        let headers =
            build_headers(registry::get("github").expect("github"), &full()).expect("headers");
        assert_eq!(header(&headers, "authorization"), Some("Bearer cp"));
        assert_eq!(header(&headers, "copilot-integration-id"), Some("vscode-chat"));
        assert_eq!(header(&headers, "editor-version"), Some("vscode/1.110.0"));
        assert_eq!(header(&headers, "X-Initiator"), Some("user"));
    }

    #[test]
    fn anthropic_uses_x_api_key() {
        let headers =
            build_headers(registry::get("anthropic").expect("anthropic"), &full()).expect("h");
        assert_eq!(header(&headers, "x-api-key"), Some("sk-key"));
        assert!(header(&headers, "authorization").is_none());
        assert_eq!(header(&headers, "anthropic-version"), Some("2023-06-01"));
    }

    #[test]
    fn claude_oauth_uses_bearer_and_oauth_beta() {
        let headers = build_headers(registry::get("claude").expect("claude"), &full()).expect("h");
        assert_eq!(header(&headers, "authorization"), Some("Bearer at"));
        assert!(header(&headers, "x-api-key").is_none());
        let beta = header(&headers, "anthropic-beta").expect("anthropic-beta");
        assert!(beta.contains("oauth-2025-04-20"), "{beta}");
    }

    #[test]
    fn api_key_providers_use_bearer_and_ollama_sends_nothing() {
        for id in ["openai", "deepseek", "gemini", "groq", "openrouter", "xai", "mistral", "together", "cerebras"] {
            let headers = build_headers(registry::get(id).expect(id), &full()).expect("h");
            assert_eq!(header(&headers, "authorization"), Some("Bearer sk-key"), "{id}");
        }
        let headers =
            build_headers(registry::get("ollama").expect("ollama"), &Credentials::default())
                .expect("h");
        assert!(headers.is_empty(), "{headers:?}");
    }

    #[test]
    fn missing_credential_is_an_error() {
        let err = build_headers(registry::get("github").expect("github"), &Credentials::default())
            .expect_err("must fail");
        assert_eq!(err.status(), 401);
        let err = build_headers(registry::get("anthropic").expect("anthropic"), &Credentials::default())
            .expect_err("must fail");
        assert_eq!(err.status(), 401);
    }

    #[test]
    fn base_url_override_wins() {
        let provider = registry::get("openai").expect("openai");
        let mut c = full();
        assert_eq!(resolve_base_url(provider, &c), provider.base_url);
        c.base_url_override = Some("https://proxy.internal/v1/chat/completions".to_string());
        assert_eq!(
            resolve_base_url(provider, &c),
            "https://proxy.internal/v1/chat/completions"
        );
    }
}
