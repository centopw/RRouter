//! Combo metadata: resolve each member to the rows a request can actually land on, then
//! fold that set with commutative operators.
//!
//! A combo has no single model behind it, so its advertised limits and price are derived,
//! never sampled. Resolution mirrors the planner (`expand_entry_resolved`): an alias hop,
//! then `provider/id`, then `group/code/id`, then a bare id fanned out to every provider
//! that serves it, then version-core matches. Every fold below is commutative and the
//! target set is sorted, so the answer never depends on registry or provider order.
//!
//! Nothing here performs I/O. [`RegistrySnapshot`] is the one loader, for callers that
//! do not already hold the registry and preset.

use std::collections::HashMap;

use rrouter_router::catalog::core_of;
use rrouter_store::{Combo, ComboStrategy, LayoutDocument, LayoutGroup, ModelAlias};
use serde_json::{json, Map, Value};

use crate::models_registry::{self, Capabilities, ModelItem};
use crate::state::AppState;

/// What one member string resolved to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberState {
    /// A provider-qualified ref (or a single-provider bare id) named one row.
    Exact,
    /// A bare id served by several providers; every one may serve a request.
    Fanout,
    /// A suffix-dated id matched curated rows by version core.
    Core,
    /// The row exists but is disabled on every provider; it contributes no numbers.
    Disabled,
    /// Nothing in the registry matches; guarantees become unknown, never guessed.
    Unknown,
}

impl MemberState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Fanout => "fanout",
            Self::Core => "core",
            Self::Disabled => "disabled",
            Self::Unknown => "unknown",
        }
    }
}

/// One combo member, resolved to every routable row it can land on.
#[derive(Debug, Clone, PartialEq)]
pub struct Member {
    /// Exactly the stored `models[i]` string, so the editor can echo it unchanged.
    pub reference: String,
    pub state: MemberState,
    /// The alias that was followed, when the member named one.
    pub via_alias: Option<String>,
    /// Sorted by `(provider, id)`; disabled targets are kept and marked.
    pub targets: Vec<ModelItem>,
}

impl Member {
    /// Compatibility view: the first target, which is also the first the router would
    /// consider because the set is sorted.
    pub fn model(&self) -> Option<&ModelItem> {
        self.targets.first()
    }

    fn enabled(&self) -> impl Iterator<Item = &ModelItem> {
        self.targets.iter().filter(|target| target.enabled)
    }

    fn to_json(&self) -> Value {
        let model = match self.model() {
            Some(model) => serde_json::to_value(model).unwrap_or(Value::Null),
            None => Value::Null,
        };
        let targets: Vec<Value> = self
            .targets
            .iter()
            .map(|target| serde_json::to_value(target).unwrap_or(Value::Null))
            .collect();
        json!({
            "ref": self.reference,
            "state": self.state.as_str(),
            "via_alias": self.via_alias,
            "model": model,
            "targets": targets,
        })
    }
}

/// A lower and upper bound. `min` is the guaranteed floor for any request, `max` the
/// best case; both are absent only when the aggregate itself is unknown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LimitRange {
    pub min: Option<u32>,
    pub max: Option<u32>,
}

/// Min/max over the known prices of one dimension, plus the coverage counts that say
/// how much of the target set the range actually describes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PriceRange {
    pub min: Option<f64>,
    pub max: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pricing {
    pub input: PriceRange,
    pub output: PriceRange,
    pub cache_read: PriceRange,
    pub cache_write: PriceRange,
    pub known: usize,
    pub total: usize,
}

impl PriceRange {
    fn from_values(values: &[f64]) -> Self {
        let mut iter = values.iter();
        let Some(first) = iter.next() else {
            return Self {
                min: None,
                max: None,
            };
        };
        let (mut min, mut max) = (*first, *first);
        for value in iter {
            min = min.min(*value);
            max = max.max(*value);
        }
        Self {
            min: Some(min),
            max: Some(max),
        }
    }

    fn to_json(self) -> Value {
        json!({ "min": self.min, "max": self.max })
    }
}

/// Can any request be served at all, and if not, why.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Availability {
    Ok,
    NoTargets,
    RouterUnavailable,
}

impl Availability {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::NoTargets => "no-targets",
            Self::RouterUnavailable => "router-unavailable",
        }
    }
}

/// The decision model of a `smart` combo. A separate billable call: never folded into
/// the candidate numbers.
#[derive(Debug, Clone, PartialEq)]
pub struct Router {
    pub reference: String,
    pub model: Option<ModelItem>,
}

/// The folded description of one combo.
#[derive(Debug, Clone, PartialEq)]
pub struct Aggregate {
    pub context: Option<LimitRange>,
    pub max_output: Option<LimitRange>,
    pub pricing: Option<Pricing>,
    pub guaranteed_capabilities: Option<Capabilities>,
    pub supported_capabilities: Option<Capabilities>,
    /// Parameter-shaped facts, without a wire key of their own: identical across every
    /// enabled target, else `None`.
    pub reasoning: Option<models_registry::ReasoningSupport>,
    pub effort: Option<Value>,
    pub api_capabilities: Option<Value>,
    pub capabilities_extra: Option<Map<String, Value>>,
    pub deprecated_members: usize,
    /// Every enabled target is deprecated. A retired row stays routable, so this is a
    /// warning, not a routing rule.
    pub all_deprecated: bool,
    pub availability: Availability,
    /// `smart` only.
    pub router: Option<Router>,
}

impl Aggregate {
    fn to_json(&self) -> Value {
        let capabilities = |caps: Option<Capabilities>| match caps {
            Some(caps) => serde_json::to_value(caps).unwrap_or(Value::Null),
            None => Value::Null,
        };
        let mut object = Map::new();
        object.insert(
            "context".into(),
            match self.context {
                Some(range) => json!({ "min": range.min, "max": range.max }),
                None => Value::Null,
            },
        );
        object.insert(
            "max_output".into(),
            match self.max_output {
                Some(range) => json!({ "min": range.min, "max": range.max }),
                None => Value::Null,
            },
        );
        object.insert(
            "pricing".into(),
            match &self.pricing {
                Some(pricing) => json!({
                    "input": pricing.input.to_json(),
                    "output": pricing.output.to_json(),
                    "cache_read": pricing.cache_read.to_json(),
                    "cache_write": pricing.cache_write.to_json(),
                    "known": pricing.known,
                    "total": pricing.total,
                }),
                None => Value::Null,
            },
        );
        object.insert(
            "guaranteed_capabilities".into(),
            capabilities(self.guaranteed_capabilities),
        );
        object.insert(
            "supported_capabilities".into(),
            capabilities(self.supported_capabilities),
        );
        object.insert(
            "deprecated_members".into(),
            Value::from(self.deprecated_members),
        );
        object.insert("all_deprecated".into(), Value::Bool(self.all_deprecated));
        object.insert(
            "availability".into(),
            Value::String(self.availability.as_str().to_string()),
        );
        if let Some(router) = &self.router {
            object.insert(
                "router".into(),
                json!({
                    "ref": router.reference,
                    "model": match &router.model {
                        Some(model) => serde_json::to_value(model).unwrap_or(Value::Null),
                        None => Value::Null,
                    },
                }),
            );
        }
        Value::Object(object)
    }
}

/// A resolved combo: every member's target set plus the fold over the union.
#[derive(Debug, Clone, PartialEq)]
pub struct ComboMetadata {
    pub members: Vec<Member>,
    pub aggregate: Option<Aggregate>,
}

impl ComboMetadata {
    pub fn members_json(&self) -> Value {
        Value::Array(self.members.iter().map(Member::to_json).collect())
    }

    pub fn aggregate_json(&self) -> Value {
        match &self.aggregate {
            Some(aggregate) => aggregate.to_json(),
            None => Value::Null,
        }
    }

    /// The legacy flat `/v1` view, derived from the same aggregate so the two can never
    /// disagree. `None` when the combo has no members at all.
    pub fn legacy_meta(&self) -> Option<ModelItem> {
        let aggregate = self.aggregate.as_ref()?;
        Some(ModelItem {
            reference: "rrouter/combo".to_string(),
            provider: "rrouter".to_string(),
            code: "rrouter".to_string(),
            id: String::new(),
            display: None,
            context: aggregate.context.and_then(|range| range.min),
            max_output: aggregate.max_output.and_then(|range| range.min),
            capabilities: aggregate.guaranteed_capabilities.unwrap_or_default(),
            capabilities_extra: aggregate.capabilities_extra.clone(),
            effort: aggregate.effort.clone(),
            api_capabilities: aggregate.api_capabilities.clone(),
            reasoning: aggregate.reasoning.clone(),
            cost: None,
            source: "combo".to_string(),
            enabled: true,
            available: aggregate.availability == Availability::Ok,
            deprecated: aggregate.deprecated_members > 0,
        })
    }
}

/// A resolved registry, ready to answer member lookups without touching the store.
pub struct RegistryIndex<'a> {
    /// Sorted by `(provider, id)`, so every target list below is deterministic.
    items: Vec<&'a ModelItem>,
    by_id: HashMap<&'a str, Vec<usize>>,
    by_core: HashMap<&'a str, Vec<usize>>,
    /// Ids sharing a provider are contiguous in `items`.
    by_provider: HashMap<&'a str, Vec<usize>>,
    /// Provider id or shorthand -> provider id. The id wins on a collision.
    segments: HashMap<&'a str, &'a str>,
    groups: &'a [LayoutGroup],
    aliases: &'a [ModelAlias],
}

impl<'a> RegistryIndex<'a> {
    pub fn new(
        items: &'a [ModelItem],
        providers: &'a [(String, String)],
        groups: &'a [LayoutGroup],
        aliases: &'a [ModelAlias],
    ) -> Self {
        let mut sorted: Vec<&'a ModelItem> = items.iter().collect();
        sorted.sort_by(|a, b| {
            a.provider
                .cmp(&b.provider)
                .then_with(|| a.id.cmp(&b.id))
                .then_with(|| a.reference.cmp(&b.reference))
        });

        let mut by_id: HashMap<&'a str, Vec<usize>> = HashMap::new();
        let mut by_core: HashMap<&'a str, Vec<usize>> = HashMap::new();
        let mut by_provider: HashMap<&'a str, Vec<usize>> = HashMap::new();
        for (index, item) in sorted.iter().enumerate() {
            by_id.entry(item.id.as_str()).or_default().push(index);
            by_core
                .entry(core_of(item.id.as_str()))
                .or_default()
                .push(index);
            by_provider
                .entry(item.provider.as_str())
                .or_default()
                .push(index);
        }

        // Shorthands first, ids second, so an id that is also some other provider's
        // shorthand still names itself.
        let mut segments: HashMap<&'a str, &'a str> = HashMap::new();
        for (provider, code) in providers {
            if !code.is_empty() {
                segments.insert(code.as_str(), provider.as_str());
            }
        }
        for (provider, _) in providers {
            segments.insert(provider.as_str(), provider.as_str());
        }

        Self {
            items: sorted,
            by_id,
            by_core,
            by_provider,
            segments,
            groups,
            aliases,
        }
    }

    fn provider_for(&self, segment: &str) -> Option<&'a str> {
        self.segments.get(segment).copied()
    }

    /// The row for `(provider, id)`, if the registry has it. Never falls back to another
    /// provider: an unknown row is unknown, not a guess.
    fn row(&self, provider: &str, id: &str) -> Option<usize> {
        let rows = self.by_provider.get(provider)?;
        rows.binary_search_by(|index| self.items[*index].id.as_str().cmp(id))
            .ok()
            .map(|position| rows[position])
    }

    /// Resolve one combo member string to its sorted target set.
    pub fn resolve_member(&self, entry: &str) -> Member {
        let mut current = entry;
        let mut via_alias = None;
        // One hop, exactly as routing does for a combo entry.
        if let Some(alias) = self.aliases.iter().find(|alias| alias.alias == entry) {
            via_alias = Some(entry.to_string());
            current = alias.target.as_str();
        }

        let (indices, path) = match self.resolve_targets(current) {
            Some((indices, path)) => (indices, path),
            None => (Vec::new(), ResolutionPath::Bare),
        };
        let targets: Vec<ModelItem> = indices
            .into_iter()
            .map(|index| self.items[index].clone())
            .collect();
        let state = if targets.is_empty() {
            MemberState::Unknown
        } else if targets.iter().all(|target| !target.enabled) {
            MemberState::Disabled
        } else {
            match path {
                ResolutionPath::Core => MemberState::Core,
                ResolutionPath::Explicit => MemberState::Exact,
                ResolutionPath::Bare if targets.len() > 1 => MemberState::Fanout,
                ResolutionPath::Bare => MemberState::Exact,
            }
        };
        Member {
            reference: entry.to_string(),
            state,
            via_alias,
            targets,
        }
    }

    fn resolve_targets(&self, entry: &str) -> Option<(Vec<usize>, ResolutionPath)> {
        if let Some((head, rest)) = entry.split_once('/') {
            if !rest.is_empty() {
                // 2. Explicit `provider/id`, id or shorthand: pin that provider and never
                // re-interpret the id on another one.
                if let Some(provider) = self.provider_for(head) {
                    let indices = self.row(provider, rest).into_iter().collect();
                    return Some((indices, ResolutionPath::Explicit));
                }
                // 3. `group/code/id`: the head must be a layout group and the middle
                // segment must name a provider.
                if let Some((segment, id)) = rest.split_once('/') {
                    if !id.is_empty() && self.groups.iter().any(|group| group.name == head) {
                        if let Some(provider) = self.provider_for(segment) {
                            let indices = self.row(provider, id).into_iter().collect();
                            return Some((indices, ResolutionPath::Explicit));
                        }
                    }
                }
            }
        }

        // 4. Bare id: every row serving it, then version-core matches.
        if let Some(indices) = self.by_id.get(entry) {
            return Some((indices.clone(), ResolutionPath::Bare));
        }
        let core = core_of(entry);
        self.by_core
            .get(core)
            .map(|indices| (indices.clone(), ResolutionPath::Core))
    }

    /// The single row an alias is advertised with: its first enabled target.
    pub fn resolve_one(&self, entry: &str) -> Option<ModelItem> {
        self.resolve_member(entry).enabled().next().cloned()
    }
}

enum ResolutionPath {
    Explicit,
    Bare,
    Core,
}

/// Fold resolved members into the combo's advertised numbers.
///
/// `strategy` and `router` change availability and the router block only; every numeric
/// field is a function of `members` alone, because every request can reach every target
/// under every strategy the gateway implements.
pub fn aggregate(
    strategy: ComboStrategy,
    members: &[Member],
    router: Option<&Member>,
) -> Option<Aggregate> {
    if members.is_empty() {
        return None;
    }
    let any_unknown = members
        .iter()
        .any(|member| member.state == MemberState::Unknown);
    // The union of every member's targets, deduped: a fan-out and a core match can name
    // the same row, and the same member can be listed twice. Sorting first makes the fold
    // independent of member order and makes the dedup explicit.
    let mut targets: Vec<&ModelItem> = members.iter().flat_map(|member| member.enabled()).collect();
    targets.sort_by(|a, b| a.provider.cmp(&b.provider).then_with(|| a.id.cmp(&b.id)));
    targets.dedup_by(|a, b| a.provider == b.provider && a.id == b.id);

    let context = limit_range(&targets, any_unknown, |item| item.context);
    let max_output = limit_range(&targets, any_unknown, |item| item.max_output);

    // Pricing is accounting data, not a safety guarantee: a range over the known prices
    // plus an explicit coverage count beats hiding everything when one row is unpriced.
    let mut input = Vec::new();
    let mut output = Vec::new();
    let mut cache_read = Vec::new();
    let mut cache_write = Vec::new();
    for target in &targets {
        let Some(cost) = &target.cost else { continue };
        input.push(cost.input);
        output.push(cost.output);
        if let Some(rate) = cost.cache_read {
            cache_read.push(rate);
        }
        if let Some(rate) = cost.cache_write {
            cache_write.push(rate);
        }
    }
    let pricing = (!input.is_empty()).then(|| Pricing {
        input: PriceRange::from_values(&input),
        output: PriceRange::from_values(&output),
        cache_read: PriceRange::from_values(&cache_read),
        cache_write: PriceRange::from_values(&cache_write),
        known: input.len(),
        // Resolvable targets, not combo members: an unresolved member contributes
        // nothing to the range or its coverage.
        total: targets.len(),
    });

    let (guaranteed_capabilities, supported_capabilities) = if any_unknown || targets.is_empty() {
        (None, None)
    } else {
        let mut guaranteed = targets[0].capabilities;
        let mut supported = targets[0].capabilities;
        for target in &targets[1..] {
            guaranteed = guaranteed.intersect(target.capabilities);
            supported = supported.union(target.capabilities);
        }
        (Some(guaranteed), Some(supported))
    };

    let identical = |get: &dyn Fn(&ModelItem) -> Option<&Value>| -> Option<Value> {
        common_value(&targets, any_unknown, get)
    };
    let identical_extra = || -> Option<Map<String, Value>> {
        let first = targets.first()?;
        if any_unknown {
            return None;
        }
        targets
            .iter()
            .all(|target| target.capabilities_extra == first.capabilities_extra)
            .then(|| first.capabilities_extra.clone())
            .flatten()
    };
    let identical_reasoning = || -> Option<models_registry::ReasoningSupport> {
        let first = targets.first()?;
        if any_unknown {
            return None;
        }
        targets
            .iter()
            .all(|target| target.reasoning == first.reasoning)
            .then(|| first.reasoning.clone())
            .flatten()
    };

    let deprecated_members = targets.iter().filter(|target| target.deprecated).count();
    let all_deprecated = !targets.is_empty() && deprecated_members == targets.len();

    let router_member = router.filter(|_| strategy == ComboStrategy::Smart);
    let router_available = router_member.is_some_and(|member| member.enabled().next().is_some());
    let availability = if targets.is_empty() {
        Availability::NoTargets
    } else if strategy == ComboStrategy::Smart && !router_available {
        Availability::RouterUnavailable
    } else {
        Availability::Ok
    };

    Some(Aggregate {
        context,
        max_output,
        pricing,
        guaranteed_capabilities,
        supported_capabilities,
        reasoning: identical_reasoning(),
        effort: identical(&|item| item.effort.as_ref()),
        api_capabilities: identical(&|item| item.api_capabilities.as_ref()),
        capabilities_extra: identical_extra(),
        deprecated_members,
        all_deprecated,
        availability,
        router: router_member.map(|member| Router {
            reference: member.reference.clone(),
            model: member.enabled().next().cloned(),
        }),
    })
}

fn limit_range(
    targets: &[&ModelItem],
    any_unknown: bool,
    get: impl Fn(&ModelItem) -> Option<u32>,
) -> Option<LimitRange> {
    if any_unknown || targets.is_empty() {
        return None;
    }
    let mut min = u32::MAX;
    let mut max = 0;
    for target in targets {
        let value = get(target)?;
        min = min.min(value);
        max = max.max(value);
    }
    Some(LimitRange {
        min: Some(min),
        max: Some(max),
    })
}

/// A value carried only when every enabled target agrees on it.
fn common_value(
    targets: &[&ModelItem],
    any_unknown: bool,
    get: &dyn Fn(&ModelItem) -> Option<&Value>,
) -> Option<Value> {
    let first = targets.first()?;
    if any_unknown {
        return None;
    }
    let value = get(first)?;
    targets
        .iter()
        .all(|target| get(target) == Some(value))
        .then(|| value.clone())
}

/// Resolve one combo against an index: the per-member target sets plus the fold.
pub fn combo_metadata(index: &RegistryIndex<'_>, combo: &Combo) -> ComboMetadata {
    let members: Vec<Member> = combo
        .models
        .iter()
        .map(|entry| index.resolve_member(entry))
        .collect();
    let router = combo
        .router_model
        .as_ref()
        .map(|reference| index.resolve_member(reference));
    let aggregate = aggregate(combo.strategy, &members, router.as_ref());
    ComboMetadata { members, aggregate }
}

/// A registry plus the preset and aliases resolution needs, loaded once per request.
pub struct RegistrySnapshot {
    pub items: Vec<ModelItem>,
    pub aliases: Vec<ModelAlias>,
    pub layout: Option<LayoutDocument>,
    /// `(provider id, shorthand)` pairs, sorted by id.
    pub providers: Vec<(String, String)>,
}

impl RegistrySnapshot {
    /// Load everything resolution needs. Only the registry read can fail; a missing
    /// preset or alias table degrades to "no groups" / "no aliases".
    pub async fn load(state: &AppState) -> rrouter_core::Result<Self> {
        let items = models_registry::list(state).await?;
        let aliases = state.store.list_aliases().await.unwrap_or_default();
        let layout = state
            .store
            .default_model_preset()
            .await
            .ok()
            .flatten()
            .map(|preset| preset.layout);
        Ok(Self::new(items, aliases, layout))
    }

    pub fn new(
        items: Vec<ModelItem>,
        aliases: Vec<ModelAlias>,
        layout: Option<LayoutDocument>,
    ) -> Self {
        // Compiled providers are routable even before a registry row exists; a registry
        // row's code is authoritative when the operator renamed the shorthand.
        let mut providers: std::collections::BTreeMap<String, String> = rrouter_auth::all()
            .map(|spec| {
                (
                    spec.id.to_string(),
                    crate::provider_catalog::default_code(spec.id),
                )
            })
            .collect();
        for item in &items {
            providers.insert(item.provider.clone(), item.code.clone());
        }
        Self {
            items,
            aliases,
            layout,
            providers: providers.into_iter().collect(),
        }
    }

    pub fn index(&self) -> RegistryIndex<'_> {
        RegistryIndex::new(
            &self.items,
            &self.providers,
            self.layout
                .as_ref()
                .map(|layout| layout.groups.as_slice())
                .unwrap_or(&[]),
            &self.aliases,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models_registry::Cost;

    fn item(provider: &str, id: &str, context: u32, max_output: u32) -> ModelItem {
        ModelItem {
            reference: format!("{provider}/{id}"),
            provider: provider.to_string(),
            code: provider.to_string(),
            id: id.to_string(),
            display: None,
            context: Some(context),
            max_output: Some(max_output),
            capabilities: Capabilities::default(),
            capabilities_extra: None,
            effort: None,
            api_capabilities: None,
            reasoning: None,
            cost: None,
            source: "catalog".to_string(),
            enabled: true,
            available: true,
            deprecated: false,
        }
    }

    fn priced(provider: &str, id: &str, context: u32, input: f64, output: f64) -> ModelItem {
        let mut model = item(provider, id, context, 8_192);
        model.cost = Some(Cost {
            input,
            output,
            cache_read: None,
            cache_write: None,
        });
        model
    }

    fn providers() -> &'static [(String, String)] {
        use std::sync::OnceLock;
        static PROVIDERS: OnceLock<Vec<(String, String)>> = OnceLock::new();
        PROVIDERS.get_or_init(|| {
            vec![
                ("openai".to_string(), "oai".to_string()),
                ("anthropic".to_string(), "ant".to_string()),
            ]
        })
    }

    fn index<'a>(items: &'a [ModelItem], aliases: &'a [ModelAlias]) -> RegistryIndex<'a> {
        RegistryIndex::new(items, providers(), &[], aliases)
    }

    fn combo(models: &[&str]) -> Combo {
        Combo {
            id: uuid::Uuid::nil(),
            name: "test".to_string(),
            models: models.iter().map(|model| model.to_string()).collect(),
            strategy: ComboStrategy::Fallback,
            router_model: None,
            cursor: 0,
            created_at: 0,
        }
    }

    #[test]
    fn aggregate_is_independent_of_target_order() {
        let items = vec![
            priced("openai", "a", 1_048_576, 2.0, 8.0),
            priced("anthropic", "b", 262_144, 1.0, 4.0),
        ];
        let index = index(&items, &[]);
        let forward = combo_metadata(&index, &combo(&["a", "b"]));
        let reverse = combo_metadata(&index, &combo(&["b", "a"]));
        assert_eq!(
            forward.aggregate_json(),
            reverse.aggregate_json(),
            "the fold must not depend on member order"
        );
    }

    #[test]
    fn aggregate_is_idempotent_under_duplicate_targets() {
        let items = vec![priced("openai", "a", 1_048_576, 2.0, 8.0)];
        let index = index(&items, &[]);
        let once = combo_metadata(&index, &combo(&["a"]));
        let twice = combo_metadata(&index, &combo(&["a", "a"]));
        assert_eq!(once.aggregate_json(), twice.aggregate_json());
    }

    #[test]
    fn adding_a_member_never_raises_the_guaranteed_floor() {
        let items = vec![
            item("openai", "a", 1_048_576, 393_216),
            item("anthropic", "b", 262_144, 64_000),
            item("anthropic", "c", 524_288, 128_000),
        ];
        let index = index(&items, &[]);
        let two = combo_metadata(&index, &combo(&["a", "b"]));
        let three = combo_metadata(&index, &combo(&["a", "b", "c"]));
        let floor = |metadata: &ComboMetadata| {
            metadata
                .aggregate
                .as_ref()
                .and_then(|aggregate| aggregate.context)
                .and_then(|range| range.min)
        };
        assert_eq!(floor(&two), Some(262_144));
        assert_eq!(floor(&three), Some(262_144));
        let ceiling = |metadata: &ComboMetadata| {
            metadata
                .aggregate
                .as_ref()
                .and_then(|aggregate| aggregate.context)
                .and_then(|range| range.max)
        };
        assert_eq!(ceiling(&two), Some(1_048_576));
        assert_eq!(ceiling(&three), Some(1_048_576));
    }

    #[test]
    fn shorthand_ref_resolves_only_to_its_provider() {
        let items = vec![
            item("openai", "x", 1_048_576, 393_216),
            item("anthropic", "x", 262_144, 64_000),
        ];
        let index = index(&items, &[]);
        let member = index.resolve_member("oai/x");
        assert_eq!(member.state, MemberState::Exact);
        assert_eq!(member.targets.len(), 1);
        assert_eq!(member.targets[0].provider, "openai");
        assert_eq!(member.targets[0].context, Some(1_048_576));
    }

    #[test]
    fn bare_id_fans_out_to_every_enabled_provider() {
        let items = vec![
            item("openai", "x", 1_048_576, 393_216),
            item("anthropic", "x", 262_144, 64_000),
        ];
        let index = index(&items, &[]);
        let member = index.resolve_member("x");
        assert_eq!(member.state, MemberState::Fanout);
        assert_eq!(member.targets.len(), 2);
        assert_eq!(
            member
                .targets
                .iter()
                .map(|target| target.provider.as_str())
                .collect::<Vec<_>>(),
            vec!["anthropic", "openai"],
            "targets are sorted by (provider, id)"
        );
    }

    #[test]
    fn core_match_resolves_a_dated_id() {
        let items = vec![item("openai", "gpt-4o", 128_000, 16_384)];
        let index = index(&items, &[]);
        let member = index.resolve_member("gpt-4o-2024-08-06");
        assert_eq!(member.state, MemberState::Core);
        assert_eq!(member.targets.len(), 1);
        assert_eq!(member.targets[0].id, "gpt-4o");
    }

    #[test]
    fn group_ref_member_resolves_through_the_middle_segment() {
        let items = vec![item("openai", "x", 1_048_576, 393_216)];
        let groups = vec![LayoutGroup {
            id: "g-main".to_string(),
            name: "main".to_string(),
            provider: None,
            visible: true,
            access: None,
        }];
        let index = RegistryIndex::new(&items, providers(), &groups, &[]);
        let member = index.resolve_member("main/oai/x");
        assert_eq!(member.state, MemberState::Exact);
        assert_eq!(member.targets.len(), 1);
        assert_eq!(member.targets[0].provider, "openai");
    }

    #[test]
    fn alias_member_resolves_one_hop() {
        let items = vec![item("openai", "x", 1_048_576, 393_216)];
        let aliases = vec![ModelAlias {
            alias: "fast".to_string(),
            target: "oai/x".to_string(),
        }];
        let index = index(&items, &aliases);
        let member = index.resolve_member("fast");
        assert_eq!(member.via_alias.as_deref(), Some("fast"));
        assert_eq!(member.state, MemberState::Exact);
        assert_eq!(member.targets[0].context, Some(1_048_576));
    }

    #[test]
    fn combo_member_is_unknown_not_nested() {
        let items = vec![item("openai", "x", 1_048_576, 393_216)];
        let index = index(&items, &[]);
        let member = index.resolve_member("other-combo");
        assert_eq!(member.state, MemberState::Unknown);
        assert!(member.targets.is_empty());
        let metadata = combo_metadata(&index, &combo(&["x", "other-combo"]));
        let aggregate = metadata.aggregate.as_ref().expect("aggregate");
        assert!(aggregate.context.is_none(), "guarantees become unknown");
        assert!(metadata.legacy_meta().is_some(), "the combo stays listed");
    }

    #[test]
    fn disabled_member_is_excluded_from_guarantees() {
        let mut disabled = item("anthropic", "small", 8_192, 1_024);
        disabled.enabled = false;
        let items = vec![item("openai", "big", 1_048_576, 393_216), disabled];
        let index = index(&items, &[]);
        let metadata = combo_metadata(&index, &combo(&["big", "small"]));
        let aggregate = metadata.aggregate.expect("aggregate");
        assert_eq!(
            aggregate.context.and_then(|range| range.min),
            Some(1_048_576)
        );
        assert_eq!(aggregate.deprecated_members, 0);
    }

    #[test]
    fn pricing_is_a_range_with_coverage_counts() {
        let mut unpriced = item("anthropic", "b", 262_144, 64_000);
        unpriced.cost = None;
        let items = vec![priced("openai", "a", 1_048_576, 2.0, 8.0), unpriced];
        let index = index(&items, &[]);
        let metadata = combo_metadata(&index, &combo(&["a", "b"]));
        let aggregate = metadata.aggregate.expect("aggregate");
        let pricing = aggregate.pricing.expect("pricing");
        assert_eq!(pricing.known, 1);
        assert_eq!(pricing.total, 2);
        assert_eq!(pricing.input.min, Some(2.0));
        assert_eq!(pricing.input.max, Some(2.0));
    }
}
