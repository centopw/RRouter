//! Temporary local mirror of `rrouter_auth::Credentials` + `CredentialStore`.
//!
//! `rrouter-auth` (task T3) is still a placeholder crate at the time this crate was
//! written, so these definitions are copied verbatim from `.orca-specs/T3-auth.md`.
//! They exist ONLY to keep this crate compiling and testable. When T3 lands:
//!
//! 1. delete this module and its `lib.rs` wiring,
//! 2. change `use crate::testkit::{Credentials, CredentialStore}` to
//!    `use rrouter_auth::{Credentials, CredentialStore}` (both re-exported from
//!    `rrouter_auth`'s root),
//! 3. drop the `serde_json`-only `extra` handling if T3 differs.
//!
//! No persistence or crypto logic lives here; `Store` behaves identically either way.

use std::future::Future;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use uuid::Uuid;

use rrouter_core::Result;

/// Plaintext model of the encrypted blob the store holds.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Credentials {
    pub access_token: Option<String>,
    pub refresh_token: Option<String>,
    pub api_key: Option<String>,
    pub id_token: Option<String>,
    /// Unix seconds.
    pub expires_at: Option<i64>,
    /// GitHub Copilot short-lived token.
    pub copilot_token: Option<String>,
    pub copilot_expires_at: Option<i64>,
    pub account_label: Option<String>,
    pub base_url_override: Option<String>,
    /// Provider-specific passthrough data.
    pub extra: Map<String, Value>,
}

/// Read/merge access to stored credentials, used by the refresh path.
pub trait CredentialStore: Send + Sync {
    /// Read current credentials. Must be cheap (called on every request).
    fn load(&self, id: Uuid) -> impl Future<Output = Result<Credentials>> + Send;
    /// Atomically merge these fields into the stored row; returns the merged value.
    fn merge(
        &self,
        id: Uuid,
        patch: Credentials,
    ) -> impl Future<Output = Result<Credentials>> + Send;
}
