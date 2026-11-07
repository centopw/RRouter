//! PKCE (RFC 7636) primitives and authorization-URL construction.
//!
//! The verifier is 32 bytes of CSPRNG output, base64url-encoded without padding
//! (43 chars, inside the RFC's 43–128 range); the challenge is `SHA-256(verifier)`
//! base64url-encoded. Both are byte-exact requirements of the providers, so the
//! encoding here is exercised by tests against the RFC's own example vector.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use rand::RngCore;
use sha2::{Digest, Sha256};

/// A fresh code verifier: 32 random bytes, base64url, no padding.
pub fn generate_verifier() -> String {
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// `BASE64URL(SHA256(ASCII(verifier)))`, per RFC 7636 §4.2.
pub fn challenge_for(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(digest)
}

/// A fresh CSRF `state` value: 24 random bytes, base64url.
pub fn generate_state() -> String {
    let mut bytes = [0u8; 24];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Build the provider's authorization URL.
///
/// `extra_params` are filtered so a provider-specific parameter can never collide
/// with a security-critical one. Dropping the collision (rather than appending a
/// duplicate) matters because RFC 6749 does not define which of two repeated query
/// parameters an authorization server honours.
#[allow(clippy::too_many_arguments)]
pub fn authorize_url(
    authorize_url: &str,
    client_id: &str,
    redirect_uri: &str,
    scope: &str,
    state: &str,
    challenge: &str,
    extra_params: &[(&str, &str)],
) -> String {
    let mut params: Vec<(&str, String)> = Vec::with_capacity(9 + extra_params.len());
    const RESERVED: &[&str] = &[
        "client_id",
        "response_type",
        "redirect_uri",
        "scope",
        "code_challenge",
        "code_challenge_method",
        "state",
    ];
    for (k, v) in extra_params {
        if !RESERVED.iter().any(|r| r.eq_ignore_ascii_case(k)) {
            params.push((k, (*v).to_string()));
        }
    }
    params.push(("client_id", client_id.to_string()));
    params.push(("response_type", "code".to_string()));
    params.push(("redirect_uri", redirect_uri.to_string()));
    params.push(("scope", scope.to_string()));
    params.push(("code_challenge", challenge.to_string()));
    params.push(("code_challenge_method", "S256".to_string()));
    params.push(("state", state.to_string()));

    let query = params
        .iter()
        .map(|(k, v)| format!("{}={}", k, percent_encode(v)))
        .collect::<Vec<_>>()
        .join("&");
    let sep = if authorize_url.contains('?') {
        '&'
    } else {
        '?'
    };
    format!("{authorize_url}{sep}{query}")
}

/// Minimal RFC 3986 query-component encoding. `application/x-www-form-urlencoded`
/// would encode a space as `+`, which OAuth scopes use as the separator; providers
/// expect `%20` there.
fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 7636 Appendix B test vector.
    #[test]
    fn matches_the_rfc_test_vector() {
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            challenge_for(verifier),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn verifier_shape_is_rfc_compliant() {
        let v = generate_verifier();
        assert!((43..=128).contains(&v.len()), "len {}", v.len());
        assert!(v
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~')));
        assert_ne!(v, generate_verifier(), "verifiers must not repeat");
    }

    #[test]
    fn state_is_random_and_url_safe() {
        let a = generate_state();
        let b = generate_state();
        assert_ne!(a, b);
        assert!(a
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_')));
    }

    #[test]
    fn scopes_encode_as_percent20_not_plus() {
        let url = authorize_url(
            "https://example.test/authorize",
            "client",
            "http://127.0.0.1:8080/callback",
            "openid profile email",
            "st",
            "ch",
            &[],
        );
        assert!(url.contains("scope=openid%20profile%20email"), "{url}");
        assert!(!url.contains('+'), "spaces must not become '+'");
        assert!(url.contains("code_challenge_method=S256"));
    }

    #[test]
    fn security_params_cannot_be_overridden_by_extra_params() {
        let url = authorize_url(
            "https://example.test/authorize",
            "client",
            "http://127.0.0.1/cb",
            "s",
            "REAL_STATE",
            "REAL_CHALLENGE",
            &[("state", "attacker"), ("code_challenge", "attacker")],
        );
        // A colliding extra param is dropped entirely: a repeated query parameter
        // has no defined winner, so it must never be emitted.
        assert_eq!(url.matches("state=").count(), 1, "{url}");
        assert!(url.contains("state=REAL_STATE"));
        assert_eq!(url.matches("code_challenge=").count(), 1, "{url}");
        assert!(url.contains("code_challenge=REAL_CHALLENGE"));
        assert!(!url.contains("attacker"), "{url}");
    }

    #[test]
    fn redirect_uri_is_encoded() {
        let url = authorize_url(
            "https://x.test/a",
            "c",
            "http://127.0.0.1:1455/auth/callback",
            "s",
            "st",
            "ch",
            &[],
        );
        assert!(url.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A1455%2Fauth%2Fcallback"));
    }
}
