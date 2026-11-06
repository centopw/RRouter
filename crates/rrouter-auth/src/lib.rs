//! Provider registry, credential material, and mutex-locked token refresh.
//!
//! Layer map:
//! - [`registry`]: static `const` table of provider transports (URL, dialect, auth style,
//!   mandatory headers) plus the refresh recipe. No I/O.
//! - [`credentials`]: plaintext [`Credentials`] — the model of the encrypted blob
//!   `rrouter-store` persists.
//! - [`fresh`]: [`Refresher`] + [`CredentialStore`]. Guarantees exactly one refresh per
//!   credential under concurrency (single-use refresh tokens would otherwise self-destruct).
//! - [`headers`]: [`build_headers`] / [`resolve_base_url`] for one outbound call.

pub mod credentials;
pub mod fresh;
pub mod headers;
pub mod registry;

pub use credentials::Credentials;
pub use fresh::{CredentialStore, Refresher, GITHUB_COPILOT_TOKEN_URL, LEAD_SECS};
pub use headers::{build_headers, resolve_base_url};
pub use registry::{
    get, is_oauth, AuthStyle, ProviderKind, ProviderSpec, RefreshBody, RefreshSpec, PROVIDERS,
};
