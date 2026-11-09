//! Header set for one outbound call, already including auth.

use crate::credentials::Credentials;
use crate::registry::{AuthStyle, ProviderKind, ProviderSpec};

/// Build the full header list for one upstream request to `provider` with `c`.
///
/// Auth selection: `github` → `copilot_token`; OAuth → `access_token`; `XApiKey` → `api_key`
/// in `x-api-key`; `ApiKeyHeader` → `api_key` in the provider's own header (Azure's
/// `api-key`); `None` → nothing. A missing credential is an error, never an unauthenticated
/// request: sending one would burn a retryable upstream error and mask a local bug.
///
/// Also refuses a placeholder endpoint before any header is built: a provider that declares
/// `base_url_requires_override` cannot be called until its connection supplies the resource
/// URL, and this is the one function every outbound request passes through.
pub fn build_headers(
    provider: &ProviderSpec,
    c: &Credentials,
) -> rrouter_core::Result<Vec<(String, String)>> {
    crate::base_url::checked_base_url(provider, c)?;
    let mut headers: Vec<(String, String)> = Vec::with_capacity(provider.extra_headers.len() + 2);

    match provider.auth {
        AuthStyle::Bearer => {
            let token = match provider.kind {
                ProviderKind::OAuth if provider.id == "github" => c.copilot_token.as_deref(),
                ProviderKind::OAuth => c.access_token.as_deref(),
                // API-key provider with Bearer auth: the key is the bearer token.
                ProviderKind::ApiKey | ProviderKind::Local => {
                    c.api_key.as_deref().or(c.access_token.as_deref())
                }
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
        AuthStyle::ApiKeyHeader => {
            // A provider that names no header cannot be called safely, so this fails closed
            // instead of falling back to Authorization (which the provider would reject,
            // turning a spec bug into a confusing 401).
            if provider.auth_header.is_empty() {
                return Err(rrouter_core::Error::Store(format!(
                    "provider {} uses api-key auth but names no header",
                    provider.id
                )));
            }
            let key = c
                .api_key
                .as_deref()
                .or(c.access_token.as_deref())
                .ok_or_else(|| missing(provider, "api key"))?;
            headers.push((provider.auth_header.to_string(), key.to_string()));
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

/// Merge the client's own protocol headers into a provider's outbound header list.
///
/// Only the names [`ProviderSpec::passthrough_headers`] lists are read at all, which is
/// what keeps a client's `authorization`, `x-api-key`, `cookie` or `host` from reaching a
/// provider. A name the spec already sets (`anthropic-beta` carries the gateway's own
/// OAuth flag) is **unioned** rather than replaced, so neither side's flags are lost, and
/// duplicate tokens are dropped.
///
/// The proxy calls this after [`build_headers`], with the inbound request's headers.
pub fn merge_client_headers(
    provider: &ProviderSpec,
    headers: &mut Vec<(String, String)>,
    client: &http::HeaderMap,
) {
    for name in provider.passthrough_headers {
        let mut tokens: Vec<String> = Vec::new();
        for value in client.get_all(*name).iter() {
            let Ok(text) = value.to_str() else {
                // A non-ASCII beta header is not a token list; skipping it is safer than
                // forwarding bytes a provider will reject.
                continue;
            };
            for token in text.split(',') {
                let token = token.trim();
                if !token.is_empty() && !tokens.iter().any(|t| t.eq_ignore_ascii_case(token)) {
                    tokens.push(token.to_string());
                }
            }
        }
        if tokens.is_empty() {
            continue;
        }
        match headers
            .iter_mut()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
        {
            Some((_, existing)) => {
                let mut merged: Vec<String> = existing
                    .split(',')
                    .map(str::trim)
                    .filter(|token| !token.is_empty())
                    .map(str::to_owned)
                    .collect();
                for token in tokens {
                    if !merged.iter().any(|t| t.eq_ignore_ascii_case(&token)) {
                        merged.push(token);
                    }
                }
                *existing = merged.join(",");
            }
            None => headers.push(((*name).to_string(), tokens.join(","))),
        }
    }
}

fn missing(provider: &ProviderSpec, what: &str) -> rrouter_core::Error {
    rrouter_core::Error::Unauthorized(format!("provider {} credential has no {what}", provider.id))
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
        assert_eq!(
            header(&headers, "copilot-integration-id"),
            Some("vscode-chat")
        );
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
        for id in [
            "openai",
            "deepseek",
            "gemini",
            "groq",
            "openrouter",
            "xai",
            "mistral",
            "together",
            "cerebras",
        ] {
            let headers = build_headers(registry::get(id).expect(id), &full()).expect("h");
            assert_eq!(
                header(&headers, "authorization"),
                Some("Bearer sk-key"),
                "{id}"
            );
        }
        let headers = build_headers(
            registry::get("ollama").expect("ollama"),
            &Credentials::default(),
        )
        .expect("h");
        assert!(headers.is_empty(), "{headers:?}");
    }

    #[test]
    fn missing_credential_is_an_error() {
        let err = build_headers(
            registry::get("github").expect("github"),
            &Credentials::default(),
        )
        .expect_err("must fail");
        assert_eq!(err.status(), 401);
        let err = build_headers(
            registry::get("anthropic").expect("anthropic"),
            &Credentials::default(),
        )
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

    #[test]
    fn client_betas_are_unioned_with_the_gateways_own() {
        let spec = registry::get("claude").expect("claude");
        let mut headers = build_headers(spec, &full()).expect("headers");
        let mut client = http::HeaderMap::new();
        client.append(
            "anthropic-beta",
            http::HeaderValue::from_static("context-1m-2025-08-07,oauth-2025-04-20"),
        );
        merge_client_headers(spec, &mut headers, &client);

        let beta = header(&headers, "anthropic-beta").expect("anthropic-beta");
        assert!(beta.contains("oauth-2025-04-20"), "{beta}");
        assert!(beta.contains("interleaved-thinking-2025-05-14"), "{beta}");
        assert!(beta.contains("context-1m-2025-08-07"), "{beta}");
        assert_eq!(
            beta.matches("oauth-2025-04-20").count(),
            1,
            "duplicates are dropped: {beta}"
        );
        assert_eq!(
            headers
                .iter()
                .filter(|(k, _)| k.eq_ignore_ascii_case("anthropic-beta"))
                .count(),
            1,
            "the merged value replaces the row instead of appending a second one"
        );
    }

    #[test]
    fn multiple_client_beta_headers_are_unioned() {
        let spec = registry::get("openai").expect("openai");
        let mut headers = build_headers(spec, &full()).expect("headers");
        let mut client = http::HeaderMap::new();
        client.append(
            "openai-beta",
            http::HeaderValue::from_static("responses=v1"),
        );
        client.append(
            "openai-beta",
            http::HeaderValue::from_static("assistants=v2,responses=v1"),
        );
        merge_client_headers(spec, &mut headers, &client);
        assert_eq!(
            header(&headers, "openai-beta"),
            Some("responses=v1,assistants=v2"),
            "unioned and deduplicated: {headers:?}"
        );
    }

    #[test]
    fn client_credentials_are_never_forwarded() {
        let spec = registry::get("claude").expect("claude");
        let mut headers = build_headers(spec, &full()).expect("headers");
        let mut client = http::HeaderMap::new();
        for (name, value) in [
            ("authorization", "Bearer client-token"),
            ("x-api-key", "sk-client"),
            ("cookie", "session=1"),
            ("host", "evil.example"),
            ("content-length", "12"),
            ("x-admin-token", "admin"),
            ("openai-beta", "responses=v1"),
            ("anthropic-beta", "context-1m-2025-08-07"),
        ] {
            client.append(name, http::HeaderValue::from_static(value));
        }
        merge_client_headers(spec, &mut headers, &client);

        // The gateway's own credential stands; the client's does not replace it.
        assert_eq!(header(&headers, "authorization"), Some("Bearer at"));
        assert_eq!(header(&headers, "x-api-key"), None);
        for forbidden in ["cookie", "host", "content-length", "x-admin-token"] {
            assert!(
                header(&headers, forbidden).is_none(),
                "{forbidden} must never be forwarded: {headers:?}"
            );
        }
        // A beta the provider's surface does not speak stays behind.
        assert!(header(&headers, "openai-beta").is_none());
        assert!(header(&headers, "anthropic-beta")
            .is_some_and(|beta| beta.contains("context-1m-2025-08-07")));
    }

    #[test]
    fn a_provider_without_an_allowlist_forwards_no_client_header() {
        let spec = registry::get("mistral").expect("mistral");
        let mut headers = build_headers(spec, &full()).expect("headers");
        let before = headers.clone();
        let mut client = http::HeaderMap::new();
        client.append(
            "anthropic-beta",
            http::HeaderValue::from_static("context-1m"),
        );
        client.append(
            "openai-beta",
            http::HeaderValue::from_static("responses=v1"),
        );
        merge_client_headers(spec, &mut headers, &client);
        assert_eq!(headers, before);
    }

    #[test]
    fn api_key_header_auth_fails_closed_without_a_header_name() {
        let base = *registry::get("azure").expect("azure manifest");
        let broken = ProviderSpec {
            auth_header: "",
            ..base
        };
        let mut creds = full();
        // Clear the base-url gate so the missing header is what fails.
        creds.base_url_override = Some("https://contoso.openai.azure.com".into());
        let err = build_headers(&broken, &creds).expect_err("must fail");
        assert_eq!(err.status(), 502, "{err}");
        assert!(err.to_string().contains("names no header"), "{err}");
    }
}
