//! Static provider capabilities, for request-scoped routing policy.
//!
//! A client can ask for providers that honour a data-retention or precision
//! commitment. Those are contract facts about a provider, not something a request can
//! discover, so they live in one flat table keyed by the registry id the planner splits
//! a `provider/model` ref on. Unlike transport facts, no manifest declares them: a
//! capability is a promise RRouter makes, so it is reviewed here.
//!
//! Every field is deliberately conservative. A provider we know nothing about claims
//! nothing, so `zdr: true` or `data_collection: deny` in a request can never be
//! "satisfied" by an operator's self-hosted endpoint we merely failed to describe.

/// Quantizations a provider is known to offer. A request that names a quantization
/// outside a provider's list is routed elsewhere; a provider that publishes none (every
/// hosted API, which does not expose precision) is excluded whenever one is requested.
pub const QUANTIZATIONS: &[&str] = &["fp16", "bf16", "int8", "int4", "fp8"];

/// What one provider can be asked to guarantee.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderCapabilities {
    /// The provider offers a contractual "do not retain or train on this request" mode.
    pub data_collection_deny: bool,
    /// The provider offers zero data retention.
    pub zdr: bool,
    /// Precisions the operator can actually select. Empty means "not published".
    pub quantizations: &'static [&'static str],
    /// The provider honours sampling controls (`temperature`, `top_p`). `false` means it
    /// rejects or ignores them — the gateway already strips them for such a provider.
    pub sampling_params: bool,
}

/// Capabilities for a registry provider id.
///
/// The conservative default (no guarantee, no published precision, sampling honoured)
/// is what an unknown or operator-supplied provider id gets.
pub fn capabilities(provider: &str) -> ProviderCapabilities {
    match provider {
        // Vendor APIs whose enterprise terms cover retention and no-training.
        "openai" | "anthropic" | "claude" => ProviderCapabilities {
            data_collection_deny: true,
            zdr: true,
            quantizations: &[],
            sampling_params: true,
        },
        // Hosted APIs that do not train on API traffic, but publish no ZDR programme.
        "xai" | "mistral" | "groq" | "cerebras" | "together" | "deepseek" => ProviderCapabilities {
            data_collection_deny: true,
            zdr: false,
            quantizations: &[],
            sampling_params: true,
        },
        // Codex is a consumer subscription: no retention commitment, and it rejects
        // `temperature`/`top_p` outright (the gateway strips them, see the proxy).
        "codex" => ProviderCapabilities {
            data_collection_deny: false,
            zdr: false,
            quantizations: &[],
            sampling_params: false,
        },
        // An aggregator cannot promise what its downstream provider does.
        "openrouter" => ProviderCapabilities {
            data_collection_deny: false,
            zdr: false,
            quantizations: &[],
            sampling_params: true,
        },
        // Self-hosted: every precision is selectable and nothing leaves the host.
        "ollama" => ProviderCapabilities {
            data_collection_deny: true,
            zdr: true,
            quantizations: QUANTIZATIONS,
            sampling_params: true,
        },
        _ => ProviderCapabilities {
            data_collection_deny: false,
            zdr: false,
            quantizations: &[],
            sampling_params: true,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_providers_promise_nothing() {
        let caps = capabilities("some-self-hosted-gateway");
        assert!(!caps.data_collection_deny);
        assert!(!caps.zdr);
        assert!(caps.quantizations.is_empty());
    }

    #[test]
    fn zdr_and_data_policy_are_only_claimed_where_contractual() {
        assert!(capabilities("anthropic").zdr);
        assert!(capabilities("claude").zdr);
        assert!(capabilities("openai").zdr);
        // An aggregator cannot pass the commitment through to its downstream.
        assert!(!capabilities("openrouter").zdr);
        assert!(!capabilities("openrouter").data_collection_deny);
        // Consumer subscription: no retention guarantee.
        assert!(!capabilities("codex").data_collection_deny);
    }

    #[test]
    fn only_self_hosted_publishes_quantizations() {
        assert_eq!(capabilities("ollama").quantizations, QUANTIZATIONS);
        assert!(capabilities("openai").quantizations.is_empty());
    }

    #[test]
    fn codex_is_the_provider_that_drops_sampling_controls() {
        assert!(!capabilities("codex").sampling_params);
        assert!(capabilities("openai").sampling_params);
        assert!(capabilities("anthropic").sampling_params);
    }
}
