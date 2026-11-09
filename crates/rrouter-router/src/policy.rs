//! Request-scoped provider policy: the OpenRouter `provider` object, plus compact
//! `x-rrouter-*` headers that override it.
//!
//! The policy is a *hard filter* over the providers a model could be served by, and an
//! intra-priority ordering hint. It never overrides a connection's `priority`: an
//! operator's configured preference stays authoritative, and the request only decides
//! which accounts are admissible and which of the equally-preferred ones to try first.
//!
//! Everything here is pure: no I/O, no store reads. The planner applies it after
//! alias/combo expansion, so a preference (or a key allowlist) is judged against the
//! provider that would actually be called rather than against the client's spelling.

use serde_json::{Map, Value};

use rrouter_core::{Error, Result};

use crate::capabilities::QUANTIZATIONS;

/// `provider.data_collection` — may this request be retained by the provider?
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataCollection {
    /// Any provider is acceptable.
    Allow,
    /// Only providers that contractually do not retain or train on the request.
    Deny,
}

/// Provider routing preferences for one request.
///
/// Unknown JSON keys are ignored on purpose: a client that sends OpenRouter's full object
/// (including fields RRouter does not implement, such as `sort`) must still route, and
/// silently ignoring an unimplemented preference is worse than rejecting the request —
/// so the *implemented* fields are validated strictly and the rest are documented as
/// unsupported.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProviderPolicy {
    /// Preferred providers, first to last. Ordering hint inside one priority group.
    pub order: Vec<String>,
    /// When non-empty, the only providers that may serve the request.
    pub only: Vec<String>,
    /// Providers that must never serve the request.
    pub ignore: Vec<String>,
    /// `false` stops the plan at the first priority group (and at `order[0]` when it is
    /// named there): a failed provider is not replaced by a lower-priority one.
    pub allow_fallbacks: Option<bool>,
    /// `Some(Deny)` excludes providers without a no-retention commitment.
    pub data_collection: Option<DataCollection>,
    /// Zero data retention required.
    pub zdr: bool,
    /// Acceptable precisions; a provider that publishes none of them is excluded.
    pub quantizations: Vec<String>,
    /// Ceiling on `input + output` USD per million tokens, as [`crate::planner`] prices a
    /// candidate. A candidate with no published price cannot prove it is under the limit
    /// and is therefore excluded.
    pub max_price_per_million: Option<f64>,
    /// Require the provider to honour sampling controls.
    pub require_parameters: bool,
}

impl ProviderPolicy {
    /// Parse the `provider` object of a client body. `None` when the body has no such
    /// object; a non-object `provider` is a client error, not a preference to ignore.
    pub fn from_body(body: &Value) -> Result<Option<Self>> {
        let Some(value) = body.get("provider") else {
            return Ok(None);
        };
        if value.is_null() {
            return Ok(None);
        }
        let Some(obj) = value.as_object() else {
            return Err(Error::BadRequest(
                "provider must be an object of routing preferences".to_string(),
            ));
        };
        let data_collection = match optional_str(obj, "data_collection")? {
            None => None,
            Some("allow") => Some(DataCollection::Allow),
            Some("deny") => Some(DataCollection::Deny),
            Some(other) => {
                return Err(Error::BadRequest(format!(
                    "provider.data_collection must be \"allow\" or \"deny\", not {other:?}"
                )))
            }
        };
        let max_price_per_million = match obj.get("max_price") {
            None | Some(Value::Null) => None,
            // A bare number is the total per million, which is how a candidate is priced.
            Some(Value::Number(n)) => Some(positive(n.as_f64(), "provider.max_price")?),
            Some(Value::Object(price)) => {
                let mut total = 0.0;
                for side in ["prompt", "completion"] {
                    if price.contains_key(side) {
                        total += number(price, side)?;
                    }
                }
                Some(positive(Some(total), "provider.max_price")?)
            }
            Some(_) => {
                return Err(Error::BadRequest(
                    "provider.max_price must be a number or {prompt, completion}".to_string(),
                ))
            }
        };
        Ok(Some(Self {
            order: strings(obj, "order")?,
            only: strings(obj, "only")?,
            ignore: strings(obj, "ignore")?,
            allow_fallbacks: boolean(obj, "allow_fallbacks")?,
            data_collection,
            zdr: boolean(obj, "zdr")?.unwrap_or(false),
            quantizations: quantizations(obj, "quantizations")?,
            max_price_per_million,
            require_parameters: boolean(obj, "require_parameters")?.unwrap_or(false),
        }))
    }

    /// Parse the `x-rrouter-*` header form. `header` is a lookup so a caller can pass the
    /// request's header map without building an intermediate collection.
    ///
    /// The header form exists for clients that cannot add a field to the body (proxies,
    /// OpenAI SDKs with a fixed request shape). Every header is optional and replaces the
    /// matching body field when present.
    pub fn from_headers<'a, F>(header: F) -> Result<Option<Self>>
    where
        F: Fn(&str) -> Option<&'a str>,
    {
        Ok(parse_headers(header)?.map(HeaderFields::into_policy))
    }

    /// Body policy with the header policy laid over it, field by field.
    ///
    /// Presence, not value, decides: a header that says `false` for a field the body
    /// said `true` for must win, which is why the header form is parsed into
    /// [`HeaderFields`] (where both booleans are `Option`) rather than into this struct.
    pub fn from_request<'a, F>(body: &Value, header: F) -> Result<Option<Self>>
    where
        F: Fn(&str) -> Option<&'a str>,
    {
        let from_body = Self::from_body(body)?;
        let from_headers = parse_headers(header)?;
        Ok(match (from_body, from_headers) {
            (None, None) => None,
            (Some(body), None) => Some(body),
            (None, Some(headers)) => Some(headers.into_policy()),
            (Some(body), Some(headers)) => Some(body.overlay(headers)),
        })
    }

    /// `over` (headers) wins wherever a header was present, including an explicit
    /// `false`.
    fn overlay(mut self, over: HeaderFields) -> Self {
        if !over.order.is_empty() {
            self.order = over.order;
        }
        if !over.only.is_empty() {
            self.only = over.only;
        }
        if !over.ignore.is_empty() {
            self.ignore = over.ignore;
        }
        if over.allow_fallbacks.is_some() {
            self.allow_fallbacks = over.allow_fallbacks;
        }
        if over.data_collection.is_some() {
            self.data_collection = over.data_collection;
        }
        if let Some(zdr) = over.zdr {
            self.zdr = zdr;
        }
        if !over.quantizations.is_empty() {
            self.quantizations = over.quantizations;
        }
        if over.max_price_per_million.is_some() {
            self.max_price_per_million = over.max_price_per_million;
        }
        if let Some(require_parameters) = over.require_parameters {
            self.require_parameters = require_parameters;
        }
        self
    }

    /// Position of `provider` in `order`, for intra-priority ordering. Unlisted
    /// providers sort after every listed one and keep their strategy order among
    /// themselves.
    pub fn order_position(&self, provider: &str) -> usize {
        self.order
            .iter()
            .position(|p| p == provider)
            .unwrap_or(usize::MAX)
    }
}

/// The `x-rrouter-*` header form, before it is merged into a [`ProviderPolicy`].
///
/// The two booleans that [`ProviderPolicy`] stores with a `false` default are `Option`
/// here on purpose: presence is the contract, so a header saying `false` can *lower* a
/// body field that said `true`, instead of being indistinguishable from "no header".
#[derive(Debug, Default)]
struct HeaderFields {
    order: Vec<String>,
    only: Vec<String>,
    ignore: Vec<String>,
    allow_fallbacks: Option<bool>,
    data_collection: Option<DataCollection>,
    zdr: Option<bool>,
    quantizations: Vec<String>,
    max_price_per_million: Option<f64>,
    require_parameters: Option<bool>,
}

impl HeaderFields {
    /// The header form read on its own: an absent boolean is `false`, exactly like a body
    /// field that was not sent.
    fn into_policy(self) -> ProviderPolicy {
        ProviderPolicy {
            order: self.order,
            only: self.only,
            ignore: self.ignore,
            allow_fallbacks: self.allow_fallbacks,
            data_collection: self.data_collection,
            zdr: self.zdr.unwrap_or(false),
            quantizations: self.quantizations,
            max_price_per_million: self.max_price_per_million,
            require_parameters: self.require_parameters.unwrap_or(false),
        }
    }
}

/// Parse the `x-rrouter-*` headers into their presence-tracking form.
///
/// `None` means no routing header was sent at all, so a request without preferences does
/// not pay for a merge.
fn parse_headers<'a, F>(header: F) -> Result<Option<HeaderFields>>
where
    F: Fn(&str) -> Option<&'a str>,
{
    let mut fields = HeaderFields::default();
    let mut any = false;
    if let Some(raw) = header("x-rrouter-provider-order") {
        fields.order = split_list(raw);
        any = true;
    }
    if let Some(raw) = header("x-rrouter-provider-only") {
        fields.only = split_list(raw);
        any = true;
    }
    if let Some(raw) = header("x-rrouter-provider-ignore") {
        fields.ignore = split_list(raw);
        any = true;
    }
    if let Some(raw) = header("x-rrouter-allow-fallbacks") {
        fields.allow_fallbacks = Some(parse_bool(raw, "x-rrouter-allow-fallbacks")?);
        any = true;
    }
    if let Some(raw) = header("x-rrouter-data-policy") {
        fields.data_collection = match raw.trim().to_ascii_lowercase().as_str() {
            "allow" => Some(DataCollection::Allow),
            "deny" => Some(DataCollection::Deny),
            _ => {
                return Err(Error::BadRequest(
                    "x-rrouter-data-policy must be \"allow\" or \"deny\"".to_string(),
                ))
            }
        };
        any = true;
    }
    if let Some(raw) = header("x-rrouter-zdr") {
        fields.zdr = Some(parse_bool(raw, "x-rrouter-zdr")?);
        any = true;
    }
    if let Some(raw) = header("x-rrouter-quantizations") {
        fields.quantizations = validate_quantizations(split_list(raw))?;
        any = true;
    }
    if let Some(raw) = header("x-rrouter-max-price") {
        let price = raw
            .trim()
            .parse::<f64>()
            .map_err(|_| Error::BadRequest("x-rrouter-max-price must be a number".to_string()))?;
        fields.max_price_per_million = Some(positive(Some(price), "x-rrouter-max-price")?);
        any = true;
    }
    if let Some(raw) = header("x-rrouter-require-parameters") {
        fields.require_parameters = Some(parse_bool(raw, "x-rrouter-require-parameters")?);
        any = true;
    }
    Ok(any.then_some(fields))
}

/// Does an allowlist cover the model string a client sent?
///
/// `allowed` comes from the database: `None` (NULL) allows every model, while an
/// explicitly empty list allows none. An entry matches when it is
///
/// - `*`,
/// - the model string itself,
/// - a `provider/*` glob covering the model's `provider/` prefix,
/// - the same model in its other form: a bare id matches the allowlist's full ref
///   (`openai/gpt-4.1` allows `gpt-4.1`), because the planner adds the provider and a
///   client should not be denied for spelling the model the shorter way.
///
/// A bare id is never covered by a `provider/*` glob: the provider is only known after
/// planning, and guessing it would widen the allowlist. This is the *pre-expansion*
/// check, used to refuse an obviously disallowed model early; it is not sufficient on
/// its own — see [`candidate_allowed`], which the planner applies to the expanded
/// `(provider, model)` pairs.
pub fn model_allowed(allowed: &[String], model: &str) -> bool {
    let bare = model.rsplit_once('/').map(|(_, id)| id).unwrap_or(model);
    allowed.iter().any(|raw| {
        let entry = raw.trim();
        if entry == "*" || entry == model {
            return true;
        }
        if let Some(provider) = entry.strip_suffix("/*") {
            return model.len() > provider.len()
                && model.starts_with(provider)
                && model.as_bytes()[provider.len()] == b'/';
        }
        entry.rsplit('/').next() == Some(bare)
    })
}

/// Does an allowlist cover one expanded `(provider, model)` candidate?
///
/// Stricter than [`model_allowed`] on purpose, and this is the check that decides
/// routing: a full ref pins the provider (`openai/gpt-4.1` never routes to
/// `github/gpt-4.1`, which a bare-id request would otherwise reach through the
/// catalog), a `provider/*` glob covers only that provider, `*` covers everything, and a
/// bare entry covers the model id on any provider — the spelling an operator uses when
/// they do not care which account serves it.
pub fn candidate_allowed(allowed: &[String], provider: &str, model: &str) -> bool {
    allowed.iter().any(|raw| {
        let entry = raw.trim();
        if entry == "*" {
            return true;
        }
        if let Some(name) = entry.strip_suffix("/*") {
            return provider == name;
        }
        match entry.split_once('/') {
            Some((name, id)) => name == provider && id == model,
            // A bare entry names the model, not an account.
            None => entry == model,
        }
    })
}

// ----- parsing helpers ---------------------------------------------------------------

fn strings(obj: &Map<String, Value>, key: &str) -> Result<Vec<String>> {
    let Some(value) = obj.get(key) else {
        return Ok(Vec::new());
    };
    let Some(items) = value.as_array() else {
        return Err(Error::BadRequest(format!(
            "provider.{key} must be an array of provider ids"
        )));
    };
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let Some(text) = item.as_str() else {
            return Err(Error::BadRequest(format!(
                "provider.{key} must contain only provider ids"
            )));
        };
        let text = text.trim();
        if !text.is_empty() && !out.iter().any(|seen: &String| seen == text) {
            out.push(text.to_string());
        }
    }
    Ok(out)
}

fn quantizations(obj: &Map<String, Value>, key: &str) -> Result<Vec<String>> {
    validate_quantizations(strings(obj, key)?)
}

fn validate_quantizations(values: Vec<String>) -> Result<Vec<String>> {
    let mut out = Vec::with_capacity(values.len());
    for value in values {
        let normalized = value.to_ascii_lowercase();
        if !QUANTIZATIONS.contains(&normalized.as_str()) {
            return Err(Error::BadRequest(format!(
                "unsupported quantization {value:?}; expected one of {}",
                QUANTIZATIONS.join(", ")
            )));
        }
        if !out.contains(&normalized) {
            out.push(normalized);
        }
    }
    Ok(out)
}

fn boolean(obj: &Map<String, Value>, key: &str) -> Result<Option<bool>> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(flag)) => Ok(Some(*flag)),
        Some(_) => Err(Error::BadRequest(format!(
            "provider.{key} must be true or false"
        ))),
    }
}

fn optional_str<'a>(obj: &'a Map<String, Value>, key: &str) -> Result<Option<&'a str>> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.trim())),
        Some(_) => Err(Error::BadRequest(format!(
            "provider.{key} must be a string"
        ))),
    }
}

fn number(obj: &Map<String, Value>, key: &str) -> Result<f64> {
    match obj.get(key) {
        Some(Value::Number(n)) => positive(n.as_f64(), &format!("provider.max_price.{key}")),
        _ => Err(Error::BadRequest(format!(
            "provider.max_price.{key} must be a number"
        ))),
    }
}

fn positive(value: Option<f64>, what: &str) -> Result<f64> {
    match value {
        Some(value) if value.is_finite() && value >= 0.0 => Ok(value),
        _ => Err(Error::BadRequest(format!(
            "{what} must be a finite, non-negative number"
        ))),
    }
}

fn parse_bool(raw: &str, header: &str) -> Result<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Ok(true),
        "false" | "0" | "no" | "off" => Ok(false),
        _ => Err(Error::BadRequest(format!("{header} must be true or false"))),
    }
}

/// Comma-separated list, trimmed, empty entries dropped, duplicates collapsed.
fn split_list(raw: &str) -> Vec<String> {
    let mut out = Vec::new();
    for part in raw.split(',') {
        let part = part.trim();
        if !part.is_empty() && !out.iter().any(|seen: &String| seen == part) {
            out.push(part.to_string());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn headers<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<&'a str> + 'a {
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(name))
                .map(|(_, value)| *value)
        }
    }

    #[test]
    fn body_object_parses_every_supported_field() {
        let body = json!({
            "model": "gpt-4.1",
            "provider": {
                "order": ["openai", "github", "openai"],
                "only": ["openai", "github"],
                "ignore": ["openrouter"],
                "allow_fallbacks": false,
                "data_collection": "deny",
                "zdr": true,
                "quantizations": ["FP16", "int8"],
                "max_price": {"prompt": 1.5, "completion": 2.5},
                "require_parameters": true,
                // Unsupported OpenRouter field: ignored, not an error.
                "sort": "throughput"
            }
        });
        let policy = ProviderPolicy::from_body(&body).unwrap().unwrap();
        assert_eq!(policy.order, ["openai", "github"]);
        assert_eq!(policy.only, ["openai", "github"]);
        assert_eq!(policy.ignore, ["openrouter"]);
        assert_eq!(policy.allow_fallbacks, Some(false));
        assert_eq!(policy.data_collection, Some(DataCollection::Deny));
        assert!(policy.zdr);
        assert_eq!(policy.quantizations, ["fp16", "int8"]);
        assert_eq!(policy.max_price_per_million, Some(4.0));
        assert!(policy.require_parameters);
    }

    #[test]
    fn body_without_provider_object_is_no_policy() {
        assert_eq!(
            ProviderPolicy::from_body(&json!({"model": "x"})).unwrap(),
            None
        );
        assert_eq!(
            ProviderPolicy::from_body(&json!({"model": "x", "provider": null})).unwrap(),
            None
        );
    }

    #[test]
    fn non_object_provider_is_a_bad_request() {
        let err = ProviderPolicy::from_body(&json!({"provider": ["openai"]})).unwrap_err();
        assert!(matches!(err, Error::BadRequest(_)));
    }

    #[test]
    fn unknown_data_policy_is_bad_request() {
        let err = ProviderPolicy::from_body(&json!({"provider": {"data_collection": "sometimes"}}))
            .unwrap_err();
        assert_eq!(err.status(), 400);
    }

    #[test]
    fn unknown_quantization_is_bad_request() {
        let err = ProviderPolicy::from_body(&json!({"provider": {"quantizations": ["int3"]}}))
            .unwrap_err();
        assert_eq!(err.status(), 400);
    }

    #[test]
    fn negative_max_price_is_refused() {
        let err = ProviderPolicy::from_body(&json!({"provider": {"max_price": -1}})).unwrap_err();
        assert_eq!(err.status(), 400);
    }

    #[test]
    fn headers_parse_and_override_body_fields() {
        let body = json!({
            "provider": {
                "order": ["openai"],
                "allow_fallbacks": true,
                "data_collection": "allow"
            }
        });
        let policy = ProviderPolicy::from_request(
            &body,
            headers(&[
                ("x-rrouter-provider-order", "anthropic, github"),
                ("x-rrouter-allow-fallbacks", "false"),
                ("x-rrouter-data-policy", "deny"),
                ("x-rrouter-zdr", "true"),
                ("x-rrouter-quantizations", "int4"),
                ("x-rrouter-max-price", "3.5"),
            ]),
        )
        .unwrap()
        .unwrap();
        assert_eq!(policy.order, ["anthropic", "github"]);
        assert_eq!(policy.allow_fallbacks, Some(false));
        assert_eq!(policy.data_collection, Some(DataCollection::Deny));
        assert!(policy.zdr);
        assert_eq!(policy.quantizations, ["int4"]);
        assert_eq!(policy.max_price_per_million, Some(3.5));
    }

    /// Regression: `zdr` and `require_parameters` are `bool` on the public type, so a
    /// header that said `false` used to be indistinguishable from an absent header and
    /// the body's `true` survived. The header is documented to win.
    #[test]
    fn explicit_false_header_overrides_body_true() {
        let body = json!({
            "provider": {"zdr": true, "require_parameters": true}
        });
        let policy = ProviderPolicy::from_request(
            &body,
            headers(&[
                ("x-rrouter-zdr", "false"),
                ("x-rrouter-require-parameters", "false"),
            ]),
        )
        .unwrap()
        .unwrap();
        assert!(!policy.zdr, "an explicit zdr=false header must win");
        assert!(
            !policy.require_parameters,
            "an explicit require-parameters=false header must win"
        );

        // The `0` spelling is the same statement.
        let policy = ProviderPolicy::from_request(
            &body,
            headers(&[
                ("x-rrouter-zdr", "0"),
                ("x-rrouter-require-parameters", "off"),
            ]),
        )
        .unwrap()
        .unwrap();
        assert!(!policy.zdr && !policy.require_parameters);
    }

    /// The direction that already worked: a header may also raise a body field.
    #[test]
    fn explicit_true_header_overrides_body_false() {
        let body = json!({
            "provider": {"zdr": false, "require_parameters": false}
        });
        let policy = ProviderPolicy::from_request(
            &body,
            headers(&[
                ("x-rrouter-zdr", "true"),
                ("x-rrouter-require-parameters", "true"),
            ]),
        )
        .unwrap()
        .unwrap();
        assert!(policy.zdr);
        assert!(policy.require_parameters);
    }

    /// No header for these fields leaves the body's value alone, including `false`.
    #[test]
    fn absent_header_keeps_the_body_value() {
        let body = json!({"provider": {"zdr": true, "require_parameters": false}});
        let policy =
            ProviderPolicy::from_request(&body, headers(&[("x-rrouter-provider-only", "openai")]))
                .unwrap()
                .unwrap();
        assert!(policy.zdr);
        assert!(!policy.require_parameters);
    }

    #[test]
    fn headers_alone_are_a_policy() {
        let policy = ProviderPolicy::from_request(&json!({}), headers(&[("x-rrouter-zdr", "1")]))
            .unwrap()
            .unwrap();
        assert!(policy.zdr);
        assert!(policy.order.is_empty());
    }

    #[test]
    fn absent_inputs_mean_no_policy() {
        assert_eq!(
            ProviderPolicy::from_request(&json!({"model": "x"}), headers(&[])).unwrap(),
            None
        );
    }

    #[test]
    fn bad_header_value_is_a_bad_request() {
        let err = ProviderPolicy::from_request(&json!({}), headers(&[("x-rrouter-zdr", "maybe")]))
            .unwrap_err();
        assert_eq!(err.status(), 400);
        let err =
            ProviderPolicy::from_request(&json!({}), headers(&[("x-rrouter-max-price", "cheap")]))
                .unwrap_err();
        assert_eq!(err.status(), 400);
    }

    #[test]
    fn order_position_puts_unlisted_providers_last() {
        let policy = ProviderPolicy {
            order: vec!["github".into(), "openai".into()],
            ..ProviderPolicy::default()
        };
        assert_eq!(policy.order_position("github"), 0);
        assert_eq!(policy.order_position("openai"), 1);
        assert_eq!(policy.order_position("anthropic"), usize::MAX);
    }

    #[test]
    fn model_allowlist_keeps_the_bare_form_rule() {
        let exact = vec!["claude/claude-haiku-4-5-20251001".to_string()];
        assert!(model_allowed(&exact, "claude/claude-haiku-4-5-20251001"));
        assert!(model_allowed(&exact, "claude-haiku-4-5-20251001"));
        assert!(!model_allowed(&exact, "claude-sonnet-4-5"));

        let glob = vec!["claude/*".to_string()];
        assert!(model_allowed(&glob, "claude/anything"));
        assert!(!model_allowed(&glob, "openai/gpt-4.1"));
        assert!(!model_allowed(&glob, "anything"));
        assert!(model_allowed(&["*".to_string()], "whatever"));
        assert!(!model_allowed(&[], "claude/haiku"));
    }

    /// The regression the audit found: a bare-id request resolves through the catalog to
    /// several providers, so a provider-scoped allowlist entry must not leak the request
    /// to a different provider that happens to serve the same id.
    #[test]
    fn candidate_allowlist_is_provider_scoped() {
        let allowed = vec!["openai/gpt-4.1".to_string()];
        assert!(candidate_allowed(&allowed, "openai", "gpt-4.1"));
        assert!(!candidate_allowed(&allowed, "github", "gpt-4.1"));

        let glob = vec!["claude/*".to_string()];
        assert!(candidate_allowed(&glob, "claude", "claude-sonnet-4-5"));
        assert!(!candidate_allowed(&glob, "openai", "gpt-4.1"));

        // A bare entry names the model, not an account.
        let bare = vec!["gpt-4.1".to_string()];
        assert!(candidate_allowed(&bare, "openai", "gpt-4.1"));
        assert!(candidate_allowed(&bare, "github", "gpt-4.1"));
        assert!(!candidate_allowed(&bare, "openai", "gpt-4.1-mini"));

        assert!(candidate_allowed(&["*".to_string()], "anything", "at-all"));
        assert!(!candidate_allowed(&[], "openai", "gpt-4.1"));
    }
}
