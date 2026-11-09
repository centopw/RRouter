//! `POST|GET /v1/batches` — the provider's own batch API, forwarded.
//!
//! Batches are forwarded, never executed here. A provider runs the job on its own schedule
//! and reports progress through its own status values; the gateway has no scheduler, no
//! local queue and no way to retry a batch, so it relays the request body and the answer
//! unchanged. The body is sent as the exact bytes the client wrote: a batch description is
//! JSON the provider owns, and re-serializing it would be a rewrite the review never saw.
//!
//! `input_file_id` refers to a file the same account uploaded through `/v1/files`, so a
//! client must pin one provider when it mixes the two surfaces (`?provider=`). Nothing is
//! metered: a batch job is control-plane until the provider executes it.
//!
//! A provider that declares no `batches_url` answers `501`; nothing is queued locally.

use std::sync::Arc;

use axum::extract::{Path, Request, State};
use axum::http::header::CONTENT_TYPE;
use axum::http::{HeaderMap, HeaderValue, Method};
use axum::response::Response;
use rrouter_core::{Dialect, Error};
use uuid::Uuid;

use super::native::{self, Capability, Target};
use crate::state::{dialect_of, Slot};
use crate::{error, AppState};

/// Batch bodies are metadata (a file id, an endpoint, a completion window), not data, so
/// they are capped far below an upload.
const MAX_BATCH_BYTES: usize = 8 * 1024 * 1024;

/// `POST /v1/batches` — relay a batch creation body byte for byte.
pub async fn create(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    req: Request,
) -> Response {
    let inbound = dialect_of(req.uri());
    let request_id = Uuid::new_v4();
    let _permit = match state.try_slot() {
        Slot::Granted(permit) => permit,
        Slot::Full => return respond(crate::overloaded_response(inbound), request_id),
    };
    let content_type = req
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !content_type.starts_with("application/json") {
        return respond(
            error::error_response(
                inbound,
                &Error::BadRequest("content-type must be application/json".into()),
            ),
            request_id,
        );
    }
    let query = req.uri().query().map(str::to_owned);
    let (_, body) = req.into_parts();
    let bytes = match axum::body::to_bytes(body, MAX_BATCH_BYTES).await {
        Ok(bytes) => bytes,
        Err(e) => {
            return respond(
                error::error_response(
                    inbound,
                    &Error::BadRequest(format!(
                        "batch body could not be read (the gateway caps it at 8 MiB): {e}"
                    )),
                ),
                request_id,
            )
        }
    };
    match eligible(&state, &headers, query.as_deref()).await {
        Err(err) => respond(error::error_response(inbound, &err), request_id),
        Ok(None) => respond(
            error::not_implemented(inbound, Capability::Batches.unsupported()),
            request_id,
        ),
        Ok(Some(target)) => {
            let query = native::client_query(query.as_deref());
            let result = native::relay(
                &state,
                &target,
                Method::POST,
                "",
                query.as_deref(),
                Some("application/json"),
                Some(bytes),
                &headers,
            )
            .await;
            respond_or_fail(result, inbound, request_id)
        }
    }
}

/// `GET /v1/batches` — the account's jobs, with the client's own query.
pub async fn list(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    req: Request,
) -> Response {
    let inbound = dialect_of(req.uri());
    let request_id = Uuid::new_v4();
    let _permit = match state.try_slot() {
        Slot::Granted(permit) => permit,
        Slot::Full => return respond(crate::overloaded_response(inbound), request_id),
    };
    let query = req.uri().query().map(str::to_owned);
    match eligible(&state, &headers, query.as_deref()).await {
        Err(err) => respond(error::error_response(inbound, &err), request_id),
        Ok(None) => respond(
            error::not_implemented(inbound, Capability::Batches.unsupported()),
            request_id,
        ),
        Ok(Some(target)) => {
            let query = native::client_query(query.as_deref());
            let result = native::relay(
                &state,
                &target,
                Method::GET,
                "",
                query.as_deref(),
                None,
                None,
                &headers,
            )
            .await;
            respond_or_fail(result, inbound, request_id)
        }
    }
}

/// `GET /v1/batches/{id}` — one job's status.
pub async fn get(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    post_or_get(&state, &headers, &id, Method::GET, None).await
}

/// `POST /v1/batches/{id}/cancel` — ask the provider to cancel a job.
pub async fn cancel(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    post_or_get(&state, &headers, &id, Method::POST, Some("/cancel")).await
}

async fn post_or_get(
    state: &Arc<AppState>,
    headers: &HeaderMap,
    id: &str,
    method: Method,
    suffix: Option<&str>,
) -> Response {
    let inbound = Dialect::OpenAiChat;
    let request_id = Uuid::new_v4();
    let _permit = match state.try_slot() {
        Slot::Granted(permit) => permit,
        Slot::Full => return respond(crate::overloaded_response(inbound), request_id),
    };
    if let Err(err) = native::resource_id("batch", id) {
        return respond(error::error_response(inbound, &err), request_id);
    }
    match eligible(state, headers, None).await {
        Err(err) => respond(error::error_response(inbound, &err), request_id),
        Ok(None) => respond(
            error::not_implemented(inbound, Capability::Batches.unsupported()),
            request_id,
        ),
        Ok(Some(target)) => {
            let suffix = format!("/{id}{}", suffix.unwrap_or_default());
            let result =
                native::relay(state, &target, method, &suffix, None, None, None, headers).await;
            respond_or_fail(result, inbound, request_id)
        }
    }
}

/// Authenticate, admit, and resolve the account that serves batches.
async fn eligible(
    state: &AppState,
    headers: &HeaderMap,
    query: Option<&str>,
) -> Result<Option<Target>, Error> {
    let caller = state.authenticate(headers).await?;
    state.admit(&caller, "").await?;
    let pinned = native::query_provider(query);
    native::resolve_target(state, &caller, pinned.as_deref(), Capability::Batches).await
}

fn respond_or_fail(
    result: Result<Response, Error>,
    inbound: Dialect,
    request_id: Uuid,
) -> Response {
    match result {
        Ok(response) => respond(response, request_id),
        Err(err) => respond(error::error_response(inbound, &err), request_id),
    }
}

fn respond(mut response: Response, request_id: Uuid) -> Response {
    if let Ok(value) = HeaderValue::from_str(&request_id.to_string()) {
        response
            .headers_mut()
            .entry("x-request-id")
            .or_insert(value);
    }
    response
}
