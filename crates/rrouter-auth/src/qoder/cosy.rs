//! COSY: Qoder's per-request request signing (AES-128-CBC + RSA PKCS#1 v1.5 + MD5).
//!
//! Every call to `api3.qoder.sh` carries a fingerprint of the client and a signature over
//! the *encoded* body bytes. Nothing here is a bearer token in the usual sense: the device
//! token travels **inside** an AES blob, the AES key travels RSA-encrypted, and the MD5
//! signature ties all of it to the exact request.
//!
//! Per request:
//!
//! 1. `aesKey` = the first 16 characters of a fresh UUID (hyphens included) — 16 ASCII
//!    bytes, which is exactly an AES-128 key. The IV is the key itself.
//! 2. `info` = AES-128-CBC(PKCS#7(`{"uid", "security_oauth_token", "name", "aid", "email"}`)),
//!    base64.
//! 3. `cosyKey` = RSA PKCS#1 v1.5(`aesKey`), base64.
//! 4. `payload` = base64(`{"version":"v1","requestId","info","cosyVersion","ideVersion"}`).
//! 5. `sig` = md5(`payload` ⧺ `\n` ⧺ `cosyKey` ⧺ `\n` ⧺ timestamp ⧺ `\n` ⧺ body ⧺ `\n` ⧺ sigPath),
//!    where `timestamp` is unix seconds and `sigPath` is the URL path with `/algo` removed.
//! 6. `Authorization: Bearer COSY.<payload>.<sig>` plus ~17 `Cosy-*` headers, including the
//!    body's own MD5 (`Cosy-Bodyhash`) and length, and a stable machine id.
//!
//! Consequences that shape the callers:
//!
//! - **The signature covers the body bytes.** A retry, a repaired envelope or a fallback
//!   URL must be re-encoded and re-signed; replaying a signature reuses its `requestId` and
//!   the server answers `403` / code `103`.
//! - **`Cosy-Date` is unix seconds.** Two requests in the same second still differ, because
//!   `requestId` and the AES key are random, so the signature is unique per attempt.
//! - **A GET is signed over an empty body** (`Cosy-Bodyhash` = md5 of nothing). The model
//!   catalogue uses that.
//!
//! The RSA public key is the one Qoder IDE v0.9 ships. It is only ever the *public* half;
//! `RROUTER_QODER_RSA_PUBLIC_KEY` replaces it (PEM) for tests and for a deployment that has
//! to follow a key rotation.

use std::sync::LazyLock;

use base64::Engine as _;
use cbc::cipher::block_padding::Pkcs7;
use cbc::cipher::{BlockEncryptMut, KeyIvInit};
use md5::{Digest as _, Md5};
use rsa::pkcs8::DecodePublicKey;
use rsa::PublicKey;
use serde_json::json;
use uuid::Uuid;

use rrouter_core::{Error, Result};

/// Qoder IDE version the client claims. The backend cross-checks it against the signature.
pub const IDE_VERSION: &str = "1.0.0";
/// `Cosy-Clienttype`: the CLI/agent client.
pub const CLIENT_TYPE: &str = "5";
/// `Cosy-Data-Policy`. qodercli submits `disagree`; anything else changes the account's
/// training opt-in, so it is not configurable here.
pub const DATA_POLICY: &str = "disagree";
/// `Login-Version` header.
pub const LOGIN_VERSION: &str = "v2";
/// `Cosy-Machineos`. Part of the client fingerprint the server validates.
pub const MACHINE_OS: &str = "x86_64_windows";
/// `Cosy-Machinetype`.
pub const MACHINE_TYPE: &str = "5";
/// `Cosy-Clientip`. Sent as a constant even when the request comes from elsewhere.
pub const CLIENT_IP: &str = "127.0.0.1";

/// PEM public key extracted from Qoder IDE v0.9 and used by qodercli for the AES key wrap.
pub const RSA_PUBLIC_KEY_PEM: &str = "-----BEGIN PUBLIC KEY-----
MIGfMA0GCSqGSIb3DQEBAQUAA4GNADCBiQKBgQDA8iMH5c02LilrsERw9t6Pv5Nc
4k6Pz1EaDicBMpdpxKduSZu5OANqUq8er4GM95omAGIOPOh+Nx0spthYA2BqGz+l
6HRkPJ7S236FZz73In/KVuLnwI8JJ2CbuJap8kvheCCZpmAWpb/cPx/3Vr/J6I17
XcW+ML9FoCI6AOvOzwIDAQAB
-----END PUBLIC KEY-----";

/// Env override for [`RSA_PUBLIC_KEY_PEM`], for tests and key rotations.
pub const ENV_RSA_PUBLIC_KEY: &str = "RROUTER_QODER_RSA_PUBLIC_KEY";

/// Everything one signed request needs. Built once per connection from
/// [`Credentials`](crate::Credentials).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QoderCredentials {
    /// Device access token (`dt-…`). Travels only inside the AES blob.
    pub token: String,
    /// Stable Qoder user id. Sent in clear as `Cosy-User` and inside the AES blob.
    pub user_id: String,
    /// Stable machine id. Qoder tracks queue position per device, so a fresh id per request
    /// makes the caller look like a new device and rejoin the back of the queue.
    pub machine_id: String,
    /// Optional display name, carried in the AES blob.
    pub name: String,
    /// Optional email, carried in the AES blob.
    pub email: String,
}

/// Parse a signer for one connection.
///
/// The operator supplies `credentials.api_key` (or `access_token`) plus
/// `credentials.extra.user_id`; `credentials.extra.machine_id` is optional and derived
/// deterministically from the user id when absent. See the module docs on the provider
/// module for what to paste and where qodercli keeps it.
pub fn signer(source: &crate::credentials::Credentials) -> Result<QoderCredentials> {
    let token = source
        .api_key
        .as_deref()
        .or(source.access_token.as_deref())
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .ok_or_else(|| {
            Error::Unauthorized(
                "qoder connection is missing credentials.api_key (the `dt-…` device token)".into(),
            )
        })?;
    let user_id = source
        .extra
        .get(crate::qoder::USER_ID_KEY)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| {
            Error::Unauthorized(
                "qoder connection is missing credentials.extra.user_id; COSY signing needs it"
                    .into(),
            )
        })?;
    let machine_id = source
        .extra
        .get(crate::qoder::MACHINE_ID_KEY)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| derive_machine_id(user_id));
    Ok(QoderCredentials {
        token: token.to_string(),
        user_id: user_id.to_string(),
        machine_id,
        name: source.account_label.clone().unwrap_or_default(),
        email: source
            .extra
            .get(crate::qoder::EMAIL_KEY)
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string(),
    })
}

/// The header set one signed request carries: `Authorization` first, then the `Cosy-*`
/// fingerprint. Returned as pairs so the caller can merge them into its own header map.
///
/// `body` must be the exact bytes that go on the wire (the obfuscated form); `url` is the
/// full request URL, used for `Cosy-Sigpath`.
pub fn sign_headers(
    body: &[u8],
    url: &str,
    credentials: &QoderCredentials,
) -> Result<Vec<(String, String)>> {
    if credentials.user_id.trim().is_empty() {
        return Err(Error::Unauthorized("qoder cosy: user id is empty".into()));
    }
    if credentials.token.trim().is_empty() {
        return Err(Error::Unauthorized("qoder cosy: token is empty".into()));
    }

    let (cosy_key, info) = encrypt_user_info(credentials)?;
    let timestamp = unix_seconds();
    let request_id = Uuid::new_v4().to_string();
    let payload = json!({
        "version": "v1",
        "requestId": request_id,
        "info": info,
        "cosyVersion": IDE_VERSION,
        "ideVersion": "",
    });
    let payload = base64_encode(payload.to_string().as_bytes());
    let sig_path = rrouter_translate::qoder::sig_path(url);
    let signature = signature_hex(&payload, &cosy_key, &timestamp, body, &sig_path);
    let body_hash = hex(&Md5::digest(body));

    Ok(vec![
        (
            "authorization".to_string(),
            format!("Bearer COSY.{payload}.{signature}"),
        ),
        ("cosy-key".to_string(), cosy_key),
        ("cosy-user".to_string(), credentials.user_id.clone()),
        ("cosy-date".to_string(), timestamp),
        ("cosy-version".to_string(), IDE_VERSION.to_string()),
        ("cosy-machineid".to_string(), credentials.machine_id.clone()),
        (
            "cosy-machinetoken".to_string(),
            credentials.machine_id.clone(),
        ),
        ("cosy-machinetype".to_string(), MACHINE_TYPE.to_string()),
        ("cosy-machineos".to_string(), MACHINE_OS.to_string()),
        ("cosy-clienttype".to_string(), CLIENT_TYPE.to_string()),
        ("cosy-clientip".to_string(), CLIENT_IP.to_string()),
        ("cosy-bodyhash".to_string(), body_hash),
        ("cosy-bodylength".to_string(), body.len().to_string()),
        ("cosy-sigpath".to_string(), sig_path),
        ("cosy-data-policy".to_string(), DATA_POLICY.to_string()),
        // qodercli sends both organization headers empty on a personal account.
        ("cosy-organization-id".to_string(), String::new()),
        ("cosy-organization-tags".to_string(), String::new()),
        ("login-version".to_string(), LOGIN_VERSION.to_string()),
        ("x-request-id".to_string(), Uuid::new_v4().to_string()),
    ])
}

/// `md5(payload \n cosyKey \n timestamp \n body \n sigPath)`, hex.
fn signature_hex(
    payload: &str,
    cosy_key: &str,
    timestamp: &str,
    body: &[u8],
    sig_path: &str,
) -> String {
    let mut hasher = Md5::new();
    hasher.update(payload.as_bytes());
    hasher.update(b"\n");
    hasher.update(cosy_key.as_bytes());
    hasher.update(b"\n");
    hasher.update(timestamp.as_bytes());
    hasher.update(b"\n");
    hasher.update(body);
    hasher.update(b"\n");
    hasher.update(sig_path.as_bytes());
    hex(&hasher.finalize())
}

/// `(base64(RSA(aesKey)), base64(AES-CBC(userInfo)))`.
fn encrypt_user_info(credentials: &QoderCredentials) -> Result<(String, String)> {
    let aes_key = Uuid::new_v4().to_string();
    let aes_key = &aes_key[..16];
    let plaintext = json!({
        "uid": credentials.user_id,
        "security_oauth_token": credentials.token,
        "name": credentials.name,
        "aid": "",
        "email": credentials.email,
    })
    .to_string();
    let info = base64_encode(&aes_cbc_encrypt(plaintext.as_bytes(), aes_key)?);
    let wrapped = rsa_public_key()?
        .pkcs1v15_encrypt(aes_key.as_bytes())
        .map_err(|e| Error::Translation(format!("qoder cosy: rsa encrypt failed: {e}")))?;
    Ok((base64_encode(&wrapped), info))
}

/// AES-128-CBC with PKCS#7 padding, key and IV both the 16 key characters.
fn aes_cbc_encrypt(plaintext: &[u8], key: &str) -> Result<Vec<u8>> {
    if key.len() != 16 {
        return Err(Error::Translation(format!(
            "qoder cosy: aes key must be 16 bytes, got {}",
            key.len()
        )));
    }
    let key = key.as_bytes();
    Ok(cbc::Encryptor::<aes::Aes128>::new_from_slices(key, key)
        .map_err(|e| Error::Translation(format!("qoder cosy: aes init failed: {e}")))?
        .encrypt_padded_vec_mut::<Pkcs7>(plaintext))
}

/// Public key for the AES wrap: the env override wins, then the built-in constant.
///
/// Parsed once per process. A test that swaps the key must be the only reader in its
/// process before the first signature (integration tests are their own process, so the
/// signing round-trip in `tests/qoder_cosy.rs` is unaffected by the unit tests here).
fn rsa_public_key() -> Result<&'static PublicKey> {
    static KEY: LazyLock<Result<PublicKey, String>> = LazyLock::new(|| {
        let env = std::env::var(ENV_RSA_PUBLIC_KEY).ok();
        let pem = env
            .as_deref()
            .map(str::trim)
            .filter(|pem| !pem.is_empty())
            .unwrap_or(RSA_PUBLIC_KEY_PEM);
        PublicKey::from_public_key_pem(pem)
            .map_err(|e| format!("qoder cosy: public key is not PEM: {e}"))
    });
    match &*KEY {
        Ok(key) => Ok(key),
        Err(message) => Err(Error::Translation(message.clone())),
    }
}

/// Machine id derived from the user id, formatted as a UUID so it is indistinguishable from
/// a generated one. Stable without extra storage: a new device rejoins the model queue.
pub fn derive_machine_id(seed: &str) -> String {
    let digest = sha2::Sha256::digest(format!("qoder-machine\0{seed}").as_bytes());
    let hex = hex(&digest);
    format!(
        "{}-{}-5{}-a{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[13..16],
        &hex[17..20],
        &hex[20..32]
    )
}

fn unix_seconds() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
        .to_string()
}

fn base64_encode(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> QoderCredentials {
        QoderCredentials {
            token: "dt-test-token".into(),
            user_id: "u-123".into(),
            machine_id: "11111111-2222-5333-a444-555555555555".into(),
            name: "Test".into(),
            email: "test@example.com".into(),
        }
    }

    #[test]
    fn headers_carry_a_self_consistent_signature() {
        let body = b"obfuscated-body";
        let url = rrouter_translate::qoder::chat_url(rrouter_translate::qoder::DEFAULT_BASE);
        let headers = sign_headers(body, &url, &fixture()).expect("sign");
        let get = |name: &str| {
            headers
                .iter()
                .find(|(header, _)| header == name)
                .map(|(_, value)| value.clone())
                .unwrap_or_else(|| panic!("missing {name}"))
        };

        assert_eq!(get("cosy-user"), "u-123");
        assert_eq!(get("cosy-bodylength"), body.len().to_string());
        assert_eq!(get("cosy-bodyhash"), hex(&Md5::digest(body)));
        assert_eq!(
            get("cosy-sigpath"),
            "/api/v2/service/pro/sse/agent_chat_generation"
        );
        let authorization = get("authorization");
        let payload = authorization
            .strip_prefix("Bearer COSY.")
            .expect("cosy authorization")
            .split('.')
            .next()
            .expect("payload");
        let expected = signature_hex(
            payload,
            &get("cosy-key"),
            &get("cosy-date"),
            body,
            &get("cosy-sigpath"),
        );
        assert!(authorization.ends_with(&expected));

        // Every attempt must be able to differ: the signature is unique per call.
        let again = sign_headers(body, &url, &fixture()).expect("sign");
        assert_ne!(again[0].1, authorization);
    }

    #[test]
    fn a_missing_credential_is_a_configuration_error() {
        let mut source = crate::credentials::Credentials {
            api_key: Some("dt-x".into()),
            ..Default::default()
        };
        assert!(signer(&source).is_err(), "user_id is required");

        source.extra.insert("user_id".into(), json!("u-1"));
        let parsed = signer(&source).expect("credentials");
        assert_eq!(parsed.machine_id, derive_machine_id("u-1"));
        assert_eq!(
            parsed.machine_id.len(),
            36,
            "a derived machine id is uuid-shaped"
        );
    }

    #[test]
    fn derived_machine_ids_are_stable_and_uuid_shaped() {
        let first = derive_machine_id("user-a");
        assert_eq!(first, derive_machine_id("user-a"));
        assert_ne!(first, derive_machine_id("user-b"));
        let parts: Vec<&str> = first.split('-').collect();
        assert_eq!(parts.len(), 5);
        assert_eq!(parts[2].len(), 4);
        assert!(parts[2].starts_with('5'));
        assert!(parts[3].starts_with('a'));
    }

    #[test]
    fn sig_path_strips_algo_and_query() {
        let path = rrouter_translate::qoder::sig_path;
        assert_eq!(
            path("https://api3.qoder.sh/algo/api/v2/model/list"),
            "/api/v2/model/list"
        );
        assert_eq!(
            path("https://api3.qoder.sh/algo/api/v2/service/pro/sse/agent_chat_generation?Encode=1&a=b"),
            "/api/v2/service/pro/sse/agent_chat_generation"
        );
        assert_eq!(
            path("https://api3.qoder.sh/api/v2/model/list"),
            "/api/v2/model/list"
        );
        assert_eq!(path(""), "/");
    }
}
