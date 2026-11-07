//! `/api/connections` — provider credential CRUD and the 1-token probe.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use rrouter_auth::Credentials;
use rrouter_core::{ChatRequest, Error, Message, Role};
use rrouter_store::Connection;
use serde_json::{json, Map, Value};
use uuid::Uuid;

use super::{admin_error, body_json, ok_json};
use crate::state::AppState;

/// A connection as the admin API shows it: booleans, never token material.
#[derive(serde::Serialize)]
pub struct ConnectionView {
    pub id: String,
    pub provider: String,
    pub auth_type: String,
    pub name: Option<String>,
    pub email: Option<String>,
    pub priority: i32,
    /// Traffic share for the weighted strategy (`1..=1000`, 100 = even).
    pub weight: u32,
    pub is_active: bool,
    pub base_url_override: Option<String>,
    pub has_access_token: bool,
    pub has_refresh_token: bool,
    pub has_api_key: bool,
    pub expires_at: Option<i64>,
    pub backoff_level: u32,
    pub last_error: Option<String>,
    pub last_error_code: Option<u16>,
    pub created_at: i64,
    pub updated_at: i64,
}

impl From<&Connection> for ConnectionView {
    fn from(c: &Connection) -> Self {
        let creds = &c.credentials;
        Self {
            id: c.id.to_string(),
            provider: c.provider.clone(),
            auth_type: c.auth_type.clone(),
            name: c.name.clone(),
            email: c.email.clone(),
            priority: c.priority,
            weight: c.weight,
            is_active: c.is_active,
            base_url_override: creds.base_url_override.clone(),
            has_access_token: creds.access_token.as_deref().is_some_and(|t| !t.is_empty()),
            has_refresh_token: creds
                .refresh_token
                .as_deref()
                .is_some_and(|t| !t.is_empty()),
            has_api_key: creds.api_key.as_deref().is_some_and(|t| !t.is_empty()),
            expires_at: creds.expires_at,
            backoff_level: c.backoff_level,
            last_error: c.last_error.clone(),
            last_error_code: c.last_error_code,
            created_at: c.created_at,
            updated_at: c.updated_at,
        }
    }
}

pub async fn list(
    State(state): State<Arc<AppState>>,
    Query(query): Query<Vec<(String, String)>>,
) -> Response {
    let provider = query
        .iter()
        .find(|(k, _)| k == "provider")
        .map(|(_, v)| v.as_str());
    match state.store.list_connections(provider).await {
        Ok(conns) => {
            let data: Vec<ConnectionView> = conns.iter().map(ConnectionView::from).collect();
            ok_json(json!({ "data": data }))
        }
        Err(e) => admin_error(&e),
    }
}

/// Body: `{provider, auth_type, name?, email?, priority?, credentials:{…}}`.
pub async fn create(State(state): State<Arc<AppState>>, body: Body) -> Response {
    let value = match body_json(body).await {
        Ok(v) => v,
        Err(resp) => return *resp,
    };
    let Some(provider) = super::models::str_field(&value, "provider") else {
        return admin_error(&Error::BadRequest("provider is required".into()));
    };
    let Some(spec) = rrouter_auth::get(&provider) else {
        return admin_error(&Error::BadRequest(format!("unknown provider {provider}")));
    };
    let auth_type = super::models::str_field(&value, "auth_type")
        .unwrap_or_else(|| default_auth_type(spec).to_string());
    let name = super::models::str_field(&value, "name");
    let creds = match credentials_from(&value) {
        Ok(c) => c,
        Err(e) => return admin_error(&e),
    };
    match state
        .store
        .create_connection(&provider, &auth_type, creds, name.as_deref())
        .await
    {
        Ok(created) => {
            // The routing knobs are their own columns, so they are applied after the
            // row exists; a body that names neither leaves the defaults alone.
            let policy = json!({
                "priority": value.get("priority").cloned(),
                "weight": value.get("weight").cloned(),
            });
            if policy.get("priority").is_some_and(|v| !v.is_null())
                || policy.get("weight").is_some_and(|v| !v.is_null())
            {
                if let Err(e) = apply_patch(&state, created.id, policy).await {
                    return admin_error(&e);
                }
            }
            // List this provider's own models now, so the registry is useful immediately.
            super::models::spawn_initial_fetch(Arc::clone(&state), created.id);
            match state.store.get_connection(created.id).await {
                Ok(Some(c)) => {
                    (StatusCode::CREATED, axum::Json(ConnectionView::from(&c))).into_response()
                }
                Ok(None) => (
                    StatusCode::CREATED,
                    axum::Json(ConnectionView::from(&created)),
                )
                    .into_response(),
                Err(e) => admin_error(&e),
            }
        }
        Err(e) => admin_error(&e),
    }
}

/// `PATCH /api/connections/:id` with any subset of the create body.
pub async fn update(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    body: Body,
) -> Response {
    let Ok(uuid) = Uuid::parse_str(&id) else {
        return admin_error(&Error::BadRequest("connection id must be a uuid".into()));
    };
    let value = match body_json(body).await {
        Ok(v) => v,
        Err(resp) => return *resp,
    };
    if let Some(creds) = value.get("credentials") {
        let patch = match credentials_patch(creds) {
            Ok(p) => p,
            Err(e) => return admin_error(&e),
        };
        if let Err(e) = state.store.merge_credentials(uuid, patch).await {
            return admin_error(&e);
        }
    }
    if let Err(e) = apply_patch(&state, uuid, value.clone()).await {
        return admin_error(&e);
    }
    match state.store.get_connection(uuid).await {
        Ok(Some(c)) => ok_json(ConnectionView::from(&c)),
        Ok(None) => admin_error(&Error::NotFound(format!("connection {uuid}"))),
        Err(e) => admin_error(&e),
    }
}

pub async fn remove(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    let Ok(uuid) = Uuid::parse_str(&id) else {
        return admin_error(&Error::BadRequest("connection id must be a uuid".into()));
    };
    match state.store.delete_connection(uuid).await {
        Ok(true) => ok_json(json!({ "deleted": true, "id": uuid.to_string() })),
        Ok(false) => admin_error(&Error::NotFound(format!("connection {uuid}"))),
        Err(e) => admin_error(&e),
    }
}

/// `POST /api/connections/:id/test`: refresh credentials, then ask for one token.
pub async fn test(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    let Ok(uuid) = Uuid::parse_str(&id) else {
        return admin_error(&Error::BadRequest("connection id must be a uuid".into()));
    };
    let Some(conn) = (match state.store.get_connection(uuid).await {
        Ok(c) => c,
        Err(e) => return admin_error(&e),
    }) else {
        return admin_error(&Error::NotFound(format!("connection {uuid}")));
    };
    let Some(spec) = rrouter_auth::get(&conn.provider) else {
        return admin_error(&Error::BadRequest(format!(
            "unknown provider {}",
            conn.provider
        )));
    };
    let started = std::time::Instant::now();
    let result = probe(&state, spec, conn).await;
    let latency_ms = started.elapsed().as_millis().min(u32::MAX as u128) as u32;
    match result {
        Ok(status) => ok_json(json!({ "ok": true, "status": status, "latency_ms": latency_ms })),
        Err(e) => ok_json(json!({
            "ok": false,
            "status": e.status(),
            "latency_ms": latency_ms,
            // A 429 here is the provider guarding its account, not a broken credential.
            "error": if e.status() == 429 {
                "the provider is rate-limiting this account; retry in a minute".to_string()
            } else {
                e.to_string()
            },
        })),
    }
}

async fn probe(
    state: &Arc<AppState>,
    spec: &'static rrouter_auth::ProviderSpec,
    conn: Connection,
) -> Result<u16, Error> {
    let creds = state
        .refresher
        .fresh(state.store.as_ref(), spec, conn.id)
        .await?;
    // The probe must name a model the provider actually serves: sending a
    // placeholder like "ping" makes every provider answer 400 model_not_found,
    // which reads as a broken credential. Use the provider's cheapest known model.
    let model = probe_model(state, spec).await;
    let mut req = ChatRequest::new(model);
    req.messages.push(Message::text(Role::User, "ping"));
    // Some providers reject a 1-token completion (minimum output length), so ask for
    // a small but legal amount.
    req.max_tokens = Some(16);
    // The probe speaks the provider's own dialect at its own endpoint. Building the
    // body for Chat and posting it to a Responses URL (codex) is a guaranteed 400.
    let (url, dialect) = crate::proxy::endpoint_for(
        spec,
        creds.base_url_override.as_deref(),
        spec.default_dialect,
    );
    let body = rrouter_translate::build_request(dialect, &req, spec.stream_only)?;

    let mut headers = axum::http::HeaderMap::new();
    for (name, value) in rrouter_auth::build_headers(spec, &creds)? {
        if let (Ok(name), Ok(value)) = (
            axum::http::header::HeaderName::from_bytes(name.as_bytes()),
            axum::http::HeaderValue::from_str(&value),
        ) {
            headers.append(name, value);
        }
    }
    headers.insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/json"),
    );
    let resp = state
        .http
        .post(url)
        .headers(headers)
        .json(&body)
        .timeout(state.request_timeout())
        .send()
        .await
        .map_err(|e| {
            if e.is_timeout() {
                Error::Timeout(e.to_string())
            } else {
                Error::Transport(e.to_string())
            }
        })?;
    let status = resp.status().as_u16();
    if !resp.status().is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(Error::Upstream {
            provider: conn.provider.clone(),
            status,
            body: text.chars().take(512).collect(),
            payload: None,
        });
    }
    if spec.stream_only {
        // A stream-only upstream answers 200 before it has produced anything, so a
        // bare status would report a broken account as healthy. Require the first
        // chunk to arrive.
        use futures_util::StreamExt as _;
        let mut stream = resp.bytes_stream();
        match stream.next().await {
            Some(Ok(chunk)) if !chunk.is_empty() => {}
            Some(Ok(_)) => {
                return Err(Error::Upstream {
                    provider: conn.provider.clone(),
                    status,
                    body: "stream closed before the first chunk".into(),
                    payload: None,
                })
            }
            Some(Err(e)) => return Err(Error::Transport(e.to_string())),
            None => {
                return Err(Error::Upstream {
                    provider: conn.provider.clone(),
                    status,
                    body: "empty stream".into(),
                    payload: None,
                })
            }
        }
    }
    Ok(status)
}

/// A model this provider can serve, for connectivity probes.
///
/// Candidate ids come from the catalog plus every registry row for the provider (a
/// synced or operator row may know a model the built-in snapshot does not). Retired
/// families are excluded, then the order is: known price (cheapest first), small family,
/// newest version, id. A probe must never spend an expensive model's minimum output or
/// land on a model the provider has retired.
async fn probe_model(state: &AppState, spec: &rrouter_auth::ProviderSpec) -> String {
    let rows = state.store.list_models().await.unwrap_or_default();
    let vendor = rrouter_router::catalog::models_dev_provider(spec.id);
    let mut candidates: std::collections::BTreeSet<String> = state
        .catalog
        .all()
        .into_iter()
        .filter(|m| m.provider == spec.id)
        .map(|m| m.id)
        .collect();
    // A synced models.dev row describes a model; it does not prove this account can call
    // it. Keep those ids only when nothing proven exists, so a probe cannot fail on a
    // model the provider never listed.
    let mut proven: std::collections::BTreeSet<String> = candidates.clone();
    for row in &rows {
        if row.provider != spec.id {
            continue;
        }
        candidates.insert(row.model.clone());
        if row.source != "synced" {
            proven.insert(row.model.clone());
        }
    }
    if !proven.is_empty() {
        candidates = proven;
    }
    candidates.retain(|id| !rrouter_router::catalog::is_retired(spec.id, id));
    if candidates.is_empty() {
        return spec.id.to_string();
    }
    // Cheapest published input rate for a candidate; the provider's own row wins over
    // the models.dev vendor row of the same id.
    let rate = |id: &str| -> Option<f64> {
        let mut best: Option<(u8, f64)> = None;
        for row in rows.iter().filter(|r| r.model == id) {
            if row.provider != spec.id && row.provider != vendor {
                continue;
            }
            let Some(input) = row.cost.as_ref().and_then(|cost| cost.get("input")) else {
                continue;
            };
            let Some(input) = input.as_f64() else {
                continue;
            };
            let key = u8::from(row.provider == spec.id);
            let replace = match best {
                None => true,
                Some((best_key, best_rate)) => {
                    key > best_key || (key == best_key && input < best_rate)
                }
            };
            if replace {
                best = Some((key, input));
            }
        }
        best.map(|(_, input)| input)
    };
    let mut ids: Vec<String> = candidates.into_iter().collect();
    order_probe_candidates(&mut ids, rate);
    ids.into_iter()
        .next()
        .unwrap_or_else(|| spec.id.to_string())
}

/// Probe order: cheapest known price, then the smaller family, then the newest version,
/// then the id. Pure so the rule is testable without a provider.
fn order_probe_candidates(ids: &mut [String], rate: impl Fn(&str) -> Option<f64>) {
    ids.sort_by(|a, b| {
        let (ra, rb) = (rate(a), rate(b));
        match (ra, rb) {
            (Some(a), Some(b)) => a.partial_cmp(&b).unwrap_or(std::cmp::Ordering::Equal),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        }
        .then_with(|| {
            rrouter_router::catalog::probe_class(a).cmp(&rrouter_router::catalog::probe_class(b))
        })
        .then_with(|| rrouter_router::catalog::compare_newest(a, b))
        .then_with(|| a.cmp(b))
    });
}

fn default_auth_type(spec: &rrouter_auth::ProviderSpec) -> &'static str {
    match spec.kind {
        rrouter_auth::ProviderKind::OAuth => "oauth",
        rrouter_auth::ProviderKind::ApiKey => "api_key",
        rrouter_auth::ProviderKind::Local => "none",
    }
}

/// Fields an admin may write, minus the ones that live in their own columns.
async fn apply_patch(state: &AppState, id: Uuid, value: Value) -> Result<(), Error> {
    let Some(conn) = state.store.get_connection(id).await? else {
        return Err(Error::NotFound(format!("connection {id}")));
    };
    let mut patch = Credentials::default();
    let mut has_patch = false;
    if let Some(email) = super::models::str_field(&value, "email") {
        patch.account_label = Some(email);
        has_patch = true;
    }
    if let Some(url) = value.get("base_url_override").and_then(Value::as_str) {
        patch.base_url_override = Some(url.to_string());
        has_patch = true;
    }
    if has_patch {
        // `email` is mirrored into the credentials blob's display label.
        state.store.merge_credentials(id, patch).await?;
    }
    // Routing knobs live in their own columns. `priority` orders candidates and
    // `weight` shares traffic inside an equal-priority group; the store clamps the
    // weight to 1..=1000, so an out-of-range value is bounded rather than refused.
    if let Some(priority) = value.get("priority").and_then(Value::as_i64) {
        let priority = i32::try_from(priority)
            .map_err(|_| Error::BadRequest("priority must fit in 32 bits".into()))?;
        state.store.set_connection_priority(id, priority).await?;
    }
    if let Some(weight) = value.get("weight").and_then(Value::as_u64) {
        state
            .store
            .set_connection_weight(id, u32::try_from(weight).unwrap_or(u32::MAX))
            .await?;
    }
    if let Some(active) = value.get("is_active").and_then(Value::as_bool) {
        state.store.set_connection_active(id, active).await?;
        // Re-enabling also clears backoff: the operator just asked for this account
        // to be used, so the breaker must not keep skipping it.
        if active && conn.backoff_level > 0 {
            state
                .store
                .update_connection_status(id, 0, None, None)
                .await?;
        }
    }
    if let Some(error) = value.get("last_error").and_then(Value::as_str) {
        state
            .store
            .update_connection_status(id, conn.backoff_level, Some(error), None)
            .await?;
    }
    Ok(())
}

/// Extract the writable credential fields from a create body.
pub fn credentials_from(value: &Value) -> Result<Credentials, Error> {
    let Some(creds) = value.get("credentials").filter(|v| v.is_object()) else {
        return Ok(Credentials::default());
    };
    let mut out = credentials_patch(creds)?;
    if let Some(email) = super::models::str_field(value, "email") {
        out.account_label = Some(email);
    }
    Ok(out)
}

/// Credential fields present in `credentials`: only these are merged, so a PATCH never
/// wipes tokens it did not mention.
fn credentials_patch(value: &Value) -> Result<Credentials, Error> {
    let Value::Object(map) = value else {
        return Err(Error::BadRequest("credentials must be an object".into()));
    };
    let mut c = Credentials::default();
    let mut extra = Map::new();
    for (key, v) in map {
        match key.as_str() {
            "access_token" => c.access_token = string_or_none(v),
            "refresh_token" => c.refresh_token = string_or_none(v),
            "api_key" => c.api_key = string_or_none(v),
            "id_token" => c.id_token = string_or_none(v),
            "copilot_token" => c.copilot_token = string_or_none(v),
            "expires_at" => c.expires_at = v.as_i64(),
            "copilot_expires_at" => c.copilot_expires_at = v.as_i64(),
            "account_label" => c.account_label = string_or_none(v),
            "base_url_override" => c.base_url_override = string_or_none(v),
            _ => {
                extra.insert(key.clone(), v.clone());
            }
        }
    }
    c.extra = extra;
    Ok(c)
}

fn string_or_none(v: &Value) -> Option<String> {
    v.as_str().map(|s| s.to_string()).filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The probe never picks a retired model and prefers the cheapest current family;
    /// with no prices it falls back to family size, then version.
    #[test]
    fn probe_order_skips_retired_and_prefers_cheap_current_models() {
        let mut ids = vec![
            "claude-3-7-sonnet-latest".to_string(),
            "claude-opus-4-1".to_string(),
            "claude-sonnet-4-5".to_string(),
            "claude-haiku-4-5".to_string(),
        ];
        ids.retain(|id| !rrouter_router::catalog::is_retired("claude", id));
        order_probe_candidates(&mut ids, |_| None);
        assert_eq!(
            ids,
            vec![
                "claude-haiku-4-5".to_string(),
                "claude-sonnet-4-5".to_string(),
                "claude-opus-4-1".to_string(),
            ],
            "retired gone, cheap family first, newest version inside a family"
        );

        // A published price outranks family class.
        order_probe_candidates(&mut ids, |id| match id {
            "claude-opus-4-1" => Some(1.0),
            "claude-haiku-4-5" => Some(2.0),
            _ => None,
        });
        assert_eq!(ids[0], "claude-opus-4-1");
    }

    #[test]
    fn views_never_carry_token_values() {
        let creds = Credentials {
            access_token: Some("SECRET-A".into()),
            refresh_token: Some("SECRET-R".into()),
            api_key: Some("SECRET-K".into()),
            ..Credentials::default()
        };
        let conn = Connection {
            id: Uuid::nil(),
            provider: "openai".into(),
            auth_type: "api_key".into(),
            name: None,
            email: None,
            priority: 0,
            weight: 100,
            is_active: true,
            credentials: creds,
            backoff_level: 0,
            last_error: None,
            last_error_code: None,
            last_error_at: None,
            created_at: 0,
            updated_at: 0,
        };
        let encoded = serde_json::to_string(&ConnectionView::from(&conn)).expect("serialize");
        assert!(!encoded.contains("SECRET"));
        assert!(encoded.contains("\"has_access_token\":true"));
    }

    #[test]
    fn credential_patch_only_touches_named_fields() {
        let patch = credentials_patch(&json!({"api_key": "k"})).expect("patch");
        assert_eq!(patch.api_key.as_deref(), Some("k"));
        assert!(patch.access_token.is_none());
    }

    /// Regression: the probe used to send the literal model "ping", so every provider
    /// answered 400 model_not_found and the dashboard showed a healthy credential as
    /// broken. The probe must name a model the provider actually serves.
    #[test]
    fn probe_uses_a_real_model_for_the_provider() {
        let catalog = rrouter_router::Catalog::builtin();
        for id in ["claude", "codex", "github", "openai", "deepseek"] {
            let spec = rrouter_auth::get(id).expect("registry provider");
            let model = catalog
                .all()
                .into_iter()
                .find(|m| m.provider == spec.id)
                .map(|m| m.id)
                .expect("catalog has a model for every routed provider");
            assert_ne!(
                model, "ping",
                "{id} must not probe with a placeholder model"
            );
            assert!(!model.is_empty(), "{id} produced an empty probe model");
            assert!(
                catalog.is_known(&model),
                "{id} probe model {model:?} must be routable"
            );
        }
    }
}
