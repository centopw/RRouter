//! `/api/models/providers` — provider shorthands and operator-defined providers.
//!
//! A provider's code is the segment model refs use in place of its id, and it must be
//! unique in the instance: two codes mapping to one provider would be a second spelling,
//! two providers sharing one code would make a ref ambiguous. Built-in providers can only
//! have their code changed; a custom provider also owns a display name, a protocol and a
//! base URL, and lives in the same id namespace as a compiled one.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::Response;
use rrouter_core::Error;
use rrouter_store::ProviderProfile;
use serde_json::{json, Value};

use super::{admin_error, body_json, ok_json};
use crate::provider_catalog::{self, MAX_CODE_LEN};
use crate::state::AppState;

/// Longest display name accepted for a custom provider.
const MAX_NAME_LEN: usize = 64;

/// `PATCH /api/models/providers/{id}` with `{code}`.
pub async fn patch(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    peer: crate::auth::Peer,
    Path(provider): Path<String>,
    body: Body,
) -> Response {
    let value = match body_json(body).await {
        Ok(v) => v,
        Err(resp) => return *resp,
    };
    if state.provider_spec(&provider).is_none() {
        return admin_error(&Error::NotFound(format!("provider {provider}")));
    }
    let Some(code) = value
        .get("code")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|code| !code.is_empty())
    else {
        return admin_error(&Error::BadRequest("code is required".into()));
    };
    if let Err(e) = check_code(&state, code, &provider).await {
        return admin_error(&e);
    }
    match state.store.set_provider_code(&provider, code).await {
        Ok(true) => {}
        Ok(false) => return admin_error(&Error::NotFound(format!("provider {provider}"))),
        Err(e) => return admin_error(&e),
    }
    if let Err(e) = state.reload_providers().await {
        return admin_error(&e);
    }
    audit(
        &state,
        &headers,
        peer,
        "provider_code_changed",
        &format!("{provider} code {code}"),
    )
    .await;
    ok_json(provider_json(&state, &provider))
}

/// `POST /api/models/providers` with `{name, code?, base_url, dialect}`.
///
/// `dialect` is `openai` (Chat Completions) or `anthropic` (Messages). The id is derived
/// from the name and de-duplicated, so a repeated name never fails the request.
pub async fn create(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    peer: crate::auth::Peer,
    body: Body,
) -> Response {
    let value = match body_json(body).await {
        Ok(v) => v,
        Err(resp) => return *resp,
    };
    let Some(name) = value
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
    else {
        return admin_error(&Error::BadRequest("name is required".into()));
    };
    if name.len() > MAX_NAME_LEN {
        return admin_error(&Error::BadRequest(format!(
            "name is longer than {MAX_NAME_LEN} characters"
        )));
    }
    let Some(dialect) = value
        .get("dialect")
        .and_then(Value::as_str)
        .filter(|name| provider_catalog::parse_dialect(name).is_some())
    else {
        return admin_error(&Error::BadRequest(
            "dialect must be \"openai\" or \"anthropic\"".into(),
        ));
    };
    let base_url = match base_url(value.get("base_url")) {
        Ok(url) => url,
        Err(e) => return admin_error(&e),
    };
    let provider = unique_id(&state, name);
    let default_code = || {
        let mut code = provider_catalog::slug(name);
        code.truncate(MAX_CODE_LEN);
        code
    };
    let code = match value.get("code").and_then(Value::as_str).map(str::trim) {
        Some(code) if !code.is_empty() => code.to_string(),
        _ => default_code(),
    };
    if let Err(e) = check_code(&state, &code, &provider).await {
        return admin_error(&e);
    }
    let now = crate::now_unix();
    let profile = ProviderProfile {
        provider: provider.clone(),
        code: code.clone(),
        display: Some(name.to_string()),
        dialect: Some(dialect.to_string()),
        base_url: Some(base_url),
        created_at: now,
    };
    if let Err(e) = state.store.create_custom_provider(&profile).await {
        return admin_error(&e);
    }
    if let Err(e) = state.reload_providers().await {
        return admin_error(&e);
    }
    audit(
        &state,
        &headers,
        peer,
        "provider_created",
        &format!("{provider} ({name}) code {code}"),
    )
    .await;
    ok_json(provider_json(&state, &provider))
}

/// `DELETE /api/models/providers/{id}` — only an operator-defined provider, and only
/// when no account still routes through it.
pub async fn delete(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    peer: crate::auth::Peer,
    Path(provider): Path<String>,
) -> Response {
    if !state.provider_is_custom(&provider) {
        return admin_error(&Error::NotFound(format!("custom provider {provider}")));
    }
    match state.store.list_connections(Some(&provider)).await {
        Ok(connections) if !connections.is_empty() => {
            return admin_error(&Error::BadRequest(format!(
                "remove the {} account(s) for {provider} first",
                connections.len()
            )))
        }
        Ok(_) => {}
        Err(e) => return admin_error(&e),
    }
    match state.store.delete_custom_provider(&provider).await {
        Ok(true) => {}
        Ok(false) => return admin_error(&Error::NotFound(format!("custom provider {provider}"))),
        Err(e) => return admin_error(&e),
    }
    if let Err(e) = state.reload_providers().await {
        return admin_error(&e);
    }
    audit(
        &state,
        &headers,
        peer,
        "provider_deleted",
        provider.as_str(),
    )
    .await;
    ok_json(json!({ "deleted": true }))
}

/// One provider as the admin API shows it. `custom` rows carry their transport; built-ins
/// omit the fields the binary owns.
fn provider_json(state: &AppState, provider: &str) -> Value {
    let spec = state.provider_spec(provider);
    let profile = state
        .providers
        .read()
        .ok()
        .and_then(|catalog| catalog.profile(provider).cloned());
    let custom = profile.as_ref().is_some_and(|p| p.display.is_some());
    json!({
        "id": provider,
        "display": profile
            .as_ref()
            .and_then(|p| p.display.clone())
            .or_else(|| spec.map(|spec| spec.display.to_string()))
            .unwrap_or_else(|| provider.to_string()),
        "code": state.provider_code(provider),
        "custom": custom,
        "kind": if custom { "api_key" } else { "compiled" },
        "dialect": profile
            .as_ref()
            .and_then(|p| p.dialect.clone())
            .or_else(|| spec.and_then(|s| provider_catalog::dialect_name(s.default_dialect).map(str::to_string))),
        "base_url": profile.as_ref().and_then(|p| p.base_url.clone()),
    })
}

/// Validate a shorthand against the syntax and the instance's unique namespace.
async fn check_code(state: &AppState, code: &str, provider: &str) -> Result<(), Error> {
    if !provider_catalog::valid_code(code) {
        return Err(Error::BadRequest(format!(
            "code {code:?} must be 1-{MAX_CODE_LEN} characters of a-z, 0-9, '-', '_' or '.', \
             and must not be \"rrouter\""
        )));
    }
    if let Some(owner) = state.provider_by_ref(code) {
        if owner != provider {
            return Err(Error::BadRequest(format!(
                "code {code:?} already names provider {owner}"
            )));
        }
    }
    if state
        .store
        .code_taken(code, Some(provider))
        .await
        .map_err(|e| Error::Store(e.to_string()))?
    {
        return Err(Error::BadRequest(format!("code {code:?} is taken")));
    }
    Ok(())
}

/// A provider id derived from `name`, suffixed when the id namespace already has it.
fn unique_id(state: &AppState, name: &str) -> String {
    let base = provider_catalog::slug(name);
    if state.provider_spec(&base).is_none() && base != "rrouter" {
        return base;
    }
    for n in 2..1000 {
        let candidate = format!("{base}-{n}");
        if state.provider_spec(&candidate).is_none() {
            return candidate;
        }
    }
    format!("{base}-{}", uuid::Uuid::new_v4().simple())
}

/// Parse and normalize the operator's base URL: an http(s) origin or path, without a
/// query or fragment, trailing slash removed. The proxy grafts the provider's path onto
/// the origin, so a full chat URL is accepted and read the same way.
fn base_url(raw: Option<&Value>) -> Result<String, Error> {
    let Some(raw) = raw.and_then(Value::as_str).map(str::trim) else {
        return Err(Error::BadRequest("base_url is required".into()));
    };
    let parsed = reqwest::Url::parse(raw)
        .map_err(|_| Error::BadRequest("base_url must be an absolute http(s) URL".into()))?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err(Error::BadRequest(
            "base_url must be an absolute http(s) URL".into(),
        ));
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err(Error::BadRequest(
            "base_url must not carry a query or fragment".into(),
        ));
    }
    Ok(raw.trim_end_matches('/').to_string())
}

/// One audit row for a provider mutation; best effort, like every other admin trail.
async fn audit(
    state: &Arc<AppState>,
    headers: &HeaderMap,
    peer: crate::auth::Peer,
    kind: &str,
    detail: &str,
) {
    let actor = super::auth::actor(state, headers).await;
    super::auth::audit(
        state,
        kind,
        Some(&actor),
        &crate::auth::client_ip(headers, peer.0),
        detail,
    )
    .await;
}
