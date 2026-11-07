//! OAuth sign-in: obtain the *first* credential set for an account.
//!
//! Three flows, one shape. `start` builds a pending login and returns whatever the
//! human needs (an authorization URL, or a user code plus verification URL);
//! `complete` performs the exchange and returns credentials ready to persist.
//!
//! Why a state machine rather than one blocking call: the human step can take
//! minutes and happens in a browser, and the dashboard must be able to show the
//! code, poll status, and survive the tab being closed. Each pending login is
//! identified by a random id; the PKCE verifier and CSRF `state` never leave the
//! server until the exchange.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rrouter_core::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::credentials::Credentials;
use crate::fresh::TokenResponse;
use crate::loopback::LoopbackServer;
use crate::pkce;
use crate::registry::{
    get, login_flow_for, DeviceFlow, LoginFlow, PkceFlow, ProviderSpec, RefreshBody,
};

/// How long a pending login may sit unused before it is dropped.
pub const PENDING_TTL: Duration = Duration::from_secs(15 * 60);
/// How long `complete` waits for a PKCE callback that has not arrived yet.
const COMPLETE_WAIT: Duration = Duration::from_secs(20);
/// Ceiling on a single device-code poll cycle.
const POLL_TIMEOUT: Duration = Duration::from_secs(30);

/// What the caller must do next.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Challenge {
    /// Open `authorize_url`, then call `complete` with the login id.
    Pkce {
        authorize_url: String,
        redirect_uri: String,
    },
    /// Show `user_code`, open `verification_uri`, then call `complete`.
    DeviceCode {
        user_code: String,
        verification_uri: String,
        /// Seconds until the device code expires.
        expires_in: u64,
        /// Minimum seconds between poll attempts (provider-mandated).
        interval: u64,
    },
}

/// Result of a successful sign-in. Not serialized to clients: the gateway stores
/// these credentials and returns only an id and a label.
#[derive(Debug, Clone, Serialize)]
pub struct SignedIn {
    pub provider: String,
    pub credentials: Credentials,
    /// Human label when the provider reports one (email/login).
    pub account_label: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingState {
    /// Waiting for the human to finish in the browser.
    Waiting,
    /// `complete` already ran; the pending login is consumed.
    Finished,
}

struct Pending {
    provider: &'static str,
    profile_url: Option<&'static str>,
    device: Option<DeviceFlowState>,
    created: Instant,
    status: PendingState,
    outcome: Option<std::result::Result<SignedIn, String>>,
    /// The task that owns the loopback listener and exchanges the code as soon as the
    /// browser redirects. Aborted on cancel so a forgotten dialog cannot hold a port.
    task: Option<tokio::task::JoinHandle<()>>,
}

struct DeviceFlowState {
    flow: DeviceFlow,
    device_code: String,
    interval: Duration,
}

/// Live sign-in sessions, keyed by the id handed to the caller.
#[derive(Clone, Default)]
pub struct OAuthFlows {
    inner: Arc<Mutex<HashMap<String, Pending>>>,
}

impl std::fmt::Debug for OAuthFlows {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let n = self.inner.lock().map(|m| m.len()).unwrap_or(0);
        write!(f, "OAuthFlows({n} pending)")
    }
}

impl OAuthFlows {
    pub fn new() -> Self {
        Self::default()
    }

    /// Begin a sign-in. Returns the pending id and what the human must do.
    pub async fn start(
        &self,
        http: &reqwest::Client,
        spec: &'static ProviderSpec,
    ) -> Result<(String, Challenge)> {
        let flow = if get(spec.id).is_some_and(|registered| std::ptr::eq(registered, spec)) {
            login_flow_for(spec.id)
        } else {
            None
        }
        .or(spec.login)
        .ok_or_else(|| Error::BadRequest(format!("provider {} has no OAuth sign-in", spec.id)))?;
        self.sweep();
        match flow {
            LoginFlow::Pkce(p) => self.start_pkce(http, spec, p).await,
            LoginFlow::DeviceCode(d) => self.start_device(http, spec, d).await,
        }
    }

    async fn start_pkce(
        &self,
        http: &reqwest::Client,
        spec: &'static ProviderSpec,
        p: PkceFlow,
    ) -> Result<(String, Challenge)> {
        let server =
            LoopbackServer::bind(p.fixed_port.unwrap_or(0), p.callback_path, p.redirect_host)
                .await?;
        let redirect_uri = server.redirect_uri();
        let verifier = pkce::generate_verifier();
        let challenge = pkce::challenge_for(&verifier);
        let state = pkce::generate_state();
        let authorize_url = pkce::authorize_url(
            p.authorize_url,
            p.client_id,
            &redirect_uri,
            p.scope,
            &state,
            &challenge,
            p.extra_params,
        );
        let id = uuid::Uuid::new_v4().to_string();
        // The browser redirect *is* the completion signal: accept it now and exchange
        // the code in the background, so the callback page answers immediately and the
        // login finishes without waiting for someone to press a button.
        let task = {
            let flows = self.clone();
            let http = http.clone();
            let waiter_id = id.clone();
            let state = state.clone();
            let redirect_uri = redirect_uri.clone();
            let verifier = verifier.clone();
            let server = server;
            tokio::spawn(async move {
                let outcome = match server.wait(&state, PENDING_TTL).await {
                    Ok(cb) => {
                        Self::exchange_code(
                            &http,
                            ExchangeCode {
                                provider: spec.id,
                                flow: p,
                                profile_url: spec.profile_url,
                                code: &cb.code,
                                redirect_uri: &redirect_uri,
                                verifier: &verifier,
                                state: &state,
                            },
                        )
                        .await
                    }
                    Err(e) => Err(e),
                };
                flows.finish(&waiter_id, outcome.map_err(|e| e.to_string()));
            })
        };
        self.insert(
            id.clone(),
            Pending {
                provider: spec.id,
                profile_url: spec.profile_url,
                device: None,
                created: Instant::now(),
                status: PendingState::Waiting,
                outcome: None,
                task: Some(task),
            },
        );
        Ok((
            id,
            Challenge::Pkce {
                authorize_url,
                redirect_uri,
            },
        ))
    }

    async fn start_device(
        &self,
        http: &reqwest::Client,
        spec: &'static ProviderSpec,
        d: DeviceFlow,
    ) -> Result<(String, Challenge)> {
        let resp = http
            .post(d.device_code_url)
            .header("accept", "application/json")
            .form(&[("client_id", d.client_id), ("scope", d.scope)])
            .send()
            .await
            .map_err(transport)?;
        let status = resp.status();
        let body = resp.text().await.map_err(transport)?;
        if !status.is_success() {
            return Err(Error::Upstream {
                provider: spec.id.to_string(),
                status: status.as_u16(),
                body: truncate(&body),
                payload: serde_json::from_str(&body).ok(),
            });
        }
        let grant: DeviceCodeResponse =
            serde_json::from_str(&body).map_err(|e| Error::Upstream {
                provider: spec.id.to_string(),
                status: status.as_u16(),
                body: format!("unparsable device-code response: {e}"),
                payload: None,
            })?;
        let interval = Duration::from_secs(grant.interval.max(1));
        let id = uuid::Uuid::new_v4().to_string();
        self.insert(
            id.clone(),
            Pending {
                provider: spec.id,
                profile_url: spec.profile_url,
                device: Some(DeviceFlowState {
                    flow: d,
                    device_code: grant.device_code,
                    interval,
                }),
                created: Instant::now(),
                status: PendingState::Waiting,
                outcome: None,
                task: None,
            },
        );
        Ok((
            id,
            Challenge::DeviceCode {
                user_code: grant.user_code,
                verification_uri: grant.verification_uri,
                expires_in: grant.expires_in,
                interval: grant.interval.max(1),
            },
        ))
    }

    /// Whether this login is still waiting.
    pub fn is_pending(&self, id: &str) -> bool {
        let map = self.lock();
        map.get(id)
            .is_some_and(|p| p.status == PendingState::Waiting && !p.expired())
    }

    /// Finish a sign-in. Idempotent: a second call returns the stored outcome
    /// instead of exchanging twice (a code is single-use at the provider).
    ///
    /// PKCE logins are exchanged by the task started in [`start`], so this waits for
    /// its outcome; device logins still poll here because the provider, not a redirect,
    /// decides when the user has approved.
    pub async fn complete(&self, http: &reqwest::Client, id: &str) -> Result<SignedIn> {
        let device = {
            let map = self.lock();
            let Some(p) = map.get(id) else {
                return Err(Error::NotFound(format!("no pending OAuth login {id}")));
            };
            if p.expired() {
                drop(map);
                self.lock().remove(id);
                return Err(Error::Timeout(
                    "this sign-in expired; start a new one".into(),
                ));
            }
            if p.status == PendingState::Finished {
                return replay(&p.outcome);
            }
            p.device
                .as_ref()
                .map(|d| (d.flow, d.device_code.clone(), d.interval))
        };

        if let Some((flow, device_code, interval)) = device {
            let (provider, profile_url) = {
                let map = self.lock();
                let p = map.get(id).ok_or_else(|| Error::NotFound(id.to_string()))?;
                (p.provider, p.profile_url)
            };
            let outcome =
                Self::poll_device(http, provider, flow, profile_url, &device_code, interval).await;
            if let Ok(done) = &outcome {
                self.finish(id, Ok(done.clone()));
            } else if let Err(e) = &outcome {
                self.finish(id, Err(e.to_string()));
            }
            return outcome;
        }

        // PKCE: the callback task owns the exchange. Wait a short while for it — the
        // caller may press "complete" before the browser has redirected — but never
        // hold an HTTP request for the full login lifetime.
        let deadline = Instant::now() + COMPLETE_WAIT;
        loop {
            {
                let map = self.lock();
                let Some(p) = map.get(id) else {
                    return Err(Error::NotFound(format!("no pending OAuth login {id}")));
                };
                if p.status == PendingState::Finished {
                    return replay(&p.outcome);
                }
            }
            if Instant::now() >= deadline {
                return Err(Error::Timeout(
                    "waiting for the provider redirect: finish the sign-in in the browser".into(),
                ));
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
    }

    /// Store the outcome of a background exchange so `complete` can replay it.
    fn finish(&self, id: &str, outcome: std::result::Result<SignedIn, String>) {
        let mut map = self.lock();
        if let Some(p) = map.get_mut(id) {
            p.status = PendingState::Finished;
            p.outcome = Some(outcome);
        }
    }

    pub fn cancel(&self, id: &str) -> bool {
        let removed = self.lock().remove(id);
        match removed {
            Some(p) => {
                if let Some(task) = p.task {
                    task.abort();
                }
                true
            }
            None => false,
        }
    }

    /// Pending logins, for diagnostics. Never exposes verifiers or codes.
    pub fn pending_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self
            .lock()
            .iter()
            .filter(|(_, p)| p.status == PendingState::Waiting && !p.expired())
            .map(|(id, _)| id.clone())
            .collect();
        ids.sort();
        ids
    }

    // ---- internals -----------------------------------------------------------------

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Pending>> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn insert(&self, id: String, pending: Pending) {
        self.lock().insert(id, pending);
    }

    /// Evict expired logins so a forgotten tab cannot accumulate bound ports.
    fn sweep(&self) {
        let mut map = self.lock();
        map.retain(|_, p| !p.expired() || p.status == PendingState::Waiting && p.outcome.is_none());
        map.retain(|_, p| !p.expired());
    }

    async fn exchange_code(http: &reqwest::Client, e: ExchangeCode<'_>) -> Result<SignedIn> {
        let ExchangeCode {
            provider,
            flow: p,
            profile_url,
            code,
            redirect_uri,
            verifier,
            state,
        } = e;
        let params = authorization_params(&p, code, redirect_uri, verifier, state);
        let req = match p.body {
            RefreshBody::Json => {
                let object: serde_json::Map<String, serde_json::Value> = params
                    .iter()
                    .map(|(k, v)| ((*k).to_string(), serde_json::Value::String(v.clone())))
                    .collect();
                http.post(p.token_url)
                    .json(&serde_json::Value::Object(object))
            }
            RefreshBody::Form => http.post(p.token_url).form(&params),
        };
        let resp = req
            .header("accept", "application/json")
            .send()
            .await
            .map_err(transport)?;
        let status = resp.status();
        let body = resp.text().await.map_err(transport)?;
        if !status.is_success() {
            return Err(Error::Unauthorized(format!(
                "provider {provider} rejected the authorization code (HTTP {status}): {}",
                truncate(&body)
            )));
        }
        let tokens: TokenResponse = serde_json::from_str(&body).map_err(|e| Error::Upstream {
            provider: provider.to_string(),
            status: status.as_u16(),
            body: format!("unparsable token response: {e}"),
            payload: None,
        })?;
        if tokens.access_token.is_empty() {
            return Err(Error::Unauthorized(format!(
                "provider {provider} returned an empty access token"
            )));
        }
        let account_label = match account_label_from(&tokens) {
            Some(label) => Some(label),
            None => profile_label(http, provider, profile_url, &tokens.access_token).await,
        };
        Ok(SignedIn {
            provider: provider.to_string(),
            credentials: Credentials {
                access_token: Some(tokens.access_token),
                refresh_token: tokens.refresh_token.filter(|t| !t.is_empty()),
                id_token: tokens.id_token.filter(|t| !t.is_empty()),
                expires_at: tokens.expires_in.map(|s| now_unix() + i64::from(s)),
                account_label: account_label.clone(),
                ..Credentials::default()
            },
            account_label,
        })
    }

    async fn poll_device(
        http: &reqwest::Client,
        provider: &str,
        d: DeviceFlow,
        profile_url: Option<&'static str>,
        device_code: &str,
        interval: Duration,
    ) -> Result<SignedIn> {
        let deadline = Instant::now() + PENDING_TTL;
        let mut wait = interval;
        loop {
            if Instant::now() >= deadline {
                return Err(Error::Timeout("device authorization expired".into()));
            }
            tokio::time::sleep(wait).await;
            let resp = http
                .post(d.token_url)
                .header("accept", "application/json")
                .form(&[
                    ("client_id", d.client_id),
                    ("device_code", device_code),
                    ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ])
                .timeout(POLL_TIMEOUT)
                .send()
                .await
                .map_err(transport)?;
            let status = resp.status();
            let body = resp.text().await.map_err(transport)?;
            let value: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
            if let Some(token) = value.get("access_token").and_then(Value::as_str) {
                let tokens = TokenResponse {
                    access_token: token.to_string(),
                    refresh_token: value
                        .get("refresh_token")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    id_token: value
                        .get("id_token")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    expires_in: value
                        .get("expires_in")
                        .and_then(Value::as_u64)
                        .map(|s| s.min(u64::from(u32::MAX)) as u32),
                };
                let account_label = match account_label_from(&tokens) {
                    Some(label) => Some(label),
                    None => profile_label(http, provider, profile_url, &tokens.access_token).await,
                };
                return Ok(SignedIn {
                    provider: provider.to_string(),
                    credentials: Credentials {
                        access_token: Some(tokens.access_token),
                        refresh_token: tokens.refresh_token,
                        id_token: tokens.id_token,
                        expires_at: tokens.expires_in.map(|s| now_unix() + i64::from(s)),
                        account_label: account_label.clone(),
                        ..Credentials::default()
                    },
                    account_label,
                });
            }
            match value.get("error").and_then(Value::as_str) {
                // Transient: keep polling at the provider's cadence.
                Some("authorization_pending") => {}
                // The provider asks us to back off; RFC 8628 §3.5 says +5 s.
                Some("slow_down") => wait += Duration::from_secs(5),
                Some("expired_token") => {
                    return Err(Error::Timeout(
                        "the device code expired; start again".into(),
                    ))
                }
                Some("access_denied") => {
                    return Err(Error::Unauthorized("the sign-in was denied".into()))
                }
                Some(other) => {
                    return Err(Error::Upstream {
                        provider: provider.to_string(),
                        status: status.as_u16(),
                        body: format!("device token error: {other}"),
                        payload: Some(value),
                    })
                }
                None => {
                    return Err(Error::Upstream {
                        provider: provider.to_string(),
                        status: status.as_u16(),
                        body: truncate(&body),
                        payload: Some(value),
                    })
                }
            }
        }
    }
}

/// Everything one authorization-code exchange needs, grouped so the call site stays
/// readable and the function does not grow an argument per provider quirk.
struct ExchangeCode<'a> {
    provider: &'a str,
    flow: PkceFlow,
    profile_url: Option<&'static str>,
    code: &'a str,
    redirect_uri: &'a str,
    verifier: &'a str,
    state: &'a str,
}

/// Token-exchange fields for an authorization-code grant.
///
/// Anthropic rejects the body without `state` (it answers "Invalid request format"),
/// while OpenAI's endpoint does not expect it, so the echo is per provider.
fn authorization_params(
    p: &PkceFlow,
    code: &str,
    redirect_uri: &str,
    verifier: &str,
    state: &str,
) -> Vec<(&'static str, String)> {
    let mut params = vec![
        ("grant_type", "authorization_code".to_string()),
        ("code", code.to_string()),
        ("client_id", p.client_id.to_string()),
        ("redirect_uri", redirect_uri.to_string()),
        ("code_verifier", verifier.to_string()),
    ];
    if p.send_state {
        params.push(("state", state.to_string()));
    }
    params
}

/// Replay a stored outcome, keeping failure messages verbatim.
fn replay(outcome: &Option<std::result::Result<SignedIn, String>>) -> Result<SignedIn> {
    match outcome {
        Some(Ok(done)) => Ok(done.clone()),
        Some(Err(msg)) => Err(Error::BadRequest(msg.clone())),
        None => Err(Error::BadRequest("login already finished".into())),
    }
}

impl Pending {
    fn expired(&self) -> bool {
        self.created.elapsed() > PENDING_TTL
    }
}

#[derive(Debug, Deserialize)]
struct DeviceCodeResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    #[serde(default = "default_expiry")]
    expires_in: u64,
    #[serde(default = "default_interval")]
    interval: u64,
}

fn default_expiry() -> u64 {
    900
}

fn default_interval() -> u64 {
    5
}

/// Best-effort human label from the id token, then the token response's own fields.
fn account_label_from(tokens: &TokenResponse) -> Option<String> {
    if let Some(id_token) = &tokens.id_token {
        if let Some(label) = label_from_jwt(id_token) {
            return Some(label);
        }
    }
    None
}

/// Read `email` (or `preferred_username`) from an unverified JWT payload.
///
/// This is a *label* shown in the dashboard, never an authorization decision: the
/// token's signature is not checked here because the only consumer is display text.
fn label_from_jwt(jwt: &str) -> Option<String> {
    let payload = jwt.split('.').nth(1)?;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine as _;
    let raw = URL_SAFE_NO_PAD.decode(payload).ok()?;
    let value: Value = serde_json::from_slice(&raw).ok()?;
    for key in ["email", "preferred_username", "login"] {
        if let Some(s) = value.get(key).and_then(Value::as_str) {
            if !s.is_empty() {
                return Some(s.to_string());
            }
        }
    }
    None
}

async fn profile_label(
    http: &reqwest::Client,
    provider: &str,
    profile_url: Option<&'static str>,
    access_token: &str,
) -> Option<String> {
    let url = profile_url?;
    let response = http
        .get(url)
        .bearer_auth(access_token)
        .timeout(POLL_TIMEOUT)
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let value: Value = response.json().await.ok()?;
    let candidate = match provider {
        "claude" => value
            .pointer("/account/email_address")
            .or_else(|| value.get("email")),
        "codex" | "github" => value.get(if provider == "codex" {
            "email"
        } else {
            "login"
        }),
        _ => None,
    };
    candidate
        .and_then(Value::as_str)
        .filter(|label| !label.is_empty())
        .map(str::to_string)
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn transport(e: reqwest::Error) -> Error {
    if e.is_timeout() {
        Error::Timeout(format!("oauth request: {e}"))
    } else {
        Error::Transport(format!("oauth request: {e}"))
    }
}

fn truncate(s: &str) -> String {
    match s.char_indices().nth(512) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{DeviceFlow, LoginFlow, PkceFlow as P, ProviderKind, RefreshBody};
    use axum::extract::State;
    use axum::routing::post;
    use axum::{Json, Router};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn spec(login: LoginFlow) -> &'static ProviderSpec {
        spec_with_profile_id("mock", login, None)
    }

    fn spec_with_profile_id(
        id: &'static str,
        login: LoginFlow,
        profile_url: Option<&'static str>,
    ) -> &'static ProviderSpec {
        // Leak once per test process: `start` takes &'static ProviderSpec.
        Box::leak(Box::new(ProviderSpec {
            id,
            display: "Mock",
            kind: ProviderKind::OAuth,
            base_url: "https://mock.test/chat",
            responses_url: None,
            messages_url: None,
            images_url: None,
            audio_speech_url: None,
            audio_transcriptions_url: None,
            audio_voices_url: None,
            default_dialect: rrouter_core::Dialect::OpenAiChat,
            auth: crate::registry::AuthStyle::Bearer,
            extra_headers: &[],
            default_max_tokens: None,
            refresh: None,
            profile_url,
            login: Some(login),
            stream_only: false,
        }))
    }

    /// Anthropic rejects the token exchange without `state` ("Invalid request format");
    /// OpenAI's endpoint does not expect it. Verified against the live endpoints.
    #[test]
    fn authorization_code_exchange_echoes_state_only_where_required() {
        let LoginFlow::Pkce(claude) = crate::registry::CLAUDE_LOGIN else {
            panic!("claude is a PKCE flow");
        };
        let params =
            authorization_params(&claude, "code", "http://localhost:1/callback", "v", "st");
        assert!(
            params.iter().any(|(k, v)| *k == "state" && v == "st"),
            "claude must echo state: {params:?}"
        );

        let LoginFlow::Pkce(codex) = crate::registry::CODEX_LOGIN else {
            panic!("codex is a PKCE flow");
        };
        let params = authorization_params(
            &codex,
            "code",
            "http://127.0.0.1:1455/auth/callback",
            "v",
            "st",
        );
        assert!(
            !params.iter().any(|(k, _)| *k == "state"),
            "codex must not send state: {params:?}"
        );
    }

    /// Anthropic's client registration accepts `http://localhost:<port>/callback` only;
    /// 127.0.0.1 fails the sign-in with "Redirect URI ... is not supported by client".
    #[tokio::test]
    async fn claude_redirect_uri_uses_localhost() {
        let flows = OAuthFlows::new();
        let http = reqwest::Client::new();
        let spec = crate::registry::get("claude").expect("claude provider");
        let (_, challenge) = flows.start(&http, spec).await.expect("start");
        let Challenge::Pkce { authorize_url, .. } = challenge else {
            panic!("claude must be a PKCE flow");
        };
        assert!(
            authorize_url.contains("redirect_uri=http%3A%2F%2Flocalhost%3A"),
            "claude must redirect to localhost: {authorize_url}"
        );
        assert!(
            !authorize_url.contains("127.0.0.1"),
            "127.0.0.1 is rejected by Anthropic's client registration: {authorize_url}"
        );
    }

    /// Codex keeps its registered host and port.
    #[tokio::test]
    async fn codex_redirect_uri_keeps_its_registered_port() {
        let flows = OAuthFlows::new();
        let http = reqwest::Client::new();
        let spec = crate::registry::get("codex").expect("codex provider");
        let (_, challenge) = flows.start(&http, spec).await.expect("start");
        let Challenge::Pkce { authorize_url, .. } = challenge else {
            panic!("codex must be a PKCE flow");
        };
        assert!(
            authorize_url.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A1455%2Fauth%2Fcallback"),
            "codex must keep its registered redirect: {authorize_url}"
        );
    }

    async fn oauth_server() -> (String, Arc<AtomicUsize>) {
        let hits = Arc::new(AtomicUsize::new(0));
        let state = (hits.clone(),);
        let app = Router::new()
            .route("/device", post(device_code))
            .route("/token", post(token))
            .with_state(state);
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}"), hits)
    }

    async fn device_code() -> Json<Value> {
        Json(serde_json::json!({
            "device_code": "DEV-1",
            "user_code": "ABCD-1234",
            "verification_uri": "https://mock.test/device",
            "expires_in": 900,
            "interval": 1
        }))
    }

    async fn token(State((hits,)): State<(Arc<AtomicUsize>,)>, body: String) -> Json<Value> {
        let n = hits.fetch_add(1, Ordering::SeqCst);
        if body.contains("device_code=DEV-1") {
            // First poll is pending, second succeeds: exercises the poll loop.
            if n == 0 {
                return Json(serde_json::json!({"error": "authorization_pending"}));
            }
            return Json(serde_json::json!({
                "access_token": "device-token",
                "refresh_token": "device-refresh",
                "expires_in": 3600
            }));
        }
        Json(serde_json::json!({
            "access_token": "pkce-token",
            "refresh_token": "pkce-refresh",
            "expires_in": 3600,
            "id_token": fake_jwt("user@example.test")
        }))
    }

    fn fake_jwt(email: &str) -> String {
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use base64::Engine as _;
        let payload = serde_json::json!({"email": email}).to_string();
        format!("h.{}.s", URL_SAFE_NO_PAD.encode(payload))
    }

    #[tokio::test]
    async fn device_flow_polls_until_success_and_persists_tokens() {
        let (base, hits) = oauth_server().await;
        let flows = OAuthFlows::new();
        let http = reqwest::Client::new();
        let leaked = spec(LoginFlow::DeviceCode(DeviceFlow {
            client_id: "cid",
            device_code_url: Box::leak(format!("{base}/device").into_boxed_str()),
            token_url: Box::leak(format!("{base}/token").into_boxed_str()),
            scope: "read:user",
        }));

        let (id, challenge) = flows.start(&http, leaked).await.unwrap();
        match challenge {
            Challenge::DeviceCode {
                user_code,
                interval,
                ..
            } => {
                assert_eq!(user_code, "ABCD-1234");
                assert_eq!(interval, 1);
            }
            other => panic!("expected device code, got {other:?}"),
        }
        assert!(flows.is_pending(&id));

        let done = flows.complete(&http, &id).await.unwrap();
        assert_eq!(
            done.credentials.access_token.as_deref(),
            Some("device-token")
        );
        assert_eq!(
            done.credentials.refresh_token.as_deref(),
            Some("device-refresh")
        );
        assert!(done.credentials.expires_at.is_some());
        assert_eq!(
            hits.load(Ordering::SeqCst),
            2,
            "one pending poll, then success"
        );

        // Idempotent: a second complete replays the outcome, no extra exchange.
        let again = flows.complete(&http, &id).await.unwrap();
        assert_eq!(
            again.credentials.access_token.as_deref(),
            Some("device-token")
        );
        assert_eq!(hits.load(Ordering::SeqCst), 2, "no second exchange");
    }

    #[tokio::test]
    async fn profile_endpoint_provides_account_label() {
        let (base, _hits) = oauth_server().await;
        let profile = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"account": {"email_address": "profile@example.test"}}),
            ))
            .expect(1)
            .mount(&profile)
            .await;
        let profile_url = Box::leak(format!("{}/profile", profile.uri()).into_boxed_str());
        let flows = OAuthFlows::new();
        let http = reqwest::Client::new();
        let leaked = spec_with_profile_id(
            "claude",
            LoginFlow::DeviceCode(DeviceFlow {
                client_id: "cid",
                device_code_url: Box::leak(format!("{base}/device").into_boxed_str()),
                token_url: Box::leak(format!("{base}/token").into_boxed_str()),
                scope: "read:user",
            }),
            Some(profile_url),
        );

        let (id, _) = flows.start(&http, leaked).await.unwrap();
        let done = flows.complete(&http, &id).await.unwrap();
        assert_eq!(done.account_label.as_deref(), Some("profile@example.test"));
        assert_eq!(
            done.credentials.account_label.as_deref(),
            Some("profile@example.test")
        );
    }

    #[tokio::test]
    async fn profile_endpoint_failure_does_not_fail_sign_in() {
        let (base, _hits) = oauth_server().await;
        let profile = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(wiremock::ResponseTemplate::new(404))
            .expect(1)
            .mount(&profile)
            .await;
        let profile_url = Box::leak(format!("{}/profile", profile.uri()).into_boxed_str());
        let flows = OAuthFlows::new();
        let http = reqwest::Client::new();
        let leaked = spec_with_profile_id(
            "claude",
            LoginFlow::DeviceCode(DeviceFlow {
                client_id: "cid",
                device_code_url: Box::leak(format!("{base}/device").into_boxed_str()),
                token_url: Box::leak(format!("{base}/token").into_boxed_str()),
                scope: "read:user",
            }),
            Some(profile_url),
        );

        let (id, _) = flows.start(&http, leaked).await.unwrap();
        let done = flows.complete(&http, &id).await.unwrap();
        assert_eq!(done.account_label, None);
        assert_eq!(done.credentials.account_label, None);
    }

    #[tokio::test]
    async fn pkce_flow_completes_after_loopback_redirect() {
        let (base, _hits) = oauth_server().await;
        let flows = OAuthFlows::new();
        let http = reqwest::Client::new();
        let profile = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(wiremock::ResponseTemplate::new(200))
            .expect(0)
            .mount(&profile)
            .await;
        let profile_url = Box::leak(format!("{}/profile", profile.uri()).into_boxed_str());
        let leaked = spec_with_profile_id(
            "claude",
            LoginFlow::Pkce(P {
                client_id: "cid",
                authorize_url: Box::leak(format!("{base}/authorize").into_boxed_str()),
                token_url: Box::leak(format!("{base}/token").into_boxed_str()),
                scope: "openid profile",
                fixed_port: None,
                redirect_host: "127.0.0.1",
                callback_path: "/callback",
                extra_params: &[],
                send_state: false,
                body: RefreshBody::Form,
            }),
            Some(profile_url),
        );

        let (id, challenge) = flows.start(&http, leaked).await.unwrap();
        let (authorize_url, redirect_uri) = match challenge {
            Challenge::Pkce {
                authorize_url,
                redirect_uri,
            } => (authorize_url, redirect_uri),
            other => panic!("expected pkce, got {other:?}"),
        };
        assert!(authorize_url.starts_with(&format!("{base}/authorize?")));
        assert!(authorize_url.contains("code_challenge_method=S256"));
        let state = authorize_url
            .split('&')
            .find_map(|p| p.strip_prefix("state="))
            .expect("state present")
            .to_string();

        // Play the browser: hit the loopback redirect with a matching state.
        let complete = tokio::spawn({
            let flows = flows.clone();
            let http = http.clone();
            async move { flows.complete(&http, &id).await }
        });
        // Give `complete` a moment to start waiting on the socket.
        tokio::time::sleep(Duration::from_millis(60)).await;
        let port: u16 = redirect_uri
            .rsplit(':')
            .next()
            .unwrap()
            .split('/')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        s.write_all(
            format!("GET /callback?code=AUTHCODE&state={state} HTTP/1.1\r\nHost: x\r\n\r\n")
                .as_bytes(),
        )
        .await
        .unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).await.unwrap();
        assert!(out.contains("200 OK"), "{out}");

        let done = complete.await.unwrap().unwrap();
        assert_eq!(done.credentials.access_token.as_deref(), Some("pkce-token"));
        assert_eq!(
            done.credentials.refresh_token.as_deref(),
            Some("pkce-refresh")
        );
        // The id-token email becomes the dashboard label.
        assert_eq!(done.account_label.as_deref(), Some("user@example.test"));
        assert_eq!(
            done.credentials.account_label.as_deref(),
            Some("user@example.test"),
            "label is also stored on the credential for display"
        );
    }

    #[tokio::test]
    async fn unknown_login_id_is_not_found() {
        let flows = OAuthFlows::new();
        let http = reqwest::Client::new();
        let err = flows.complete(&http, "nope").await.unwrap_err();
        assert!(matches!(err, Error::NotFound(_)), "{err}");
    }

    #[tokio::test]
    async fn cancel_releases_the_listener() {
        let flows = OAuthFlows::new();
        let http = reqwest::Client::new();
        let leaked = spec(LoginFlow::Pkce(P {
            client_id: "cid",
            authorize_url: "https://mock.test/authorize",
            token_url: "https://mock.test/token",
            scope: "s",
            fixed_port: None,
            redirect_host: "127.0.0.1",
            callback_path: "/callback",
            extra_params: &[],
            send_state: false,
            body: RefreshBody::Form,
        }));
        let (id, _) = flows.start(&http, leaked).await.unwrap();
        assert_eq!(flows.pending_ids(), vec![id.clone()]);
        assert!(flows.cancel(&id));
        assert!(flows.pending_ids().is_empty());
    }

    #[test]
    fn jwt_label_extraction_is_defensive() {
        assert_eq!(label_from_jwt("garbage"), None);
        assert_eq!(label_from_jwt("a.b.c"), None);
        assert_eq!(label_from_jwt(&fake_jwt("x@y.z")).as_deref(), Some("x@y.z"));
    }
}
