//! Plaintext credential material: the model of the encrypted blob `rrouter-store` persists.
//!
//! Every field except `extra` is optional and defaults to `None`, so a stored row that only
//! knows an API key deserializes cleanly into this struct. `extra` is the
//! provider-specific passthrough metadata (project ids, region, enterprise URLs, ...)
//! and is never interpreted by this crate.

use serde::{Deserialize, Serialize};

/// One account's live credential material for one provider.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Credentials {
    /// OAuth access token (expiring) or the long-lived GitHub token for `github`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_token: Option<String>,
    /// OAuth refresh token. Single-use for some providers, hence the refresh mutex.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    /// Static API key for `ApiKey` providers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// OIDC id token, when the provider returns one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id_token: Option<String>,
    /// Unix seconds at which `access_token` stops being usable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
    /// GitHub Copilot short-lived session token (`github` only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copilot_token: Option<String>,
    /// Unix seconds at which `copilot_token` stops being usable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copilot_expires_at: Option<i64>,
    /// Human label shown in the dashboard (email, account name).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_label: Option<String>,
    /// Overrides [`ProviderSpec::base_url`](crate::ProviderSpec::base_url) unconditionally.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url_override: Option<String>,
    /// Egress proxy for this account only, as `scheme://[user:pass@]host:port`
    /// (`http`, `https`, `socks5`, `socks5h`).
    ///
    /// Free-tier accounts are usually rate-limited per (account, address), so an operator
    /// running several of them needs each one to leave from its own address. Keeping it on
    /// the credential rather than in a global configuration is what makes that possible:
    /// two accounts of the same provider can route through different proxies, and a 429 on
    /// one never blocks the other. It is secret material (`user:pass@`), so it lives in the
    /// encrypted blob and the admin API only ever echoes it masked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy_url: Option<String>,
    /// This account belongs to free mode: routing may spend it for any free model and
    /// spreads load across every free-mode account before a provider answers 429.
    #[serde(default, skip_serializing_if = "is_false")]
    pub free_mode: bool,
    /// Proactive per-account cap, in requests per minute, applied by the free-mode
    /// rotation *before* the request is sent. `None` = no local cap (the provider's own
    /// 429 is then the only feedback).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub free_rpm: Option<u32>,
    /// Provider-specific data passed through untouched.
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

impl Credentials {
    /// True when this account must leave through its own proxy.
    pub fn has_proxy(&self) -> bool {
        self.proxy_url
            .as_deref()
            .map(str::trim)
            .is_some_and(|url| !url.is_empty())
    }

    /// The proxy as the admin API may show it: scheme, host and port survive, any
    /// `user:pass@` userinfo is replaced with `***@`.
    ///
    /// Proxy URLs routinely carry credentials, so the dashboard must never echo them —
    /// but an operator does need to tell two proxies on the same host apart.
    pub fn proxy_masked(&self) -> Option<String> {
        let url = self.proxy_url.as_deref()?.trim();
        if url.is_empty() {
            return None;
        }
        let (scheme, rest) = match url.split_once("://") {
            Some((scheme, rest)) => (Some(scheme), rest),
            None => (None, url),
        };
        let masked = match rest.rsplit_once('@') {
            Some((_userinfo, host)) => format!("***@{host}"),
            None => rest.to_string(),
        };
        Some(match scheme {
            Some(scheme) => format!("{scheme}://{masked}"),
            None => masked,
        })
    }

    /// True when neither the access token nor the API key is known to be expired.
    ///
    /// `now` is unix seconds. A credential with no `expires_at` is treated as non-expiring
    /// (static API keys, local servers).
    pub fn is_expired(&self, now: i64) -> bool {
        match self.expires_at {
            Some(at) => at <= now,
            None => false,
        }
    }
}
