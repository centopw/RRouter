//! Shared application state handed to every handler.

use std::sync::Arc;
use std::time::Instant;

use axum::extract::{FromRequestParts, State};
use rrouter_core::Dialect;
use rrouter_router::Catalog;
use rrouter_store::Store;

use crate::config::Config;

pub struct AppState {
    pub store: Arc<Store>,
    pub catalog: Arc<Catalog>,
    pub refresher: Arc<rrouter_auth::Refresher>,
    pub planner: rrouter_router::Planner,
    pub dispatcher: Arc<rrouter_router::Dispatcher>,
    pub http: reqwest::Client,
    pub config: Config,
    pub started: Instant,
}

impl AppState {
    pub fn request_timeout(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.config.request_timeout_secs)
    }

    pub fn uptime_secs(&self) -> u64 {
        self.started.elapsed().as_secs()
    }
}

pub type Ctx = State<Arc<AppState>>;

/// Dialect implied by an inbound URI path; unknown paths fall back to [OI] chat.
pub fn dialect_of(uri: &axum::http::Uri) -> Dialect {
    Dialect::from_path(uri.path()).unwrap_or(Dialect::OpenAiChat)
}

/// Rejects a request when `require_api_key` is on and the presented credential is
/// absent or not a stored key. Runs before the body is read.
pub struct ApiKeyGuard;

impl FromRequestParts<Arc<AppState>> for ApiKeyGuard {
    type Rejection = axum::response::Response;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        use crate::error::error_response;
        if !state.config.require_api_key {
            return Ok(Self);
        }
        let dialect = dialect_of(&parts.uri);
        let reject = |err: rrouter_core::Error| -> axum::response::Response {
            let mut resp = error_response(dialect, &err);
            *resp.status_mut() = axum::http::StatusCode::UNAUTHORIZED;
            resp
        };
        let Some(key) = crate::auth_header(&parts.headers) else {
            return Err(reject(rrouter_core::Error::Unauthorized("missing API key".into())));
        };
        match state.store.verify_api_key(&key).await {
            Ok(Some(_)) => Ok(Self),
            Ok(None) => Err(reject(rrouter_core::Error::Unauthorized("invalid API key".into()))),
            Err(e) => Err(reject(e)),
        }
    }
}
