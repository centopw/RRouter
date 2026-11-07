//! The model registry: the merged, deduplicated view behind `GET /api/models`.
//!
//! One row per `(provider, model)`. Sources stack in a fixed order —
//! `catalog` (compiled-in), `synced` (models.dev), `upstream` (the provider's own
//! `/v1/models`), `custom` (operator-defined) — and a later source wins on a collision.
//! Capabilities merge per field, so a partial capability object never erases what the
//! row underneath already knew.
//!
//! Nothing here performs I/O beyond the `Store` reads handed in, except
//! [`download_models_dev`], which owns the models.dev fetch.

use std::collections::{BTreeMap, BTreeSet};

use rrouter_core::{Error, Result};
use rrouter_router::catalog::ingest_providers;
use rrouter_router::ModelInfo;
use rrouter_store::{ModelRow, Store};
use serde::Serialize;
use serde_json::{Map, Value};

use crate::state::AppState;

/// Compiled-in catalog [`Catalog::all`].
pub const SOURCE_CATALOG: &str = "catalog";
/// models.dev download.
pub const SOURCE_SYNCED: &str = "synced";
/// The provider's own model list endpoint.
pub const SOURCE_UPSTREAM: &str = "upstream";
/// Operator-defined row.
pub const SOURCE_CUSTOM: &str = "custom";

/// URL of the models.dev catalog.
pub const MODELS_DEV_URL: &str = "https://models.dev/api.json";

/// A provider's model list is capped here so one bad download cannot fill the database.
pub const MAX_FETCHED_IDS: usize = 5000;

/// The nine capability flags every model object carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Capabilities {
    pub vision: bool,
    pub pdf: bool,
    pub audio_input: bool,
    pub video_input: bool,
    pub image_output: bool,
    pub audio_output: bool,
    pub tools: bool,
    pub reasoning: bool,
    pub search: bool,
}

/// Catalog defaults: a curated chat model is assumed to support tools and nothing else
/// the catalog cannot know. `search` is never derived from models.dev — it has no such
/// field — so it is only ever set by an operator.
impl Default for Capabilities {
    fn default() -> Self {
        Self {
            vision: false,
            pdf: false,
            audio_input: false,
            video_input: false,
            image_output: false,
            audio_output: false,
            tools: true,
            reasoning: false,
            search: false,
        }
    }
}

impl Capabilities {
    /// Layer a stored capability object over these values. Only keys present as booleans
    /// change; everything else falls through to the row underneath.
    pub fn overlay(&mut self, caps: &Value) {
        let Some(object) = caps.as_object() else { return };
        for (key, flag) in object {
            let Some(flag) = flag.as_bool() else { continue };
            match key.as_str() {
                "vision" => self.vision = flag,
                "pdf" => self.pdf = flag,
                "audio_input" => self.audio_input = flag,
                "video_input" => self.video_input = flag,
                "image_output" => self.image_output = flag,
                "audio_output" => self.audio_output = flag,
                "tools" => self.tools = flag,
                "reasoning" => self.reasoning = flag,
                "search" => self.search = flag,
                _ => {}
            }
        }
    }
}

/// Published price per million tokens, in USD. models.dev is the only source; a
/// provider's own `/models` response does not carry pricing.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Cost {
    pub input: f64,
    pub output: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_read: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_write: Option<f64>,
}

/// One item of the `GET /api/models` payload.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ModelItem {
    #[serde(rename = "ref")]
    pub reference: String,
    pub provider: String,
    pub id: String,
    pub display: Option<String>,
    pub context: Option<u32>,
    pub max_output: Option<u32>,
    pub capabilities: Capabilities,
    pub cost: Option<Cost>,
    pub source: String,
    pub enabled: bool,
    pub available: bool,
    /// A family its provider has retired (or models.dev marks deprecated). Still
    /// routable when a client names it explicitly, but never offered first.
    pub deprecated: bool,
}

impl ModelItem {
    fn new(provider: &str, id: &str, source: &str) -> Self {
        Self {
            reference: format!("{provider}/{id}"),
            provider: provider.to_string(),
            id: id.to_string(),
            display: None,
            context: None,
            max_output: None,
            capabilities: Capabilities::default(),
            cost: None,
            source: source.to_string(),
            enabled: true,
            available: false,
            deprecated: rrouter_router::catalog::is_retired(provider, id),
        }
    }
}

/// Precedence of a stored source. Higher wins; unknown values rank lowest.
fn source_rank(source: &str) -> u8 {
    match source {
        SOURCE_CUSTOM => 3,
        SOURCE_UPSTREAM => 2,
        SOURCE_SYNCED => 1,
        _ => 0,
    }
}

/// Merge catalog rows and stored rows into the contract's item list, sorted by
/// `provider`, then `id`.
pub fn merge(
    catalog_rows: Vec<ModelInfo>,
    rows: &[ModelRow],
    disabled: &BTreeSet<String>,
    active_providers: &BTreeSet<String>,
) -> Vec<ModelItem> {
    let mut items: BTreeMap<(String, String), ModelItem> = BTreeMap::new();

    // models.dev describes a model under its vendor (`openai`), while a registry
    // provider may be a different product line serving the same model id (`codex`).
    // Those rows carry the limits and capabilities; the catalog carries the fact that
    // the product line serves it.
    let donors: BTreeMap<(&str, &str), &ModelRow> = rows
        .iter()
        .filter(|row| row.source == SOURCE_SYNCED)
        .map(|row| ((row.provider.as_str(), row.model.as_str()), row))
        .collect();

    for info in catalog_rows {
        let mut item = ModelItem::new(&info.provider, &info.id, SOURCE_CATALOG);
        // The catalog records an output cap, not a context window.
        item.max_output = info.max_tokens;
        let vendor = rrouter_router::catalog::models_dev_provider(&info.provider);
        if let Some(donor) = donors.get(&(vendor, info.id.as_str())) {
            overlay(&mut item, donor);
        }
        items.insert((info.provider, info.id), item);
    }

    // Later sources win. Sorting by rank (stable) makes the store's own ordering
    // irrelevant: rows of one source keep their `(provider, model)` order.
    let mut ordered: Vec<&ModelRow> = rows.iter().collect();
    ordered.sort_by_key(|row| source_rank(&row.source));

    for row in ordered {
        // models.dev describes models; it does not prove a provider serves one. Its rows
        // are merged into an existing item and never create one, so the list stays
        // "models this gateway can actually call".
        if row.source == SOURCE_SYNCED {
            if let Some(item) = items.get_mut(&(row.provider.clone(), row.model.clone())) {
                overlay(item, row);
            }
            continue;
        }
        let key = (row.provider.clone(), row.model.clone());
        let item = match items.get_mut(&key) {
            Some(item) => item,
            None => items
                .entry(key)
                .or_insert_with(|| ModelItem::new(&row.provider, &row.model, &row.source)),
        };
        overlay(item, row);
        // Source answers "why does this model exist here", not "who wrote last".
        // models.dev enriching a shipped catalog entry must not relabel it as a
        // discovery; only the operator's own row overrides a catalog entry.
        let keep = matches!(item.source.as_str(), SOURCE_CATALOG | SOURCE_CUSTOM);
        if !keep || row.source == SOURCE_CUSTOM {
            item.source = row.source.clone();
        }
    }

    let mut out: Vec<ModelItem> = items
        .into_values()
        // A model is listed only when its provider is connected right now. This is the
        // whole point of the registry: what the gateway can route, not what exists.
        .filter(|item| active_providers.contains(&item.provider))
        .map(|mut item| {
            item.enabled = !disabled.contains(&item.id);
            item.available = true;
            item.deprecated =
                item.deprecated || rrouter_router::catalog::is_retired(&item.provider, &item.id);
            item
        })
        .collect();
    // Newest first inside each provider; retired families after everything current.
    out.sort_by(|a, b| {
        a.provider
            .cmp(&b.provider)
            .then_with(|| a.deprecated.cmp(&b.deprecated))
            .then_with(|| rrouter_router::catalog::compare_newest(&a.id, &b.id))
    });
    out
}

/// Apply one row's stated facts to an item. Absent fields never erase present ones,
/// so a vendor row enriching a catalog entry cannot blank its output cap.
fn overlay(item: &mut ModelItem, row: &ModelRow) {
    if row.display.is_some() {
        item.display = row.display.clone();
    }
    if row.context.is_some() {
        item.context = row.context;
    }
    if row.max_output.is_some() {
        item.max_output = row.max_output;
    }
    if row.cost.is_some() {
        item.cost = row.cost.as_ref().and_then(parse_cost);
    }
    item.capabilities.overlay(&row.caps);
    if row.caps.get("deprecated").and_then(Value::as_bool) == Some(true) {
        item.deprecated = true;
    }
}

/// `cost: {input, output, cache_read, cache_write}` in USD per million tokens.
fn parse_cost(value: &Value) -> Option<Cost> {
    let input = value.get("input").and_then(Value::as_f64)?;
    let output = value.get("output").and_then(Value::as_f64)?;
    Some(Cost {
        input,
        output,
        cache_read: value.get("cache_read").and_then(Value::as_f64),
        cache_write: value.get("cache_write").and_then(Value::as_f64),
    })
}

/// Rates for accounting, in USD per million tokens: every field the row does not
/// publish prices 0, which is the contract's rule for a partial price. A blob with no
/// numeric rate at all is not a price, so it can never shadow a vendor row that has one.
fn rates(value: &Value) -> Option<Cost> {
    if !value.is_object() {
        return None;
    }
    let rate = |key: &str| value.get(key).and_then(Value::as_f64);
    let (input, output) = (rate("input"), rate("output"));
    if input.is_none() && output.is_none() && rate("cache_read").is_none()
        && rate("cache_write").is_none()
    {
        return None;
    }
    Some(Cost {
        input: input.unwrap_or(0.0),
        output: output.unwrap_or(0.0),
        cache_read: rate("cache_read"),
        cache_write: rate("cache_write"),
    })
}

impl Cost {
    /// Micro-USD for one request. A per-million-token rate times a token count is
    /// already micro-dollars (`tokens × USD/Mtok = tokens × µUSD/token`), so this is a
    /// sum and a round — no division, no float drift from scaling. A rate the catalog
    /// does not publish prices its tokens at 0 rather than at a guess.
    pub fn micros(&self, usage: rrouter_core::Usage) -> u64 {
        let total = f64::from(usage.input_tokens) * self.input
            + f64::from(usage.output_tokens) * self.output
            + f64::from(usage.cache_read_tokens) * self.cache_read.unwrap_or(0.0)
            + f64::from(usage.cache_write_tokens) * self.cache_write.unwrap_or(0.0);
        if !total.is_finite() || total <= 0.0 {
            return 0;
        }
        // Half away from zero; a single request cannot approach u64::MAX micro-USD, but
        // a hand-edited price could, and saturating beats wrapping.
        total.round().min(u64::MAX as f64) as u64
    }
}

/// What one request cost, in micro-USD, from the registry rows that publish a price.
///
/// Precedence matches the registry merge — `custom` > `upstream` > `synced` — and the
/// provider's own row wins over the models.dev vendor row it mirrors, so a `codex`
/// request is priced from `openai`'s catalog when codex has no synced rows of its own.
/// A model with no published price costs 0: the gateway never guesses a rate.
///
/// Deliberately unmemoized. The caller runs this inside the spawned usage writer, so
/// the indexed read never touches the response path, and a price the operator just
/// edited applies to the next request instead of waiting for a cache to expire.
pub async fn cost_micros(
    store: &Store,
    provider: &str,
    model: &str,
    usage: rrouter_core::Usage,
) -> u64 {
    let vendor = rrouter_router::catalog::models_dev_provider(provider);
    // Usage rows record the resolved ref (`provider/id`), while the registry keys models
    // by bare id. Strip our own provider prefix; ids that legitimately contain a slash
    // (meta-llama/...) only lose the prefix that matches this provider.
    let bare = model
        .strip_prefix(provider)
        .and_then(|rest| rest.strip_prefix('/'))
        .unwrap_or(model);
    let rows = match store.model_costs(bare, &[provider, vendor]).await {
        Ok(rows) if rows.is_empty() && bare != model => {
            store.model_costs(model, &[provider, vendor]).await.unwrap_or_default()
        }
        Ok(rows) => rows,
        Err(e) => {
            // Accounting must never fail a request; an unknown price is 0, not a panic.
            tracing::warn!(error = %e, provider, model, "price lookup failed; cost recorded as 0");
            return 0;
        }
    };
    let mut best: Option<((u8, u8), Cost)> = None;
    for row in &rows {
        let Some(cost) = rates(&row.cost) else {
            continue;
        };
        // The provider's own row first, then the registry's source precedence.
        let key = (u8::from(row.provider == provider), source_rank(&row.source));
        let better = match best.as_ref() {
            Some((best_key, _)) => key > *best_key,
            None => true,
        };
        if better {
            best = Some((key, cost));
        }
    }
    best.map_or(0, |(_, cost)| cost.micros(usage))
}

/// Live `GET /api/models` payload.
pub async fn list(state: &AppState) -> Result<Vec<ModelItem>> {
    let store = state.store.as_ref();
    // Custom rows are the operator's own statement, so they always list. Synced and
    // upstream rows are dropped for providers this binary cannot route, which keeps a
    // stale sync from flooding the UI after a provider is removed.
    let rows: Vec<ModelRow> = store
        .list_models()
        .await?
        .into_iter()
        .filter(|row| {
            row.source == SOURCE_CUSTOM || rrouter_auth::get(&row.provider).is_some()
        })
        .collect();
    let disabled: BTreeSet<String> = store.disabled_models().await?.into_iter().collect();
    let active_providers: BTreeSet<String> = store
        .list_connections(None)
        .await?
        .into_iter()
        .filter(|conn| conn.is_active)
        .map(|conn| conn.provider)
        .collect();
    Ok(merge(state.catalog.all(), &rows, &disabled, &active_providers))
}

/// One merged item, for the `POST /api/models/custom` echo.
pub async fn item(state: &AppState, provider: &str, id: &str) -> Result<Option<ModelItem>> {
    // Built from storage rather than from `list`: a row the operator just defined must be
    // echoed even while its provider is disconnected, which is exactly when a custom model
    // is most useful — it documents what will be callable once the account is added.
    let rows = state.store.list_models().await?;
    let Some(row) = rows.iter().find(|r| r.provider == provider && r.model == id) else {
        return Ok(None);
    };
    let mut item = ModelItem::new(provider, id, &row.source);
    let vendor = rrouter_router::catalog::models_dev_provider(provider);
    if let Some(donor) = rows
        .iter()
        .find(|r| r.source == SOURCE_SYNCED && r.provider == vendor && r.model == id)
    {
        overlay(&mut item, donor);
    }
    overlay(&mut item, row);
    let disabled: BTreeSet<String> = state.store.disabled_models().await?.into_iter().collect();
    item.enabled = !disabled.contains(&item.id);
    item.available = state
        .store
        .list_connections(None)
        .await?
        .iter()
        .any(|c| c.is_active && c.provider == provider);
    item.deprecated = item.deprecated || rrouter_router::catalog::is_retired(provider, id);
    Ok(Some(item))
}

/// Download models.dev and return the parsed rows. Network failures are
/// [`Error::Transport`] so the route answers `502` and no row is touched.
pub async fn download_models_dev(state: &AppState) -> Result<Vec<ModelRow>> {
    let response = state
        .http
        .get(MODELS_DEV_URL)
        .timeout(std::time::Duration::from_secs(60))
        .send()
        .await
        // Every download failure is a `Transport` error, which the admin envelope renders
        // as 502 — the contract's status for a failed refresh.
        .map_err(|e| Error::Transport(format!("models.dev: {e}")))?;
    let status = response.status();
    if !status.is_success() {
        return Err(Error::Transport(format!("models.dev answered {status}")));
    }
    let value: Value = response
        .json()
        .await
        .map_err(|e| Error::Transport(format!("models.dev body: {e}")))?;
    Ok(parse_models_dev(&value))
}

/// Parse a models.dev document into `synced` rows.
///
/// Every models.dev provider is mirrored under its own id, and additionally under each
/// registry provider that aliases to it (`openai` → `codex` as well as `openai`), so a
/// codex account and an OpenAI API key both see the vendor's models. Pure: the tests
/// feed a fixture here instead of the network.
pub fn parse_models_dev(document: &Value) -> Vec<ModelRow> {
    let Some(providers) = document.as_object() else {
        return Vec::new();
    };
    let mut rows = Vec::new();
    for (models_dev_id, provider) in providers {
        let Some(models) = provider.get("models").and_then(Value::as_object) else {
            continue;
        };
        // Only providers this gateway can route are worth storing: models.dev lists
        // 240+, and a row nobody can call is noise in every list and select.
        let mut targets: Vec<&str> = ingest_providers(models_dev_id)
            .into_iter()
            .filter(|id| rrouter_auth::get(id).is_some())
            .collect();
        if rrouter_auth::get(models_dev_id).is_some() && !targets.contains(&models_dev_id.as_str()) {
            targets.push(models_dev_id.as_str());
        }
        if targets.is_empty() {
            continue;
        }
        for (model_id, entry) in models {
            if model_id.is_empty() || !entry.is_object() {
                continue;
            }
            for target in &targets {
                rows.push(ModelRow {
                    provider: (*target).to_string(),
                    model: model_id.clone(),
                    display: entry
                        .get("name")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .filter(|name| !name.is_empty()),
                    context: limit(entry, "context"),
                    max_output: limit(entry, "output"),
                    caps: caps_from_models_dev(entry),
                    cost: entry.get("cost").cloned().filter(|c| c.is_object()),
                    source: SOURCE_SYNCED.to_string(),
                    updated_at: 0,
                });
            }
        }
    }
    rows
}

/// `limit.<key>` as a token count. models.dev writes `0` for "unpublished", which
/// must read as unknown rather than as a zero-token window.
fn limit(entry: &Value, key: &str) -> Option<u32> {
    entry
        .get("limit")
        .and_then(|limit| limit.get(key))
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
        .and_then(|value| u32::try_from(value).ok())
}

/// Capability overrides derived from one models.dev entry.
///
/// Only what models.dev states is stored. `tools`/`reasoning` appear when the entry has
/// `tool_call`/`reasoning`; the modality lists are authoritative only when present.
/// `search` is never derived.
fn caps_from_models_dev(entry: &Value) -> Value {
    let mut caps = Map::new();
    if let Some(flag) = entry.get("tool_call").and_then(Value::as_bool) {
        caps.insert("tools".into(), Value::Bool(flag));
    }
    if let Some(flag) = entry.get("reasoning").and_then(Value::as_bool) {
        caps.insert("reasoning".into(), Value::Bool(flag));
    }
    // models.dev occasionally flags retired ids; keep the flag with the row so the
    // registry can sort and label them without another column.
    if let Some(flag) = entry.get("deprecated").and_then(Value::as_bool) {
        caps.insert("deprecated".into(), Value::Bool(flag));
    }
    let modalities = entry.get("modalities");
    let input = modalities.and_then(|m| m.get("input")).and_then(Value::as_array);
    if let Some(input) = input {
        let listed = |name: &str| input.iter().any(|value| value.as_str() == Some(name));
        for (key, name) in [
            ("vision", "image"),
            ("pdf", "pdf"),
            ("audio_input", "audio"),
            ("video_input", "video"),
        ] {
            caps.insert(key.into(), Value::Bool(listed(name)));
        }
    }
    let output = modalities.and_then(|m| m.get("output")).and_then(Value::as_array);
    if let Some(output) = output {
        let listed = |name: &str| output.iter().any(|value| value.as_str() == Some(name));
        for (key, name) in [("image_output", "image"), ("audio_output", "audio")] {
            caps.insert(key.into(), Value::Bool(listed(name)));
        }
    }
    Value::Object(caps)
}

/// The provider's model-list URL.
///
/// Rule: take the provider's chat endpoint (or the connection's `base_url_override`),
/// drop the trailing operation path, then append `/v1/models` — except when the
/// remainder already ends in `/v1`, in which case only `/models` is appended. That
/// keeps `https://api.openai.com/v1/chat/completions` from becoming
/// `…/v1/v1/models` while a bare override origin still gets `/v1/models`.
pub fn models_url(base: &str) -> String {
    let trimmed = base.trim_end_matches('/');
    let root = ["/chat/completions", "/v1/messages", "/messages"]
        .iter()
        .find_map(|suffix| trimmed.strip_suffix(suffix))
        .unwrap_or(trimmed);
    if root.ends_with("/v1") {
        format!("{root}/models")
    } else {
        format!("{root}/v1/models")
    }
}

/// Extract `data[].id` from a provider's model-list response, dropping empty ids and
/// duplicates. Also accepts a bare array of strings.
pub fn parse_model_ids(body: &Value) -> Vec<String> {
    let entries = match body {
        Value::Array(entries) => entries,
        Value::Object(object) => match object.get("data").and_then(Value::as_array) {
            Some(entries) => entries,
            None => return Vec::new(),
        },
        _ => return Vec::new(),
    };
    let mut ids: Vec<String> = Vec::with_capacity(entries.len());
    for entry in entries {
        let id = match entry {
            Value::String(id) => id.trim(),
            Value::Object(object) => object
                .get("id")
                .and_then(Value::as_str)
                .map(str::trim)
                .unwrap_or(""),
            _ => "",
        };
        if !id.is_empty() && !ids.iter().any(|seen| seen == id) {
            ids.push(id.to_string());
        }
    }
    ids
}

/// Whether a provider can enumerate its models over HTTP. Codex has no such endpoint
/// (its models arrive through the ChatGPT OAuth session), so the UI offers the custom
/// form instead of a fetch button.
pub fn can_list(provider: &str) -> bool {
    provider != "codex"
}

/// Replace every `synced` row with a fresh models.dev download. Returns
/// `(rows stored, distinct providers, unix seconds)`.
///
/// The download and parse happen before the delete, so a failed refresh leaves the
/// existing rows untouched.
pub async fn sync(state: &AppState) -> Result<(usize, usize, i64)> {
    let rows = download_models_dev(state).await?;
    let store: &Store = state.store.as_ref();
    store.clear_models_by_source(SOURCE_SYNCED).await?;
    for row in &rows {
        store.upsert_model(row).await?;
    }
    // Count what is actually stored: an `upstream` or `custom` row keeps its source, so
    // the download's row count can be larger than the number of `synced` rows written.
    let stored = store.list_models().await?;
    let synced: Vec<&ModelRow> = stored
        .iter()
        .filter(|row| row.source == SOURCE_SYNCED)
        .collect();
    let providers = synced
        .iter()
        .map(|row| row.provider.as_str())
        .collect::<BTreeSet<&str>>()
        .len();
    Ok((synced.len(), providers, crate::now_unix()))
}

/// Keep the registry fresh without an operator: first run after a minute (so a restart
/// is never slowed by the download), then daily. Failures are logged, never fatal.
pub fn spawn_daily_sync(state: std::sync::Arc<AppState>) {
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        loop {
            match sync(&state).await {
                Ok((models, providers, at)) => {
                    tracing::info!(models, providers, at, "models.dev synced")
                }
                Err(error) => tracing::warn!(%error, "models.dev sync failed"),
            }
            tokio::time::sleep(std::time::Duration::from_secs(24 * 60 * 60)).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The registry lists current models newest-first and retired/synced-deprecated
    /// rows last, whatever order the store returned.
    #[test]
    fn merge_orders_newest_first_and_deprecated_last() {
        let catalog = rrouter_router::Catalog::builtin();
        let rows = vec![ModelRow {
            provider: "claude".to_string(),
            model: "claude-sonnet-4-5".to_string(),
            display: None,
            context: None,
            max_output: None,
            caps: json!({"deprecated": true}),
            cost: None,
            source: SOURCE_SYNCED.to_string(),
            updated_at: 0,
        }];
        let active: BTreeSet<String> = ["claude".to_string()].into_iter().collect();
        let items = merge(catalog.all(), &rows, &BTreeSet::new(), &active);
        let ids: Vec<&str> = items.iter().map(|item| item.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "claude-haiku-4-5",
                "claude-opus-4-1",
                "claude-sonnet-4",
                "claude-sonnet-4-5",
            ],
            "newest first; the deprecated row sorts last"
        );
        assert!(items.last().expect("row").deprecated);
        assert!(!items[0].deprecated);
    }

    #[test]
    fn models_url_drops_operation_and_avoids_v1_v1() {
        assert_eq!(
            models_url("https://api.openai.com/v1/chat/completions"),
            "https://api.openai.com/v1/models"
        );
        assert_eq!(models_url("https://api.anthropic.com/v1/messages"), "https://api.anthropic.com/v1/models");
        assert_eq!(models_url("https://api.githubcopilot.com/chat/completions"), "https://api.githubcopilot.com/v1/models");
        assert_eq!(models_url("http://127.0.0.1:9/"), "http://127.0.0.1:9/v1/models");
        assert_eq!(models_url("https://api.groq.com/openai/v1"), "https://api.groq.com/openai/v1/models");
    }

    #[test]
    fn parse_ids_dedupes_and_ignores_empty() {
        let body = json!({"data": [{"id": "a"}, {"id": "a"}, {"id": ""}, {"id": "b"}]});
        assert_eq!(parse_model_ids(&body), vec!["a", "b"]);
        assert_eq!(parse_model_ids(&json!(["x", "x", " "])), vec!["x"]);
        assert!(parse_model_ids(&json!({"nope": []})).is_empty());
    }

    #[test]
    fn cost_in_micro_usd_is_exact_for_mixed_cache_tokens() {
        // USD per million tokens: 3 input, 12 output, 0.30 cache read, 3.75 cache write.
        let cost = parse_cost(&json!({
            "input": 3.0, "output": 12.0, "cache_read": 0.3, "cache_write": 3.75
        }))
        .expect("published price");
        // 1000×3 + 500×12 + 2000×0.3 + 300×3.75 = 3000 + 6000 + 600 + 1125.
        let usage = rrouter_core::Usage {
            input_tokens: 1_000,
            output_tokens: 500,
            cache_read_tokens: 2_000,
            cache_write_tokens: 300,
        };
        assert_eq!(cost.micros(usage), 10_725, "micro-USD, no rounding loss");

        // A rate the catalog leaves out prices its own tokens at 0, not at input's rate.
        let partial = rates(&json!({"input": 2.0, "output": 4.0})).expect("price");
        assert_eq!(
            partial.micros(rrouter_core::Usage {
                input_tokens: 10,
                output_tokens: 10,
                cache_read_tokens: 1_000,
                cache_write_tokens: 1_000,
            }),
            60
        );
        // Only output published: input is priced 0, per the contract.
        let output_only = rates(&json!({"output": 10.0})).expect("partial price");
        assert_eq!(
            output_only.micros(rrouter_core::Usage {
                input_tokens: 1_000,
                output_tokens: 2,
                ..Default::default()
            }),
            20
        );
        // No numeric rate at all: not a price, so it cannot shadow another row.
        assert!(rates(&json!({})).is_none());
        assert!(rates(&Value::Null).is_none());
    }

    #[test]
    fn cost_rounds_to_the_nearest_micro_usd() {
        let cost = rates(&json!({"input": 2.5, "output": 0.0})).expect("price");
        // 7 × 2.5 = 17.5 micro-USD; half a micro-dollar rounds away from zero.
        assert_eq!(
            cost.micros(rrouter_core::Usage { input_tokens: 7, ..Default::default() }),
            18
        );
        // Nothing priced is nothing spent, never a negative row.
        assert_eq!(cost.micros(rrouter_core::Usage::default()), 0);
    }

    /// The registry row that prices a model is chosen exactly the way the merge chooses
    /// it: the provider's own row first, then `custom` > `upstream` > `synced`. A model
    /// with no published price is 0, and a product line without its own row (`codex`)
    /// prices from the vendor catalog it mirrors (`openai`).
    #[tokio::test]
    async fn price_lookup_prefers_the_provider_row_then_the_best_source() {
        let store = Store::open_in_memory().expect("store");
        let row = |provider: &str, model: &str, source: &str, cost: Value| ModelRow {
            provider: provider.to_string(),
            model: model.to_string(),
            display: None,
            context: None,
            max_output: None,
            caps: json!({}),
            cost: Some(cost),
            source: source.to_string(),
            updated_at: 0,
        };
        let usage = rrouter_core::Usage { input_tokens: 1_000, ..Default::default() };
        store
            .upsert_model(&row(
                "openai",
                "gpt-priced",
                SOURCE_SYNCED,
                json!({"input": 1.0, "output": 2.0}),
            ))
            .await
            .expect("synced row");
        assert_eq!(cost_micros(&store, "openai", "gpt-priced", usage).await, 1_000);
        assert_eq!(
            cost_micros(&store, "codex", "gpt-priced", usage).await,
            1_000,
            "codex prices from the openai catalog row it mirrors"
        );

        // The operator's own rate beats the synced one for this provider.
        store
            .upsert_model(&row(
                "openai",
                "gpt-priced",
                SOURCE_CUSTOM,
                json!({"input": 4.0, "output": 8.0}),
            ))
            .await
            .expect("custom row");
        assert_eq!(cost_micros(&store, "openai", "gpt-priced", usage).await, 4_000);

        // No row at all: 0, never a guessed rate.
        assert_eq!(cost_micros(&store, "openai", "unpriced", usage).await, 0);
    }

    /// Usage records the resolved ref (`provider/id`); the registry keys bare ids. A ref
    /// must price exactly like its bare id, or every real request would cost 0.
    #[tokio::test]
    async fn a_ref_prices_like_its_bare_id() {
        let store = Store::open_in_memory().expect("store");
        store
            .upsert_model(&ModelRow {
                provider: "claude".into(),
                model: "claude-haiku-4-5".into(),
                display: None,
                context: None,
                max_output: None,
                caps: json!({}),
                cost: Some(json!({"input": 1.0, "output": 5.0})),
                source: SOURCE_UPSTREAM.into(),
                updated_at: 0,
            })
            .await
            .expect("row");
        let usage = rrouter_core::Usage { input_tokens: 8, output_tokens: 4, ..Default::default() };
        assert_eq!(
            cost_micros(&store, "claude", "claude/claude-haiku-4-5", usage).await,
            28,
            "8 in at $1/M + 4 out at $5/M"
        );
        assert_eq!(cost_micros(&store, "claude", "claude-haiku-4-5", usage).await, 28);
    }
}
