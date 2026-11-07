//! Model string → ordered attempt candidates.
//!
//! Resolution order is fixed and documented in `.orca-specs/T5-router.md`:
//! alias (one hop), then explicit `provider/model`, then `combo:<name>` (or a
//! combo that happens to share the client's model name), then a catalog lookup
//! over every active connection. Connections of equal priority are ordered by the
//! configured [`Strategy`]: round-robin (default), a smooth weighted round-robin over
//! their `weight`, ascending EWMA latency, or the model's published price. Priority
//! always dominates the strategy, so a group is fully drained before the next one is
//! reached.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use rrouter_core::{Dialect, Error, Result};
use rrouter_store::{Connection, Store};
use uuid::Uuid;

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

/// How equal-priority connections are ordered inside one plan.
///
/// Priority is authoritative in every strategy: a connection at priority 0 is always
/// tried before one at priority 1. The strategy only decides the order *within* a group.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Strategy {
    /// Round-robin on the planner cursor: successive plans start on a different account.
    #[default]
    Priority,
    /// Smooth weighted round-robin on `connections.weight` (higher = more traffic).
    Weighted,
    /// Ascending measured latency; never-tried connections sort after measured ones.
    LeastLatency,
    /// Ascending `input + output` price per million tokens for the candidate's
    /// `(provider, model)`; a pair with no published price sorts after every priced one.
    Cheapest,
}

impl Strategy {
    /// Parse the `RROUTER_STRATEGY` value. `None` is an invalid value, which the
    /// gateway turns into a startup error rather than a silent fallback.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "priority" => Some(Self::Priority),
            "weighted" => Some(Self::Weighted),
            "least_latency" => Some(Self::LeastLatency),
            "cheapest" => Some(Self::Cheapest),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Priority => "priority",
            Self::Weighted => "weighted",
            Self::LeastLatency => "least_latency",
            Self::Cheapest => "cheapest",
        }
    }
}

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
        Self {
            cursor: AtomicU64::new(0),
        }
    }

    /// Build the ordered candidate list for `model`.
    /// `affinity` pins account selection for a conversation: equal-priority accounts are
    /// ordered by `affinity` as the strategy's offset instead of the planner cursor. KV
    /// caches live per upstream account, so alternating accounts between turns turns
    /// every prompt into a cold write. Callers pass a stable hash of the request; `None`
    /// advances the cursor, which is what probes, embeddings and the budgeted rotation
    /// want.
    ///
    /// `strategy` only orders connections of equal priority; priority itself always
    /// dominates, so an operator's preference is never overridden by a weight, a
    /// latency sample or a price.
    pub async fn plan(
        &self,
        catalog: &Catalog,
        store: &Store,
        model: &str,
        stream: bool,
        affinity: Option<u64>,
        strategy: Strategy,
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

        // Only `least_latency` needs the extra read; the other strategies must not pay
        // a query per plan for a table they never consult.
        let latencies = match strategy {
            Strategy::LeastLatency => store.connection_latencies().await?,
            _ => HashMap::new(),
        };
        let mut candidates = Vec::with_capacity(active.len());
        let offset = affinity.unwrap_or_else(|| self.next_offset());
        let target_count = seen_pairs.len();
        for (provider, model_id) in seen_pairs {
            if disabled.contains(model_id) {
                continue;
            }
            let mut matching: Vec<&Connection> = active
                .iter()
                .copied()
                .filter(|c| c.provider == provider)
                .collect();
            if matching.is_empty() {
                continue;
            }
            // Priority ascending; the strategy only orders inside each equal-priority
            // group so the configured preference stays authoritative.
            matching.sort_by_key(|c| c.priority);
            let mut group_start = 0;
            while group_start < matching.len() {
                let priority = matching[group_start].priority;
                let mut group_end = group_start;
                while group_end < matching.len() && matching[group_end].priority == priority {
                    group_end += 1;
                }
                let group = &mut matching[group_start..group_end];
                match strategy {
                    Strategy::Priority | Strategy::Cheapest => rotate(group, offset),
                    Strategy::Weighted => weighted_order(group, offset),
                    Strategy::LeastLatency => latency_order(group, &latencies),
                }
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

        if strategy == Strategy::Cheapest {
            candidates = cheapest_order(store, candidates).await;
        }

        if candidates.is_empty() {
            return Err(Error::NoRoute {
                model: model.to_string(),
                detail: format!(
                    "resolved to {target_count} provider/model target(s) but none has an active connection",
                ),
            });
        }

        // An account over its quota is not tried at all, so a healthy lower-priority
        // account still serves the request. When every candidate is exhausted the
        // request fails before any provider call, with the wait until the first reset.
        let exhausted = store.exhausted_accounts().await?;
        if !exhausted.connections.is_empty() {
            let planned = candidates.len();
            candidates.retain(|c| !exhausted.connections.contains(&c.connection));
            if candidates.is_empty() && planned > 0 {
                let wait = exhausted.retry_after_secs.unwrap_or(60);
                return Err(Error::Limited {
                    code: "quota_exhausted",
                    message: format!(
                        "every account for {model} is over its quota; the first resets in {wait}s",
                    ),
                    retry_after_secs: wait,
                });
            }
        }

        Ok(Plan {
            model: model.to_string(),
            candidates,
            stream,
        })
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
            let mut inner_aliases: HashMap<&str, &str> = HashMap::with_capacity(aliases.len());
            for a in aliases {
                inner_aliases.insert(a.alias.as_str(), a.target.as_str());
            }
            for entry in &combo.models {
                // Resolve one alias hop inside combo entries too; combos are
                // operator-authored and routinely mix aliases with raw ids.
                let resolved = inner_aliases
                    .get(entry.as_str())
                    .copied()
                    .unwrap_or(entry.as_str());
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
            return hits
                .into_iter()
                .map(|m| (m.provider, m.id, false))
                .collect();
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

/// Order one equal-priority group by smooth weighted round-robin.
///
/// The plan starts at pick `offset` of the SWRR cycle and each connection is ranked by
/// where it next comes up, so successive plans advance one pick at a time and a 3:1 pair
/// puts the heavy account first eight times out of ten. Nothing is stored between calls:
/// `offset` is the planner cursor, which makes the sequence deterministic and
/// restart-safe.
///
/// Weights are divided by their gcd before the simulation, which bounds the cycle
/// (`Σ weight/gcd`) without changing the ratio, so the per-plan cost stays small even
/// for the schema's 1..=1000 range with several default-100 connections.
fn weighted_order(group: &mut [&Connection], offset: u64) {
    let len = group.len();
    if len < 2 {
        return;
    }
    let gcd = group.iter().map(|c| c.weight).fold(0u32, gcd_u32).max(1);
    let weights: Vec<i64> = group
        .iter()
        .map(|c| i64::from(c.weight.max(1) / gcd))
        .collect();
    let total: i64 = weights.iter().sum();
    if total <= 0 {
        return;
    }
    let mut current = vec![0i64; len];
    let advance = |current: &mut [i64]| -> usize {
        // Smooth WRR: every candidate gains its weight, the largest gains the slot and
        // pays the whole cycle. Ties go to the earlier entry, which keeps it stable.
        let mut best = 0;
        for (i, weight) in weights.iter().enumerate() {
            current[i] += weight;
            if current[i] > current[best] {
                best = i;
            }
        }
        current[best] -= total;
        best
    };
    let start = (offset % total as u64) as usize;
    for _ in 0..start {
        advance(&mut current);
    }
    // Picks may repeat a connection (weights are not a permutation), so the group is
    // ordered by each connection's next scheduled pick: the first time it comes up.
    // Every weight is at least 1, so all `len` distinct indices appear within the cycle.
    let mut order: Vec<usize> = Vec::with_capacity(len);
    let mut seen = vec![false; len];
    while order.len() < len {
        let idx = advance(&mut current);
        if !seen[idx] {
            seen[idx] = true;
            order.push(idx);
        }
    }
    reorder(group, &order);
}

fn gcd_u32(a: u32, b: u32) -> u32 {
    if b == 0 {
        a
    } else {
        gcd_u32(b, a % b)
    }
}

/// Order one equal-priority group by measured latency: known connections first,
/// fastest first; never-measured connections after them, heavier first. Ties fall back
/// to weight and then to creation order, so the plan is deterministic.
fn latency_order(group: &mut [&Connection], latencies: &HashMap<Uuid, f64>) {
    group.sort_by(|a, b| {
        match (latencies.get(&a.id), latencies.get(&b.id)) {
            (Some(la), Some(lb)) => la.total_cmp(lb),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        }
        .then_with(|| b.weight.cmp(&a.weight))
        .then_with(|| a.created_at.cmp(&b.created_at))
        .then_with(|| a.id.cmp(&b.id))
    });
}

/// Stable reorder of a whole plan by price: priority first, because an operator's
/// preference must never be outbid, then `input + output` USD per million tokens for the
/// candidate's `(provider, model)`.
///
/// The price applies to every target in the plan rather than to one provider's accounts
/// in isolation — a model served by several providers at the same priority is exactly the
/// case this strategy exists for. Equal prices keep the order the group rotation produced,
/// so equal-priced accounts still alternate across plans.
async fn cheapest_order(store: &Store, candidates: Vec<Candidate>) -> Vec<Candidate> {
    let prices: Vec<Option<f64>> = {
        // One registry read per distinct pair, per plan: no cache, so a price the
        // operator just edited decides the next route.
        let mut seen: HashMap<(&str, &str), Option<f64>> = HashMap::with_capacity(candidates.len());
        let mut aligned = Vec::with_capacity(candidates.len());
        for candidate in &candidates {
            let key = (candidate.provider.as_str(), candidate.model.as_str());
            let price = match seen.get(&key) {
                Some(price) => *price,
                None => {
                    let price =
                        target_price(store, candidate.provider.as_str(), candidate.model.as_str())
                            .await;
                    seen.insert(key, price);
                    price
                }
            };
            aligned.push(price);
        }
        aligned
    };

    let mut priced: Vec<(Option<f64>, Candidate)> = prices.into_iter().zip(candidates).collect();
    priced.sort_by(|(price_a, a), (price_b, b)| {
        a.priority
            .cmp(&b.priority)
            .then_with(|| price_order(*price_a, *price_b))
    });
    priced.into_iter().map(|(_, candidate)| candidate).collect()
}

/// Ascending price; `None` (no published price) sorts after every known price.
fn price_order(a: Option<f64>, b: Option<f64>) -> std::cmp::Ordering {
    match (a, b) {
        (Some(a), Some(b)) => a.total_cmp(&b),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    }
}

/// `input + output` USD per million tokens for one `(provider, model)` pair, from the
/// registry rows the cost accounting prices a request with.
///
/// Precedence mirrors the registry merge: the provider's own row wins over the models.dev
/// vendor row it mirrors, and within one provider `custom` > `upstream` > `synced`. A pair
/// with no price, or a failed read, is `None`: routing must not invent a rate, and a
/// broken registry must not fail every plan while another strategy still works.
async fn target_price(store: &Store, provider: &str, model: &str) -> Option<f64> {
    let vendor = crate::catalog::models_dev_provider(provider);
    // Usage rows record the resolved ref (`provider/id`), while the registry keys models
    // by bare id. Strip only this provider's own prefix, so `meta-llama/...` survives.
    let bare = model
        .strip_prefix(provider)
        .and_then(|rest| rest.strip_prefix('/'))
        .unwrap_or(model);
    let rows = match store.model_costs(bare, &[provider, vendor]).await {
        Ok(rows) if rows.is_empty() && bare != model => store
            .model_costs(model, &[provider, vendor])
            .await
            .unwrap_or_default(),
        Ok(rows) => rows,
        Err(e) => {
            tracing::debug!(error = %e, provider, model, "price lookup failed; treating as unpriced");
            return None;
        }
    };
    let mut best: Option<((u8, u8), f64)> = None;
    for row in &rows {
        let Some(price) = published_price(&row.cost) else {
            continue;
        };
        // The provider's own row first, then the registry's source precedence.
        let key = (u8::from(row.provider == provider), source_rank(&row.source));
        let better = match best.as_ref() {
            Some((best_key, _)) => key > *best_key,
            None => true,
        };
        if better {
            best = Some((key, price));
        }
    }
    best.map(|(_, price)| price)
}

/// Per-million-token price a stored `cost` blob publishes, or `None` when it publishes no
/// rate at all. Same validity rule as the accounting's `rates`: a partial price prices
/// the missing side at 0 rather than disqualifying the row.
fn published_price(value: &serde_json::Value) -> Option<f64> {
    if !value.is_object() {
        return None;
    }
    let rate = |key: &str| value.get(key).and_then(serde_json::Value::as_f64);
    let (input, output) = (rate("input"), rate("output"));
    if input.is_none()
        && output.is_none()
        && rate("cache_read").is_none()
        && rate("cache_write").is_none()
    {
        return None;
    }
    Some(input.unwrap_or(0.0) + output.unwrap_or(0.0))
}

/// Precedence of a stored source. Higher wins; unknown values rank lowest.
fn source_rank(source: &str) -> u8 {
    match source {
        "custom" => 3,
        "upstream" => 2,
        "synced" => 1,
        _ => 0,
    }
}

/// Apply an index permutation to a slice in place.
fn reorder<T: Copy>(items: &mut [T], order: &[usize]) {
    let mut slots: Vec<Option<T>> = items.iter().copied().map(Some).collect();
    for (dst, &src) in order.iter().enumerate() {
        items[dst] = slots[src].take().expect("every index appears once");
    }
}
