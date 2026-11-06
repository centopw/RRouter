//! Routing for RRouter: client model string → ordered attempt candidates, then
//! fallback execution with a per-connection circuit breaker.
//!
//! No HTTP lives here. The gateway supplies the transport (`send` / `open`
//! closures) and this crate decides *which* connection to hit, *when* it is
//! allowed to, and *when to stop*.
//!
//! Layering:
//! - [`Catalog`] — static "who serves what".
//! - [`Planner`] — [`Store`](rrouter_store::Store) reads + catalog + [`Strategy`] → [`Plan`].
//! - [`Breaker`] — per-connection closed/open/half-open state.
//! - [`Dispatcher`] — drives a plan; `run` for JSON, `run_streaming` for bodies.

pub mod breaker;
pub mod candidate;
pub mod catalog;
pub mod dispatch;
pub mod planner;
pub mod stream_dispatch;

pub use breaker::{Breaker, BreakerState};
pub use candidate::{Candidate, Plan};
pub use catalog::{Catalog, ModelInfo};
pub use dispatch::{AttemptOutcome, Dispatcher, DEFAULT_MAX_ATTEMPTS};
pub use planner::{Planner, Strategy};
pub use stream_dispatch::ByteStream;
