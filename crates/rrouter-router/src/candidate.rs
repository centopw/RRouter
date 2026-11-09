//! Attempt candidates: one (provider, model, connection) triple the dispatcher
//! may send to, plus the ordered plan produced for a client model string.

use std::collections::HashMap;

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

/// One account's subscription headroom, normalized by the gateway from the provider's
/// plan windows. `used_percent` is the most-consumed window; `resets_at` is that window's
/// reset (unix seconds), when the provider reports one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CapacityHint {
    pub used_percent: f64,
    pub resets_at: Option<i64>,
}

/// Subscription hints keyed by connection. An absent connection has no known plan
/// window (API-key account, unsupported provider, or a cold usage cache) and is treated
/// as healthy; hints are a filter, never a reason to refuse service by themselves.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CapacityHints {
    by_connection: HashMap<Uuid, CapacityHint>,
}

impl CapacityHints {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, connection: Uuid, hint: CapacityHint) {
        self.by_connection.insert(connection, hint);
    }

    pub fn get(&self, connection: Uuid) -> Option<&CapacityHint> {
        self.by_connection.get(&connection)
    }

    pub fn used_percent(&self, connection: Uuid) -> Option<f64> {
        self.get(connection).map(|hint| hint.used_percent)
    }

    pub fn iter(&self) -> impl Iterator<Item = (Uuid, &CapacityHint)> {
        self.by_connection
            .iter()
            .map(|(connection, hint)| (*connection, hint))
    }

    pub fn is_empty(&self) -> bool {
        self.by_connection.is_empty()
    }

    pub fn len(&self) -> usize {
        self.by_connection.len()
    }
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
