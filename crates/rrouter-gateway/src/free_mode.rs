//! Free mode: the operator's imported document, its total validation, and the discovery
//! run that turns a provider's own model list into `free_models` rows.
//!
//! The document is the single place an operator declares free-tier providers, the
//! accounts on them, and the egress proxy each account must leave through. Validation is
//! pre-flight and total: every problem the document has is collected into one list and
//! nothing is written until the whole document is acceptable, because a half-applied
//! import would leave accounts that route through a proxy the operator never meant to use.
//!
//! Credential material is written exactly once — into each connection's encrypted blob.
//! The stored copy of the document is redacted ([`public_document`]) because it lives in a
//! plain column and every caller that reads it only needs the shape.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use rrouter_auth::{Credentials, ProviderSpec};
use rrouter_core::Error;
use rrouter_store::{Connection, FreeModel, ProviderProfile, Store};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::models_registry::{self, FetchedModel};
use crate::provider_catalog::{self, ProviderCatalog};
use crate::state::AppState;

/// The only document version this build understands. A future schema bump must be
/// rejected loudly rather than silently half-read.
pub const DOCUMENT_VERSION: u32 = 1;

/// Where a `free_models` row came from. Routing filters on `free`, the dashboard shows
/// `source`, and an operator needs to know which signal decided a row.
pub const SOURCE_PROVIDER_LIST: &str = "provider-list";
pub const SOURCE_REGISTRY: &str = "registry";
pub const SOURCE_DOCUMENT: &str = "document";

/// Bounds that keep one import from turning into an unbounded write. They are generous
/// next to any real free-tier setup — an operator with more accounts than this is running
/// a farm and should script `POST /api/free-mode/import` per batch.
const MAX_PROVIDERS: usize = 64;
const MAX_ACCOUNTS_PER_PROVIDER: usize = 64;
const MAX_MODELS_PER_PROVIDER: usize = 500;
const MAX_LABEL_CHARS: usize = 128;
const MAX_PROVIDER_ID_CHARS: usize = 64;
const MAX_RPM: u32 = 100_000;
const MAX_ATTEMPTS: u32 = 64;
const MAX_WEIGHT: u32 = 1000;
/// The bare `free` ref may be narrowed to this many models; an operator selecting more is
/// describing a catalog, not a selection.
const MAX_SELECTED_MODELS: usize = 200;
const MAX_MODEL_ID_CHARS: usize = 200;

/// Routing knobs the routing workstream reads back from `free_mode.routing`.
const DEFAULT_MAX_ATTEMPTS: u32 = 8;
const DEFAULT_PREFER: &str = "spread";

// ---------------------------------------------------------------- document types

/// One imported document.
///
/// Every field is either defaulted or validated rather than required at parse time, so a
/// document with a missing `label` still reports the *other* problems in the same answer.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FreeModeDocument {
    #[serde(default = "document_version")]
    pub version: u32,
    /// Disable free-mode accounts of a listed provider that the document no longer names.
    #[serde(default)]
    pub prune: bool,
    #[serde(default)]
    pub routing: RoutingKnobs,
    /// Named proxies, so several accounts can share one without repeating its URL.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub proxies: BTreeMap<String, String>,
    #[serde(default)]
    pub providers: Vec<ProviderDoc>,
}

fn document_version() -> u32 {
    DOCUMENT_VERSION
}

/// The routing knobs the import stores for the free router.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RoutingKnobs {
    #[serde(default = "enabled_default")]
    pub enabled: bool,
    /// Default proactive per-account cap; an account's own `rpm` wins.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_account_rpm: Option<u32>,
    /// Cap on free-mode candidates in one plan.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_attempts: Option<u32>,
    /// `spread` (default) or `cheapest`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefer: Option<String>,
    /// Models the bare `free` ref may use. `None` (absent) and `Some([])` both mean every
    /// discovered free model; a non-empty list narrows the pool to exactly those ids.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub models: Option<Vec<String>>,
}

fn enabled_default() -> bool {
    true
}

impl Default for RoutingKnobs {
    fn default() -> Self {
        Self {
            enabled: true,
            per_account_rpm: None,
            max_attempts: None,
            prefer: None,
            models: None,
        }
    }
}

impl RoutingKnobs {
    /// The knobs as stored, with every default filled in.
    ///
    /// Stored resolved rather than as sent: the router must not have to carry the same
    /// defaults as the importer, or the two can drift and a document that omitted
    /// `max_attempts` would mean something different after a restart.
    ///
    /// `models` is the exception: it is stored exactly as given, and only when given, so
    /// "absent" and "empty" both mean the full discovered pool rather than becoming a
    /// selection of nothing.
    pub fn value(&self) -> Value {
        let mut value = json!({
            "enabled": self.enabled,
            "per_account_rpm": self.per_account_rpm,
            "max_attempts": self.max_attempts.unwrap_or(DEFAULT_MAX_ATTEMPTS),
            "prefer": self.prefer.as_deref().unwrap_or(DEFAULT_PREFER),
        });
        if let Some(models) = &self.models {
            if let Some(object) = value.as_object_mut() {
                object.insert("models".to_string(), json!(models));
            }
        }
        value
    }
}

/// One provider entry: which provider, how to reach it when it is operator-defined, the
/// models it serves for free, and the accounts that serve them.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderDoc {
    #[serde(default)]
    pub provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom: Option<CustomProvider>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<String>,
    #[serde(default)]
    pub accounts: Vec<AccountDoc>,
}

/// A provider this build does not compile: created and updated through the same
/// `provider_profiles` path `POST /api/models/providers` uses.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CustomProvider {
    #[serde(default)]
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<String>,
    #[serde(default)]
    pub base_url: String,
    /// `chat`/`openai` or `anthropic`/`messages`.
    #[serde(default)]
    pub dialect: String,
}

/// One account on a provider. `label` is the identity: `(provider, label)` is what makes a
/// re-import an update instead of a second connection.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AccountDoc {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    /// A key of `proxies`, or a full `scheme://[user:pass@]host[:port]` URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rpm: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weight: Option<u32>,
}

impl FreeModeDocument {
    /// The URL an account's `proxy` resolves to, or `None` when it declares no proxy.
    /// Validation has already proved the key exists and the URL parses.
    pub fn proxy_url(&self, account: &AccountDoc) -> Option<String> {
        let raw = account.proxy.as_deref()?.trim();
        if raw.is_empty() {
            return None;
        }
        if raw.contains("://") {
            return Some(raw.to_string());
        }
        self.proxies.get(raw).cloned()
    }
}

// ---------------------------------------------------------------- validation

/// A rejected document: one sentence for the operator plus every problem found.
///
/// The list is the point. An operator fixing a hand-written JSON document one 400 at a
/// time is the failure mode this exists to avoid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationError {
    pub message: String,
    pub details: Vec<String>,
}

impl ValidationError {
    fn new(details: Vec<String>) -> Self {
        Self {
            message: format!("the free-mode document has {} problem(s)", details.len()),
            details,
        }
    }

    fn single(detail: String) -> Self {
        Self {
            message: "the free-mode document is not valid JSON for this schema".to_string(),
            details: vec![detail],
        }
    }
}

/// Parse and validate a document, returning the normalized form the apply step uses.
///
/// `catalog` resolves `provider` refs exactly as model refs do (id or shorthand), so a
/// document can name `or` and mean `openrouter`. Nothing here writes.
pub fn validate(
    value: &Value,
    catalog: &ProviderCatalog,
) -> Result<FreeModeDocument, ValidationError> {
    let mut document: FreeModeDocument = match serde_json::from_value(value.clone()) {
        Ok(document) => document,
        Err(error) => return Err(ValidationError::single(format!("document: {error}"))),
    };
    let mut problems: Vec<String> = Vec::new();

    if document.version != DOCUMENT_VERSION {
        problems.push(format!(
            "version: this build understands version {DOCUMENT_VERSION}, not {}",
            document.version
        ));
    }

    check_routing(&document.routing, &mut problems);

    for (key, url) in &document.proxies {
        if key.trim().is_empty() {
            problems.push("proxies: a proxy needs a name".to_string());
        }
        if !valid_proxy_url(url) {
            // The URL is masked in the message: it may carry `user:pass@`.
            problems.push(format!(
                "proxies.{key}: {} is not a scheme://[user:pass@]host[:port] URL with a scheme in \
                 http, https, socks5 or socks5h",
                mask_proxy(url)
            ));
        }
    }

    if document.providers.is_empty() {
        problems.push("providers: at least one provider is required".to_string());
    }
    if document.providers.len() > MAX_PROVIDERS {
        problems.push(format!(
            "providers: at most {MAX_PROVIDERS} providers per document"
        ));
    }

    let mut seen_providers: BTreeSet<String> = BTreeSet::new();
    for (index, entry) in document.providers.iter_mut().enumerate() {
        let path = format!("providers[{index}]");
        match resolve_provider_id(entry, catalog) {
            Ok(id) => {
                if !seen_providers.insert(id.clone()) {
                    problems.push(format!("{path}.provider: {id} appears twice"));
                }
                entry.provider = id;
            }
            Err(detail) => problems.push(format!("{path}.provider: {detail}")),
        }
        if let Some(custom) = &entry.custom {
            check_custom(custom, &path, &mut problems);
        }
        if entry.accounts.is_empty() {
            problems.push(format!("{path}.accounts: at least one account is required"));
        }
        if entry.accounts.len() > MAX_ACCOUNTS_PER_PROVIDER {
            problems.push(format!(
                "{path}.accounts: at most {MAX_ACCOUNTS_PER_PROVIDER} accounts per provider"
            ));
        }
        if entry.models.len() > MAX_MODELS_PER_PROVIDER {
            problems.push(format!(
                "{path}.models: at most {MAX_MODELS_PER_PROVIDER} models per provider"
            ));
        }
        for (model_index, model) in entry.models.iter().enumerate() {
            if model.trim().is_empty() {
                problems.push(format!("{path}.models[{model_index}]: empty model id"));
            }
        }

        let mut seen_labels: BTreeSet<String> = BTreeSet::new();
        for (account_index, account) in entry.accounts.iter().enumerate() {
            let account_path = format!("{path}.accounts[{account_index}]");
            match account.label.as_deref().map(str::trim) {
                Some(label) if !label.is_empty() => {
                    if label.chars().count() > MAX_LABEL_CHARS {
                        problems.push(format!(
                            "{account_path}.label: longer than {MAX_LABEL_CHARS} characters"
                        ));
                    }
                    if !seen_labels.insert(label.to_string()) {
                        problems.push(format!(
                            "{account_path}.label: {} appears twice on this provider",
                            label
                        ));
                    }
                }
                _ => problems.push(format!("{account_path}.label: a label is required")),
            }
            if account
                .api_key
                .as_deref()
                .is_none_or(|key| key.trim().is_empty())
                && account
                    .access_token
                    .as_deref()
                    .is_none_or(|token| token.trim().is_empty())
            {
                problems.push(format!(
                    "{account_path}: one of api_key or access_token is required"
                ));
            }
            if let Some(rpm) = account.rpm {
                // A cap of zero would park the account forever; "no cap" is the absence of
                // the field (or of `routing.per_account_rpm`).
                if rpm == 0 || rpm > MAX_RPM {
                    problems.push(format!(
                        "{account_path}.rpm: must be between 1 and {MAX_RPM}"
                    ));
                }
            }
            if let Some(weight) = account.weight {
                if weight == 0 || weight > MAX_WEIGHT {
                    problems.push(format!(
                        "{account_path}.weight: must be between 1 and {MAX_WEIGHT}"
                    ));
                }
            }
            if let Some(raw) = account
                .proxy
                .as_deref()
                .map(str::trim)
                .filter(|raw| !raw.is_empty())
            {
                if raw.contains("://") {
                    if !valid_proxy_url(raw) {
                        problems.push(format!(
                            "{account_path}.proxy: {} is not a scheme://[user:pass@]host[:port] URL \
                             with a scheme in http, https, socks5 or socks5h",
                            mask_proxy(raw)
                        ));
                    }
                } else if !document.proxies.contains_key(raw) {
                    problems.push(format!("{account_path}.proxy: unknown proxy key {raw}"));
                }
            }
        }
    }

    if problems.is_empty() {
        Ok(document)
    } else {
        Err(ValidationError::new(problems))
    }
}

fn check_routing(routing: &RoutingKnobs, problems: &mut Vec<String>) {
    if let Some(rpm) = routing.per_account_rpm {
        if rpm == 0 || rpm > MAX_RPM {
            problems.push(format!(
                "routing.per_account_rpm: must be between 1 and {MAX_RPM}"
            ));
        }
    }
    if let Some(attempts) = routing.max_attempts {
        if attempts == 0 || attempts > MAX_ATTEMPTS {
            problems.push(format!(
                "routing.max_attempts: must be between 1 and {MAX_ATTEMPTS}"
            ));
        }
    }
    match routing.prefer.as_deref().map(str::trim) {
        None => {}
        Some("spread") | Some("cheapest") => {}
        Some(other) => problems.push(format!(
            "routing.prefer: {other:?} is not \"spread\" or \"cheapest\""
        )),
    }
    // A selection is a list of bare model ids: the router compares them against ids, so a
    // whitespace or duplicated entry would silently never match and shrink the pool.
    if let Some(models) = &routing.models {
        if models.len() > MAX_SELECTED_MODELS {
            problems.push(format!(
                "routing.models: at most {MAX_SELECTED_MODELS} entries"
            ));
        }
        let mut seen: BTreeSet<String> = BTreeSet::new();
        for (index, model) in models.iter().enumerate() {
            let id = model.trim();
            if id.is_empty() {
                problems.push(format!("routing.models[{index}]: a model id is required"));
                continue;
            }
            if model.chars().any(char::is_whitespace) {
                problems.push(format!(
                    "routing.models[{index}]: {id:?} cannot contain whitespace"
                ));
            }
            if id.chars().count() > MAX_MODEL_ID_CHARS {
                problems.push(format!(
                    "routing.models[{index}]: longer than {MAX_MODEL_ID_CHARS} characters"
                ));
            }
            if !seen.insert(id.to_string()) {
                problems.push(format!(
                    "routing.models[{index}]: {id} appears twice; each model id may appear once"
                ));
            }
        }
    }
}

/// The provider id one entry declares.
///
/// A `custom` block wins: it describes the host the accounts actually call. `provider` must
/// still name something the gateway knows so a typo cannot conjure a provider — either the
/// same id the block declares, or a compiled provider whose protocol family the operator
/// used as the reference.
fn resolve_provider_id(entry: &ProviderDoc, catalog: &ProviderCatalog) -> Result<String, String> {
    let declared = entry.provider.trim();
    if declared.is_empty() {
        return Err("a provider id is required".to_string());
    }
    let Some(custom) = &entry.custom else {
        return catalog.provider_for(declared).ok_or_else(|| {
            format!("unknown provider {declared}; add a custom block to declare it")
        });
    };
    let id = custom.id.trim();
    if id.is_empty() {
        return Err("custom.id is required".to_string());
    }
    if !valid_provider_id(id) {
        return Err(format!(
            "custom.id {id:?} must be 1-{MAX_PROVIDER_ID_CHARS} characters of a-z, 0-9, '-', '_' or '.'"
        ));
    }
    // A compiled provider owns its transport; a custom row under its id would shadow it.
    if rrouter_auth::get(id).is_some() {
        return Err(format!(
            "custom.id {id} collides with a compiled provider; pick another id"
        ));
    }
    if id != declared && catalog.provider_for(declared).is_none() {
        return Err(format!(
            "unknown provider {declared}; either make custom.id the provider id or name a \
             provider this build knows"
        ));
    }
    Ok(id.to_string())
}

fn check_custom(custom: &CustomProvider, path: &str, problems: &mut Vec<String>) {
    if custom.base_url.trim().is_empty() {
        problems.push(format!("{path}.custom.base_url: a base URL is required"));
    } else if let Err(message) = check_base_url(custom.base_url.trim()) {
        problems.push(format!("{path}.custom.base_url: {message}"));
    }
    if canonical_dialect(&custom.dialect).is_none() {
        problems.push(format!(
            "{path}.custom.dialect: {} is not \"chat\", \"openai\" or \"anthropic\"",
            if custom.dialect.trim().is_empty() {
                "(empty)"
            } else {
                custom.dialect.trim()
            }
        ));
    }
    if custom
        .display
        .as_deref()
        .is_some_and(|display| display.chars().count() > 64)
    {
        problems.push(format!("{path}.custom.display: longer than 64 characters"));
    }
}

/// A provider id that can appear as a ref segment.
fn valid_provider_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_PROVIDER_ID_CHARS
        && !id.starts_with('.')
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
}

/// Same rule `POST /api/models/providers` applies: an absolute http(s) URL with a host, no
/// query and no fragment. A trailing slash is dropped because the proxy grafts the
/// provider's path onto this origin.
fn check_base_url(raw: &str) -> Result<(), String> {
    let parsed =
        reqwest::Url::parse(raw).map_err(|_| "must be an absolute http(s) URL".to_string())?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err("must be an absolute http(s) URL".to_string());
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err("must not carry a query or fragment".to_string());
    }
    Ok(())
}

/// `scheme://[userinfo@]host[:port]` with a scheme the egress path supports.
///
/// `socks5`/`socks5h` need reqwest's `socks` feature, which the proxy workstream enables;
/// accepting them here only means the document's shape is right.
fn valid_proxy_url(raw: &str) -> bool {
    let Ok(parsed) = reqwest::Url::parse(raw.trim()) else {
        return false;
    };
    matches!(parsed.scheme(), "http" | "https" | "socks5" | "socks5h")
        && parsed.host_str().is_some_and(|host| !host.is_empty())
        && parsed.query().is_none()
        && parsed.fragment().is_none()
}

fn canonical_dialect(name: &str) -> Option<&'static str> {
    match name.trim().to_ascii_lowercase().as_str() {
        "openai" | "openai_chat" | "chat" => Some("openai"),
        "anthropic" | "anthropic_messages" | "messages" => Some("anthropic"),
        _ => None,
    }
}

// ---------------------------------------------------------------- redaction

/// A proxy URL as a read path may show it: userinfo replaced with `***`, host and port kept.
fn mask_proxy(url: &str) -> String {
    Credentials {
        proxy_url: Some(url.to_string()),
        ..Credentials::default()
    }
    .proxy_masked()
    .unwrap_or_else(|| url.to_string())
}

/// The document with every secret replaced by its presence.
///
/// The imported document carries API keys and OAuth tokens, and it is also what
/// `GET /api/free-mode` answers with. The credentials themselves are already sealed into
/// each connection's blob, so this copy keeps the shape an operator needs (which accounts
/// exist, whether they carry a key, which proxy they use) and drops the material.
pub fn public_document(value: &Value) -> Value {
    let mut out = value.clone();
    if let Some(providers) = out.get_mut("providers").and_then(Value::as_array_mut) {
        for entry in providers {
            let Some(accounts) = entry.get_mut("accounts").and_then(Value::as_array_mut) else {
                continue;
            };
            for account in accounts {
                let Some(object) = account.as_object_mut() else {
                    continue;
                };
                // Replacing the secret with its presence, never re-deriving it from an
                // already-redacted document: a second pass must keep reporting what the
                // first one recorded, or a read path would claim an account has no key.
                let strip = |object: &mut serde_json::Map<String, Value>, key: &str, flag: &str| {
                    match object.remove(key) {
                        Some(value) => {
                            let present =
                                value.as_str().is_some_and(|value| !value.trim().is_empty());
                            object.insert(flag.to_string(), Value::Bool(present));
                        }
                        None => {
                            object.entry(flag.to_string()).or_insert(Value::Bool(false));
                        }
                    }
                };
                strip(object, "api_key", "has_api_key");
                strip(object, "access_token", "has_access_token");
                strip(object, "refresh_token", "has_refresh_token");
                if let Some(Value::String(proxy)) = object.get("proxy").cloned() {
                    if proxy.contains("://") {
                        object.insert("proxy".to_string(), Value::String(mask_proxy(&proxy)));
                    }
                }
            }
        }
    }
    if let Some(proxies) = out.get_mut("proxies").and_then(Value::as_object_mut) {
        for url in proxies.values_mut() {
            if let Some(raw) = url.as_str() {
                if raw.contains("://") {
                    *url = Value::String(mask_proxy(raw));
                }
            }
        }
    }
    out
}

// ---------------------------------------------------------------- classification

/// Decide whether one advertised model is free, and by which signal.
///
/// Two signals count, in this order:
/// 1. the id advertises it — a `:free` suffix, or `-free` / `free-` / `free:` anywhere,
///    case-insensitive. That is OpenRouter's `:free` convention and the `-free`/`free-`
///    spelling most other free tiers use.
/// 2. the published price is exactly zero for input *and* output, which is how a registry
///    records a genuinely free model that carries no marker in its id.
///
/// A model that neither signal calls free is not free: an unknown price is not a free
/// price, and guessing would spend an operator's paid quota by accident.
///
/// The provider is part of the contract because availability is per provider, but nothing
/// here is provider-specific yet.
pub fn classify_free(
    provider: &str,
    model_id: &str,
    published_price: Option<f64>,
) -> (bool, Option<f64>, &'static str) {
    let _ = provider;
    let id = model_id.trim().to_ascii_lowercase();
    let advertised = id.ends_with(":free")
        || id.contains("-free")
        || id.contains("free-")
        || id.contains("free:");
    if advertised {
        return (true, published_price, SOURCE_PROVIDER_LIST);
    }
    if published_price.is_some_and(|price| price == 0.0) {
        return (true, published_price, SOURCE_REGISTRY);
    }
    (false, published_price, SOURCE_PROVIDER_LIST)
}

/// Per-million-token `input + output` price a stored `cost` blob publishes.
///
/// Same validity rule as the planner's: a partial price prices the missing side at zero
/// rather than disqualifying the row, and a blob with no rate at all publishes nothing.
fn published_price_of(cost: &Value) -> Option<f64> {
    if !cost.is_object() {
        return None;
    }
    let rate = |key: &str| cost.get(key).and_then(Value::as_f64);
    let (input, output) = (rate("input"), rate("output"));
    if input.is_none() && output.is_none() && rate("cache_read").is_none() {
        return None;
    }
    Some(input.unwrap_or(0.0) + output.unwrap_or(0.0))
}

/// The registry's price for one model on one provider, when it has one.
async fn published_price(
    store: &Store,
    provider: &str,
    model: &str,
) -> rrouter_core::Result<Option<f64>> {
    let rows = store.model_costs(model, &[provider]).await?;
    Ok(rows.iter().find_map(|row| published_price_of(&row.cost)))
}

// ---------------------------------------------------------------- apply

/// What one import changed, for the endpoint and the CLI summary.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct ApplyReport {
    pub providers: usize,
    pub created: usize,
    pub updated: usize,
    pub disabled: usize,
    pub models: usize,
}

/// Write a validated document: providers, accounts, routing knobs and the seeded models.
///
/// Order matters. A custom provider is created before the connections that dispatch to it,
/// and the stored document is written last, so a failure anywhere leaves the previous
/// document in place (the router keeps reading the last document that fully applied).
/// Every write is idempotent, so a retry after a partial failure converges.
pub async fn apply(
    store: &Store,
    document: &FreeModeDocument,
) -> rrouter_core::Result<ApplyReport> {
    let mut report = ApplyReport {
        providers: document.providers.len(),
        ..ApplyReport::default()
    };
    let mut seeded: BTreeSet<(String, String)> = BTreeSet::new();
    let now = crate::now_unix();

    for entry in &document.providers {
        let provider = entry.provider.trim();
        if let Some(custom) = &entry.custom {
            ensure_custom_provider(store, custom, provider).await?;
        }
        let existing = store.list_connections(Some(provider)).await?;
        let mut listed: Vec<String> = Vec::with_capacity(entry.accounts.len());

        for account in &entry.accounts {
            let label = account
                .label
                .as_deref()
                .unwrap_or_default()
                .trim()
                .to_string();
            listed.push(label.clone());
            let proxy = document.proxy_url(account);
            let rpm = account.rpm.or(document.routing.per_account_rpm);
            match existing.iter().find(|conn| identity(conn, &label)) {
                Some(conn) => {
                    // A merge cannot express "remove this", so an absent proxy/rpm is sent
                    // as the empty-string/zero sentinel `merge_into` understands.
                    let mut patch = account_credentials(entry, account, &label);
                    patch.proxy_url = Some(proxy.unwrap_or_default());
                    patch.free_rpm = Some(rpm.unwrap_or(0));
                    store.merge_credentials(conn.id, patch).await?;
                    store.set_connection_name(conn.id, Some(&label)).await?;
                    store.set_connection_active(conn.id, true).await?;
                    if let Some(priority) = account.priority {
                        store.set_connection_priority(conn.id, priority).await?;
                    }
                    if let Some(weight) = account.weight {
                        store.set_connection_weight(conn.id, weight).await?;
                    }
                    report.updated += 1;
                }
                None => {
                    let mut creds = account_credentials(entry, account, &label);
                    creds.proxy_url = proxy;
                    creds.free_rpm = rpm;
                    let auth_type = if creds.api_key.is_some() {
                        "api_key"
                    } else {
                        "oauth"
                    };
                    let created = store
                        .create_connection(provider, auth_type, creds, Some(&label))
                        .await?;
                    if let Some(priority) = account.priority {
                        store.set_connection_priority(created.id, priority).await?;
                    }
                    if let Some(weight) = account.weight {
                        store.set_connection_weight(created.id, weight).await?;
                    }
                    report.created += 1;
                }
            }
        }

        if document.prune {
            for conn in &existing {
                if !conn.credentials.free_mode || !conn.is_active {
                    continue;
                }
                let label = conn
                    .credentials
                    .account_label
                    .as_deref()
                    .or(conn.name.as_deref())
                    .unwrap_or_default();
                if listed.iter().any(|listed| listed == label) {
                    continue;
                }
                store.set_connection_active(conn.id, false).await?;
                report.disabled += 1;
            }
        }

        for model in &entry.models {
            let model = model.trim();
            if model.is_empty() {
                continue;
            }
            let price = published_price(store, provider, model).await?;
            store
                .upsert_free_model(&FreeModel {
                    provider: provider.to_string(),
                    model: model.to_string(),
                    free: true,
                    price,
                    source: SOURCE_DOCUMENT.to_string(),
                    connection_id: None,
                    discovered_at: now,
                })
                .await?;
            seeded.insert((provider.to_string(), model.to_string()));
        }
    }
    report.models = seeded.len();

    let stored = serde_json::to_value(document)?;
    store
        .save_free_mode_config(&public_document(&stored), &document.routing.value())
        .await?;
    Ok(report)
}

/// Does this connection already carry the document's account identity?
///
/// Both columns are checked because the two have moved independently in the past:
/// `connections.name` is the operator's label and `credentials.account_label` is the one
/// the importer writes. Accepting either is what keeps a re-import from creating a second
/// account for a row the operator renamed in the dashboard.
fn identity(conn: &Connection, label: &str) -> bool {
    conn.credentials.account_label.as_deref() == Some(label) || conn.name.as_deref() == Some(label)
}

/// The credential fields an account declares, minus the proxy and rpm (the import path sets
/// those differently for create and update).
fn account_credentials(entry: &ProviderDoc, account: &AccountDoc, label: &str) -> Credentials {
    let secret = |value: &Option<String>| {
        value
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    Credentials {
        api_key: secret(&account.api_key),
        access_token: secret(&account.access_token),
        refresh_token: secret(&account.refresh_token),
        account_label: Some(label.to_string()),
        // A custom provider owns its endpoint, exactly as `POST /api/connections` pins it.
        base_url_override: entry
            .custom
            .as_ref()
            .map(|custom| custom.base_url.trim().trim_end_matches('/').to_string()),
        free_mode: true,
        ..Credentials::default()
    }
}

/// Create or update the custom provider one entry declares.
async fn ensure_custom_provider(
    store: &Store,
    custom: &CustomProvider,
    provider: &str,
) -> rrouter_core::Result<()> {
    let dialect = canonical_dialect(&custom.dialect).ok_or_else(|| {
        Error::BadRequest(format!(
            "provider {provider} declares an unsupported dialect {:?}",
            custom.dialect
        ))
    })?;
    let display = custom
        .display
        .as_deref()
        .map(str::trim)
        .filter(|display| !display.is_empty())
        .unwrap_or(provider);
    let existing = store.provider_profile(provider).await?;
    let is_custom = existing
        .as_ref()
        .is_some_and(|profile| profile.display.is_some());
    let profile = ProviderProfile {
        provider: provider.to_string(),
        // An existing custom provider keeps its shorthand: operators may have model refs
        // written against it, and an import is not the place to rename a code.
        code: match existing.as_ref() {
            Some(profile) if is_custom => profile.code.clone(),
            _ => unique_code(store, provider).await?,
        },
        display: Some(display.to_string()),
        dialect: Some(dialect.to_string()),
        base_url: Some(custom.base_url.trim().trim_end_matches('/').to_string()),
        created_at: existing
            .map(|profile| profile.created_at)
            .unwrap_or_else(crate::now_unix),
    };
    if is_custom {
        store.update_custom_provider(&profile).await?;
    } else {
        store.create_custom_provider(&profile).await?;
    }
    Ok(())
}

/// A shorthand for a new custom provider that no other provider owns.
///
/// `provider_profiles.code` is unique, so a collision has to be resolved here rather than
/// failing the whole import on an unrelated provider that happens to share a slug.
async fn unique_code(store: &Store, provider: &str) -> rrouter_core::Result<String> {
    let base = provider_catalog::slug(provider);
    for suffix in 0..100u32 {
        let candidate = if suffix == 0 {
            base.clone()
        } else {
            format!("{base}-{suffix}")
        };
        if !provider_catalog::valid_code(&candidate) {
            break;
        }
        if !store.code_taken(&candidate, Some(provider)).await? {
            return Ok(candidate);
        }
    }
    Ok(format!("fm-{}", &Uuid::new_v4().simple().to_string()[..8]))
}

// ---------------------------------------------------------------- discovery

/// One provider's share of a discovery run.
#[derive(Debug, Clone, Serialize)]
pub struct ProviderDiscovery {
    pub provider: String,
    pub accounts: usize,
    /// Distinct ids any of the provider's accounts listed.
    pub models: usize,
    /// Of those, the ones classified free.
    pub free: usize,
}

/// One account's share of a discovery run. `error` is set instead of failing the run: one
/// dead account must not cost the operator the rows the other accounts proved.
#[derive(Debug, Clone, Serialize)]
pub struct AccountDiscovery {
    pub connection_id: String,
    pub provider: String,
    pub label: Option<String>,
    pub models: usize,
    pub free: usize,
    pub error: Option<String>,
}

/// The answer of one discovery run.
#[derive(Debug, Clone, Serialize)]
pub struct DiscoveryReport {
    pub ok: bool,
    pub discovered_at: i64,
    pub providers: Vec<ProviderDiscovery>,
    pub accounts: Vec<AccountDiscovery>,
}

/// Ask every free-mode account for its own model list and record what it serves.
///
/// Each account is asked through *its own* client ([`egress`]) and its own credentials, so
/// the rows prove that this account can serve this model — which is the only claim the free
/// rotation makes. `only` filters to the named provider ids; empty means every provider.
pub async fn discover(
    state: &Arc<AppState>,
    only: &[String],
) -> rrouter_core::Result<DiscoveryReport> {
    let connections = state.store.list_connections(None).await?;
    let declared = declared_models(state.store.as_ref()).await?;
    let mut report = DiscoveryReport {
        ok: true,
        discovered_at: crate::now_unix(),
        providers: Vec::new(),
        accounts: Vec::new(),
    };
    let mut counted: BTreeMap<String, (BTreeSet<String>, BTreeSet<String>, usize)> =
        BTreeMap::new();

    for conn in connections
        .iter()
        .filter(|conn| conn.credentials.free_mode)
        .filter(|conn| only.is_empty() || only.contains(&conn.provider))
    {
        let label = conn
            .credentials
            .account_label
            .clone()
            .or_else(|| conn.name.clone());
        let mut entry = AccountDiscovery {
            connection_id: conn.id.to_string(),
            provider: conn.provider.clone(),
            label: label.clone(),
            models: 0,
            free: 0,
            error: None,
        };
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut free_ids: BTreeSet<String> = BTreeSet::new();
        match fetch_models(state, conn).await {
            Ok(items) => {
                for item in items.into_iter().take(models_registry::MAX_FETCHED_IDS) {
                    let id = item.id.trim().to_string();
                    if id.is_empty() || !seen.insert(id.clone()) {
                        continue;
                    }
                    let price = published_price(state.store.as_ref(), &conn.provider, &id).await?;
                    let (mut free, price, mut source) = classify_free(&conn.provider, &id, price);
                    if !free && declared.contains(&(conn.provider.clone(), id.clone())) {
                        // The document named this model as free. A list that merely omits
                        // the marker must not downgrade the operator's declaration; a list
                        // that proves it free upgrades the source.
                        free = true;
                        source = SOURCE_DOCUMENT;
                    }
                    if free {
                        free_ids.insert(id.clone());
                    }
                    state
                        .store
                        .upsert_free_model(&FreeModel {
                            provider: conn.provider.clone(),
                            model: id,
                            free,
                            price,
                            source: source.to_string(),
                            connection_id: Some(conn.id),
                            discovered_at: report.discovered_at,
                        })
                        .await?;
                }
                entry.models = seen.len();
                entry.free = free_ids.len();
            }
            Err(error) => entry.error = Some(error),
        }
        // The provider's account count includes the accounts that failed: it reports how
        // many accounts the run covered, and a provider whose only account is broken must
        // still appear in the answer.
        let counts = counted.entry(conn.provider.clone()).or_default();
        counts.0.extend(seen);
        counts.1.extend(free_ids);
        counts.2 += 1;
        report.accounts.push(entry);
    }

    report.providers = counted
        .into_iter()
        .map(|(provider, (models, free, accounts))| ProviderDiscovery {
            provider,
            accounts,
            models: models.len(),
            free: free.len(),
        })
        .collect();
    Ok(report)
}

/// The models the stored document declares free, as `(provider, model)`.
async fn declared_models(store: &Store) -> rrouter_core::Result<BTreeSet<(String, String)>> {
    let Some(config) = store.free_mode_config().await? else {
        return Ok(BTreeSet::new());
    };
    let mut declared = BTreeSet::new();
    let Some(providers) = config.document.get("providers").and_then(Value::as_array) else {
        return Ok(declared);
    };
    for entry in providers {
        let Some(provider) = entry.get("provider").and_then(Value::as_str) else {
            continue;
        };
        let Some(models) = entry.get("models").and_then(Value::as_array) else {
            continue;
        };
        for model in models {
            if let Some(model) = model
                .as_str()
                .map(str::trim)
                .filter(|model| !model.is_empty())
            {
                declared.insert((provider.to_string(), model.to_string()));
            }
        }
    }
    Ok(declared)
}

/// The client one account's requests must leave through.
///
/// Until `AppState::http_for(creds)` lands from the per-account proxy workstream, every
/// account uses the shared client. This is deliberately the only place that decision is
/// made, so wiring the proxy in touches one line here instead of every call site.
fn egress(state: &AppState, _creds: &Credentials) -> reqwest::Client {
    state.http.clone()
}

/// Fetch one account's model list through that account's credentials.
///
/// Errors are strings because they end up in the per-account row of the report, and they
/// are scrubbed: an upstream body that echoes the key it rejected must not be handed back.
async fn fetch_models(state: &AppState, conn: &Connection) -> Result<Vec<FetchedModel>, String> {
    let Some(spec) = state.provider_spec(&conn.provider) else {
        return Err(format!("unknown provider {}", conn.provider));
    };
    if !models_registry::can_list(&conn.provider) {
        return Err(format!("{} has no model-list endpoint", conn.provider));
    }
    let creds = state
        .refresher
        .fresh(state.store.as_ref(), spec, conn.id)
        .await
        .map_err(|error| scrub(&error.to_string(), &conn.credentials))?;
    let base = creds.base_url_override.as_deref().unwrap_or(spec.base_url);
    let url = match (creds.base_url_override.as_deref(), spec.models_url) {
        // An override points at a different deployment, so its catalogue wins.
        (Some(_), _) => models_registry::models_url(base),
        (None, Some(explicit)) => explicit.to_string(),
        (None, None) => models_registry::models_url(base),
    };
    let headers = provider_headers(spec, &creds)?;
    let response = egress(state, &creds)
        .get(&url)
        .headers(headers)
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|error| {
            if error.is_timeout() {
                "the model list timed out".to_string()
            } else {
                scrub(&error.to_string(), &creds)
            }
        })?;
    let status = response.status().as_u16();
    if !response.status().is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(format!(
            "upstream answered {status}: {}",
            scrub(&body, &creds)
        ));
    }
    let body: Value = response
        .json()
        .await
        .map_err(|error| format!("unreadable model list: {error}"))?;
    Ok(models_registry::parse_models(&body))
}

fn provider_headers(
    spec: &ProviderSpec,
    creds: &Credentials,
) -> Result<axum::http::HeaderMap, String> {
    let mut headers = axum::http::HeaderMap::new();
    for (name, value) in rrouter_auth::build_headers(spec, creds).map_err(|e| e.to_string())? {
        if let (Ok(name), Ok(value)) = (
            axum::http::header::HeaderName::from_bytes(name.as_bytes()),
            axum::http::header::HeaderValue::from_str(&value),
        ) {
            headers.append(name, value);
        }
    }
    Ok(headers)
}

/// A short, secret-free excerpt of an error message.
fn scrub(text: &str, creds: &Credentials) -> String {
    let mut out = text.to_string();
    for secret in [
        creds.api_key.as_deref(),
        creds.access_token.as_deref(),
        creds.refresh_token.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        if !secret.is_empty() {
            out = out.replace(secret, "***");
        }
    }
    out.chars().take(200).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn price(input: f64, output: f64) -> Option<f64> {
        published_price_of(&json!({ "input": input, "output": output }))
    }

    /// The two signals, in the order the contract names them.
    #[test]
    fn an_advertised_free_id_wins_before_any_price() {
        assert_eq!(
            classify_free("openrouter", "deepseek-r1:free", Some(1.5)),
            (true, Some(1.5), SOURCE_PROVIDER_LIST)
        );
        assert_eq!(
            classify_free("openrouter", "llama-3-70b-free", None),
            (true, None, SOURCE_PROVIDER_LIST)
        );
        assert_eq!(
            classify_free("openrouter", "free-gpt-oss", None),
            (true, None, SOURCE_PROVIDER_LIST)
        );
        // Case does not matter, and neither does the provider.
        assert!(classify_free("anything", "MODEL:FREE", None).0);
    }

    /// A zero registry price is free only when it is really zero: an unpublished price is
    /// an unknown price, and guessing would spend paid quota.
    #[test]
    fn only_an_exact_zero_price_is_free_from_the_registry() {
        assert_eq!(
            classify_free("openai", "gpt-4.1-mini", price(0.0, 0.0)),
            (true, Some(0.0), SOURCE_REGISTRY)
        );
        assert_eq!(
            classify_free("openai", "gpt-4.1-mini", price(0.0, 0.4)),
            (false, Some(0.4), SOURCE_PROVIDER_LIST)
        );
        assert_eq!(
            classify_free("openai", "gpt-4.1-mini", None),
            (false, None, SOURCE_PROVIDER_LIST)
        );
        // A blob that publishes no rate at all has no price.
        assert_eq!(published_price_of(&json!({})), None);
        assert_eq!(published_price_of(&json!("free")), None);
        // A cache-only blob still publishes a price: the planner prices it the same way.
        assert_eq!(published_price_of(&json!({ "cache_read": 0.0 })), Some(0.0));
    }

    /// A word that merely contains the letters is not an advertisement.
    #[test]
    fn freeform_is_not_free() {
        assert!(!classify_free("x", "freeform-7b", None).0);
        assert!(!classify_free("x", "freedom", None).0);
        assert!(classify_free("x", "x:free:beta", Some(0.0)).0);
    }

    #[test]
    fn proxy_validation_accepts_the_schemes_the_egress_path_supports() {
        assert!(valid_proxy_url("http://10.0.0.1:8080"));
        assert!(valid_proxy_url("https://user:pass@127.0.0.1:1080"));
        assert!(valid_proxy_url("socks5://10.0.0.2:1080"));
        assert!(valid_proxy_url("socks5h://10.0.0.2:1080"));
        assert!(!valid_proxy_url("ftp://10.0.0.1:21"));
        assert!(!valid_proxy_url("10.0.0.1:8080"));
        assert!(!valid_proxy_url("http://"));
        assert!(!valid_proxy_url("http://host:8080/?q=1"));
    }

    #[test]
    fn redaction_never_leaves_a_secret_or_proxy_userinfo() {
        let document = json!({
            "version": 1,
            "proxies": { "p1": "http://user:pass@10.0.0.1:8080" },
            "providers": [{
                "provider": "p",
                "accounts": [{
                    "label": "a",
                    "api_key": "sk-secret",
                    "access_token": "ya29-secret",
                    "proxy": "http://user:pass@10.0.0.2:8080",
                }],
            }],
        });
        let public = public_document(&document);
        let encoded = serde_json::to_string(&public).expect("serialize");
        assert!(!encoded.contains("sk-secret"));
        assert!(!encoded.contains("ya29-secret"));
        assert!(!encoded.contains("user:pass"));
        assert_eq!(public["providers"][0]["accounts"][0]["has_api_key"], true);
        assert_eq!(
            public["providers"][0]["accounts"][0]["has_access_token"],
            true
        );
        assert_eq!(
            public["providers"][0]["accounts"][0]["proxy"],
            "http://***@10.0.0.2:8080"
        );
        assert_eq!(public["proxies"]["p1"], "http://***@10.0.0.1:8080");

        // Applying it twice is stable, which is what makes it safe on a read path.
        assert_eq!(public_document(&public), public);
    }
}
