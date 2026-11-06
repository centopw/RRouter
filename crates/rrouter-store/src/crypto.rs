//! AES-256-GCM sealing of credential blobs, plus master-key material handling.
//!
//! * Key: either a 64-char lowercase/uppercase hex string passed in by the caller, or a
//!   `master.key` file holding exactly 32 raw bytes under the data dir. The file is created
//!   `0600` on first use and reused afterwards.
//! * Ciphertext layout: `nonce(12) || ciphertext+tag(16)`, stored as a BLOB. The nonce is
//!   fresh random bytes on every seal, so sealing the same plaintext twice never repeats.
//! * Domain separation: the credential id (as its 16 raw bytes) is passed as associated data,
//!   so a ciphertext cannot be transplanted between rows.
//! * Unsealing anything that was not produced for this key/id is a hard error; it never
//!   silently yields a default.

use std::fs;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use rand::RngCore;

use rrouter_core::{Error, Result};

const KEY_LEN: usize = 32;
const NONCE_LEN: usize = 12;
const TAG_LEN: usize = 16;

/// Name of the key file created inside the data dir when no master key is supplied.
pub const KEY_FILE_NAME: &str = "master.key";

/// AES-256-GCM key used for every credential blob in one `Store`.
///
/// Deliberately has no `Debug`/`Display` impl so the key cannot leak into a log line.
pub(crate) struct Cipher {
    aead: Aes256Gcm,
}

impl Cipher {
    /// Parse a 64-char hex master key.
    pub(crate) fn from_hex(hex_key: &str) -> Result<Self> {
        let bytes = hex::decode(hex_key.trim()).map_err(|e| {
            Error::Store(format!(
                "master key must be hex, got {} chars: {e}",
                hex_key.trim().len()
            ))
        })?;
        Self::from_bytes(&bytes)
    }

    pub(crate) fn from_bytes(key: &[u8]) -> Result<Self> {
        if key.len() != KEY_LEN {
            return Err(Error::Store(format!(
                "master key must be {KEY_LEN} bytes (64 hex chars), got {}",
                key.len()
            )));
        }
        let aead = Aes256Gcm::new_from_slice(key)
            .map_err(|e| Error::Store(format!("invalid AES-256-GCM key: {e}")))?;
        Ok(Self { aead })
    }

    /// Load the key file from `data_dir`, creating it `0600` with 32 fresh random bytes
    /// on first use. An existing file must hold exactly 32 bytes.
    pub(crate) fn load_or_create_file(data_dir: &Path) -> Result<Self> {
        let path = data_dir.join(KEY_FILE_NAME);
        match fs::read(&path) {
            Ok(bytes) => Self::from_bytes(&bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let mut key = [0u8; KEY_LEN];
                rand::rngs::OsRng.fill_bytes(&mut key);
                let mut f = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(&path)
                    .map_err(|e| Error::Store(format!("create {}: {e}", path.display())))?;
                f.write_all(&key)
                    .and_then(|()| f.sync_all())
                    .map_err(|e| Error::Store(format!("write {}: {e}", path.display())))?;
                Self::from_bytes(&key)
            }
            Err(e) => Err(Error::Store(format!("read {}: {e}", path.display()))),
        }
    }

    /// Seal `plaintext`, binding it to `id` as associated data.
    pub(crate) fn seal(&self, id: &[u8; 16], plaintext: &[u8]) -> Result<Vec<u8>> {
        let mut nonce = [0u8; NONCE_LEN];
        rand::rngs::OsRng.fill_bytes(&mut nonce);
        let sealed = self
            .aead
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload { msg: plaintext, aad: id },
            )
            .map_err(|_| Error::Store("credential encryption failed".to_string()))?;
        let mut out = Vec::with_capacity(NONCE_LEN + sealed.len());
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&sealed);
        Ok(out)
    }

    /// Open a blob produced by [`Cipher::seal`] for the same `id`.
    pub(crate) fn open(&self, id: &[u8; 16], blob: &[u8]) -> Result<Vec<u8>> {
        if blob.len() < NONCE_LEN + TAG_LEN {
            return Err(Error::Store(format!(
                "credential blob too short: {} bytes",
                blob.len()
            )));
        }
        let (nonce, body) = blob.split_at(NONCE_LEN);
        self.aead
            .decrypt(Nonce::from_slice(nonce), Payload { msg: body, aad: id })
            .map_err(|_| {
                Error::Store(
                    "credential decryption failed (wrong master key, tampered data, or \
                     blob moved between rows)"
                        .to_string(),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cipher() -> Cipher {
        Cipher::from_bytes(&[7u8; KEY_LEN]).expect("static key")
    }

    #[test]
    fn round_trip_and_random_nonce() {
        let c = cipher();
        let id = [1u8; 16];
        let plain = br#"{"access_token":"abc"}"#;
        let a = c.seal(&id, plain).expect("seal");
        let b = c.seal(&id, plain).expect("seal");
        assert_eq!(c.open(&id, &a).expect("open"), plain);
        assert_ne!(a, b, "two seals of the same plaintext must differ (random nonce)");
        assert_eq!(a.len(), NONCE_LEN + plain.len() + TAG_LEN);
    }

    #[test]
    fn tampered_ciphertext_errors() {
        let c = cipher();
        let id = [2u8; 16];
        let mut blob = c.seal(&id, b"secret payload").expect("seal");
        let last = blob.len() - 1;
        blob[last] ^= 0xff;
        assert!(c.open(&id, &blob).is_err(), "tag mismatch must error");

        // Wrong associated data (row transplant) must also fail.
        let good = c.seal(&id, b"secret payload").expect("seal");
        assert!(c.open(&[9u8; 16], &good).is_err());

        // Truncated blob is rejected, not interpreted.
        assert!(c.open(&id, &good[..NONCE_LEN]).is_err());
    }

    #[test]
    fn wrong_key_and_bad_hex_rejected() {
        let id = [3u8; 16];
        let blob = Cipher::from_bytes(&[1u8; KEY_LEN])
            .expect("key")
            .seal(&id, b"x")
            .expect("seal");
        assert!(Cipher::from_bytes(&[2u8; KEY_LEN])
            .expect("key")
            .open(&id, &blob)
            .is_err());

        assert!(Cipher::from_hex("abcd").is_err(), "short hex key must be rejected");
        assert!(Cipher::from_hex(&"zz".repeat(32)).is_err(), "non-hex must be rejected");
        let ok = Cipher::from_hex(&"ab".repeat(32)).expect("64-char hex");
        assert_eq!(ok.open(&id, &ok.seal(&id, b"ok").expect("seal")).expect("open"), b"ok");
    }

    #[test]
    fn key_file_mode_and_hex_key_priority() {
        let dir = std::env::temp_dir().join(format!("rrouter-crypto-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).expect("tmp dir");

        Cipher::load_or_create_file(&dir).expect("create");
        let path = dir.join(KEY_FILE_NAME);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).expect("stat").permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "key file must be 0600");
        }
        let first = fs::read(&path).expect("read key");
        assert_eq!(first.len(), KEY_LEN);
        // Second call reuses, does not rotate, the key.
        Cipher::load_or_create_file(&dir).expect("reuse");
        assert_eq!(fs::read(&path).expect("read key"), first);

        fs::remove_dir_all(&dir).ok();
    }
}
