//! The runtime provider view: the compiled transport table plus operator-defined
//! providers, each with a short code that model refs may use in place of its id.
//!
//! A custom provider is a name, a wire protocol, and a base URL. It borrows the compiled
//! transport of that protocol ([`rrouter_auth::get`]) and routes with the connection's
//! `base_url_override`, which create/update pins to the provider's base URL. That keeps
//! the proxy on one static [`ProviderSpec`] type: only identity, dialect, code and the
//! endpoint are per provider.

use std::collections::{BTreeMap, HashMap};

use rrouter_auth::ProviderSpec;
use rrouter_core::Dialect;
use rrouter_store::{ProviderProfile, Store};

/// Longest accepted shorthand. Long enough for `opencode-go`, short enough that a ref
/// stays readable.
pub const MAX_CODE_LEN: usize = 16;

/// The code a provider's refs use when an operator has not chosen one.
///
/// Curated so a ref reads short (`oai`, `ocg`) instead of restating the id. A provider
/// this build does not know falls back to its sanitized id.
pub fn default_code(provider: &str) -> String {
    match provider {
        "openai" => "oai".to_string(),
        "github" => "gh".to_string(),
        "opencode" => "oc".to_string(),
        "opencode-go" => "ocg".to_string(),
        "anthropic" => "ant".to_string(),
        "deepseek" => "ds".to_string(),
        "gemini" => "gem".to_string(),
        "openrouter" => "or".to_string(),
        "mistral" => "mis".to_string(),
        "together" => "tog".to_string(),
        "cerebras" => "cer".to_string(),
        "ollama" => "oll".to_string(),
        "azure" => "az".to_string(),
        "cohere" => "coh".to_string(),
        "fireworks" => "fw".to_string(),
        other => sanitize(other),
    }
}

/// Lowercase ASCII with runs of anything else collapsed to `-`.
fn sanitize(provider: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for c in provider.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            dash = false;
        } else if !out.is_empty() && !dash {
            out.push('-');
            dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        out.push_str("provider");
    }
    out.truncate(MAX_CODE_LEN);
    out
}

/// `true` when `code` can appear as one segment of `group/code/model`.
pub fn valid_code(code: &str) -> bool {
    !code.is_empty()
        && code.len() <= MAX_CODE_LEN
        && code
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
        && !code.starts_with('.')
        && code != "rrouter"
}

/// Parse the stored dialect name. Only the two wire shapes an operator endpoint can
/// realistically speak are accepted; anything else is a row this build must not route.
pub fn parse_dialect(name: &str) -> Option<Dialect> {
    match name {
        "openai" | "openai_chat" => Some(Dialect::OpenAiChat),
        "anthropic" | "anthropic_messages" => Some(Dialect::AnthropicMessages),
        _ => None,
    }
}

/// Stored name for a dialect.
pub fn dialect_name(dialect: Dialect) -> Option<&'static str> {
    match dialect {
        Dialect::OpenAiChat => Some("openai"),
        Dialect::AnthropicMessages => Some("anthropic"),
        _ => None,
    }
}

/// The compiled provider whose transport a custom protocol borrows.
fn transport_for(dialect: Dialect) -> Option<&'static ProviderSpec> {
    match dialect {
        Dialect::OpenAiChat => rrouter_auth::get("openai"),
        Dialect::AnthropicMessages => rrouter_auth::get("anthropic"),
        _ => None,
    }
}

/// A provider id derived from an operator-entered name.
///
/// Lowercase ASCII, runs of anything else collapsed to `-`. The caller resolves a
/// collision by appending `-2`, `-3`, … so a name is never rejected for being taken.
pub fn slug(name: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            dash = false;
        } else if !out.is_empty() && !dash {
            out.push('-');
            dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        out.push_str("custom");
    }
    out.truncate(48);
    out
}

/// The merged provider view. Owned maps only; cheap enough to rebuild on every mutation.
pub struct ProviderCatalog {
    profiles: BTreeMap<String, ProviderProfile>,
    /// provider id -> shorthand.
    provider_codes: HashMap<String, String>,
    /// shorthand -> provider id.
    code_to_provider: BTreeMap<String, String>,
}

impl Default for ProviderCatalog {
    /// Compiled providers only. Tests and pre-load states use this; the stored codes are
    /// layered on by [`Self::load`].
    fn default() -> Self {
        let mut catalog = Self {
            profiles: BTreeMap::new(),
            provider_codes: HashMap::new(),
            code_to_provider: BTreeMap::new(),
        };
        for spec in rrouter_auth::all() {
            let code = default_code(spec.id);
            catalog
                .code_to_provider
                .insert(code.clone(), spec.id.to_string());
            catalog.provider_codes.insert(spec.id.to_string(), code);
        }
        catalog
    }
}

impl ProviderCatalog {
    /// Build from the compiled registry and the stored profiles. A custom row this build
    /// cannot route (unknown dialect, missing URL) is skipped rather than advertised.
    pub async fn load(store: &Store) -> rrouter_core::Result<Self> {
        let mut catalog = Self {
            profiles: BTreeMap::new(),
            provider_codes: HashMap::new(),
            code_to_provider: BTreeMap::new(),
        };
        // Seed a row for every compiled provider (idempotent): the code then lives in one
        // table, so an operator edit and the default are read the same way.
        for spec in rrouter_auth::all() {
            store
                .seed_provider_profile(spec.id, &default_code(spec.id))
                .await?;
        }
        for profile in store.list_provider_profiles().await? {
            if profile.display.is_some()
                && (profile.base_url.is_none()
                    || profile.dialect.as_deref().and_then(parse_dialect).is_none())
            {
                continue;
            }
            catalog
                .provider_codes
                .insert(profile.provider.clone(), profile.code.clone());
            catalog
                .code_to_provider
                .insert(profile.code.clone(), profile.provider.clone());
            catalog.profiles.insert(profile.provider.clone(), profile);
        }
        // A compiled provider with no stored row still needs a code: fall back to its id
        // rather than advertising a ref with no shorthand.
        let missing: Vec<(String, String)> = rrouter_auth::all()
            .filter(|spec| !catalog.provider_codes.contains_key(spec.id))
            .map(|spec| (spec.id.to_string(), default_code(spec.id)))
            .collect();
        for (provider, code) in missing {
            catalog
                .code_to_provider
                .insert(code.clone(), provider.clone());
            catalog.provider_codes.insert(provider, code);
        }
        Ok(catalog)
    }

    /// The static transport for one provider: its own for a compiled id, the protocol's
    /// transport for a custom one.
    pub fn spec(&self, provider: &str) -> Option<&'static ProviderSpec> {
        if let Some(spec) = rrouter_auth::get(provider) {
            return Some(spec);
        }
        let profile = self.profiles.get(provider)?;
        transport_for(parse_dialect(profile.dialect.as_deref()?)?)
    }

    /// The dialect a custom provider speaks. Built-ins report their compiled dialect.
    pub fn dialect(&self, provider: &str) -> Option<Dialect> {
        if let Some(spec) = rrouter_auth::get(provider) {
            return Some(spec.default_dialect);
        }
        parse_dialect(self.profiles.get(provider)?.dialect.as_deref()?)
    }

    /// The shorthand for `provider`. Unknown providers fall back to their id, which is
    /// what an operator's own model row keyed by an id this build does not compile uses.
    pub fn code(&self, provider: &str) -> String {
        self.provider_codes
            .get(provider)
            .cloned()
            .unwrap_or_else(|| default_code(provider))
    }

    /// Resolve a ref segment to the provider id it names. The id wins over a code so an
    /// existing `openai/model` spelling keeps working even if `openai`'s code changed.
    pub fn provider_for(&self, segment: &str) -> Option<String> {
        if self.spec(segment).is_some() {
            return Some(segment.to_string());
        }
        self.code_to_provider.get(segment).cloned()
    }

    pub fn profile(&self, provider: &str) -> Option<&ProviderProfile> {
        self.profiles.get(provider)
    }

    pub fn is_custom(&self, provider: &str) -> bool {
        self.profiles
            .get(provider)
            .is_some_and(|profile| profile.display.is_some())
    }

    /// Every custom provider, ordered by id.
    pub fn customs(&self) -> impl Iterator<Item = &ProviderProfile> {
        self.profiles.values().filter(|p| p.display.is_some())
    }

    /// The base URL a custom provider's accounts must dispatch to.
    pub fn custom_base_url(&self, provider: &str) -> Option<&str> {
        self.profiles
            .get(provider)
            .filter(|profile| profile.display.is_some())
            .and_then(|profile| profile.base_url.as_deref())
    }
}

impl crate::state::AppState {
    /// Transport for one provider. Never holds the read lock past the call.
    pub fn provider_spec(&self, provider: &str) -> Option<&'static ProviderSpec> {
        self.providers.read().ok()?.spec(provider)
    }

    /// The shorthand a ref for this provider uses.
    pub fn provider_code(&self, provider: &str) -> String {
        self.providers
            .read()
            .map(|catalog| catalog.code(provider))
            .unwrap_or_else(|_| provider.to_string())
    }

    /// Provider id named by `segment` (id or shorthand).
    pub fn provider_by_ref(&self, segment: &str) -> Option<String> {
        self.providers.read().ok()?.provider_for(segment)
    }

    /// Dialect spoken by one provider, custom ones included.
    pub fn provider_dialect(&self, provider: &str) -> Option<Dialect> {
        self.providers.read().ok()?.dialect(provider)
    }

    /// The base URL a custom provider's accounts dispatch to. `None` for built-ins.
    pub fn custom_base_url(&self, provider: &str) -> Option<String> {
        self.providers
            .read()
            .ok()?
            .custom_base_url(provider)
            .map(str::to_string)
    }

    pub fn provider_is_custom(&self, provider: &str) -> bool {
        self.providers
            .read()
            .is_ok_and(|catalog| catalog.is_custom(provider))
    }

    /// Rebuild the catalog after a provider or code mutation.
    pub async fn reload_providers(&self) -> rrouter_core::Result<()> {
        let next = ProviderCatalog::load(self.store.as_ref()).await?;
        if let Ok(mut current) = self.providers.write() {
            *current = next;
        }
        Ok(())
    }
}
