//! `/api/free-mode` — zero-config free model discovery, toggles, live probes, plus the
//! Advanced JSON document import and teardown.
//!
//! Admin-only, like every other write on this router. Every response is secrets-free: proxy
//! URLs are masked, credentials appear as `has_*` booleans, probe errors are scrubbed, and
//! any stored document is redacted before it is echoed.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use rrouter_core::Error;
use serde_json::{json, Value};

use super::{admin_error, body_json, ok_json};
use crate::free_mode::{
    self, PatchModelRequest, PatchSettingsRequest, ProbeRequest, ValidationError,
};
use crate::state::AppState;

/// `POST /api/free-mode/import` — validate the document, then apply it. Idempotent.
///
/// The body is the document itself; `{"document": {...}}` is accepted too because the
/// dashboard posts a form object. Validation is total and runs before the first write, so a
/// rejected document changes nothing.
pub async fn import(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    peer: crate::auth::Peer,
    body: Body,
) -> Response {
    let value = match body_json(body).await {
        Ok(value) => value,
        Err(response) => return *response,
    };
    // A wrapper object is accepted without ambiguity: `document` is not a document field.
    let raw = value
        .get("document")
        .filter(|_| value.get("providers").is_none())
        .cloned()
        .unwrap_or(value);
    // The catalog guard is dropped before the first await: `apply` is async and the lock
    // guard is not `Send`.
    let validated = {
        let catalog = state
            .providers
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        free_mode::validate(&raw, &catalog)
    };
    let document = match validated {
        Ok(document) => document,
        Err(error) => return invalid(&error),
    };
    match free_mode::apply(state.store.as_ref(), &document).await {
        Ok(report) => {
            // A new custom provider has to become visible to ref resolution now, not on
            // the next restart.
            if let Err(error) = state.reload_providers().await {
                return admin_error(&error);
            }
            audit(
                &state,
                &headers,
                peer,
                "free_mode_imported",
                &format!(
                    "{} provider(s), {} account(s) created, {} updated, {} disabled, {} model(s) seeded",
                    report.providers, report.created, report.updated, report.disabled, report.models
                ),
            )
            .await;
            ok_json(json!({
                "ok": true,
                "providers": report.providers,
                "accounts": { "created": report.created, "updated": report.updated },
                "disabled": report.disabled,
                "routing": document.routing.value(),
            }))
        }
        Err(error) => admin_error(&error),
    }
}

/// `GET /api/free-mode[?refresh=true]` — zero-config free-mode state, discovered models,
/// probe status, catalogs, plus any imported Advanced document and accounts.
pub async fn get(
    State(state): State<Arc<AppState>>,
    Query(query): Query<Vec<(String, String)>>,
) -> Response {
    let refresh = query
        .iter()
        .any(|(key, value)| key == "refresh" && matches!(value.trim(), "true" | "1" | "yes"));
    match free_mode::state_view(&state, refresh).await {
        Ok(view) => ok_json(view),
        Err(error) => admin_error(&error),
    }
}

/// `PATCH /api/free-mode` — toggle the free pool and/or automatic quota fallback.
pub async fn patch(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    peer: crate::auth::Peer,
    body: Body,
) -> Response {
    let value = match body_json(body).await {
        Ok(value) => value,
        Err(response) => return *response,
    };
    let req: PatchSettingsRequest = match serde_json::from_value(value) {
        Ok(req) => req,
        Err(error) => return admin_error(&Error::BadRequest(format!("invalid body: {error}"))),
    };
    match free_mode::patch_settings(&state, req.clone()).await {
        Ok(view) => {
            audit(
                &state,
                &headers,
                peer,
                "free_mode_updated",
                &format!(
                    "enabled={}, fallback_on_quota={}",
                    view.enabled, view.fallback_on_quota
                ),
            )
            .await;
            ok_json(view)
        }
        Err(error) => admin_error(&error),
    }
}

/// `PATCH /api/free-mode/models` — enable or disable one discovered free model.
pub async fn patch_model(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    peer: crate::auth::Peer,
    body: Body,
) -> Response {
    let value = match body_json(body).await {
        Ok(value) => value,
        Err(response) => return *response,
    };
    let req: PatchModelRequest = match serde_json::from_value(value) {
        Ok(req) => req,
        Err(error) => return admin_error(&Error::BadRequest(format!("invalid body: {error}"))),
    };
    match free_mode::patch_model(&state, req).await {
        Ok(resp) => {
            audit(
                &state,
                &headers,
                peer,
                "free_mode_model_updated",
                &format!("{}/{} enabled={}", resp.provider, resp.model, resp.enabled),
            )
            .await;
            ok_json(resp)
        }
        Err(error) => admin_error(&error),
    }
}

/// `POST /api/free-mode/probe` — test one free model now, or every enabled usable model
/// when `model` is omitted (or `all: true`).
pub async fn probe(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    peer: crate::auth::Peer,
    body: Body,
) -> Response {
    let value = match body_json(body).await {
        Ok(value) => value,
        Err(response) => return *response,
    };
    let req: ProbeRequest = match serde_json::from_value(value) {
        Ok(req) => req,
        Err(error) => return admin_error(&Error::BadRequest(format!("invalid body: {error}"))),
    };
    if !req.all {
        if let (Some(provider), Some(model)) = (req.provider.as_deref(), req.model.as_deref()) {
            return match free_mode::probe_one(&state, provider, model).await {
                Ok(resp) => {
                    audit(
                        &state,
                        &headers,
                        peer,
                        "free_mode_probed",
                        &format!(
                            "{}/{} ok={} status={}",
                            resp.provider, resp.model, resp.probe.ok, resp.probe.status
                        ),
                    )
                    .await;
                    ok_json(resp)
                }
                Err(error) => admin_error(&error),
            };
        }
        if req.provider.is_some() || req.model.is_some() {
            return admin_error(&Error::BadRequest(
                "both provider and model are required to probe one model".into(),
            ));
        }
    }
    match free_mode::probe_all(&state, req.refresh_catalogs).await {
        Ok(resp) => {
            let working = resp.results.iter().filter(|r| r.probe.ok).count();
            audit(
                &state,
                &headers,
                peer,
                "free_mode_probed",
                &format!("batch: {working}/{} ok", resp.results.len()),
            )
            .await;
            ok_json(resp)
        }
        Err(error) => admin_error(&error),
    }
}

/// `POST /api/free-mode/discover` with an optional `{"providers": ["openrouter"]}`.
///
/// Refreshes both imported free-mode accounts and public catalogs. Each account is asked
/// through its own credentials, so one broken account costs its own row in the report and
/// nothing else.
pub async fn discover(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    peer: crate::auth::Peer,
    body: Body,
) -> Response {
    let value = match body_json(body).await {
        Ok(value) => value,
        Err(response) => return *response,
    };
    let only = match value.get("providers") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => {
            let mut only = Vec::with_capacity(items.len());
            for item in items {
                let Some(segment) = item.as_str().map(str::trim).filter(|id| !id.is_empty()) else {
                    return admin_error(&Error::BadRequest(
                        "providers must be an array of provider ids or codes".into(),
                    ));
                };
                let Some(provider) = state.provider_by_ref(segment) else {
                    return admin_error(&Error::BadRequest(format!("unknown provider {segment}")));
                };
                if !only.contains(&provider) {
                    only.push(provider);
                }
            }
            only
        }
        Some(_) => {
            return admin_error(&Error::BadRequest(
                "providers must be an array of provider ids or codes".into(),
            ))
        }
    };
    match free_mode::discover(&state, &only).await {
        Ok(report) => {
            audit(
                &state,
                &headers,
                peer,
                "free_mode_discovered",
                &format!(
                    "{} account(s), {} provider(s), {} catalog(s)",
                    report.accounts.len(),
                    report.providers.len(),
                    report.catalogs.len()
                ),
            )
            .await;
            ok_json(report)
        }
        Err(error) => admin_error(&error),
    }
}

/// `DELETE /api/free-mode[?disable=true]` — forget the document and every discovery row.
///
/// `?disable=true` also flips the free-mode accounts inactive. Credentials are never
/// deleted: a teardown that dropped them could not be undone by a re-import.
pub async fn remove(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    peer: crate::auth::Peer,
    Query(query): Query<Vec<(String, String)>>,
) -> Response {
    let disable = query
        .iter()
        .any(|(key, value)| key == "disable" && matches!(value.trim(), "true" | "1" | "yes"));
    if let Err(error) = state.store.clear_free_mode().await {
        return admin_error(&error);
    }
    let mut disabled = 0usize;
    if disable {
        let connections = match state.store.list_connections(None).await {
            Ok(connections) => connections,
            Err(error) => return admin_error(&error),
        };
        for conn in connections.iter().filter(|conn| conn.credentials.free_mode) {
            if !conn.is_active {
                continue;
            }
            if let Err(error) = state.store.set_connection_active(conn.id, false).await {
                return admin_error(&error);
            }
            disabled += 1;
        }
    }
    audit(
        &state,
        &headers,
        peer,
        "free_mode_cleared",
        &format!("document and models cleared, {disabled} account(s) disabled"),
    )
    .await;
    ok_json(json!({ "ok": true, "disabled": disabled }))
}

/// A `400` in the shape the spec fixes: one message plus every problem found.
fn invalid(error: &ValidationError) -> Response {
    (
        StatusCode::BAD_REQUEST,
        axum::Json(json!({
            "error": {
                "message": error.message,
                "details": error.details,
            }
        })),
    )
        .into_response()
}

/// One audit row per free-mode mutation. Counts only: the document itself is secret
/// material and a trail that recorded it would be a second copy of it.
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
