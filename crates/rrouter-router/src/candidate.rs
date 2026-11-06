//! Attempt candidates: one (provider, model, connection) triple the dispatcher
//! may send to, plus the ordered plan produced for a client model string.

use rrouter_core::Dialect;
use uuid::Uuid;

/// A single upstream attempt target. A provider/model pair can expand to several
/// candidates — one per active connection — which is what makes account-level
/// fallback possible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub provider: String,
    pub model: String,
    pub connection: Uuid,
    /// Lower runs first. Comes from the connection's configured priority.
    pub priority: i32,
    /// Wire dialect this candidate's provider speaks.
    pub dialect: Dialect,
}

/// Ordered attempt list for one client request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// The model string the client asked for, before alias/combo expansion.
    pub model: String,
    pub candidates: Vec<Candidate>,
    pub stream: bool,
}

impl Plan {
    /// True when no connection can serve the requested model.
    pub fn is_empty(&self) -> bool {
        self.candidates.is_empty()
    }

    pub fn len(&self) -> usize {
        self.candidates.len()
    }
}
