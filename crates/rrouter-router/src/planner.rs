//! Model string → ordered attempt candidates.
//!
//! Resolution order is fixed and documented in `.orca-specs/T5-router.md`:
//! alias (one hop), then explicit `provider/model`, then `combo:<name>` (or a
//! combo that happens to share the client's model name), then a catalog lookup
//! over every active connection. Connections of equal priority rotate on
//! successive calls so a healthy account is not pinned to one token bucket.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use rrouter_core::{Dialect, Error, Result};
use rrouter_store::{Connection, Store};

use crate::candidate::{Candidate, Plan};
use crate::catalog::Catalog;

/// Provider registry order, used to pick a connection's dialect. Mirrors
/// `rrouter-auth`'s `ProviderSpec::default_dialect`; duplicated as a flat table
/// so routing does not depend on the auth crate's link-time registry.
fn provider_dialect(provider: &str) -> Dialect {
    match provider {
        "claude" | "anthropic" => Dialect::AnthropicMessages,
        "codex" => Dialect::OpenAiResponses,
        _ => Dialect::OpenAiChat,
    }
}

/// Registered provider ids. Only used to tell `provider/model` apart from a
/// model id that itself contains a slash (`meta-llama/Llama-3.3-70B`).
const KNOWN_PROVIDERS: &[&str] = &[
    "claude",
    "codex",
    "github",
    "openai",
    "anthropic",
    "deepseek",
    "gemini",
    "groq",
    "openrouter",
    "ollama",
    "xai",
    "mistral",
    "together",
    "cerebras",
];

/// Prefix that forces combo interpretation even when a combo name collides with
/// a model id.
const COMBO_PREFIX: &str = "combo:";

/// Pure planner: no I/O beyond the `Store` reads handed to it.
pub struct Planner {
    /// Round-robin cursor, advanced per `plan()` call.
    cursor: AtomicU64,
}

impl Default for Planner {
    fn default() -> Self {
        Self::new()
    }
}

impl Planner {
    pub fn new() -> Self {
        Self { cursor: AtomicU64::new(0) }
    }

    /// Build the ordered candidate list for `model`.
    /// `affinity` pins account selection for a conversation: equal-priority accounts are
    /// ordered by `affinity % group`, not round-robin. KV caches live per upstream
    /// account, so alternating accounts between turns turns every prompt into a cold
    /// write. Callers pass a stable hash of the request; `None` keeps round-robin for
    /// probes and embeddings where there is no conversation to preserve.
    pub async fn plan(
        &self,
        catalog: &Catalog,
        store: &Store,
        model: &str,
        stream: bool,
        affinity: Option<u64>,
    ) -> Result<Plan> {
        let connections = store.list_connections(None).await?;
        let aliases = store.list_aliases().await?;
        let disabled: Vec<String> = store.disabled_models().await?;

        let targets = self.targets(catalog, store, model, &aliases).await?;
        if targets.is_empty() {
            return Err(Error::NoRoute {
                model: model.to_string(),
                detail: "no connection declares this model and no alias/combo resolves it"
                    .to_string(),
            });
        }

        let active: Vec<&Connection> = connections.iter().filter(|c| c.is_active).collect();

        // A bare id the catalog does not know is taken as "every active provider can
        // serve it" — that is how a self-hosted or freshly published id reaches the
        // aggregators. An explicit `provider/model` never widens: asking for
        // `claude/claude-opus-5-5` must not be answered by a Codex account.
        let mut expanded: Vec<(String, String)> = Vec::with_capacity(targets.len());
        for (provider, model_id, explicit) in targets {
            if explicit || catalog.is_known(&model_id) {
                expanded.push((provider, model_id));
                continue;
            }
            let mut seen_providers: Vec<&str> = Vec::with_capacity(active.len());
            for conn in &active {
                if !seen_providers.contains(&conn.provider.as_str()) {
                    seen_providers.push(conn.provider.as_str());
                }
            }
            expanded.extend(
                seen_providers
                    .into_iter()
                    .map(|p| (p.to_string(), model_id.clone())),
            );
        }
        let targets = expanded;
        let disabled: std::collections::HashSet<&str> =
            disabled.iter().map(String::as_str).collect();

        // Dedupe (provider, model) targets while preserving declared order.
        let mut seen_pairs: Vec<(&str, &str)> = Vec::with_capacity(targets.len());
        for (provider, model_id) in &targets {
            let pair = (provider.as_str(), model_id.as_str());
            if !seen_pairs.contains(&pair) {
                seen_pairs.push(pair);
            }
        }

        let mut candidates = Vec::with_capacity(active.len());
        let offset = affinity.unwrap_or_else(|| self.next_offset());
        let target_count = seen_pairs.len();
        for (provider, model_id) in seen_pairs {
            if disabled.contains(model_id) {
                continue;
            }
            let mut matching: Vec<&Connection> =
                active.iter().copied().filter(|c| c.provider == provider).collect();
            if matching.is_empty() {
                continue;
            }
            // Priority ascending; round-robin only inside each equal-priority group
            // so the configured preference stays authoritative.
            matching.sort_by_key(|c| c.priority);
            let mut group_start = 0;
            while group_start < matching.len() {
                let priority = matching[group_start].priority;
                let mut group_end = group_start;
                while group_end < matching.len() && matching[group_end].priority == priority {
                    group_end += 1;
                }
                rotate(&mut matching[group_start..group_end], offset);
                group_start = group_end;
            }
            for conn in matching {
                candidates.push(Candidate {
                    provider: provider.to_string(),
                    model: model_id.to_string(),
                    connection: conn.id,
                    priority: conn.priority,
                    dialect: provider_dialect(provider),
                });
            }
        }

        if candidates.is_empty() {
            return Err(Error::NoRoute {
                model: model.to_string(),
                detail: format!(
                    "resolved to {target_count} provider/model target(s) but none has an active connection",
                ),
            });
        }

        Ok(Plan { model: model.to_string(), candidates, stream })
    }

    fn next_offset(&self) -> u64 {
        self.cursor.fetch_add(1, Ordering::Relaxed)
    }

    /// Ordered `(provider, model, explicit)` list for `model`, before connection
    /// expansion. `explicit` records that the caller named the provider, which stops
    /// an unknown id from being fanned out to every active connection.
    async fn targets(
        &self,
        catalog: &Catalog,
        store: &Store,
        model: &str,
        aliases: &[rrouter_store::ModelAlias],
    ) -> Result<Vec<(String, String, bool)>> {
        let mut current = model.to_string();

        // 1. Alias, exactly one hop: a self-referential or cyclic alias must not loop.
        if let Some(alias) = aliases.iter().find(|a| a.alias == current) {
            current = alias.target.clone();
        }

        // 3. Combo, by explicit prefix or by bare name. Checked before
        //    `provider/model` so a combo named `openrouter/x` stays a combo.
        let combo_name = current.strip_prefix(COMBO_PREFIX).unwrap_or(&current);
        if let Some(combo) = store.get_combo(combo_name).await? {
            let mut out = Vec::with_capacity(combo.models.len());
            let mut inner_aliases: HashMap<&str, &str> =
                HashMap::with_capacity(aliases.len());
            for a in aliases {
                inner_aliases.insert(a.alias.as_str(), a.target.as_str());
            }
            for entry in &combo.models {
                // Resolve one alias hop inside combo entries too; combos are
                // operator-authored and routinely mix aliases with raw ids.
                let resolved = inner_aliases.get(entry.as_str()).copied().unwrap_or(entry.as_str());
                out.extend(self.expand_entry(catalog, resolved));
            }
            return Ok(out);
        }
        if current.starts_with(COMBO_PREFIX) {
            return Err(Error::NotFound(format!("combo {}", combo_name)));
        }

        Ok(self.expand_entry(catalog, &current))
    }

    /// Expand one non-combo model string into `(provider, model, explicit)` targets.
    fn expand_entry(&self, catalog: &Catalog, entry: &str) -> Vec<(String, String, bool)> {
        // 2. Explicit `provider/model` for a registered provider.
        if let Some((head, rest)) = entry.split_once('/') {
            if !rest.is_empty() && KNOWN_PROVIDERS.contains(&head) {
                // Trust the caller even when the catalog does not know the id:
                // regional deployments and freshly published models are common.
                return vec![(head.to_string(), rest.to_string(), true)];
            }
        }

        // 4. Everything the catalog says serves this id.
        let hits = catalog.resolve(entry);
        if !hits.is_empty() {
            return hits.into_iter().map(|m| (m.provider, m.id, false)).collect();
        }

        // Unknown bare id (or a self-referential alias): no provider declares it,
        // so fall back to the aggregator and the self-hosted/local providers that
        // accept arbitrary ids. `plan` widens this to every active connection.
        ["openrouter", "ollama", "together", "groq", "cerebras"]
            .into_iter()
            .map(|provider| (provider.to_string(), entry.to_string(), false))
            .collect()
    }
}

/// Rotate `items` left by `offset` so successive plans start with a different
/// connection among equal priority. Ordering by priority afterwards keeps the
/// configured preference authoritative.
fn rotate<T>(items: &mut [T], offset: u64) {
    let len = items.len();
    if len < 2 {
        return;
    }
    let shift = (offset % len as u64) as usize;
    items.rotate_left(shift);
}
