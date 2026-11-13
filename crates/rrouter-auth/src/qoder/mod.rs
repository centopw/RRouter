//! Qoder (api3.qoder.sh): the subscription coding-agent cloud qodercli talks to.
//!
//! Qoder is not an OpenAI-compatible endpoint. Inference is a `POST` to
//! `/algo/api/v2/service/pro/sse/agent_chat_generation?…&Encode=1` whose body is obfuscated
//! ([`encoding`]), COSY-signed ([`cosy`]) and whose answer is an SSE wrapper around stringified
//! OpenAI chunks. Two pieces live outside this crate: the envelope and the response decoder in
//! `rrouter_translate::qoder`, and the per-attempt signing in the gateway.
//!
//! # What the operator must provide
//!
//! RRouter does not run Qoder's OAuth/device flow: a Qoder connection is operator-supplied
//! credential material. Create the connection with:
//!
//! ```json
//! {
//!   "provider": "qoder",
//!   "credentials": {
//!     "api_key": "dt-…",
//!     "user_id": "…",
//!     "machine_id": "…"        // optional
//!   }
//! }
//! ```
//!
//! - `api_key` — a **personal access token** (`pt-…`) or the **device access token**
//!   (`dt-…`) qodercli stores after a browser sign-in. `access_token` is accepted as an
//!   alias. A PAT is exchanged for a short-lived job token (`jt-…`) by [`pat::resolve`];
//!   the exchange, its cache and the userinfo lookup live in [`pat`].
//! - `user_id` — the account's stable Qoder user id. COSY signing puts it in the clear
//!   (`Cosy-User`) and inside the encrypted blob, so a wrong value signs every request
//!   wrongly. With a PAT it is optional: the id is read from `/api/v1/userinfo` and this
//!   value is the fallback when that call fails.
//! - `machine_id` — optional. qodercli generates one UUID per install and reuses it for
//!   every request; Qoder tracks per-device queue position by it. When it is omitted RRouter
//!   derives a stable one from `user_id` ([`cosy::derive_machine_id`]), which keeps queue
//!   position across restarts. Two connections for the same account SHOULD NOT share one,
//!   exactly as two installs do not.
//! - `account_label` / `extra.email` — optional, carried inside the encrypted blob.
//!
//! Where qodercli keeps these on the operator's own machine (v1.1.x, macOS and Linux):
//!
//! - `~/.qoder/.auth` (or `%USERPROFILE%\.qoder\.auth` on Windows) — the signed-in account
//!   record: the `dt-…` token and the user id written by `qoder login`.
//! - `~/.qoder/.qoder/machineid` — the per-install machine UUID.
//!
//! Read them from there while signed in; RRouter never writes to those files.
//!
//! # Model identity
//!
//! `/algo/api/v2/model/list` is per-account and the chat endpoint *silently serves a
//! different model* when `model_config` does not match the requested key. So [`model_config`]
//! refuses a model the account's catalogue does not list, instead of downgrading: a loud
//! error is recoverable, an answer from the wrong model is not.
//!
//! # What keeps this from being an OpenAI-compatible provider
//!
//! - The signature covers the encoded body bytes, so every attempt is re-encoded and
//!   re-signed; a replayed signature is rejected (`403`, code `103`).
//! - A `pt-` personal access token is exchanged for a job token (`jt-…`) on the openapi
//!   host first ([`pat`]). Job tokens are served by `api2.qoder.sh`; `api3` answers them
//!   with `Login expired` (`403`), so callers pick the origin with
//!   [`rrouter_translate::qoder::base_for`]. Device tokens stay on `api3`.
//! - Requests are never cached, because the body embeds fresh ids and the signature covers
//!   a timestamp.

pub mod cosy;
pub mod encoding;
pub mod pat;

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use rrouter_core::{Error, Result};

pub use cosy::{signer, QoderCredentials};
pub use pat::resolve;

/// Registry id of the provider.
pub const PROVIDER_ID: &str = "qoder";

/// `credentials.extra` keys a connection carries.
pub const USER_ID_KEY: &str = "user_id";
pub const MACHINE_ID_KEY: &str = "machine_id";
pub const EMAIL_KEY: &str = "email";

/// How long a fetched catalogue is reused. The per-account model list changes when an
/// entitlement changes, not between requests.
const CATALOG_TTL: Duration = Duration::from_secs(60 * 60);

/// Deadline for one catalogue fetch.
const FETCH_TIMEOUT: Duration = Duration::from_secs(20);

/// Prefix of a personal access token. These cannot sign COSY requests directly; they are
/// exchanged for a job token first ([`pat`]).
pub const PAT_PREFIX: &str = "pt-";

/// One account's model catalogue: the raw `model_config` blocks keyed by model id.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Catalog {
    configs: Map<String, Value>,
}

impl Catalog {
    /// Number of models the account can call.
    pub fn len(&self) -> usize {
        self.configs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.configs.is_empty()
    }

    /// The account's own `model_config` for `model`, verbatim.
    pub fn config(&self, model: &str) -> Option<&Value> {
        self.configs.get(model)
    }

    /// Model ids, in the order the upstream listed them.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.configs.keys().map(String::as_str)
    }

    /// `(model id, raw block)` pairs, in the order the upstream listed them.
    pub fn entries(&self) -> impl Iterator<Item = (&str, &Value)> {
        self.configs
            .iter()
            .map(|(key, block)| (key.as_str(), block))
    }

    /// A model to use when the caller has none in mind (the admin probe): `auto` when the
    /// account lists it, else the first entry, since Qoder always publishes a tier model.
    pub fn default_model(&self) -> Option<&str> {
        self.configs
            .get_key_value("auto")
            .map(|(key, _)| key.as_str())
            .or_else(|| self.configs.keys().next().map(String::as_str))
    }
}

/// Parse `{"chat": [{"key": …, "display_name": …, "max_output_tokens": …}, …]}`.
///
/// An entry without a string `key` is not addressable and is dropped. `enable:false`
/// entries are kept: the IDE hides them, the chat endpoint still serves them.
pub fn parse_catalog(body: &Value) -> Result<Catalog> {
    let entries = body
        .get("chat")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::Translation("qoder: model list has no `chat` array".into()))?;
    let mut configs = Map::with_capacity(entries.len());
    for entry in entries {
        let Some(object) = entry.as_object() else {
            continue;
        };
        let Some(key) = object.get("key").and_then(Value::as_str) else {
            continue;
        };
        if key.trim().is_empty() {
            continue;
        }
        configs.insert(key.to_string(), Value::Object(object.clone()));
    }
    if configs.is_empty() {
        return Err(Error::Translation(
            "qoder: model list carried no addressable model".into(),
        ));
    }
    Ok(Catalog { configs })
}

/// Fetch one account's catalogue, or reuse the cached copy.
pub async fn catalog(
    http: &reqwest::Client,
    credentials: &QoderCredentials,
    base: &str,
) -> Result<Arc<Catalog>> {
    let key = cache_key(credentials, base);
    if let Some(cached) = cached(&key) {
        return Ok(cached);
    }
    let fetched = Arc::new(fetch_catalog(http, credentials, base).await?);
    store(&key, Arc::clone(&fetched));
    Ok(fetched)
}

/// The account's `model_config` for `model`.
///
/// A model the catalogue does not list is a hard error: the chat endpoint does not validate
/// `model_config.key` against the catalogue, it simply answers with whatever the block
/// describes, so guessing would route the request to a different model.
pub async fn model_config(
    http: &reqwest::Client,
    credentials: &QoderCredentials,
    base: &str,
    model: &str,
) -> Result<Value> {
    let catalog = catalog(http, credentials, base).await?;
    catalog.config(model).cloned().ok_or_else(|| {
        let mut available: Vec<&str> = catalog.keys().take(12).collect();
        available.sort_unstable();
        Error::BadRequest(format!(
            "qoder account does not list model \"{model}\" (available: {}); \
             refusing to send a request the upstream would answer with a different model",
            available.join(", ")
        ))
    })
}

async fn fetch_catalog(
    http: &reqwest::Client,
    credentials: &QoderCredentials,
    base: &str,
) -> Result<Catalog> {
    if credentials.token.starts_with(PAT_PREFIX) {
        return Err(Error::Unauthorized(
            "qoder: a `pt-` personal access token cannot sign COSY requests; supply the \
             `dt-…` device token"
                .into(),
        ));
    }
    let url = rrouter_translate::qoder::models_url(base);
    // A GET is signed over an empty body, which is also what `Cosy-Bodyhash` covers.
    let mut request = http
        .get(&url)
        .timeout(FETCH_TIMEOUT)
        .header(reqwest::header::ACCEPT, "application/json")
        // gzip makes Qoder's CDN validate the signature against the compressed form.
        .header(reqwest::header::ACCEPT_ENCODING, "identity");
    for (name, value) in cosy::sign_headers(&[], &url, credentials)? {
        request = request.header(name, value);
    }
    let response = request.send().await.map_err(|e| {
        if e.is_timeout() {
            Error::Timeout(format!("qoder model list: {e}"))
        } else {
            Error::Transport(format!("qoder model list: {e}"))
        }
    })?;
    let status = response.status().as_u16();
    let bytes = response
        .bytes()
        .await
        .map_err(|e| Error::Transport(format!("qoder model list body: {e}")))?;
    let payload: Option<Value> = serde_json::from_slice(&bytes).ok();
    if !(200..300).contains(&status) {
        return Err(Error::Upstream {
            provider: PROVIDER_ID.into(),
            status,
            body: String::from_utf8_lossy(&bytes).chars().take(512).collect(),
            payload,
        });
    }
    let body = payload.ok_or_else(|| Error::Translation("qoder: model list is not JSON".into()))?;
    parse_catalog(&body)
}

struct Entry {
    configs: Arc<Catalog>,
    expires: Instant,
}

type Cache = Mutex<HashMap<String, Entry>>;

fn cache() -> &'static Cache {
    static CACHE: LazyLock<Cache> = LazyLock::new(|| Mutex::new(HashMap::new()));
    &CACHE
}

fn cached(key: &str) -> Option<Arc<Catalog>> {
    let mut cache = cache().lock().unwrap_or_else(|e| e.into_inner());
    let entry = cache.get(key)?;
    if entry.expires <= Instant::now() {
        cache.remove(key);
        return None;
    }
    Some(Arc::clone(&entry.configs))
}

fn store(key: &str, configs: Arc<Catalog>) {
    cache().lock().unwrap_or_else(|e| e.into_inner()).insert(
        key.to_string(),
        Entry {
            configs,
            expires: Instant::now() + CATALOG_TTL,
        },
    );
}

/// Cache identity: one entry per account *and* origin. The token is hashed, never stored.
fn cache_key(credentials: &QoderCredentials, base: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(base.trim_end_matches('/').as_bytes());
    hash.update(b"\0");
    hash.update(credentials.user_id.as_bytes());
    hash.update(b"\0");
    hash.update(credentials.token.as_bytes());
    let digest = hash.finalize();
    let mut out = String::with_capacity(32);
    for byte in &digest[..16] {
        use std::fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_catalog_keeps_raw_blocks_and_drops_unaddressable_entries() {
        let body = json!({
            "chat": [
                {"key": "auto", "display_name": "Auto", "max_input_tokens": 200000, "is_vl": true},
                {"key": "qmodel", "enable": false},
                {"display_name": "no key"},
                {"key": ""},
                "not an object",
            ]
        });
        let catalog = parse_catalog(&body).expect("catalog");
        assert_eq!(catalog.len(), 2);
        assert_eq!(catalog.default_model(), Some("auto"));
        let auto = catalog.config("auto").expect("auto");
        assert_eq!(auto["is_vl"], json!(true), "the block is stored verbatim");
        assert!(
            catalog.config("qmodel").is_some(),
            "enable:false stays usable"
        );
        assert!(catalog.config("nope").is_none());
    }

    #[test]
    fn a_body_without_a_chat_array_is_an_error() {
        assert!(parse_catalog(&json!({"models": []})).is_err());
        assert!(parse_catalog(&json!({"chat": []})).is_err());
    }

    #[test]
    fn the_cache_key_separates_accounts_origins_and_tokens() {
        let base = QoderCredentials {
            token: "dt-a".into(),
            user_id: "u-1".into(),
            machine_id: "m".into(),
            name: String::new(),
            email: String::new(),
        };
        let key = cache_key(&base, "https://api3.qoder.sh");
        assert_ne!(key, cache_key(&base, "http://127.0.0.1:9"));
        let mut other_token = base.clone();
        other_token.token = "dt-b".into();
        assert_ne!(key, cache_key(&other_token, "https://api3.qoder.sh"));
        let mut other_user = base.clone();
        other_user.user_id = "u-2".into();
        assert_ne!(key, cache_key(&other_user, "https://api3.qoder.sh"));
        assert_eq!(key.len(), 32);
        assert!(!key.contains("dt-a"), "the token is hashed, not embedded");
    }
}
