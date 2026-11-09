//! SQLite persistence for RRouter: encrypted provider credentials, gateway API keys,
//! model aliases/catalog, combos, and usage records.
//!
//! Provider connections carry an optional owning team (`connections.team_id`). Reads pick
//! a [`ConnectionScope`]: `Global` is the instance view (admin/reporting and non-team
//! keys), `Team` is strict tenant isolation, and a shared row (`team_id IS NULL`) is
//! visible only to `Global`. The filter lives in SQL so no caller can forget it.
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

mod audit;
mod crypto;
mod evals;
mod migrations;
mod store;
mod teams;
mod users;
mod webhooks;

pub use audit::AuditEvent;
pub use crypto::KEY_FILE_NAME;
pub use evals::{
    eval_passes, EvalCase, EvalResult, EvalRun, EvalRunStatus, EvalSuite, EvalSummary,
    EVAL_RUN_PAGE, MAX_EVAL_CASES, MAX_EVAL_MODEL, MAX_EVAL_NAME, MAX_EVAL_TEXT,
};
pub use migrations::SCHEMA_VERSION;
pub use rrouter_auth::{CredentialStore, Credentials};
pub use store::{
    cache_key, window_reset, window_start, AccountQuota, AccountQuotaState, AccountUsage, ApiKey,
    ApiKeyPatch, AttemptRecord, BudgetWindow, CacheEntry, Combo, Connection, ConnectionScope,
    ExhaustedAccounts, Guardrails, KeyModel, KeyScope, ModelAlias, ModelCost, ModelRow,
    PromptTemplate, PromptTemplateVersion, PruneReport, SemanticCacheCandidate, SemanticCacheEntry,
    SemanticCacheScope, StatusFilter, Store, UsageBucket, UsageModelBucket, UsageQuery,
    UsageRecord, UsageSummary, API_KEY_PREFIX,
};
pub use teams::{normalize_name, Team, TeamMember, TeamPatch, TeamRole, TeamSpend, MAX_TEAM_NAME};
pub use users::{
    hash_password, hash_secret, normalize_email, random_recovery_codes, random_setup_code,
    random_token, verify_password, Invite, SessionInfo, User, UserRole, MAX_SESSION_IP,
    MAX_SESSION_USER_AGENT,
};
pub use webhooks::{
    OutboxEvent, OutboxStatus, WebhookEndpoint, WebhookPatch, DELIVERY_LEASE_SECS, MAX_EVENT_TYPES,
    MAX_PAYLOAD_LEN, MAX_SECRET_ENV_LEN, MAX_URL_LEN, MAX_WEBHOOKS, OUTBOX_RETENTION_SECS,
    STATUS_DEAD, STATUS_DELIVERED, STATUS_DELIVERING, STATUS_PENDING, STATUS_UNMATCHED,
};
