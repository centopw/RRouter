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
use std::time::{Duration, Instant};

use rrouter_auth::{AuthStyle, Credentials, ProviderSpec};
use rrouter_core::Error;
use rrouter_router::free_mode::{
    supports_anonymous_free, ANONYMOUS_CONNECTION, SOURCE_PUBLIC_CATALOG,
    SOURCE_PUBLIC_CATALOG_UNUSABLE,
};
use rrouter_store::{Connection, FreeModel, ProviderProfile, Store};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
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

/// Public keyless catalogs auto-discovered by zero-config Free mode.
pub const DEFAULT_OPENCODE_CATALOG_URL: &str = "https://opencode.ai/inference/v1/models";
pub const DEFAULT_OPENROUTER_CATALOG_URL: &str = "https://openrouter.ai/api/v1/models";

/// Anonymous bearer token OpenCode Zen accepts for its public free tier.
pub const OPENCODE_ANONYMOUS_KEY: &str = "public";

/// Refresh interval for cached public catalogs when reading `GET /api/free-mode`.
const CATALOG_TTL_SECS: i64 = 15 * 60;
const CATALOG_FETCH_TIMEOUT: Duration = Duration::from_secs(5);
const PROBE_TIMEOUT: Duration = Duration::from_secs(6);
const PROBE_CONCURRENCY: usize = 5;

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
    let mut routing = document.routing.value();
    // Preserve zero-config toggles, probe cache and public catalog metadata across Advanced
    // document re-imports so importing accounts never wipes live probe or fallback state.
    if let Some(existing) = store.free_mode_config().await? {
        if let (Some(dst), Some(src)) = (routing.as_object_mut(), existing.routing.as_object()) {
            for key in [
                "fallback_on_quota",
                "disabled_models",
                "unusable_models",
                "probes",
                "catalogs",
                "last_checked_at",
                "_meta",
            ] {
                if !dst.contains_key(key) {
                    if let Some(value) = src.get(key) {
                        dst.insert(key.to_string(), value.clone());
                    }
                }
            }
        }
    }
    store
        .save_free_mode_config(&public_document(&stored), &routing)
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
    pub catalogs: Vec<CatalogStatus>,
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
        catalogs: Vec::new(),
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
                    if !free && whole_key_free_model(&conn.provider, &id) {
                        // A whole-key free tier (Groq, Cerebras, Gemini, Mistral) prices
                        // nothing at zero and carries no `:free`/`-free` id marker, so only
                        // the vendored catalog says which of its models are on the free
                        // tier. Widen *only* those rows: a billed key flagged `free_mode`
                        // must never leak a paid model (e.g. gemini-2.5-pro, imagen/veo/tts)
                        // into the free pool.
                        free = true;
                        source = SOURCE_PROVIDER_LIST;
                    }
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
    if should_auto_discover_catalogs(state) {
        let sources: Vec<CatalogSource> = default_catalog_sources()
            .into_iter()
            .filter(|source| only.is_empty() || only.iter().any(|p| p == source.provider))
            .collect();
        if !sources.is_empty() {
            report.catalogs = sync_public_catalogs_from(state, &sources).await?;
        }
    }
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
/// Discovery is an authenticated request on an account's own credential, so it has to leave
/// from that account's address exactly like generation traffic: a provider that sees the
/// catalogue fetched from a different IP than the completions is the inconsistency these
/// accounts get suspended for. One place makes the decision so no call site can forget it.
fn egress(state: &AppState, creds: &Credentials) -> Result<reqwest::Client, String> {
    state
        .http_for(creds)
        .map_err(|error| scrub(&error.to_string(), creds))
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
        .fresh_via(state.store.as_ref(), spec, conn.id, |creds| {
            state.http_for(creds)
        })
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
    let response = egress(state, &creds)?
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

// ---------------------------------------------------------------- zero-config public catalogs & probes

/// One keyless public catalog source that RRouter can discover free models from.
#[derive(Debug, Clone)]
pub struct CatalogSource {
    pub provider: &'static str,
    pub url: String,
}

/// One free model parsed from a public catalog before persistence.
#[derive(Debug, Clone, PartialEq)]
pub struct DiscoveredCatalogModel {
    pub provider: String,
    pub model: String,
    pub display: Option<String>,
    pub endpoint: String,
    pub dialect: String,
    pub usable: bool,
    pub unusable_reason: Option<String>,
    pub auth_mode: String,
    pub chat_only: bool,
    pub price: Option<f64>,
}

/// Per-model catalog metadata stored inside `free_mode.routing._meta`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<String>,
    pub endpoint: String,
    pub dialect: String,
    pub usable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unusable_reason: Option<String>,
    pub auth_mode: String,
    #[serde(default)]
    pub chat_only: bool,
}

/// Outcome of the most recent live probe for one free model.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProbeResult {
    pub ok: bool,
    pub status: u16,
    pub latency_ms: u32,
    pub message: Option<String>,
    pub needs_key: bool,
    pub at: i64,
}

/// Status of one public catalog fetch.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CatalogStatus {
    pub provider: String,
    pub url: String,
    pub discovered_at: i64,
    pub models: usize,
    pub usable: usize,
    pub error: Option<String>,
}

/// Headline counts for the Free mode UI.
#[derive(Debug, Clone, Copy, Default, Serialize, PartialEq, Eq)]
pub struct FreeModeSummary {
    pub total: usize,
    pub usable: usize,
    pub enabled: usize,
    pub working: usize,
    pub unusable: usize,
}

/// One row in `GET /api/free-mode`'s `models` array.
#[derive(Debug, Clone, Serialize)]
pub struct FreeModelView {
    pub provider: String,
    pub provider_display: String,
    pub model: String,
    pub display: Option<String>,
    #[serde(rename = "ref")]
    pub reference: String,
    pub endpoint: String,
    pub dialect: String,
    pub free: bool,
    pub enabled: bool,
    pub usable: bool,
    pub unusable_reason: Option<String>,
    pub auth_mode: String,
    pub has_account: bool,
    pub needs_key: bool,
    pub price: Option<f64>,
    pub source: String,
    pub discovered_at: i64,
    pub probe: Option<ProbeResult>,
}

/// Full `GET /api/free-mode` and `PATCH /api/free-mode` response payload (frozen contract).
#[derive(Debug, Clone, Serialize)]
pub struct FreeModeStateView {
    pub enabled: bool,
    pub fallback_on_quota: bool,
    pub fallback_triggers: [&'static str; 3],
    #[serde(rename = "ref")]
    pub reference: &'static str,
    pub last_checked_at: Option<i64>,
    pub discovered_at: Option<i64>,
    pub summary: FreeModeSummary,
    pub next_fallback_model: Option<String>,
    pub models: Vec<FreeModelView>,
    pub catalogs: Vec<CatalogStatus>,
    pub document: Option<Value>,
    pub routing: Value,
    pub updated_at: Option<i64>,
    pub accounts: Vec<Value>,
}

/// `PATCH /api/free-mode` request body.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PatchSettingsRequest {
    pub enabled: Option<bool>,
    pub fallback_on_quota: Option<bool>,
    pub probe_on_enable: Option<bool>,
}

/// `PATCH /api/free-mode/models` request body.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PatchModelRequest {
    pub provider: String,
    pub model: String,
    pub enabled: bool,
}

/// `PATCH /api/free-mode/models` response body.
#[derive(Debug, Clone, Serialize)]
pub struct PatchModelResponse {
    pub ok: bool,
    pub provider: String,
    pub model: String,
    pub enabled: bool,
    pub summary: FreeModeSummary,
    pub next_fallback_model: Option<String>,
}

/// `POST /api/free-mode/probe` request body.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProbeRequest {
    pub provider: Option<String>,
    pub model: Option<String>,
    #[serde(default)]
    pub all: bool,
    #[serde(default)]
    pub refresh_catalogs: bool,
}

/// One model's probe response (`POST /api/free-mode/probe` single-model mode and batch items).
#[derive(Debug, Clone, Serialize)]
pub struct SingleProbeResponse {
    pub provider: String,
    pub model: String,
    pub endpoint: String,
    pub probe: ProbeResult,
}

/// Batch probe response (`POST /api/free-mode/probe` with `{}` or `{"all": true}`).
#[derive(Debug, Clone, Serialize)]
pub struct BatchProbeResponse {
    pub ok: bool,
    pub checked_at: i64,
    pub results: Vec<SingleProbeResponse>,
}

// ---------------------------------------------------------------- vendored free tiers

/// The vendored FreeLLMAPI catalog, hand-curated per `design/freellmapi-parity.md`.
///
/// It holds **only providers RRouter compiles** and records, per row, the upstream path and
/// SHA it was read from. A row is a *seed*, never an authority over live evidence: the
/// public catalogs (OpenCode, OpenRouter) and an account's own `/v1/models` still decide
/// whether a model exists; this file supplies the free rule and the limits.
const VENDORED_FREE_TIERS: &str = include_str!("../data/free-tiers.json");

/// Provenance of the vendored file, so an operator (or the upstream-diff command) can tell
/// exactly which upstream revision the data was read from.
#[derive(Debug, Clone, Deserialize)]
pub struct VendoredUpstream {
    pub repo: String,
    pub sha: String,
    pub reviewed_at: String,
    pub license: String,
    pub copyright: String,
    pub paths: Vec<String>,
}

/// One provider's vendored free-tier facts.
#[derive(Debug, Clone, Deserialize)]
pub struct VendoredProvider {
    pub id: String,
    pub base_url: String,
    pub auth: String,
    pub free_rule: String,
    pub pool: String,
    #[serde(default)]
    pub provider_caps: Option<VendoredProviderCaps>,
    #[serde(default)]
    pub quota_headers: Map<String, Value>,
    pub tos: String,
    pub src: String,
}

/// Provider-wide request caps a free-tier pool enforces across every model on one account.
#[derive(Debug, Clone, Deserialize)]
pub struct VendoredProviderCaps {
    #[serde(default)]
    pub rpd: Option<u32>,
    /// The lower daily cap for accounts that have bought fewer than the credit threshold.
    #[serde(default)]
    pub rpd_low_credit: Option<u32>,
}

/// One model's vendored limits (all `Option`; an absent limit means "unknown, do not gate").
#[derive(Debug, Clone, Deserialize)]
pub struct VendoredModel {
    pub provider: String,
    pub model: String,
    pub rpm: Option<u32>,
    pub rpd: Option<u32>,
    pub tpm: Option<u32>,
    pub tpd: Option<u32>,
    pub context: Option<u32>,
    pub as_of: String,
    pub src: String,
}

/// The whole vendored catalog, parsed once at module load.
#[derive(Debug, Clone, Deserialize)]
pub struct VendoredFreeTiers {
    pub upstream: VendoredUpstream,
    pub providers: Vec<VendoredProvider>,
    pub models: Vec<VendoredModel>,
}

/// Parse the vendored catalog. A malformed or empty file is a hard error at first use:
/// silent fallback to "no limits" would turn a typo into unexpected paid-quota spend.
pub fn vendored_free_tiers() -> &'static VendoredFreeTiers {
    static TIERS: std::sync::LazyLock<VendoredFreeTiers> = std::sync::LazyLock::new(|| {
        serde_json::from_str(VENDORED_FREE_TIERS).expect("data/free-tiers.json must parse")
    });
    &TIERS
}

/// The vendored `free_rule` for `provider`, or `None` when the provider is not in the file.
///
/// * `"all-models-on-free-key"` — every model a free-mode account lists is free, but only
///   on a free-mode account (Groq, Cerebras, Gemini, Mistral whole-key free tiers).
/// * `"id-suffix:free"` — OpenRouter's `:free` id convention.
pub fn vendored_free_rule(provider: &str) -> Option<&'static str> {
    vendored_free_tiers()
        .providers
        .iter()
        .find(|p| p.id == provider)
        .map(|p| p.free_rule.as_str())
}

/// True when `(provider, model)` is free by a whole-key free-tier rule: the provider
/// (Groq, Cerebras, Gemini, Mistral) is a whole-key free tier **and** the model has a
/// vendored row naming its free-tier limits. Restricting to vendored rows is what keeps a
/// *billed* key flagged `free_mode` from leaking a paid model (gemini-2.5-pro,
/// imagen/veo/tts) into the free pool — an unlisted model is never widened.
pub fn whole_key_free_model(provider: &str, model: &str) -> bool {
    vendored_free_rule(provider) == Some("all-models-on-free-key")
        && vendored_model_limits(provider, model).is_some()
}

/// Vendored per-model limits for `(provider, model)`, or `None` when the file does not seed
/// a row (the caller then treats limits as unknown and does not gate).
///
/// Gemini's OpenAI-compatible `/models` endpoint prefixes model ids with `"models/"`;
/// [`rrouter_router::free_mode::normalize_model_key`] strips that prefix so discovery and
/// runtime lookups resolve the vendored Gemini rows cleanly.
pub fn vendored_model_limits(provider: &str, model: &str) -> Option<&'static VendoredModel> {
    let normalized = rrouter_router::free_mode::normalize_model_key(provider, model);
    vendored_free_tiers()
        .models
        .iter()
        .find(|m| m.provider == provider && m.model == normalized)
}

/// Effective per-model free-tier limits for `(provider, model)`: the vendored baseline
/// tightened (never raised) by anything learned from provider 429/413 error bodies.
pub fn effective_model_limits(
    rotation: &rrouter_router::FreeRotation,
    provider: &str,
    model: &str,
) -> rrouter_router::free_mode::ModelLimits {
    let base = vendored_model_limits(provider, model)
        .map(|row| rrouter_router::free_mode::ModelLimits {
            rpm: row.rpm.map(u64::from),
            rpd: row.rpd.map(u64::from),
            tpm: row.tpm.map(u64::from),
            tpd: row.tpd.map(u64::from),
        })
        .unwrap_or_default();
    let learned = rotation.learned_model_limits(provider, model);
    rrouter_router::free_mode::ModelLimits::merge_tighter(base, learned)
}

/// Rebuild today's UTC-day request and token counters from durable `usage` rows once after
/// boot so a restart does not forget an already-exhausted daily free-tier cap (gap 2).
///
/// Cheap on the steady-state hot path: an in-memory flag short-circuits after the first
/// load, so normal requests pay zero database queries.
pub async fn ensure_rotation_seeded(store: &Store, rotation: &rrouter_router::FreeRotation) {
    if !rotation.needs_usage_seed() {
        return;
    }
    let now = crate::now_unix().max(0) as u64;
    let day_start = rrouter_router::free_mode::utc_day_start(now) as i64;
    let rows = match store
        .usage_summary_by_connection(Some(day_start), None)
        .await
    {
        Ok(rows) => rows,
        Err(err) => {
            tracing::warn!(error = %err, "failed to seed free-mode daily counters from usage");
            return;
        }
    };
    let entries: Vec<(uuid::Uuid, String, String, u32, u64)> = rows
        .into_iter()
        .filter(|row| !row.provider.is_empty())
        .filter_map(|row| {
            let conn = row
                .connection_id
                .unwrap_or(rrouter_router::free_mode::ANONYMOUS_CONNECTION);
            let requests = row.requests.saturating_sub(row.errors).min(u32::MAX as u64) as u32;
            let tokens = row
                .input_tokens
                .saturating_add(row.output_tokens)
                .saturating_add(row.cache_read_tokens)
                .saturating_add(row.cache_write_tokens);
            // `usage.model` stores `"{provider}/{model}"` (see `proxy.rs` `served_model`),
            // whereas live counters key on the bare normalized `candidate.model`. Strip one
            // leading `"{provider}/"` before normalizing so boot seeds match live lookups.
            let stripped = row
                .model
                .strip_prefix(&format!("{}/", row.provider))
                .unwrap_or(&row.model);
            let model =
                rrouter_router::free_mode::normalize_model_key(&row.provider, stripped).to_string();
            (requests > 0 || tokens > 0).then_some((conn, row.provider, model, requests, tokens))
        })
        .collect();
    rotation.seed_from_usage_at(&entries, now);
}

/// The provider-wide daily request cap for `provider` from the vendored file (OpenRouter
/// 1000/day, 50 below the credit threshold), or `None` when the provider has no cap. `low`
/// selects the lower tier for accounts that bought fewer than the credit threshold; the
/// exact tier switch is a later header/key-poll concern, so callers pass it in.
pub fn vendored_provider_daily_cap(provider: &str, low_credit: bool) -> Option<u32> {
    let caps = vendored_free_tiers()
        .providers
        .iter()
        .find(|p| p.id == provider)?
        .provider_caps
        .as_ref()?;
    if low_credit {
        caps.rpd_low_credit.or(caps.rpd)
    } else {
        caps.rpd
    }
}

/// Configured public catalog sources (env-overridable; empty env var disables a source).
pub fn default_catalog_sources() -> Vec<CatalogSource> {
    let mut out = Vec::with_capacity(2);
    let opencode = std::env::var("RROUTER_FREE_OPENCODE_CATALOG_URL")
        .unwrap_or_else(|_| DEFAULT_OPENCODE_CATALOG_URL.to_string());
    if !opencode.trim().is_empty() {
        out.push(CatalogSource {
            provider: "opencode",
            url: opencode.trim().to_string(),
        });
    }
    let openrouter = std::env::var("RROUTER_FREE_OPENROUTER_CATALOG_URL")
        .unwrap_or_else(|_| DEFAULT_OPENROUTER_CATALOG_URL.to_string());
    if !openrouter.trim().is_empty() {
        out.push(CatalogSource {
            provider: "openrouter",
            url: openrouter.trim().to_string(),
        });
    }
    out
}

fn should_auto_discover_catalogs(state: &AppState) -> bool {
    if state.config.load().port != 0 {
        return true;
    }
    std::env::var("RROUTER_FREE_OPENCODE_CATALOG_URL").is_ok()
        || std::env::var("RROUTER_FREE_OPENROUTER_CATALOG_URL").is_ok()
}

fn model_key(provider: &str, model: &str) -> String {
    format!("{provider}/{model}")
}

/// Guard catalog-supplied chat base URLs against SSRF and credential smuggling: only the
/// provider's own https host (with empty userinfo) is accepted. Loopback hosts are accepted
/// only under `cfg(test)` so local wiremock tests can exercise the path without shipping a
/// bypass that a crafted public catalog could use to probe the loopback.
fn allowed_catalog_endpoint(provider: &str, url: &str) -> bool {
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return false;
    };
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return false;
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return false;
    }
    let Some(host) = parsed.host_str() else {
        return false;
    };
    if matches!(host, "127.0.0.1" | "localhost" | "::1") {
        return cfg!(test);
    }
    if parsed.scheme() != "https" {
        return false;
    }
    match provider {
        "opencode" => host == "opencode.ai",
        "openrouter" => host == "openrouter.ai",
        _ => false,
    }
}

/// Parse OpenCode's public `/inference/v1/models` (or `/zen/v1/models`) catalog.
///
/// Only models priced strictly `0` for both text input and text output are free. Models
/// exposing `api["chat-completions"]` are usable OpenAI Chat targets; models that only
/// speak another dialect (e.g. `jev-1.13-free` on `systemone`) are returned with
/// `usable: false` so the UI can disclose them under "Not usable here" while the planner
/// excludes them.
pub fn parse_opencode_catalog(body: &Value) -> Vec<DiscoveredCatalogModel> {
    let Some(entries) = body.get("data").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut out = Vec::new();
    for entry in entries {
        let Some(object) = entry.as_object() else {
            continue;
        };
        let Some(id) = object
            .get("id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|id| !id.is_empty())
        else {
            continue;
        };
        if !seen.insert(id.to_string()) {
            continue;
        }
        let input_price = entry
            .pointer("/cost/text/input/price")
            .and_then(Value::as_f64);
        let output_price = entry
            .pointer("/cost/text/output/price")
            .and_then(Value::as_f64);
        if input_price != Some(0.0) || output_price != Some(0.0) {
            continue;
        }
        let display = object
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let apis = object.get("api").and_then(Value::as_object);
        let recommended = object
            .get("recommended_api")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty());

        let (endpoint, dialect, usable, unusable_reason, chat_only) = if let Some(chat) =
            apis.and_then(|map| map.get("chat-completions"))
        {
            let raw_base = chat
                .get("base_url")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or("https://opencode.ai/inference/openai/v1");
            let candidate_url = if raw_base.ends_with("/chat/completions") {
                raw_base.to_string()
            } else {
                format!("{}/chat/completions", raw_base.trim_end_matches('/'))
            };
            let endpoint = if allowed_catalog_endpoint("opencode", &candidate_url) {
                candidate_url
            } else {
                "https://opencode.ai/inference/openai/v1/chat/completions".to_string()
            };
            let chat_only = apis.is_some_and(|map| !map.contains_key("responses"));
            (endpoint, "openai_chat".to_string(), true, None, chat_only)
        } else {
            let dialect = recommended
                .map(str::to_string)
                .or_else(|| apis.and_then(|map| map.keys().next().cloned()))
                .unwrap_or_else(|| "unknown".to_string());
            let endpoint = apis
                .and_then(|map| map.get(&dialect))
                .and_then(|v| v.get("base_url"))
                .and_then(Value::as_str)
                .map(str::trim)
                .unwrap_or_default()
                .to_string();
            let reason = format!("Speaks the {dialect} dialect, which RRouter does not support");
            (endpoint, dialect, false, Some(reason), false)
        };

        out.push(DiscoveredCatalogModel {
            provider: "opencode".to_string(),
            model: id.to_string(),
            display,
            endpoint,
            dialect,
            usable,
            unusable_reason,
            auth_mode: "key_or_anonymous".to_string(),
            chat_only,
            price: Some(0.0),
        });
    }
    out
}

/// Parse OpenRouter's public `/api/v1/models` catalog for `:free` / zero-priced models.
pub fn parse_openrouter_catalog(body: &Value) -> Vec<DiscoveredCatalogModel> {
    let Some(entries) = body.get("data").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut out = Vec::new();
    for entry in entries {
        let Some(object) = entry.as_object() else {
            continue;
        };
        let Some(id) = object
            .get("id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|id| !id.is_empty())
        else {
            continue;
        };
        if !seen.insert(id.to_string()) {
            continue;
        }
        let price_val = |v: &Value| -> Option<f64> {
            v.as_f64()
                .or_else(|| v.as_str().and_then(|s| s.trim().parse::<f64>().ok()))
        };
        let prompt = entry.pointer("/pricing/prompt").and_then(price_val);
        let completion = entry.pointer("/pricing/completion").and_then(price_val);
        let zero_priced = prompt == Some(0.0) && completion == Some(0.0);
        let free_suffix = id.to_ascii_lowercase().ends_with(":free");
        if !free_suffix && !zero_priced {
            continue;
        }
        let display = object
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        out.push(DiscoveredCatalogModel {
            provider: "openrouter".to_string(),
            model: id.to_string(),
            display,
            endpoint: "https://openrouter.ai/api/v1/chat/completions".to_string(),
            dialect: "openai_chat".to_string(),
            usable: true,
            unusable_reason: None,
            auth_mode: "key_required".to_string(),
            chat_only: true,
            price: Some(0.0),
        });
    }
    out
}

/// Fetch and persist the default public catalogs (`opencode`, `openrouter`).
pub async fn sync_public_catalogs(state: &AppState) -> rrouter_core::Result<Vec<CatalogStatus>> {
    let sources = default_catalog_sources();
    sync_public_catalogs_from(state, &sources).await
}

/// Fetch and persist the given public catalog sources. Public so wiremock tests can point
/// discovery at local mock HTTP servers without mutating process-wide environment variables.
pub async fn sync_public_catalogs_from(
    state: &AppState,
    sources: &[CatalogSource],
) -> rrouter_core::Result<Vec<CatalogStatus>> {
    let now = crate::now_unix();
    let mut statuses: Vec<CatalogStatus> = Vec::with_capacity(sources.len());
    let mut new_meta: BTreeMap<String, ModelMeta> = BTreeMap::new();
    let mut unusable_keys: BTreeSet<String> = BTreeSet::new();

    for source in sources {
        let mut status = CatalogStatus {
            provider: source.provider.to_string(),
            url: source.url.clone(),
            discovered_at: now,
            models: 0,
            usable: 0,
            error: None,
        };
        let fetched = state
            .http
            .get(&source.url)
            .header(axum::http::header::USER_AGENT, "rrouter-free-mode/1.0")
            .timeout(CATALOG_FETCH_TIMEOUT)
            .send()
            .await;
        let response = match fetched {
            Ok(resp) => resp,
            Err(err) => {
                status.error = Some(if err.is_timeout() {
                    "catalog request timed out".to_string()
                } else {
                    err.without_url().to_string()
                });
                statuses.push(status);
                continue;
            }
        };
        if !response.status().is_success() {
            status.error = Some(format!("catalog answered {}", response.status().as_u16()));
            statuses.push(status);
            continue;
        }
        let body: Value = match response.json().await {
            Ok(body) => body,
            Err(err) => {
                status.error = Some(format!("unreadable catalog JSON: {err}"));
                statuses.push(status);
                continue;
            }
        };
        let parsed = match source.provider {
            "opencode" => parse_opencode_catalog(&body),
            "openrouter" => parse_openrouter_catalog(&body),
            _ => Vec::new(),
        };
        status.models = parsed.len();
        status.usable = parsed.iter().filter(|m| m.usable).count();

        // A 200 whose body reshaped or emptied (zero parsed models) is a fetch failure, not
        // "the provider now has no free models": never delete the existing pool on it.
        if parsed.is_empty() {
            status.error = Some("catalog answered 200 but parsed to zero models".to_string());
            statuses.push(status);
            continue;
        }

        // Build the new public-catalog rows and replace the provider's stale ones atomically.
        // Document/account rows for this provider are untouched.
        let rows: Vec<FreeModel> = parsed
            .iter()
            .map(|item| FreeModel {
                provider: item.provider.clone(),
                model: item.model.clone(),
                free: item.usable,
                price: item.price,
                source: if item.usable {
                    SOURCE_PUBLIC_CATALOG.to_string()
                } else {
                    SOURCE_PUBLIC_CATALOG_UNUSABLE.to_string()
                },
                connection_id: None,
                discovered_at: now,
            })
            .collect();
        state
            .store
            .sync_public_catalog(source.provider, &rows)
            .await?;

        for item in parsed {
            let key = model_key(&item.provider, &item.model);
            if !item.usable {
                unusable_keys.insert(key.clone());
            }
            new_meta.insert(
                key,
                ModelMeta {
                    display: item.display,
                    endpoint: item.endpoint,
                    dialect: item.dialect,
                    usable: item.usable,
                    unusable_reason: item.unusable_reason,
                    auth_mode: item.auth_mode,
                    chat_only: item.chat_only,
                },
            );
        }
        statuses.push(status);
    }

    // Merge updated catalog statuses, metadata, and unusable keys into `free_mode.routing`
    // without touching `enabled`, `fallback_on_quota`, `disabled_models`, `probes`, or any
    // imported document.
    let (doc, mut routing) = load_config_parts(state.store.as_ref()).await?;
    {
        let obj = ensure_object(&mut routing);
        // Merge `_meta` per model key.
        let mut meta_map = obj
            .get("_meta")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        for (key, meta) in new_meta {
            if let Ok(val) = serde_json::to_value(meta) {
                meta_map.insert(key, val);
            }
        }
        obj.insert("_meta".to_string(), Value::Object(meta_map));

        // A provider whose fetch succeeded (`error` is None) refreshes its `unusable_models`
        // entries and its catalog status. A failed fetch must not overwrite a previous good
        // status (`models` -> 0) nor clear the provider's existing `unusable_models`.
        let succeeded: BTreeSet<&str> = statuses
            .iter()
            .filter(|st| st.error.is_none())
            .map(|st| st.provider.as_str())
            .collect();
        let mut unusable: BTreeSet<String> = obj
            .get("unusable_models")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(Value::as_str)
                    .filter(|entry| {
                        entry
                            .split_once('/')
                            .is_none_or(|(p, _)| !succeeded.contains(p))
                    })
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        unusable.extend(unusable_keys);
        obj.insert(
            "unusable_models".to_string(),
            json!(unusable.into_iter().collect::<Vec<_>>()),
        );

        // Merge `catalogs` per provider. Only a successful fetch may replace an existing
        // status; a failed one records its error only when no prior status exists.
        let mut catalog_map: BTreeMap<String, CatalogStatus> = obj
            .get("catalogs")
            .and_then(|v| serde_json::from_value::<Vec<CatalogStatus>>(v.clone()).ok())
            .unwrap_or_default()
            .into_iter()
            .map(|c| (c.provider.clone(), c))
            .collect();
        for st in &statuses {
            if st.error.is_none() {
                catalog_map.insert(st.provider.clone(), st.clone());
            } else {
                catalog_map
                    .entry(st.provider.clone())
                    .or_insert_with(|| st.clone());
            }
        }
        obj.insert(
            "catalogs".to_string(),
            serde_json::to_value(catalog_map.into_values().collect::<Vec<_>>())?,
        );
    }
    state.store.save_free_mode_config(&doc, &routing).await?;
    Ok(statuses)
}

/// Keep public free-model catalogs fresh in the background: initial pass shortly after
/// startup, then every 30 minutes. Failures are logged, never fatal.
pub fn spawn_catalog_sync(state: Arc<AppState>) {
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(2)).await;
        loop {
            match sync_public_catalogs(&state).await {
                Ok(statuses) => {
                    let total: usize = statuses.iter().map(|s| s.usable).sum();
                    tracing::info!(
                        catalogs = statuses.len(),
                        usable_models = total,
                        "free-mode public catalogs synced"
                    );
                }
                Err(error) => tracing::warn!(%error, "free-mode public catalog sync failed"),
            }
            tokio::time::sleep(Duration::from_secs(30 * 60)).await;
        }
    });
}

/// Read the stored `(document, routing)` pair, initializing zero-config defaults when no
/// row has been written yet.
async fn load_config_parts(store: &Store) -> rrouter_core::Result<(Value, Value)> {
    match store.free_mode_config().await? {
        Some(cfg) => Ok((cfg.document, cfg.routing)),
        None => Ok((
            Value::Null,
            json!({
                "enabled": false,
                "fallback_on_quota": false,
                "per_account_rpm": null,
                "max_attempts": DEFAULT_MAX_ATTEMPTS,
                "prefer": DEFAULT_PREFER,
                "disabled_models": [],
            }),
        )),
    }
}

fn ensure_object(value: &mut Value) -> &mut Map<String, Value> {
    if !value.is_object() {
        *value = Value::Object(Map::new());
    }
    value.as_object_mut().expect("object initialized above")
}

/// Whether a stored `free_mode.document` represents a real imported document (versus a
/// zero-config row created by catalog discovery or toggles).
fn has_imported_document(doc: &Value) -> bool {
    doc.is_object() && doc.get("providers").is_some()
}

/// Public-facing `routing` object (internal metadata/probe caches stripped).
fn public_routing(routing: &Value, enabled: bool, fallback_on_quota: bool) -> Value {
    let mut out = Map::new();
    out.insert("enabled".to_string(), Value::Bool(enabled));
    out.insert(
        "fallback_on_quota".to_string(),
        Value::Bool(fallback_on_quota),
    );
    out.insert(
        "per_account_rpm".to_string(),
        routing
            .get("per_account_rpm")
            .cloned()
            .unwrap_or(Value::Null),
    );
    out.insert(
        "max_attempts".to_string(),
        routing
            .get("max_attempts")
            .cloned()
            .unwrap_or_else(|| json!(DEFAULT_MAX_ATTEMPTS)),
    );
    out.insert(
        "prefer".to_string(),
        routing
            .get("prefer")
            .cloned()
            .unwrap_or_else(|| json!(DEFAULT_PREFER)),
    );
    if let Some(models) = routing.get("models") {
        out.insert("models".to_string(), models.clone());
    }
    let disabled = routing
        .get("disabled_models")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    out.insert("disabled_models".to_string(), Value::Array(disabled));
    Value::Object(out)
}

/// Build the full `GET /api/free-mode` state view.
pub async fn state_view(
    state: &AppState,
    refresh: bool,
) -> rrouter_core::Result<FreeModeStateView> {
    let catalogs_stale = {
        let cfg = state.store.free_mode_config().await?;
        let now = crate::now_unix();
        match cfg
            .as_ref()
            .and_then(|c| c.routing.get("catalogs"))
            .and_then(|v| serde_json::from_value::<Vec<CatalogStatus>>(v.clone()).ok())
        {
            Some(list) if !list.is_empty() => list
                .iter()
                .all(|c| now.saturating_sub(c.discovered_at) >= CATALOG_TTL_SECS),
            _ => true,
        }
    };
    if refresh || (catalogs_stale && should_auto_discover_catalogs(state)) {
        if let Err(err) = sync_public_catalogs(state).await {
            tracing::warn!(%err, "free-mode catalog refresh failed; returning cached view");
        }
    }
    build_state_view(state).await
}

async fn build_state_view(state: &AppState) -> rrouter_core::Result<FreeModeStateView> {
    let config = state.store.free_mode_config().await?;
    let rows = state.store.list_free_models().await?;
    let connections = state.store.list_connections(None).await?;

    let routing_raw = config
        .as_ref()
        .map(|c| c.routing.clone())
        .unwrap_or_else(|| json!({}));
    let enabled = routing_raw
        .get("enabled")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let fallback_on_quota = routing_raw
        .get("fallback_on_quota")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let disabled_set: BTreeSet<String> = routing_raw
        .get("disabled_models")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let meta_map: BTreeMap<String, ModelMeta> = routing_raw
        .get("_meta")
        .and_then(Value::as_object)
        .map(|map| {
            map.iter()
                .filter_map(|(k, v)| {
                    serde_json::from_value::<ModelMeta>(v.clone())
                        .ok()
                        .map(|m| (k.clone(), m))
                })
                .collect()
        })
        .unwrap_or_default();
    let probe_map: BTreeMap<String, ProbeResult> = routing_raw
        .get("probes")
        .and_then(Value::as_object)
        .map(|map| {
            map.iter()
                .filter_map(|(k, v)| {
                    serde_json::from_value::<ProbeResult>(v.clone())
                        .ok()
                        .map(|p| (k.clone(), p))
                })
                .collect()
        })
        .unwrap_or_default();
    let catalogs: Vec<CatalogStatus> = routing_raw
        .get("catalogs")
        .and_then(|v| serde_json::from_value::<Vec<CatalogStatus>>(v.clone()).ok())
        .unwrap_or_default();
    let last_checked_at = routing_raw
        .get("last_checked_at")
        .and_then(Value::as_i64)
        .or_else(|| probe_map.values().map(|p| p.at).max());

    let active_free_providers: BTreeSet<&str> = connections
        .iter()
        .filter(|c| c.is_active && c.credentials.free_mode)
        .map(|c| c.provider.as_str())
        .collect();
    let active_any_providers: BTreeSet<&str> = connections
        .iter()
        .filter(|c| c.is_active)
        .map(|c| c.provider.as_str())
        .collect();

    let mut models: Vec<FreeModelView> = Vec::new();
    for row in &rows {
        let is_unusable_catalog = row.source == SOURCE_PUBLIC_CATALOG_UNUSABLE;
        if !row.free && !is_unusable_catalog {
            continue;
        }
        let key = model_key(&row.provider, &row.model);
        let meta = meta_map.get(&key);
        let spec = state.provider_spec(&row.provider);
        let provider_display = spec
            .map(|s| s.display.to_string())
            .unwrap_or_else(|| row.provider.clone());
        let default_auth_mode = if supports_anonymous_free(&row.provider) {
            "key_or_anonymous"
        } else if spec.is_some_and(|s| s.auth == AuthStyle::None) {
            "anonymous"
        } else {
            "key_required"
        };
        let auth_mode = meta
            .map(|m| m.auth_mode.clone())
            .unwrap_or_else(|| default_auth_mode.to_string());
        let endpoint = meta
            .map(|m| m.endpoint.clone())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| spec.map(|s| s.base_url.to_string()).unwrap_or_default());
        let dialect = meta.map(|m| m.dialect.clone()).unwrap_or_else(|| {
            spec.map(|s| s.default_dialect.as_str().to_string())
                .unwrap_or_else(|| "openai_chat".to_string())
        });
        let usable = !is_unusable_catalog && meta.is_none_or(|m| m.usable);
        let unusable_reason = if usable {
            None
        } else {
            meta.and_then(|m| m.unusable_reason.clone())
                .or_else(|| Some("Uses a dialect RRouter does not support".to_string()))
        };
        let is_public = matches!(
            row.source.as_str(),
            SOURCE_PUBLIC_CATALOG | SOURCE_PUBLIC_CATALOG_UNUSABLE
        );
        let has_account = if is_public {
            active_any_providers.contains(row.provider.as_str())
        } else {
            active_free_providers.contains(row.provider.as_str())
        };
        let probe = probe_map.get(&key).cloned();
        let probe_needs_key = probe.as_ref().is_some_and(|p| p.needs_key);
        let needs_key =
            (!has_account && auth_mode == "key_required") || (!has_account && probe_needs_key);
        let model_enabled =
            usable && !disabled_set.contains(&key) && !disabled_set.contains(&row.model);

        models.push(FreeModelView {
            provider: row.provider.clone(),
            provider_display,
            model: row.model.clone(),
            display: meta.and_then(|m| m.display.clone()),
            reference: format!("free/{}", row.model),
            endpoint,
            dialect,
            free: true,
            enabled: model_enabled,
            usable,
            unusable_reason,
            auth_mode,
            has_account,
            needs_key,
            price: row.price,
            source: if is_unusable_catalog {
                SOURCE_PUBLIC_CATALOG.to_string()
            } else {
                row.source.clone()
            },
            discovered_at: row.discovered_at,
            probe,
        });
    }

    let summary = summarize_models(&models);
    let next_fallback_model = pick_next_fallback_model(&models);
    let discovered_at = models.iter().map(|m| m.discovered_at).max();

    let accounts: Vec<Value> = connections
        .iter()
        .filter(|conn| conn.credentials.free_mode)
        .map(|conn| {
            json!({
                "connection_id": conn.id.to_string(),
                "provider": conn.provider.clone(),
                "label": conn
                    .credentials
                    .account_label
                    .clone()
                    .or_else(|| conn.name.clone()),
                "proxy": conn.credentials.proxy_masked(),
                "free_rpm": conn.credentials.free_rpm,
                "is_active": conn.is_active,
                "last_error": conn.last_error.clone(),
            })
        })
        .collect();

    let document = config
        .as_ref()
        .filter(|cfg| has_imported_document(&cfg.document))
        .map(|cfg| public_document(&cfg.document));

    Ok(FreeModeStateView {
        enabled,
        fallback_on_quota,
        fallback_triggers: ["quota", "429", "402"],
        reference: "free",
        last_checked_at,
        discovered_at,
        summary,
        next_fallback_model,
        models,
        catalogs,
        document,
        routing: public_routing(&routing_raw, enabled, fallback_on_quota),
        updated_at: config.as_ref().map(|c| c.updated_at),
        accounts,
    })
}

fn summarize_models(models: &[FreeModelView]) -> FreeModeSummary {
    let mut s = FreeModeSummary {
        total: models.len(),
        ..FreeModeSummary::default()
    };
    for m in models {
        if m.usable {
            s.usable += 1;
            if m.enabled {
                s.enabled += 1;
            }
            if m.probe.as_ref().is_some_and(|p| p.ok) {
                s.working += 1;
            }
        } else {
            s.unusable += 1;
        }
    }
    s
}

/// Choose the first model that automatic fallback would pick right now (matching the
/// planner's working-fastest-first ordering).
fn pick_next_fallback_model(models: &[FreeModelView]) -> Option<String> {
    let mut candidates: Vec<&FreeModelView> = models
        .iter()
        .filter(|m| m.usable && m.enabled && (m.has_account || m.auth_mode != "key_required"))
        .collect();
    candidates.sort_by_key(|m| match &m.probe {
        Some(p) if p.ok => (0u8, p.latency_ms),
        None => (1u8, u32::MAX),
        Some(p) => (2u8, p.latency_ms),
    });
    candidates.first().map(|m| m.model.clone())
}

/// Apply `PATCH /api/free-mode` (zero-config toggles). When `enabled` flips `false -> true`
/// and `probe_on_enable` is `true` (default), probes enabled usable models concurrently,
/// detached from this answer.
pub async fn patch_settings(
    state: &Arc<AppState>,
    req: PatchSettingsRequest,
) -> rrouter_core::Result<FreeModeStateView> {
    let (doc, mut routing) = load_config_parts(state.store.as_ref()).await?;
    let was_enabled = routing
        .get("enabled")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    {
        let obj = ensure_object(&mut routing);
        if let Some(enabled) = req.enabled {
            obj.insert("enabled".to_string(), Value::Bool(enabled));
        }
        if let Some(fallback) = req.fallback_on_quota {
            obj.insert("fallback_on_quota".to_string(), Value::Bool(fallback));
        }
    }
    state.store.save_free_mode_config(&doc, &routing).await?;

    let now_enabled = routing
        .get("enabled")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let view = state_view(state, false).await?;
    let should_probe = !was_enabled
        && now_enabled
        && req.probe_on_enable.unwrap_or(true)
        // Nothing discovered yet (a fresh instance, or after teardown): there is no model
        // to probe, and the catalogs get discovered by the next state read anyway.
        && view.summary.enabled > 0;
    if should_probe {
        // The pool IS on after the save above; a probe failure is a diagnostic, not a
        // reason to answer the PATCH with an error. The probes are live upstream sends with
        // their own timeouts (a whole free pool is tens of seconds), so they run detached:
        // the toggle itself is a store write and must answer immediately. Nothing in this
        // answer reads probe state, so the caller never waits for them.
        let state = Arc::clone(state);
        let summary = view.summary;
        tokio::spawn(async move {
            tracing::info!(
                enabled = summary.enabled,
                "free-mode pool enabled; probing in the background"
            );
            if let Err(err) = probe_all(&state, false).await {
                tracing::warn!(error = %err, "free-mode probe after enable failed");
            }
        });
    }
    Ok(view)
}

/// Apply `PATCH /api/free-mode/models` to enable or disable one discovered model.
pub async fn patch_model(
    state: &AppState,
    req: PatchModelRequest,
) -> rrouter_core::Result<PatchModelResponse> {
    let raw_provider = req.provider.trim();
    let model = req.model.trim();
    if raw_provider.is_empty() || model.is_empty() {
        return Err(Error::BadRequest(
            "provider and model are required".to_string(),
        ));
    }
    let provider = state
        .provider_by_ref(raw_provider)
        .unwrap_or_else(|| raw_provider.to_string());

    let view = build_state_view(state).await?;
    let Some(target) = view
        .models
        .iter()
        .find(|m| m.provider == provider && m.model == model)
    else {
        return Err(Error::NotFound(format!(
            "free model {provider}/{model} not found"
        )));
    };
    if req.enabled && !target.usable {
        return Err(Error::BadRequest(
            target
                .unusable_reason
                .clone()
                .unwrap_or_else(|| format!("{provider}/{model} is not usable on this gateway")),
        ));
    }

    let key = model_key(&provider, model);
    let (doc, mut routing) = load_config_parts(state.store.as_ref()).await?;
    {
        let obj = ensure_object(&mut routing);
        let mut disabled: BTreeSet<String> = obj
            .get("disabled_models")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        if req.enabled {
            disabled.remove(&key);
            disabled.remove(model);
        } else {
            disabled.insert(key);
        }
        obj.insert(
            "disabled_models".to_string(),
            json!(disabled.into_iter().collect::<Vec<_>>()),
        );
    }
    state.store.save_free_mode_config(&doc, &routing).await?;

    let updated = build_state_view(state).await?;
    Ok(PatchModelResponse {
        ok: true,
        provider,
        model: model.to_string(),
        enabled: req.enabled,
        summary: updated.summary,
        next_fallback_model: updated.next_fallback_model,
    })
}

/// Probe one discovered free model right now and persist the outcome.
pub async fn probe_one(
    state: &AppState,
    raw_provider: &str,
    model: &str,
) -> rrouter_core::Result<SingleProbeResponse> {
    let raw_provider = raw_provider.trim();
    let model = model.trim();
    if raw_provider.is_empty() || model.is_empty() {
        return Err(Error::BadRequest(
            "provider and model are required".to_string(),
        ));
    }
    let provider = state
        .provider_by_ref(raw_provider)
        .unwrap_or_else(|| raw_provider.to_string());

    let view = build_state_view(state).await?;
    let Some(target) = view
        .models
        .iter()
        .find(|m| m.provider == provider && m.model == model)
        .cloned()
    else {
        return Err(Error::NotFound(format!(
            "free model {provider}/{model} not found"
        )));
    };

    let connections = state.store.list_connections(Some(&provider)).await?;
    let outcome = run_single_probe(state, &target, &connections).await;
    persist_probes(
        state.store.as_ref(),
        &[(model_key(&provider, model), outcome.probe.clone())],
        outcome.probe.at,
    )
    .await?;
    Ok(outcome)
}

/// Probe every enabled usable free model concurrently (bounded by [`PROBE_CONCURRENCY`]).
pub async fn probe_all(
    state: &AppState,
    refresh_catalogs: bool,
) -> rrouter_core::Result<BatchProbeResponse> {
    if refresh_catalogs {
        let _ = sync_public_catalogs(state).await?;
    }
    let view = build_state_view(state).await?;
    let connections = state.store.list_connections(None).await?;
    let targets: Vec<FreeModelView> = view
        .models
        .into_iter()
        .filter(|m| m.usable && m.enabled)
        .collect();

    let sem = Arc::new(tokio::sync::Semaphore::new(PROBE_CONCURRENCY));
    let mut handles = Vec::with_capacity(targets.len());
    for target in targets {
        let permit = Arc::clone(&sem)
            .acquire_owned()
            .await
            .map_err(|e| Error::Store(format!("probe semaphore closed: {e}")))?;
        let provider_conns: Vec<Connection> = connections
            .iter()
            .filter(|c| c.provider == target.provider)
            .cloned()
            .collect();
        handles.push(async move {
            let _permit = permit;
            run_single_probe(state, &target, &provider_conns).await
        });
    }
    let results = futures_util::future::join_all(handles).await;
    let checked_at = crate::now_unix();
    let pairs: Vec<(String, ProbeResult)> = results
        .iter()
        .map(|r| (model_key(&r.provider, &r.model), r.probe.clone()))
        .collect();
    persist_probes(state.store.as_ref(), &pairs, checked_at).await?;

    Ok(BatchProbeResponse {
        ok: true,
        checked_at,
        results,
    })
}

async fn persist_probes(
    store: &Store,
    entries: &[(String, ProbeResult)],
    checked_at: i64,
) -> rrouter_core::Result<()> {
    let (doc, mut routing) = load_config_parts(store).await?;
    {
        let obj = ensure_object(&mut routing);
        let mut probes = obj
            .get("probes")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        for (key, probe) in entries {
            if let Ok(val) = serde_json::to_value(probe) {
                probes.insert(key.clone(), val);
            }
        }
        obj.insert("probes".to_string(), Value::Object(probes));
        obj.insert("last_checked_at".to_string(), json!(checked_at));
    }
    store.save_free_mode_config(&doc, &routing).await?;
    Ok(())
}

async fn run_single_probe(
    state: &AppState,
    target: &FreeModelView,
    provider_conns: &[Connection],
) -> SingleProbeResponse {
    let at = crate::now_unix();
    if !target.usable {
        return SingleProbeResponse {
            provider: target.provider.clone(),
            model: target.model.clone(),
            endpoint: target.endpoint.clone(),
            probe: ProbeResult {
                ok: false,
                status: 400,
                latency_ms: 0,
                message: target.unusable_reason.clone(),
                needs_key: false,
                at,
            },
        };
    }

    let active_free = provider_conns
        .iter()
        .find(|c| c.is_active && c.credentials.free_mode);
    let active_normal = (target.source == SOURCE_PUBLIC_CATALOG)
        .then(|| {
            provider_conns
                .iter()
                .find(|c| c.is_active && !c.credentials.free_mode)
        })
        .flatten();
    let chosen_conn = active_free.or(active_normal);
    let has_account = chosen_conn.is_some();

    // Anonymous dispatch reuses the exact guard the proxy uses: only a verified free
    // public-catalog row on an anonymous-capable provider yields an endpoint. A probe that
    // cannot be anonymous (paid row, unsupported provider, no verified endpoint) falls
    // through to the "add a key" answer instead of fabricating a `Bearer public` send.
    let anonymous_endpoint = if chosen_conn.is_none() {
        anonymous_endpoint_for(state.store.as_ref(), &target.provider, &target.model).await
    } else {
        None
    };

    let (http, mut headers, url, creds_for_scrub, conn_id_for_breaker) = match chosen_conn {
        Some(conn) => {
            let Some(spec) = state.provider_spec(&conn.provider) else {
                return SingleProbeResponse {
                    provider: target.provider.clone(),
                    model: target.model.clone(),
                    endpoint: target.endpoint.clone(),
                    probe: ProbeResult {
                        ok: false,
                        status: 400,
                        latency_ms: 0,
                        message: Some(format!("unknown provider {}", conn.provider)),
                        needs_key: false,
                        at,
                    },
                };
            };
            let creds = match state
                .refresher
                .fresh_via(state.store.as_ref(), spec, conn.id, |c| state.http_for(c))
                .await
            {
                Ok(creds) => creds,
                Err(err) => {
                    return SingleProbeResponse {
                        provider: target.provider.clone(),
                        model: target.model.clone(),
                        endpoint: target.endpoint.clone(),
                        probe: ProbeResult {
                            ok: false,
                            status: err.status(),
                            latency_ms: 0,
                            message: Some(scrub(&err.to_string(), &conn.credentials)),
                            needs_key: err.status() == 401,
                            at,
                        },
                    };
                }
            };
            let http = match egress(state, &creds) {
                Ok(client) => client,
                Err(msg) => {
                    return SingleProbeResponse {
                        provider: target.provider.clone(),
                        model: target.model.clone(),
                        endpoint: target.endpoint.clone(),
                        probe: ProbeResult {
                            ok: false,
                            status: 502,
                            latency_ms: 0,
                            message: Some(msg),
                            needs_key: false,
                            at,
                        },
                    };
                }
            };
            let headers = match provider_headers(spec, &creds) {
                Ok(h) => h,
                Err(msg) => {
                    return SingleProbeResponse {
                        provider: target.provider.clone(),
                        model: target.model.clone(),
                        endpoint: target.endpoint.clone(),
                        probe: ProbeResult {
                            ok: false,
                            status: 401,
                            latency_ms: 0,
                            message: Some(msg),
                            needs_key: true,
                            at,
                        },
                    };
                }
            };
            let url = if let Some(over) = creds.base_url_override.as_deref() {
                crate::proxy::endpoint_for(spec, Some(over), rrouter_core::Dialect::OpenAiChat).0
            } else if !target.endpoint.is_empty() {
                target.endpoint.clone()
            } else {
                spec.base_url.to_string()
            };
            (http, headers, url, Some(creds), conn.id)
        }
        None if anonymous_endpoint.is_some() => {
            let mut headers = axum::http::HeaderMap::new();
            headers.insert(
                axum::http::header::AUTHORIZATION,
                axum::http::HeaderValue::from_static("Bearer public"),
            );
            // `anonymous_endpoint_for` already verified the endpoint against
            // `allowed_catalog_endpoint`, so this URL is safe to use verbatim.
            let url = anonymous_endpoint.expect("checked Some above");
            (state.http.clone(), headers, url, None, ANONYMOUS_CONNECTION)
        }
        None => {
            return SingleProbeResponse {
                provider: target.provider.clone(),
                model: target.model.clone(),
                endpoint: target.endpoint.clone(),
                probe: ProbeResult {
                    ok: false,
                    status: 401,
                    latency_ms: 0,
                    message: Some(format!(
                        "Add a {} API key on Providers to use its free models",
                        target.provider_display
                    )),
                    needs_key: true,
                    at,
                },
            };
        }
    };

    headers
        .entry(axum::http::header::CONTENT_TYPE)
        .or_insert(axum::http::HeaderValue::from_static("application/json"));
    headers
        .entry(axum::http::header::USER_AGENT)
        .or_insert(axum::http::HeaderValue::from_static(
            "rrouter-free-mode/1.0",
        ));
    if matches!(target.provider.as_str(), "opencode" | "opencode-go") {
        let session = crate::proxy::opencode_session(None, 0xF4EE_0001);
        if let Ok(val) = axum::http::HeaderValue::from_str(&session) {
            headers.insert("x-opencode-session", val);
        }
    }

    let body = json!({
        "model": target.model,
        "messages": [{"role": "user", "content": "ping"}],
        "max_tokens": 16,
        "stream": false,
    });
    let started = Instant::now();
    let resp = http
        .post(&url)
        .headers(headers)
        .json(&body)
        .timeout(PROBE_TIMEOUT)
        .send()
        .await;
    let latency_ms = started.elapsed().as_millis().min(u32::MAX as u128) as u32;

    match resp {
        Ok(resp) => {
            let status = resp.status().as_u16();
            let retry_after = retry_after_header_secs(resp.headers());
            let norm_model =
                rrouter_router::free_mode::normalize_model_key(&target.provider, &target.model);
            if resp.status().is_success() {
                state
                    .dispatcher
                    .breaker()
                    .record_success_for(conn_id_for_breaker, &target.model);
                state
                    .planner
                    .free_rotation()
                    .clear_provider_limit(conn_id_for_breaker, &target.provider);
                state
                    .planner
                    .free_rotation()
                    .clear_model_daily_park(conn_id_for_breaker, norm_model);
                SingleProbeResponse {
                    provider: target.provider.clone(),
                    model: target.model.clone(),
                    endpoint: url,
                    probe: ProbeResult {
                        ok: true,
                        status,
                        latency_ms,
                        message: None,
                        needs_key: false,
                        at,
                    },
                }
            } else {
                let text = resp.text().await.unwrap_or_default();
                if matches!(status, 429 | 413) {
                    if let Some((kind, limit)) = learn_limit_from_error(&text) {
                        state.planner.free_rotation().learn_model_limit(
                            &target.provider,
                            &target.model,
                            kind,
                            limit,
                        );
                    }
                }
                let free_limit = is_free_usage_limit_error(status, &text);
                if free_limit {
                    let wait = free_cooldown_duration(status, retry_after, &text, at);
                    state.planner.free_rotation().record_provider_limit(
                        conn_id_for_breaker,
                        &target.provider,
                        wait,
                    );
                } else if is_daily_exhaustion(status, &text) {
                    let wait = daily_park_duration(retry_after, &text, at);
                    state.planner.free_rotation().record_model_daily_park(
                        conn_id_for_breaker,
                        norm_model,
                        wait,
                    );
                }
                let message = extract_provider_error_message(&text, creds_for_scrub.as_ref());
                let needs_key = status == 401 || (!has_account && free_limit);
                SingleProbeResponse {
                    provider: target.provider.clone(),
                    model: target.model.clone(),
                    endpoint: url,
                    probe: ProbeResult {
                        ok: false,
                        status,
                        latency_ms,
                        message: Some(message),
                        needs_key,
                        at,
                    },
                }
            }
        }
        Err(err) => {
            let is_timeout = err.is_timeout();
            let msg = if is_timeout {
                "probe timed out".to_string()
            } else if let Some(creds) = creds_for_scrub.as_ref() {
                scrub(&err.without_url().to_string(), creds)
            } else {
                err.without_url().to_string().chars().take(240).collect()
            };
            SingleProbeResponse {
                provider: target.provider.clone(),
                model: target.model.clone(),
                endpoint: url,
                probe: ProbeResult {
                    ok: false,
                    status: if is_timeout { 504 } else { 502 },
                    latency_ms,
                    message: Some(msg),
                    needs_key: false,
                    at,
                },
            }
        }
    }
}

// ---------------------------------------------------------------- proxy helpers

/// True when both the free pool AND automatic fallback on paid quota/429/402 are turned on.
pub async fn fallback_enabled(store: &Store) -> bool {
    let Ok(Some(cfg)) = store.free_mode_config().await else {
        return false;
    };
    let enabled = cfg
        .routing
        .get("enabled")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let fallback = cfg
        .routing
        .get("fallback_on_quota")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    enabled && fallback
}

/// Verified endpoint for an anonymous free-tier send (`opencode` `Bearer public`), or `None`
/// if `(provider, model)` is NOT a verified usable public-catalog free model.
///
/// This is the hard guard guaranteeing anonymous `Bearer public` can never be used for a
/// paid model or an unsupported dialect.
pub async fn anonymous_endpoint_for(store: &Store, provider: &str, model: &str) -> Option<String> {
    if !supports_anonymous_free(provider) {
        return None;
    }
    let rows = store.list_free_models().await.ok()?;
    let verified = rows.iter().any(|row| {
        row.provider == provider
            && row.model == model
            && row.free
            && row.source == SOURCE_PUBLIC_CATALOG
    });
    if !verified {
        return None;
    }
    let key = model_key(provider, model);
    let from_meta = store
        .free_mode_config()
        .await
        .ok()
        .flatten()
        .and_then(|cfg| {
            cfg.routing
                .get("_meta")?
                .get(&key)?
                .get("endpoint")?
                .as_str()
                .map(str::to_string)
        })
        .filter(|url| allowed_catalog_endpoint(provider, url));
    Some(
        from_meta.unwrap_or_else(|| {
            "https://opencode.ai/inference/openai/v1/chat/completions".to_string()
        }),
    )
}

/// True when `(provider, model)` is a free model that only declares `chat-completions`
/// (not `responses`), so an inbound `/v1/responses` request must translate to OpenAI Chat.
pub async fn is_chat_only_free_model(store: &Store, provider: &str, model: &str) -> bool {
    let key = model_key(provider, model);
    if let Ok(Some(cfg)) = store.free_mode_config().await {
        if let Some(chat_only) = cfg
            .routing
            .get("_meta")
            .and_then(|m| m.get(&key))
            .and_then(|m| m.get("chat_only"))
            .and_then(Value::as_bool)
        {
            return chat_only;
        }
    }
    // Safe fallback for OpenCode public-catalog free rows when `_meta` wasn't populated.
    if provider == "opencode" {
        if let Ok(rows) = store.list_free_models().await {
            return rows.iter().any(|row| {
                row.provider == "opencode"
                    && row.model == model
                    && row.free
                    && row.source == SOURCE_PUBLIC_CATALOG
            });
        }
    }
    false
}

/// True when `(provider, model)` is a verified free public-catalog model — one the planner
/// may serve from a *keyed* account on that provider (OpenRouter `:free`) or anonymously
/// (OpenCode). Distinct from [`is_chat_only_free_model`] (a dialect fact) and from
/// [`anonymous_endpoint_for`] (which only admits anonymous-capable providers): the
/// provider-wide free-tier cooldown must skip a keyed OpenRouter `:free` model just as it
/// skips an anonymous OpenCode one, or one exhausted cap fans out across every model on the
/// account.
pub async fn is_public_catalog_free_model(store: &Store, provider: &str, model: &str) -> bool {
    let Ok(rows) = store.list_free_models().await else {
        return false;
    };
    rows.iter().any(|row| {
        row.provider == provider
            && row.model == model
            && row.free
            && row.source == SOURCE_PUBLIC_CATALOG
    })
}

/// True when an upstream 429 is a *platform-wide* free-tier limit that should cool the
/// whole provider rather than one model. Two providers publish such a limit:
///
/// * **OpenCode** — its account/IP-wide `FreeUsageLimitError`.
/// * **OpenRouter** — its own `:free` request caps (`free-models-per-min` /
///   `free-models-per-day`), signalled by a message `"Rate limit exceeded: free-models-per-…"`
///   and `metadata.provider_name` absent *or* `null` (OpenRouter is the limiter, not an
///   upstream model provider). An OpenRouter 429 that names a real `provider_name`/`raw`
///   (e.g. Google) is the upstream provider throttling, not OpenRouter's platform cap: that
///   is a per-model limit, so it must NOT cool the whole provider — the breaker's
///   `(connection, model)` cooldown and cross-model fallback own it.
pub fn is_free_usage_limit_error(status: u16, body: &str) -> bool {
    if status != 429 {
        return false;
    }
    if body.contains("FreeUsageLimitError") {
        return true;
    }
    if body.to_ascii_lowercase().contains("free usage limit") {
        return true;
    }
    // OpenRouter's platform cap is announced in prose, which a non-JSON or reshaped body
    // still carries. Match the message prefix before any JSON parse so a plain-text body
    // is classified too.
    if body
        .to_ascii_lowercase()
        .contains("rate limit exceeded: free-models-per-")
    {
        return true;
    }
    // Otherwise, a platform cap is a 429 whose `metadata` names no upstream provider
    // (absent or `null`) and carries no `raw` inner error.
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return false;
    };
    let Some(metadata) = value.pointer("/error/metadata") else {
        return false;
    };
    // An upstream-provider error names who refused (a non-null string) or carries an inner
    // `raw`; either is a per-model limit, not a platform cap.
    if metadata
        .get("provider_name")
        .and_then(Value::as_str)
        .is_some_and(|name| !name.trim().is_empty())
        || metadata.get("raw").is_some_and(|raw| !raw.is_null())
    {
        return false;
    }
    // `error_type` (when present) is a second, provider-independent signal.
    matches!(
        metadata.get("error_type").and_then(Value::as_str),
        Some("rate_limit_exceeded" | "free_models_per_day" | "free_models_per_minute")
    )
}

/// True when a 429/413 body names a *daily* exhaustion (per-day quota, "daily limit",
/// "requests per day", "tokens per day", RPD/TPD) rather than a transient per-minute
/// throttle. A daily-exhausted model/account parks until UTC midnight instead of the 1 h
/// cap a transient 429 earns.
pub fn is_daily_exhaustion(status: u16, body: &str) -> bool {
    if status != 429 && status != 413 {
        return false;
    }
    let lower = body.to_ascii_lowercase();
    if [
        "per day",
        "per-day",
        "daily",
        "requests/day",
        "tokens/day",
        "quota exceeded for the day",
        "free-models-per-day",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
    {
        return true;
    }
    has_ascii_word(&lower, "rpd") || has_ascii_word(&lower, "tpd")
}

fn is_ascii_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn has_ascii_word(haystack: &str, word: &str) -> bool {
    let bytes = haystack.as_bytes();
    let mut start = 0;
    while let Some(rel) = haystack[start..].find(word) {
        let pos = start + rel;
        let end = pos + word.len();
        let left_ok = pos == 0 || !is_ascii_word_byte(bytes[pos - 1]);
        let right_ok = end == bytes.len() || !is_ascii_word_byte(bytes[end]);
        if left_ok && right_ok {
            return true;
        }
        start = pos + 1;
    }
    false
}

/// Cooldown duration for a platform-wide free-tier 429 (`FreeUsageLimitError` or
/// OpenRouter `free-models-per-*`). A daily cap lifts the 1 h clamp to 24 h (parking until
/// the provider's `X-RateLimit-Reset` or the next UTC midnight), whereas a transient
/// per-minute/unknown cap stays clamped at 1 h.
pub fn free_cooldown_duration(
    status: u16,
    retry_after: Option<u64>,
    body: &str,
    now_unix: i64,
) -> Duration {
    if is_daily_exhaustion(status, body) {
        return daily_park_duration(retry_after, body, now_unix);
    }
    let secs = free_tier_cooldown_secs(retry_after, body, now_unix)
        .or_else(|| retry_hint_from_body(body))
        .unwrap_or(300)
        .clamp(30, 3600);
    Duration::from_secs(secs)
}

/// Cooldown duration for a daily exhaustion: prefers an authoritative header/reset/body
/// hint, otherwise parks until the next UTC midnight (clamped to `30..=86_400` s).
pub fn daily_park_duration(retry_after: Option<u64>, body: &str, now_unix: i64) -> Duration {
    let secs = free_tier_cooldown_secs(retry_after, body, now_unix)
        .or_else(|| retry_hint_from_body(body))
        .unwrap_or_else(|| {
            rrouter_router::free_mode::seconds_until_utc_midnight(now_unix.max(0) as u64)
        })
        .clamp(30, MAX_RETRY_HINT_SECS);
    Duration::from_secs(secs)
}

/// Parse an HTTP `Retry-After` header into whole seconds (ignores HTTP-date values).
pub fn parse_retry_after_secs(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    let raw = headers
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim();
    if let Ok(secs) = raw.parse::<u64>() {
        return Some(secs);
    }
    raw.parse::<f64>()
        .ok()
        .filter(|v| v.is_finite() && *v >= 0.0)
        .map(|v| v.ceil() as u64)
}

/// Hard ceiling for a learned/observed retry delay: upstream clamps its own to 24 h, and
/// parking a free account longer than a day is an operator action, not a throttle.
const MAX_RETRY_HINT_SECS: u64 = 86_400;

/// Retry delay in whole seconds from an upstream 429/503/413 answer, in the order the
/// parity doc fixes: the `Retry-After` header (integer, fractional, or HTTP-date) wins;
/// otherwise a bounded body walk for `retryDelay` / `retry_after` / `retryAfterSeconds`;
/// otherwise anchored prose ("try again in N s/m/ms"). Clamped to 24 h. `None` when the
/// upstream said nothing.
pub fn retry_hint(headers: &reqwest::header::HeaderMap, body: &str) -> Option<u64> {
    if let Some(secs) = retry_after_header_secs(headers) {
        return Some(secs.min(MAX_RETRY_HINT_SECS));
    }
    retry_hint_from_body(body)
}

/// Body-only retry hint (field walk then anchored prose), for call sites that already
/// parsed the header separately. Clamped to 24 h.
pub fn retry_hint_from_body(body: &str) -> Option<u64> {
    if let Some(secs) = body_retry_after(body) {
        return Some(secs.min(MAX_RETRY_HINT_SECS));
    }
    prose_retry_after(body).map(|secs| secs.min(MAX_RETRY_HINT_SECS))
}

/// `Retry-After` header as whole seconds: integer, fractional, or an HTTP-date (RFC 7231
/// IMF-fixdate / RFC 2822), clamped to 24 h.
pub fn retry_after_header_secs(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    retry_after_header_secs_at(headers, jiff::Timestamp::now())
        .map(|secs| secs.min(MAX_RETRY_HINT_SECS))
}

fn retry_after_header_secs_at(
    headers: &reqwest::header::HeaderMap,
    now: jiff::Timestamp,
) -> Option<u64> {
    let raw = headers
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim();
    if let Ok(secs) = raw.parse::<u64>() {
        return Some(secs);
    }
    if let Ok(frac) = raw.parse::<f64>() {
        if frac.is_finite() && frac >= 0.0 {
            return Some(frac.ceil() as u64);
        }
    }
    // HTTP-date form (RFC 7231 IMF-fixdate): "Wed, 21 Oct 2026 07:28:00 GMT".
    let when = jiff::fmt::rfc2822::parse(raw).ok()?.timestamp();
    let wait = when.as_second() - now.as_second();
    (wait > 0).then_some(wait as u64)
}

/// Depth-capped walk of a JSON error body for a retry delay field.
fn body_retry_after(body: &str) -> Option<u64> {
    let value: Value = serde_json::from_str(body).ok()?;
    const KEYS: &[&str] = &[
        "retryDelay",
        "retry_after",
        "retryAfterSeconds",
        "retry_after_seconds",
    ];
    let mut stack = vec![(&value, 0usize)];
    let mut visited = 0usize;
    while let Some((node, depth)) = stack.pop() {
        if depth > 8 || visited >= 256 {
            continue;
        }
        visited += 1;
        match node {
            Value::Object(map) => {
                for key in KEYS {
                    if let Some(hit) = map.get(*key).and_then(parse_seconds_value) {
                        return Some(hit);
                    }
                }
                for child in map.values() {
                    stack.push((child, depth + 1));
                }
            }
            Value::Array(items) => {
                for item in items {
                    stack.push((item, depth + 1));
                }
            }
            _ => {}
        }
    }
    None
}

fn parse_seconds_value(value: &Value) -> Option<u64> {
    if let Some(n) = value.as_u64() {
        return Some(n);
    }
    if let Some(n) = value.as_f64() {
        if n.is_finite() && n >= 0.0 {
            return Some(n.ceil() as u64);
        }
        return None;
    }
    // Protobuf / Gemini `google.rpc.RetryInfo.retryDelay` carries a duration string like
    // `"45s"`, `"1.5s"`, or `"500ms"` (or a bare numeric string).
    let raw = value.as_str()?.trim();
    if raw.is_empty() {
        return None;
    }
    if let Ok(secs) = raw.parse::<u64>() {
        return Some(secs);
    }
    if let Ok(frac) = raw.parse::<f64>() {
        if frac.is_finite() && frac >= 0.0 {
            return Some(frac.ceil() as u64);
        }
    }
    parse_duration_secs_exact(raw)
}

/// Anchored prose retry hints like `"Please try again in 640ms."`, `"in 2.04s"`,
/// `"in 7m30.144s"`, `"retry in 2m"`, `"try again in 1 hour"`, `"retry after 45s"`.
///
/// Only word-bounded multi-word retry phrases followed by whitespace or `:` qualify, so
/// unrelated text like `"contain 5m tokens"` or `"within 5m"` is never misread.
fn prose_retry_after(body: &str) -> Option<u64> {
    let lower = body.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let mut best: Option<u64> = None;
    for needle in ["try again in", "retry again in", "retry in", "retry after"] {
        let mut start = 0;
        while let Some(rel) = lower[start..].find(needle) {
            let pos = start + rel;
            let end = pos + needle.len();
            start = pos + 1;
            if pos > 0 && is_ascii_word_byte(bytes[pos - 1]) {
                continue;
            }
            let after = &lower[end..];
            // Require a separator (` `, `\t`, `:`) right after the phrase so e.g.
            // "retry into" cannot match.
            let Some(first) = after.chars().next() else {
                continue;
            };
            if !matches!(first, ' ' | '\t' | ':') {
                continue;
            }
            let after = after.trim_start_matches([' ', '\t', ':']);
            let Some((secs, _)) = parse_leading_duration_secs(after) else {
                continue;
            };
            best = Some(best.map_or(secs, |b| b.min(secs)));
        }
    }
    best
}

/// Parse an exact duration string (`"45s"`, `"1.5s"`, `"500ms"`, `"7m30.144s"`), rejecting
/// any trailing garbage.
fn parse_duration_secs_exact(s: &str) -> Option<u64> {
    let lower = s.trim().to_ascii_lowercase();
    let (secs, rest) = parse_leading_duration_secs(&lower)?;
    rest.trim().is_empty().then_some(secs)
}

/// Parse one or more contiguous duration components from the start of lowercase `s`
/// (longest unit match first: `ms`, `s`, `m`, `h`, `d` and their long English names),
/// summing compound forms like `"7m30.144s"` or `"1h 30m"`. Positive sub-second waits
/// (e.g. `"640ms"`, `"20ms"`) round up to `1` s.
fn parse_leading_duration_secs(s: &str) -> Option<(u64, &str)> {
    let mut rest = s.trim_start_matches([' ', '\t']);
    let mut total_secs = 0.0_f64;
    let mut parts = 0usize;

    loop {
        let candidate = if parts == 0 {
            rest
        } else {
            rest.trim_start_matches([' ', '\t'])
        };
        let Some((part_secs, next)) = parse_single_duration_component(candidate) else {
            break;
        };
        total_secs += part_secs;
        parts += 1;
        rest = next;
    }

    if parts == 0 || !total_secs.is_finite() || total_secs <= 0.0 {
        return None;
    }
    Some(((total_secs.ceil() as u64).max(1), rest))
}

fn parse_single_duration_component(s: &str) -> Option<(f64, &str)> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i == 0 {
        return None;
    }
    if i < bytes.len() && bytes[i] == b'.' {
        let frac_start = i + 1;
        let mut j = frac_start;
        while j < bytes.len() && bytes[j].is_ascii_digit() {
            j += 1;
        }
        if j == frac_start {
            return None;
        }
        i = j;
    }
    let num: f64 = s[..i].parse().ok()?;
    if !num.is_finite() || num < 0.0 {
        return None;
    }
    let after_num = s[i..].trim_start_matches([' ', '\t']);
    // Longest unit match first so `ms` / `milliseconds` is matched before `m`.
    const UNITS: &[(&str, f64)] = &[
        ("milliseconds", 0.001),
        ("millisecond", 0.001),
        ("msecs", 0.001),
        ("msec", 0.001),
        ("ms", 0.001),
        ("seconds", 1.0),
        ("second", 1.0),
        ("secs", 1.0),
        ("sec", 1.0),
        ("s", 1.0),
        ("minutes", 60.0),
        ("minute", 60.0),
        ("mins", 60.0),
        ("min", 60.0),
        ("m", 60.0),
        ("hours", 3600.0),
        ("hour", 3600.0),
        ("hrs", 3600.0),
        ("hr", 3600.0),
        ("h", 3600.0),
        ("days", 86_400.0),
        ("day", 86_400.0),
        ("d", 86_400.0),
    ];
    for &(unit, mult) in UNITS {
        if let Some(tail) = after_num.strip_prefix(unit) {
            // Reject partial identifier matches like "5meters" or "5mb", while still
            // allowing compound durations like "7m30.144s" where `'3'` is non-alphabetic.
            if tail.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
                continue;
            }
            return Some((num * mult, tail));
        }
    }
    None
}

/// Re-exported from `rrouter-router` so learned limits and rotation share one enum.
pub use rrouter_router::free_mode::LimitKind;

/// Parse a limit the provider states in an error body:
/// - `"Limit 30000 requests per minute"` / `"Limit 12000 tokens per minute"`
/// - `"on tokens per minute (TPM): Limit 12000, Used 11500, Requested 1000"` (Groq 413/429)
/// - `"quota exceeded for the day: 50 requests per day"`
///
/// Only ever *tightens* the stored value at the call site, never raises it. `None` when no
/// recognizable positive limit is present.
pub fn learn_limit_from_error(body: &str) -> Option<(LimitKind, u64)> {
    let lower = body.to_ascii_lowercase();
    let units: &[(&str, LimitKind, bool)] = &[
        ("requests per minute", LimitKind::RequestsPerMinute, false),
        ("requests/minute", LimitKind::RequestsPerMinute, false),
        ("request per minute", LimitKind::RequestsPerMinute, false),
        ("requests per day", LimitKind::RequestsPerDay, false),
        ("requests/day", LimitKind::RequestsPerDay, false),
        ("request per day", LimitKind::RequestsPerDay, false),
        ("tokens per minute", LimitKind::TokensPerMinute, false),
        ("tokens/minute", LimitKind::TokensPerMinute, false),
        ("tokens per day", LimitKind::TokensPerDay, false),
        ("tokens/day", LimitKind::TokensPerDay, false),
        ("tpm", LimitKind::TokensPerMinute, true),
        ("rpm", LimitKind::RequestsPerMinute, true),
        ("tpd", LimitKind::TokensPerDay, true),
        ("rpd", LimitKind::RequestsPerDay, true),
    ];
    for &(unit, kind, word_bounded) in units {
        let Some(idx) = find_unit(&lower, unit, word_bounded) else {
            continue;
        };
        let before = &lower[..idx];
        // 1. Number immediately before the unit: "Limit 30000 requests per minute".
        if let Some(n) = trailing_number(before).filter(|&n| n > 0) {
            return Some((kind, n));
        }
        // 2. Number immediately after the unit: "requests per minute: 30".
        let after = &lower[idx + unit.len()..];
        let after_clean = after.trim_start_matches([' ', '\t', ':', '=', ')']);
        if let Some(n) = leading_number(after_clean).filter(|&n| n > 0) {
            return Some((kind, n));
        }
        // 3. Groq 413/429 format: "on tokens per minute (TPM): Limit 12000, Used ...".
        let window_after: String = after.chars().take(48).collect();
        if let Some(lim_pos) = window_after.find("limit") {
            let after_limit =
                window_after[lim_pos + "limit".len()..].trim_start_matches([' ', '\t', ':', '=']);
            if let Some(n) = leading_number(after_limit).filter(|&n| n > 0) {
                return Some((kind, n));
            }
        }
    }
    None
}

fn find_unit(haystack: &str, unit: &str, word_bounded: bool) -> Option<usize> {
    if !word_bounded {
        return haystack.find(unit);
    }
    let bytes = haystack.as_bytes();
    let mut start = 0;
    while let Some(rel) = haystack[start..].find(unit) {
        let pos = start + rel;
        let end = pos + unit.len();
        let left_ok = pos == 0 || !is_ascii_word_byte(bytes[pos - 1]);
        let right_ok = end == bytes.len() || !is_ascii_word_byte(bytes[end]);
        if left_ok && right_ok {
            return Some(pos);
        }
        start = pos + 1;
    }
    None
}

fn trailing_number(s: &str) -> Option<u64> {
    let digits: String = s
        .trim_end()
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_digit() || *c == ',')
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .filter(|c| c.is_ascii_digit())
        .collect();
    if digits.is_empty() {
        return None;
    }
    digits.parse().ok()
}

fn leading_number(s: &str) -> Option<u64> {
    let t = s.trim_start();
    let digits: String = t
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == ',')
        .filter(|c| c.is_ascii_digit())
        .collect();
    if digits.is_empty() {
        return None;
    }
    digits.parse().ok()
}

/// Whole seconds until a free-tier cap resets, from the most authoritative signal
/// available. The HTTP `Retry-After` header (already parsed into `retry_after`) wins;
/// otherwise an OpenRouter platform-cap 429 carries
/// `error.metadata.headers.X-RateLimit-Reset` (epoch **milliseconds**, string or number),
/// which parks a per-day cap until the provider's own reset instead of re-probing every
/// 300 s. `None` when neither is present, so callers fall back to their own default.
pub fn free_tier_cooldown_secs(retry_after: Option<u64>, body: &str, now_unix: i64) -> Option<u64> {
    if let Some(secs) = retry_after {
        return Some(secs);
    }
    let value: Value = serde_json::from_str(body).ok()?;
    let reset_val = value.pointer("/error/metadata/headers/X-RateLimit-Reset")?;
    let reset_ms = reset_val.as_i64().or_else(|| {
        reset_val
            .as_str()
            .and_then(|s| s.trim().parse::<i64>().ok())
    })?;
    let reset = reset_ms / 1000;
    if reset <= now_unix {
        return None;
    }
    Some((reset - now_unix) as u64)
}

/// Extract a concise, credential-scrubbed provider error message for probe responses.
///
/// OpenRouter wraps an upstream provider's refusal behind the generic
/// `"Provider returned error"`; the real reason sits in `error.metadata.raw` (a JSON
/// string the inner provider sent). Unwrap it so the operator and the UI see *why* the
/// free model failed instead of a placeholder that names nothing.
pub fn extract_provider_error_message(body: &str, creds: Option<&Credentials>) -> String {
    let extracted = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| {
            openrouter_inner_message(&v)
                .or_else(|| v.pointer("/error/message").cloned())
                .or_else(|| v.get("message").cloned())
        })
        .and_then(|s| s.as_str().map(str::to_string))
        .unwrap_or_else(|| body.trim().to_string());
    let cleaned = match creds {
        Some(c) => scrub(&extracted, c),
        None => extracted,
    };
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        "upstream returned an empty error body".to_string()
    } else {
        trimmed.chars().take(240).collect()
    }
}

/// OpenRouter's `error.metadata.raw`, unwrapped one level, when the outer message is the
/// unhelpful `"Provider returned error"`. `raw` is the inner provider's own error and can
/// be a JSON string (a provider error body), an already-parsed object, or a plain-text
/// sentence — the plain-text form is the common upstream 429. Read the inner
/// `error.message`/`message` from JSON, else return the raw text itself.
fn openrouter_inner_message(value: &Value) -> Option<Value> {
    let metadata = value.pointer("/error/metadata")?;
    let raw = metadata.get("raw")?;
    match raw {
        Value::String(text) => {
            if let Ok(inner) = serde_json::from_str::<Value>(text) {
                if let Some(message) = inner
                    .pointer("/error/message")
                    .or_else(|| inner.get("message"))
                    .cloned()
                {
                    if message.as_str().is_some_and(|s| !s.trim().is_empty()) {
                        return Some(message);
                    }
                }
            }
            // Plain-text raw (not JSON): the sentence is itself the reason.
            if text.trim().is_empty() {
                None
            } else {
                Some(Value::String(text.trim().to_string()))
            }
        }
        Value::Object(_) => raw
            .pointer("/error/message")
            .or_else(|| raw.get("message"))
            .cloned(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn price(input: f64, output: f64) -> Option<f64> {
        published_price_of(&json!({ "input": input, "output": output }))
    }

    /// The vendored catalog parses, records its upstream provenance (repo + sha + MIT
    /// notice), and answers the free rule and per-model limits the routing paths read.
    #[test]
    fn vendored_free_tiers_parse_and_expose_rules_and_limits() {
        let tiers = vendored_free_tiers();
        assert_eq!(
            tiers.upstream.repo,
            "https://github.com/tashfeenahmed/freellmapi"
        );
        assert_eq!(
            tiers.upstream.sha,
            "6daa81b8def6a7e1d99fd67a611779913c287076"
        );
        assert_eq!(tiers.upstream.license, "MIT");
        assert!(tiers.upstream.paths.len() >= 8, "tracked paths recorded");

        // Whole-key free tiers carry the all-models-on-free-key rule; OpenRouter keeps
        // its `:free` suffix rule.
        assert_eq!(vendored_free_rule("groq"), Some("all-models-on-free-key"));
        assert_eq!(
            vendored_free_rule("cerebras"),
            Some("all-models-on-free-key")
        );
        assert_eq!(vendored_free_rule("gemini"), Some("all-models-on-free-key"));
        assert_eq!(
            vendored_free_rule("mistral"),
            Some("all-models-on-free-key")
        );
        assert_eq!(vendored_free_rule("openrouter"), Some("id-suffix:free"));
        assert_eq!(vendored_free_rule("opencode"), None);

        // OpenRouter's provider-wide daily cap is recorded (the gap-3 signal).
        let openrouter = tiers
            .providers
            .iter()
            .find(|p| p.id == "openrouter")
            .expect("openrouter");
        let caps = openrouter.provider_caps.as_ref().expect("caps");
        assert_eq!(caps.rpd, Some(1000));
        assert_eq!(caps.rpd_low_credit, Some(50));

        // Groq's corrected final limits (TPM raised to 12000, TPD cut to 100000) survive
        // the UPDATE-chain reconciliation the parity doc describes.
        let groq = vendored_model_limits("groq", "llama-3.3-70b-versatile").expect("groq row");
        assert_eq!(groq.rpm, Some(30));
        assert_eq!(groq.rpd, Some(1000));
        assert_eq!(groq.tpm, Some(12000));
        assert_eq!(groq.tpd, Some(100000));
        assert_eq!(groq.context, Some(131072));

        // Gemini's stale 250/1000 RPD was corrected to the live 20 RPD free pool.
        let gemini = vendored_model_limits("gemini", "gemini-2.5-flash").expect("gemini row");
        assert_eq!(gemini.rpd, Some(20));

        // Cerebras: the three originally-seeded rows were removed from the free tier (V2),
        // and gpt-oss-120b was re-added (V10) and corrected to the shared 5 RPM / 30K TPM /
        // 2400 RPD / 1M TPD free pool (V13). The removed qwen-3-coder-480b must not appear.
        let cerebras = vendored_model_limits("cerebras", "gpt-oss-120b").expect("cerebras row");
        assert_eq!(cerebras.rpm, Some(5));
        assert_eq!(cerebras.rpd, Some(2400));
        assert_eq!(cerebras.tpm, Some(30000));
        assert_eq!(cerebras.tpd, Some(1000000));
        assert!(vendored_model_limits("cerebras", "qwen-3-coder-480b").is_none());

        // Mistral's context window was corrected 131K → 262K (V12).
        let mistral =
            vendored_model_limits("mistral", "mistral-large-latest").expect("mistral row");
        assert_eq!(mistral.context, Some(262144));

        assert!(vendored_model_limits("groq", "does-not-exist").is_none());
    }

    /// The whole-key free rule widens only a vendored model row, so a billed key flagged
    /// `free_mode` can never leak a paid model into the pool.
    #[test]
    fn whole_key_free_model_widens_only_vendored_rows() {
        // A vendored Groq model is free; a non-vendored id on the same whole-key provider
        // (gemini-2.5-pro, or any unlisted id) is not.
        assert!(whole_key_free_model("groq", "llama-3.3-70b-versatile"));
        assert!(whole_key_free_model("gemini", "gemini-2.5-flash"));
        assert!(!whole_key_free_model("gemini", "gemini-2.5-pro"));
        assert!(!whole_key_free_model("groq", "gpt-5-expensive"));

        // Providers without the whole-key rule never widen, even for a vendored id.
        assert!(!whole_key_free_model(
            "openrouter",
            "llama-3.3-70b-versatile"
        ));
        assert!(!whole_key_free_model("opencode", "llama-3.3-70b-versatile"));
        assert!(!whole_key_free_model("unknown", "llama-3.3-70b-versatile"));
    }

    /// The provider-wide daily cap comes from the vendored file, with a lower tier for
    /// low-credit accounts; providers without a cap return `None`.
    #[test]
    fn vendored_provider_daily_cap_selects_the_tier() {
        assert_eq!(vendored_provider_daily_cap("openrouter", false), Some(1000));
        assert_eq!(vendored_provider_daily_cap("openrouter", true), Some(50));
        // No cap for the whole-key providers.
        assert_eq!(vendored_provider_daily_cap("groq", false), None);
        assert_eq!(vendored_provider_daily_cap("gemini", true), None);
        assert_eq!(vendored_provider_daily_cap("unknown", false), None);
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

    /// OpenCode Zen: zero-priced chat models are usable; `jev-1.13-free` (systemone) is
    /// recorded but marked unusable with the exact reason the UI discloses.
    #[test]
    fn opencode_catalog_keeps_chat_free_and_marks_systemone_unusable() {
        let body = json!({
            "data": [
                {
                    "id": "big-pickle",
                    "name": "Big Pickle",
                    "recommended_api": "chat-completions",
                    "api": { "chat-completions": { "base_url": "https://opencode.ai/inference/openai/v1" } },
                    "cost": { "text": { "input": { "price": 0 }, "output": { "price": 0 } } }
                },
                {
                    "id": "jev-1.13-free",
                    "name": "Jev 1.13 Free",
                    "type": "decision",
                    "recommended_api": "systemone",
                    "api": { "systemone": { "base_url": "https://opencode.ai/inference/systemone/v1" } },
                    "cost": { "text": { "input": { "price": 0 }, "output": { "price": 0 } } }
                },
                {
                    "id": "gpt-5",
                    "name": "GPT-5",
                    "recommended_api": "chat-completions",
                    "api": { "chat-completions": { "base_url": "https://opencode.ai/inference/openai/v1" } },
                    "cost": { "text": { "input": { "price": 1.5 }, "output": { "price": 6 } } }
                }
            ]
        });
        let models = parse_opencode_catalog(&body);
        assert_eq!(models.len(), 2, "the paid model must be excluded");

        let pickle = &models[0];
        assert_eq!(pickle.model, "big-pickle");
        assert!(pickle.usable);
        assert_eq!(pickle.dialect, "openai_chat");
        assert_eq!(
            pickle.endpoint,
            "https://opencode.ai/inference/openai/v1/chat/completions"
        );
        assert!(pickle.chat_only, "chat-completions only, no responses");
        assert_eq!(pickle.auth_mode, "key_or_anonymous");
        assert_eq!(pickle.price, Some(0.0));

        let jev = &models[1];
        assert_eq!(jev.model, "jev-1.13-free");
        assert!(!jev.usable);
        assert_eq!(jev.dialect, "systemone");
        assert_eq!(
            jev.unusable_reason.as_deref(),
            Some("Speaks the systemone dialect, which RRouter does not support")
        );
    }

    /// OpenRouter: `:free` suffix OR zero pricing; always `key_required`.
    #[test]
    fn openrouter_catalog_marks_free_suffix_and_zero_price_key_required() {
        let body = json!({
            "data": [
                { "id": "inclusionai/ling-3.2-flash:free", "name": "Ling", "pricing": { "prompt": "0", "completion": "0" } },
                { "id": "openrouter/auto", "name": "Auto", "pricing": { "prompt": "0", "completion": "0" } },
                { "id": "openrouter/gpt-4o", "name": "GPT-4o", "pricing": { "prompt": "2.5", "completion": "10" } }
            ]
        });
        let models = parse_openrouter_catalog(&body);
        assert_eq!(models.len(), 2, "the paid model must be excluded");
        for m in &models {
            assert!(m.usable);
            assert_eq!(m.dialect, "openai_chat");
            assert_eq!(m.auth_mode, "key_required");
            assert_eq!(m.price, Some(0.0));
            assert!(m.chat_only);
        }
        // The `:free` suffix alone is enough even if the price string ever went nonzero.
        assert!(models
            .iter()
            .any(|m| m.model == "inclusionai/ling-3.2-flash:free"));
    }

    /// OpenCode's account/IP-wide free limit is recognized by its type token, its prose, or
    /// both; other 429s and other statuses are not.
    #[test]
    fn free_usage_limit_error_is_only_the_ip_wide_429() {
        let body = r#"{"type":"error","error":{"type":"FreeUsageLimitError","message":"Free usage limit reached."}}"#;
        assert!(is_free_usage_limit_error(429, body));
        assert!(is_free_usage_limit_error(429, "free usage limit reached"));
        assert!(!is_free_usage_limit_error(429, "too many requests"));
        assert!(!is_free_usage_limit_error(401, body));
        assert!(!is_free_usage_limit_error(200, body));
    }

    /// OpenRouter's own `:free` request cap is a platform-wide limit (cool the provider);
    /// its upstream-provider 429 is per-model (cool the model, fall back). The real
    /// platform-cap body carries `provider_name: null` and no `error_type` — it is
    /// announced in the message prose — while the upstream-provider form names a real
    /// provider.
    #[test]
    fn openrouter_free_tier_cap_cools_the_provider_but_provider_429_does_not() {
        // OpenRouter's real per-day cap body (provider_name is null, no error_type).
        let platform_day = r#"{"error":{"message":"Rate limit exceeded: free-models-per-day. Add 10 credits to unlock 1000 free model requests per day","code":429,"metadata":{"headers":{"X-RateLimit-Limit":"50","X-RateLimit-Remaining":"0","X-RateLimit-Reset":"1741305600000"},"provider_name":null}}}"#;
        assert!(is_free_usage_limit_error(429, platform_day));

        // The per-minute variant.
        let platform_min = r#"{"error":{"message":"Rate limit exceeded: free-models-per-min","code":429,"metadata":{"provider_name":null}}}"#;
        assert!(is_free_usage_limit_error(429, platform_min));

        // The upstream provider (Google) throttled: named via a real provider_name.
        let provider = r#"{"error":{"code":429,"message":"Provider returned error","metadata":{"provider_name":"Google AI Studio","raw":"google/gemma-3-27b-it:free is temporarily rate-limited upstream"}}}"#;
        assert!(
            !is_free_usage_limit_error(429, provider),
            "an upstream provider 429 is per-model, not a platform cap"
        );

        // Neither signal: an ordinary 429 is not a free-tier limit.
        assert!(!is_free_usage_limit_error(
            429,
            r#"{"error":{"code":429,"message":"too many requests"}}"#
        ));
    }

    /// The probe surfaces the *inner* provider reason behind OpenRouter's generic
    /// "Provider returned error", not the placeholder.
    #[test]
    fn extract_message_unwraps_openrouter_metadata_raw() {
        // JSON-string raw: the inner provider's error body.
        let json_raw = r#"{"error":{"code":429,"message":"Provider returned error","metadata":{"raw":"{\"error\":{\"message\":\"Resource exhausted: gemini rate limit\"}}","provider_name":"Google"}}}"#;
        let msg = extract_provider_error_message(json_raw, None);
        assert_eq!(msg, "Resource exhausted: gemini rate limit");

        // Plain-text raw: the sentence is itself the reason.
        let plain_raw = r#"{"error":{"code":429,"message":"Provider returned error","metadata":{"raw":"google/gemma-3-27b-it:free is temporarily rate-limited upstream. Please retry shortly, or add your own key to accumulate your rate limits: https://openrouter.ai/settings/integrations","provider_name":"Google AI Studio"}}}"#;
        let msg = extract_provider_error_message(plain_raw, None);
        assert!(
            msg.starts_with("google/gemma-3-27b-it:free is temporarily rate-limited"),
            "plain-text raw must surface verbatim, got {msg:?}"
        );

        // Without a raw inner message, the outer message is the fallback.
        let plain = r#"{"error":{"code":429,"message":"Provider returned error"}}"#;
        assert_eq!(
            extract_provider_error_message(plain, None),
            "Provider returned error"
        );

        // A non-OpenRouter error still reads `/error/message`.
        let anthropic = r#"{"type":"error","error":{"message":"rate_limit_error"}}"#;
        assert_eq!(
            extract_provider_error_message(anthropic, None),
            "rate_limit_error"
        );
    }

    /// `Retry-After` is read as integer seconds, fractional seconds, and ignored when it is
    /// an HTTP-date or absent — never invented.
    #[test]
    fn retry_after_parses_seconds_and_ignores_dates() {
        let mut h = reqwest::header::HeaderMap::new();
        h.insert(reqwest::header::RETRY_AFTER, "61".parse().unwrap());
        assert_eq!(parse_retry_after_secs(&h), Some(61));

        h.insert(reqwest::header::RETRY_AFTER, "0.5".parse().unwrap());
        assert_eq!(parse_retry_after_secs(&h), Some(1), "ceil of 0.5");

        h.insert(
            reqwest::header::RETRY_AFTER,
            "Wed, 21 Oct 2026 07:28:00 GMT".parse().unwrap(),
        );
        assert_eq!(parse_retry_after_secs(&h), None);

        let empty = reqwest::header::HeaderMap::new();
        assert_eq!(parse_retry_after_secs(&empty), None);
    }

    /// A free-tier cooldown prefers the already-parsed `Retry-After`, then reads the
    /// OpenRouter platform cap's `metadata.headers.X-RateLimit-Reset` (epoch ms, string or
    /// integer) so a per-day cap parks until the provider's own reset instead of re-probing
    /// on the local default.
    #[test]
    fn free_tier_cooldown_secs_prefers_header_then_rate_limit_reset() {
        assert_eq!(
            free_tier_cooldown_secs(Some(90), "{}", 1_700_000_000),
            Some(90),
            "Retry-After wins"
        );

        // No header: the string reset (epoch ms) drives the wait.
        let body = r#"{"error":{"code":429,"message":"Rate limit exceeded: free-models-per-day","metadata":{"headers":{"X-RateLimit-Reset":"1700001000000"},"provider_name":null}}}"#;
        let now = 1_700_000_000;
        assert_eq!(
            free_tier_cooldown_secs(None, body, now),
            Some(1000),
            "1000 s until the epoch-ms reset"
        );

        // Numeric JSON X-RateLimit-Reset is accepted too.
        let numeric = r#"{"error":{"code":429,"message":"Rate limit exceeded: free-models-per-day","metadata":{"headers":{"X-RateLimit-Reset":1700020000000},"provider_name":null}}}"#;
        assert_eq!(
            free_tier_cooldown_secs(None, numeric, now),
            Some(20_000),
            "numeric epoch-ms reset"
        );
        // Daily platform cap lifts the 1 h clamp and parks until reset (20,000 s > 3,600 s).
        assert_eq!(
            free_cooldown_duration(429, None, numeric, now),
            Duration::from_secs(20_000)
        );
        // Transient per-minute platform cap stays clamped at 1 h.
        let transient = r#"{"error":{"code":429,"message":"Rate limit exceeded: free-models-per-min","metadata":{"headers":{"X-RateLimit-Reset":1700020000000},"provider_name":null}}}"#;
        assert_eq!(
            free_cooldown_duration(429, None, transient, now),
            Duration::from_secs(3600)
        );

        // A reset already in the past yields nothing.
        assert_eq!(free_tier_cooldown_secs(None, body, 1_700_002_000), None);
    }

    /// `retry_hint` prefers the header (integer/fractional/RFC 2822 HTTP-date), then a body
    /// field (including Gemini `RetryInfo.retryDelay: "45s"`), then word-anchored prose
    /// (supporting `ms`, decimals, compound durations, and spaced long units), clamped to 24 h.
    #[test]
    fn retry_hint_reads_header_then_body_then_prose() {
        let mut h = reqwest::header::HeaderMap::new();
        h.insert(reqwest::header::RETRY_AFTER, "61".parse().unwrap());
        assert_eq!(retry_hint(&h, "{}"), Some(61), "integer header wins");

        // RFC 2822 HTTP-date header parses via jiff::fmt::rfc2822::parse.
        let mut date_h = reqwest::header::HeaderMap::new();
        date_h.insert(
            reqwest::header::RETRY_AFTER,
            "Wed, 21 Oct 2026 07:28:00 GMT".parse().unwrap(),
        );
        let one_min_before = jiff::fmt::rfc2822::parse("Wed, 21 Oct 2026 07:27:00 GMT")
            .unwrap()
            .timestamp();
        assert_eq!(
            retry_after_header_secs_at(&date_h, one_min_before),
            Some(60),
            "HTTP-date Retry-After"
        );

        // Body field when no header.
        let empty = reqwest::header::HeaderMap::new();
        let json_body = r#"{"error":{"message":"slow down","retry_after": 30}}"#;
        assert_eq!(retry_hint(&empty, json_body), Some(30), "body field");

        // Real Gemini RetryInfo protobuf duration strings ("45s", "1.5s", "500ms") and wide JSON.
        let gemini = r#"{"error":{"code":429,"status":"RESOURCE_EXHAUSTED","message":"exhausted","a":1,"b":2,"c":3,"d":4,"e":5,"f":6,"g":7,"h":8,"details":[{"@type":"type.googleapis.com/google.rpc.RetryInfo","retryDelay":"45s"}]}}"#;
        assert_eq!(retry_hint(&empty, gemini), Some(45), "Gemini RetryInfo 45s");
        let gemini_frac = r#"{"error":{"details":[{"retryDelay":"1.5s"}]}}"#;
        assert_eq!(
            retry_hint(&empty, gemini_frac),
            Some(2),
            "Gemini RetryInfo 1.5s"
        );
        let gemini_ms = r#"{"error":{"details":[{"retryDelay":"500ms"}]}}"#;
        assert_eq!(
            retry_hint(&empty, gemini_ms),
            Some(1),
            "Gemini RetryInfo 500ms"
        );

        // Real Groq / OpenAI prose formats: ms, decimals, compound durations, spaced units.
        assert_eq!(
            retry_hint(&empty, "Please try again in 640ms."),
            Some(1),
            "640ms rounds up to 1s, NOT 640 minutes"
        );
        assert_eq!(
            retry_hint(&empty, "Please try again in 20ms."),
            Some(1),
            "20ms rounds up to 1s, NOT 20 minutes"
        );
        assert_eq!(
            retry_hint(&empty, "Please try again in 2.04s."),
            Some(3),
            "fractional seconds ceil"
        );
        assert_eq!(
            retry_hint(&empty, "Please try again in 7m30.144s."),
            Some(451),
            "compound 7m30.144s = 450.144s -> 451s"
        );
        assert_eq!(
            retry_hint(&empty, "Please try again in 1 hour."),
            Some(3600),
            "spaced long unit"
        );
        assert_eq!(
            retry_hint(&empty, "rate limited, try again in 30s"),
            Some(30),
            "prose seconds"
        );
        assert_eq!(
            retry_hint(&empty, "please retry in 2m"),
            Some(120),
            "prose minutes"
        );
        assert_eq!(
            retry_hint(&empty, "retry after 45 seconds"),
            Some(45),
            "retry after seconds"
        );

        // Unanchored or unrelated prose must NEVER produce a false-positive cooldown.
        assert_eq!(
            retry_hint(&empty, "the request may contain 5m tokens"),
            None,
            "contain 5m tokens must not match"
        );
        assert_eq!(
            retry_hint(&empty, "error occurred within 5m of startup"),
            None,
            "within 5m must not match"
        );
        assert_eq!(
            retry_hint(&empty, "retry into 5m buffer"),
            None,
            "retry into must not match"
        );

        // Clamped to 24 h.
        let mut big = reqwest::header::HeaderMap::new();
        big.insert(reqwest::header::RETRY_AFTER, "999999".parse().unwrap());
        assert_eq!(retry_hint(&big, "{}"), Some(86_400), "clamped");
    }

    /// `learn_limit_from_error` extracts a numeric limit from provider prose (including
    /// Groq's real `on tokens per minute (TPM): Limit 12000, Used ...` shape) and ignores
    /// accidental substring hits.
    #[test]
    fn learn_limit_reads_provider_limit_prose() {
        let groq_rpm =
            "Rate limit reached for model in organization. Limit 30000 requests per minute.";
        assert_eq!(
            learn_limit_from_error(groq_rpm),
            Some((LimitKind::RequestsPerMinute, 30000))
        );

        let groq_real_tpm = "Rate limit reached for model `llama-3.3-70b-versatile` in organization `org_1` on tokens per minute (TPM): Limit 12000, Used 11500, Requested 1000. Please try again in 2.5s.";
        assert_eq!(
            learn_limit_from_error(groq_real_tpm),
            Some((LimitKind::TokensPerMinute, 12000))
        );

        let daily = "quota exceeded for the day: 50 requests per day";
        assert_eq!(
            learn_limit_from_error(daily),
            Some((LimitKind::RequestsPerDay, 50))
        );

        let tpm = "Limit 12,000 tokens per minute reached";
        assert_eq!(
            learn_limit_from_error(tpm),
            Some((LimitKind::TokensPerMinute, 12000))
        );

        // No number near a limit unit, or substring inside another word → nothing.
        assert_eq!(learn_limit_from_error("something went wrong"), None);
        assert_eq!(learn_limit_from_error("httpmodel limit 500"), None);
    }

    /// Gemini's `/models` endpoint prefixes model ids with `models/`; vendored lookup and
    /// whole-key matching strip that prefix while keeping paid / non-chat Gemini models out.
    #[test]
    fn vendored_gemini_normalizes_models_prefix_and_keeps_paid_out() {
        assert!(whole_key_free_model("gemini", "gemini-2.5-flash"));
        assert!(whole_key_free_model("gemini", "models/gemini-2.5-flash"));
        assert_eq!(
            vendored_model_limits("gemini", "models/gemini-2.5-flash").and_then(|m| m.rpd),
            Some(20)
        );
        assert!(!whole_key_free_model("gemini", "models/gemini-2.5-pro"));
        assert!(!whole_key_free_model("gemini", "models/text-embedding-004"));
    }

    /// `is_daily_exhaustion` flags daily / RPD / TPD exhaustions on 429/413 and leaves
    /// transient per-minute throttles alone.
    #[test]
    fn is_daily_exhaustion_distinguishes_daily_from_transient() {
        assert!(is_daily_exhaustion(
            429,
            "Rate limit exceeded: free-models-per-day"
        ));
        assert!(is_daily_exhaustion(
            429,
            "Quota exceeded for quota metric 'Generate Content API requests per day'"
        ));
        assert!(is_daily_exhaustion(413, "Exceeded model TPD limit"));
        assert!(!is_daily_exhaustion(
            429,
            "Rate limit exceeded: free-models-per-min"
        ));
        assert!(!is_daily_exhaustion(200, "requests per day"));
    }

    /// A catalog-supplied endpoint may only be the provider's own https host with no
    /// userinfo. Loopback is refused outside `cfg(test)`; a credential in the URL, a query,
    /// a fragment, or `http` on a remote host are all rejected.
    #[test]
    fn catalog_endpoint_rejects_non_https_credentials_and_remote_loopback() {
        // The provider's own https host is accepted.
        assert!(allowed_catalog_endpoint(
            "opencode",
            "https://opencode.ai/inference/openai/v1/chat/completions"
        ));
        assert!(allowed_catalog_endpoint(
            "openrouter",
            "https://openrouter.ai/api/v1/chat/completions"
        ));

        // Credentials in the URL are smuggling material and always rejected.
        assert!(!allowed_catalog_endpoint(
            "opencode",
            "https://user:pass@opencode.ai/inference/openai/v1/chat/completions"
        ));

        // `http` on a remote host is rejected (downgrade).
        assert!(!allowed_catalog_endpoint(
            "opencode",
            "http://opencode.ai/inference/openai/v1/chat/completions"
        ));

        // A foreign host is rejected even over https.
        assert!(!allowed_catalog_endpoint(
            "opencode",
            "https://evil.example/chat/completions"
        ));

        // A query or fragment is rejected.
        assert!(!allowed_catalog_endpoint(
            "opencode",
            "https://opencode.ai/inference/openai/v1/chat/completions?x=1"
        ));
        assert!(!allowed_catalog_endpoint(
            "opencode",
            "https://opencode.ai/inference/openai/v1/chat/completions#frag"
        ));

        // Loopback is refused in the shipping binary (only `cfg(test)` opens it).
        let loopback_ok =
            allowed_catalog_endpoint("opencode", "http://127.0.0.1:8080/chat/completions");
        if cfg!(test) {
            assert!(loopback_ok);
        } else {
            assert!(!loopback_ok);
        }
    }
}
