//! `POST|GET /v1/files` — the provider's own file API, forwarded.
//!
//! OpenAI-compatible file endpoints are transparent: an upload is a multipart body whose
//! boundary the client signed, so the gateway relays the bytes and the `content-type`
//! unchanged (parsing and re-encoding would produce a different boundary and a body the
//! provider rejects). A list is relayed with the client's query verbatim, a retrieve with
//! the provider's own id, and a download streams the provider's bytes with its content type.
//!
//! These calls are control-plane operations: they are authenticated, admitted (RPM, budgets,
//! allowlist) and concurrency-limited like every `/v1/*` route, but no usage row or token
//! count is recorded — a file upload does not spend model tokens, and inventing a row would
//! distort per-key spend and the activity log.
//!
//! A provider that declares no `files_url` answers `501`; nothing is stored or faked
//! locally.

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

/// Upload cap. The provider allows more, but the gateway buffers the body to relay it, so
/// the limit is memory protection and is named in the message the client gets.
const MAX_UPLOAD_BYTES: usize = 64 * 1024 * 1024;

/// `POST /v1/files` — relay a multipart upload byte for byte.
pub async fn upload(
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
    let (parts, body) = req.into_parts();
    let Some(content_type) = parts
        .headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
    else {
        return respond(
            error::error_response(
                inbound,
                &Error::BadRequest("content-type must be multipart/form-data".into()),
            ),
            request_id,
        );
    };
    if !content_type
        .to_ascii_lowercase()
        .starts_with("multipart/form-data")
    {
        return respond(
            error::error_response(
                inbound,
                &Error::BadRequest(format!(
                    "content-type must be multipart/form-data, got {content_type}"
                )),
            ),
            request_id,
        );
    }
    let bytes = match axum::body::to_bytes(body, MAX_UPLOAD_BYTES).await {
        Ok(bytes) => bytes,
        Err(e) => {
            return respond(
                error::error_response(
                    inbound,
                    &Error::BadRequest(format!(
                        "file upload could not be read (the gateway caps uploads at 64 MiB): {e}"
                    )),
                ),
                request_id,
            )
        }
    };
    let query = parts.uri.query().map(str::to_owned);
    match eligible(&state, &headers, query.as_deref()).await {
        Err(err) => respond(error::error_response(inbound, &err), request_id),
        Ok(None) => respond(
            error::not_implemented(inbound, Capability::Files.unsupported()),
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
                Some(&content_type),
                Some(bytes),
                &headers,
            )
            .await;
            respond_or_fail(result, inbound, request_id)
        }
    }
}

/// `GET /v1/files` — the account's files, with the client's own query.
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
            error::not_implemented(inbound, Capability::Files.unsupported()),
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

/// `GET /v1/files/{id}` — one file's metadata.
pub async fn get(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    req: Request,
) -> Response {
    one(&state, &headers, &id, req, false).await
}

/// `GET /v1/files/{id}/content` — the file's bytes, streamed.
pub async fn content(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    req: Request,
) -> Response {
    one(&state, &headers, &id, req, true).await
}

async fn one(
    state: &Arc<AppState>,
    headers: &HeaderMap,
    id: &str,
    req: Request,
    content: bool,
) -> Response {
    let inbound = dialect_of(req.uri());
    let request_id = Uuid::new_v4();
    let _permit = match state.try_slot() {
        Slot::Granted(permit) => permit,
        Slot::Full => return respond(crate::overloaded_response(inbound), request_id),
    };
    if let Err(err) = native::resource_id("file", id) {
        return respond(error::error_response(inbound, &err), request_id);
    }
    let query = req.uri().query().map(str::to_owned);
    match eligible(state, headers, query.as_deref()).await {
        Err(err) => respond(error::error_response(inbound, &err), request_id),
        Ok(None) => respond(
            error::not_implemented(inbound, Capability::Files.unsupported()),
            request_id,
        ),
        Ok(Some(target)) => {
            let suffix = if content {
                format!("/{id}/content")
            } else {
                format!("/{id}")
            };
            let query = native::client_query(query.as_deref());
            let result = if content {
                native::relay_stream(state, &target, &suffix, query.as_deref(), headers).await
            } else {
                native::relay(
                    state,
                    &target,
                    Method::GET,
                    &suffix,
                    query.as_deref(),
                    None,
                    None,
                    headers,
                )
                .await
            };
            respond_or_fail(result, inbound, request_id)
        }
    }
}

/// Authenticate, admit, and resolve the account that serves files.
///
/// Model-less endpoint: an allowlisted key is told to name a model rather than bypassing its
/// list, exactly as voices and transcriptions do. `None` means no visible account serves the
/// capability, which is a `501` rather than a fallback to some other provider.
async fn eligible(
    state: &AppState,
    headers: &HeaderMap,
    query: Option<&str>,
) -> Result<Option<Target>, Error> {
    let caller = state.authenticate(headers).await?;
    state.admit(&caller, "").await?;
    let pinned = native::query_provider(query);
    native::resolve_target(state, &caller, pinned.as_deref(), Capability::Files).await
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
