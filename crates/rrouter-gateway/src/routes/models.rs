//! `/v1/models`, the model registry admin routes, and the alias / disabled-model routes.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::Response;
use rrouter_auth::ProviderSpec;
use rrouter_core::{ChatRequest, ChatResponse, Dialect, Error, Message, Part, Role};
use rrouter_store::{Connection, ModelRow};
use serde_json::{json, Map, Value};
use uuid::Uuid;

use super::{admin_error, body_json, ok_json};
use crate::models_registry::{self, SOURCE_CUSTOM, SOURCE_UPSTREAM};
use crate::state::AppState;

/// Models a client can actually call: the registry (already limited to connected
/// providers) plus aliases and combos.
pub async fn list_models(State(state): State<Arc<AppState>>) -> Response {
    let mut entries: BTreeMap<String, String> = BTreeMap::new();
    match models_registry::list(&state).await {
        Ok(items) => {
            for item in items {
                entries.entry(item.id).or_insert(item.provider);
            }
        }
        Err(e) => return admin_error(&e),
    }
    if let Ok(aliases) = state.store.list_aliases().await {
        for alias in aliases {
            entries
                .entry(alias.alias)
                .or_insert_with(|| "rrouter".to_string());
        }
    }
    if let Ok(combos) = state.store.list_combos().await {
        for combo in combos {
            entries
                .entry(combo.name)
                .or_insert_with(|| "rrouter".to_string());
        }
    }

    let created = crate::now_unix();
    let data: Vec<Value> = entries
        .into_iter()
        .map(|(id, owned_by)| {
            json!({ "id": id, "object": "model", "owned_by": owned_by, "created": created })
        })
        .collect();
    ok_json(json!({ "object": "list", "data": data }))
}

/// `GET /v1/models/{id}`: the OpenAI model object for a routable `id` or `provider/id`.
///
/// Routable means the registry lists the model for a provider connected right now and
/// the model is not disabled; aliases and combos are routable names the gateway owns.
/// Anything else is the OpenAI 404 `model_not_found`, never a guess.
pub async fn get_model(
    State(state): State<Arc<AppState>>,
    Path(reference): Path<String>,
) -> Response {
    let reference = reference.trim_start_matches('/');
    match routable_owner(&state, reference).await {
        Ok(Some(owned_by)) => ok_json(json!({
            "id": reference,
            "object": "model",
            "created": crate::now_unix(),
            "owned_by": owned_by,
        })),
        Ok(None) => crate::error::error_response(
            Dialect::OpenAiChat,
            &Error::NotFound(format!("model {reference} is not available")),
        ),
        Err(e) => crate::error::error_response(Dialect::OpenAiChat, &e),
    }
}

/// Which provider owns `reference`, when the gateway can route it.
async fn routable_owner(state: &AppState, reference: &str) -> rrouter_core::Result<Option<String>> {
    // `provider/id` only splits when the prefix is a registry provider: ids such as
    // `meta-llama/Llama-3-70B` carry a slash of their own.
    let (provider, bare) = match reference.split_once('/') {
        Some((prefix, rest)) if rrouter_auth::get(prefix).is_some() => (Some(prefix), rest),
        _ => (None, reference),
    };
    let items = models_registry::list(state).await?;
    if let Some(item) = items
        .iter()
        .find(|item| item.enabled && item.id == bare && provider.is_none_or(|p| item.provider == p))
    {
        return Ok(Some(item.provider.clone()));
    }
    if state
        .store
        .list_aliases()
        .await?
        .iter()
        .any(|a| a.alias == reference)
    {
        return Ok(Some("rrouter".to_string()));
    }
    if state
        .store
        .list_combos()
        .await?
        .iter()
        .any(|c| c.name == reference)
    {
        return Ok(Some("rrouter".to_string()));
    }
    Ok(None)
}

/// Fetch a connection's models in the background, right after it is added.
///
/// The registry lists a provider's real catalog without an extra click; a provider
/// that cannot enumerate (codex) or a failed fetch is logged and otherwise ignored —
/// the catalog rows remain.
pub fn spawn_initial_fetch(state: Arc<AppState>, id: Uuid) {
    tokio::spawn(async move {
        let Ok(Some(conn)) = state.store.get_connection(id).await else {
            return;
        };
        let Some(spec) = rrouter_auth::get(&conn.provider) else {
            return;
        };
        if !models_registry::can_list(spec.id) {
            return;
        }
        if let Err(e) = fetch_upstream(&state, spec, &conn).await {
            tracing::debug!(provider = %conn.provider, error = %e, "initial model fetch failed");
        }
    });
}

pub async fn list_aliases(State(state): State<Arc<AppState>>) -> Response {
    match state.store.list_aliases().await {
        Ok(aliases) => {
            let data: Vec<Value> = aliases
                .into_iter()
                .map(|a| json!({ "alias": a.alias, "target": a.target }))
                .collect();
            ok_json(json!({ "data": data }))
        }
        Err(e) => admin_error(&e),
    }
}

/// `POST /api/aliases` with `{alias, target}`.
pub async fn create_alias(State(state): State<Arc<AppState>>, body: Body) -> Response {
    upsert_alias(&state, body).await
}

/// `PUT /api/aliases` with `{alias, target}`.
pub async fn update_alias(State(state): State<Arc<AppState>>, body: Body) -> Response {
    upsert_alias(&state, body).await
}

async fn upsert_alias(state: &AppState, body: Body) -> Response {
    let value = match body_json(body).await {
        Ok(v) => v,
        Err(resp) => return *resp,
    };
    let (Some(alias), Some(target)) = (str_field(&value, "alias"), str_field(&value, "target"))
    else {
        return admin_error(&Error::BadRequest("alias and target are required".into()));
    };
    match state.store.upsert_alias(&alias, &target).await {
        Ok(()) => ok_json(json!({ "alias": alias, "target": target })),
        Err(e) => admin_error(&e),
    }
}

/// `DELETE /api/aliases?alias=x` (or `{alias}` in the body).
pub async fn delete_alias(
    State(state): State<Arc<AppState>>,
    req: axum::extract::Request,
) -> Response {
    let alias = match alias_from_request(req).await {
        Ok(a) => a,
        Err(resp) => return *resp,
    };
    match state.store.delete_alias(&alias).await {
        Ok(true) => ok_json(json!({ "deleted": true, "alias": alias })),
        Ok(false) => admin_error(&Error::NotFound(format!("alias {alias}"))),
        Err(e) => admin_error(&e),
    }
}

async fn alias_from_request(req: axum::extract::Request) -> Result<String, Box<Response>> {
    let query = req.uri().query().unwrap_or("");
    if let Some(alias) = query_param(query, "alias") {
        return Ok(alias);
    }
    let (_, body) = req.into_parts();
    let value = body_json(body).await?;
    str_field(&value, "alias")
        .ok_or_else(|| Box::new(admin_error(&Error::BadRequest("alias is required".into()))))
}

pub async fn disable(State(state): State<Arc<AppState>>, Path(model): Path<String>) -> Response {
    set_disabled(&state, &model, true).await
}

pub async fn enable(State(state): State<Arc<AppState>>, Path(model): Path<String>) -> Response {
    set_disabled(&state, &model, false).await
}

async fn set_disabled(state: &AppState, model: &str, disabled: bool) -> Response {
    match state.store.set_disabled(model, disabled).await {
        Ok(()) => ok_json(json!({ "model": model, "disabled": disabled })),
        Err(e) => admin_error(&e),
    }
}

// ----- model registry --------------------------------------------------------------

/// `GET /api/models`: the merged registry, sorted by `provider`, then `id`.
pub async fn registry_list(State(state): State<Arc<AppState>>) -> Response {
    match models_registry::list(&state).await {
        Ok(items) => ok_json(json!({ "data": items })),
        Err(e) => admin_error(&e),
    }
}

/// `GET /api/models/providers`: every routable provider, with whether it has an active
/// connection and whether it can enumerate its own models.
pub async fn providers(State(state): State<Arc<AppState>>) -> Response {
    let active: BTreeSet<String> = match state.store.list_connections(None).await {
        Ok(connections) => connections
            .into_iter()
            .filter(|conn| conn.is_active)
            .map(|conn| conn.provider)
            .collect(),
        Err(e) => return admin_error(&e),
    };
    let mut data: Vec<Value> = rrouter_auth::PROVIDERS
        .iter()
        .map(|spec| {
            json!({
                "id": spec.id,
                "display": spec.display,
                // How this provider authenticates, so the dashboard can offer
                // "subscription" and "API key" as separate paths instead of guessing.
                "kind": match spec.kind {
                    rrouter_auth::ProviderKind::OAuth => "oauth",
                    rrouter_auth::ProviderKind::ApiKey => "api_key",
                    rrouter_auth::ProviderKind::Local => "local",
                },
                "has_connection": active.contains(spec.id),
                "listed": models_registry::can_list(spec.id),
            })
        })
        .collect();
    data.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    ok_json(json!({ "data": data }))
}

/// `POST /api/models/sync`: replace every `synced` row from models.dev.
pub async fn sync(State(state): State<Arc<AppState>>) -> Response {
    match models_registry::sync(&state).await {
        Ok((models, providers, synced_at)) => ok_json(json!({
            "ok": true,
            "models": models,
            "providers": providers,
            "synced_at": synced_at,
        })),
        Err(e) => admin_error(&e),
    }
}

/// `POST /api/models/fetch` with `{connection_id}`: store the provider's own model list
/// as `upstream` rows.
pub async fn fetch(State(state): State<Arc<AppState>>, body: Body) -> Response {
    let value = match body_json(body).await {
        Ok(v) => v,
        Err(resp) => return *resp,
    };
    let Some(raw_id) = str_field(&value, "connection_id") else {
        return admin_error(&Error::BadRequest("connection_id is required".into()));
    };
    let Ok(id) = Uuid::parse_str(&raw_id) else {
        return admin_error(&Error::BadRequest("connection_id must be a uuid".into()));
    };
    let conn = match state.store.get_connection(id).await {
        Ok(Some(conn)) => conn,
        Ok(None) => return admin_error(&Error::NotFound(format!("connection {id}"))),
        Err(e) => return admin_error(&e),
    };
    let Some(spec) = rrouter_auth::get(&conn.provider) else {
        return admin_error(&Error::BadRequest(format!(
            "unknown provider {}",
            conn.provider
        )));
    };
    if !models_registry::can_list(spec.id) {
        // Contract: 400 plus an explanation the UI can show next to the disabled button.
        return admin_error(&Error::BadRequest(format!(
            "{} has no model list endpoint; add models with POST /api/models/custom",
            spec.id
        )));
    }
    match fetch_upstream(&state, spec, &conn).await {
        Ok(payload) => ok_json(payload),
        Err(e) => admin_error(&e),
    }
}

async fn fetch_upstream(
    state: &AppState,
    spec: &'static ProviderSpec,
    conn: &Connection,
) -> rrouter_core::Result<Value> {
    let creds = state
        .refresher
        .fresh(state.store.as_ref(), spec, conn.id)
        .await?;
    let base = creds.base_url_override.as_deref().unwrap_or(spec.base_url);
    let mut url = models_registry::models_url(base);
    // Anthropic's list paginates (default 20). Asking for the maximum page once avoids a
    // "latest models missing" list without implementing cursor paging this endpoint does
    // not need for its other providers.
    if matches!(conn.provider.as_str(), "claude" | "anthropic") && !url.contains('?') {
        url.push_str("?limit=1000");
    }
    let headers = provider_headers(spec, &creds)?;
    let response = state
        .http
        .get(&url)
        .headers(headers)
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(transport_error)?;
    let status = response.status().as_u16();
    if !response.status().is_success() {
        let body: String = response
            .text()
            .await
            .unwrap_or_default()
            .chars()
            .take(512)
            .collect();
        return Err(Error::Upstream {
            provider: conn.provider.clone(),
            status,
            body,
            payload: None,
        });
    }
    let body: Value = response.json().await.map_err(transport_error)?;
    let mut ids = models_registry::parse_model_ids(&body);
    ids.truncate(models_registry::MAX_FETCHED_IDS);

    let known: BTreeSet<(String, String)> = state
        .store
        .list_models()
        .await?
        .into_iter()
        .map(|row| (row.provider, row.model))
        .collect();
    let added = ids
        .iter()
        .filter(|id| !known.contains(&(conn.provider.clone(), (*id).clone())))
        .count();
    for id in &ids {
        state
            .store
            .upsert_model(&ModelRow {
                provider: conn.provider.clone(),
                model: id.clone(),
                display: None,
                context: None,
                max_output: None,
                caps: Value::Object(Map::new()),
                // A provider's own list carries ids, not prices; models.dev supplies them.
                cost: None,
                source: SOURCE_UPSTREAM.to_string(),
                updated_at: 0,
            })
            .await?;
    }
    Ok(json!({
        "provider": conn.provider,
        "status": status,
        "count": ids.len(),
        "added": added,
        "data": ids,
    }))
}

/// `POST /api/models/custom`: upsert one operator-defined row.
pub async fn custom(State(state): State<Arc<AppState>>, body: Body) -> Response {
    let value = match body_json(body).await {
        Ok(v) => v,
        Err(resp) => return *resp,
    };
    let (Some(provider), Some(id)) = (str_field(&value, "provider"), str_field(&value, "id"))
    else {
        return admin_error(&Error::BadRequest("provider and id are required".into()));
    };
    let row = ModelRow {
        provider: provider.clone(),
        model: id.clone(),
        display: str_field(&value, "display"),
        context: u32_field(&value, "context"),
        max_output: u32_field(&value, "max_output"),
        caps: capabilities_from(&value),
        cost: value.get("cost").cloned().filter(|c| c.is_object()),
        source: SOURCE_CUSTOM.to_string(),
        updated_at: 0,
    };
    if let Err(e) = state.store.upsert_model(&row).await {
        return admin_error(&e);
    }
    match models_registry::item(&state, &provider, &id).await {
        Ok(Some(item)) => ok_json(item),
        Ok(None) => admin_error(&Error::Store(format!(
            "custom row {provider}/{id} vanished"
        ))),
        Err(e) => admin_error(&e),
    }
}

/// `DELETE /api/models/custom?provider=…&id=…`: only a `custom` row may be removed this way.
pub async fn delete_custom(
    State(state): State<Arc<AppState>>,
    Query(query): Query<Vec<(String, String)>>,
) -> Response {
    let param = |key: &str| {
        query
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
            .filter(|v| !v.is_empty())
    };
    let (Some(provider), Some(id)) = (param("provider"), param("id")) else {
        return admin_error(&Error::BadRequest("provider and id are required".into()));
    };
    match state.store.delete_model(&provider, &id).await {
        Ok(true) => ok_json(json!({ "deleted": true })),
        Ok(false) => admin_error(&Error::NotFound(format!("custom model {provider}/{id}"))),
        Err(e) => admin_error(&e),
    }
}

/// `POST /api/models/test` with `{model}`: one real 16-token completion through the
/// planner and dispatcher the proxy uses.
pub async fn test(State(state): State<Arc<AppState>>, body: Body) -> Response {
    let value = match body_json(body).await {
        Ok(v) => v,
        Err(resp) => return *resp,
    };
    let Some(model) = str_field(&value, "model") else {
        return admin_error(&Error::BadRequest("model is required".into()));
    };
    let started = Instant::now();
    // Same entry point a client call takes, so "test" cannot pass on a code path the
    // proxy never runs (it did once: the probe bypassed forced streaming).
    let mut request = ChatRequest::new(model.clone());
    request.messages.push(Message::text(Role::User, "ping"));
    request.max_tokens = Some(16);
    let result = match rrouter_translate::build_request(Dialect::OpenAiChat, &request, false) {
        Ok(probe) => crate::proxy::complete(&state, Dialect::OpenAiChat, &probe).await,
        Err(e) => Err(e),
    };
    match result {
        Ok(response) => ok_json(json!({
            "ok": true,
            "status": 200,
            "latency_ms": elapsed_ms(started),
            "model": model,
            "reply": reply_text(&response),
        })),
        Err(e) => test_failure(&e, started),
    }
}

/// A failed probe is still an HTTP 200: the UI renders both outcomes.
fn test_failure(error: &Error, started: Instant) -> Response {
    ok_json(json!({
        "ok": false,
        "status": error.status(),
        "latency_ms": elapsed_ms(started),
        "error": error.to_string(),
    }))
}

fn elapsed_ms(started: Instant) -> u32 {
    started.elapsed().as_millis().min(u32::MAX as u128) as u32
}

fn reply_text(response: &ChatResponse) -> String {
    response
        .parts
        .iter()
        .filter_map(|part| match part {
            Part::Text(text) => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

/// Request headers for one outbound provider call, auth included.
fn provider_headers(
    spec: &ProviderSpec,
    creds: &rrouter_auth::Credentials,
) -> rrouter_core::Result<HeaderMap> {
    let mut headers = HeaderMap::new();
    for (name, value) in rrouter_auth::build_headers(spec, creds)? {
        if let (Ok(name), Ok(value)) = (
            axum::http::header::HeaderName::from_bytes(name.as_bytes()),
            axum::http::header::HeaderValue::from_str(&value),
        ) {
            headers.append(name, value);
        }
    }
    Ok(headers)
}

fn transport_error(e: reqwest::Error) -> Error {
    if e.is_timeout() {
        Error::Timeout(e.to_string())
    } else {
        Error::Transport(e.to_string())
    }
}

/// The nine known capability keys the body stated; absent keys fall through to the row
/// this one overlays.
fn capabilities_from(value: &Value) -> Value {
    const KEYS: [&str; 9] = [
        "vision",
        "pdf",
        "audio_input",
        "video_input",
        "image_output",
        "audio_output",
        "tools",
        "reasoning",
        "search",
    ];
    let mut caps = Map::new();
    let Some(given) = value.get("capabilities").and_then(Value::as_object) else {
        return Value::Object(caps);
    };
    for key in KEYS {
        if let Some(flag) = given.get(key).and_then(Value::as_bool) {
            caps.insert(key.to_string(), Value::Bool(flag));
        }
    }
    Value::Object(caps)
}

fn u32_field(value: &Value, key: &str) -> Option<u32> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
}

pub(crate) fn str_field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Minimal `?k=v&k2=v2` reader, percent-decoding `%XX` sequences.
pub(crate) fn query_param(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (percent_decode(k) == key).then(|| percent_decode(v))
    })
}

fn percent_decode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => match u8::from_str_radix(&raw[i + 1..i + 3], 16) {
                Ok(byte) => {
                    out.push(byte);
                    i += 3;
                }
                Err(_) => {
                    out.push(b'%');
                    i += 1;
                }
            },
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_params_decode() {
        assert_eq!(
            query_param("a=1&model=claude%2Fsonnet-4-5", "model").as_deref(),
            Some("claude/sonnet-4-5")
        );
        assert_eq!(query_param("alias=", "alias").as_deref(), Some(""));
        assert_eq!(query_param("x=1", "alias"), None);
    }
}
