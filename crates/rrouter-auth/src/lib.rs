//! Provider registry, credential material, and mutex-locked token refresh.
//!
//! Layer map:
//! - [`registry`]: static `const` table of provider transports (URL, dialect, auth style,
//!   mandatory headers) plus the refresh recipe. No I/O.
//! - [`credentials`]: plaintext [`Credentials`] — the model of the encrypted blob
//!   `rrouter-store` persists.
//! - [`fresh`]: [`Refresher`] + [`CredentialStore`]. Guarantees exactly one refresh per
//!   credential under concurrency (single-use refresh tokens would otherwise self-destruct).
//! - [`headers`]: [`build_headers`] / [`merge_client_headers`] / [`resolve_base_url`] for
//!   one outbound call.
//! - [`manifest`]: the reviewed community-provider contract, compiled by `build.rs` (the
//!   same file, `include!`d, so tests and build share one validator).
//! - [`query`]: [`with_query`], the transport's static query parameters applied to a URL.
//! - [`endpoint`]: [`capability_endpoint`], a reviewed capability URL resolved for one
//!   connection (override origin, manifest query).
//! - [`base_url`]: [`effective_base_url`], the per-account endpoint — and the refusal when a
//!   provider's declared host is only a placeholder (`RESOURCE.openai.azure.com`).

pub mod antigravity;
pub mod base_url;
pub mod credentials;
pub mod endpoint;
pub mod flows;
pub mod fresh;
pub mod headers;
pub mod loopback;
pub mod manifest;
pub mod overrides;
pub mod pkce;
pub mod query;
pub mod registry;

pub use base_url::{effective_base_url, validate_base_url_override};
pub use credentials::Credentials;
pub use endpoint::capability_endpoint;
pub use flows::{Challenge, OAuthFlows, SignedIn};
pub use fresh::{CredentialStore, Refresher, GITHUB_COPILOT_TOKEN_URL, LEAD_SECS};
pub use headers::{build_headers, merge_client_headers, resolve_base_url};
pub use loopback::LoopbackServer;
pub use overrides::{invalid_overrides, override_for, OverrideKind};
pub use query::with_query;
pub use registry::{
    all, get, is_oauth, login_flow_for, AuthStyle, DeviceFlow, LoginFlow, PkceFlow, ProviderKind,
    ProviderSpec, RefreshBody, RefreshSpec, PROVIDERS,
};

/// Serializes tests that mutate process environment. Rust tests share one process, and
/// the Google overrides are read per call, so a concurrent set/remove would make those
/// tests observe another test's value.
#[cfg(test)]
pub(crate) static TEST_ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());
