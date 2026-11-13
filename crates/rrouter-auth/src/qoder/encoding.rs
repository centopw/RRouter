//! Qoder's request-body obfuscation.
//!
//! qodercli never posts plaintext JSON to the inference endpoint. The body is:
//!
//! 1. base64-encoded with the standard alphabet,
//! 2. rearranged — the string is split into thirds and reordered `[tail][mid][head]`,
//! 3. substituted character by character through a custom 64-symbol alphabet.
//!
//! The result is sent with `&Encode=1` appended to the URL so the server reverses it. It is
//! obfuscation, not encryption (there is no key), and it exists so an intermediary WAF has
//! nothing to pattern-match on.
//!
//! [`decode_body`] is the exact inverse. It is public because it is the only way to inspect
//! a captured qodercli request, and because the mock server in
//! `tests/qoder_cosy.rs` needs it to prove a signed body round-trips.

use base64::Engine as _;
use rrouter_core::{Error, Result};

/// Standard base64 alphabet, the intermediate form step 2 rearranges.
const STD_ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Qoder's substitution alphabet, indexed by the standard alphabet's position.
const CUSTOM_ALPHABET: &[u8; 64] =
    b"_doRTgHZBKcGVjlvpC,@aFSx#DPuNJme&i*MzLOEn)sUrthbf%Y^w.(kIQyXqWA!";

/// `=` padding is substituted too, so the encoded body holds no standard base64 padding.
const STD_PAD: u8 = b'=';
const CUSTOM_PAD: u8 = b'$';

/// Obfuscate `plain` for the wire. The bytes are what the signature covers and what the
/// server decrypts, so callers must sign *these* bytes, never the JSON they came from.
pub fn encode_body(plain: &[u8]) -> Vec<u8> {
    let standard = base64::engine::general_purpose::STANDARD.encode(plain);
    let standard = standard.as_bytes();
    let len = standard.len();
    let third = len / 3;
    let mut out = Vec::with_capacity(len);
    let (head, rest) = standard.split_at(third);
    let (mid, tail) = rest.split_at(len - 2 * third);
    for byte in tail.iter().chain(mid).chain(head) {
        out.push(substitute(*byte));
    }
    out
}

/// Reverse [`encode_body`].
pub fn decode_body(encoded: &[u8]) -> Result<Vec<u8>> {
    let len = encoded.len();
    let third = len / 3;
    // The forward pass emits `[tail][mid][head]`, each `third` long except `mid`, which
    // takes the remainder. Undo it by taking the head off the end and the tail off the
    // front, then reassembling `[head][mid][tail]`.
    let (tail, rest) = encoded.split_at(third);
    let (mid, head) = rest.split_at(rest.len() - third);
    let mut standard = Vec::with_capacity(len);
    for byte in head.iter().chain(mid).chain(tail) {
        standard.push(unsubstitute(*byte)?);
    }
    base64::engine::general_purpose::STANDARD
        .decode(&standard)
        .map_err(|e| Error::Translation(format!("qoder body: not base64 after de-shuffle: {e}")))
}

fn substitute(byte: u8) -> u8 {
    if byte == STD_PAD {
        return CUSTOM_PAD;
    }
    STD_ALPHABET
        .iter()
        .position(|candidate| *candidate == byte)
        .map(|i| CUSTOM_ALPHABET[i])
        .unwrap_or(byte)
}

fn unsubstitute(byte: u8) -> Result<u8> {
    if byte == CUSTOM_PAD {
        return Ok(STD_PAD);
    }
    CUSTOM_ALPHABET
        .iter()
        .position(|candidate| *candidate == byte)
        .map(|i| STD_ALPHABET[i])
        .ok_or_else(|| {
            Error::Translation(format!(
                "qoder body: 0x{byte:02x} is not in the custom alphabet"
            ))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(plain: &[u8]) {
        let encoded = encode_body(plain);
        assert_eq!(decode_body(&encoded).expect("decode"), plain);
        // Nothing standard survives: the point is that a WAF cannot see base64 or JSON.
        assert!(!encoded.contains(&b'='), "padding is substituted");
        assert!(
            !encoded.iter().any(u8::is_ascii_digit),
            "the custom alphabet has no digits: {:?}",
            String::from_utf8_lossy(&encoded)
        );
    }

    #[test]
    fn every_residue_class_round_trips() {
        // The thirds shuffle is length-dependent: cover inputs whose base64 length leaves
        // each possible remainder, including the empty body a GET is signed with.
        for len in 0..40 {
            round_trip(&vec![b'x'; len]);
        }
        round_trip(b"{}");
        round_trip(r#"{"system":"你好","messages":[{"role":"user","content":"hi"}]}"#.as_bytes());
    }

    #[test]
    fn an_unknown_symbol_is_refused() {
        // 0x7f is in neither alphabet: a body that carries it is corrupt.
        assert!(decode_body(&[b'a', b'b', b'c', 0x7f]).is_err());
    }
}
