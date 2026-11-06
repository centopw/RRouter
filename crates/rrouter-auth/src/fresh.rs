//! Credential freshness: exactly one refresh per credential no matter how many concurrent
//! requests need it.
//!
//! The failure this module exists to prevent: several in-flight requests notice an expired
//! access token at the same moment, all POST the *single-use* refresh token, the first one
//! rotates it, the rest send a now-dead token and the account is permanently `401`. Instead
//! each credential id owns a `tokio::sync::Mutex<()>`; the first waiter refreshes and
//! persists, everyone else re-reads the store after acquiring it and sees the new token.
//!
//! Lock ordering is always `refresh_locks -> store`, never the reverse: `CredentialStore`
//! implementations must not call back into [`Refresher`] while holding their own lock.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use rrouter_core::{Error, Result};
use serde::Deserialize;
use tokio::sync::Mutex as AsyncMutex;
use uuid::Uuid;

use crate::credentials::Credentials;
use crate::registry::{ProviderKind, ProviderSpec, RefreshBody, RefreshSpec};

/// Credentials are considered stale this many seconds before they actually expire, so a
/// token cannot die mid-flight between the freshness check and the upstream request.
pub const LEAD_SECS: i64 = 300;

/// GitHub Copilot session-token endpoint. Not a `RefreshSpec`: the long-lived GitHub token
/// is the credential and the copilot token is derived from it.
pub const GITHUB_COPILOT_TOKEN_URL: &str = "https://api.github.com/copilot_internal/v2/token";

const GITHUB_EDITOR_VERSION: &str = "vscode/1.110.0";
const GITHUB_USER_AGENT: &str = "rrouter/0.1.0";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// `Credentials.extra` key that redirects the copilot token derivation (enterprise or
/// self-hosted GitHub). Tests use it to point at a local mock server.
pub const EXTRA_COPILOT_URL: &str = "copilot_token_url";

pub trait CredentialStore: Send + Sync {
    /// Read current credentials. Called on every request, so it must be cheap.
    fn load(&self, id: Uuid) -> Pin<Box<dyn Future<Output = Result<Credentials>> + Send + '_>>;

    /// Atomically merge these fields into the stored row; returns the merged value.
    ///
    /// `None` and empty `extra` mean "leave as stored", so two refreshes cannot clobber
    /// each other's fields. The returned value is the authoritative post-merge state.
    fn merge(
        &self,
        id: Uuid,
        patch: Credentials,
    ) -> Pin<Box<dyn Future<Output = Result<Credentials>> + Send + '_>>;
}

type RefreshLock = Arc<AsyncMutex<()>>;

/// Refreshes expired credentials with per-credential deduplication.
pub struct Refresher {
    http: reqwest::Client,
    /// One async mutex per credential id. Entries are removed when the last waiter drops
    /// its `Arc`; the outer mutex guards only the map, never network or store I/O.
    locks: Mutex<HashMap<Uuid, RefreshLock>>,
}

impl Refresher {
    pub fn new(http: reqwest::Client) -> Self {
        Self {
            http,
            locks: Mutex::new(HashMap::new()),
        }
    }

    /// Returns credentials guaranteed valid for the next [`LEAD_SECS`].
    ///
    /// Fast path: nothing expires within the lead window → one `load`, no lock, no HTTP.
    /// Slow path: take the credential's mutex, re-read the store (a peer may have refreshed
    /// while we waited), and only transport for real if it is *still* stale.
    pub async fn fresh(
        &self,
        store: &dyn CredentialStore,
        provider: &ProviderSpec,
        id: Uuid,
    ) -> Result<Credentials> {
        let creds = store.load(id).await?;
        if !self.is_stale(provider, &creds) {
            return Ok(creds);
        }

        let lock = self.lock_for(id);
        let _guard = lock.lock().await;

        // Double-check: the peer we queued behind has already refreshed and persisted.
        let creds = store.load(id).await?;
        if !self.is_stale(provider, &creds) {
            return Ok(creds);
        }

        let patch = self.exchange(provider, &creds).await?;
        store.merge(id, patch).await
    }

    /// True when the credential material this provider authenticates with will expire
    /// within [`LEAD_SECS`], or is already expired.
    fn is_stale(&self, provider: &ProviderSpec, c: &Credentials) -> bool {
        let now = now_unix();
        let deadline = match provider.id {
            // The copilot session token is what goes on the wire; the GitHub token behind it
            // is long-lived, so its own expiry does not matter here. A connection holding the
            // long-lived token but no copilot token yet must derive one immediately.
            "github" => {
                if c.copilot_token.is_none() && c.access_token.is_some() {
                    return true;
                }
                c.copilot_expires_at
            }
            _ => match provider.kind {
                ProviderKind::OAuth => c.expires_at,
                ProviderKind::ApiKey | ProviderKind::Local => return false,
            },
        };
        match deadline {
            Some(at) => at - LEAD_SECS <= now,
            // No expiry recorded: nothing to refresh toward.
            None => false,
        }
    }

    /// Per-credential lock. Entries with no waiter left are dropped, so the map
    /// stays bounded by the credentials refreshing *right now*, not by every
    /// credential the process has ever refreshed.
    fn lock_for(&self, id: Uuid) -> RefreshLock {
        let mut map = lock_map(&self.locks);
        map.retain(|_, lock| Arc::strong_count(lock) > 1);
        map.entry(id).or_default().clone()
    }

    /// Perform the provider's refresh and turn the response into a store patch (never a
    /// full overwrite: only the fields the endpoint actually returned).
    async fn exchange(&self, provider: &ProviderSpec, c: &Credentials) -> Result<Credentials> {
        if provider.id == "github" {
            return self.exchange_github_copilot(c).await;
        }
        let spec = provider.refresh.ok_or_else(|| {
            Error::Unauthorized(format!(
                "provider {} has no refresh recipe; credential is stale",
                provider.id
            ))
        })?;
        let refresh_token = c.refresh_token.as_deref().ok_or_else(|| {
            Error::Unauthorized(format!(
                "provider {} credential has no refresh token",
                provider.id
            ))
        })?;
        self.exchange_oauth(provider.id, spec, refresh_token).await
    }

    /// RFC 6749 refresh for `claude` (JSON), `codex` (form) and any future OAuth provider.
    async fn exchange_oauth(
        &self,
        provider_id: &str,
        spec: RefreshSpec,
        refresh_token: &str,
    ) -> Result<Credentials> {
        let mut req = self
            .http
            .post(spec.token_url)
            .timeout(REQUEST_TIMEOUT)
            .header("accept", "application/json");

        req = match spec.body {
            RefreshBody::Json => {
                let mut body = serde_json::Map::with_capacity(4);
                body.insert("grant_type".into(), "refresh_token".into());
                body.insert("refresh_token".into(), refresh_token.into());
                body.insert("client_id".into(), spec.client_id.into());
                if provider_id == "codex" {
                    body.insert(
                        "scope".into(),
                        "openid profile email offline_access".into(),
                    );
                }
                req.json(&serde_json::Value::Object(body))
            }
            RefreshBody::Form => req.form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh_token),
                ("client_id", spec.client_id),
                ("scope", "openid profile email offline_access"),
            ]),
        };

        let resp = req.send().await.map_err(transport)?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.map_err(transport)?;
            return Err(refresh_error(provider_id, status.as_u16(), &body));
        }
        let tokens: TokenResponse = resp.json().await.map_err(transport)?;
        if tokens.access_token.is_empty() {
            return Err(Error::Unauthorized(format!(
                "provider {provider_id} refresh returned an empty access token"
            )));
        }

        let mut patch = Credentials {
            access_token: Some(tokens.access_token),
            refresh_token: tokens.refresh_token.filter(|t| !t.is_empty()),
            ..Credentials::default()
        };
        patch.expires_at = tokens.expires_in.map(|secs| now_unix() + i64::from(secs));
        tracing::debug!(provider = provider_id, "refreshed OAuth credential");
        Ok(patch)
    }

    /// `github`: derive a short-lived copilot session token from the long-lived GitHub
    /// access token stored in `access_token`.
    async fn exchange_github_copilot(&self, c: &Credentials) -> Result<Credentials> {
        let gh_token = c.access_token.as_deref().ok_or_else(|| {
            Error::Unauthorized("github credential has no access token".to_string())
        })?;

        // Enterprise/self-hosted GitHub may override via provider-specific data.
        let url = c
            .extra
            .get(EXTRA_COPILOT_URL)
            .and_then(serde_json::Value::as_str)
            .unwrap_or(GITHUB_COPILOT_TOKEN_URL);

        let resp = self
            .http
            .get(url)
            .header("authorization", format!("Bearer {gh_token}"))
            .header("editor-version", GITHUB_EDITOR_VERSION)
            .header("user-agent", GITHUB_USER_AGENT)
            .header("accept", "application/json")
            .send()
            .await
            .map_err(transport)?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.map_err(transport)?;
            return Err(refresh_error("github", status.as_u16(), &body));
        }
        let token: CopilotTokenResponse = resp.json().await.map_err(transport)?;
        if token.token.is_empty() {
            return Err(Error::Unauthorized(
                "github copilot token endpoint returned an empty token".to_string(),
            ));
        }

        let mut patch = Credentials {
            copilot_token: Some(token.token),
            copilot_expires_at: token.expires_at,
            ..Credentials::default()
        };
        // Copilot tokens carry per-token upstream selection in the `proxy-ep`/`tid` query
        // of the token string; 9router keeps the raw token, so `extra` stays untouched.
        patch.extra = serde_json::Map::new();
        tracing::debug!("derived new GitHub Copilot session token");
        Ok(patch)
    }
}

/// Response of an RFC 6749 token endpoint (superset of what any provider returns).
#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<u32>,
}

/// Response of the GitHub Copilot token endpoint.
#[derive(Debug, Deserialize)]
struct CopilotTokenResponse {
    token: String,
    #[serde(default)]
    expires_at: Option<i64>,
}

/// Map a reqwest failure onto the shared error type, keeping timeouts distinguishable so the
/// gateway can return 504 rather than 502.
fn transport(e: reqwest::Error) -> Error {
    if e.is_timeout() {
        Error::Timeout(format!("credential refresh: {e}"))
    } else {
        Error::Transport(format!("credential refresh: {e}"))
    }
}

fn refresh_error(provider_id: &str, status: u16, body: &str) -> Error {
    // A dead refresh token is the account's problem, not the client's: surface it as
    // Unauthorized(401) so routing can fall through to another account.
    Error::Unauthorized(format!(
        "provider {provider_id} refresh failed with HTTP {status}: {}",
        truncate(body, 512)
    ))
}

fn truncate(s: &str, max: usize) -> &str {
    match s.char_indices().nth(max) {
        Some((idx, _)) => &s[..idx],
        None => s,
    }
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Rows are never poisoned by a panic in another task in a way that matters here (the map
/// holds no invariants beyond "id → mutex"), so recover the guard instead of panicking.
fn lock_map(m: &Mutex<HashMap<Uuid, RefreshLock>>) -> MutexGuard<'_, HashMap<Uuid, RefreshLock>> {
    m.lock().unwrap_or_else(|poison| poison.into_inner())
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use rrouter_core::Error;
    use uuid::Uuid;

    use super::*;
    use crate::registry;

    /// In-memory store that counts `load`/`merge` and can be seeded.
    struct FakeStore {
        row: Mutex<Credentials>,
        loads: AtomicUsize,
        merges: AtomicUsize,
    }

    impl FakeStore {
        fn new(creds: Credentials) -> Arc<Self> {
            Arc::new(Self {
                row: Mutex::new(creds),
                loads: AtomicUsize::new(0),
                merges: AtomicUsize::new(0),
            })
        }
        fn snapshot(&self) -> Credentials {
            self.row.lock().expect("row").clone()
        }
    }

    impl CredentialStore for FakeStore {
        fn load(&self, _id: Uuid) -> Pin<Box<dyn Future<Output = Result<Credentials>> + Send + '_>> {
            self.loads.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move { Ok(self.snapshot()) })
        }

        fn merge(
            &self,
            _id: Uuid,
            patch: Credentials,
        ) -> Pin<Box<dyn Future<Output = Result<Credentials>> + Send + '_>> {
            self.merges.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                let mut row = self.row.lock().expect("row");
                // `None`/empty patch fields mean "leave as stored".
                if patch.access_token.is_some() {
                    row.access_token = patch.access_token;
                }
                if patch.refresh_token.is_some() {
                    row.refresh_token = patch.refresh_token;
                }
                if patch.expires_at.is_some() {
                    row.expires_at = patch.expires_at;
                }
                if patch.copilot_token.is_some() {
                    row.copilot_token = patch.copilot_token;
                }
                if patch.copilot_expires_at.is_some() {
                    row.copilot_expires_at = patch.copilot_expires_at;
                }
                Ok(row.clone())
            })
        }
    }

    fn client() -> reqwest::Client {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .expect("client")
    }

    fn expired(access: &str) -> Credentials {
        Credentials {
            access_token: Some(access.to_string()),
            refresh_token: Some("rt-old".to_string()),
            expires_at: Some(now_unix() - 60),
            ..Credentials::default()
        }
    }

    /// `'static` URL of a mock server, for building a `ProviderSpec` inside a test.
    fn mock_url(server: &wiremock::MockServer) -> &'static str {
        let uri = server.uri();
        let mut urls = MOCK_URLS.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(found) = urls.iter().find(|u| **u == uri) {
            return found;
        }
        // Mock servers outlive the specs built from them; one small allocation per test.
        let url = Box::leak(uri.into_boxed_str());
        urls.push(url);
        url
    }

    static MOCK_URLS: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());

    /// Registry spec of `id` with its token endpoint swapped for a mock.
    fn claude_spec(id: &str, token_url: &'static str, body: RefreshBody) -> ProviderSpec {
        ProviderSpec {
            refresh: Some(RefreshSpec {
                token_url,
                client_id: "test-client",
                body,
            }),
            ..*registry::get(id).expect("registered provider")
        }
    }

    #[tokio::test]
    async fn concurrent_callers_hit_token_endpoint_once() {
        let server = wiremock::MockServer::start().await;
        let hits = Arc::new(AtomicUsize::new(0));

        let hits_for_handler = hits.clone();
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .respond_with(move |req: &wiremock::Request| {
                hits_for_handler.fetch_add(1, Ordering::SeqCst);
                // Single-use refresh token: the rotated value is what proves dedup.
                let body: serde_json::Value =
                    serde_json::from_slice(&req.body).unwrap_or_default();
                assert_eq!(body["refresh_token"], "rt-old");
                wiremock::ResponseTemplate::new(200)
                    // Hold the winner in the endpoint so late arrivals must queue on the
                    // per-credential mutex instead of racing past a completed refresh.
                    .set_delay(Duration::from_millis(50))
                    .set_body_json(serde_json::json!({
                        "access_token": "at-new",
                        "refresh_token": "rt-new",
                        "expires_in": 3600
                    }))
            })
            .mount(&server)
            .await;

        let store = FakeStore::new(expired("at-old"));
        let refresher = Arc::new(Refresher::new(client()));

        // Same provider, pointed at the mock endpoint.
        let spec = claude_spec("claude", mock_url(&server), RefreshBody::Json);

        let id = Uuid::new_v4();

        // All 20 are spawned before the first await, so they contend on the leader's lock.
        let mut tasks = Vec::with_capacity(20);
        for _ in 0..20 {
            let refresher = refresher.clone();
            let store = store.clone();
            tasks.push(tokio::spawn(async move {
                refresher.fresh(store.as_ref(), &spec, id).await
            }));
        }
        let mut tokens = Vec::with_capacity(20);
        for t in tasks {
            let c = t.await.expect("join").expect("fresh");
            tokens.push(c.access_token.expect("access token"));
        }

        assert_eq!(hits.load(Ordering::SeqCst), 1, "token endpoint must be hit once");
        assert!(tokens.iter().all(|t| t == "at-new"), "all callers get the new token");
        assert_eq!(store.merges.load(Ordering::SeqCst), 1, "one persist");
        assert_eq!(store.snapshot().refresh_token.as_deref(), Some("rt-new"));
    }

    #[tokio::test]
    async fn second_caller_after_completion_does_not_refresh_again() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "access_token": "at-new",
                    "expires_in": 3600
                })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let store = FakeStore::new(expired("at-old"));
        let refresher = Refresher::new(client());
        let spec = claude_spec("claude", mock_url(&server), RefreshBody::Json);
        let id = Uuid::new_v4();

        let first = refresher.fresh(store.as_ref(), &spec, id).await.expect("first");
        assert_eq!(first.access_token.as_deref(), Some("at-new"));

        // Second caller entering after the first completed: fresh now, zero network.
        let second = refresher.fresh(store.as_ref(), &spec, id).await.expect("second");
        assert_eq!(second.access_token.as_deref(), Some("at-new"));
        assert_eq!(store.merges.load(Ordering::SeqCst), 1);

        // Dropping the mock asserts `expect(1)` was satisfied exactly.
        drop(server);
    }

    #[tokio::test]
    async fn valid_credential_makes_no_http_request() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::any())
            .respond_with(wiremock::ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;

        let store = FakeStore::new(Credentials {
            access_token: Some("at-live".to_string()),
            refresh_token: Some("rt".to_string()),
            expires_at: Some(now_unix() + 3600),
            ..Credentials::default()
        });
        let refresher = Refresher::new(client());
        let spec = claude_spec("claude", mock_url(&server), RefreshBody::Json);

        let c = refresher
            .fresh(store.as_ref(), &spec, Uuid::new_v4())
            .await
            .expect("fresh");
        assert_eq!(c.access_token.as_deref(), Some("at-live"));
        assert_eq!(store.loads.load(Ordering::SeqCst), 1);
        assert_eq!(store.merges.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn credential_without_expiry_is_never_refreshed() {
        let store = FakeStore::new(Credentials {
            api_key: Some("sk-static".to_string()),
            ..Credentials::default()
        });
        let refresher = Refresher::new(client());
        let c = refresher
            .fresh(store.as_ref(), registry::get("openai").expect("openai"), Uuid::new_v4())
            .await
            .expect("fresh");
        assert_eq!(c.api_key.as_deref(), Some("sk-static"));
        assert_eq!(store.merges.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn github_copilot_token_is_rederived_on_expiry() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::header("authorization", "Bearer gh-token"))
            .and(wiremock::matchers::header("editor-version", GITHUB_EDITOR_VERSION))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "token": "copilot-new",
                    "expires_at": now_unix() + 1800
                })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let mut extra = serde_json::Map::new();
        extra.insert(
            EXTRA_COPILOT_URL.to_string(),
            serde_json::Value::String(server.uri()),
        );
        let store = FakeStore::new(Credentials {
            access_token: Some("gh-token".to_string()),
            copilot_token: Some("copilot-old".to_string()),
            copilot_expires_at: Some(now_unix() - 10),
            extra,
            ..Credentials::default()
        });
        let provider = registry::get("github").expect("github");
        let id = Uuid::new_v4();
        let refresher = Refresher::new(client());

        let c = refresher.fresh(store.as_ref(), provider, id).await.expect("fresh");
        assert_eq!(c.copilot_token.as_deref(), Some("copilot-new"));
        assert!(c.copilot_expires_at.is_some_and(|at| at > now_unix()));
        assert_eq!(
            c.access_token.as_deref(),
            Some("gh-token"),
            "long-lived GitHub token must not be rewritten"
        );

        // Copilot token now inside the lead window: no second derivation.
        let again = refresher.fresh(store.as_ref(), provider, id).await.expect("fresh");
        assert_eq!(again.copilot_token.as_deref(), Some("copilot-new"));
        assert_eq!(store.merges.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn refresh_failure_is_unauthorized_and_not_persisted() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .respond_with(
                wiremock::ResponseTemplate::new(400)
                    .set_body_json(serde_json::json!({"error": "invalid_grant"})),
            )
            .mount(&server)
            .await;

        let store = FakeStore::new(expired("at-old"));
        let refresher = Refresher::new(client());
        let spec = claude_spec("claude", mock_url(&server), RefreshBody::Form);

        let err = refresher
            .fresh(store.as_ref(), &spec, Uuid::new_v4())
            .await
            .expect_err("must fail");
        assert!(matches!(err, Error::Unauthorized(_)), "got {err:?}");
        assert_eq!(err.status(), 401);
        assert_eq!(store.merges.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn oauth_provider_without_refresh_token_fails_closed() {
        let store = FakeStore::new(Credentials {
            access_token: Some("at".to_string()),
            expires_at: Some(now_unix() - 1),
            ..Credentials::default()
        });
        let refresher = Refresher::new(client());
        let err = refresher
            .fresh(
                store.as_ref(),
                registry::get("codex").expect("codex"),
                Uuid::new_v4(),
            )
            .await
            .expect_err("must fail");
        assert!(matches!(err, Error::Unauthorized(_)), "got {err:?}");
    }
}
