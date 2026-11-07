//! `/api/auth/*` — the unauthenticated auth surface plus session lifecycle.
//!
//! These routes sit outside the admin gate on purpose: creating the first user, signing
//! in, accepting an invitation and recovering a password must all work signed out.

use std::sync::Arc;
use std::sync::OnceLock;

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use rrouter_core::Error;
use rrouter_store::{
    hash_password, hash_secret, normalize_email, random_recovery_codes, random_token,
    verify_password, User, UserRole,
};
use serde_json::{json, Value};

use super::{admin_error, body_json, ok_json};
use crate::auth::{
    self, clear_session_cookie, is_secure, session_key, session_token, set_session_cookie,
    REMEMBER_SECS, SESSION_SECS,
};
use crate::state::AppState;

/// How long an invitation link stays usable.
const INVITE_SECS: i64 = 7 * 24 * 60 * 60;
/// Minimum password length; the strength meter is advisory.
pub const MIN_PASSWORD: usize = 12;

pub fn user_json(user: &User) -> Value {
    json!({
        "id": user.id.to_string(),
        "name": user.name,
        "email": user.email,
        "role": user.role.as_str(),
        "is_owner": user.is_owner,
        "disabled": user.disabled,
        "created_at": user.created_at,
        "last_sign_in_at": user.last_sign_in_at,
    })
}

/// Persist one audit row; auditing never fails the request it describes.
pub(crate) async fn audit(state: &AppState, kind: &str, actor: Option<&str>, ip: &str, detail: &str) {
    if let Err(e) = state
        .store
        .record_audit(kind, actor, Some(ip), Some(detail))
        .await
    {
        tracing::warn!(kind, error = %e, "audit write failed");
    }
}

/// Public id for one session row: a hash of the stored hash, so the API can name a
/// session without exposing anything that authenticates.
pub(crate) fn session_public_id(token_hash: &str) -> String {
    hash_secret(&format!("rrouter-session:{token_hash}"))
}

/// A machine-readable failure with an explicit status (auth has codes the core error
/// enum does not carry, such as `already_initialized`).
fn fail(status: StatusCode, code: &str, message: &str) -> Response {
    let kind = if status.is_server_error() { "api_error" } else { "invalid_request_error" };
    (
        status,
        axum::Json(json!({ "error": { "code": code, "message": message, "type": kind } })),
    )
        .into_response()
}

/// A fixed hash so a sign-in for an unknown email spends the same argon2 work as a real
/// one; without it, response time tells an attacker which emails exist.
fn dummy_hash() -> &'static str {
    static HASH: OnceLock<String> = OnceLock::new();
    HASH.get_or_init(|| hash_password("rrouter-dummy-password").unwrap_or_default())
}

/// A split field: `(value, problem_code)`.
fn text(value: &Value, field: &str) -> Option<String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

fn email_problem(email: &str) -> Option<&'static str> {
    if email.len() > 254 || !email.contains('@') || email.starts_with('@') || email.ends_with('@') {
        return Some("enter a valid email address");
    }
    None
}

pub async fn current_user(state: &AppState, headers: &HeaderMap) -> Option<User> {
    let token = session_token(headers)?;
    state
        .store
        .session_user(&session_key(&token), crate::now_unix())
        .await
        .ok()
        .flatten()
}

/// `GET /api/auth/state` — unauthenticated; decides every route on the client.
pub async fn state(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let initialized = state.store.users_exist().await.unwrap_or(true);
    let user = if initialized {
        current_user(&state, &headers).await
    } else {
        None
    };
    ok_json(json!({
        "initialized": initialized,
        "setup_code_required": !initialized,
        "signup": "invite_only",
        "user": user.as_ref().map(user_json),
    }))
}

/// `POST /api/auth/setup` — create the owner exactly once, consuming the setup code.
pub async fn setup(State(state): State<Arc<AppState>>, headers: HeaderMap, body: Body) -> Response {
    let value = match body_json(body).await {
        Ok(v) => v,
        Err(resp) => return *resp,
    };
    match state.store.users_exist().await {
        Ok(true) => return fail(StatusCode::CONFLICT, "already_initialized", "this RRouter already has an administrator"),
        Ok(false) => {}
        Err(e) => return admin_error(&e),
    }
    let Some(code) = text(&value, "setup_code") else {
        return fail(StatusCode::BAD_REQUEST, "setup_code_required", "the setup code printed in the terminal is required");
    };
    if !state.auth.verify(&code) {
        return fail(
            StatusCode::FORBIDDEN,
            "setup_code_invalid",
            "That setup code isn't valid. Check the terminal where RRouter is running.",
        );
    }
    let Some(name) = text(&value, "name") else {
        return fail(StatusCode::BAD_REQUEST, "invalid_name", "enter your full name");
    };
    let Some(email) = text(&value, "email") else {
        return fail(StatusCode::BAD_REQUEST, "invalid_email", "enter your email");
    };
    if let Some(problem) = email_problem(&email) {
        return fail(StatusCode::BAD_REQUEST, "invalid_email", problem);
    }
    let password = value.get("password").and_then(Value::as_str).unwrap_or("");
    if password.chars().count() < MIN_PASSWORD {
        return fail(StatusCode::BAD_REQUEST, "password_too_short", "use at least 12 characters");
    }
    let hash = match hash_password(password) {
        Ok(hash) => hash,
        Err(e) => return admin_error(&e),
    };
    let user = match state.store.create_first_user(&name, &email, &hash).await {
        Ok(Some(user)) => user,
        Ok(None) => {
            return fail(StatusCode::CONFLICT, "already_initialized", "this RRouter already has an administrator")
        }
        Err(e) => return admin_error(&e),
    };
    state.auth.retire();
    audit(&state, "setup", Some(&user.email), &auth::client_ip(&headers, None), "owner account created").await;
    tracing::info!(email = %user.email, "setup: owner account created");
    let codes = match issue_recovery_codes(&state, &user).await {
        Ok(codes) => codes,
        Err(e) => return admin_error(&e),
    };
    let mut resp = match start_session(&state, &headers, &user.id, true).await {
        Ok(resp) => resp,
        Err(e) => return admin_error(&e),
    };
    let body = json!({ "user": user_json(&user), "recovery_codes": codes });
    *resp.status_mut() = StatusCode::CREATED;
    *resp.body_mut() = axum::body::Body::from(body.to_string());
    resp.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    resp
}

async fn issue_recovery_codes(state: &AppState, user: &User) -> rrouter_core::Result<Vec<String>> {
    let codes = random_recovery_codes();
    let hashes: Vec<String> = codes.iter().map(|code| hash_secret(&normalize_code(code))).collect();
    state.store.add_recovery_codes(user.id, &hashes).await?;
    Ok(codes)
}

fn normalize_code(code: &str) -> String {
    code.chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_uppercase()
}

/// Create the session row and build the empty 204/201 response carrying the cookie.
async fn start_session(
    state: &AppState,
    headers: &HeaderMap,
    user_id: &uuid::Uuid,
    remember: bool,
) -> rrouter_core::Result<Response> {
    let token = random_token();
    let lifetime = if remember { REMEMBER_SECS } else { SESSION_SECS };
    state
        .store
        .create_session(&session_key(&token), *user_id, crate::now_unix() + lifetime)
        .await?;
    let mut resp = StatusCode::NO_CONTENT.into_response();
    set_session_cookie(&mut resp, &token, remember, is_secure(headers));
    Ok(resp)
}

/// Generic sign-in failure: unknown email, wrong password and disabled account are
/// deliberately indistinguishable.
fn invalid_credentials() -> Response {
    fail(
        StatusCode::UNAUTHORIZED,
        "invalid_credentials",
        "Email or password is incorrect",
    )
}

/// `POST /api/auth/login`
pub async fn login(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    peer: crate::auth::Peer,
    body: Body,
) -> Response {
    let value = match body_json(body).await {
        Ok(v) => v,
        Err(resp) => return *resp,
    };
    let Some(email) = text(&value, "email") else {
        return invalid_credentials();
    };
    let password = value.get("password").and_then(Value::as_str).unwrap_or("");
    if password.is_empty() {
        return invalid_credentials();
    }
    let remember = value.get("remember").and_then(Value::as_bool).unwrap_or(false);
    let email = normalize_email(&email);
    let ip = auth::client_ip(&headers, peer.0);
    // The email bucket always applies. The IP bucket only guards non-loopback callers:
    // throttling the operator's own machine by address would lock the dashboard for a
    // minute after any five typos, while a remote or proxied client must still be limited.
    let via_proxy = headers.get("x-forwarded-for").is_some();
    let remote = peer.0.is_some_and(|addr| !addr.ip().is_loopback());
    let mut keys = vec![format!("email:{email}")];
    if via_proxy || remote {
        keys.push(format!("ip:{ip}"));
    }
    for key in &keys {
        if let Some(wait) = state.auth.throttle(key) {
            let mut resp = fail(
                StatusCode::TOO_MANY_REQUESTS,
                "too_many_attempts",
                "Too many attempts. Try again in a minute.",
            );
            if let Ok(value) = HeaderValue::from_str(&wait.to_string()) {
                resp.headers_mut()
                    .insert(axum::http::header::RETRY_AFTER, value);
            }
            return resp;
        }
    }

    let user = match state.store.user_by_email(&email).await {
        Ok(user) => user,
        Err(e) => return admin_error(&e),
    };
    let (valid, user) = match user {
        Some(user) => {
            let valid = verify_password(password, &stored_hash(&state, &user).await.unwrap_or_default());
            (valid, Some(user))
        }
        None => {
            verify_password(password, dummy_hash());
            (false, None)
        }
    };
    let Some(user) = user.filter(|u| valid && !u.disabled) else {
        for key in &keys {
            state.auth.note_failure(key);
        }
        return invalid_credentials();
    };
    for key in &keys {
        state.auth.clear_failures(key);
    }
    if let Err(e) = state.store.mark_signed_in(user.id).await {
        return admin_error(&e);
    }
    let mut resp = match start_session(&state, &headers, &user.id, remember).await {
        Ok(resp) => resp,
        Err(e) => return admin_error(&e),
    };
    let body = json!({ "user": user_json(&user) });
    *resp.status_mut() = StatusCode::OK;
    *resp.body_mut() = axum::body::Body::from(body.to_string());
    resp.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    resp
}

/// The stored hash for a user. `Store` deliberately hides it from the `User` type, so
/// sign-in reads it through the one query that needs it.
async fn stored_hash(state: &AppState, user: &User) -> Option<String> {
    state.store.password_hash(user.id).await.ok().flatten()
}

/// `GET /api/auth/me` — the signed-in person and every live session of theirs.
pub async fn me(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let Some(user) = current_user(&state, &headers).await else {
        return fail(StatusCode::UNAUTHORIZED, "sign_in_required", "sign in to continue");
    };
    let current = session_token(&headers)
        .map(|token| session_key(&token))
        .unwrap_or_default();
    let sessions = match state.store.sessions_for_user(user.id).await {
        Ok(rows) => rows,
        Err(e) => return admin_error(&e),
    };
    let sessions: Vec<Value> = sessions
        .iter()
        .map(|session| {
            json!({
                "id": session_public_id(&session.token_hash),
                "created_at": session.created_at,
                "expires_at": session.expires_at,
                "current": session.token_hash == current,
            })
        })
        .collect();
    ok_json(json!({ "user": user_json(&user), "sessions": sessions }))
}

/// `PUT /api/auth/me` — rename, or change the password with the current one.
pub async fn update_me(State(state): State<Arc<AppState>>, headers: HeaderMap, body: Body) -> Response {
    let Some(user) = current_user(&state, &headers).await else {
        return fail(StatusCode::UNAUTHORIZED, "sign_in_required", "sign in to continue");
    };
    let value = match body_json(body).await {
        Ok(v) => v,
        Err(resp) => return *resp,
    };
    let ip = auth::client_ip(&headers, None);
    let mut cookie: Option<Response> = None;
    if let Some(name) = text(&value, "name") {
        if let Err(e) = state.store.rename_user(user.id, &name).await {
            return admin_error(&e);
        }
        audit(&state, "profile_updated", Some(&user.email), &ip, "name changed").await;
    }
    if let Some(password) = value.get("new_password").and_then(Value::as_str) {
        if password.chars().count() < MIN_PASSWORD {
            return fail(StatusCode::BAD_REQUEST, "password_too_short", "use at least 12 characters");
        }
        let current = value.get("current_password").and_then(Value::as_str).unwrap_or("");
        let stored = match state.store.password_hash(user.id).await {
            Ok(Some(hash)) => hash,
            _ => return admin_error(&Error::Store("account has no password".into())),
        };
        if !verify_password(current, &stored) {
            audit(&state, "password_change_failed", Some(&user.email), &ip, "wrong current password").await;
            return fail(StatusCode::FORBIDDEN, "invalid_password", "the current password is incorrect");
        }
        let hash = match hash_password(password) {
            Ok(hash) => hash,
            Err(e) => return admin_error(&e),
        };
        if let Err(e) = state.store.set_user_password(user.id, &hash).await {
            return admin_error(&e);
        }
        audit(&state, "password_changed", Some(&user.email), &ip, "sessions ended").await;
        // The password change ended every session, including this one; keep the current
        // tab signed in with a fresh cookie.
        match start_session(&state, &headers, &user.id, false).await {
            Ok(resp) => cookie = Some(resp),
            Err(e) => return admin_error(&e),
        }
    }
    let refreshed = state.store.user(user.id).await.ok().flatten().unwrap_or(user);
    let payload = json!({ "user": user_json(&refreshed) });
    let mut resp = cookie.unwrap_or_else(|| ok_json(Value::Null));
    *resp.status_mut() = StatusCode::OK;
    *resp.body_mut() = axum::body::Body::from(payload.to_string());
    resp.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    resp
}

/// `DELETE /api/auth/me/sessions/{id}` — revoke one device.
pub async fn revoke_session(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Response {
    let Some(user) = current_user(&state, &headers).await else {
        return fail(StatusCode::UNAUTHORIZED, "sign_in_required", "sign in to continue");
    };
    let current = session_token(&headers)
        .map(|token| session_key(&token))
        .unwrap_or_default();
    let sessions = match state.store.sessions_for_user(user.id).await {
        Ok(rows) => rows,
        Err(e) => return admin_error(&e),
    };
    let Some(target) = sessions.into_iter().find(|s| session_public_id(&s.token_hash) == id) else {
        return fail(StatusCode::NOT_FOUND, "session_not_found", "that session is no longer active");
    };
    let revoking_current = target.token_hash == current;
    match state.store.delete_user_session(user.id, &target.token_hash).await {
        Ok(true) => {}
        Ok(false) => {
            return fail(StatusCode::NOT_FOUND, "session_not_found", "that session is no longer active")
        }
        Err(e) => return admin_error(&e),
    }
    audit(&state, "session_revoked", Some(&user.email), &auth::client_ip(&headers, None), "one device").await;
    let mut resp = StatusCode::NO_CONTENT.into_response();
    if revoking_current {
        clear_session_cookie(&mut resp, is_secure(&headers));
    }
    resp
}

/// `POST /api/auth/me/sessions/revoke-others` — end every other device.
pub async fn revoke_other_sessions(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let Some(user) = current_user(&state, &headers).await else {
        return fail(StatusCode::UNAUTHORIZED, "sign_in_required", "sign in to continue");
    };
    let Some(token) = session_token(&headers) else {
        return fail(StatusCode::UNAUTHORIZED, "sign_in_required", "sign in to continue");
    };
    match state.store.delete_sessions_except(user.id, &session_key(&token)).await {
        Ok(count) => {
            audit(
                &state,
                "sessions_revoked",
                Some(&user.email),
                &auth::client_ip(&headers, None),
                &format!("{count} other device(s)"),
            )
            .await;
            ok_json(json!({ "revoked": count }))
        }
        Err(e) => admin_error(&e),
    }
}

/// `POST /api/auth/logout` — clears the session both server- and client-side.
pub async fn logout(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if let Some(token) = session_token(&headers) {
        let user = current_user(&state, &headers).await;
        if let Err(e) = state.store.delete_session(&session_key(&token)).await {
            return admin_error(&e);
        }
        audit(
            &state,
            "sign_out",
            user.as_ref().map(|u| u.email.as_str()),
            &auth::client_ip(&headers, None),
            "session ended",
        )
        .await;
    }
    let mut resp = StatusCode::NO_CONTENT.into_response();
    clear_session_cookie(&mut resp, is_secure(&headers));
    resp
}

/// `POST /api/auth/recover` — a recovery code resets the password and ends every other
/// session. The response never says whether the email exists.
pub async fn recover(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let value = match body_json(body).await {
        Ok(v) => v,
        Err(resp) => return *resp,
    };
    let ip = auth::client_ip(&headers, None);
    let invalid = || {
        fail(
            StatusCode::BAD_REQUEST,
            "invalid_recovery",
            "That email and recovery code don't match.",
        )
    };
    let Some(email) = text(&value, "email") else {
        return invalid();
    };
    // Recovery codes are 64-bit and single use, but a script can still guess: the same
    // per-email bucket as sign-in keeps the endpoint honest.
    let email = normalize_email(&email);
    let key = format!("recover:{email}");
    if let Some(wait) = state.auth.throttle(&key) {
        let mut resp = fail(
            StatusCode::TOO_MANY_REQUESTS,
            "too_many_attempts",
            "Too many attempts. Try again in a minute.",
        );
        if let Ok(value) = HeaderValue::from_str(&wait.to_string()) {
            resp.headers_mut()
                .insert(axum::http::header::RETRY_AFTER, value);
        }
        return resp;
    }
    let Some(code) = text(&value, "recovery_code") else {
        return invalid();
    };
    let password = value.get("new_password").and_then(Value::as_str).unwrap_or("");
    if password.chars().count() < MIN_PASSWORD {
        return fail(StatusCode::BAD_REQUEST, "password_too_short", "use at least 12 characters");
    }
    let Ok(user) = state.store.user_by_email(&email).await else {
        state.auth.note_failure(&key);
        return invalid();
    };
    let Some(user) = user.filter(|u| !u.disabled) else {
        return invalid();
    };
    let consumed = match state
        .store
        .consume_recovery_code(&hash_secret(&normalize_code(&code)), crate::now_unix())
        .await
    {
        Ok(consumed) => consumed,
        Err(e) => return admin_error(&e),
    };
    if consumed != Some(user.id) {
        state.auth.note_failure(&key);
        return invalid();
    }
    state.auth.clear_failures(&key);
    let hash = match hash_password(password) {
        Ok(hash) => hash,
        Err(e) => return admin_error(&e),
    };
    if let Err(e) = state.store.set_user_password(user.id, &hash).await {
        return admin_error(&e);
    }
    audit(&state, "password_reset", Some(&user.email), &ip, "recovery code used; sessions ended").await;
    tracing::info!(email = %user.email, "password reset with a recovery code; sessions ended");
    StatusCode::NO_CONTENT.into_response()
}

/// `GET /api/auth/invites/{token}`
pub async fn invite(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(token): axum::extract::Path<String>,
) -> Response {
    let key = hash_secret(&token);
    match state.store.invite_by_token(&key, crate::now_unix()).await {
        Ok(Some(invite)) => {
            let inviter = state
                .store
                .user(invite.created_by)
                .await
                .ok()
                .flatten()
                .map(|u| u.name)
                .unwrap_or_else(|| "An administrator".to_string());
            ok_json(json!({
                "email": invite.email,
                "role": invite.role.as_str(),
                "inviter_name": inviter,
                "expires_at": invite.expires_at,
            }))
        }
        Ok(None) => fail(StatusCode::GONE, "invite_invalid", "this invitation has expired"),
        Err(e) => admin_error(&e),
    }
}

/// `POST /api/auth/signup` — joins through a single-use invitation.
pub async fn signup(State(state): State<Arc<AppState>>, headers: HeaderMap, body: Body) -> Response {
    let value = match body_json(body).await {
        Ok(v) => v,
        Err(resp) => return *resp,
    };
    let Some(token) = text(&value, "token") else {
        return fail(StatusCode::BAD_REQUEST, "invite_required", "an invitation is required");
    };
    let invite = match state
        .store
        .invite_by_token(&hash_secret(&token), crate::now_unix())
        .await
    {
        Ok(Some(invite)) => invite,
        Ok(None) => {
            return fail(StatusCode::GONE, "invite_invalid", "this invitation has expired")
        }
        Err(e) => return admin_error(&e),
    };
    let Some(name) = text(&value, "name") else {
        return fail(StatusCode::BAD_REQUEST, "invalid_name", "enter your full name");
    };
    let password = value.get("password").and_then(Value::as_str).unwrap_or("");
    if password.chars().count() < MIN_PASSWORD {
        return fail(StatusCode::BAD_REQUEST, "password_too_short", "use at least 12 characters");
    }
    let hash = match hash_password(password) {
        Ok(hash) => hash,
        Err(e) => return admin_error(&e),
    };
    let user = match state
        .store
        .create_user(&name, &invite.email, &hash, invite.role, false)
        .await
    {
        Ok(user) => user,
        Err(e) => return admin_error(&e),
    };
    if let Err(e) = state.store.consume_invite(invite.id, crate::now_unix()).await {
        return admin_error(&e);
    }
    audit(
        &state,
        "invitation_accepted",
        Some(&user.email),
        &auth::client_ip(&headers, None),
        &format!("role {}", user.role.as_str()),
    )
    .await;
    tracing::info!(email = %user.email, role = user.role.as_str(), "invitation accepted");
    let codes = match issue_recovery_codes(&state, &user).await {
        Ok(codes) => codes,
        Err(e) => return admin_error(&e),
    };
    let mut resp = match start_session(&state, &headers, &user.id, false).await {
        Ok(resp) => resp,
        Err(e) => return admin_error(&e),
    };
    let payload = json!({ "user": user_json(&user), "recovery_codes": codes });
    *resp.status_mut() = StatusCode::CREATED;
    *resp.body_mut() = axum::body::Body::from(payload.to_string());
    resp.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    resp
}

/// `POST /api/users/invites` — admin only; the link is returned once.
pub async fn create_invite(State(state): State<Arc<AppState>>, body: Body) -> Response {
    let value = match body_json(body).await {
        Ok(v) => v,
        Err(resp) => return *resp,
    };
    let Some(email) = text(&value, "email") else {
        return fail(StatusCode::BAD_REQUEST, "invalid_email", "enter the email to invite");
    };
    if let Some(problem) = email_problem(&email) {
        return fail(StatusCode::BAD_REQUEST, "invalid_email", problem);
    }
    let Some(role) = text(&value, "role").and_then(|raw| UserRole::parse(&raw)) else {
        return fail(StatusCode::BAD_REQUEST, "invalid_role", "role must be admin, member or viewer");
    };
    // The handler is behind the admin gate, which may have admitted a token, a key or
    // a loopback call, so the invitation is attributed to the owner when it can be.
    let creator = match state.store.users().await {
        Ok(users) => users.into_iter().find(|u| u.is_owner),
        Err(e) => return admin_error(&e),
    };
    let Some(creator) = creator else {
        return fail(StatusCode::BAD_REQUEST, "no_user", "no administrator exists");
    };
    let token = random_token();
    let invite = match state
        .store
        .create_invite(
            &email,
            role,
            &hash_secret(&token),
            creator.id,
            crate::now_unix() + INVITE_SECS,
        )
        .await
    {
        Ok(invite) => invite,
        Err(e) => return admin_error(&e),
    };
    audit(
        &state,
        "invitation_created",
        Some(&creator.email),
        &auth::client_ip(&HeaderMap::new(), None),
        &format!("{} as {}", invite.email, invite.role.as_str()),
    )
    .await;
    tracing::info!(email = %invite.email, role = invite.role.as_str(), "invitation created");
    (
        StatusCode::CREATED,
        axum::Json(json!({
            "invite": invite_json(&invite),
            "token": token,
            "path": format!("/invite/{token}"),
        })),
    )
        .into_response()
}

pub fn invite_json(invite: &rrouter_store::Invite) -> Value {
    json!({
        "id": invite.id.to_string(),
        "email": invite.email,
        "role": invite.role.as_str(),
        "created_at": invite.created_at,
        "expires_at": invite.expires_at,
        "used_at": invite.used_at,
    })
}

/// `GET /api/users/invites`
pub async fn list_invites(State(state): State<Arc<AppState>>) -> Response {
    match state.store.pending_invites().await {
        Ok(invites) => ok_json(json!({
            "invites": invites.iter().map(invite_json).collect::<Vec<_>>(),
        })),
        Err(e) => admin_error(&e),
    }
}

/// `DELETE /api/users/invites/{id}`
pub async fn revoke_invite(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Response {
    let Ok(id) = uuid::Uuid::parse_str(&id) else {
        return admin_error(&Error::NotFound("invite".into()));
    };
    match state.store.delete_invite(id).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => admin_error(&Error::NotFound(format!("invite {id}"))),
        Err(e) => admin_error(&e),
    }
}
