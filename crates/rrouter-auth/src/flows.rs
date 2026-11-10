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
use tokio::sync::Mutex as AsyncMutex;

use crate::antigravity;
use crate::credentials::Credentials;
use crate::fresh::TokenResponse;
use crate::loopback::{self, LoopbackServer, PageContext};
use crate::overrides::oauth_client;
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
        /// `true` when no listener was bound: the browser cannot reach the gateway, so
        /// the operator pastes the final redirect URL into `complete.callback_url`.
        manual: bool,
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
    /// Provider transport base, needed by a manual/remote completion.
    base_url: &'static str,
    device: Option<DeviceFlowState>,
    /// Present for PKCE sign-ins: everything the exchange needs after the redirect.
    pkce: Option<PkceState>,
    /// Serializes exchanges for one pending login, so a loopback redirect racing a
    /// pasted callback cannot spend the single-use code twice.
    exchange: Option<Arc<AsyncMutex<()>>>,
    created: Instant,
    status: PendingState,
    outcome: Option<std::result::Result<SignedIn, String>>,
    /// The task that owns the loopback listener and exchanges the code as soon as the
    /// browser redirects. Aborted on cancel so a forgotten dialog cannot hold a port.
    task: Option<tokio::task::JoinHandle<()>>,
}

#[derive(Clone)]
struct PkceState {
    flow: PkceFlow,
    verifier: String,
    state: String,
    redirect_uri: String,
    /// Manual sign-ins have no listener; `complete` must carry `callback_url`.
    manual: bool,
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
        let flow = self.resolve_flow(spec)?;
        self.sweep();
        match flow {
            LoginFlow::Pkce(p) => self.start_pkce(http, spec, p, false).await,
            LoginFlow::DeviceCode(d) => self.start_device(http, spec, d).await,
        }
    }

    /// Begin a PKCE sign-in that never binds a loopback listener.
    ///
    /// For a gateway on a remote host: the browser redirects to the browser machine's
    /// localhost, which the gateway cannot reach. The caller gives the human
    /// `authorize_url`, then posts the final redirect URL to
    /// [`complete_manual`](Self::complete_manual).
    pub async fn start_manual(
        &self,
        http: &reqwest::Client,
        spec: &'static ProviderSpec,
    ) -> Result<(String, Challenge)> {
        let flow = self.resolve_flow(spec)?;
        self.sweep();
        match flow {
            LoginFlow::Pkce(p) => self.start_pkce(http, spec, p, true).await,
            LoginFlow::DeviceCode(_) => Err(Error::BadRequest(format!(
                "provider {} uses a device code; manual callback is only for PKCE sign-ins",
                spec.id
            ))),
        }
    }

    fn resolve_flow(&self, spec: &'static ProviderSpec) -> Result<LoginFlow> {
        if get(spec.id).is_some_and(|registered| std::ptr::eq(registered, spec)) {
            login_flow_for(spec.id)
        } else {
            None
        }
        .or(spec.login)
        .ok_or_else(|| Error::BadRequest(format!("provider {} has no OAuth sign-in", spec.id)))
    }

    async fn start_pkce(
        &self,
        http: &reqwest::Client,
        spec: &'static ProviderSpec,
        p: PkceFlow,
        manual: bool,
    ) -> Result<(String, Challenge)> {
        // Resolve first: a provider that requires operator-supplied credentials
        // (google-antigravity) must fail with a clear 400 before binding a port or
        // building an authorize URL from a placeholder.
        let (client_id, _client_secret) = oauth_client(spec.id, p.client_id, p.client_secret)?;
        // Manual mode must not touch the registered port: the whole point is that this
        // host cannot receive the browser redirect.
        let (redirect_uri, server) = if manual {
            (fixed_redirect_uri(spec.id, &p)?, None)
        } else {
            let server =
                LoopbackServer::bind(p.fixed_port.unwrap_or(0), p.callback_path, p.redirect_host)
                    .await?;
            (server.redirect_uri(), Some(server))
        };
        let verifier = pkce::generate_verifier();
        let challenge = pkce::challenge_for(&verifier);
        let state = pkce::generate_state();
        let authorize_url = pkce::authorize_url(
            p.authorize_url,
            &client_id,
            &redirect_uri,
            p.scope,
            &state,
            &challenge,
            p.extra_params,
        );
        let id = uuid::Uuid::new_v4().to_string();
        let exchange = Arc::new(AsyncMutex::new(()));

        let task = server.map(|server| {
            // The browser redirect *is* the completion signal: accept it now and exchange
            // the code in the background, so the callback page answers immediately.
            let flows = self.clone();
            let http = http.clone();
            let waiter_id = id.clone();
            let expected_state = state.clone();
            let context = PageContext::for_provider(spec.display);
            tokio::spawn(async move {
                match server
                    .wait_with(&expected_state, PENDING_TTL, &context)
                    .await
                {
                    Ok(callback) => {
                        // The shared exchange lock makes a pasted callback and this
                        // redirect mutually exclusive; the loser replays the outcome.
                        let _ = flows.exchange_pkce(&http, &waiter_id, &callback.code).await;
                    }
                    Err(e) => flows.finish(&waiter_id, Err(e.to_string())),
                }
            })
        });
        self.insert(
            id.clone(),
            Pending {
                provider: spec.id,
                profile_url: spec.profile_url,
                base_url: spec.base_url,
                device: None,
                pkce: Some(PkceState {
                    flow: p,
                    verifier,
                    state,
                    redirect_uri: redirect_uri.clone(),
                    manual,
                }),
                exchange: Some(exchange),
                created: Instant::now(),
                status: PendingState::Waiting,
                outcome: None,
                task,
            },
        );
        Ok((
            id,
            Challenge::Pkce {
                authorize_url,
                redirect_uri,
                manual,
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
                base_url: spec.base_url,
                device: Some(DeviceFlowState {
                    flow: d,
                    device_code: grant.device_code,
                    interval,
                }),
                pkce: None,
                exchange: None,
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

    /// The provider a pending login was started for. `None` when the id is unknown.
    ///
    /// The id is a bearer capability handed to the dashboard, but the route also names a
    /// provider. Callers must compare this value before completing or cancelling so an id
    /// started for one provider cannot be redeemed as another.
    pub fn pending_provider(&self, id: &str) -> Option<&'static str> {
        self.lock().get(id).map(|p| p.provider)
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
        let (device, manual) = {
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
            (
                p.device
                    .as_ref()
                    .map(|d| (d.flow, d.device_code.clone(), d.interval)),
                p.pkce.as_ref().is_some_and(|k| k.manual),
            )
        };

        if manual {
            // No listener exists to wait on; the caller must send the pasted URL.
            return Err(Error::BadRequest(format!(
                "manual sign-in {id} has no loopback listener; call complete with callback_url"
            )));
        }

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

    /// Finish a PKCE sign-in from the final redirect URL the browser reached.
    ///
    /// Works for a manual login (no listener) and also completes a loopback login whose
    /// listener could not see the redirect: any waiting listener is cancelled first.
    /// The URL's `state` must match this pending login, and a code is exchanged at most
    /// once because every path shares the pending login's exchange lock.
    pub async fn complete_manual(
        &self,
        http: &reqwest::Client,
        id: &str,
        callback_url: &str,
    ) -> Result<SignedIn> {
        let expected_state = {
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
            let Some(pkce) = p.pkce.as_ref() else {
                return Err(Error::BadRequest(format!(
                    "login {id} is not an authorization-code sign-in; complete it without callback_url"
                )));
            };
            pkce.state.clone()
        };

        // Cancel a loopback waiter before exchanging: a second consumer of the
        // single-use code must never run in parallel with this call.
        if let Some(task) = self.take_task(id) {
            task.abort();
        }

        let callback = match loopback::parse_callback_url(callback_url, &expected_state) {
            Ok(callback) => callback,
            Err(error) => {
                // Record the failure so a retry replays it instead of re-parsing a URL
                // whose code may already be unusable.
                self.finish(id, Err(error.to_string()));
                return Err(error);
            }
        };
        self.exchange_pkce(http, id, &callback.code).await
    }

    /// Exchange one PKCE code for `id` exactly once, whoever supplied it.
    async fn exchange_pkce(
        &self,
        http: &reqwest::Client,
        id: &str,
        code: &str,
    ) -> Result<SignedIn> {
        let (provider, profile_url, base_url, lock, pkce) = {
            let map = self.lock();
            let Some(p) = map.get(id) else {
                return Err(Error::NotFound(format!("no pending OAuth login {id}")));
            };
            if p.status == PendingState::Finished {
                return replay(&p.outcome);
            }
            let Some(pkce) = p.pkce.clone() else {
                return Err(Error::BadRequest(format!(
                    "login {id} is not an authorization-code sign-in"
                )));
            };
            let Some(lock) = p.exchange.clone() else {
                return Err(Error::BadRequest(format!(
                    "login {id} has no exchange lock"
                )));
            };
            (p.provider, p.profile_url, p.base_url, lock, pkce)
        };

        let _guard = lock.lock().await;
        {
            // The peer we queued behind may have exchanged and stored the outcome, or
            // the login may have been cancelled.
            let map = self.lock();
            let Some(p) = map.get(id) else {
                return Err(Error::NotFound(format!("no pending OAuth login {id}")));
            };
            if p.status == PendingState::Finished {
                return replay(&p.outcome);
            }
        }

        let outcome = Self::exchange_code(
            http,
            ExchangeCode {
                provider,
                flow: pkce.flow,
                profile_url,
                base_url,
                code,
                redirect_uri: &pkce.redirect_uri,
                verifier: &pkce.verifier,
                state: &pkce.state,
            },
        )
        .await;
        self.finish(
            id,
            outcome
                .as_ref()
                .map(|signed_in| signed_in.clone())
                .map_err(|e| e.to_string()),
        );
        outcome
    }

    /// Remove and return the login's listener task, if one is still attached.
    fn take_task(&self, id: &str) -> Option<tokio::task::JoinHandle<()>> {
        self.lock().get_mut(id)?.task.take()
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
            base_url,
            code,
            redirect_uri,
            verifier,
            state,
        } = e;
        let params = authorization_params(&p, provider, code, redirect_uri, verifier, state)?;
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
        let mut credentials = Credentials {
            access_token: Some(tokens.access_token),
            refresh_token: tokens.refresh_token.filter(|t| !t.is_empty()),
            id_token: tokens.id_token.filter(|t| !t.is_empty()),
            expires_at: tokens.expires_in.map(|s| now_unix() + i64::from(s)),
            account_label: account_label.clone(),
            ..Credentials::default()
        };

        // A Google Antigravity account is not usable until its Cloud project is known:
        // every CCA envelope carries it. Resolve it here, while the access token is
        // fresh, and persist it as provider-specific data.
        if provider == antigravity::PROVIDER_ID {
            let project = antigravity::discover_project(
                http,
                credentials.access_token.as_deref().unwrap_or_default(),
                &antigravity::prod_base(),
                &antigravity::daily_base(base_url),
            )
            .await?;
            attach_project(&mut credentials, project);
        }

        Ok(SignedIn {
            provider: provider.to_string(),
            credentials,
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
    /// Provider transport base. Antigravity uses it as the daily CCA origin when no
    /// `RROUTER_ANTIGRAVITY_DAILY_HOST` override is set.
    base_url: &'a str,
    code: &'a str,
    redirect_uri: &'a str,
    verifier: &'a str,
    state: &'a str,
}

/// Redirect URI a manual sign-in must quote to the provider.
///
/// Without a registered fixed port the provider accepts a per-process ephemeral port,
/// which no other machine can reach; manual mode has no way to receive that redirect, so
/// it fails before creating a pending login.
fn fixed_redirect_uri(provider: &str, p: &PkceFlow) -> Result<String> {
    let port = p.fixed_port.ok_or_else(|| {
        Error::BadRequest(format!(
            "manual callback requires a fixed redirect port; {provider} registers an \
             ephemeral loopback port"
        ))
    })?;
    let path = if p.callback_path.starts_with('/') {
        p.callback_path.to_string()
    } else {
        format!("/{}", p.callback_path)
    };
    Ok(format!("http://{}:{}{}", p.redirect_host, port, path))
}

/// Token-exchange fields for an authorization-code grant.
///
/// Anthropic rejects the body without `state` (it answers "Invalid request format"),
/// while OpenAI's endpoint does not expect it, so the echo is per provider. Google's
/// registered client is confidential, so its exchange also carries `client_secret`.
fn authorization_params(
    p: &PkceFlow,
    provider: &str,
    code: &str,
    redirect_uri: &str,
    verifier: &str,
    state: &str,
) -> Result<Vec<(&'static str, String)>> {
    // Google Antigravity has no static client: its id/secret come from the environment,
    // and a missing one is refused here rather than sending a placeholder upstream.
    let (client_id, client_secret) = oauth_client(provider, p.client_id, p.client_secret)?;
    let mut params = vec![
        ("grant_type", "authorization_code".to_string()),
        ("code", code.to_string()),
        ("client_id", client_id.into_owned()),
        ("redirect_uri", redirect_uri.to_string()),
        ("code_verifier", verifier.to_string()),
    ];
    if let Some(secret) = client_secret {
        params.push(("client_secret", secret.into_owned()));
    }
    if p.send_state {
        params.push(("state", state.to_string()));
    }
    Ok(params)
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

/// Persist the discovered Cloud project where the CCA transport reads it.
///
/// The key is a cross-crate contract: `rrouter-gateway` refuses a CCA request without
/// `credentials.extra.project_id`.
fn attach_project(credentials: &mut Credentials, project: String) {
    credentials.extra.insert(
        antigravity::PROJECT_ID_KEY.to_string(),
        Value::String(project),
    );
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
    profile_value_label(provider, &value)
}

/// Label member for one provider's profile payload.
///
/// Pure so every shape is testable without a mock server; `None` means the provider
/// answered with something this build does not recognize, never an error.
fn profile_value_label(provider: &str, value: &Value) -> Option<String> {
    let candidate = match provider {
        "claude" => value
            .pointer("/account/email_address")
            .or_else(|| value.get("email")),
        "codex" | "github" => value.get(if provider == "codex" {
            "email"
        } else {
            "login"
        }),
        // Google userinfo: the email is the account label the dashboard shows.
        "google-antigravity" => value.get("email"),
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
            base_url_requires_override: false,
            responses_url: None,
            models_url: None,
            messages_url: None,
            images_url: None,
            audio_speech_url: None,
            audio_transcriptions_url: None,
            audio_voices_url: None,
            default_dialect: rrouter_core::Dialect::OpenAiChat,
            auth: crate::registry::AuthStyle::Bearer,
            extra_headers: &[],
            auth_header: "",
            query: &[],
            max_tokens_key: rrouter_translate::MaxTokensKey::MaxTokens,
            passthrough_headers: &[],
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
        let params = authorization_params(
            &claude,
            "claude",
            "code",
            "http://localhost:1/callback",
            "v",
            "st",
        )
        .expect("params");
        assert!(
            params.iter().any(|(k, v)| *k == "state" && v == "st"),
            "claude must echo state: {params:?}"
        );
        assert!(
            !params.iter().any(|(k, _)| *k == "client_secret"),
            "claude is a public client: {params:?}"
        );

        let LoginFlow::Pkce(codex) = crate::registry::CODEX_LOGIN else {
            panic!("codex is a PKCE flow");
        };
        let params = authorization_params(
            &codex,
            "codex",
            "code",
            "http://127.0.0.1:1455/auth/callback",
            "v",
            "st",
        )
        .expect("params");
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
                client_secret: None,
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
                ..
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

    /// Google's token endpoint authenticates an operator-supplied client, so the exchange
    /// must carry the env values and no `state`; the static row is only a placeholder.
    #[test]
    fn google_exchange_carries_the_env_client_secret() {
        let _guard = crate::TEST_ENV
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let LoginFlow::Pkce(google) = crate::registry::GOOGLE_ANTIGRAVITY_LOGIN else {
            panic!("antigravity is a PKCE flow");
        };
        for name in [
            "GOOGLE_ANTIGRAVITY_CLIENT_ID",
            "GOOGLE_ANTIGRAVITY_CLIENT_SECRET",
            "RROUTER_OAUTH_CLIENT_ID_GOOGLE_ANTIGRAVITY",
            "RROUTER_OAUTH_CLIENT_SECRET_GOOGLE_ANTIGRAVITY",
        ] {
            std::env::remove_var(name);
        }
        std::env::set_var("GOOGLE_ANTIGRAVITY_CLIENT_ID", "env-client-id");
        std::env::set_var("GOOGLE_ANTIGRAVITY_CLIENT_SECRET", "env-client-secret");
        let params = authorization_params(
            &google,
            antigravity::PROVIDER_ID,
            "code",
            "http://localhost:51121/oauth-callback",
            "v",
            "st",
        )
        .expect("params");
        let param = |name: &str| {
            params
                .iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| v.as_str())
        };
        assert_eq!(param("client_id"), Some("env-client-id"));
        assert_eq!(param("client_secret"), Some("env-client-secret"));
        assert_eq!(
            param("redirect_uri"),
            Some("http://localhost:51121/oauth-callback")
        );
        assert_eq!(param("state"), None, "Google does not expect state back");
        assert_eq!(param("scope"), None, "the exchange sends no scope");

        for name in [
            "GOOGLE_ANTIGRAVITY_CLIENT_ID",
            "GOOGLE_ANTIGRAVITY_CLIENT_SECRET",
        ] {
            std::env::remove_var(name);
        }
    }

    /// The placeholder is never used on the wire: without both env variables the exchange
    /// is refused before any request.
    #[test]
    fn google_exchange_refuses_missing_env_credentials() {
        let _guard = crate::TEST_ENV
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let LoginFlow::Pkce(google) = crate::registry::GOOGLE_ANTIGRAVITY_LOGIN else {
            panic!("antigravity is a PKCE flow");
        };
        for name in [
            "GOOGLE_ANTIGRAVITY_CLIENT_ID",
            "GOOGLE_ANTIGRAVITY_CLIENT_SECRET",
            "RROUTER_OAUTH_CLIENT_ID_GOOGLE_ANTIGRAVITY",
            "RROUTER_OAUTH_CLIENT_SECRET_GOOGLE_ANTIGRAVITY",
        ] {
            std::env::remove_var(name);
        }
        let err = authorization_params(
            &google,
            antigravity::PROVIDER_ID,
            "code",
            "http://localhost:51121/oauth-callback",
            "v",
            "st",
        )
        .expect_err("missing env");
        assert!(matches!(err, Error::BadRequest(_)), "{err:?}");
        let message = err.to_string();
        assert!(
            message.contains("GOOGLE_ANTIGRAVITY_CLIENT_ID"),
            "{message}"
        );
        assert!(
            message.contains("GOOGLE_ANTIGRAVITY_CLIENT_SECRET"),
            "{message}"
        );
        assert!(
            !message.contains(antigravity::PLACEHOLDER_CLIENT),
            "{message}"
        );
    }

    /// Google userinfo is the only label source: the authorization request does not ask
    /// for `openid`, so no id token is returned.
    #[test]
    fn google_userinfo_email_becomes_the_label() {
        assert_eq!(
            profile_value_label(
                "google-antigravity",
                &serde_json::json!({"email": "user@example.test", "sub": "1"})
            )
            .as_deref(),
            Some("user@example.test")
        );
        assert_eq!(
            profile_value_label("google-antigravity", &serde_json::json!({"email": ""})),
            None
        );
    }

    /// The proxy reads `credentials.extra.project_id`; the exchange writes the same key.
    #[test]
    fn discovered_project_lands_under_the_cca_key() {
        let mut creds = Credentials::default();
        attach_project(&mut creds, "proj-abc".to_string());
        assert_eq!(
            creds.extra.get(antigravity::PROJECT_ID_KEY),
            Some(&serde_json::Value::String("proj-abc".to_string()))
        );
        assert_eq!(antigravity::PROJECT_ID_KEY, "project_id");
    }

    fn authorize_state(authorize_url: &str) -> String {
        reqwest::Url::parse(authorize_url)
            .expect("authorize URL")
            .query_pairs()
            .find(|(key, _)| key == "state")
            .map(|(_, value)| value.into_owned())
            .expect("state")
    }

    fn manual_spec(base: &str) -> &'static ProviderSpec {
        spec_with_profile_id(
            "mock",
            LoginFlow::Pkce(P {
                client_id: "cid",
                client_secret: None,
                authorize_url: Box::leak(format!("{base}/authorize").into_boxed_str()),
                token_url: Box::leak(format!("{base}/token").into_boxed_str()),
                scope: "openid",
                fixed_port: Some(1455),
                redirect_host: "127.0.0.1",
                callback_path: "/auth/callback",
                extra_params: &[],
                send_state: false,
                body: RefreshBody::Form,
            }),
            None,
        )
    }

    /// Manual mode must never bind the registered port: on a remote host that listener
    /// cannot receive the browser redirect. The env client id reaches the authorize URL,
    /// the secret does not.
    #[allow(clippy::await_holding_lock)] // serializes the process-wide env override
    #[tokio::test]
    async fn manual_start_returns_the_registered_redirect_without_binding() {
        let _guard = crate::TEST_ENV
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        std::env::set_var("GOOGLE_ANTIGRAVITY_CLIENT_ID", "env-manual-id");
        std::env::set_var("GOOGLE_ANTIGRAVITY_CLIENT_SECRET", "env-manual-secret");
        let flows = OAuthFlows::new();
        let http = reqwest::Client::new();
        let spec = crate::registry::get("google-antigravity").expect("provider");
        let (id, challenge) = flows.start_manual(&http, spec).await.expect("manual start");
        match &challenge {
            Challenge::Pkce {
                authorize_url,
                redirect_uri,
                manual,
            } => {
                assert!(manual);
                assert_eq!(redirect_uri, "http://localhost:51121/oauth-callback");
                assert!(authorize_url.contains("client_id=env-manual-id"));
                assert!(!authorize_url.contains(antigravity::PLACEHOLDER_CLIENT));
                assert!(authorize_url.contains("code_challenge_method=S256"));
                assert!(authorize_url.contains("access_type=offline"));
                assert!(authorize_url.contains("prompt=consent"));
            }
            other => panic!("expected PKCE, got {other:?}"),
        }
        let serialized = serde_json::to_string(&challenge).expect("challenge JSON");
        assert!(
            !serialized.contains("env-manual-secret"),
            "the secret must never leave the server: {serialized}"
        );
        assert!(flows.is_pending(&id));
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 51121)).await;
        assert!(
            listener.is_ok(),
            "manual start must not bind the registered redirect port"
        );
        drop(listener);
        assert!(flows.cancel(&id));
        std::env::remove_var("GOOGLE_ANTIGRAVITY_CLIENT_ID");
        std::env::remove_var("GOOGLE_ANTIGRAVITY_CLIENT_SECRET");
    }

    /// No listener, no authorize URL and no pending login when the required env is absent.
    #[allow(clippy::await_holding_lock)] // serializes the process-wide env override
    #[tokio::test]
    async fn manual_start_refuses_missing_google_client_env() {
        let _guard = crate::TEST_ENV
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        for name in [
            "GOOGLE_ANTIGRAVITY_CLIENT_ID",
            "GOOGLE_ANTIGRAVITY_CLIENT_SECRET",
            "RROUTER_OAUTH_CLIENT_ID_GOOGLE_ANTIGRAVITY",
            "RROUTER_OAUTH_CLIENT_SECRET_GOOGLE_ANTIGRAVITY",
        ] {
            std::env::remove_var(name);
        }
        let flows = OAuthFlows::new();
        let http = reqwest::Client::new();
        let spec = crate::registry::get("google-antigravity").expect("provider");
        let err = flows
            .start_manual(&http, spec)
            .await
            .expect_err("missing env");
        assert!(matches!(err, Error::BadRequest(_)), "{err:?}");
        let message = err.to_string();
        for name in [
            "GOOGLE_ANTIGRAVITY_CLIENT_ID",
            "GOOGLE_ANTIGRAVITY_CLIENT_SECRET",
        ] {
            assert!(message.contains(name), "{message}");
        }
        assert!(flows.pending_ids().is_empty(), "no pending login created");
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 51121)).await;
        assert!(listener.is_ok(), "no listener was bound");
    }

    #[tokio::test]
    async fn manual_start_requires_a_fixed_redirect_port() {
        let flows = OAuthFlows::new();
        let http = reqwest::Client::new();
        let spec = crate::registry::get("claude").expect("claude");
        let err = flows
            .start_manual(&http, spec)
            .await
            .expect_err("claude uses an ephemeral port");
        assert!(matches!(err, Error::BadRequest(_)), "{err:?}");
        assert!(
            err.to_string()
                .contains("manual callback requires a fixed redirect port"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn device_sign_in_rejects_manual_mode() {
        let flows = OAuthFlows::new();
        let http = reqwest::Client::new();
        let spec = crate::registry::get("github").expect("github");
        let err = flows
            .start_manual(&http, spec)
            .await
            .expect_err("device flow has no redirect");
        assert!(err.to_string().contains("device code"), "{err}");
    }

    #[tokio::test]
    async fn manual_complete_exchanges_the_pasted_code_once() {
        let (base, hits) = oauth_server().await;
        let spec = manual_spec(&base);
        let flows = OAuthFlows::new();
        let http = reqwest::Client::new();
        let (id, challenge) = flows.start_manual(&http, spec).await.expect("start");
        let Challenge::Pkce { authorize_url, .. } = challenge else {
            panic!("expected PKCE");
        };
        let state = authorize_state(&authorize_url);
        let callback = format!("http://127.0.0.1:1455/auth/callback?code=MANUAL&state={state}");

        let done = flows
            .complete_manual(&http, &id, &callback)
            .await
            .expect("manual complete");
        assert_eq!(done.provider, "mock");
        assert_eq!(done.credentials.access_token.as_deref(), Some("pkce-token"));
        assert_eq!(hits.load(Ordering::SeqCst), 1, "one exchange");

        // Replay must not spend the code a second time.
        let again = flows
            .complete_manual(&http, &id, &callback)
            .await
            .expect("replay");
        assert_eq!(
            again.credentials.access_token.as_deref(),
            Some("pkce-token")
        );
        assert_eq!(hits.load(Ordering::SeqCst), 1, "no second exchange");
    }

    #[tokio::test]
    async fn manual_complete_rejects_a_foreign_state_without_exchanging() {
        let (base, hits) = oauth_server().await;
        let spec = manual_spec(&base);
        let flows = OAuthFlows::new();
        let http = reqwest::Client::new();
        let (id, _) = flows.start_manual(&http, spec).await.expect("start");

        let err = flows
            .complete_manual(
                &http,
                &id,
                "http://127.0.0.1:1455/auth/callback?code=STOLEN&state=ATTACKER",
            )
            .await
            .expect_err("state mismatch");
        assert!(matches!(err, Error::Unauthorized(_)), "{err:?}");
        assert_eq!(hits.load(Ordering::SeqCst), 0, "no token request");

        // The failure is recorded: a later correct URL replays it, still no exchange.
        let err = flows
            .complete_manual(
                &http,
                &id,
                "http://127.0.0.1:1455/auth/callback?code=OK&state=whatever",
            )
            .await
            .expect_err("replayed failure");
        assert!(err.to_string().contains("state mismatch"), "{err}");
        assert_eq!(hits.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn completing_a_manual_login_without_callback_url_is_a_clear_error() {
        let (base, _hits) = oauth_server().await;
        let spec = manual_spec(&base);
        let flows = OAuthFlows::new();
        let http = reqwest::Client::new();
        let (id, _) = flows.start_manual(&http, spec).await.expect("start");
        let err = flows.complete(&http, &id).await.expect_err("no listener");
        assert!(matches!(err, Error::BadRequest(_)), "{err:?}");
        assert!(err.to_string().contains("callback_url"), "{err}");
    }

    #[tokio::test]
    async fn manual_callback_error_redirect_is_reported() {
        let (base, hits) = oauth_server().await;
        let spec = manual_spec(&base);
        let flows = OAuthFlows::new();
        let http = reqwest::Client::new();
        let (id, challenge) = flows.start_manual(&http, spec).await.expect("start");
        let Challenge::Pkce { authorize_url, .. } = challenge else {
            panic!("expected PKCE");
        };
        let state = authorize_state(&authorize_url);
        let callback = format!(
            "http://127.0.0.1:1455/auth/callback?error=access_denied\
&error_description=user%20refused&state={state}"
        );
        let err = flows
            .complete_manual(&http, &id, &callback)
            .await
            .expect_err("denied");
        assert!(matches!(err, Error::Unauthorized(_)), "{err:?}");
        assert_eq!(hits.load(Ordering::SeqCst), 0);
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
            client_secret: None,
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
