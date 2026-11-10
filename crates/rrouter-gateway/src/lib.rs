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
pub mod events;
pub mod headers;
pub mod metrics;
pub mod models_registry;
pub mod provider_usage;
pub mod proxy;
pub mod quota_ping;
pub mod quota_ping_plan;
pub mod redaction;
pub mod routes;
pub mod semantic;
pub mod settings;
pub mod state;
pub mod syslog;
pub mod telemetry;
pub mod ui;
pub mod updates;
pub mod webhooks;

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
    spawn_provider_usage(&state);
    spawn_quota_ping(&state);
    updates::spawn_daily(&state);
    // Eval runs are in-process work; anything still active at boot is orphaned.
    routes::evals::sweep_orphan_runs(&state);
    // Read-only dashboard surface: usage, the model catalog and prompt templates. A
    // signed-in account of any role may read these; every mutation lives on the admin
    // router below, even where the paths are shared. Each router carries its own gate,
    // so the privilege split is one list to audit instead of a rule per handler.
    let api_read = Router::new()
        .route("/usage", get(routes::usage::list))
        .route("/usage/export.csv", get(routes::usage::export_csv))
        .route("/usage/summary", get(routes::usage::summary))
        .route("/usage/series", get(routes::usage::series))
        .route("/usage/{request_id}", get(routes::usage::detail))
        .route("/models", get(routes::models::registry_list))
        .route("/models/preferences", get(routes::models::list_preferences))
        // The preset library: metadata for every preset plus each account's active choice.
        // Mutations live on the admin router; activation is a session preference (below).
        .route("/model-presets", get(routes::model_presets::list))
        .route("/model-presets/active", get(routes::model_presets::active))
        .route("/model-presets/{id}", get(routes::model_presets::get))
        .route("/models/providers", get(routes::models::providers))
        .route("/templates", get(routes::templates::list))
        .route("/templates/{id}", get(routes::templates::get))
        .route("/templates/{id}/versions", get(routes::templates::versions))
        // Evals: suites, their cases and run history are readable by any signed-in
        // account; running a suite and editing it are admin actions below.
        .route("/evals/suites", get(routes::evals::list_suites))
        .route("/evals/suites/{id}", get(routes::evals::get_suite))
        .route("/evals/runs", get(routes::evals::list_runs))
        .route("/evals/runs/{id}", get(routes::evals::get_run))
        // Provider accounts: an admin sees every row, a team account only its team's
        // rows, and a shared row is global-scope only. Every mutation stays admin-only.
        .route("/connections", get(routes::connections::list))
        .route("/connections/{id}", get(routes::connections::get))
        // Teams are read-only here: a member sees their own team, an account with no team
        // sees none. Every mutation lives on the admin router below.
        .route("/teams", get(routes::teams::list))
        .route("/teams/{id}", get(routes::teams::get))
        .route("/teams/{id}/members", get(routes::teams::members))
        // Only that team's admin (or a gateway admin) may read these rows; the handler
        // decides, because the rule is a team role rather than "signed in".
        .route("/teams/{id}/keys", get(routes::teams::keys))
        .route_layer(axum::middleware::from_fn_with_state(
            Arc::clone(&state),
            require_reader,
        ));

    // Membership mutations: a gateway admin, or the `team_admin` of the team in the path.
    // Its own router so the widened rule is one line to audit next to `require_admin`.
    let api_team_admin = Router::new()
        .route("/teams/{id}/members", post(routes::teams::add_member))
        .route(
            "/teams/{id}/members/{user_id}",
            axum::routing::delete(routes::teams::remove_member),
        )
        .route_layer(axum::middleware::from_fn_with_state(
            Arc::clone(&state),
            require_team_admin,
        ));

    let api = Router::new()
        .route("/connections", post(routes::connections::create))
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
        // Preset administration. Activation is a per-account preference, not library
        // administration, so that one POST lives on the session router below.
        .route("/model-presets", post(routes::model_presets::create))
        .route(
            "/model-presets/{id}",
            patch(routes::model_presets::update).delete(routes::model_presets::remove),
        )
        .route(
            "/model-presets/{id}/duplicate",
            post(routes::model_presets::duplicate),
        )
        .route(
            "/model-presets/{id}/layout",
            put(routes::model_presets::save_layout),
        )
        .route(
            "/model-presets/{id}/default",
            post(routes::model_presets::set_default),
        )
        .route("/models/sync", post(routes::models::sync))
        .route("/models/fetch", post(routes::models::fetch))
        .route(
            "/models/custom",
            post(routes::models::custom).delete(routes::models::delete_custom),
        )
        .route("/models/test", post(routes::models::test))
        .route("/models/preference", put(routes::models::put_preference))
        .route("/route/explain", post(routes::explain::explain))
        .route(
            "/combos",
            get(routes::combos::list)
                .post(routes::combos::create)
                .delete(routes::combos::remove),
        )
        // The reads live on `api_read`; the same paths keep their writes here.
        .route("/templates", post(routes::templates::create))
        .route(
            "/templates/{id}",
            put(routes::templates::update).delete(routes::templates::remove),
        )
        .route("/evals/suites", post(routes::evals::create_suite))
        .route(
            "/evals/suites/{id}",
            patch(routes::evals::update_suite).delete(routes::evals::delete_suite),
        )
        .route("/evals/suites/{id}/cases", post(routes::evals::create_case))
        .route(
            "/evals/suites/{id}/cases/{case_id}",
            axum::routing::delete(routes::evals::delete_case),
        )
        .route("/evals/runs", post(routes::evals::start_run))
        .route("/teams", post(routes::teams::create))
        .route(
            "/teams/{id}",
            patch(routes::teams::update).delete(routes::teams::remove),
        )
        .route("/accounts/quotas", get(routes::quotas::list))
        .route("/accounts/usage", get(routes::quotas::usage))
        .route("/quota-pings", get(routes::quota_pings::list))
        .route(
            "/quota-pings/{connection_id}",
            put(routes::quota_pings::update),
        )
        .route(
            "/accounts/quotas/{id}",
            put(routes::quotas::update).delete(routes::quotas::remove),
        )
        .route("/audit", get(routes::audit::list))
        // Webhook receivers and their delivery queue. Admin-only: a receiver URL is an
        // egress capability, and the queue names administrative actions.
        .route(
            "/webhooks",
            get(routes::webhooks::list).post(routes::webhooks::create),
        )
        .route("/webhooks/status", get(routes::webhooks::status))
        .route(
            "/webhooks/{id}",
            patch(routes::webhooks::update).delete(routes::webhooks::remove),
        )
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
        .route("/update", get(routes::updates::status))
        .route("/update/check", post(routes::updates::check))
        .route("/update/download", post(routes::updates::download))
        .route("/update/apply", post(routes::updates::apply))
        .route("/update/settings", put(routes::updates::settings))
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
            require_admin,
        ))
        .merge(api_read)
        .merge(api_team_admin);

    // Prometheus scrapes `/metrics`, not `/api/metrics`, so it needs its own gate: the
    // same loopback-only rule as the admin API, and no API key — a scraper is not a
    // model client and must not need a virtual key to read counters.
    // Signed-out surfaces: creating the first user, signing in, accepting an invitation
    // and recovering a password. They sit outside `require_admin` by design.
    let auth_api = Router::new()
        .route("/api/auth/state", get(routes::auth::state))
        .route("/api/auth/setup", post(routes::auth::setup))
        .route("/api/auth/login", post(routes::auth::login))
        .route("/api/auth/logout", post(routes::auth::logout))
        .route("/api/auth/refresh", post(routes::auth::refresh))
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
        .route("/api/auth/signup", post(routes::auth::signup))
        // One account's active preset: session-scoped, CSRF-checked in the handler.
        .route(
            "/api/model-presets/{id}/activate",
            post(routes::model_presets::activate),
        );

    let ops = Router::new()
        .route("/metrics", get(metrics::scrape))
        .route_layer(axum::middleware::from_fn_with_state(
            Arc::clone(&state),
            require_admin,
        ));

    Router::new()
        .merge(auth_api)
        .route("/health", get(routes::health::health))
        .route("/ready", get(routes::health::ready))
        .route("/v1/models", get(v1_models))
        .route("/v1/models/search", get(routes::search::models))
        .route("/v1/models/{*id}", get(v1_model))
        .route("/v1/chat/completions", post(proxy::proxy))
        .route("/v1/messages", post(proxy::proxy))
        .route("/v1/responses", post(proxy::proxy))
        .route("/v1/responses/compact", post(proxy::compact))
        .route("/v1/messages/count_tokens", post(proxy::count_tokens))
        .route("/v1/embeddings", post(proxy::embeddings))
        .route("/v1/rerank", post(routes::rerank::rerank))
        .route("/v1/search", post(routes::search::search))
        // Provider-native storage and jobs: forwarded verbatim, `501` where the provider
        // declares no endpoint (see `routes::native` for the shared plumbing).
        .route(
            "/v1/files",
            post(routes::files::upload).get(routes::files::list),
        )
        .route("/v1/files/{id}", get(routes::files::get))
        .route("/v1/files/{id}/content", get(routes::files::content))
        .route(
            "/v1/batches",
            post(routes::batches::create).get(routes::batches::list),
        )
        .route("/v1/batches/{id}", get(routes::batches::get))
        .route("/v1/batches/{id}/cancel", post(routes::batches::cancel))
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

/// Starts the subscription-usage refresh loop exactly once per process.
///
/// Same rule as the models.dev timer: `app()` is the startup hook, the first pass waits a
/// minute, and outside a runtime there is nothing to schedule. The loop feeds
/// [`provider_usage::cached_hints`] without ever making a request wait on a provider.
fn spawn_provider_usage(state: &Arc<AppState>) {
    static START: std::sync::Once = std::sync::Once::new();
    if tokio::runtime::Handle::try_current().is_err() {
        return;
    }
    START.call_once(|| provider_usage::spawn_refresh(Arc::clone(state)));
}

/// Starts the quota-ping scheduler once per process, next to the usage refresh.
fn spawn_quota_ping(state: &Arc<AppState>) {
    static START: std::sync::Once = std::sync::Once::new();
    if tokio::runtime::Handle::try_current().is_err() {
        return;
    }
    START.call_once(|| quota_ping::spawn(Arc::clone(state)));
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
    let Some(allow) = state.config.load().cors_origin_for(&origin) else {
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

/// Admin API gate. `/api/*` (except the read-only surface) and `/metrics` admit:
///
/// 1. a configured `RROUTER_ADMIN_TOKEN` presented in the request,
/// 2. a live key with `scope=admin` (`Authorization: Bearer …` or `x-admin-token`),
/// 3. a dashboard session whose account has the `admin` role,
/// 4. no token configured and the peer is loopback (or `RROUTER_ALLOW_REMOTE_ADMIN`).
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
async fn require_admin(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    req: Request,
    next: Next,
) -> Response {
    let facts = probe(&req);
    match access(&state, &facts).await {
        Ok(Access::Admin) => next.run(req).await,
        // Same refusal a non-admin always got, so the dashboard can explain the role.
        Ok(Access::Reader) => admin_forbidden(
            facts.dialect,
            "role_required",
            "this account cannot administer the gateway",
        ),
        Err(response) => *response,
    }
}

/// Read-only gate: every credential `require_admin` admits, plus any signed-in account
/// regardless of role.
///
/// The routes behind it are GET-only, and the safe-method check keeps that true: a
/// reader credential must never authorise a mutation even if someone later registers one
/// on this router by mistake.
async fn require_reader(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    req: Request,
    next: Next,
) -> Response {
    let facts = probe(&req);
    match access(&state, &facts).await {
        Ok(_) if facts.safe_method => next.run(req).await,
        Ok(_) => admin_forbidden(
            facts.dialect,
            "role_required",
            "this account cannot administer the gateway",
        ),
        Err(response) => *response,
    }
}

/// Membership mutation gate: a gateway admin, or the `team_admin` of the team named in the
/// path.
///
/// The role is resolved here rather than in each handler so the routes behind this gate
/// cannot disagree, and the CSRF header is enforced here too — [`access`] only checks it
/// for admin sessions, which is exactly the case this gate widens.
async fn require_team_admin(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    req: Request,
    next: Next,
) -> Response {
    let facts = probe(&req);
    match access(&state, &facts).await {
        Ok(Access::Admin) => next.run(req).await,
        Ok(Access::Reader) => {
            if !facts.csrf {
                return admin_forbidden(
                    facts.dialect,
                    "csrf_required",
                    "the x-rrouter-csrf header is required for this request",
                );
            }
            let Some(team) = team_id_from_path(req.uri().path()) else {
                return admin_forbidden(
                    facts.dialect,
                    "role_required",
                    "this account cannot manage that team",
                );
            };
            let Some(user) = routes::auth::current_user(&state, req.headers()).await else {
                return admin_forbidden(
                    facts.dialect,
                    "role_required",
                    "this account cannot manage that team",
                );
            };
            match state.store.team_member_role(team, user.id).await {
                Ok(Some(rrouter_store::TeamRole::TeamAdmin)) => next.run(req).await,
                Ok(_) => admin_forbidden(
                    facts.dialect,
                    "role_required",
                    "this account cannot manage that team",
                ),
                Err(e) => error::error_response(facts.dialect, &e),
            }
        }
        Err(response) => *response,
    }
}

/// The `{team}` segment of `/api/teams/{team}/members…`, for the gate above.
///
/// The router has already matched the path; this only needs the id. A malformed one is
/// refused as "not allowed" rather than passed to a handler that would answer `404`, so a
/// reader cannot use the difference to probe which teams exist.
fn team_id_from_path(path: &str) -> Option<uuid::Uuid> {
    let mut segments = path.split('/').filter(|segment| !segment.is_empty());
    while let Some(segment) = segments.next() {
        if segment == "teams" {
            return segments
                .next()
                .and_then(|raw| uuid::Uuid::parse_str(raw).ok());
        }
    }
    None
}

/// What the credentials on a request prove.
enum Access {
    /// Full administration.
    Admin,
    /// A signed-in dashboard account whose role is not `admin`.
    Reader,
}

/// The facts one request contributes to an access decision.
///
/// Read before the first `.await`, because `axum::body::Body` is not `Sync`: holding a
/// `&Request` across an await point would make the middleware future non-`Send` and the
/// whole router stop compiling.
struct Probe {
    dialect: rrouter_core::Dialect,
    safe_method: bool,
    presented: Option<String>,
    session: Option<String>,
    csrf: bool,
    /// In-process `oneshot` calls carry no peer address and count as loopback.
    loopback: bool,
}

fn probe(req: &Request) -> Probe {
    Probe {
        dialect: state::dialect_of(req.uri()),
        safe_method: matches!(
            req.method(),
            &axum::http::Method::GET | &axum::http::Method::HEAD | &axum::http::Method::OPTIONS
        ),
        presented: admin_credential(req.headers()),
        session: auth::session_token(req.headers()),
        csrf: auth::csrf_ok(req.headers()),
        loopback: req
            .extensions()
            .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
            .map(|ci| ci.0.ip().is_loopback())
            .unwrap_or(true),
    }
}

/// Resolve the credential on a request once, so the two gates cannot drift apart.
///
/// Order is the pre-existing contract and is load-bearing: configured token, then
/// dashboard session (a session works even when a token is configured), then the
/// loopback rule, then an admin-scoped key. A signed-in non-admin stops at
/// [`Access::Reader`] instead of falling through, so the role is authoritative even on
/// the operator's own machine; the CSRF check stays on the write path for admins.
async fn access(state: &Arc<AppState>, facts: &Probe) -> Result<Access, Box<Response>> {
    let dialect = facts.dialect;
    // One snapshot for the whole decision: a token rotation mid-request must not compare
    // against one value and then fall through using another.
    let config = state.config.load();
    if let Some(expected) = config.admin_token.as_deref() {
        if facts
            .presented
            .as_deref()
            .is_some_and(|presented| constant_time_eq(presented, expected))
        {
            return Ok(Access::Admin);
        }
    }
    if let Some(token) = facts.session.as_deref() {
        if let Some(claims) = state.auth.verify_session(token) {
            match state.store.session_user(&claims.sid, now_unix()).await {
                Ok(Some(user)) if !user.is_admin() => return Ok(Access::Reader),
                Ok(Some(_)) if !facts.safe_method && !facts.csrf => {
                    return Err(Box::new(admin_forbidden(
                        dialect,
                        "csrf_required",
                        "the x-rrouter-csrf header is required for this request",
                    )))
                }
                Ok(Some(_)) => return Ok(Access::Admin),
                Ok(None) => {}
                Err(e) => return Err(Box::new(error::error_response(dialect, &e))),
            }
        }
    }
    if config.admin_token.is_none() && (facts.loopback || config.allow_remote_admin) {
        return Ok(Access::Admin);
    }
    // Only a credential that could be a gateway key is worth a database lookup: an
    // arbitrary token can never match one.
    let key = match facts.presented.as_deref() {
        Some(token) if token.starts_with(rrouter_store::API_KEY_PREFIX) => {
            state.live_api_key(token).await
        }
        _ => Ok(None),
    };
    match key {
        Ok(Some(key)) if key.scope == rrouter_store::KeyScope::Admin => Ok(Access::Admin),
        Ok(Some(_)) => Err(Box::new(admin_required(dialect))),
        Ok(None) if config.admin_token.is_some() => Err(Box::new(admin_unauthorized(dialect))),
        Ok(None) => Err(Box::new(admin_loopback_only(dialect))),
        Err(e) => Err(Box::new(error::error_response(dialect, &e))),
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
