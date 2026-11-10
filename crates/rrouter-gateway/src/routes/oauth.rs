//! OAuth sign-in endpoints.
//!
//! Three calls, matching the three moments of a browser login:
//!
//! - `POST /api/oauth/{provider}/start` — begin; returns an id plus what the human
//!   must do (an authorization URL, or a user code and verification URL).
//! - `GET  /api/oauth/{provider}/status/{id}` — poll while the human finishes.
//! - `POST /api/oauth/{provider}/complete` — exchange and store; returns the new
//!   connection id only (never token material).
//!
//! `DELETE /api/oauth/{provider}/{id}` abandons a login and releases its port.
//!
//! # Request/response shapes (manual/remote sign-in)
//!
//! ```text
//! POST /api/oauth/{provider}/start        body: {} | {"manual":true}
//! 200 {"id":"<uuid>","provider":"claude","flow":"pkce",
//!      "challenge":{"kind":"pkce","authorize_url":"<url>",
//!                   "redirect_uri":"http://localhost:51121/oauth-callback",
//!                   "manual":true}}
//! 400 {"error":{"message":"manual callback requires a fixed redirect port; ..."}}
//!
//! POST /api/oauth/{provider}/complete     body: {"id":"<uuid>",
//!                                                "callback_url":"<final browser URL>"}
//! 200 {"id":"<connection-uuid>","provider":"claude","name":"...","warning":null}
//! 400/401 {"error":{"message":"OAuth state mismatch: ..."}}
//! ```
//!
//! Omitting `manual` (or sending no body) keeps the loopback behavior; omitting
//! `callback_url` keeps the existing loopback wait.
//!
//! These never return credentials: a sign-in produces a stored connection, and the
//! existing `/api/connections` view already reports only booleans about it.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::response::Response;
use serde_json::json;

use super::{admin_error, body_json, ok_json};
use crate::state::AppState;
use rrouter_core::Error;

/// `GET /api/oauth/providers`
pub async fn providers() -> Response {
    let data: Vec<_> = rrouter_auth::PROVIDERS
        .iter()
        .filter_map(|spec| {
            spec.login_flow().map(|flow| {
                // Manual (paste-the-callback) mode needs a registered fixed port; a
                // dynamic loopback port cannot be reached from the browser host.
                // Every PKCE flow has a manual mode: a fixed registered port is used
                // when present, otherwise the stable manual loopback port is used.
                let manual = matches!(flow, rrouter_auth::LoginFlow::Pkce(_));
                json!({
                    "id": spec.id,
                    "display": spec.display,
                    "flow": flow.kind(),
                    "manual": manual,
                })
            })
        })
        .collect();
    ok_json(json!({ "data": data }))
}

/// `POST /api/oauth/{provider}/start` with optional `{manual:true}`.
pub async fn start(
    State(state): State<Arc<AppState>>,
    Path(provider): Path<String>,
    body: Body,
) -> Response {
    let Some(spec) = rrouter_auth::get(&provider) else {
        return admin_error(&Error::NotFound(format!("unknown provider {provider}")));
    };
    let value = match body_json(body).await {
        Ok(value) => value,
        Err(response) => return *response,
    };
    let manual = value
        .get("manual")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let started = if manual {
        state.oauth.start_manual(&state.http, spec).await
    } else {
        state.oauth.start(&state.http, spec).await
    };
    match started {
        Ok((id, challenge)) => ok_json(json!({
            "id": id,
            "provider": spec.id,
            "flow": challenge_kind(&challenge),
            "challenge": challenge,
        })),
        Err(e) => admin_error(&e),
    }
}

/// `GET /api/oauth/{provider}/status/{id}`
pub async fn status(
    State(state): State<Arc<AppState>>,
    Path((provider, id)): Path<(String, String)>,
) -> Response {
    if rrouter_auth::get(&provider).is_none() {
        return admin_error(&Error::NotFound(format!("unknown provider {provider}")));
    }
    if let Err(e) = ensure_pending_provider(&state, &provider, &id) {
        return admin_error(&e);
    }
    ok_json(json!({
        "id": id,
        "provider": provider,
        "pending": state.oauth.is_pending(&id),
    }))
}

/// `POST /api/oauth/{provider}/complete` with `{id, callback_url?}`.
///
/// `callback_url` is the full URL the browser landed on after consent. It is required
/// for a manual start (no loopback listener) and also accepted for a loopback start
/// whose redirect could not reach this host.
pub async fn complete(
    State(state): State<Arc<AppState>>,
    Path(provider): Path<String>,
    body: Body,
) -> Response {
    let Some(spec) = rrouter_auth::get(&provider) else {
        return admin_error(&Error::NotFound(format!("unknown provider {provider}")));
    };
    let value = match body_json(body).await {
        Ok(v) => v,
        Err(resp) => return *resp,
    };
    let Some(id) = value.get("id").and_then(|v| v.as_str()) else {
        return admin_error(&Error::BadRequest("id is required".into()));
    };
    if let Err(e) = ensure_pending_provider(&state, &provider, id) {
        return admin_error(&e);
    }
    let callback_url = value
        .get("callback_url")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|url| !url.is_empty());
    let signed_in = match callback_url {
        Some(url) => state.oauth.complete_manual(&state.http, id, url).await,
        None => state.oauth.complete(&state.http, id).await,
    };
    let signed_in = match signed_in {
        Ok(done) => done,
        Err(e) => return admin_error(&e),
    };
    if signed_in.provider != spec.id {
        return admin_error(&Error::BadRequest(format!(
            "pending sign-in belongs to provider {}, not {}",
            signed_in.provider, spec.id
        )));
    }
    // A GitHub sign-in yields a long-lived token that still needs the short-lived
    // Copilot session token before it can serve requests; derive it now so the
    // connection is usable the moment the dashboard shows it.
    let mut creds = signed_in.credentials;
    let mut warning = None;
    if spec.id == "github" && creds.copilot_token.is_none() {
        if let Err(e) = derive_copilot(&state, &mut creds).await {
            warning = Some(format!(
                "signed in, but the Copilot token could not be derived: {e}"
            ));
        }
    }
    let name = signed_in
        .account_label
        .clone()
        .unwrap_or_else(|| format!("{} (OAuth)", spec.display));
    match state
        .store
        .create_connection(spec.id, "oauth", creds, Some(&name))
        .await
    {
        Ok(conn) => {
            // The dashboard invalidates its model query as soon as this response lands,
            // so wait (bounded) for the provider's catalogue instead of racing it.
            super::models::wait_initial_fetch(Arc::clone(&state), conn.id).await;
            ok_json(json!({
                "id": conn.id.to_string(),
                "provider": conn.provider,
                "name": conn.name,
                "warning": warning,
            }))
        }
        Err(e) => admin_error(&e),
    }
}

/// `DELETE /api/oauth/{provider}/{id}`
pub async fn cancel(
    State(state): State<Arc<AppState>>,
    Path((provider, id)): Path<(String, String)>,
) -> Response {
    if rrouter_auth::get(&provider).is_none() {
        return admin_error(&Error::NotFound(format!("unknown provider {provider}")));
    }
    if let Err(e) = ensure_pending_provider(&state, &provider, &id) {
        return admin_error(&e);
    }
    ok_json(json!({ "id": id, "cancelled": state.oauth.cancel(&id) }))
}

/// The pending id must belong to the provider named in the route. Without this check a
/// caller can start provider A, then post the id to provider B and persist A's
/// credentials as a B connection.
fn ensure_pending_provider(
    state: &AppState,
    provider: &str,
    id: &str,
) -> std::result::Result<(), Error> {
    match state.oauth.pending_provider(id) {
        Some(actual) if actual != provider => Err(Error::BadRequest(format!(
            "pending sign-in belongs to provider {actual}, not {provider}"
        ))),
        // Unknown ids stay idempotent for status/cancel; complete reports NotFound itself.
        _ => Ok(()),
    }
}

fn challenge_kind(c: &rrouter_auth::Challenge) -> &'static str {
    match c {
        rrouter_auth::Challenge::Pkce { .. } => "pkce",
        rrouter_auth::Challenge::DeviceCode { .. } => "device_code",
    }
}

/// Derive the GitHub Copilot session token from a freshly obtained GitHub token by
/// reusing the refresher's derivation path through a throwaway credential store.
async fn derive_copilot(
    state: &Arc<AppState>,
    creds: &mut rrouter_auth::Credentials,
) -> Result<(), Error> {
    use rrouter_auth::{CredentialStore, Refresher};
    use std::pin::Pin;

    struct OneShot(Option<rrouter_auth::Credentials>);
    impl CredentialStore for OneShot {
        fn load<'a>(
            &'a self,
            _id: uuid::Uuid,
        ) -> Pin<
            Box<
                dyn std::future::Future<Output = rrouter_core::Result<rrouter_auth::Credentials>>
                    + Send
                    + 'a,
            >,
        > {
            Box::pin(async move {
                self.0
                    .clone()
                    .ok_or_else(|| Error::NotFound("one-shot credential already consumed".into()))
            })
        }
        fn merge<'a>(
            &'a self,
            _id: uuid::Uuid,
            patch: rrouter_auth::Credentials,
        ) -> Pin<
            Box<
                dyn std::future::Future<Output = rrouter_core::Result<rrouter_auth::Credentials>>
                    + Send
                    + 'a,
            >,
        > {
            Box::pin(async move { Ok(patch) })
        }
    }

    let spec = rrouter_auth::get("github").expect("github is a registry provider");
    let refresher = Refresher::new(state.http.clone());
    let store = OneShot(Some(creds.clone()));
    let derived = refresher.fresh(&store, spec, uuid::Uuid::new_v4()).await?;
    if let Some(token) = derived.copilot_token {
        creds.copilot_token = Some(token);
        creds.copilot_expires_at = derived.copilot_expires_at;
    }
    Ok(())
}
