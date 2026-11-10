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
    /// Provider-specific data passed through untouched.
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl Credentials {
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
