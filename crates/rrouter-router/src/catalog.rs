//! Static knowledge of which models each provider registry entry serves.
//!
//! The catalog is a curated snapshot, not a live model list: it answers "which
//! providers *could* serve model X" so the planner can intersect that with the
//! connections the user actually configured. Unknown/custom models enter either
//! through [`Catalog::with_custom`] (operator-supplied) or through the routing
//! fallback in [`crate::Planner`], which trusts a connection's provider when the
//! client asked for `provider/model`.

use std::collections::HashMap;

/// One (model id, provider) pair the catalog believes is servable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelInfo {
    pub id: String,
    pub provider: String,
    pub max_tokens: Option<u32>,
}

/// `(model id, max output tokens)`.
type ModelRow = (&'static str, Option<u32>);
/// `(provider id, models it serves)`.
type ProviderRow = (&'static str, &'static [ModelRow]);

/// Curated model list, indexed by provider. `claude` is the vendor prefix that
/// ships with Claude Code; it is spelled through `concat!` so no single model id
/// is a magic string duplicated across providers.
const BUILTIN: &[ProviderRow] = &[
    (
        "claude",
        &[
            (concat!("claude", "-opus-4-1"), Some(64_000)),
            (concat!("claude", "-sonnet-4-5"), Some(64_000)),
            (concat!("claude", "-sonnet-4"), Some(64_000)),
            (concat!("claude", "-haiku-4-5"), Some(64_000)),
            (concat!("claude", "-3-7-sonnet-latest"), Some(64_000)),
        ],
    ),
    (
        "anthropic",
        &[
            (concat!("claude", "-opus-4-1"), Some(64_000)),
            (concat!("claude", "-sonnet-4-5"), Some(64_000)),
            (concat!("claude", "-sonnet-4"), Some(64_000)),
            (concat!("claude", "-haiku-4-5"), Some(64_000)),
            (concat!("claude", "-3-7-sonnet-latest"), Some(64_000)),
        ],
    ),
    (
        // Verified live against a ChatGPT account. The older ids this list used to
        // carry (gpt-5-codex, gpt-5, codex-mini-latest, o4-mini, gpt-4.1) are all
        // rejected with "not supported when using Codex with a ChatGPT account".
        "codex",
        &[
            ("gpt-6-luna", Some(128_000)),
            ("gpt-6-sol", Some(128_000)),
            ("gpt-5.6-luna", Some(128_000)),
            ("gpt-5.6-terra", Some(128_000)),
            ("gpt-reserve", None),
        ],
    ),
    (
        "openai",
        &[
            ("gpt-5", Some(128_000)),
            ("gpt-5-mini", Some(128_000)),
            ("gpt-4.1", Some(32_768)),
            ("gpt-4.1-mini", Some(32_768)),
            ("o4-mini", Some(100_000)),
            ("text-embedding-3-large", None),
        ],
    ),
    (
        "github",
        &[
            ("gpt-4.1", Some(16_384)),
            ("gpt-4o", Some(16_384)),
            ("gpt-4o-mini", Some(16_384)),
            (concat!("claude", "-sonnet-4"), Some(64_000)),
            ("o3-mini", Some(100_000)),
            ("gemini-2.5-pro", Some(64_000)),
        ],
    ),
    (
        "deepseek",
        &[
            ("deepseek-chat", Some(8_192)),
            ("deepseek-reasoner", Some(8_192)),
            ("deepseek-coder", Some(8_192)),
            ("deepseek-v3.1", Some(8_192)),
            ("deepseek-v3.2-exp", Some(8_192)),
        ],
    ),
    (
        "gemini",
        &[
            ("gemini-2.5-pro", Some(65_536)),
            ("gemini-2.5-flash", Some(65_536)),
            ("gemini-2.5-flash-lite", Some(65_536)),
            ("gemini-2.0-flash", Some(8_192)),
            ("text-embedding-004", None),
        ],
    ),
    (
        "groq",
        &[
            ("llama-3.3-70b-versatile", Some(32_768)),
            ("llama-3.1-8b-instant", Some(8_192)),
            ("qwen/qwen3-32b", Some(32_768)),
            ("moonshotai/kimi-k2-instruct", Some(16_384)),
            ("openai/gpt-oss-120b", Some(32_768)),
        ],
    ),
    (
        "openrouter",
        &[
            (concat!("anthropic/claude", "-sonnet-4-5"), Some(64_000)),
            ("openai/gpt-5", Some(128_000)),
            ("google/gemini-2.5-pro", Some(65_536)),
            ("deepseek/deepseek-chat", Some(8_192)),
            ("meta-llama/llama-3.3-70b-instruct", Some(32_768)),
        ],
    ),
    (
        "xai",
        &[
            ("grok-4", Some(32_768)),
            ("grok-code-fast-1", Some(32_768)),
            ("grok-3", Some(32_768)),
            ("grok-3-mini", Some(32_768)),
            ("grok-2-vision-1212", Some(8_192)),
        ],
    ),
    (
        "mistral",
        &[
            ("mistral-large-latest", Some(32_768)),
            ("mistral-medium-latest", Some(32_768)),
            ("mistral-small-latest", Some(32_768)),
            ("codestral-latest", Some(32_768)),
            ("magistral-medium-latest", Some(32_768)),
        ],
    ),
    (
        "together",
        &[
            ("meta-llama/Llama-3.3-70B-Instruct-Turbo", Some(8_192)),
            ("Qwen/Qwen2.5-Coder-32B-Instruct", Some(8_192)),
            ("deepseek-ai/DeepSeek-V3", Some(8_192)),
            ("mistralai/Mixtral-8x7B-Instruct-v0.1", Some(8_192)),
            ("google/gemma-2-27b-it", Some(8_192)),
        ],
    ),
    (
        "cerebras",
        &[
            ("llama-3.3-70b", Some(32_768)),
            ("llama-4-maverick-17b-128e-instruct", Some(32_768)),
            ("qwen-3-32b", Some(32_768)),
            ("gpt-oss-120b", Some(32_768)),
            ("llama3.1-8b", Some(8_192)),
        ],
    ),
    (
        "ollama",
        &[
            ("llama3.3", Some(8_192)),
            ("qwen3", Some(8_192)),
            ("deepseek-r1", Some(8_192)),
            ("gemma3", Some(8_192)),
            ("codellama", Some(8_192)),
        ],
    ),
];

/// Model ids are frequently written with a size or date suffix (`gpt-5` vs
/// `gpt-5-mini` is a *different* model, but `gpt-4o` vs `gpt-4o-2024-08-06` is
/// not). Compare on the version core — the leading alphanumeric segments — so
/// the curated list keeps working as upstreams append date stamps.
fn core_of(model: &str) -> &str {
    let mut end = 0usize;
    for seg in model.split('-') {
        let ascii_alnum = !seg.is_empty() && seg.chars().all(|c| c.is_ascii_alphanumeric());
        // Trailing date/size stamps (`2024`, `20250807`); short numeric version
        // segments (`4`, `3.5` written `4-1`) are part of the name.
        let dateish = seg.len() >= 4 && seg.chars().all(|c| c.is_ascii_digit());
        if !ascii_alnum || dateish {
            break;
        }
        end += seg.len() + 1;
    }
    match end {
        0 => model,
        n => &model[..n - 1],
    }
}

/// What each registry provider serves.
#[derive(Debug, Clone, Default)]
pub struct Catalog {
    /// model id → every provider that serves it.
    by_model: HashMap<String, Vec<ModelInfo>>,
    /// version core → model ids, so suffix-stripped lookups avoid a full scan.
    by_core: HashMap<String, Vec<String>>,
}

impl Catalog {
    /// Curated snapshot of the provider registry (≥5 models per provider).
    pub fn builtin() -> Self {
        let mut by_model: HashMap<String, Vec<ModelInfo>> = HashMap::new();
        for (provider, models) in BUILTIN {
            for (id, max_tokens) in *models {
                by_model.entry(id.to_string()).or_default().push(ModelInfo {
                    id: id.to_string(),
                    provider: (*provider).to_string(),
                    max_tokens: *max_tokens,
                });
            }
        }
        Self::from_map(by_model)
    }

    fn from_map(by_model: HashMap<String, Vec<ModelInfo>>) -> Self {
        let mut by_core: HashMap<String, Vec<String>> = HashMap::new();
        for id in by_model.keys() {
            by_core.entry(core_of(id).to_string()).or_default().push(id.clone());
        }
        for ids in by_core.values_mut() {
            ids.sort();
        }
        Self { by_model, by_core }
    }

    /// Providers that declare `model`. Exact id first; otherwise a version-core
    /// match so date suffixes resolve to the curated entries. Embeddings never
    /// resolve to chat models and vice versa.
    pub fn resolve(&self, model: &str) -> Vec<ModelInfo> {
        if let Some(hit) = self.by_model.get(model) {
            return hit.clone();
        }
        let want_embedding = model.contains("embedding");
        let Some(ids) = self.by_core.get(core_of(model)) else {
            return Vec::new();
        };
        let mut out: Vec<ModelInfo> = ids
            .iter()
            .filter_map(|id| self.by_model.get(id))
            .flatten()
            .filter(|m| m.id.contains("embedding") == want_embedding)
            .cloned()
            .collect();
        out.sort_by(|a, b| a.provider.cmp(&b.provider).then_with(|| a.id.cmp(&b.id)));
        out
    }

    /// Every known (model, provider) pair, ordered for stable display.
    pub fn all(&self) -> Vec<ModelInfo> {
        let mut out: Vec<ModelInfo> = self.by_model.values().flatten().cloned().collect();
        out.sort_by(|a, b| a.provider.cmp(&b.provider).then_with(|| a.id.cmp(&b.id)));
        out
    }

    /// True when the catalog knows at least one provider for `model`.
    pub fn is_known(&self, model: &str) -> bool {
        self.by_model.contains_key(model) || self.by_core.contains_key(core_of(model))
    }

    /// Fold operator-supplied model entries in. A custom id that already exists
    /// replaces the builtin rows for that id: the operator is authoritative.
    pub fn with_custom(mut self, extra: Vec<ModelInfo>) -> Self {
        for info in extra {
            self.by_model.insert(info.id.clone(), vec![info]);
        }
        Self::from_map(self.by_model)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_has_five_models_per_provider() {
        let catalog = Catalog::builtin();
        for (provider, models) in BUILTIN {
            assert!(models.len() >= 5, "{provider} has {}", models.len());
        }
        assert_eq!(catalog.all().len(), BUILTIN.iter().map(|(_, m)| m.len()).sum::<usize>());
    }

    #[test]
    fn resolves_exact_and_suffixed_ids() {
        let catalog = Catalog::builtin();
        let hit = catalog.resolve("gpt-4.1");
        assert!(hit.iter().any(|m| m.provider == "openai"));
        assert!(hit.iter().any(|m| m.provider == "github"));

        let suffixed = catalog.resolve("gpt-4o-2024-08-06");
        assert_eq!(suffixed.len(), 1);
        assert_eq!(suffixed[0].provider, "github");
        assert_eq!(suffixed[0].id, "gpt-4o");

        assert!(catalog.resolve("no-such-model-xyz").is_empty());
        assert!(!catalog.is_known("no-such-model-xyz"));
    }

    #[test]
    fn embedding_and_chat_namespaces_do_not_cross_resolve() {
        let catalog = Catalog::builtin();
        let emb = catalog.resolve("text-embedding-3-large");
        assert_eq!(emb.len(), 1);
        assert_eq!(emb[0].provider, "openai");
        assert!(catalog.resolve("text-embedding-004").iter().all(|m| m.provider == "gemini"));
    }

    #[test]
    fn custom_entries_override_builtin() {
        let catalog = Catalog::builtin().with_custom(vec![ModelInfo {
            id: "gpt-4.1".into(),
            provider: "ollama".into(),
            max_tokens: Some(4_096),
        }]);
        let hit = catalog.resolve("gpt-4.1");
        assert_eq!(hit.len(), 1);
        assert_eq!(hit[0].provider, "ollama");
    }
}
