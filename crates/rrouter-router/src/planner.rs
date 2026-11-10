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
//!
//! Two request-scoped inputs are applied *after* expansion, because both are about the
//! provider that would actually be called:
//!
//! - the caller's `allowed_models`, via [`candidate_allowed`] — a provider-scoped entry
//!   never leaks the request to another provider that happens to serve the same id;
//! - the request's [`ProviderPolicy`], as a hard filter plus an intra-priority ordering
//!   hint. `priority` stays authoritative: a request can reorder the accounts an operator
//!   put in the same tier, never promote a lower tier.
//!
//! One input is applied *before* expansion, because it decides which accounts exist at
//! all: [`PlanRequest::team_id`]. A team-scoped key is planned from that team's accounts
//! only — the shared rows (`connections.team_id IS NULL`) are the instance's and never
//! reach a team key — so a team that owns no account for the model fails with a `NoRoute`
//! that names the team rather than being quietly served by someone else's account.
//!
//! A [`CanaryRule`] (operator A/B, `RROUTER_CANARY`) is applied *last*, and its
//! precedence is deliberate, highest first:
//!
//! 1. the priority tier — a canary never promotes a lower tier above a higher one;
//! 2. the client's `provider.order` — an explicit request preference still wins;
//! 3. the canary promotion, inside each equal-priority run;
//! 4. the strategy's ordering and the account headroom hint.
//!
//! It only reorders candidates the plan already has: the allowlist, the [`ProviderPolicy`]
//! and team scope have already removed ineligible providers, so a canary cannot resurrect
//! one, and a provider with no eligible connection for the model cannot be invented. The
//! decision is `hash(model, provider, seed) % 100 < percent`, where the seed is the
//! gateway's request id (or the conversation's affinity), so it is stable within a request
//! and repeatable for a fixed seed.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use rrouter_core::{Dialect, Error, Result};
use rrouter_store::{AccountQuotaState, Connection, ConnectionScope, Store};
use uuid::Uuid;

use crate::candidate::{Candidate, CapacityHints, Plan};
use crate::capabilities::capabilities;
use crate::catalog::Catalog;
use crate::policy::{candidate_allowed, DataCollection, ProviderPolicy};

/// A connection's dialect, from the provider's registry entry.
///
/// The auth registry is the single source: the core transport table plus every reviewed
/// community manifest `build.rs` compiles into the binary. A provider the registry does
/// not know is an operator's own OpenAI-compatible endpoint, which speaks the Chat
/// dialect — the same default the registry would give it.
fn provider_dialect(provider: &str) -> Dialect {
    rrouter_auth::registry::get(provider).map_or(Dialect::OpenAiChat, |spec| spec.default_dialect)
}

/// `true` when `head` is a provider the binary can route to, i.e. the prefix of a
/// `provider/model` ref rather than a slash inside a model id
/// (`meta-llama/Llama-3.3-70B`, `accounts/fireworks/models/<name>`).
///
/// Read from the auth registry so the split and the transport table cannot disagree: a
/// provider compiled from a reviewed manifest is routable by its prefix on the same build
/// that compiled it, instead of needing a second hand-copied id list here. `rrouter-store`
/// already links `rrouter-auth`, so this states the dependency rather than adding a layer.
fn is_registered_provider(id: &str) -> bool {
    rrouter_auth::registry::get(id).is_some()
}

/// Does the client's spelling name one provider explicitly? A preference keyed by a
/// bare model id must not reorder an explicit `provider/model` request.
fn explicit_provider(model: &str) -> bool {
    model
        .split_once('/')
        .is_some_and(|(provider, _)| is_registered_provider(provider))
}

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

/// One A/B canary mapping: requests for `model` land on `provider` in `percent` of cases.
///
/// The rule never *adds* a candidate. It only reorders the candidates a request already
/// has, inside each equal-priority group, so priority, the key's allowlist, the request's
/// [`ProviderPolicy`], team scope and every circuit breaker still decide what can be sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanaryRule {
    /// The model ref the rule keys on, matched exactly against the requested model
    /// (after per-key mapping): either a bare id (`gpt-4.1`) or `provider/model`.
    pub model: String,
    /// Provider whose candidates are promoted when the request falls in the bucket.
    pub provider: String,
    /// Share of requests promoted, `0..=100`. `0` disables, `100` promotes every match.
    pub percent: u8,
}

impl CanaryRule {
    /// The wire spelling `model=provider:percent` (as `RROUTER_CANARY` expects).
    pub fn as_wire(&self) -> String {
        format!("{}={}:{}", self.model, self.provider, self.percent)
    }
}

impl std::fmt::Display for CanaryRule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.as_wire())
    }
}

/// Deterministic 64-bit bucket seed (FNV-1a) for a request id or conversation key.
///
/// The gateway hashes the request id when the request has no conversation affinity, so a
/// request is stable within itself and a conversation keeps one side of an A/B.
pub fn canary_seed(bytes: &[u8]) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

/// Deterministic canary decision for one request.
///
/// The model and provider are part of the hash so two rules with the same percentage do
/// not split the same requests, and the seed makes the answer stable for a conversation.
fn canary_selected(rule: &CanaryRule, seed: u64) -> bool {
    if rule.percent == 0 {
        return false;
    }
    let mut material = Vec::with_capacity(rule.model.len() + rule.provider.len() + 9);
    material.extend_from_slice(rule.model.as_bytes());
    material.push(0);
    material.extend_from_slice(rule.provider.as_bytes());
    material.push(0);
    material.extend_from_slice(&seed.to_le_bytes());
    (canary_seed(&material) % 100) < u64::from(rule.percent)
}

/// Promote `provider`'s candidates to the front of each equal-priority run.
///
/// Runs, not a global sort: a canary may reorder the accounts an operator put in one
/// tier, never promote a lower tier above a higher one. The sort is stable, so every
/// other candidate keeps the order the strategy produced.
fn canary_first(candidates: &mut [Candidate], provider: &str) {
    let mut start = 0;
    while start < candidates.len() {
        let priority = candidates[start].priority;
        let mut end = start;
        while end < candidates.len() && candidates[end].priority == priority {
            end += 1;
        }
        let group = &mut candidates[start..end];
        group.sort_by_key(|candidate| candidate.provider != provider);
        start = end;
    }
}

/// One plan call: the model the client asked for, plus the request-scoped inputs that
/// decide which expanded providers and accounts are admissible.
///
/// A struct rather than eight positional arguments on [`Planner::plan`], so a later
/// request-scoped knob (headroom ordering, a hedge budget) is one field instead of a
/// call-site audit.
#[derive(Debug, Clone, Copy)]
pub struct PlanRequest<'a> {
    /// The model string the client asked for, after this key's model mapping.
    pub model: &'a str,
    pub stream: bool,
    /// Stable per-conversation offset for account selection; `None` advances the cursor.
    pub affinity: Option<u64>,
    pub strategy: Strategy,
    /// Request-scoped provider preferences (OpenRouter `provider` object or
    /// `x-rrouter-*` headers). `None` = the operator's configuration decides.
    pub policy: Option<&'a ProviderPolicy>,
    /// The paying key's `allowed_models`; `None` = every model. Evaluated on the expanded
    /// `(provider, model)` pairs, never on the client's spelling alone.
    pub allowed_models: Option<&'a [String]>,
    /// The paying key's team. `None` = an instance-wide key (and every admin probe), which
    /// sees every account. `Some(team)` restricts candidates to that team's own accounts:
    /// the shared rows (`connections.team_id IS NULL`) belong to the instance and are not
    /// eligible for a team key.
    pub team_id: Option<Uuid>,
    /// Operator A/B rule; `None` = no canary. Applied after every eligibility filter and
    /// the strategy's ordering, and only inside each equal-priority group.
    pub canary: Option<&'a CanaryRule>,
    /// Stable seed for the canary decision (see [`canary_seed`]). Unused when `canary`
    /// is `None`, and equal seeds always take the same side.
    pub canary_seed: u64,
    /// Where a combo's model list starts for this request. `None` keeps the stored order;
    /// round-robin and smart strategies pass the chosen index. Applied only after a combo
    /// resolves, before its entries are expanded, so allowlist/policy/team filters still
    /// see every expanded candidate.
    pub combo_start: Option<usize>,
}

impl<'a> PlanRequest<'a> {
    /// A plain request with the operator's configuration and no request-scoped inputs.
    pub fn new(model: &'a str) -> Self {
        Self {
            model,
            stream: false,
            affinity: None,
            strategy: Strategy::default(),
            policy: None,
            allowed_models: None,
            team_id: None,
            canary: None,
            canary_seed: 0,
            combo_start: None,
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

    /// Build the ordered candidate list for `req.model`.
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
        req: PlanRequest<'_>,
    ) -> Result<Plan> {
        self.plan_with_capacity(catalog, store, req, None, 0.0)
            .await
    }

    /// [`Self::plan`] with subscription headroom applied.
    ///
    /// `reserve_percent` is a hard lower bound on health: an account whose most-consumed
    /// plan window is at or above it is skipped **while at least one healthier candidate
    /// remains**. If every candidate is over the reserve the plan keeps all of them and
    /// orders the healthiest first, because refusing a request the provider would still
    /// serve is worse than spending a nearly-full window. `0` disables the filter.
    ///
    /// Hints also feed the existing intra-priority headroom order together with operator
    /// quota utilization, so priority, strategy, canary, provider policy, team isolation
    /// and allowlists keep their existing precedence.
    pub async fn plan_with_capacity(
        &self,
        catalog: &Catalog,
        store: &Store,
        req: PlanRequest<'_>,
        capacity: Option<&CapacityHints>,
        reserve_percent: f64,
    ) -> Result<Plan> {
        let model = req.model;
        let stream = req.stream;
        let affinity = req.affinity;
        let strategy = req.strategy;
        let team = req.team_id;
        let connections = store
            .list_connections_scoped(ConnectionScope::for_team(team), None)
            .await?;
        let aliases = store.list_aliases().await?;
        let disabled: Vec<String> = store.disabled_models().await?;

        // A team key is routed only to its own accounts, so a team with none can never be
        // served — say so by name instead of the generic "no connection declares this
        // model", which would send an operator looking at the catalog rather than the team.
        if let Some(team) = team {
            if connections.is_empty() {
                return Err(Error::NoRoute {
                    model: model.to_string(),
                    detail: format!(
                        "team {team} owns no provider accounts; a team-scoped key is routed \
                         only to that team's connections and shared instance-wide accounts \
                         are not eligible"
                    ),
                });
            }
        }

        let targets = self
            .targets(catalog, store, model, &aliases, req.combo_start)
            .await?;
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

        // Request policy and the caller's allowlist are hard filters, and both are judged
        // on the expanded pairs: by now every `(provider, model)` a request could reach is
        // known, so a provider-scoped entry cannot leak the request to a provider that
        // merely serves the same id.
        let mut excluded: Vec<String> = Vec::new();
        let mut admitted: Vec<(&str, &str)> = Vec::with_capacity(seen_pairs.len());
        for (provider, model_id) in seen_pairs {
            if let Some(reason) = policy_reason(store, req.policy, provider, model_id).await {
                excluded.push(reason);
                continue;
            }
            if let Some(allowed) = req.allowed_models {
                if !candidate_allowed(allowed, provider, model_id) {
                    excluded.push(format!(
                        "{provider}/{model_id} is outside the key's allowed_models"
                    ));
                    continue;
                }
            }
            admitted.push((provider, model_id));
        }
        if admitted.is_empty() {
            // A key allowlist refusal is a governance 403, exactly as the pre-expansion
            // check reports it — the message names the model the client asked for.
            if req.allowed_models.is_some() {
                return Err(Error::Forbidden {
                    code: "model_not_allowed",
                    message: format!(
                        "model {model} is not allowed by this key's allowed_models after \
                         provider expansion ({})",
                        summarize(&excluded)
                    ),
                });
            }
            return Err(Error::NoRoute {
                model: model.to_string(),
                detail: format!(
                    "request policy excluded every provider ({})",
                    summarize(&excluded)
                ),
            });
        }
        let seen_pairs = admitted;

        // Only `least_latency` needs the extra read; the other strategies must not pay
        // a query per plan for a table they never consult.
        let latencies = match strategy {
            Strategy::LeastLatency => store.connection_latencies().await?,
            _ => HashMap::new(),
        };
        let mut candidates = Vec::with_capacity(active.len());
        let offset = affinity.unwrap_or_else(|| self.next_offset());
        let target_count = seen_pairs.len();
        for &(provider, model_id) in &seen_pairs {
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
            // A request excluded by policy would otherwise be reported as "no active
            // connection", which sends an operator looking at the wrong table. A team key
            // additionally names its team and the providers the model resolved to: the
            // accounts exist somewhere in the instance, just not in this tenant.
            let policy_note = if excluded.is_empty() {
                String::new()
            } else {
                format!("; excluded by request policy: {}", summarize(&excluded))
            };
            let detail = if let Some(team) = team {
                let resolved: Vec<String> = seen_pairs
                    .iter()
                    .map(|(provider, model_id)| format!("{provider}/{model_id}"))
                    .collect();
                format!(
                    "team {team} has no eligible connection for {model}: resolved to {} \
                     (a team-scoped key is routed only to that team's accounts, never to \
                     shared instance-wide ones){policy_note}",
                    summarize(&resolved)
                )
            } else if excluded.is_empty() {
                format!(
                    "resolved to {target_count} provider/model target(s) but none has an active connection",
                )
            } else {
                format!(
                    "resolved to {target_count} provider/model target(s), none with an active \
                     connection; excluded by request policy: {}",
                    summarize(&excluded)
                )
            };
            return Err(Error::NoRoute {
                model: model.to_string(),
                detail,
            });
        }

        // Account quotas decide two things: an exhausted account is not a candidate at
        // all, and among candidates that share a priority the account with the most
        // headroom is tried first. Both are read once, from the same aggregate.
        //
        // With no quota rows the map is empty, neither rule can fire, and the plan is
        // exactly the order the strategy produced — the property the no-quota test pins.
        let quotas = store.account_quota_states().await?;
        let mut utilization: HashMap<Uuid, f64> = HashMap::new();
        if !quotas.is_empty() {
            let now = now_unix();
            let mut exhausted: Vec<Uuid> = Vec::new();
            let mut wait: Option<u64> = None;
            for state in &quotas {
                utilization.insert(state.quota.connection_id, quota_utilization(state));
                if !state.exhausted {
                    continue;
                }
                exhausted.push(state.quota.connection_id);
                // An all-time quota never resets, so the wait is an operator-action hint.
                let reset = state
                    .resets_at
                    .map(|reset| (reset - now).max(1) as u64)
                    .unwrap_or(60);
                wait = Some(wait.map_or(reset, |seen| seen.min(reset)));
            }
            if !exhausted.is_empty() {
                let planned = candidates.len();
                candidates.retain(|c| !exhausted.contains(&c.connection));
                if candidates.is_empty() && planned > 0 {
                    let wait = wait.unwrap_or(60);
                    return Err(Error::Limited {
                        code: "quota_exhausted",
                        message: format!(
                            "every account for {model} is over its quota; the first resets in {wait}s",
                        ),
                        retry_after_secs: wait,
                    });
                }
            }
        }
        // Subscription windows are a second health input. Filter only when the setting is
        // on and at least one candidate is below the reserve; an all-over plan stays
        // serviceable and is ordered by the same intra-priority headroom pass.
        if let Some(hints) = capacity.filter(|hints| !hints.is_empty()) {
            let reserve = reserve_percent.clamp(0.0, 100.0);
            // `0` means off, not "reserve nothing": no filter and no reordering, so an
            // operator who disables the feature gets exactly the pre-capacity plan.
            if reserve > 0.0 {
                let healthy = candidates
                    .iter()
                    .filter(|candidate| {
                        hints
                            .used_percent(candidate.connection)
                            .is_none_or(|used| used < reserve)
                    })
                    .count();
                if healthy > 0 && healthy < candidates.len() {
                    candidates.retain(|candidate| {
                        hints
                            .used_percent(candidate.connection)
                            .is_none_or(|used| used < reserve)
                    });
                }
                for (connection, hint) in hints.iter() {
                    let slot = utilization.entry(connection).or_insert(0.0);
                    *slot = slot.max(hint.used_percent / 100.0);
                }
            }
        }
        if !utilization.is_empty() {
            headroom_order(&mut candidates, &utilization);
        }

        // Operator A/B: promote the canary provider inside each equal-priority run, after
        // the strategy and headroom ordering and before the client's own `provider.order`
        // (an explicit request preference still wins). This never adds a candidate: a
        // provider the plan does not already contain cannot be selected.
        if let Some(rule) = req.canary {
            if rule.model == model && canary_selected(rule, req.canary_seed) {
                canary_first(&mut candidates, &rule.provider);
            }
        }

        // Operator per-model preference. It is deliberately stronger than priority,
        // strategy, headroom and canary: the operator said this provider should serve
        // this bare id. Explicit `provider/model` requests skip it, and a later
        // request-scoped `provider.order` still wins because that is the client's own
        // explicit choice for this request. A preference for a provider with no
        // surviving candidate is a no-op, so disconnected/disabled providers fall back.
        if !explicit_provider(model) {
            if let Some(preferred) = store.model_preference(model).await? {
                candidates.sort_by_key(|candidate| candidate.provider != preferred.provider);
            }
        }

        // Request ordering is an intra-priority hint. A stable sort keeps the strategy's
        // order inside one provider's accounts and inside every unlisted provider, so a
        // `weighted` group still alternates while the named providers come first.
        if let Some(policy) = req.policy {
            if !policy.order.is_empty() {
                candidates.sort_by_key(|c| (c.priority, policy.order_position(&c.provider)));
            }
            if policy.allow_fallbacks == Some(false) {
                truncate_fallbacks(policy, &mut candidates);
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
        combo_start: Option<usize>,
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
            let mut models = combo.models.clone();
            if !models.is_empty() {
                let start = combo_start.unwrap_or(0) % models.len();
                models.rotate_left(start);
            }
            for entry in &models {
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
            if !rest.is_empty() && is_registered_provider(head) {
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

/// Why `provider/model` cannot serve a request carrying `policy`, or `None` when it can.
///
/// One function for every policy dimension so the planner and its reasons cannot drift:
/// the string returned here is the whole explanation a `NoRoute` carries.
async fn policy_reason(
    store: &Store,
    policy: Option<&ProviderPolicy>,
    provider: &str,
    model: &str,
) -> Option<String> {
    let policy = policy?;
    let caps = capabilities(provider);
    if policy.ignore.iter().any(|p| p == provider) {
        return Some(format!("{provider} is in the request's ignore list"));
    }
    if !policy.only.is_empty() && !policy.only.iter().any(|p| p == provider) {
        return Some(format!("{provider} is not in the request's only list"));
    }
    if policy.data_collection == Some(DataCollection::Deny) && !caps.data_collection_deny {
        return Some(format!(
            "{provider} cannot guarantee a data-collection-deny policy"
        ));
    }
    if policy.zdr && !caps.zdr {
        return Some(format!("{provider} does not offer zero data retention"));
    }
    if !policy.quantizations.is_empty()
        && !policy
            .quantizations
            .iter()
            .any(|q| caps.quantizations.contains(&q.as_str()))
    {
        return Some(format!(
            "{provider} publishes none of the requested quantizations"
        ));
    }
    if policy.require_parameters && !caps.sampling_params {
        return Some(format!("{provider} does not honour sampling parameters"));
    }
    if let Some(max) = policy.max_price_per_million {
        // A candidate with no published price cannot prove it is under the ceiling, so
        // routing must not guess on the client's behalf.
        return match target_price(store, provider, model).await {
            Some(price) if price <= max => None,
            Some(price) => Some(format!(
                "{provider}/{model} costs {price:.4} per million, above the request's {max:.4}"
            )),
            None => Some(format!(
                "{provider}/{model} has no published price, so a {max:.4} ceiling cannot be proven"
            )),
        };
    }
    None
}

/// How much of an account's quota is already spent, in `0.0..=1.0` for a live account and
/// above it for an exhausted one. The worst of the three limits decides, because hitting
/// any one of them removes the account; a limit of `0` is unlimited and contributes `0.0`.
fn quota_utilization(state: &AccountQuotaState) -> f64 {
    let ratio = |used: u64, limit: u64| {
        if limit == 0 {
            0.0
        } else {
            used as f64 / limit as f64
        }
    };
    ratio(state.usage.requests, state.quota.max_requests)
        .max(ratio(state.usage.tokens, state.quota.max_tokens))
        .max(ratio(state.usage.cost_micros, state.quota.max_cost_micros))
}

/// Within each contiguous run of equal priority, order accounts by ascending utilization.
///
/// Runs, not a global sort: cross-provider order is the catalog's, and sorting the whole
/// plan by priority would silently rewrite it. A stable sort with every utilization at
/// `0.0` (no quota rows, or accounts with only unlimited limits) is a no-op, so the
/// no-quota order is preserved exactly.
fn headroom_order(candidates: &mut [Candidate], headroom: &HashMap<Uuid, f64>) {
    let utilization = |c: &Candidate| headroom.get(&c.connection).copied().unwrap_or(0.0);
    let mut start = 0;
    while start < candidates.len() {
        let priority = candidates[start].priority;
        let mut end = start + 1;
        while end < candidates.len() && candidates[end].priority == priority {
            end += 1;
        }
        candidates[start..end].sort_by(|a, b| utilization(a).total_cmp(&utilization(b)));
        start = end;
    }
}

/// Wall-clock seconds, for the quota reset hint. The planner otherwise reads no clock.
fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs() as i64)
        .unwrap_or(0)
}

/// Drop every candidate outside the first priority group, and outside `order[0]` when the
/// request named it there.
///
/// `allow_fallbacks: false` is a client saying "do not silently answer with a different
/// provider": within the chosen tier the accounts are still interchangeable (that is what
/// a tier is), but a lower tier must not be reached.
fn truncate_fallbacks(policy: &ProviderPolicy, candidates: &mut Vec<Candidate>) {
    let Some(first) = candidates.first().map(|c| c.priority) else {
        return;
    };
    candidates.retain(|c| c.priority == first);
    if let Some(preferred) = policy.order.first() {
        if candidates.iter().any(|c| &c.provider == preferred) {
            candidates.retain(|c| &c.provider == preferred);
        }
    }
}

/// Join exclusion reasons for an error detail, bounded so one wide model cannot produce a
/// kilobyte of body.
fn summarize(reasons: &[String]) -> String {
    const MAX: usize = 4;
    let shown: Vec<&str> = reasons.iter().take(MAX).map(String::as_str).collect();
    if reasons.len() > MAX {
        format!("{} and {} more", shown.join("; "), reasons.len() - MAX)
    } else if shown.is_empty() {
        "no candidate was declared".to_string()
    } else {
        shown.join("; ")
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

#[cfg(test)]
mod tests {
    use super::*;
    use rrouter_store::Credentials;

    /// One store with one active connection per entry: `(provider, priority, weight)`.
    async fn store_with(accounts: &[(&str, i32, u32)]) -> Store {
        let store = Store::open_in_memory().expect("store");
        for (provider, priority, weight) in accounts {
            let conn = store
                .create_connection(
                    provider,
                    "api_key",
                    Credentials {
                        api_key: Some("test-key".to_string()),
                        ..Credentials::default()
                    },
                    None,
                )
                .await
                .expect("connection");
            store
                .set_connection_priority(conn.id, *priority)
                .await
                .expect("priority");
            store
                .set_connection_weight(conn.id, *weight)
                .await
                .expect("weight");
        }
        store
    }

    fn catalog() -> Catalog {
        Catalog::builtin()
    }

    /// The provider of every candidate, in order — what a test actually asserts on.
    fn providers(plan: &Plan) -> Vec<&str> {
        plan.candidates
            .iter()
            .map(|c| c.provider.as_str())
            .collect()
    }

    /// One quota row, plus `requests` recorded usage rows on that connection.
    async fn exhaust(store: &Store, connection: Uuid, max_requests: u64, requests: usize) {
        store
            .set_account_quota(&rrouter_store::AccountQuota {
                connection_id: connection,
                window: rrouter_store::BudgetWindow::Total,
                max_requests,
                max_tokens: 0,
                max_cost_micros: 0,
            })
            .await
            .expect("quota");
        let ts = now_unix();
        for _ in 0..requests {
            store
                .record_usage(&rrouter_store::UsageRecord {
                    id: None,
                    ts,
                    request_id: Some(Uuid::new_v4()),
                    trace_id: None,
                    provider: "claude".to_string(),
                    model: "claude-sonnet-4-5".to_string(),
                    connection_id: Some(connection),
                    endpoint: None,
                    key_id: None,
                    cost_micros: 0,
                    input_tokens: 1,
                    output_tokens: 0,
                    cache_read_tokens: 0,
                    cache_write_tokens: 0,
                    latency_ms: 1,
                    ttft_ms: 0,
                    upstream_ms: 0,
                    status: 200,
                    streamed: false,
                    error: None,
                })
                .await
                .expect("usage");
        }
    }

    /// Between accounts of the same priority, the one with more quota left is tried first:
    /// a 90%-spent account must yield to a fresh one instead of serving nine of ten.
    #[tokio::test]
    async fn headroom_order_prefers_the_less_consumed_account() {
        let store = store_with(&[("claude", 0, 100), ("claude", 0, 100)]).await;
        let conns = store.list_connections(None).await.expect("connections");
        let (busy, idle) = (conns[0].id, conns[1].id);
        exhaust(&store, busy, 10, 9).await;
        exhaust(&store, idle, 10, 0).await;

        let plan = Planner::new()
            .plan(
                &catalog(),
                &store,
                PlanRequest::new("claude/claude-sonnet-4-5"),
            )
            .await
            .expect("plan");
        assert_eq!(plan.candidates.len(), 2);
        assert_eq!(plan.candidates[0].connection, idle, "90% spent yields");
        assert_eq!(plan.candidates[1].connection, busy);
    }

    fn hint(used: f64) -> crate::candidate::CapacityHint {
        crate::candidate::CapacityHint {
            used_percent: used,
            resets_at: None,
        }
    }

    async fn capacity_plan(store: &Store, hints: &CapacityHints, reserve: f64) -> Plan {
        Planner::new()
            .plan_with_capacity(
                &catalog(),
                store,
                PlanRequest::new("claude/claude-sonnet-4-5"),
                Some(hints),
                reserve,
            )
            .await
            .expect("plan")
    }

    /// An account at/above the reserve is skipped while a healthier one remains, even
    /// across priority tiers: preserving a subscription window is the point of the knob.
    #[tokio::test]
    async fn subscription_reserve_filters_a_nearly_full_account() {
        let store = store_with(&[("claude", 0, 100), ("claude", 1, 100)]).await;
        let conns = store.list_connections(None).await.expect("connections");
        let busy = conns[0].id;
        let healthy = conns[1].id;
        let mut hints = CapacityHints::new();
        hints.insert(busy, hint(95.0));
        hints.insert(healthy, hint(20.0));

        let plan = capacity_plan(&store, &hints, 90.0).await;
        assert_eq!(plan.candidates.len(), 1);
        assert_eq!(plan.candidates[0].connection, healthy);
        assert_eq!(
            plan.candidates[0].priority, 1,
            "the healthy lower-priority account wins"
        );
    }

    /// If every account is over the reserve the request still runs; the healthiest is
    /// first. A hint is a filter, never a reason to refuse service by itself.
    #[tokio::test]
    async fn all_accounts_over_reserve_stay_routable_healthiest_first() {
        let store = store_with(&[("claude", 0, 100), ("claude", 0, 100)]).await;
        let conns = store.list_connections(None).await.expect("connections");
        let mut hints = CapacityHints::new();
        hints.insert(conns[0].id, hint(99.0));
        hints.insert(conns[1].id, hint(95.0));

        let plan = capacity_plan(&store, &hints, 90.0).await;
        assert_eq!(plan.candidates.len(), 2, "all-over is still serviceable");
        assert_eq!(plan.candidates[0].connection, conns[1].id);
        assert_eq!(plan.candidates[1].connection, conns[0].id);
    }

    /// `0` means off: no filter and no reordering, exactly the pre-capacity plan.
    #[tokio::test]
    async fn a_zero_reserve_disables_the_filter_and_ordering() {
        let store = store_with(&[("claude", 0, 100), ("claude", 0, 100)]).await;
        let conns = store.list_connections(None).await.expect("connections");
        let mut hints = CapacityHints::new();
        hints.insert(conns[0].id, hint(99.0));
        hints.insert(conns[1].id, hint(10.0));

        let plan = capacity_plan(&store, &hints, 0.0).await;
        assert_eq!(plan.candidates.len(), 2);
        assert_eq!(plan.candidates[0].connection, conns[0].id);
        assert_eq!(plan.candidates[1].connection, conns[1].id);
    }

    /// No hints must be byte-for-byte the old plan: capacity is opt-in data, not a
    /// silent reordering of every installation.
    #[tokio::test]
    async fn no_capacity_hints_keeps_the_original_plan() {
        let store = store_with(&[("claude", 0, 100), ("claude", 0, 100)]).await;
        let request = || PlanRequest::new("claude/claude-sonnet-4-5");
        // A fresh planner on each call so the round-robin cursor cannot differ.
        let plain = Planner::new()
            .plan(&catalog(), &store, request())
            .await
            .expect("plain");
        let with_empty = Planner::new()
            .plan_with_capacity(
                &catalog(),
                &store,
                request(),
                Some(&CapacityHints::new()),
                90.0,
            )
            .await
            .expect("empty hints");
        assert_eq!(plain, with_empty);
    }

    /// Without quota rows the headroom pass must be invisible: the plan keeps the exact
    /// order the strategy produced, including the interleaving across providers that a
    /// global priority sort would rewrite.
    #[tokio::test]
    async fn no_quota_rows_keeps_existing_order() {
        let store = store_with(&[("openai", 0, 100), ("openai", 1, 100), ("github", 0, 100)]).await;
        let plan = Planner::new()
            .plan(&catalog(), &store, PlanRequest::new("gpt-4.1"))
            .await
            .expect("plan");
        let order: Vec<(String, i32)> = plan
            .candidates
            .iter()
            .map(|c| (c.provider.clone(), c.priority))
            .collect();
        assert_eq!(
            order,
            vec![
                ("openai".to_string(), 0),
                ("openai".to_string(), 1),
                ("github".to_string(), 0),
            ],
            "catalog order, priority within a provider: no global re-sort"
        );
    }

    /// Every account spent still fails before a provider call, with the reset hint.
    #[tokio::test]
    async fn all_exhausted_still_returns_quota_exhausted_429() {
        let store = store_with(&[("claude", 0, 100), ("claude", 0, 100)]).await;
        for conn in store.list_connections(None).await.expect("connections") {
            exhaust(&store, conn.id, 1, 1).await;
        }
        let err = Planner::new()
            .plan(
                &catalog(),
                &store,
                PlanRequest::new("claude/claude-sonnet-4-5"),
            )
            .await
            .expect_err("every account is spent");
        match err {
            Error::Limited {
                code,
                retry_after_secs,
                ..
            } => {
                assert_eq!(code, "quota_exhausted");
                assert!(retry_after_secs >= 1, "the wait is a hint, not zero");
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[tokio::test]
    async fn plain_request_keeps_the_legacy_plan() {
        let store = store_with(&[("claude", 0, 100), ("claude", 0, 100), ("claude", 0, 100)]).await;
        let plan = Planner::new()
            .plan(
                &catalog(),
                &store,
                PlanRequest::new("claude/claude-sonnet-4-5"),
            )
            .await
            .expect("plan");
        assert_eq!(plan.candidates.len(), 3);
        assert!(plan.candidates.iter().all(|c| c.priority == 0));
        assert_eq!(plan.model, "claude/claude-sonnet-4-5");
        assert!(!plan.stream);
    }

    #[tokio::test]
    async fn priority_dominates_the_strategy() {
        // The lower-priority account is the heavy one: weight must not promote it.
        let store = store_with(&[("claude", 1, 1000), ("claude", 0, 1)]).await;
        let mut req = PlanRequest::new("claude/claude-sonnet-4-5");
        req.strategy = Strategy::Weighted;
        let plan = Planner::new()
            .plan(&catalog(), &store, req)
            .await
            .expect("plan");
        assert_eq!(plan.candidates.len(), 2);
        assert_eq!(plan.candidates[0].priority, 0);
        assert_eq!(plan.candidates[1].priority, 1);
    }

    #[tokio::test]
    async fn weighted_puts_the_heavier_account_first_three_times_in_four() {
        let store = store_with(&[("claude", 0, 300), ("claude", 0, 100)]).await;
        let heavy = store
            .list_connections(None)
            .await
            .expect("connections")
            .into_iter()
            .find(|c| c.weight == 300)
            .expect("heavy account")
            .id;
        let planner = Planner::new();
        let mut heavy_first = 0;
        for _ in 0..8 {
            let mut req = PlanRequest::new("claude/claude-sonnet-4-5");
            req.strategy = Strategy::Weighted;
            let plan = planner.plan(&catalog(), &store, req).await.expect("plan");
            assert_eq!(plan.candidates.len(), 2);
            if plan.candidates[0].connection == heavy {
                heavy_first += 1;
            }
        }
        // Smooth WRR over a 3:1 weight, sampled across one full cycle twice.
        assert_eq!(heavy_first, 6);
    }

    #[tokio::test]
    async fn least_latency_prefers_the_faster_account() {
        let store = store_with(&[("claude", 0, 100), ("claude", 0, 100)]).await;
        let conns = store.list_connections(None).await.expect("connections");
        store
            .record_connection_latency(conns[0].id, 900)
            .await
            .expect("latency");
        store
            .record_connection_latency(conns[1].id, 40)
            .await
            .expect("latency");
        let mut req = PlanRequest::new("claude/claude-sonnet-4-5");
        req.strategy = Strategy::LeastLatency;
        let plan = Planner::new()
            .plan(&catalog(), &store, req)
            .await
            .expect("plan");
        assert_eq!(plan.candidates[0].connection, conns[1].id);
        assert_eq!(plan.candidates[1].connection, conns[0].id);
    }

    #[tokio::test]
    async fn request_order_reorders_a_priority_group() {
        let store = store_with(&[("github", 0, 100), ("openai", 0, 100)]).await;
        let planner = Planner::new();
        // Catalog order follows the builtin table: openai is declared before github.
        let baseline = planner
            .plan(&catalog(), &store, PlanRequest::new("gpt-4.1"))
            .await
            .expect("plan");
        assert_eq!(providers(&baseline), ["openai", "github"]);

        let policy = ProviderPolicy {
            order: vec!["github".to_string()],
            ..ProviderPolicy::default()
        };
        let mut req = PlanRequest::new("gpt-4.1");
        req.policy = Some(&policy);
        let plan = planner.plan(&catalog(), &store, req).await.expect("plan");
        assert_eq!(providers(&plan), ["github", "openai"]);
    }

    /// The audit's leak: a provider-scoped allowlist entry must not reach another
    /// provider that happens to serve the same bare model id.
    #[tokio::test]
    async fn allowlist_is_evaluated_on_expanded_providers() {
        let store = store_with(&[("openai", 0, 100), ("github", 0, 100)]).await;
        let allowed = vec!["openai/gpt-4.1".to_string()];
        let mut req = PlanRequest::new("gpt-4.1");
        req.allowed_models = Some(&allowed);
        let plan = Planner::new()
            .plan(&catalog(), &store, req)
            .await
            .expect("plan");
        assert_eq!(providers(&plan), ["openai"]);
    }

    #[tokio::test]
    async fn combo_cannot_escape_the_key_allowlist() {
        let store = store_with(&[("openai", 0, 100)]).await;
        store
            .upsert_combo("claude/cheap", &["openai/gpt-4.1".to_string()])
            .await
            .expect("combo");
        // The glob matches the *combo name*, so only the expanded candidates can tell
        // that the contents belong to another provider.
        let allowed = vec!["claude/*".to_string()];
        let mut req = PlanRequest::new("claude/cheap");
        req.allowed_models = Some(&allowed);
        let err = Planner::new()
            .plan(&catalog(), &store, req)
            .await
            .expect_err("combo must be refused");
        assert!(
            matches!(
                err,
                Error::Forbidden {
                    code: "model_not_allowed",
                    ..
                }
            ),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn bare_allowlist_entry_covers_the_model_on_any_provider() {
        let store = store_with(&[("openai", 0, 100), ("github", 0, 100)]).await;
        let allowed = vec!["gpt-4.1".to_string()];
        let mut req = PlanRequest::new("gpt-4.1");
        req.allowed_models = Some(&allowed);
        let plan = Planner::new()
            .plan(&catalog(), &store, req)
            .await
            .expect("plan");
        assert_eq!(providers(&plan), ["openai", "github"]);
    }

    #[tokio::test]
    async fn ignore_removes_every_candidate_of_a_provider() {
        let store = store_with(&[("openai", 0, 100), ("github", 0, 100)]).await;
        let policy = ProviderPolicy {
            ignore: vec!["github".to_string()],
            ..ProviderPolicy::default()
        };
        let mut req = PlanRequest::new("gpt-4.1");
        req.policy = Some(&policy);
        let plan = Planner::new()
            .plan(&catalog(), &store, req)
            .await
            .expect("plan");
        assert_eq!(providers(&plan), ["openai"]);
    }

    #[tokio::test]
    async fn policy_can_empty_the_plan_with_reasons() {
        let store = store_with(&[("openai", 0, 100)]).await;
        let policy = ProviderPolicy {
            only: vec!["anthropic".to_string()],
            ..ProviderPolicy::default()
        };
        let mut req = PlanRequest::new("gpt-4.1");
        req.policy = Some(&policy);
        let err = Planner::new()
            .plan(&catalog(), &store, req)
            .await
            .expect_err("no provider is allowed");
        match err {
            Error::NoRoute { detail, .. } => {
                assert!(detail.contains("only list"), "detail was {detail}");
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    /// Ignoring the only *connected* provider leaves a catalog target with no account:
    /// the failure must name the policy, not claim the accounts are missing.
    #[tokio::test]
    async fn policy_reasons_survive_the_no_account_failure() {
        let store = store_with(&[("openai", 0, 100)]).await;
        let policy = ProviderPolicy {
            ignore: vec!["openai".to_string()],
            ..ProviderPolicy::default()
        };
        let mut req = PlanRequest::new("gpt-4.1");
        req.policy = Some(&policy);
        let err = Planner::new()
            .plan(&catalog(), &store, req)
            .await
            .expect_err("the only connected provider is ignored");
        match err {
            Error::NoRoute { detail, .. } => {
                assert!(
                    detail.contains("excluded by request policy") && detail.contains("ignore list"),
                    "detail was {detail}"
                );
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[tokio::test]
    async fn zdr_excludes_providers_without_it() {
        let store = store_with(&[("openai", 0, 100), ("github", 0, 100)]).await;
        let policy = ProviderPolicy {
            zdr: true,
            ..ProviderPolicy::default()
        };
        let mut req = PlanRequest::new("gpt-4.1");
        req.policy = Some(&policy);
        let plan = Planner::new()
            .plan(&catalog(), &store, req)
            .await
            .expect("plan");
        assert_eq!(providers(&plan), ["openai"]);
    }

    #[tokio::test]
    async fn quantization_filter_keeps_the_self_hosted_account() {
        let store = store_with(&[("ollama", 0, 100)]).await;
        let policy = ProviderPolicy {
            quantizations: vec!["int4".to_string()],
            ..ProviderPolicy::default()
        };
        let mut req = PlanRequest::new("llama3.3");
        req.policy = Some(&policy);
        let plan = Planner::new()
            .plan(&catalog(), &store, req)
            .await
            .expect("plan");
        assert_eq!(providers(&plan), ["ollama"]);
    }

    /// Hosted APIs publish no precision, so a quantization request must not be answered
    /// by one of them.
    #[tokio::test]
    async fn quantization_filter_excludes_providers_that_publish_none() {
        let store = store_with(&[("openai", 0, 100), ("github", 0, 100)]).await;
        let policy = ProviderPolicy {
            quantizations: vec!["int4".to_string()],
            ..ProviderPolicy::default()
        };
        let mut req = PlanRequest::new("gpt-4.1");
        req.policy = Some(&policy);
        let err = Planner::new()
            .plan(&catalog(), &store, req)
            .await
            .expect_err("no hosted provider publishes int4");
        match err {
            Error::NoRoute { detail, .. } => {
                assert!(detail.contains("quantizations"), "detail was {detail}");
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[tokio::test]
    async fn allow_fallbacks_false_stays_in_the_first_priority_group() {
        let store = store_with(&[("openai", 0, 100), ("github", 1, 100)]).await;
        let planner = Planner::new();
        let baseline = planner
            .plan(&catalog(), &store, PlanRequest::new("gpt-4.1"))
            .await
            .expect("plan");
        assert_eq!(providers(&baseline), ["openai", "github"]);

        let policy = ProviderPolicy {
            allow_fallbacks: Some(false),
            ..ProviderPolicy::default()
        };
        let mut req = PlanRequest::new("gpt-4.1");
        req.policy = Some(&policy);
        let plan = planner.plan(&catalog(), &store, req).await.expect("plan");
        assert_eq!(providers(&plan), ["openai"]);
    }

    /// Every provider the binary compiles — the core transports and every reviewed
    /// community manifest — must split out of a `provider/model` ref. This is the guard
    /// that fails when `rrouter-auth`'s provider set grows: the split reads that same
    /// registry, so a provider is routable by its prefix on the build that compiles it,
    /// and a reintroduced hand-copied list shows up here immediately.
    #[test]
    fn every_registered_provider_splits_its_ref_explicitly() {
        let planner = Planner::new();
        for spec in rrouter_auth::registry::all() {
            let entry = format!("{}/some-model", spec.id);
            assert_eq!(
                planner.expand_entry(&catalog(), &entry),
                vec![(spec.id.to_string(), "some-model".to_string(), true)],
                "{}",
                spec.id
            );
        }
    }

    /// A ref naming a provider that ships only as a reviewed manifest resolves to exactly
    /// that provider, with the prefix stripped. Before the split came from the registry
    /// these widened to every active account and kept the whole ref as the model id.
    #[tokio::test]
    async fn manifest_provider_refs_resolve_to_exactly_that_provider() {
        let store = store_with(&[
            ("cohere", 0, 100),
            ("jina", 0, 100),
            ("azure", 0, 100),
            ("fireworks", 0, 100),
            ("openai", 0, 100),
            ("claude", 0, 100),
        ])
        .await;
        let planner = Planner::new();
        for (provider, model) in [
            ("cohere", "rerank-v3.5"),
            ("jina", "jina-reranker-v2-base-multilingual"),
            ("azure", "gpt-4o-deployment"),
            (
                "fireworks",
                "accounts/fireworks/models/llama-v3p3-70b-instruct",
            ),
        ] {
            let plan = planner
                .plan(
                    &catalog(),
                    &store,
                    PlanRequest::new(&format!("{provider}/{model}")),
                )
                .await
                .expect("plan");
            assert_eq!(providers(&plan), [provider], "{provider}");
            assert!(
                plan.candidates.iter().all(|c| c.model == model),
                "{provider} kept a prefixed model id"
            );
        }
    }

    /// A prefix the registry does not know is part of the model id, not a provider: the
    /// ref keeps the bare-id behaviour and widens to every active account, which is what
    /// a self-hosted id needs and what an unknown provider must not break.
    #[tokio::test]
    async fn unknown_provider_prefix_keeps_bare_id_fanout() {
        let store = store_with(&[("openrouter", 0, 100), ("ollama", 0, 100)]).await;
        let plan = Planner::new()
            .plan(
                &catalog(),
                &store,
                PlanRequest::new("notaprovider/some-model"),
            )
            .await
            .expect("plan");
        let mut got = providers(&plan);
        got.sort_unstable();
        assert_eq!(got, ["ollama", "openrouter"]);
        assert!(plan
            .candidates
            .iter()
            .all(|c| c.model == "notaprovider/some-model"));
    }

    /// The core ids keep resolving exactly as they did before the split moved to the
    /// registry: prefix stripped, model id untouched, one provider, no fan-out.
    #[tokio::test]
    async fn core_provider_refs_are_unchanged() {
        const CORE: &[&str] = &[
            "claude",
            "codex",
            "github",
            "openai",
            "anthropic",
            "opencode",
            "opencode-go",
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
        let accounts: Vec<(&str, i32, u32)> = CORE.iter().map(|p| (*p, 0, 100)).collect();
        let store = store_with(&accounts).await;
        let planner = Planner::new();
        for provider in CORE {
            let plan = planner
                .plan(
                    &catalog(),
                    &store,
                    PlanRequest::new(&format!("{provider}/some-model")),
                )
                .await
                .expect("plan");
            assert_eq!(providers(&plan), [*provider], "{provider}");
            assert_eq!(plan.candidates[0].model, "some-model", "{provider}");
        }
    }

    /// A team for the scoping tests below.
    async fn team(store: &Store, name: &str) -> Uuid {
        store
            .create_team(name, 0, 0, rrouter_store::BudgetWindow::Total)
            .await
            .expect("team")
            .id
    }

    /// One active account owned by `team` (`None` = shared/instance-wide).
    async fn owned(store: &Store, provider: &str, team: Option<Uuid>) -> rrouter_store::Connection {
        store
            .create_connection_owned(
                provider,
                "api_key",
                Credentials {
                    api_key: Some("test-key".to_string()),
                    ..Credentials::default()
                },
                None,
                team,
                None,
            )
            .await
            .expect("connection")
    }

    /// A team key is planned from its own accounts only; the global (instance-wide) plan
    /// still sees every account, which is the pre-scoping behaviour.
    #[tokio::test]
    async fn team_scope_plans_only_its_own_accounts() {
        let store = Store::open_in_memory().expect("store");
        let alpha = team(&store, "alpha").await;
        let beta = team(&store, "beta").await;
        let alpha_conn = owned(&store, "openai", Some(alpha)).await;
        let beta_conn = owned(&store, "openai", Some(beta)).await;
        let shared = owned(&store, "openai", None).await;
        let planner = Planner::new();

        for (scope, expected) in [(alpha, alpha_conn.id), (beta, beta_conn.id)] {
            let mut req = PlanRequest::new("openai/gpt-4.1");
            req.team_id = Some(scope);
            let plan = planner.plan(&catalog(), &store, req).await.expect("plan");
            assert_eq!(plan.candidates.len(), 1, "team {scope}");
            assert_eq!(plan.candidates[0].connection, expected, "team {scope}");
        }

        let plan = planner
            .plan(&catalog(), &store, PlanRequest::new("openai/gpt-4.1"))
            .await
            .expect("plan");
        let mut ids: Vec<Uuid> = plan.candidates.iter().map(|c| c.connection).collect();
        ids.sort();
        let mut expected = vec![alpha_conn.id, beta_conn.id, shared.id];
        expected.sort();
        assert_eq!(ids, expected, "global keeps every account");
    }

    /// Shared (`team_id IS NULL`) accounts belong to the instance, so a team key with no
    /// accounts of its own is refused by name instead of borrowing one.
    #[tokio::test]
    async fn team_scope_never_borrows_a_shared_account() {
        let store = Store::open_in_memory().expect("store");
        let alpha = team(&store, "alpha").await;
        let _shared = owned(&store, "openai", None).await;
        let mut req = PlanRequest::new("openai/gpt-4.1");
        req.team_id = Some(alpha);
        let err = Planner::new()
            .plan(&catalog(), &store, req)
            .await
            .expect_err("a team with no account cannot be served");
        match err {
            Error::NoRoute { detail, .. } => {
                assert!(detail.contains(&alpha.to_string()), "detail was {detail}");
                assert!(detail.contains("team"), "detail was {detail}");
                assert!(
                    detail.contains("shared"),
                    "detail must say shared accounts are ineligible: {detail}"
                );
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    /// A team that owns accounts, but none for this model's provider, gets a NoRoute that
    /// names the team and the providers the model resolved to.
    #[tokio::test]
    async fn team_without_the_provider_gets_a_scoped_no_route() {
        let store = Store::open_in_memory().expect("store");
        let alpha = team(&store, "alpha").await;
        let _alpha_claude = owned(&store, "claude", Some(alpha)).await;
        let _shared_openai = owned(&store, "openai", None).await;
        let mut req = PlanRequest::new("openai/gpt-4.1");
        req.team_id = Some(alpha);
        let err = Planner::new()
            .plan(&catalog(), &store, req)
            .await
            .expect_err("alpha owns no openai account");
        match err {
            Error::NoRoute { detail, .. } => {
                assert!(detail.contains(&alpha.to_string()), "detail was {detail}");
                assert!(
                    detail.contains("openai/gpt-4.1"),
                    "the resolved target must be named: {detail}"
                );
                assert!(detail.contains("team"), "detail was {detail}");
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    /// The bare-id fan-out is scoped too: an unknown model reaches only the team's own
    /// providers, never another team's account or a shared one.
    #[tokio::test]
    async fn team_scope_does_not_widen_a_bare_model_to_other_tenants() {
        let store = Store::open_in_memory().expect("store");
        let alpha = team(&store, "alpha").await;
        let beta = team(&store, "beta").await;
        let _alpha_openai = owned(&store, "openai", Some(alpha)).await;
        let _beta_claude = owned(&store, "claude", Some(beta)).await;
        let _shared_groq = owned(&store, "groq", None).await;
        let mut req = PlanRequest::new("mystery-model");
        req.team_id = Some(alpha);
        let plan = Planner::new()
            .plan(&catalog(), &store, req)
            .await
            .expect("plan");
        assert_eq!(providers(&plan), ["openai"]);
        assert!(plan.candidates.iter().all(|c| c.model == "mystery-model"));
    }

    /// `together` promoted for the unknown bare model both providers fan out for.
    fn canary_rule(percent: u8) -> CanaryRule {
        CanaryRule {
            model: "canary-model".to_string(),
            provider: "together".to_string(),
            percent,
        }
    }

    async fn canary_plan(store: &Store, rule: &CanaryRule, seed: u64) -> Plan {
        let mut req = PlanRequest::new("canary-model");
        req.canary = Some(rule);
        req.canary_seed = seed;
        Planner::new()
            .plan(&catalog(), store, req)
            .await
            .expect("plan")
    }

    /// The canary decision is a deterministic function of the seed: repeating a seed
    /// always lands on the same provider, and 10% lands near 10% of a large sample.
    #[tokio::test]
    async fn canary_split_is_deterministic_and_matches_the_percentage() {
        let store = store_with(&[("openai", 0, 100), ("together", 0, 100)]).await;
        let rule = canary_rule(10);
        let mut promoted = 0usize;
        const SAMPLES: u64 = 300;
        for seed in 0..SAMPLES {
            let first = canary_plan(&store, &rule, seed).await;
            let again = canary_plan(&store, &rule, seed).await;
            assert_eq!(
                first.candidates[0].provider, again.candidates[0].provider,
                "seed {seed} must be stable"
            );
            if first.candidates[0].provider == "together" {
                promoted += 1;
            }
        }
        let rate = promoted as f64 / SAMPLES as f64;
        assert!(
            (0.02..0.22).contains(&rate),
            "roughly one request in ten is promoted, got {rate}"
        );
    }

    /// `0` never promotes and `100` always does, for every seed.
    #[tokio::test]
    async fn canary_percentage_boundaries_are_exact() {
        let store = store_with(&[("openai", 0, 100), ("together", 0, 100)]).await;
        let never = canary_rule(0);
        let always = canary_rule(100);
        for seed in [0u64, 1, 7, 99, u64::MAX] {
            assert_eq!(
                providers(&canary_plan(&store, &never, seed).await),
                ["openai", "together"],
                "0% never promotes (seed {seed})"
            );
            assert_eq!(
                providers(&canary_plan(&store, &always, seed).await),
                ["together", "openai"],
                "100% always promotes (seed {seed})"
            );
        }
    }

    /// Priority dominates: a canary provider in a lower tier is never promoted above it.
    #[tokio::test]
    async fn canary_never_promotes_across_a_priority_group() {
        let store = store_with(&[("openai", 0, 100), ("together", 1, 100)]).await;
        for seed in [0u64, 3, 42] {
            let plan = canary_plan(&store, &canary_rule(100), seed).await;
            assert_eq!(
                providers(&plan),
                ["openai", "together"],
                "the lower tier stays behind (seed {seed})"
            );
            assert_eq!(plan.candidates[0].priority, 0);
        }
    }

    /// The allowlist and the request policy are eligibility filters, so a canary cannot
    /// resurrect a provider they excluded.
    #[tokio::test]
    async fn canary_respects_the_allowlist_and_policy() {
        let store = store_with(&[("openai", 0, 100), ("together", 0, 100)]).await;
        let rule = canary_rule(100);
        let allowed = vec!["openai/*".to_string()];
        let mut req = PlanRequest::new("canary-model");
        req.canary = Some(&rule);
        req.canary_seed = 42;
        req.allowed_models = Some(&allowed);
        let plan = Planner::new()
            .plan(&catalog(), &store, req)
            .await
            .expect("plan");
        assert_eq!(providers(&plan), ["openai"], "the allowlist wins");

        let policy = ProviderPolicy {
            ignore: vec!["together".to_string()],
            ..ProviderPolicy::default()
        };
        let mut req = PlanRequest::new("canary-model");
        req.canary = Some(&rule);
        req.canary_seed = 42;
        req.policy = Some(&policy);
        let plan = Planner::new()
            .plan(&catalog(), &store, req)
            .await
            .expect("plan");
        assert_eq!(providers(&plan), ["openai"], "the policy wins");
    }

    /// Team scope decides which accounts exist, so a canary naming another team's
    /// provider has nothing to promote.
    #[tokio::test]
    async fn canary_respects_team_scope() {
        let store = Store::open_in_memory().expect("store");
        let alpha = team(&store, "alpha").await;
        let beta = team(&store, "beta").await;
        let _alpha_openai = owned(&store, "openai", Some(alpha)).await;
        let _beta_together = owned(&store, "together", Some(beta)).await;

        let rule = canary_rule(100);
        let mut req = PlanRequest::new("canary-model");
        req.canary = Some(&rule);
        req.canary_seed = 42;
        req.team_id = Some(alpha);
        let plan = Planner::new()
            .plan(&catalog(), &store, req)
            .await
            .expect("plan");
        assert_eq!(
            providers(&plan),
            ["openai"],
            "beta's account is not eligible"
        );

        // The same rule does apply to beta's own plan.
        let mut req = PlanRequest::new("canary-model");
        req.canary = Some(&rule);
        req.canary_seed = 42;
        req.team_id = Some(beta);
        let plan = Planner::new()
            .plan(&catalog(), &store, req)
            .await
            .expect("plan");
        assert_eq!(providers(&plan), ["together"]);
    }

    /// A canary never adds a candidate: without a canary-provider account for the model
    /// the plan is exactly what it would have been, and a rule for another model does
    /// nothing at all.
    #[tokio::test]
    async fn canary_never_adds_a_candidate_and_ignores_other_models() {
        let store = store_with(&[("openai", 0, 100)]).await;
        let plan = canary_plan(&store, &canary_rule(100), 42).await;
        assert_eq!(providers(&plan), ["openai"]);

        let other = CanaryRule {
            model: "another-model".to_string(),
            provider: "together".to_string(),
            percent: 100,
        };
        let plan = canary_plan(&store, &other, 42).await;
        assert_eq!(providers(&plan), ["openai"]);
    }
}
