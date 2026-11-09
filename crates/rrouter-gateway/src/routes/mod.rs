//! Admin + public JSON API. Nothing here touches the `/v1` hot path.

pub mod audit;
pub mod auth;
pub mod batches;
pub mod combos;
pub mod connections;
pub mod evals;
pub mod files;
pub mod health;
pub mod keys;
pub mod models;
pub(crate) mod native;
pub mod oauth;
pub mod quotas;
pub mod rerank;
pub mod settings;
pub mod system;
pub mod system_logs;
pub mod teams;
pub mod templates;
pub mod updates;
pub mod usage;
pub mod users;
pub mod webhooks;

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use rrouter_core::Error;
use serde_json::{json, Value};

/// Admin errors are always plain `{"error":{...}}`; an admin client is not a model
/// client, so it never gets a dialect envelope.
pub fn admin_error(err: &Error) -> Response {
    let status = StatusCode::from_u16(err.status()).unwrap_or(StatusCode::BAD_GATEWAY);
    (
        status,
        axum::Json(json!({
            "error": crate::error::error_object_with(err.status(), &err.to_string(), err.code()),
        })),
    )
        .into_response()
}

pub fn ok_json<T: serde::Serialize>(value: T) -> Response {
    (StatusCode::OK, axum::Json(value)).into_response()
}

/// Decode a request body once; admin bodies are small. Errors are boxed because a
/// `Response` is large and this is not a hot path.
pub async fn body_json(body: axum::body::Body) -> Result<Value, Box<Response>> {
    let bytes = axum::body::to_bytes(body, 1024 * 1024)
        .await
        .map_err(|e| Box::new(admin_error(&Error::BadRequest(format!("body: {e}")))))?;
    if bytes.is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_slice(&bytes)
        .map_err(|e| Box::new(admin_error(&Error::BadRequest(format!("json: {e}")))))
}
