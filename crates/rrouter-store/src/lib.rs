//! SQLite persistence for RRouter: encrypted provider credentials, gateway API keys,
//! model aliases/catalog, combos, and usage records.
//!
//! # Concurrency model
//!
//! `rusqlite` is synchronous. This crate wraps ONE `std::sync::Mutex<rusqlite::Connection>`
//! (shared through an `Arc`, so `Store` is `Clone` and cheap to hand to every axum handler)
//! and holds it only for the duration of a single statement or transaction — no `.await`
//! ever happens while the guard is held. There is no `spawn_blocking` and no
//! `block_in_place` (the gateway's runtime may be current-thread): every call is a handful
//! of indexed statements on a local file, far cheaper than a task hop, and a burst of
//! handler tasks simply queues on the mutex instead of stampeding SQLite. WAL is on, so an
//! operator's `sqlite3` shell never blocks the writers here.
//!
//! # Secrets
//!
//! A credential blob is `AES-256-GCM(serde_json(Credentials))` with a fresh 12-byte nonce
//! per write and the row's UUID bytes as associated data; see [`crypto`] for the key
//! material rules (hex master key, else a `0600` key file in the data dir). API keys are
//! stored as SHA-256 hex only. Usage rows never hold prompt or response text.
//!

mod crypto;
mod migrations;
mod store;

pub use crypto::KEY_FILE_NAME;
pub use store::{
    cache_key, window_start, ApiKey, ApiKeyPatch, AttemptRecord, BudgetWindow, CacheEntry, Combo,
    Connection, Guardrails, KeyModel, KeyScope, ModelAlias, ModelCost, ModelRow, PromptTemplate,
    PromptTemplateVersion, PruneReport, StatusFilter, Store, UsageBucket, UsageModelBucket,
    UsageQuery, UsageRecord, UsageSummary, API_KEY_PREFIX,
};
pub use rrouter_auth::{CredentialStore, Credentials};
