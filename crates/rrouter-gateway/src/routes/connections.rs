//! `/api/connections` — provider credential CRUD and the 1-token probe.
//!
//! A connection is owned by a team or shared with the whole instance. Reads are scoped by
//! the caller ([`super::teams::reader_teams`]): global/admin authority sees every row, a
//! team account sees only its own team's rows, and a shared row (`team_id IS NULL`) is
//! visible to global scope alone — a team key must never spend an operator's shared
//! account, so showing it to a team would be a lie about routing. Mutations are
//! admin-only at the gate, and `owner_user_id` is attribution, never an access grant.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
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
    /// Owning team; `null` means shared/instance-wide (visible to global scope only).
    pub team_id: Option<String>,
    /// Dashboard attribution: the account that added this connection. Not an access grant.
    pub owner_user_id: Option<String>,
    pub base_url_override: Option<String>,
    /// Cloud project an Antigravity (Cloud Code Assist) sign-in was provisioned with.
    /// Read from `credentials.extra.project_id`; `None` for every other provider.
    pub project_id: Option<String>,
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
            team_id: c.team_id.map(|team| team.to_string()),
            owner_user_id: c.owner_user_id.map(|user| user.to_string()),
            base_url_override: creds.base_url_override.clone(),
            project_id: rrouter_auth::antigravity::project_id(creds).map(str::to_string),
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

/// Which connections a caller may see: `None` is global/admin authority (every row),
/// `Some(teams)` is that account's own teams. A shared row is global-only by design.
///
/// Shared with `/api/models/providers`, which reports the same scope on a provider so the
/// dashboard and the connection list can never disagree about what a caller can reach.
pub(crate) fn in_scope(conn: &Connection, scope: &Option<Vec<Uuid>>) -> bool {
    match scope {
        None => true,
        Some(teams) => conn.team_id.is_some_and(|team| teams.contains(&team)),
    }
}

/// The non-secret facts an audit reader needs about a connection: which row, which
/// provider, and who can route to it. Credential material is never part of this string.
fn describe(conn: &Connection) -> String {
    let scope = match conn.team_id {
        Some(team) => format!("team {team}"),
        None => "shared".to_string(),
    };
    format!(
        "connection {} · provider {} · name {} · scope {scope} · auth {}",
        conn.id,
        conn.provider,
        conn.name.as_deref().unwrap_or("—"),
        conn.auth_type,
    )
}

/// Body fields a PATCH acted on, for the audit row. Only names are listed — a credential
/// value is never read here — and unknown keys are ignored, so a client cannot smuggle
/// free text into the trail through a field the route does not honour.
fn patched_fields(value: &Value) -> String {
    const FIELDS: &[&str] = &[
        "credentials",
        "email",
        "base_url_override",
        "priority",
        "weight",
        "is_active",
        "last_error",
        "team_id",
        "name",
    ];
    let mut seen: Vec<&str> = FIELDS
        .iter()
        .copied()
        .filter(|field| value.get(*field).is_some())
        .collect();
    seen.sort_unstable();
    if seen.is_empty() {
        "no fields".to_string()
    } else {
        seen.join(", ")
    }
}

/// One audit row for a connection mutation.
///
/// Best effort by construction: [`super::auth::audit`] logs a failed write and returns
/// `()`, so auditing can never fail or roll back the mutation it describes. It also
/// enqueues the webhook fan-out, which is why callers do not touch the outbox directly.
async fn audit_mutation(
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

/// `GET /api/connections[?provider=&team_id=]` — the caller's visible accounts.
///
/// Global authority (admin token, admin key, an admin session, loopback) sees every row.
/// A signed-in member sees only the rows owned by a team they belong to; a shared
/// (`team_id IS NULL`) row is deliberately invisible to them, because it is not a
/// candidate for their keys either.
pub async fn list(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<Vec<(String, String)>>,
) -> Response {
    let scope = match super::teams::reader_teams(&state, &headers).await {
        Ok(scope) => scope,
        Err(response) => return *response,
    };
    let provider = query
        .iter()
        .find(|(k, _)| k == "provider")
        .map(|(_, v)| v.as_str())
        .filter(|value| !value.is_empty());
    // `?team_id=` narrows inside the caller's scope. A team account asking for a team it
    // does not belong to is refused: silently answering with its own rows would make a
    // typo look like a permissions bug.
    let requested = match query.iter().find(|(k, _)| k == "team_id") {
        None => None,
        Some((_, raw)) if raw.trim().is_empty() || raw.trim() == "null" => None,
        Some((_, raw)) => match Uuid::parse_str(raw.trim()) {
            Ok(team) => Some(team),
            Err(_) => {
                return admin_error(&Error::BadRequest("team_id must be a uuid or null".into()))
            }
        },
    };
    if let (Some(teams), Some(team)) = (&scope, requested) {
        if !teams.contains(&team) {
            return admin_error(&Error::Forbidden {
                code: "team_forbidden",
                message: "this account is not a member of that team".into(),
            });
        }
    }
    match state.store.list_connections(provider).await {
        Ok(conns) => {
            let data: Vec<ConnectionView> = conns
                .iter()
                .filter(|conn| in_scope(conn, &scope))
                .filter(|conn| requested.is_none_or(|team| conn.team_id == Some(team)))
                .map(ConnectionView::from)
                .collect();
            ok_json(json!({ "data": data }))
        }
        Err(e) => admin_error(&e),
    }
}

/// `GET /api/connections/{id}` — one visible account, or `404`.
///
/// An invisible row answers exactly like a missing one: telling a team that another
/// team's connection exists is itself a disclosure.
pub async fn get(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let Ok(uuid) = Uuid::parse_str(&id) else {
        return admin_error(&Error::NotFound(format!("connection {id}")));
    };
    let scope = match super::teams::reader_teams(&state, &headers).await {
        Ok(scope) => scope,
        Err(response) => return *response,
    };
    match state.store.get_connection(uuid).await {
        Ok(Some(conn)) if in_scope(&conn, &scope) => ok_json(ConnectionView::from(&conn)),
        Ok(_) => admin_error(&Error::NotFound(format!("connection {uuid}"))),
        Err(e) => admin_error(&e),
    }
}

/// Body: `{provider, auth_type, name?, email?, priority?, team_id?, credentials:{…}}`.
///
/// `team_id` is nullable: absent or `null` creates the shared/instance-wide account. A
/// named team must exist (the store answers `404`), and the row is attributed to the
/// signed-in account that created it when there is one.
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
    // A provider whose manifest host is a placeholder cannot be saved without a real
    // resource URL; the message names what to fix and never echoes the credential.
    if let Err(e) = check_override(
        Some(spec),
        creds.base_url_override.as_deref(),
        value.get("credentials"),
    ) {
        return admin_error(&e);
    }
    let team_id = match team_field(&value) {
        Ok(team) => team,
        Err(e) => return admin_error(&e),
    };
    // Attribution only: the actor who added this account. Admin token, admin key and
    // loopback have no user, so the column stays `NULL` for operator-created rows.
    let owner_user_id = super::auth::current_user(&state, &headers)
        .await
        .map(|user| user.id);
    match state
        .store
        .create_connection_owned(
            &provider,
            &auth_type,
            creds,
            name.as_deref(),
            team_id,
            owner_user_id,
        )
        .await
    {
        Ok(created) => {
            // The row is committed: audit it now, so a later failure in this handler (a
            // rejected routing knob) cannot leave a connection that the trail never saw.
            audit_mutation(
                &state,
                &headers,
                peer,
                "connection_created",
                &describe(&created),
            )
            .await;
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
            // List this provider's own models before responding, bounded: the dashboard's
            // model invalidation then sees the rows, while a slow provider is detached.
            super::models::wait_initial_fetch(Arc::clone(&state), created.id).await;
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
    headers: HeaderMap,
    peer: crate::auth::Peer,
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
        // The merge model cannot express "remove this credential", so validate the value
        // this request would *leave* on the row: the new one when present, the stored one
        // otherwise. That is also what keeps a legacy row from being saved still-broken.
        let existing = match state.store.get_connection(uuid).await {
            Ok(Some(connection)) => connection,
            Ok(None) => return admin_error(&Error::NotFound(format!("connection {uuid}"))),
            Err(e) => return admin_error(&e),
        };
        let effective = patch
            .base_url_override
            .as_deref()
            .or(existing.credentials.base_url_override.as_deref());
        // A provider dropped from the registry has no rule to apply; the empty-value check
        // still runs, so a removed provider's account can be re-keyed but not broken.
        let spec = rrouter_auth::get(&existing.provider);
        if let Err(e) = check_override(spec, effective, Some(creds)) {
            return admin_error(&e);
        }
        if let Err(e) = state.store.merge_credentials(uuid, patch).await {
            return admin_error(&e);
        }
    }
    if let Err(e) = apply_patch(&state, uuid, value.clone()).await {
        return admin_error(&e);
    }
    // Ownership is its own column pair: `team_id: null` hands the account back to the
    // instance, a uuid moves it to that team, and an absent key leaves it alone.
    // `owner_user_id` is attribution, so it follows the row rather than the patcher.
    if value.get("team_id").is_some() {
        let team_id = match team_field(&value) {
            Ok(team) => team,
            Err(e) => return admin_error(&e),
        };
        let owner = match state.store.get_connection(uuid).await {
            Ok(Some(conn)) => conn.owner_user_id,
            Ok(None) => return admin_error(&Error::NotFound(format!("connection {uuid}"))),
            Err(e) => return admin_error(&e),
        };
        if let Err(e) = state.store.set_connection_owner(uuid, team_id, owner).await {
            return admin_error(&e);
        }
    }
    // A credential or endpoint edit can change which catalogue the account serves.
    // Refresh before responding so an immediate UI invalidate sees the new rows; name,
    // team, priority, weight and active-only edits do not pay for a provider call.
    if value.get("credentials").is_some() || value.get("base_url_override").is_some() {
        super::models::wait_initial_fetch(Arc::clone(&state), uuid).await;
    }
    match state.store.get_connection(uuid).await {
        Ok(Some(c)) => {
            audit_mutation(
                &state,
                &headers,
                peer,
                "connection_updated",
                &format!("{} · fields {}", describe(&c), patched_fields(&value)),
            )
            .await;
            ok_json(ConnectionView::from(&c))
        }
        Ok(None) => admin_error(&Error::NotFound(format!("connection {uuid}"))),
        Err(e) => admin_error(&e),
    }
}

pub async fn remove(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    peer: crate::auth::Peer,
    Path(id): Path<String>,
) -> Response {
    let Ok(uuid) = Uuid::parse_str(&id) else {
        return admin_error(&Error::BadRequest("connection id must be a uuid".into()));
    };
    // Read the row before it goes: the audit trail should outlive what it describes.
    let existing = match state.store.get_connection(uuid).await {
        Ok(Some(conn)) => conn,
        Ok(None) => return admin_error(&Error::NotFound(format!("connection {uuid}"))),
        Err(e) => return admin_error(&e),
    };
    match state.store.delete_connection(uuid).await {
        Ok(true) => {
            audit_mutation(
                &state,
                &headers,
                peer,
                "connection_deleted",
                &describe(&existing),
            )
            .await;
            ok_json(json!({ "deleted": true, "id": uuid.to_string() }))
        }
        Ok(false) => admin_error(&Error::NotFound(format!("connection {uuid}"))),
        Err(e) => admin_error(&e),
    }
}

/// `POST /api/connections/:id/test`: refresh credentials, then ask for one token.
///
/// The probe spends the account's own credential, so it follows the same visibility rule
/// as `GET`: a row outside the caller's scope answers `404`, never a probe. The route is
/// admin-gated today, which makes that check redundant for the current gates and
/// load-bearing the moment a team gate widens it.
pub async fn test(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    peer: crate::auth::Peer,
    Path(id): Path<String>,
) -> Response {
    let Ok(uuid) = Uuid::parse_str(&id) else {
        return admin_error(&Error::BadRequest("connection id must be a uuid".into()));
    };
    let Some(conn) = (match state.store.get_connection(uuid).await {
        Ok(c) => c,
        Err(e) => return admin_error(&e),
    }) else {
        return admin_error(&Error::NotFound(format!("connection {uuid}")));
    };
    let scope = match super::teams::reader_teams(&state, &headers).await {
        Ok(scope) => scope,
        Err(response) => return *response,
    };
    if !in_scope(&conn, &scope) {
        return admin_error(&Error::NotFound(format!("connection {uuid}")));
    }
    let Some(spec) = rrouter_auth::get(&conn.provider) else {
        return admin_error(&Error::BadRequest(format!(
            "unknown provider {}",
            conn.provider
        )));
    };
    let started = std::time::Instant::now();
    let described = describe(&conn);
    let result = probe(&state, spec, conn).await;
    let latency_ms = started.elapsed().as_millis().min(u32::MAX as u128) as u32;
    // One row per probe, whether it answered or failed. The outcome is the status the
    // provider gave, never the error body (which can echo the credential).
    let outcome = match &result {
        Ok(status) => format!("probe ok {status}"),
        Err(e) => format!("probe failed {}", e.status()),
    };
    audit_mutation(
        &state,
        &headers,
        peer,
        "connection_tested",
        &format!("{described} · {outcome} · {latency_ms}ms"),
    )
    .await;
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
    // Qoder is upstream-only *and* needs its catalog and a per-attempt signature, so it
    // has its own probe: the catalog fetch alone already exercises the credential.
    if spec.default_dialect == rrouter_core::Dialect::QoderAgent {
        return probe_qoder(state, spec, &conn, &creds, &model).await;
    }
    let mut req = ChatRequest::new(model);
    req.messages.push(Message::text(Role::User, "ping"));
    // Some providers reject a 1-token completion (minimum output length), so ask for
    // a small but legal amount.
    req.max_tokens = Some(16);
    // The probe speaks the provider's own dialect at its own endpoint. Building the
    // body for Chat and posting it to a Responses URL (codex) is a guaranteed 400.
    // CCA is upstream-only, so it has no generic body builder: the project and envelope
    // come from the same helpers the proxy uses.
    let is_cca = spec.default_dialect == rrouter_core::Dialect::GoogleCodeAssist;
    let project = if is_cca {
        Some(
            creds
                .extra
                .get("project_id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|project| !project.is_empty())
                .ok_or_else(|| {
                    Error::BadRequest(
                        "google-antigravity connection is missing credentials.extra.project_id"
                            .into(),
                    )
                })?
                .to_string(),
        )
    } else {
        None
    };
    let (url, dialect) = match &project {
        Some(_) => {
            let base = match creds.base_url_override.as_deref() {
                Some(base) => base.trim_end_matches('/').to_string(),
                None => rrouter_translate::google_cca::daily_host(spec.base_url),
            };
            (
                rrouter_translate::google_cca::endpoint(&base, spec.stream_only),
                rrouter_core::Dialect::GoogleCodeAssist,
            )
        }
        None => crate::proxy::endpoint_for(
            spec,
            creds.base_url_override.as_deref(),
            spec.default_dialect,
        ),
    };
    let body = match &project {
        Some(project) => rrouter_translate::google_cca::build_request(&req, &req.model, project)?,
        None => rrouter_translate::build_request(dialect, &req, spec.stream_only)?,
    };

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
    if is_cca {
        headers.insert(
            axum::http::header::USER_AGENT,
            axum::http::HeaderValue::from_static(rrouter_translate::google_cca::USER_AGENT),
        );
        headers.insert(
            "x-goog-api-client",
            axum::http::HeaderValue::from_static(rrouter_translate::google_cca::CLIENT_HEADER),
        );
    }
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

/// Probe a Qoder connection: fetch the account's catalog (a signed `GET`), then send one
/// 1-token completion through the real envelope and require a frame back.
///
/// The model is the catalog's own: a probe must not send a key the account cannot serve,
/// and `model_config` has to be the account's block, exactly as chat does it.
async fn probe_qoder(
    state: &Arc<AppState>,
    spec: &'static rrouter_auth::ProviderSpec,
    conn: &Connection,
    creds: &Credentials,
    requested: &str,
) -> Result<u16, Error> {
    let signer = rrouter_auth::qoder::signer(creds)?;
    let base = creds
        .base_url_override
        .as_deref()
        .map(|base| base.trim().trim_end_matches('/').to_string())
        .unwrap_or_else(|| spec.base_url.to_string());
    let catalog = rrouter_auth::qoder::catalog(&state.http, &signer, &base).await?;
    let model = catalog
        .config(requested)
        .map(|_| requested.to_string())
        .or_else(|| catalog.default_model().map(str::to_string))
        .ok_or_else(|| {
            Error::BadRequest("qoder account catalog is empty; nothing to probe".into())
        })?;
    let Some(model_config) = catalog.config(&model).cloned() else {
        return Err(Error::BadRequest(format!(
            "qoder account does not list model \"{model}\""
        )));
    };

    let mut request = ChatRequest::new(&model);
    request.messages.push(Message::text(Role::User, "ping"));
    request.max_tokens = Some(16);
    let session_id = rrouter_translate::qoder::session_id(&signer.user_id, &model);
    let body = rrouter_translate::qoder::build_request(
        &request,
        &rrouter_translate::qoder::Envelope {
            model: &model,
            model_config: &model_config,
            session_id: &session_id,
        },
    )?;
    let url = rrouter_translate::qoder::chat_url(&base);
    let wire = rrouter_auth::qoder::encoding::encode_body(&serde_json::to_vec(&body)?);

    let mut headers = axum::http::HeaderMap::new();
    headers.insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/json"),
    );
    headers.insert(
        axum::http::header::ACCEPT,
        axum::http::HeaderValue::from_static("text/event-stream"),
    );
    headers.insert(
        axum::http::header::ACCEPT_ENCODING,
        axum::http::HeaderValue::from_static("identity"),
    );
    for (name, value) in rrouter_auth::qoder::cosy::sign_headers(&wire, &url, &signer)? {
        if let (Ok(name), Ok(value)) = (
            axum::http::header::HeaderName::from_bytes(name.as_bytes()),
            axum::http::HeaderValue::from_str(&value),
        ) {
            headers.insert(name, value);
        }
    }

    let response = state
        .http
        .post(&url)
        .headers(headers)
        .body(wire)
        .timeout(state.request_timeout())
        .send()
        .await
        .map_err(|e| {
            if e.is_timeout() {
                Error::Timeout(format!("qoder probe: {e}"))
            } else {
                Error::Transport(format!("qoder probe: {e}"))
            }
        })?;
    let status = response.status().as_u16();
    if !response.status().is_success() {
        let text = response.text().await.unwrap_or_default();
        return Err(Error::Upstream {
            provider: conn.provider.clone(),
            status,
            body: text.chars().take(512).collect(),
            payload: None,
        });
    }
    // The HTTP status is 200 even for a queued model or a quota block; the peek maps those
    // to a real error, so a broken account cannot report healthy.
    use futures_util::StreamExt as _;
    let mut stream = crate::proxy::qoder_stream(response).await?;
    match stream.next().await {
        Some(Ok(chunk)) if !chunk.is_empty() => Ok(status),
        Some(Ok(_)) => Err(Error::Upstream {
            provider: conn.provider.clone(),
            status,
            body: "stream closed before the first frame".into(),
            payload: None,
        }),
        Some(Err(e)) => Err(e),
        None => Err(Error::Upstream {
            provider: conn.provider.clone(),
            status,
            body: "empty stream".into(),
            payload: None,
        }),
    }
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
    // The operator's label lives in its own column. `null` or a blank string clears it;
    // the store trims and bounds it, so the audit row can name the field regardless.
    if let Some(raw) = value.get("name") {
        let name = match raw {
            Value::Null => None,
            Value::String(text) => Some(text.as_str()),
            _ => return Err(Error::BadRequest("name must be a string or null".into())),
        };
        state.store.set_connection_name(id, name).await?;
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
/// Validate the base URL a connection would save with.
///
/// Two rules, both `400`: the provider's own rule ([`rrouter_auth::validate_base_url_override`]
/// refuses a placeholder host, including one an operator pasted from the manifest), and the
/// gateway's refusal of an explicitly empty value — an empty base URL would send every
/// request to the origin root, and the credential-merge model has no way to express "remove
/// this field", so the honest answer is a message rather than a silently unusable row.
fn check_override(
    spec: Option<&rrouter_auth::ProviderSpec>,
    effective: Option<&str>,
    credentials: Option<&Value>,
) -> Result<(), Error> {
    if let Some(raw) = credentials.and_then(|creds| creds.get("base_url_override")) {
        if raw.as_str().is_some_and(|value| value.trim().is_empty()) {
            return Err(Error::BadRequest(
                "base_url_override cannot be empty: an empty base URL would send traffic to \
                 the origin root. Send a full URL, or omit the field to keep the saved one."
                    .into(),
            ));
        }
    }
    match spec {
        Some(spec) => rrouter_auth::validate_base_url_override(spec, effective),
        None => Ok(()),
    }
}

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

/// `team_id` from a body: a uuid, or absent/`null`/empty for the shared account.
fn team_field(value: &Value) -> Result<Option<Uuid>, Error> {
    match value.get("team_id") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(raw)) if raw.trim().is_empty() => Ok(None),
        Some(Value::String(raw)) => Uuid::parse_str(raw.trim())
            .map(Some)
            .map_err(|_| Error::BadRequest("team_id must be a uuid or null".into())),
        Some(_) => Err(Error::BadRequest("team_id must be a uuid or null".into())),
    }
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
            team_id: None,
            owner_user_id: None,
            created_at: 0,
            updated_at: 0,
        };
        let encoded = serde_json::to_string(&ConnectionView::from(&conn)).expect("serialize");
        assert!(!encoded.contains("SECRET"));
        assert!(encoded.contains("\"has_access_token\":true"));
    }

    #[test]
    fn views_project_the_cloud_project_without_token_material() {
        let mut creds = Credentials {
            access_token: Some("SECRET-A".into()),
            refresh_token: Some("SECRET-R".into()),
            ..Credentials::default()
        };
        creds.extra.insert(
            "project_id".into(),
            serde_json::Value::String("  proj-antigravity  ".into()),
        );
        let mut conn = Connection {
            id: Uuid::nil(),
            provider: "google-antigravity".into(),
            auth_type: "oauth".into(),
            name: Some("Antigravity account".into()),
            email: Some("user@example.test".into()),
            priority: 0,
            weight: 100,
            is_active: true,
            credentials: creds,
            backoff_level: 0,
            last_error: None,
            last_error_code: None,
            last_error_at: None,
            team_id: None,
            owner_user_id: None,
            created_at: 0,
            updated_at: 0,
        };
        let view = ConnectionView::from(&conn);
        assert_eq!(view.project_id.as_deref(), Some("proj-antigravity"));
        let encoded = serde_json::to_string(&view).expect("serialize");
        assert!(encoded.contains("\"project_id\":\"proj-antigravity\""));
        assert!(!encoded.contains("SECRET"));

        // Providers without a project serialize the field as null, which the UI treats
        // as "no project line".
        conn.provider = "openai".into();
        conn.credentials.extra.clear();
        let view = ConnectionView::from(&conn);
        assert_eq!(view.project_id, None);
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
