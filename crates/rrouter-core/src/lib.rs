//! Shared types for RRouter: protocol dialects, errors, and the canonical
//! request/response model that translators normalize to.

pub mod dialect;
pub mod error;
pub mod sse;
pub mod types;

pub use dialect::Dialect;
pub use error::{Error, Result};
pub use sse::SseCodec;
pub use types::*;
