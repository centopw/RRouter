//! axum HTTP server for RRouter: the whole product surface.
//!
//! Module split keeps the hot path isolated:
//! - [`proxy`] — `/v1/*` request path: translate, dispatch, stream. No admin/UI code.
//! - [`routes`] — health, models, admin JSON API.
//! - [`ui`] — embedded dashboard (static assets, no build step, no CDN).
//! - [`config`] / [`state`] — startup configuration and shared state.
//!
//! Every handler returns either the proxy's translated response or an error body in
//! the inbound dialect's shape (see [`error`]).

pub mod config;
pub mod error;
pub mod models_registry;
pub mod proxy;
pub mod routes;
pub mod settings;
pub mod state;
pub mod ui;

use std::sync::Arc;

use axum::extract::Request;
use axum::http::header::{ACCESS_CONTROL_ALLOW_HEADERS, ACCESS_CONTROL_ALLOW_METHODS, ACCESS_CONTROL_ALLOW_ORIGIN, ACCESS_CONTROL_MAX_AGE, AUTHORIZATION, VARY};
use axum::http::{HeaderMap, HeaderValue};
use axum::middleware::Next;
use axum::response::Response;
use axum::routing::{delete, get, patch, post, put};
use axum::Router;

pub use config::Config;
pub use state::AppState;

/// Process start time, so `/health` can report uptime.
pub fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Client credential from `Authorization: Bearer …` or `x-api-key: …`.
pub fn auth_header(headers: &HeaderMap) -> Option<String> {
    if let Some(value) = headers.get(AUTHORIZATION).and_then(|v| v.to_str().ok()) {
        let trimmed = value.trim();
        let token = trimmed
            .strip_prefix("Bearer ")
            .or_else(|| trimmed.strip_prefix("bearer "))
            .unwrap_or(trimmed);
        if !token.is_empty() {
            return Some(token.trim().to_string());
        }
    }
    headers
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// Build the full application router.
pub fn app(state: Arc<AppState>) -> Router {
    spawn_model_sync(&state);
    let api = Router::new()
        .route("/connections", get(routes::connections::list).post(routes::connections::create))
        .route(
            "/connections/{id}",
            patch(routes::connections::update).delete(routes::connections::remove),
        )
        .route("/connections/{id}/test", post(routes::connections::test))
        .route("/keys", get(routes::keys::list).post(routes::keys::create))
        .route("/keys/{id}", delete(routes::keys::remove))
        .route(
            "/aliases",
            get(routes::models::list_aliases)
                .post(routes::models::create_alias)
                .put(routes::models::update_alias)
                .delete(routes::models::delete_alias),
        )
        .route("/disabled/{model}", put(routes::models::disable))
        .route("/disabled/{model}/enable", put(routes::models::enable))
        .route("/models", get(routes::models::registry_list))
        .route("/models/providers", get(routes::models::providers))
        .route("/models/sync", post(routes::models::sync))
        .route("/models/fetch", post(routes::models::fetch))
        .route(
            "/models/custom",
            post(routes::models::custom).delete(routes::models::delete_custom),
        )
        .route("/models/test", post(routes::models::test))
        .route(
            "/combos",
            get(routes::combos::list).post(routes::combos::create).delete(routes::combos::remove),
        )
        .route("/usage", get(routes::usage::list))
        .route("/usage/summary", get(routes::usage::summary))
        .route("/system", get(routes::system::system))
        .route("/settings", get(routes::settings::get_settings).put(routes::settings::put_settings))
        .route("/oauth/providers", get(routes::oauth::providers))
        .route("/oauth/{provider}/start", post(routes::oauth::start))
        .route(
            "/oauth/{provider}/status/{id}",
            get(routes::oauth::status),
        )
        .route("/oauth/{provider}/complete", post(routes::oauth::complete))
        .route("/oauth/{provider}/{id}", axum::routing::delete(routes::oauth::cancel))
        .route("/ui/panels", get(ui::panels))
        .route_layer(axum::middleware::from_fn_with_state(
            Arc::clone(&state),
            admin_gate,
        ));

    Router::new()
        .route("/health", get(routes::health::health))
        .route("/v1/models", get(routes::models::list_models))
        .route("/v1/chat/completions", post(proxy::proxy))
        .route("/v1/messages", post(proxy::proxy))
        .route("/v1/responses", post(proxy::proxy))
        .route("/v1/responses/compact", post(proxy::unsupported))
        .route("/v1/messages/count_tokens", post(proxy::unsupported))
        .route("/v1/embeddings", post(proxy::embeddings))
        .route("/", get(ui::index))
        .route("/app", get(ui::app_index))
        .route("/app/", get(ui::app_index))
        .route("/app/{*path}", get(ui::app_asset))
        .route("/{*path}", get(ui::asset))
        .nest("/api", api)
        .with_state(Arc::clone(&state))
        .layer(axum::middleware::from_fn_with_state(state, cors))
}

/// Starts the models.dev refresh loop exactly once per process.
///
/// `app()` is the only startup hook this crate owns, so the timer starts here; the
/// background task sleeps a minute first and never blocks serving. Outside a tokio
/// runtime (a plain library build) there is nothing to schedule and it is skipped.
fn spawn_model_sync(state: &Arc<AppState>) {
    static START: std::sync::Once = std::sync::Once::new();
    if tokio::runtime::Handle::try_current().is_err() {
        return;
    }
    START.call_once(|| models_registry::spawn_daily_sync(Arc::clone(state)));
}

/// CORS: echo only the configured origins and loopback tooling.
///
/// No wildcard is ever emitted: `Access-Control-Allow-Origin: *` on the admin API
/// would let any web origin read it. A request without an `Origin` header is not a
/// CORS request at all, so it gets no `Access-Control-*` headers.
async fn cors(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    req: Request,
    next: Next,
) -> Response {
    let origin = req
        .headers()
        .get(axum::http::header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let preflight = req.method() == axum::http::Method::OPTIONS;
    let mut response = if preflight {
        Response::new(axum::body::Body::empty())
    } else {
        next.run(req).await
    };
    let Some(origin) = origin else { return response };
    let Some(allow) = state.config.cors_origin_for(&origin) else {
        return response;
    };
    let headers = response.headers_mut();
    if let Ok(v) = HeaderValue::from_str(&allow) {
        headers.insert(ACCESS_CONTROL_ALLOW_ORIGIN, v);
        headers.append(VARY, HeaderValue::from_static("origin"));
    }
    headers.insert(
        ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("GET, POST, PUT, PATCH, DELETE, OPTIONS"),
    );
    headers.insert(
        ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static("content-type, authorization, x-api-key, anthropic-version, anthropic-beta"),
    );
    headers.insert(
        ACCESS_CONTROL_MAX_AGE,
        HeaderValue::from_static("600"),
    );
    response
}

/// Admin API gate: loopback peers only unless `RROUTER_ALLOW_REMOTE_ADMIN=true`.
///
/// A request without a peer address (in-process `oneshot` calls) is treated as
/// loopback, which is what makes the integration tests exercise the same path.
async fn admin_gate(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    req: Request,
    next: Next,
) -> Response {
    let peer_is_loopback = req
        .extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|ci| ci.0.ip().is_loopback())
        .unwrap_or(true);
    if !peer_is_loopback && !state.config.allow_remote_admin {
        let dialect = state::dialect_of(req.uri());
        let mut resp = error::error_response(
            dialect,
            &rrouter_core::Error::Unauthorized(
                "admin API is loopback-only (set RROUTER_ALLOW_REMOTE_ADMIN=true)".into(),
            ),
        );
        *resp.status_mut() = axum::http::StatusCode::FORBIDDEN;
        return resp;
    }
    next.run(req).await
}
