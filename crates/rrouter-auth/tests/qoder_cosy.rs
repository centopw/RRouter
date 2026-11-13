//! Spike: one COSY-signed Qoder request, verified end to end against a mock server.
//!
//! The mock holds the RSA **private** key and undoes everything the client does, so a bug in
//! any single piece fails here instead of at the real `api3.qoder.sh` (which these tests
//! never contact):
//!
//! 1. `Authorization: Bearer COSY.<payload>.<sig>` is split, and `payload` is base64-decoded
//!    to `{version, requestId, info, cosyVersion, ideVersion}`.
//! 2. `Cosy-Key` is RSA-decrypted into the AES key, and `info` is AES-CBC-decrypted with it
//!    (key = IV, PKCS#7), yielding the user id and the device token.
//! 3. The signature is recomputed over `payload \n cosyKey \n timestamp \n body \n sigPath`
//!    from the bytes actually received, so a stale or plaintext-body signature fails.
//! 4. `Cosy-Bodyhash` / `Cosy-Bodylength` must match those same bytes.
//! 5. The body is de-shuffled through the custom alphabet and must parse as the JSON the
//!    client built — proving the obfuscation is reversible and nothing was lost.
//!
//! The last test tampers with the body after signing and asserts the mock refuses it, so the
//! checks above cannot silently become vacuous.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use cbc::cipher::block_padding::Pkcs7;
use cbc::cipher::{BlockDecryptMut, KeyIvInit};
use md5::Digest as _;
use rsa::pkcs8::{DecodePrivateKey, EncodePublicKey, LineEnding};
use rsa::PrivateKey;
use serde_json::{json, Value};

use rrouter_auth::qoder::{cosy, encoding, QoderCredentials};
use rrouter_core::ChatRequest;
use rrouter_translate::qoder::{self, Frame, QoderAgentDecoder};

/// Test-only RSA key pair. It is a fixture, not a secret: the real COSY public key is
/// [`cosy::RSA_PUBLIC_KEY_PEM`] and is installed into the mock's counterpart at runtime via
/// [`cosy::ENV_RSA_PUBLIC_KEY`].
const TEST_PRIVATE_KEY_PEM: &str = "-----BEGIN PRIVATE KEY-----
MIICdQIBADANBgkqhkiG9w0BAQEFAASCAl8wggJbAgEAAoGBAM1EGCqcd8r2AiuH
TLYaSsf+W592nJXtwA2D4bYZ0Ddw08zo91EMgV+2AR0Hpca11cjUWy2Jt4rynjhl
JvqYSSn9wambGvC9XFFAnJYWnWy9yfZoXleYuzkDlhiyP2hTCcNuwYFX8mU9QQA0
tUKtN+/eo3RVax6EVNr//bZaLA4JAgMBAAECgYAvUbdQe2mSPEwbxMEGaeE7YYO1
qx5F4d0YQILT+/doEHaDEmGRKZTZf5eaHZ01xc1W0r2CLfFF8tBOp+LoehrDTTGg
Hry/O929J1rR11hWD/Yq+BQ6TnlJciK8bRVDivYFJjT8axyHq3cMp74ZahJgZ7qv
UPIfM6k2Ik76NNaREQJBAPxxaWWu5/D33/HxcVdKhq3JNA/hgZPtFjBmrd51xmUw
POs9fUTR2drcAURjvaNzZ0J+ynRG4SdIBNKyryI1qqsCQQDQKIKgKKEE1eMFd9CY
7JjKL+Ip3pmmZXy1kBgSlv3D756SQVhSapRUyu7T8LZPcp1zOGdCYSuTRB0U37nR
iSobAkAqNJfnfTegDQEvBSvq6TXdJk+C/DPa1H+qNfCJye0Si98TgWNJN0CSG8hG
c9qH0kqIToRwrk0EcUEFLQSk1o4jAkBT47+hJD6gYEK59WuP9eotsdaH4TozITt9
QLEy7L8gs/VIJCWgMWvOMGrH537RzfM+eEDFM7wIAa9mtjOBCrJ5AkAgBQJkHge/
0XVXZVyjG1QiRdCjKfricRahi2NF0iBEPOVG0nitAQjjSwcY0MRkg4WuwV7lCwL1
oJN19WkGC9vl
-----END PRIVATE KEY-----";

fn md5_hex(bytes: &[u8]) -> String {
    md5::Md5::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// What the mock managed to prove about one request.
#[derive(Debug, Default, Clone)]
struct Verified {
    user_id: String,
    token: String,
    sig_path: String,
    machine_id: String,
    body_hash: String,
    body_length: usize,
    envelope: Value,
}

struct Mock {
    key: PrivateKey,
    seen: Mutex<Vec<Verified>>,
}

/// Install the fixture's public half as the signing key. Idempotent, and every test in this
/// binary sets the same value, so the concurrent default test harness cannot observe a
/// different key.
fn install_public_key() -> String {
    let key = PrivateKey::from_pkcs8_pem(TEST_PRIVATE_KEY_PEM).expect("fixture private key");
    let pem = key
        .to_public_key()
        .to_public_key_pem(LineEnding::LF)
        .expect("public pem");
    std::env::set_var(cosy::ENV_RSA_PUBLIC_KEY, &pem);
    pem
}

async fn mock_server() -> (SocketAddr, Arc<Mock>) {
    install_public_key();
    let mock = Arc::new(Mock {
        key: PrivateKey::from_pkcs8_pem(TEST_PRIVATE_KEY_PEM).expect("fixture private key"),
        seen: Mutex::new(Vec::new()),
    });
    let app = axum::Router::new()
        .route(
            "/algo/api/v2/service/pro/sse/agent_chat_generation",
            axum::routing::post(chat),
        )
        .with_state(Arc::clone(&mock));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (addr, mock)
}

async fn chat(State(mock): State<Arc<Mock>>, headers: HeaderMap, body: Bytes) -> Response {
    let header = |name: &str| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string()
    };
    match verify(&mock, &headers, &body) {
        Ok(verified) => {
            mock.seen.lock().expect("lock").push(verified);
            (
                StatusCode::OK,
                [("content-type", "text/event-stream")],
                sse_body(),
            )
                .into_response()
        }
        Err(reason) => (
            StatusCode::BAD_REQUEST,
            format!(
                "mock rejected: {reason} (body {} bytes, hash {})",
                body.len(),
                header("cosy-bodyhash")
            ),
        )
            .into_response(),
    }
}

/// Undo the client's signing. Every failure is a distinct message so a broken piece is
/// obvious from the test output alone.
fn verify(mock: &Mock, headers: &HeaderMap, body: &[u8]) -> Result<Verified, String> {
    let header = |name: &str| -> Result<String, String> {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
            .ok_or_else(|| format!("missing header {name}"))
    };

    let authorization = header("authorization")?;
    let rest = authorization
        .strip_prefix("Bearer COSY.")
        .ok_or("authorization is not a COSY bearer")?;
    let (payload_b64, sig) = rest
        .split_once('.')
        .ok_or("authorization is not COSY.<payload>.<sig>")?;
    let payload: Value = serde_json::from_slice(
        &base64::engine::general_purpose::STANDARD
            .decode(payload_b64)
            .map_err(|e| format!("payload is not base64: {e}"))?,
    )
    .map_err(|e| format!("payload is not json: {e}"))?;
    if payload["version"] != json!("v1") {
        return Err("payload version is not v1".into());
    }
    if payload["requestId"].as_str().unwrap_or_default().is_empty() {
        return Err("payload carries no requestId".into());
    }

    // 2. RSA unwrap the AES key, then AES-decrypt the user blob.
    let cosy_key = header("cosy-key")?;
    let wrapped = base64::engine::general_purpose::STANDARD
        .decode(&cosy_key)
        .map_err(|e| format!("cosy-key is not base64: {e}"))?;
    let aes_key = mock
        .key
        .pkcs1v15_decrypt(&wrapped)
        .map_err(|e| format!("cosy-key does not RSA-decrypt: {e}"))?;
    if aes_key.len() != 16 {
        return Err(format!("aes key is {} bytes, not 16", aes_key.len()));
    }
    let info = payload["info"].as_str().ok_or("payload has no info")?;
    let sealed = base64::engine::general_purpose::STANDARD
        .decode(info)
        .map_err(|e| format!("info is not base64: {e}"))?;
    let plaintext = cbc::Decryptor::<aes::Aes128>::new_from_slices(&aes_key, &aes_key)
        .map_err(|e| format!("aes init: {e}"))?
        .decrypt_padded_vec_mut::<Pkcs7>(&sealed)
        .map_err(|e| format!("info does not AES-decrypt: {e}"))?;
    let user: Value = serde_json::from_slice(&plaintext)
        .map_err(|e| format!("info plaintext is not json: {e}"))?;

    // 3. The signature covers these exact bytes.
    let timestamp = header("cosy-date")?;
    let sig_path = header("cosy-sigpath")?;
    let mut hasher = md5::Md5::new();
    for part in [
        payload_b64.as_bytes(),
        cosy_key.as_bytes(),
        timestamp.as_bytes(),
    ] {
        hasher.update(part);
        hasher.update(b"\n");
    }
    hasher.update(body);
    hasher.update(b"\n");
    hasher.update(sig_path.as_bytes());
    let expected = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    if sig.to_ascii_lowercase() != expected {
        return Err(format!(
            "signature mismatch: got {sig}, expected {expected}"
        ));
    }

    // 4. Body integrity.
    let body_hash = header("cosy-bodyhash")?;
    let digest = md5::Md5::digest(body)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    if body_hash != digest {
        return Err(format!("cosy-bodyhash {body_hash} != md5 {digest}"));
    }
    let body_length: usize = header("cosy-bodylength")?
        .parse()
        .map_err(|_| "cosy-bodylength is not a number")?;
    if body_length != body.len() {
        return Err(format!("cosy-bodylength {body_length} != {}", body.len()));
    }

    // 5. The obfuscation is reversible and lossless.
    let decoded = encoding::decode_body(body).map_err(|e| format!("body does not decode: {e}"))?;
    let envelope: Value =
        serde_json::from_slice(&decoded).map_err(|e| format!("decoded body is not json: {e}"))?;

    Ok(Verified {
        user_id: user["uid"].as_str().unwrap_or_default().to_string(),
        token: user["security_oauth_token"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        sig_path,
        machine_id: header("cosy-machineid")?,
        body_hash,
        body_length,
        envelope,
    })
}

/// Three frames, a heartbeat and a comment: the shapes the decoder must tolerate.
fn sse_body() -> String {
    let chunk = |inner: Value| {
        format!(
            "data: {}\n\n",
            json!({"statusCodeValue": 200, "body": inner.to_string()})
        )
    };
    let mut body = String::new();
    body.push_str(&chunk(json!({
        "id": "chatcmpl-1",
        "model": "auto",
        "choices": [{"index": 0, "delta": {"content": "hello"}}],
    })));
    // A comment heart beat and a blank data line, both of which must be skipped.
    body.push_str(": ping\n\n");
    body.push_str("data:\n\n");
    body.push_str(&chunk(json!({
        "id": "chatcmpl-1",
        "model": "auto",
        "choices": [{"index": 0, "delta": {"content": " world"}}],
    })));
    body.push_str(&chunk(json!({
        "id": "chatcmpl-1",
        "model": "auto",
        "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 5, "completion_tokens": 2, "total_tokens": 7},
    })));
    body.push_str("data: [DONE]\n\n");
    body
}

fn credentials() -> QoderCredentials {
    QoderCredentials {
        token: "dt-test-token".into(),
        user_id: "user-42".into(),
        machine_id: "11111111-2222-5333-a444-555555555555".into(),
        name: "Operator".into(),
        email: "op@example.com".into(),
    }
}

/// The envelope the client builds for one prompt, with a catalog block the upstream would
/// have published.
fn envelope(model: &str) -> Value {
    let mut req = ChatRequest::new(model);
    req.system = Some("You are qodercli.".into());
    req.messages
        .push(rrouter_core::Message::text(rrouter_core::Role::User, "hi"));
    req.max_tokens = Some(64);
    let config = json!({
        "key": model,
        "display_name": "Auto",
        "format": "openai",
        "source": "system",
        "is_reasoning": true,
        "max_input_tokens": 200000,
        "max_output_tokens": 32768,
    });
    let session = qoder::session_id("user-42", model);
    qoder::build_request(
        &req,
        &qoder::Envelope {
            model,
            model_config: &config,
            session_id: &session,
        },
    )
    .expect("envelope")
}

async fn send(url: &str, body: &[u8]) -> reqwest::Response {
    let headers = cosy::sign_headers(body, url, &credentials()).expect("sign");
    let mut request = reqwest::Client::new()
        .post(url)
        .header("content-type", "application/json")
        .header("accept", "text/event-stream")
        // A gzipped body would be validated against its compressed bytes.
        .header("accept-encoding", "identity")
        .body(body.to_vec());
    for (name, value) in headers {
        request = request.header(name, value);
    }
    request.send().await.expect("send")
}

#[tokio::test]
async fn a_signed_request_round_trips_and_its_stream_decodes() {
    let (addr, mock) = mock_server().await;
    let url = qoder::chat_url(&format!("http://{addr}"));
    let built = envelope("auto");
    let wire = encoding::encode_body(&built.to_string().into_bytes());

    let response = send(&url, &wire).await;
    assert_eq!(response.status(), 200);
    let stream = response.bytes().await.expect("body");

    let seen = mock.seen.lock().expect("lock");
    let verified = seen.first().expect("the mock verified one request");
    assert_eq!(verified.user_id, "user-42");
    assert_eq!(verified.token, "dt-test-token");
    assert_eq!(
        verified.sig_path,
        "/api/v2/service/pro/sse/agent_chat_generation"
    );
    assert_eq!(verified.machine_id, credentials().machine_id);
    assert_eq!(verified.body_length, wire.len());
    assert_eq!(verified.body_hash, md5_hex(&wire));
    // What the mock de-shuffled is exactly the envelope that was built.
    assert_eq!(verified.envelope, built);
    assert_eq!(verified.envelope["system"], json!("You are qodercli."));
    assert!(
        verified.envelope["messages"]
            .as_array()
            .expect("messages")
            .iter()
            .all(|message| message["role"] != json!("system")),
        "system was hoisted out of messages"
    );
    assert_eq!(verified.envelope["parameters"]["max_tokens"], json!(64));
    assert_eq!(verified.envelope["business"]["product"], json!("cli"));
    assert_eq!(verified.envelope["model_config"]["key"], json!("auto"));
    drop(seen);

    let mut decoder = QoderAgentDecoder::new("auto");
    let mut events = Vec::new();
    // Chunk boundaries inside a frame must not lose anything.
    for chunk in stream.chunks(17) {
        decoder.push(chunk, &mut events).expect("push");
    }
    decoder.finish(&mut events).expect("finish");

    let text: String = events
        .iter()
        .filter_map(|event| match event {
            rrouter_core::StreamEvent::TextDelta(text) => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "hello world");
    match events.last() {
        Some(rrouter_core::StreamEvent::Done { reason, usage }) => {
            assert_eq!(*reason, rrouter_core::FinishReason::Stop);
            let usage = usage.expect("usage");
            assert_eq!(usage.output_tokens, 2);
        }
        other => panic!("expected one terminal Done, got {other:?}"),
    }
}

#[tokio::test]
async fn every_attempt_is_signed_fresh() {
    let (addr, mock) = mock_server().await;
    let url = qoder::chat_url(&format!("http://{addr}"));
    let built = envelope("auto");
    let wire = encoding::encode_body(&built.to_string().into_bytes());

    let first = cosy::sign_headers(&wire, &url, &credentials()).expect("sign");
    let second = cosy::sign_headers(&wire, &url, &credentials()).expect("sign");
    let authorization = |headers: &[(String, String)]| {
        headers
            .iter()
            .find(|(name, _)| name == "authorization")
            .map(|(_, value)| value.clone())
            .expect("authorization")
    };
    assert_ne!(
        authorization(&first),
        authorization(&second),
        "a replayed signature reuses its requestId and is rejected upstream"
    );

    for _ in 0..2 {
        assert_eq!(send(&url, &wire).await.status(), 200);
    }
    assert_eq!(mock.seen.lock().expect("lock").len(), 2);
}

#[tokio::test]
async fn the_mock_refuses_bytes_the_signature_did_not_cover() {
    let (addr, _mock) = mock_server().await;
    let url = qoder::chat_url(&format!("http://{addr}"));
    let built = envelope("auto");
    let wire = encoding::encode_body(&built.to_string().into_bytes());
    // Sign the real body, then ship a different one: the point of signing.
    let headers = cosy::sign_headers(&wire, &url, &credentials()).expect("sign");
    let mut tampered = wire.clone();
    tampered[0] = if tampered[0] == b'a' { b'b' } else { b'a' };
    let mut request = reqwest::Client::new()
        .post(&url)
        .header("content-type", "application/json")
        .body(tampered);
    for (name, value) in headers {
        request = request.header(name, value);
    }
    assert_eq!(request.send().await.expect("send").status(), 400);
}

/// The classifier is the only place that turns an upstream error envelope into a gateway
/// error; the queue and quota paths are what the dispatcher acts on.
#[test]
fn error_envelopes_map_to_the_gateway_contract() {
    let envelope = |body: Value| {
        format!(
            "{}",
            json!({"statusCodeValue": 500, "body": body.to_string()})
        )
    };

    // 10605: a capacity signal. Retryable, with the server's own interval.
    let queued = envelope(json!({
        "code": "10605",
        "message": "{\"isQueued\":true,\"queueType\":\"p3\",\"retryAfterSeconds\":42,\"subCode\":\"RESOURCE_LIMIT\"}",
    }));
    match qoder::classify(&queued) {
        Frame::Failed(rrouter_core::Error::Upstream {
            status, payload, ..
        }) => {
            assert_eq!(status, 503);
            assert_eq!(payload.expect("payload")["retry_after"], json!(42.0));
        }
        other => panic!("expected a retryable queue error, got {other:?}"),
    }

    // 112: quota exhausted, on the gateway's existing 429 path.
    let quota = envelope(json!({"code": "112", "pricingUrl": "https://qoder.com/pricing"}));
    match qoder::classify(&quota) {
        Frame::Failed(rrouter_core::Error::Upstream { status, .. }) => assert_eq!(status, 429),
        other => panic!("expected a quota error, got {other:?}"),
    }
    let daily = envelope(json!({"code": "110", "message": "daily limit"}));
    match qoder::classify(&daily) {
        Frame::Failed(rrouter_core::Error::Upstream { status, .. }) => assert_eq!(status, 429),
        other => panic!("expected a quota error, got {other:?}"),
    }

    // Anything else keeps its own status, and a non-error envelope yields content.
    let broken = envelope(json!({"message": "boom"}));
    match qoder::classify(&broken) {
        Frame::Failed(rrouter_core::Error::Upstream { status, .. }) => assert_eq!(status, 500),
        other => panic!("expected a 500, got {other:?}"),
    }
    assert!(matches!(
        qoder::classify(r#"{"statusCodeValue":200,"body":"{\"choices\":[]}"}"#),
        Frame::Payload(_)
    ));
    // `classify` takes the payload, not the whole `data:` line.
    assert!(matches!(qoder::classify("[DONE]"), Frame::Done));
    assert!(matches!(
        qoder::classify("{\"choices\":[]}"),
        Frame::Payload(_)
    ));
    assert!(matches!(qoder::classify(": keepalive"), Frame::Empty));
}
