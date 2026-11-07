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

pub mod auth;
pub mod config;
pub mod error;
pub mod headers;
pub mod metrics;
pub mod models_registry;
pub mod provider_usage;
pub mod proxy;
pub mod routes;
pub mod settings;
pub mod state;
pub mod syslog;
pub mod ui;

use std::sync::Arc;

use axum::extract::Request;
use axum::http::header::{
    ACCESS_CONTROL_ALLOW_HEADERS, ACCESS_CONTROL_ALLOW_METHODS, ACCESS_CONTROL_ALLOW_ORIGIN,
    ACCESS_CONTROL_MAX_AGE, AUTHORIZATION, VARY,
};
use axum::http::{HeaderMap, HeaderValue};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post, put};
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
        .route(
            "/connections",
            get(routes::connections::list).post(routes::connections::create),
        )
        .route(
            "/connections/{id}",
            patch(routes::connections::update).delete(routes::connections::remove),
        )
        .route("/connections/{id}/test", post(routes::connections::test))
        .route("/keys", get(routes::keys::list).post(routes::keys::create))
        .route(
            "/keys/{id}",
            patch(routes::keys::update).delete(routes::keys::remove),
        )
        .route(
            "/keys/{id}/models",
            get(routes::keys::list_models).put(routes::keys::put_models),
        )
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
            get(routes::combos::list)
                .post(routes::combos::create)
                .delete(routes::combos::remove),
        )
        .route(
            "/templates",
            get(routes::templates::list).post(routes::templates::create),
        )
        .route(
            "/templates/{id}",
            get(routes::templates::get)
                .put(routes::templates::update)
                .delete(routes::templates::remove),
        )
        .route("/templates/{id}/versions", get(routes::templates::versions))
        .route("/accounts/quotas", get(routes::quotas::list))
        .route("/accounts/usage", get(routes::quotas::usage))
        .route(
            "/accounts/quotas/{id}",
            put(routes::quotas::update).delete(routes::quotas::remove),
        )
        .route("/audit", get(routes::audit::list))
        .route("/users", get(routes::users::list))
        .route(
            "/users/{id}",
            put(routes::users::update).delete(routes::users::remove),
        )
        .route(
            "/users/invites",
            get(routes::auth::list_invites).post(routes::auth::create_invite),
        )
        .route(
            "/users/invites/{id}",
            axum::routing::delete(routes::auth::revoke_invite),
        )
        .route("/usage", get(routes::usage::list))
        .route("/usage/export.csv", get(routes::usage::export_csv))
        .route("/usage/{request_id}", get(routes::usage::detail))
        .route("/usage/summary", get(routes::usage::summary))
        .route("/usage/series", get(routes::usage::series))
        .route("/system", get(routes::system::system))
        .route(
            "/system/logs",
            get(routes::system_logs::list)
                .put(routes::system_logs::update)
                .delete(routes::system_logs::clear),
        )
        .route(
            "/settings",
            get(routes::settings::get_settings).put(routes::settings::put_settings),
        )
        .route("/oauth/providers", get(routes::oauth::providers))
        .route("/oauth/{provider}/start", post(routes::oauth::start))
        .route("/oauth/{provider}/status/{id}", get(routes::oauth::status))
        .route("/oauth/{provider}/complete", post(routes::oauth::complete))
        .route(
            "/oauth/{provider}/{id}",
            axum::routing::delete(routes::oauth::cancel),
        )
        .route("/ui/panels", get(ui::panels))
        .route_layer(axum::middleware::from_fn_with_state(
            Arc::clone(&state),
            admin_gate,
        ));

    // Prometheus scrapes `/metrics`, not `/api/metrics`, so it needs its own gate: the
    // same loopback-only rule as the admin API, and no API key — a scraper is not a
    // model client and must not need a virtual key to read counters.
    // Signed-out surfaces: creating the first user, signing in, accepting an invitation
    // and recovering a password. They sit outside `admin_gate` by design.
    let auth_api = Router::new()
        .route("/api/auth/state", get(routes::auth::state))
        .route("/api/auth/setup", post(routes::auth::setup))
        .route("/api/auth/login", post(routes::auth::login))
        .route("/api/auth/logout", post(routes::auth::logout))
        .route("/api/auth/recover", post(routes::auth::recover))
        .route("/api/auth/invites/{token}", get(routes::auth::invite))
        .route(
            "/api/auth/me",
            get(routes::auth::me).put(routes::auth::update_me),
        )
        .route(
            "/api/auth/me/sessions/revoke-others",
            post(routes::auth::revoke_other_sessions),
        )
        .route(
            "/api/auth/me/sessions/{id}",
            axum::routing::delete(routes::auth::revoke_session),
        )
        .route("/api/auth/signup", post(routes::auth::signup));

    let ops = Router::new()
        .route("/metrics", get(metrics::scrape))
        .route_layer(axum::middleware::from_fn_with_state(
            Arc::clone(&state),
            admin_gate,
        ));

    Router::new()
        .merge(auth_api)
        .route("/health", get(routes::health::health))
        .route("/ready", get(routes::health::ready))
        .route("/v1/models", get(v1_models))
        .route("/v1/models/{*id}", get(v1_model))
        .route("/v1/chat/completions", post(proxy::proxy))
        .route("/v1/messages", post(proxy::proxy))
        .route("/v1/responses", post(proxy::proxy))
        .route("/v1/responses/compact", post(proxy::compact))
        .route("/v1/messages/count_tokens", post(proxy::count_tokens))
        .route("/v1/embeddings", post(proxy::embeddings))
        .route("/v1/images/generations", post(proxy::images))
        .route("/v1/audio/speech", post(proxy::speech))
        .route("/v1/audio/transcriptions", post(proxy::transcriptions))
        .route("/v1/audio/voices", get(proxy::voices))
        .route("/", get(ui::index))
        .route("/app", get(ui::app_index))
        .route("/app/", get(ui::app_index))
        .route("/app/{*path}", get(ui::app_asset))
        .merge(ops)
        .route("/{*path}", get(ui::asset))
        .nest("/api", api)
        .with_state(Arc::clone(&state))
        .layer(axum::middleware::from_fn_with_state(state, cors))
        // Outermost, so the gate's rejections, CORS preflights and the dashboard all
        // get hardened, and so it sees the final response: a header already set by a
        // handler or an upstream hop is left alone.
        .layer(axum::middleware::from_fn(headers::harden))
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
    let Some(origin) = origin else {
        return response;
    };
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
        HeaderValue::from_static(
            "content-type, authorization, x-api-key, x-admin-token, anthropic-version, anthropic-beta",
        ),
    );
    headers.insert(ACCESS_CONTROL_MAX_AGE, HeaderValue::from_static("600"));
    response
}

/// Admin API gate. `/api/*` and `/metrics` are admitted when any of these holds:
///
/// 1. a configured `RROUTER_ADMIN_TOKEN` is presented,
/// 2. a live key with `scope=admin` is presented (`Authorization: Bearer …` or
///    `x-admin-token`),
/// 3. no token is configured and the peer is loopback (or `RROUTER_ALLOW_REMOTE_ADMIN`
///    is on).
///
/// A configured token still *replaces* the loopback rule instead of adding to it: behind
/// a reverse proxy every peer is the proxy, so the socket address proves nothing about
/// who is calling. `X-Forwarded-For` is deliberately never consulted — it is
/// client-written.
///
/// A live inference-scoped key is `403 admin_required`: naming the scope is more useful
/// than `401`, because the caller holds a real key and only lacks the authority. A
/// request without a peer address (in-process `oneshot` calls) counts as loopback, which
/// is what makes the integration tests exercise the same path.
async fn admin_gate(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    req: Request,
    next: Next,
) -> Response {
    let dialect = state::dialect_of(req.uri());
    let safe_method = matches!(
        req.method(),
        &axum::http::Method::GET | &axum::http::Method::HEAD | &axum::http::Method::OPTIONS
    );
    let presented = admin_credential(req.headers());
    if let Some(expected) = state.config.admin_token.as_deref() {
        if presented
            .as_deref()
            .is_some_and(|presented| constant_time_eq(presented, expected))
        {
            return next.run(req).await;
        }
    }
    // A dashboard session identifies a person, so it works even when
    // `RROUTER_ADMIN_TOKEN` is configured (that token is for automation). A signed-in
    // non-admin is refused here rather than at each route; a cookie-authenticated write
    // must carry the CSRF header.
    if let Some(token) = auth::session_token(req.headers()) {
        match state
            .store
            .session_user(&auth::session_key(&token), now_unix())
            .await
        {
            Ok(Some(user)) if !user.is_admin() => {
                return admin_forbidden(
                    dialect,
                    "role_required",
                    "this account cannot administer the gateway",
                )
            }
            Ok(Some(_)) if !safe_method && !auth::csrf_ok(req.headers()) => {
                return admin_forbidden(
                    dialect,
                    "csrf_required",
                    "the x-rrouter-csrf header is required for this request",
                )
            }
            Ok(Some(_)) => return next.run(req).await,
            Ok(None) => {}
            Err(e) => return error::error_response(dialect, &e),
        }
    }
    if state.config.admin_token.is_none() {
        let peer_is_loopback = req
            .extensions()
            .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
            .map(|ci| ci.0.ip().is_loopback())
            .unwrap_or(true);
        if peer_is_loopback || state.config.allow_remote_admin {
            return next.run(req).await;
        }
    }
    // Only a credential that could be a gateway key is worth a database lookup: an
    // arbitrary token can never match one.
    let key = match presented.as_deref() {
        Some(token) if token.starts_with(rrouter_store::API_KEY_PREFIX) => {
            state.live_api_key(token).await
        }
        _ => Ok(None),
    };
    match key {
        Ok(Some(key)) if key.scope == rrouter_store::KeyScope::Admin => next.run(req).await,
        Ok(Some(_)) => admin_required(dialect),
        Ok(None) if state.config.admin_token.is_some() => admin_unauthorized(dialect),
        Ok(None) => admin_loopback_only(dialect),
        Err(e) => error::error_response(dialect, &e),
    }
}

/// The admin credential: `Authorization: Bearer …` or `x-admin-token: …`.
///
/// `x-api-key` is *not* accepted here: that header belongs to model clients, and an
/// admin call must not be authorised by a key it can spend.
fn admin_credential(headers: &HeaderMap) -> Option<String> {
    if let Some(value) = headers.get(AUTHORIZATION).and_then(|v| v.to_str().ok()) {
        let value = value.trim();
        let token = value
            .strip_prefix("Bearer ")
            .or_else(|| value.strip_prefix("bearer "))
            .unwrap_or(value)
            .trim();
        if !token.is_empty() {
            return Some(token.to_string());
        }
    }
    headers
        .get("x-admin-token")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// Compare a presented secret with the configured one without an early exit.
///
/// Length is compared too (a length difference is not a secret), but every byte is
/// touched regardless of where the first mismatch is, so the response time does not
/// leak how much of the token a caller guessed.
fn constant_time_eq(presented: &str, expected: &str) -> bool {
    let a = presented.as_bytes();
    let b = expected.as_bytes();
    let mut diff = (a.len() ^ b.len()) as u64;
    for i in 0..a.len().max(b.len()) {
        let left = a.get(i).copied().unwrap_or(0);
        let right = b.get(i).copied().unwrap_or(0);
        diff |= u64::from(left ^ right);
    }
    diff == 0
}

/// `503 overloaded` with `Retry-After: 1`: the concurrency cap is a "come back in a
/// moment", not a rate limit, so the client is told to retry rather than to give up.
pub(crate) fn overloaded_response(inbound: rrouter_core::Dialect) -> Response {
    let obj = error::error_object_with(
        axum::http::StatusCode::SERVICE_UNAVAILABLE.as_u16(),
        "too many concurrent requests; retry shortly",
        Some("overloaded"),
    );
    let body = match inbound {
        rrouter_core::Dialect::AnthropicMessages => {
            serde_json::json!({"type": "error", "error": obj})
        }
        _ => serde_json::json!({"error": obj}),
    };
    let mut resp = (
        axum::http::StatusCode::SERVICE_UNAVAILABLE,
        axum::Json(body),
    )
        .into_response();
    resp.headers_mut().insert(
        axum::http::header::RETRY_AFTER,
        HeaderValue::from_static("1"),
    );
    resp
}

/// `/v1/models` takes a concurrency slot like every other `/v1/*` route: a client that
/// saturates the gateway is told to back off whatever it asks for. The catalog answer is
/// built in memory, so the permit ends with this scope.
async fn v1_models(axum::extract::State(state): axum::extract::State<Arc<AppState>>) -> Response {
    let _permit = match state.try_slot() {
        state::Slot::Granted(permit) => permit,
        state::Slot::Full => return overloaded_response(rrouter_core::Dialect::OpenAiChat),
    };
    routes::models::list_models(axum::extract::State(state)).await
}

/// `GET /v1/models/{id}` shares `/v1/models`' slot rule: a catalog answer is still a
/// `/v1/*` request, and the id may contain slashes (`openai/gpt-image-1`).
async fn v1_model(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Response {
    let _permit = match state.try_slot() {
        state::Slot::Granted(permit) => permit,
        state::Slot::Full => return overloaded_response(rrouter_core::Dialect::OpenAiChat),
    };
    routes::models::get_model(axum::extract::State(state), axum::extract::Path(id)).await
}

/// `403 admin_required`: a live key asked for admin surface with `scope=inference`.
fn admin_required(dialect: rrouter_core::Dialect) -> Response {
    error::error_response(
        dialect,
        &rrouter_core::Error::Forbidden {
            code: "admin_required",
            message: "admin API requires an admin-scoped key or RROUTER_ADMIN_TOKEN".into(),
        },
    )
}

/// `403` with an explicit machine code (role and CSRF refusals name themselves so the
/// dashboard can show the right message).
fn admin_forbidden(dialect: rrouter_core::Dialect, code: &'static str, message: &str) -> Response {
    error::error_response(
        dialect,
        &rrouter_core::Error::Forbidden {
            code,
            message: message.to_string(),
        },
    )
}

/// `403` for a remote peer with no admin credential when no token is configured.
fn admin_loopback_only(dialect: rrouter_core::Dialect) -> Response {
    let mut resp = error::error_response(
        dialect,
        &rrouter_core::Error::Unauthorized(
            "admin API is loopback-only (set RROUTER_ALLOW_REMOTE_ADMIN=true or RROUTER_ADMIN_TOKEN)"
                .into(),
        ),
    );
    *resp.status_mut() = axum::http::StatusCode::FORBIDDEN;
    resp
}

/// `401 admin_unauthorized` in the inbound dialect's error shape.
fn admin_unauthorized(dialect: rrouter_core::Dialect) -> Response {
    let obj = error::error_object_with(
        axum::http::StatusCode::UNAUTHORIZED.as_u16(),
        "admin API requires RROUTER_ADMIN_TOKEN (Authorization: Bearer … or x-admin-token)",
        Some("admin_unauthorized"),
    );
    let body = match dialect {
        rrouter_core::Dialect::AnthropicMessages => {
            serde_json::json!({"type": "error", "error": obj})
        }
        _ => serde_json::json!({"error": obj}),
    };
    (axum::http::StatusCode::UNAUTHORIZED, axum::Json(body)).into_response()
}
