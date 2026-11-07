//! `Store`: the single embedded SQLite database behind the gateway.

use std::path::{Path, PathBuf};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection as Sql, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use rrouter_core::{Error, Result};

use crate::crypto::Cipher;
use crate::migrations;
use rrouter_auth::{CredentialStore, Credentials};

/// Prefix of every gateway API key. The remainder is 32 hex chars of CSPRNG output.
pub const API_KEY_PREFIX: &str = "sk-rr-";
const API_KEY_SECRET_HEX_LEN: usize = 32;

/// A stored provider credential set. `credentials` comes from the encrypted `data` blob.
#[derive(Debug, Clone)]
pub struct Connection {
    pub id: Uuid,
    pub provider: String,
    pub auth_type: String,
    pub name: Option<String>,
    pub email: Option<String>,
    pub priority: i32,
    /// Traffic share for the weighted strategy, `1..=1000` (higher = more traffic).
    /// The weighted planner reads it; the priority strategy ignores it.
    pub weight: u32,
    pub is_active: bool,
    pub credentials: Credentials,
    pub backoff_level: u32,
    pub last_error: Option<String>,
    pub last_error_code: Option<u16>,
    pub last_error_at: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// What a key may do. An `Inference` key spends tokens through `/v1/*`; an `Admin`
/// key manages the gateway through `/api/*` and `/metrics` and is refused by `/v1/*`.
///
/// Scope is a separate axis from the limits, so an operator can grant administration
/// without also granting spend (and without sharing one global admin token).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum KeyScope {
    /// The default for every key, and for every key created before v16.
    #[default]
    Inference,
    Admin,
}

impl KeyScope {
    /// The wire and column spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Inference => "inference",
            Self::Admin => "admin",
        }
    }

    /// Strict parse for client input: anything but the two spellings is `None`.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "inference" => Some(Self::Inference),
            "admin" => Some(Self::Admin),
            _ => None,
        }
    }

    /// Lenient read for a stored value. An unknown spelling reads as `Inference`, the
    /// narrower scope: a row written by a newer build can never administer the gateway
    /// on a build that does not understand its scope.
    pub fn from_stored(raw: &str) -> Self {
        Self::parse(raw).unwrap_or_default()
    }
}

/// A gateway API key. Only the SHA-256 hex of the plaintext is persisted.
///
/// The governance columns are the virtual-key contract: `0` means unlimited for the
/// numeric caps, `None` means unset for the nullable ones. Spend is not stored here —
/// it is the SQL sum over `usage` for this key (see [`Store::api_key_spend`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiKey {
    pub id: Uuid,
    pub key_hash: String,
    pub name: Option<String>,
    pub is_active: bool,
    /// Whether this key may spend tokens (`Inference`) or administer the gateway (`Admin`).
    pub scope: KeyScope,
    pub created_at: i64,
    /// Requests per minute allowed; 0 = unlimited.
    pub rpm: u32,
    /// Tokens per minute allowed; 0 = unlimited. Measured post-hoc from the tokens
    /// recorded for this key in the last 60 s — a single request can overshoot it.
    pub tpm: u32,
    /// Token budget per `budget_window`; 0 = unlimited. Spend is the durable `usage` sum.
    pub budget_tokens: u64,
    /// Spend budget per `budget_window` in micro-USD (millionths of a dollar); 0 = unlimited.
    /// Compared against the summed `usage.cost_micros` of this key inside the window.
    pub budget_micros: u64,
    /// Exact model refs or `provider/*` globs; `None` = every model.
    pub allowed_models: Option<Vec<String>>,
    /// Unix seconds after which the key is refused; `None` = never.
    pub expires_at: Option<i64>,
    /// Span of time the two budgets are measured over; see [`BudgetWindow`].
    pub budget_window: BudgetWindow,
    /// Traffic policy; `None` = no checks at all for this key.
    pub guardrails: Option<Guardrails>,
}

impl ApiKey {
    /// May this key be used as a credential at `now`? Active and unexpired — the two
    /// facts every credential path checks, so the `/v1` and admin gates ask once.
    ///
    /// Scope is not part of this: which routes a live key may call is the gateway's
    /// decision, not the store's.
    pub fn is_live(&self, now: i64) -> bool {
        self.is_active && self.expires_at.is_none_or(|expires_at| now < expires_at)
    }
}

/// Policy checks applied to one key's traffic: a prompt-size cap, blocked substrings, and
/// whether the response is scanned for them too.
///
/// Every field is optional and the empty document is equivalent to "no policy" — the
/// store turns `{}` into NULL, so "no guardrails" has exactly one representation. The
/// checks themselves live in the gateway; this type only defines the shape and the
/// invariants a stored policy must satisfy.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Guardrails {
    /// Refuse a request whose prompt text is longer than this many characters. `0`
    /// means "no prompt at all", which is a policy, not "unlimited": unlimited is
    /// `null`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_prompt_chars: Option<usize>,
    /// Case-insensitive substrings that refuse the request (prompt) or the response
    /// (when `scan_output`). Plain substrings, no regex engine.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked: Vec<String>,
    /// Scan response text for `blocked` too. Without `blocked` terms this does nothing.
    #[serde(default, skip_serializing_if = "is_false")]
    pub scan_output: bool,
}

/// `skip_serializing_if` for a flag that is off by default.
fn is_false(value: &bool) -> bool {
    !*value
}

impl Guardrails {
    /// Most blocked terms one key may carry: a policy that needs more is a content
    /// filter, and a 200-term list is already an order of magnitude past useful.
    pub const MAX_BLOCKED_TERMS: usize = 200;
    /// Longest single blocked term, in characters.
    pub const MAX_TERM_CHARS: usize = 200;

    /// Does this policy do anything at all? An all-default document is stored as NULL.
    pub fn is_empty(&self) -> bool {
        self.max_prompt_chars.is_none() && self.blocked.is_empty() && !self.scan_output
    }

    /// Reject a policy the gateway cannot enforce as written. A bad policy is a client
    /// error: silently dropping a term would leave an operator believing traffic is
    /// filtered when it is not.
    pub fn validate(&self) -> Result<()> {
        if self.blocked.len() > Self::MAX_BLOCKED_TERMS {
            return Err(Error::BadRequest(format!(
                "guardrails.blocked allows at most {} terms, got {}",
                Self::MAX_BLOCKED_TERMS,
                self.blocked.len()
            )));
        }
        for term in &self.blocked {
            if term.is_empty() {
                return Err(Error::BadRequest(
                    "guardrails.blocked terms must not be empty".into(),
                ));
            }
            if term.chars().count() > Self::MAX_TERM_CHARS {
                return Err(Error::BadRequest(format!(
                    "guardrails.blocked terms are limited to {} characters",
                    Self::MAX_TERM_CHARS
                )));
            }
        }
        Ok(())
    }

    /// Does the response side need scanning? `scan_output` without terms is a no-op.
    pub fn scans_output(&self) -> bool {
        self.scan_output && !self.blocked.is_empty()
    }

    /// The first blocked term contained in `text`, case-insensitively.
    ///
    /// Lowercasing both sides is what makes the match Unicode-safe: comparing bytes or
    /// ASCII-only case folding would miss `Ä`/`ä`. Returned as written in the policy so
    /// the refusal can name the term the operator configured.
    pub fn hit(&self, text: &str) -> Option<&str> {
        if self.blocked.is_empty() {
            return None;
        }
        let haystack = text.to_lowercase();
        self.blocked
            .iter()
            .find(|term| haystack.contains(&term.to_lowercase()))
            .map(String::as_str)
    }
}

/// A partial update of an [`ApiKey`]. Absent = leave alone; `Some(None)` = clear a
/// nullable column. Mirrors the `PATCH /api/keys/{id}` body.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ApiKeyPatch {
    pub is_active: Option<bool>,
    /// `inference` (spend) or `admin` (administer). Not nullable: a key is always one.
    pub scope: Option<KeyScope>,
    pub rpm: Option<u32>,
    pub tpm: Option<u32>,
    pub budget_tokens: Option<u64>,
    /// Integer, so `null` never clears: `0` is the "unlimited" value.
    pub budget_micros: Option<u64>,
    /// `Some(None)` clears the allowlist (back to every model).
    pub allowed_models: Option<Option<Vec<String>>>,
    /// `Some(None)` clears the expiry (back to never).
    pub expires_at: Option<Option<i64>>,
    pub budget_window: Option<BudgetWindow>,
    /// `Some(None)` clears the policy. An empty document is normalised to `None`, so
    /// `{}` and `null` leave a key with no guardrails.
    pub guardrails: Option<Option<Guardrails>>,
}

impl ApiKeyPatch {
    /// Apply this patch to `key` in place. Pure, so the merge rule is testable on its
    /// own and the SQL only ever writes the merged row.
    pub fn apply(&self, key: &mut ApiKey) {
        if let Some(v) = self.is_active {
            key.is_active = v;
        }
        if let Some(v) = self.scope {
            key.scope = v;
        }
        if let Some(v) = self.rpm {
            key.rpm = v;
        }
        if let Some(v) = self.tpm {
            key.tpm = v;
        }
        if let Some(v) = self.budget_tokens {
            key.budget_tokens = v;
        }
        if let Some(v) = self.budget_micros {
            key.budget_micros = v;
        }
        if let Some(v) = &self.allowed_models {
            key.allowed_models = v.clone();
        }
        if let Some(v) = self.expires_at {
            key.expires_at = v;
        }
        if let Some(v) = self.budget_window {
            key.budget_window = v;
        }
        if let Some(v) = &self.guardrails {
            // `{}` and `null` are the same policy: none. Normalising here keeps the
            // merged row identical to what a later read returns.
            key.guardrails = v.clone().filter(|g| !g.is_empty());
        }
    }
}

/// The span of time a key's budgets are measured over. `Total` is lifetime, the
/// pre-v12 behaviour; the others reset at UTC boundaries (see [`window_start`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BudgetWindow {
    #[default]
    Total,
    Daily,
    Weekly,
    Monthly,
}

impl BudgetWindow {
    /// The wire and column spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Total => "total",
            Self::Daily => "daily",
            Self::Weekly => "weekly",
            Self::Monthly => "monthly",
        }
    }

    /// Strict parse for client input: anything but the four spellings is `None`.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "total" => Some(Self::Total),
            "daily" => Some(Self::Daily),
            "weekly" => Some(Self::Weekly),
            "monthly" => Some(Self::Monthly),
            _ => None,
        }
    }

    /// Lenient read for a stored value. An unknown spelling reads as `Total`, the
    /// strictest window: all-time spend can only refuse more, never less.
    pub fn from_stored(raw: &str) -> Self {
        Self::parse(raw).unwrap_or_default()
    }
}

/// Start of the budget window containing `now` (unix seconds), or `None` for
/// [`BudgetWindow::Total`] (all time). Windows are UTC calendar spans: a day starts at
/// 00:00, a week at Monday 00:00, a month on its 1st at 00:00.
pub fn window_start(window: BudgetWindow, now: i64) -> Option<i64> {
    const DAY: i64 = 86_400;
    let days = now.div_euclid(DAY);
    let first_day = match window {
        BudgetWindow::Total => return None,
        BudgetWindow::Daily => days,
        // 1970-01-01 was a Thursday; with Monday = 0 that is weekday index 3.
        BudgetWindow::Weekly => days - (days + 3).rem_euclid(7),
        BudgetWindow::Monthly => {
            let (year, month, _) = civil_from_days(days);
            days_from_civil(year, month, 1)
        }
    };
    Some(first_day * DAY)
}

/// Days since 1970-01-01 of a proleptic Gregorian date (H. Hinnant's algorithm).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12; // March = 0
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// (year, month, day) of `days` since 1970-01-01 (H. Hinnant's algorithm).
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// Model alias -> `"provider/model"` target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelAlias {
    pub alias: String,
    pub target: String,
}

/// One per-key model mapping: the client's `from` ref is routed as `to`.
///
/// A mapping is one hop and runs before alias/combo resolution, so a client can keep a
/// stable model name while the operator repoints it for that key alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyModel {
    pub from: String,
    pub to: String,
}

/// Ordered fallback chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Combo {
    pub id: Uuid,
    pub name: String,
    pub models: Vec<String>,
    pub created_at: i64,
}

/// A reusable named system prompt; `body` is the current revision.
///
/// `version` counts body revisions only — renaming a template does not consume one — so
/// every version number maps to exactly one stored body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptTemplate {
    pub id: String,
    pub name: String,
    pub body: String,
    pub version: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

/// One historical body of a template. `version` matches [`PromptTemplate::version`]
/// when that revision is the current one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptTemplateVersion {
    pub version: i64,
    pub body: String,
    pub created_at: i64,
}

/// One row of the model registry.
///
/// `source` decides precedence — `custom` > `upstream` > `synced` > `catalog`
/// (`catalog` rows are never stored, they come from [`rrouter_router::Catalog`]) —
/// and [`Store::upsert_model`] refuses to lower it. `caps` is the stored JSON object
/// of capability overrides: a key that is absent falls through to the row this one
/// overlays.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelRow {
    pub provider: String,
    pub model: String,
    pub display: Option<String>,
    pub context: Option<u32>,
    pub max_output: Option<u32>,
    pub caps: serde_json::Value,
    /// Per-million-token pricing, when a catalog publishes it. `null` means unknown.
    pub cost: Option<serde_json::Value>,
    pub source: String,
    /// Set by [`Store::upsert_model`]; the field is ignored on write.
    pub updated_at: i64,
}

/// One stored price candidate for a model: the row's `cost` blob plus the source that
/// wrote it, so the caller can apply the same precedence the registry merge uses
/// (`custom` > `upstream` > `synced`). Several rows can match when a vendor catalog is
/// mirrored under a product-line provider (`openai` → `codex`).
#[derive(Debug, Clone, PartialEq)]
pub struct ModelCost {
    pub provider: String,
    pub cost: serde_json::Value,
    pub source: String,
}

/// One completed request. Never contains prompt or response bodies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageRecord {
    /// Row id, set when read back; ignored on write.
    pub id: Option<i64>,
    pub ts: i64,
    /// UUID echoed to the client as `x-request-id`; `None` for rows written before v8.
    pub request_id: Option<Uuid>,
    pub provider: String,
    pub model: String,
    pub connection_id: Option<Uuid>,
    /// Client endpoint that carried the request, e.g. `/v1/messages`.
    pub endpoint: Option<String>,
    /// Gateway key that paid for the request; `None` for anonymous traffic.
    pub key_id: Option<Uuid>,
    /// What the request cost in micro-USD (millionths of a dollar), priced from the
    /// registry's per-million-token rates at record time; 0 = no known price.
    pub cost_micros: u64,
    pub input_tokens: u32,
    pub output_tokens: u32,
    /// Prompt-cache reads (billed at a discount) and writes (billed at a premium).
    pub cache_read_tokens: u32,
    pub cache_write_tokens: u32,
    pub latency_ms: u32,
    /// Milliseconds from request start to the first model output; 0 when unmeasured.
    pub ttft_ms: u32,
    pub status: u16,
    pub streamed: bool,
    pub error: Option<String>,
}

/// Outcome class for [`UsageQuery::status`]. `Success` is a 2xx answer with no error text;
/// `Error` is the dashboard's rule everywhere else: `status >= 400 OR error IS NOT NULL`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusFilter {
    Success,
    Error,
}

impl StatusFilter {
    fn as_sql(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Error => "error",
        }
    }
}

/// Filters and keyset cursor for [`Store::recent_usage`]. Every field except `limit` is
/// optional and an unset field does not constrain the walk. All filters are applied in SQL,
/// so each page is the next `limit` rows of the filtered set rather than a filtered slice.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UsageQuery {
    /// Keep rows with `ts >= since`.
    pub since: Option<i64>,
    /// Cursor `ts` of the last row on the previous page.
    pub before_ts: Option<i64>,
    /// Cursor `id` of the last row on the previous page; with `before_ts`, pages exactly.
    pub before_id: Option<i64>,
    /// Exact provider match.
    pub provider: Option<String>,
    /// Stored model string, matched exactly.
    pub model: Option<String>,
    /// Bare model id that also matches. The gateway sets it when the caller passed
    /// `provider/model`, so both spellings of a stored model are found.
    pub bare_model: Option<String>,
    /// Gateway key that paid for the request.
    pub key_id: Option<Uuid>,
    pub status: Option<StatusFilter>,
    /// Rows per page.
    pub limit: usize,
}

/// One candidate that was dispatched for a request.
///
/// A request has one attempt per candidate the dispatcher actually sent to, numbered
/// from 1 in dispatch order, so a fallback is readable from the log alone. `status` is
/// the upstream HTTP status when there was one and `0` when nobody answered (transport
/// failure, timeout, breaker). A row is also the honest record of a request that failed
/// in the end, which never produces a usage row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttemptRecord {
    /// Row id, set when read back; ignored on write.
    pub id: Option<i64>,
    pub request_id: Uuid,
    pub ts: i64,
    /// 1-based position in dispatch order.
    pub attempt: u32,
    pub provider: String,
    /// Model id actually sent upstream, not the client's alias.
    pub model: String,
    pub connection_id: Option<Uuid>,
    pub status: u16,
    /// Same message the client received for this attempt.
    pub error: Option<String>,
    pub latency_ms: u32,
    /// Gateway key that paid for the request; `None` for anonymous traffic.
    pub key_id: Option<Uuid>,
}

/// One stored exact-match response.
///
/// `key` is the hash of the canonical request ([`cache_key`]); `body` is the JSON the
/// client received, never a prompt. `hits` counts how often the entry was served, and
/// `expires_at` (unix seconds) is the last second it may be served.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheEntry {
    pub key: String,
    pub body: String,
    pub created_at: i64,
    pub expires_at: i64,
    pub hits: u32,
}

/// What one retention sweep removed. Counts are rows, not bytes.
///
/// `usage` and `attempts` are deleted by the same cutoff in one transaction, so the
/// pair is the honest answer to "how much history did the retention knob discard?".
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PruneReport {
    pub usage: usize,
    pub attempts: usize,
}

/// One time bucket for one model: the traffic chart stacks these by model, so
/// every segment is an honest server-side count rather than a client estimate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageModelBucket {
    pub start: i64,
    pub provider: String,
    pub model: String,
    pub requests: u64,
    pub errors: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    /// Summed micro-USD for this bucket.
    pub cost_micros: u64,
    pub avg_latency_ms: u32,
}

/// Per provider+model aggregate over `usage`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageSummary {
    pub provider: String,
    pub model: String,
    pub requests: u64,
    pub errors: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    /// Summed micro-USD over the same rows.
    pub cost_micros: u64,
    pub avg_latency_ms: u32,
}

/// One time bucket of usage, for charts that must stay honest at high volume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageBucket {
    /// Bucket start, unix seconds, aligned to the requested bucket width.
    pub start: i64,
    pub requests: u64,
    pub errors: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    /// Summed micro-USD over the same rows.
    pub cost_micros: u64,
    pub avg_latency_ms: u32,
}

/// Embedded database handle. Cheap to clone; all clones share one connection.
#[derive(Clone)]
pub struct Store {
    conn: Arc<Mutex<Sql>>,
    cipher: Arc<Cipher>,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store").finish_non_exhaustive()
    }
}

impl Store {
    /// Opens (creating if needed) the database at `path` and runs migrations.
    ///
    /// The data dir is `path`'s parent; when `master_key` is `None` the AES key is read from
    /// (or created in) `<data_dir>/master.key`.
    pub fn open(path: &Path, master_key: Option<String>) -> Result<Self> {
        let data_dir = data_dir_for(path)?;
        std::fs::create_dir_all(&data_dir).map_err(|e| {
            Error::Store(format!("create data dir {}: {e}", data_dir.display()))
        })?;
        let cipher = match master_key {
            Some(k) => Cipher::from_hex(&k)?,
            None => Cipher::load_or_create_file(&data_dir)?,
        };
        Self::open_with(path, cipher)
    }

    /// In-memory database with an ephemeral random key. For tests and dry runs.
    pub fn open_in_memory() -> Result<Self> {
        let mut key = [0u8; 32];
        rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut key);
        let cipher = Cipher::from_bytes(&key)?;
        let mut conn = Sql::open_in_memory().map_err(migrations::db_err)?;
        Self::configure(&conn)?;
        migrations::migrate(&mut conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            cipher: Arc::new(cipher),
        })
    }

    fn open_with(path: &Path, cipher: Cipher) -> Result<Self> {
        let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
            | rusqlite::OpenFlags::SQLITE_OPEN_CREATE
            | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX;
        let mut conn = Sql::open_with_flags(path, flags).map_err(|e| {
            Error::Store(format!("open database {}: {e}", path.display()))
        })?;
        Self::configure(&conn)?;
        migrations::migrate(&mut conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            cipher: Arc::new(cipher),
        })
    }

    /// WAL + foreign keys + 5 s busy timeout. WAL is a file property, so it persists.
    fn configure(conn: &Sql) -> Result<()> {
        conn.busy_timeout(Duration::from_millis(5000))
            .map_err(migrations::db_err)?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;\
             PRAGMA synchronous = NORMAL;\
             PRAGMA foreign_keys = ON;",
        )
        .map_err(migrations::db_err)
    }

    /// Lock the connection. A panicking writer would poison the mutex; the underlying SQLite
    /// connection is still valid (rusqlite does not unwind mid-statement here), so recover.
    fn conn(&self) -> MutexGuard<'_, Sql> {
        self.conn.lock().unwrap_or_else(|p| p.into_inner())
    }

    // ----- connections -------------------------------------------------------------

    pub async fn create_connection(
        &self,
        provider: &str,
        auth_type: &str,
        credentials: Credentials,
        name: Option<&str>,
    ) -> Result<Connection> {
        let id = Uuid::new_v4();
        let blob = self.seal(&id, &credentials)?;
        let now = now();
        let email = credentials.account_label.clone();
        {
            let conn = self.conn();
            conn.execute(
                "INSERT INTO connections \
                 (id, provider, auth_type, name, email, priority, is_active, data, backoff_level, \
                  created_at, updated_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, 0, 1, ?6, 0, ?7, ?7)",
                params![
                    id.to_string(),
                    provider,
                    auth_type,
                    name,
                    email,
                    blob,
                    now
                ],
            )
            .map_err(migrations::db_err)?;
        }
        self.get_connection(id)
            .await?
            .ok_or_else(|| Error::Store("connection vanished after insert".to_string()))
    }

    pub async fn get_connection(&self, id: Uuid) -> Result<Option<Connection>> {
        let blob = {
            let conn = self.conn();
            conn.query_row(
                &format!("SELECT {CONN_COLUMNS} FROM connections WHERE id = ?1"),
                params![id.to_string()],
                row_to_connection,
            )
            .optional()
            .map_err(migrations::db_err)?
        };
        blob.map(|raw| self.decode(raw)).transpose()
    }

    pub async fn list_connections(&self, provider: Option<&str>) -> Result<Vec<Connection>> {
        let raws = {
            let conn = self.conn();
            let mut out = Vec::new();
            match provider {
                Some(p) => {
                    let mut stmt = conn
                        .prepare(&format!(
                            "SELECT {CONN_COLUMNS} FROM connections WHERE provider = ?1 \
                             ORDER BY priority ASC, created_at ASC"
                        ))
                        .map_err(migrations::db_err)?;
                    let rows = stmt
                        .query_map(params![p], row_to_connection)
                        .map_err(migrations::db_err)?;
                    for r in rows {
                        out.push(r.map_err(migrations::db_err)?);
                    }
                }
                None => {
                    let mut stmt = conn
                        .prepare(&format!(
                            "SELECT {CONN_COLUMNS} FROM connections \
                             ORDER BY provider ASC, priority ASC, created_at ASC"
                        ))
                        .map_err(migrations::db_err)?;
                    let rows = stmt.query_map([], row_to_connection).map_err(migrations::db_err)?;
                    for r in rows {
                        out.push(r.map_err(migrations::db_err)?);
                    }
                }
            }
            out
        };
        raws.into_iter().map(|raw| self.decode(raw)).collect()
    }

    /// Read-merge-write of the credentials blob inside one SQL transaction.
    ///
    /// The transaction is `IMMEDIATE`, so the read lock is taken up front: two concurrent
    /// merges serialize instead of both reading the pre-merge row and losing one update.
    pub async fn merge_credentials(&self, id: Uuid, patch: Credentials) -> Result<Connection> {
        {
            let mut conn = self.conn();
            let tx = conn
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(migrations::db_err)?;
            let raw = tx
                .query_row(
                    &format!("SELECT {CONN_COLUMNS} FROM connections WHERE id = ?1"),
                    params![id.to_string()],
                    row_to_connection,
                )
                .optional()
                .map_err(migrations::db_err)?
                .ok_or_else(|| Error::NotFound(format!("connection {id}")))?;

            let mut merged = self.decode(raw)?.credentials;
            merge_into(&mut merged, patch);
            let blob = self.seal(&id, &merged)?;
            if let Some(label) = merged.account_label.clone() {
                tx.execute(
                    "UPDATE connections SET email = ?2, updated_at = ?3 WHERE id = ?1",
                    params![id.to_string(), label, now()],
                )
                .map_err(migrations::db_err)?;
            }
            tx.execute(
                "UPDATE connections SET data = ?2, updated_at = ?3 WHERE id = ?1",
                params![id.to_string(), blob, now()],
            )
            .map_err(migrations::db_err)?;
            tx.commit().map_err(migrations::db_err)?;
        }
        self.get_connection(id)
            .await?
            .ok_or_else(|| Error::NotFound(format!("connection {id}")))
    }

    /// Record a routing/upstream failure on a connection.
    pub async fn update_connection_status(
        &self,
        id: Uuid,
        backoff_level: u32,
        last_error: Option<&str>,
        code: Option<u16>,
    ) -> Result<()> {
        let conn = self.conn();
        let now = now();
        let mut stmt = conn
            .prepare(
                "UPDATE connections SET backoff_level = ?2, last_error = ?3, last_error_code = ?4, \
                 last_error_at = ?5, updated_at = ?5 WHERE id = ?1",
            )
            .map_err(migrations::db_err)?;
        let n = stmt
            .execute(params![id.to_string(), backoff_level, last_error, code, now])
            .map_err(migrations::db_err)?;
        if n == 0 {
            return Err(Error::NotFound(format!("connection {id}")));
        }
        Ok(())
    }

    /// Lower `priority` values are tried first by the planner; equal values round-robin.
    pub async fn set_connection_priority(&self, id: Uuid, priority: i32) -> Result<()> {
        let conn = self.conn();
        let n = conn
            .execute(
                "UPDATE connections SET priority = ?2, updated_at = ?3 WHERE id = ?1",
                params![id.to_string(), priority, now()],
            )
            .map_err(migrations::db_err)?;
        if n == 0 {
            return Err(Error::NotFound(format!("connection {id}")));
        }
        Ok(())
    }

    /// Traffic share used by the weighted strategy. Clamped to the schema's
    /// `1..=1000`: zero would silently starve an account that is still meant to serve,
    /// and an unbounded value would make the weighted cycle unbounded too.
    pub async fn set_connection_weight(&self, id: Uuid, weight: u32) -> Result<()> {
        let conn = self.conn();
        let n = conn
            .execute(
                "UPDATE connections SET weight = ?2, updated_at = ?3 WHERE id = ?1",
                params![id.to_string(), weight.clamp(MIN_WEIGHT, MAX_WEIGHT), now()],
            )
            .map_err(migrations::db_err)?;
        if n == 0 {
            return Err(Error::NotFound(format!("connection {id}")));
        }
        Ok(())
    }

    /// Fold one successful attempt's latency into the connection's EWMA:
    /// `new = old * 0.8 + latency * 0.2`, evaluated in SQL so concurrent attempts
    /// cannot lose each other's update. Callers fire this and forget it.
    pub async fn record_connection_latency(&self, id: Uuid, latency_ms: u32) -> Result<()> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO connection_stats (connection_id, ewma_latency_ms, updated_at) \
             VALUES (?1, ?2, ?3) \
             ON CONFLICT(connection_id) DO UPDATE SET \
               ewma_latency_ms = connection_stats.ewma_latency_ms * 0.8 + \
                                 excluded.ewma_latency_ms * 0.2, \
               updated_at = excluded.updated_at",
            params![id.to_string(), f64::from(latency_ms), now()],
        )
        .map_err(migrations::db_err)?;
        Ok(())
    }

    /// Every measured connection latency, for `least_latency` ordering. A connection
    /// with no successful attempt yet is simply absent.
    pub async fn connection_latencies(&self) -> Result<std::collections::HashMap<Uuid, f64>> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare("SELECT connection_id, ewma_latency_ms FROM connection_stats")
            .map_err(migrations::db_err)?;
        let rows = stmt
            .query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?))
            })
            .map_err(migrations::db_err)?;
        let mut out = std::collections::HashMap::new();
        for row in rows {
            let (raw, ewma) = row.map_err(migrations::db_err)?;
            if let Ok(id) = Uuid::parse_str(&raw) {
                out.insert(id, ewma);
            }
        }
        Ok(out)
    }

    /// Take a connection out of (or back into) routing without deleting its credentials.
    pub async fn set_connection_active(&self, id: Uuid, active: bool) -> Result<()> {
        let conn = self.conn();
        let n = conn
            .execute(
                "UPDATE connections SET is_active = ?2, updated_at = ?3 WHERE id = ?1",
                params![id.to_string(), i64::from(active), now()],
            )
            .map_err(migrations::db_err)?;
        if n == 0 {
            return Err(Error::NotFound(format!("connection {id}")));
        }
        Ok(())
    }

    pub async fn delete_connection(&self, id: Uuid) -> Result<bool> {
        let conn = self.conn();
        let n = conn
            .execute(
                "DELETE FROM connections WHERE id = ?1",
                params![id.to_string()],
            )
            .map_err(migrations::db_err)?;
        // A deleted connection's latency sample must not outlive it: the id is never
        // reused, so the row would only ever be dead weight.
        conn.execute(
            "DELETE FROM connection_stats WHERE connection_id = ?1",
            params![id.to_string()],
        )
        .map_err(migrations::db_err)?;
        Ok(n > 0)
    }

    fn seal(&self, id: &Uuid, creds: &Credentials) -> Result<Vec<u8>> {
        let plain = serde_json::to_vec(creds)?;
        self.cipher.seal(id.as_bytes(), &plain)
    }

    fn decode(&self, raw: RawConnection) -> Result<Connection> {
        let plain = self.cipher.open(raw.id.as_bytes(), &raw.data)?;
        let credentials: Credentials = serde_json::from_slice(&plain).map_err(|e| {
            Error::Store(format!("credential blob for {} is not valid JSON: {e}", raw.id))
        })?;
        Ok(Connection {
            id: raw.id,
            provider: raw.provider,
            auth_type: raw.auth_type,
            name: raw.name,
            email: raw.email,
            priority: raw.priority,
            weight: raw.weight,
            is_active: raw.is_active,
            credentials,
            backoff_level: raw.backoff_level,
            last_error: raw.last_error,
            last_error_code: raw.last_error_code,
            last_error_at: raw.last_error_at,
            created_at: raw.created_at,
            updated_at: raw.updated_at,
        })
    }

    // ----- gateway API keys --------------------------------------------------------

    /// The key columns every read selects, in [`row_to_api_key`]'s order.
    const KEY_COLUMNS: &'static str =
        "id, key_hash, name, is_active, created_at, rpm, tpm, budget_tokens, allowed_models, \
         expires_at, budget_micros, budget_window, guardrails, scope";

    /// Generate `sk-rr-<32 hex>`, persist `SHA-256(plaintext)` only, return both.
    ///
    /// A new key is an inference key with no limits, no allowlist and no expiry.
    pub async fn create_api_key(&self, name: Option<&str>) -> Result<(String, ApiKey)> {
        let plaintext = generate_api_key();
        let key_hash = hash_api_key(&plaintext);
        let id = Uuid::new_v4();
        let created_at = now();
        let conn = self.conn();
        conn.execute(
            "INSERT INTO api_keys (id, key_hash, name, is_active, created_at) \
             VALUES (?1, ?2, ?3, 1, ?4)",
            params![id.to_string(), key_hash, name, created_at],
        )
        .map_err(migrations::db_err)?;
        Ok((
            plaintext,
            ApiKey {
                id,
                key_hash,
                name: name.map(str::to_owned),
                is_active: true,
                scope: KeyScope::Inference,
                created_at,
                rpm: 0,
                tpm: 0,
                budget_tokens: 0,
                budget_micros: 0,
                allowed_models: None,
                expires_at: None,
                budget_window: BudgetWindow::Total,
                guardrails: None,
            },
        ))
    }

    /// `Some(key)` when `presented` matches a stored key, active or not.
    ///
    /// Activity and expiry are governance decisions, not lookup rules: the caller must
    /// see an inactive or expired key so it can answer `403` rather than `401`.
    pub async fn verify_api_key(&self, presented: &str) -> Result<Option<ApiKey>> {
        let presented = presented.trim();
        if presented.is_empty() {
            return Ok(None);
        }
        let hash = hash_api_key(presented);
        let conn = self.conn();
        conn.query_row(
            &format!(
                "SELECT {} FROM api_keys WHERE key_hash = ?1",
                Self::KEY_COLUMNS
            ),
            params![hash],
            row_to_api_key,
        )
        .optional()
        .map_err(migrations::db_err)
    }

    pub async fn list_api_keys(&self) -> Result<Vec<ApiKey>> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {} FROM api_keys ORDER BY created_at ASC",
                Self::KEY_COLUMNS
            ))
            .map_err(migrations::db_err)?;
        let rows = stmt.query_map([], row_to_api_key).map_err(migrations::db_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(migrations::db_err)?);
        }
        Ok(out)
    }

    /// One key by id, or `None` when it does not exist.
    pub async fn get_api_key(&self, id: Uuid) -> Result<Option<ApiKey>> {
        let conn = self.conn();
        conn.query_row(
            &format!("SELECT {} FROM api_keys WHERE id = ?1", Self::KEY_COLUMNS),
            params![id.to_string()],
            row_to_api_key,
        )
        .optional()
        .map_err(migrations::db_err)
    }

    /// Merge `patch` into the stored key and return the merged row, or `None` when the
    /// id does not exist. Read and write happen under the same connection lock, so two
    /// concurrent patches cannot interleave into a lost update.
    pub async fn update_api_key(&self, id: Uuid, patch: &ApiKeyPatch) -> Result<Option<ApiKey>> {
        let conn = self.conn();
        let mut key: ApiKey = match conn
            .query_row(
                &format!("SELECT {} FROM api_keys WHERE id = ?1", Self::KEY_COLUMNS),
                params![id.to_string()],
                row_to_api_key,
            )
            .optional()
            .map_err(migrations::db_err)?
        {
            Some(k) => k,
            None => return Ok(None),
        };
        // A policy that cannot be enforced as written is refused here rather than
        // stored: an operator must never believe traffic is filtered when it is not.
        if let Some(Some(policy)) = &patch.guardrails {
            policy.validate()?;
        }
        patch.apply(&mut key);
        let allowed = match &key.allowed_models {
            Some(models) => Some(serde_json::to_string(models)?),
            None => None,
        };
        let guardrails = match &key.guardrails {
            Some(policy) => Some(serde_json::to_string(policy)?),
            None => None,
        };
        conn.execute(
            "UPDATE api_keys SET is_active = ?2, rpm = ?3, tpm = ?4, budget_tokens = ?5, \
             allowed_models = ?6, expires_at = ?7, budget_micros = ?8, budget_window = ?9, \
             guardrails = ?10, scope = ?11 WHERE id = ?1",
            params![
                id.to_string(),
                key.is_active as i64,
                key.rpm,
                key.tpm,
                key.budget_tokens,
                allowed,
                key.expires_at,
                key.budget_micros,
                key.budget_window.as_str(),
                guardrails,
                key.scope.as_str(),
            ],
        )
        .map_err(migrations::db_err)?;
        Ok(Some(key))
    }

    pub async fn delete_api_key(&self, id: Uuid) -> Result<bool> {
        let conn = self.conn();
        let n = conn
            .execute("DELETE FROM api_keys WHERE id = ?1", params![id.to_string()])
            .map_err(migrations::db_err)?;
        if n > 0 {
            // The key id is never reused, so its mappings can only ever be dead weight.
            conn.execute(
                "DELETE FROM key_models WHERE key_id = ?1",
                params![id.to_string()],
            )
            .map_err(migrations::db_err)?;
        }
        Ok(n > 0)
    }

    // ----- per-key model mapping ---------------------------------------------------

    /// All mappings for one key, ordered by the client ref.
    pub async fn key_models(&self, key_id: Uuid) -> Result<Vec<KeyModel>> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(
                "SELECT from_ref, to_ref FROM key_models WHERE key_id = ?1 ORDER BY from_ref ASC",
            )
            .map_err(migrations::db_err)?;
        let rows = stmt
            .query_map(params![key_id.to_string()], |r| {
                Ok(KeyModel { from: r.get(0)?, to: r.get(1)? })
            })
            .map_err(migrations::db_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(migrations::db_err)?);
        }
        Ok(out)
    }

    /// The target this key maps `from` to, if any. One indexed row, because it runs on
    /// the request path.
    pub async fn key_model_target(&self, key_id: Uuid, from: &str) -> Result<Option<String>> {
        let conn = self.conn();
        conn.query_row(
            "SELECT to_ref FROM key_models WHERE key_id = ?1 AND from_ref = ?2",
            params![key_id.to_string(), from],
            |r| r.get(0),
        )
        .optional()
        .map_err(migrations::db_err)
    }

    /// Every key's mappings, grouped by key, for `GET /api/keys`.
    pub async fn key_models_all(
        &self,
    ) -> Result<std::collections::HashMap<Uuid, Vec<KeyModel>>> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(
                "SELECT key_id, from_ref, to_ref FROM key_models ORDER BY key_id ASC, from_ref ASC",
            )
            .map_err(migrations::db_err)?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    KeyModel { from: r.get(1)?, to: r.get(2)? },
                ))
            })
            .map_err(migrations::db_err)?;
        let mut out: std::collections::HashMap<Uuid, Vec<KeyModel>> =
            std::collections::HashMap::new();
        for row in rows {
            let (raw, model) = row.map_err(migrations::db_err)?;
            if let Ok(id) = Uuid::parse_str(&raw) {
                out.entry(id).or_default().push(model);
            }
        }
        Ok(out)
    }

    /// Replace one key's whole mapping in a single transaction, so a reader never sees
    /// half of an update and a duplicate `from` cannot survive. `Ok(false)` when the key
    /// does not exist — the caller answers `404` without a second query.
    pub async fn replace_key_models(&self, key_id: Uuid, models: &[KeyModel]) -> Result<bool> {
        let mut conn = self.conn();
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(migrations::db_err)?;
        let exists: bool = tx
            .query_row(
                "SELECT 1 FROM api_keys WHERE id = ?1",
                params![key_id.to_string()],
                |_| Ok(true),
            )
            .optional()
            .map_err(migrations::db_err)?
            .unwrap_or(false);
        if !exists {
            return Ok(false);
        }
        tx.execute(
            "DELETE FROM key_models WHERE key_id = ?1",
            params![key_id.to_string()],
        )
        .map_err(migrations::db_err)?;
        {
            let mut stmt = tx
                .prepare("INSERT INTO key_models (key_id, from_ref, to_ref) VALUES (?1, ?2, ?3)")
                .map_err(migrations::db_err)?;
            for m in models {
                stmt.execute(params![key_id.to_string(), m.from, m.to])
                    .map_err(migrations::db_err)?;
            }
        }
        tx.commit().map_err(migrations::db_err)?;
        Ok(true)
    }

    /// Tokens this key spent since `since` (unix seconds; `None` = all time): input +
    /// output + cache reads + cache writes, summed in SQL so a budget check never has to
    /// read events into Rust. Pass [`window_start`] to measure one budget window.
    pub async fn api_key_spend(&self, id: Uuid, since: Option<i64>) -> Result<u64> {
        let conn = self.conn();
        let total: i64 = conn
            .query_row(
                "SELECT COALESCE(sum(input_tokens + output_tokens + cache_read_tokens + \
                 cache_write_tokens), 0) FROM usage WHERE key_id = ?1 AND ts >= ?2",
                params![id.to_string(), since.unwrap_or(i64::MIN)],
                |r| r.get(0),
            )
            .map_err(migrations::db_err)?;
        Ok(total.max(0) as u64)
    }

    /// Token spend since `since` for every key in one statement, for `GET /api/keys`.
    pub async fn api_key_spend_all(
        &self,
        since: Option<i64>,
    ) -> Result<std::collections::HashMap<Uuid, u64>> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(
                "SELECT key_id, sum(input_tokens + output_tokens + cache_read_tokens + \
                 cache_write_tokens) FROM usage WHERE key_id IS NOT NULL AND ts >= ?1 \
                 GROUP BY key_id",
            )
            .map_err(migrations::db_err)?;
        let rows = stmt
            .query_map(params![since.unwrap_or(i64::MIN)], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
            })
            .map_err(migrations::db_err)?;
        let mut out = std::collections::HashMap::new();
        for row in rows {
            let (raw, total) = row.map_err(migrations::db_err)?;
            if let Ok(id) = Uuid::parse_str(&raw) {
                out.insert(id, total.max(0) as u64);
            }
        }
        Ok(out)
    }

    /// Money this key spent since `since` (unix seconds; `None` = all time): the summed
    /// `usage.cost_micros` of its rows.
    ///
    /// Rows priced at 0 (an unknown model) contribute nothing, which is the honest
    /// answer: the gateway must not invent a price for a model it has no rate for.
    pub async fn api_key_cost_spend(&self, id: Uuid, since: Option<i64>) -> Result<u64> {
        let conn = self.conn();
        let total: i64 = conn
            .query_row(
                "SELECT COALESCE(sum(cost_micros), 0) FROM usage \
                 WHERE key_id = ?1 AND ts >= ?2",
                params![id.to_string(), since.unwrap_or(i64::MIN)],
                |r| r.get(0),
            )
            .map_err(migrations::db_err)?;
        Ok(total.max(0) as u64)
    }

    /// Money spent since `since` by every key in one statement, for `GET /api/keys`.
    pub async fn api_key_cost_spend_all(
        &self,
        since: Option<i64>,
    ) -> Result<std::collections::HashMap<Uuid, u64>> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(
                "SELECT key_id, sum(cost_micros) FROM usage \
                 WHERE key_id IS NOT NULL AND ts >= ?1 GROUP BY key_id",
            )
            .map_err(migrations::db_err)?;
        let rows = stmt
            .query_map(params![since.unwrap_or(i64::MIN)], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
            })
            .map_err(migrations::db_err)?;
        let mut out = std::collections::HashMap::new();
        for row in rows {
            let (raw, total) = row.map_err(migrations::db_err)?;
            if let Ok(id) = Uuid::parse_str(&raw) {
                out.insert(id, total.max(0) as u64);
            }
        }
        Ok(out)
    }

    /// Every stored price for `model` under `providers` (the registry provider and the
    /// models.dev vendor it mirrors, so a product line without its own synced rows is
    /// still priced). One indexed read of `models`; used when a usage row is written.
    pub async fn model_costs(&self, model: &str, providers: &[&str]) -> Result<Vec<ModelCost>> {
        if providers.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = (2..providers.len() + 2)
            .map(|i| format!("?{i}"))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT provider, cost, source FROM models \
             WHERE model = ?1 AND cost IS NOT NULL AND provider IN ({placeholders})"
        );
        let conn = self.conn();
        let mut stmt = conn.prepare(&sql).map_err(migrations::db_err)?;
        let mut params: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(providers.len() + 1);
        params.push(&model);
        for provider in providers {
            params.push(provider);
        }
        let rows = stmt
            .query_map(params.as_slice(), |r| {
                Ok(ModelCost {
                    provider: r.get(0)?,
                    cost: parse_cost_blob(r.get::<_, String>(1)?),
                    source: r.get(2)?,
                })
            })
            .map_err(migrations::db_err)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(migrations::db_err)?);
        }
        Ok(out)
    }

    pub async fn count_api_keys(&self) -> Result<usize> {
        let conn = self.conn();
        let n: i64 = conn
            .query_row("SELECT count(*) FROM api_keys", [], |r| r.get(0))
            .map_err(migrations::db_err)?;
        Ok(usize::try_from(n).unwrap_or(usize::MAX))
    }

    // ----- model routing config ----------------------------------------------------

    pub async fn upsert_alias(&self, alias: &str, target: &str) -> Result<()> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO model_aliases (alias, target) VALUES (?1, ?2) \
             ON CONFLICT(alias) DO UPDATE SET target = excluded.target",
            params![alias, target],
        )
        .map_err(migrations::db_err)?;
        Ok(())
    }

    pub async fn list_aliases(&self) -> Result<Vec<ModelAlias>> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare("SELECT alias, target FROM model_aliases ORDER BY alias ASC")
            .map_err(migrations::db_err)?;
        let rows = stmt
            .query_map([], |r| {
                Ok(ModelAlias {
                    alias: r.get(0)?,
                    target: r.get(1)?,
                })
            })
            .map_err(migrations::db_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(migrations::db_err)?);
        }
        Ok(out)
    }

    pub async fn delete_alias(&self, alias: &str) -> Result<bool> {
        let conn = self.conn();
        let n = conn
            .execute("DELETE FROM model_aliases WHERE alias = ?1", params![alias])
            .map_err(migrations::db_err)?;
        Ok(n > 0)
    }

    pub async fn set_disabled(&self, model: &str, disabled: bool) -> Result<()> {
        let conn = self.conn();
        if disabled {
            conn.execute(
                "INSERT INTO disabled_models (model) VALUES (?1) ON CONFLICT(model) DO NOTHING",
                params![model],
            )
            .map_err(migrations::db_err)?;
        } else {
            conn.execute("DELETE FROM disabled_models WHERE model = ?1", params![model])
                .map_err(migrations::db_err)?;
        }
        Ok(())
    }

    pub async fn disabled_models(&self) -> Result<Vec<String>> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare("SELECT model FROM disabled_models ORDER BY model ASC")
            .map_err(migrations::db_err)?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0)).map_err(migrations::db_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(migrations::db_err)?);
        }
        Ok(out)
    }

    // ----- model registry ----------------------------------------------------------

    /// Insert or replace one registry row.
    ///
    /// Precedence is enforced in SQL: a `synced` refresh or an `upstream` fetch never
    /// overwrites a higher-precedence row for the same `(provider, model)`, and a
    /// `synced` row never overwrites an `upstream` one. `updated_at` is set here.
    pub async fn upsert_model(&self, row: &ModelRow) -> Result<()> {
        let caps = serde_json::to_string(&row.caps)?;
        let conn = self.conn();
        conn.execute(
            UPSERT_MODEL_SQL,
            params![
                row.provider,
                row.model,
                row.display,
                row.context,
                row.max_output,
                caps,
                row.cost.as_ref().map(|c| c.to_string()),
                row.source,
                now()
            ],
        )
        .map_err(migrations::db_err)?;
        Ok(())
    }

    /// Delete a `custom` row. Returns whether something was deleted; a row of another
    /// source is left alone, so the route can answer `404` for both absent and
    /// non-custom rows.
    pub async fn delete_model(&self, provider: &str, model: &str) -> Result<bool> {
        let conn = self.conn();
        let n = conn
            .execute(
                "DELETE FROM models WHERE provider = ?1 AND model = ?2 AND source = 'custom'",
                params![provider, model],
            )
            .map_err(migrations::db_err)?;
        Ok(n > 0)
    }

    /// Every stored registry row, ordered by `provider`, then `model`.
    pub async fn list_models(&self) -> Result<Vec<ModelRow>> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(
                "SELECT provider, model, display, context, max_output, caps, cost, source, updated_at \
                 FROM models ORDER BY provider ASC, model ASC",
            )
            .map_err(migrations::db_err)?;
        let rows = stmt
            .query_map([], |r| {
                Ok(ModelRow {
                    provider: r.get(0)?,
                    model: r.get(1)?,
                    display: r.get(2)?,
                    context: r.get(3)?,
                    max_output: r.get(4)?,
                    caps: parse_caps(r.get::<_, String>(5)?),
                    cost: r
                        .get::<_, Option<String>>(6)?
                        .and_then(|raw| serde_json::from_str(&raw).ok()),
                    source: r.get(7)?,
                    updated_at: r.get(8)?,
                })
            })
            .map_err(migrations::db_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(migrations::db_err)?);
        }
        Ok(out)
    }

    /// Drop every row of one `source`; returns how many were removed. Used by the
    /// models.dev refresh, which replaces its own rows wholesale.
    pub async fn clear_models_by_source(&self, source: &str) -> Result<usize> {
        let conn = self.conn();
        let n = conn
            .execute("DELETE FROM models WHERE source = ?1", params![source])
            .map_err(migrations::db_err)?;
        Ok(n)
    }

    // ----- combos ------------------------------------------------------------------

    /// Insert or replace the chain named `name`. Order is preserved; duplicates are dropped.
    pub async fn upsert_combo(&self, name: &str, models: &[String]) -> Result<Combo> {
        let mut unique = Vec::with_capacity(models.len());
        for m in models {
            if !unique.contains(m) {
                unique.push(m.clone());
            }
        }
        let json = serde_json::to_string(&unique)?;
        // Read-then-write under one mutex guard: keep the original id/created_at on update.
        let conn = self.conn();
        let existing: Option<(Uuid, i64)> = conn
            .query_row(
                "SELECT id, created_at FROM combos WHERE name = ?1",
                params![name],
                |r| Ok((uuid_col(r.get(0)?)?, r.get(1)?)),
            )
            .optional()
            .map_err(migrations::db_err)?;
        let (id, created_at) = existing.unwrap_or_else(|| (Uuid::new_v4(), now()));
        conn.execute(
            "INSERT INTO combos (id, name, models, created_at) VALUES (?1, ?2, ?3, ?4) \
             ON CONFLICT(name) DO UPDATE SET models = excluded.models",
            params![id.to_string(), name, json, created_at],
        )
        .map_err(migrations::db_err)?;
        Ok(Combo {
            id,
            name: name.to_string(),
            models: unique,
            created_at,
        })
    }

    pub async fn get_combo(&self, name: &str) -> Result<Option<Combo>> {
        let conn = self.conn();
        let row = conn
            .query_row(
                "SELECT id, name, models, created_at FROM combos WHERE name = ?1",
                params![name],
                row_to_combo,
            )
            .optional()
            .map_err(migrations::db_err)?;
        row.map(decode_combo).transpose()
    }

    pub async fn list_combos(&self) -> Result<Vec<Combo>> {
        let rows = {
            let conn = self.conn();
            let mut stmt = conn
                .prepare("SELECT id, name, models, created_at FROM combos ORDER BY name ASC")
                .map_err(migrations::db_err)?;
            let rows = stmt.query_map([], row_to_combo).map_err(migrations::db_err)?;
            let mut out = Vec::new();
            for r in rows {
                out.push(r.map_err(migrations::db_err)?);
            }
            out
        };
        rows.into_iter().map(decode_combo).collect()
    }

    pub async fn delete_combo(&self, name: &str) -> Result<bool> {
        let conn = self.conn();
        let n = conn
            .execute("DELETE FROM combos WHERE name = ?1", params![name])
            .map_err(migrations::db_err)?;
        Ok(n > 0)
    }

    // ----- prompt templates --------------------------------------------------------

    /// Create a template at version 1 and record that revision in its history.
    ///
    /// Name and body limits are enforced here rather than in the route: the store is the
    /// only writer, so an operator's script talking to the database directly gets the
    /// same rule as the admin API.
    pub async fn create_template(&self, name: &str, body: &str) -> Result<PromptTemplate> {
        validate_template(name, body)?;
        let id = Uuid::new_v4().to_string();
        let now = now();
        let mut conn = self.conn();
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(migrations::db_err)?;
        let taken: Option<String> = tx
            .query_row(
                "SELECT id FROM prompt_templates WHERE name = ?1",
                params![name],
                |r| r.get(0),
            )
            .optional()
            .map_err(migrations::db_err)?;
        if taken.is_some() {
            return Err(Error::BadRequest(format!(
                "template name already exists: {name}"
            )));
        }
        tx.execute(
            "INSERT INTO prompt_templates (id, name, body, version, created_at, updated_at) \
             VALUES (?1, ?2, ?3, 1, ?4, ?4)",
            params![id, name, body, now],
        )
        .map_err(migrations::db_err)?;
        insert_template_version(&tx, &id, 1, body, now)?;
        tx.commit().map_err(migrations::db_err)?;
        Ok(PromptTemplate {
            id,
            name: name.to_string(),
            body: body.to_string(),
            version: 1,
            created_at: now,
            updated_at: now,
        })
    }

    pub async fn get_template(&self, id: &str) -> Result<Option<PromptTemplate>> {
        let conn = self.conn();
        conn.query_row(
            &format!("SELECT {TEMPLATE_COLUMNS} FROM prompt_templates WHERE id = ?1"),
            params![id],
            row_to_template,
        )
        .optional()
        .map_err(migrations::db_err)
    }

    /// Lookup by the unique name. The gateway uses it to tell a duplicate name (409)
    /// from any other write failure.
    pub async fn get_template_by_name(&self, name: &str) -> Result<Option<PromptTemplate>> {
        let conn = self.conn();
        conn.query_row(
            &format!("SELECT {TEMPLATE_COLUMNS} FROM prompt_templates WHERE name = ?1"),
            params![name],
            row_to_template,
        )
        .optional()
        .map_err(migrations::db_err)
    }

    pub async fn list_templates(&self) -> Result<Vec<PromptTemplate>> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {TEMPLATE_COLUMNS} FROM prompt_templates ORDER BY name ASC"
            ))
            .map_err(migrations::db_err)?;
        let rows = stmt.query_map([], row_to_template).map_err(migrations::db_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(migrations::db_err)?);
        }
        Ok(out)
    }

    /// Rename and/or republish a template. A body change appends a revision and bumps
    /// [`PromptTemplate::version`]; a rename alone leaves the version and `updated_at`
    /// untouched, because neither describes the prompt text.
    pub async fn update_template(
        &self,
        id: &str,
        name: Option<&str>,
        body: &str,
    ) -> Result<PromptTemplate> {
        let mut conn = self.conn();
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(migrations::db_err)?;
        let existing: Option<PromptTemplate> = tx
            .query_row(
                &format!("SELECT {TEMPLATE_COLUMNS} FROM prompt_templates WHERE id = ?1"),
                params![id],
                row_to_template,
            )
            .optional()
            .map_err(migrations::db_err)?;
        let Some(mut row) = existing else {
            return Err(Error::NotFound(format!("template {id}")));
        };
        let name = name.unwrap_or(&row.name);
        validate_template(name, body)?;
        if name != row.name {
            let conflict: Option<String> = tx
                .query_row(
                    "SELECT id FROM prompt_templates WHERE name = ?1 AND id <> ?2",
                    params![name, id],
                    |r| r.get(0),
                )
                .optional()
                .map_err(migrations::db_err)?;
            if conflict.is_some() {
                return Err(Error::BadRequest(format!(
                    "template name already exists: {name}"
                )));
            }
            row.name = name.to_string();
        }
        if body != row.body {
            row.version += 1;
            row.body = body.to_string();
            row.updated_at = now();
            insert_template_version(&tx, id, row.version, body, row.updated_at)?;
        }
        tx.execute(
            "UPDATE prompt_templates SET name = ?2, body = ?3, version = ?4, updated_at = ?5 \
             WHERE id = ?1",
            params![id, row.name, row.body, row.version, row.updated_at],
        )
        .map_err(migrations::db_err)?;
        tx.commit().map_err(migrations::db_err)?;
        Ok(row)
    }

    /// Delete a template and every revision it owns, atomically. `false` means no such id.
    pub async fn delete_template(&self, id: &str) -> Result<bool> {
        let mut conn = self.conn();
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(migrations::db_err)?;
        tx.execute(
            "DELETE FROM prompt_template_versions WHERE template_id = ?1",
            params![id],
        )
        .map_err(migrations::db_err)?;
        let deleted = tx
            .execute("DELETE FROM prompt_templates WHERE id = ?1", params![id])
            .map_err(migrations::db_err)?;
        tx.commit().map_err(migrations::db_err)?;
        Ok(deleted > 0)
    }

    /// Revision history, newest first. Empty when the id has no revisions.
    pub async fn template_versions(&self, id: &str) -> Result<Vec<PromptTemplateVersion>> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(
                "SELECT version, body, created_at FROM prompt_template_versions \
                 WHERE template_id = ?1 ORDER BY version DESC",
            )
            .map_err(migrations::db_err)?;
        let rows = stmt
            .query_map(params![id], |r| {
                Ok(PromptTemplateVersion {
                    version: r.get(0)?,
                    body: r.get(1)?,
                    created_at: r.get(2)?,
                })
            })
            .map_err(migrations::db_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(migrations::db_err)?);
        }
        Ok(out)
    }

    // ----- response cache ----------------------------------------------------------

    /// Serve a cached response, or `None` when the key is absent or expired.
    ///
    /// An expired row is deleted here rather than by a sweeper: the only moment the
    /// gateway can know an entry is useless is the read that wanted it, and a delete
    /// costs one indexed statement on a path that already touched the row.
    pub async fn cache_get(&self, key: &str, now: i64) -> Result<Option<String>> {
        let conn = self.conn();
        let row: Option<(String, i64)> = conn
            .query_row(
                "SELECT body, expires_at FROM response_cache WHERE key = ?1",
                params![key],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(migrations::db_err)?;
        let Some((body, expires_at)) = row else {
            return Ok(None);
        };
        if expires_at <= now {
            conn.execute("DELETE FROM response_cache WHERE key = ?1", params![key])
                .map_err(migrations::db_err)?;
            return Ok(None);
        }
        conn.execute(
            "UPDATE response_cache SET hits = hits + 1 WHERE key = ?1",
            params![key],
        )
        .map_err(migrations::db_err)?;
        Ok(Some(body))
    }

    /// Store (or replace) a cached response and keep the table bounded.
    ///
    /// `created_at` is the insertion time and doubles as "now" for the expiry sweep.
    /// Eviction is by `created_at` ascending, so the entries an operator was least
    /// recently served lose to the ones just written. The insert and both sweeps run
    /// under the single connection mutex, so a reader in this process never observes
    /// the table above `max_entries`.
    pub async fn cache_put(
        &self,
        key: &str,
        body: &str,
        created_at: i64,
        expires_at: i64,
        max_entries: usize,
    ) -> Result<()> {
        let max_entries = i64::try_from(max_entries.max(1)).unwrap_or(i64::MAX);
        let conn = self.conn();
        conn.execute(
            "INSERT INTO response_cache (key, body, created_at, expires_at, hits) \
             VALUES (?1, ?2, ?3, ?4, 0) \
             ON CONFLICT(key) DO UPDATE SET \
               body = excluded.body, created_at = excluded.created_at, \
               expires_at = excluded.expires_at, hits = 0",
            params![key, body, created_at, expires_at],
        )
        .map_err(migrations::db_err)?;
        conn.execute(
            "DELETE FROM response_cache WHERE expires_at <= ?1",
            params![created_at],
        )
        .map_err(migrations::db_err)?;
        conn.execute(
            "DELETE FROM response_cache WHERE rowid IN ( \
               SELECT rowid FROM response_cache ORDER BY created_at ASC, rowid ASC \
               LIMIT max(0, (SELECT count(*) FROM response_cache) - ?1))",
            params![max_entries],
        )
        .map_err(migrations::db_err)?;
        Ok(())
    }

    /// Every cached entry, oldest first. Ops/debugging view: the bodies are exactly the
    /// responses their clients already received.
    pub async fn cache_entries(&self) -> Result<Vec<CacheEntry>> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(
                "SELECT key, body, created_at, expires_at, hits FROM response_cache \
                 ORDER BY created_at ASC, rowid ASC",
            )
            .map_err(migrations::db_err)?;
        let rows = stmt
            .query_map([], |r| {
                Ok(CacheEntry {
                    key: r.get(0)?,
                    body: r.get(1)?,
                    created_at: r.get(2)?,
                    expires_at: r.get(3)?,
                    hits: u32::try_from(r.get::<_, i64>(4)?).unwrap_or(u32::MAX),
                })
            })
            .map_err(migrations::db_err)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(migrations::db_err)?);
        }
        Ok(out)
    }

    // ----- usage -------------------------------------------------------------------

    /// Append one usage row. Only counters and error text are stored, never payloads.
    pub async fn record_usage(&self, r: &UsageRecord) -> Result<()> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO usage \
             (ts, request_id, provider, model, connection_id, endpoint, key_id, input_tokens, \
              output_tokens, cache_read_tokens, cache_write_tokens, latency_ms, ttft_ms, status, \
              streamed, error, cost_micros) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
            params![
                r.ts,
                r.request_id.map(|u| u.to_string()).unwrap_or_default(),
                r.provider,
                r.model,
                r.connection_id.map(|u| u.to_string()),
                r.endpoint,
                r.key_id.map(|u| u.to_string()),
                r.input_tokens,
                r.output_tokens,
                r.cache_read_tokens,
                r.cache_write_tokens,
                r.latency_ms,
                r.ttft_ms,
                r.status,
                r.streamed as i64,
                r.error,
                r.cost_micros,
            ],
        )
        .map_err(migrations::db_err)?;
        Ok(())
    }

    /// Append one attempt row. Failures are recorded here and nowhere else, so this is
    /// the only per-candidate history the gateway keeps.
    pub async fn record_attempt(&self, a: &AttemptRecord) -> Result<()> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO attempts \
             (request_id, ts, attempt, provider, model, connection_id, status, error, latency_ms, \
              key_id) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                a.request_id.to_string(),
                a.ts,
                a.attempt,
                a.provider,
                a.model,
                a.connection_id.map(|u| u.to_string()),
                a.status,
                a.error,
                a.latency_ms,
                a.key_id.map(|u| u.to_string()),
            ],
        )
        .map_err(migrations::db_err)?;
        Ok(())
    }

    /// Every attempt for one request, in dispatch order.
    pub async fn attempts_for(&self, request_id: Uuid) -> Result<Vec<AttemptRecord>> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(
                "SELECT id, ts, attempt, provider, model, connection_id, status, error, \
                        latency_ms, key_id \
                 FROM attempts WHERE request_id = ?1 ORDER BY attempt ASC, id ASC",
            )
            .map_err(migrations::db_err)?;
        let rows = stmt
            .query_map(params![request_id.to_string()], |r| {
                let cid: Option<String> = r.get(5)?;
                let kid: Option<String> = r.get(9)?;
                Ok(AttemptRecord {
                    id: r.get(0)?,
                    request_id,
                    ts: r.get(1)?,
                    attempt: r.get(2)?,
                    provider: r.get(3)?,
                    model: r.get(4)?,
                    connection_id: cid.as_deref().and_then(|s| Uuid::parse_str(s).ok()),
                    status: r.get(6)?,
                    error: r.get(7)?,
                    latency_ms: r.get(8)?,
                    key_id: kid.as_deref().and_then(|s| Uuid::parse_str(s).ok()),
                })
            })
            .map_err(migrations::db_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(migrations::db_err)?);
        }
        Ok(out)
    }

    /// The usage row a request id belongs to, when the request finished successfully.
    pub async fn usage_by_request(&self, request_id: Uuid) -> Result<Option<UsageRecord>> {
        let conn = self.conn();
        conn.query_row(
            &format!("SELECT {USAGE_COLUMNS} FROM usage WHERE request_id = ?1 ORDER BY id DESC"),
            params![request_id.to_string()],
            row_to_usage,
        )
        .optional()
        .map_err(migrations::db_err)
    }

    /// Most recent records first, ordered `(ts, id)` descending. Every filter in `query`
    /// is applied in one SQL statement, so a filtered walk over history pages correctly.
    ///
    /// `before_ts` is an exclusive cursor. With `before_id` as well, the cursor is the
    /// pair: rows strictly after `(before_ts, before_id)` in that order are excluded, so
    /// rows sharing a timestamp across a page boundary are neither dropped nor repeated.
    /// Without `before_id`, only `ts < before_ts` is kept. `since` filters `ts >= since`.
    pub async fn recent_usage(&self, query: &UsageQuery) -> Result<Vec<UsageRecord>> {
        let limit = i64::try_from(query.limit).unwrap_or(i64::MAX);
        let key_id = query.key_id.map(|k| k.to_string());
        let status = query.status.map(StatusFilter::as_sql);
        let conn = self.conn();
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {USAGE_COLUMNS} FROM usage \
                 WHERE (?2 IS NULL OR ts >= ?2) \
                 AND (?3 IS NULL OR ts < ?3 OR (?4 IS NOT NULL AND ts = ?3 AND id < ?4)) \
                 AND (?5 IS NULL OR provider = ?5) \
                 AND (?6 IS NULL OR model = ?6 OR (?7 IS NOT NULL AND model = ?7)) \
                 AND (?8 IS NULL OR key_id = ?8) \
                 AND (?9 IS NULL \
                      OR (?9 = 'success' AND status BETWEEN 200 AND 299 AND error IS NULL) \
                      OR (?9 = 'error' AND (status >= 400 OR error IS NOT NULL))) \
                 ORDER BY ts DESC, id DESC LIMIT ?1"
            ))
            .map_err(migrations::db_err)?;
        let rows = stmt
            .query_map(
                params![
                    limit,
                    query.since,
                    query.before_ts,
                    query.before_id,
                    query.provider.as_deref(),
                    query.model.as_deref(),
                    query.bare_model.as_deref(),
                    key_id.as_deref(),
                    status,
                ],
                row_to_usage,
            )
            .map_err(migrations::db_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(migrations::db_err)?);
        }
        Ok(out)
    }

    /// Totals per provider+model, optionally restricted to rows at or after `since`.
    /// A request is an error when `status >= 400` or the row carries an error message.
    pub async fn usage_summary(&self, since: Option<i64>) -> Result<Vec<UsageSummary>> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(
                "SELECT provider, model, count(*), \
                        sum(CASE WHEN status >= 400 OR error IS NOT NULL THEN 1 ELSE 0 END), \
                        sum(input_tokens), sum(output_tokens), \
                        sum(cache_read_tokens), sum(cache_write_tokens), \
                        sum(cost_micros), avg(latency_ms) \
                 FROM usage WHERE ?1 IS NULL OR ts >= ?1 \
                 GROUP BY provider, model ORDER BY provider ASC, model ASC",
            )
            .map_err(migrations::db_err)?;
        let rows = stmt
            .query_map(params![since], |r| {
                let avg: f64 = r.get(9)?;
                Ok(UsageSummary {
                    provider: r.get(0)?,
                    model: r.get(1)?,
                    requests: r.get(2)?,
                    errors: r.get(3)?,
                    input_tokens: r.get(4)?,
                    output_tokens: r.get(5)?,
                    cache_read_tokens: r.get(6)?,
                    cache_write_tokens: r.get(7)?,
                    cost_micros: r.get(8)?,
                    avg_latency_ms: avg.round().clamp(0.0, u32::MAX as f64) as u32,
                })
            })
            .map_err(migrations::db_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(migrations::db_err)?);
        }
        Ok(out)
    }
}

impl Store {
    /// Aggregate usage into fixed-width buckets starting at `since`.
    ///
    /// Counters are grouped in SQL so a dashboard never has to fetch every event to
    /// draw a chart; `recent_usage` is capped at 1000 rows and cannot answer a week.
    pub async fn usage_series(&self, since: i64, bucket_secs: i64) -> Result<Vec<UsageBucket>> {
        let bucket_secs = bucket_secs.max(60);
        let conn = self.conn();
        let mut stmt = conn
            .prepare(
                "SELECT (ts / ?2) * ?2 AS bucket, count(*), \
                        sum(CASE WHEN status >= 400 OR error IS NOT NULL THEN 1 ELSE 0 END), \
                        sum(input_tokens), sum(output_tokens), \
                        sum(cache_read_tokens), sum(cache_write_tokens), \
                        sum(cost_micros), avg(latency_ms) \
                 FROM usage WHERE ts >= ?1 GROUP BY bucket ORDER BY bucket ASC",
            )
            .map_err(migrations::db_err)?;
        let rows = stmt
            .query_map(params![since, bucket_secs], |r| {
                let avg: f64 = r.get(8)?;
                Ok(UsageBucket {
                    start: r.get(0)?,
                    requests: r.get(1)?,
                    errors: r.get(2)?,
                    input_tokens: r.get(3)?,
                    output_tokens: r.get(4)?,
                    cache_read_tokens: r.get(5)?,
                    cache_write_tokens: r.get(6)?,
                    cost_micros: r.get(7)?,
                    avg_latency_ms: avg.round().clamp(0.0, u32::MAX as f64) as u32,
                })
            })
            .map_err(migrations::db_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(migrations::db_err)?);
        }
        Ok(out)
    }

    /// Per-model buckets, for a traffic chart that stacks one segment per model.
    ///
    /// The chart needs at most a few thousand rows (30 days × models in use), which is
    /// why this groups in SQL instead of shipping every event to the browser.
    pub async fn usage_series_by_model(
        &self,
        since: i64,
        bucket_secs: i64,
    ) -> Result<Vec<UsageModelBucket>> {
        let bucket_secs = bucket_secs.max(60);
        let conn = self.conn();
        let mut stmt = conn
            .prepare(
                "SELECT (ts / ?2) * ?2 AS bucket, provider, model, count(*), \
                        sum(CASE WHEN status >= 400 OR error IS NOT NULL THEN 1 ELSE 0 END), \
                        sum(input_tokens), sum(output_tokens), \
                        sum(cache_read_tokens), sum(cache_write_tokens), \
                        sum(cost_micros), avg(latency_ms) \
                 FROM usage WHERE ts >= ?1 \
                 GROUP BY bucket, provider, model ORDER BY bucket ASC, provider ASC, model ASC",
            )
            .map_err(migrations::db_err)?;
        let rows = stmt
            .query_map(params![since, bucket_secs], |r| {
                let avg: f64 = r.get(10)?;
                Ok(UsageModelBucket {
                    start: r.get(0)?,
                    provider: r.get(1)?,
                    model: r.get(2)?,
                    requests: r.get(3)?,
                    errors: r.get(4)?,
                    input_tokens: r.get(5)?,
                    output_tokens: r.get(6)?,
                    cache_read_tokens: r.get(7)?,
                    cache_write_tokens: r.get(8)?,
                    cost_micros: r.get(9)?,
                    avg_latency_ms: avg.round().clamp(0.0, u32::MAX as f64) as u32,
                })
            })
            .map_err(migrations::db_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(migrations::db_err)?);
        }
        Ok(out)
    }

    // ----- maintenance --------------------------------------------------------------

    /// Delete `usage` and `attempts` rows older than `cutoff` (unix seconds).
    ///
    /// One immediate transaction covers both tables: a sweep is either fully applied or
    /// not at all, and no reader observes usage rows whose attempt trail already went.
    /// Rows exactly at `cutoff` survive — retention is "keep this many days", inclusive
    /// at the boundary.
    pub async fn prune_usage(&self, cutoff: i64) -> Result<PruneReport> {
        let mut conn = self.conn();
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(migrations::db_err)?;
        let usage = tx
            .execute("DELETE FROM usage WHERE ts < ?1", params![cutoff])
            .map_err(migrations::db_err)?;
        let attempts = tx
            .execute("DELETE FROM attempts WHERE ts < ?1", params![cutoff])
            .map_err(migrations::db_err)?;
        tx.commit().map_err(migrations::db_err)?;
        Ok(PruneReport { usage, attempts })
    }

    /// Delete every response-cache row that can no longer be served at `now`.
    ///
    /// Independent of usage retention: a cache entry's life is its own TTL, so expired
    /// rows go even when the operator keeps history forever. The expiry index makes
    /// this one range delete.
    pub async fn prune_expired_cache(&self, now: i64) -> Result<usize> {
        let mut conn = self.conn();
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(migrations::db_err)?;
        let deleted = tx
            .execute("DELETE FROM response_cache WHERE expires_at < ?1", params![now])
            .map_err(migrations::db_err)?;
        tx.commit().map_err(migrations::db_err)?;
        Ok(deleted)
    }

    /// Let SQLite refresh its query-planner statistics.
    ///
    /// `PRAGMA optimize` is cheap when it has nothing to do — it checks whether the
    /// schema or workload changed since the last run — and is the documented way to
    /// keep plans sane after the large deletes a retention sweep performs.
    pub async fn optimize(&self) -> Result<()> {
        let conn = self.conn();
        conn.execute_batch("PRAGMA optimize")
            .map_err(migrations::db_err)
    }
}

impl CredentialStore for Store {
    fn load<'a>(
        &'a self,
        id: Uuid,
    ) -> Pin<Box<dyn Future<Output = Result<Credentials>> + Send + 'a>> {
        Box::pin(async move {
            self.get_connection(id)
                .await?
                .map(|c| c.credentials)
                .ok_or_else(|| Error::NotFound(format!("connection {id}")))
        })
    }

    fn merge<'a>(
        &'a self,
        id: Uuid,
        patch: Credentials,
    ) -> Pin<Box<dyn Future<Output = Result<Credentials>> + Send + 'a>> {
        Box::pin(async move { Ok(self.merge_credentials(id, patch).await?.credentials) })
    }
}

// ----- helpers ---------------------------------------------------------------------

const CONN_COLUMNS: &str = "id, provider, auth_type, name, email, priority, is_active, data, \
                            backoff_level, last_error, last_error_code, last_error_at, \
                            created_at, updated_at, weight";

/// Columns of a [`PromptTemplate`], in the order [`row_to_template`] reads them. Shared
/// by every reader so a new column cannot desynchronize one query from the rest.
const TEMPLATE_COLUMNS: &str = "id, name, body, version, created_at, updated_at";

/// Operator-facing limits. A template is a prompt, not a document store: 100 characters
/// of name is a label, and 100k characters of body covers every published system prompt
/// while keeping one admin response small enough to inline.
const TEMPLATE_NAME_MAX: usize = 100;
const TEMPLATE_BODY_MAX: usize = 100_000;

/// Reject a template the API would accept but the product cannot render. Counted in
/// characters, not bytes, so a non-ASCII prompt is not penalised.
fn validate_template(name: &str, body: &str) -> Result<()> {
    if name.trim().is_empty() {
        return Err(Error::BadRequest("template name is required".to_string()));
    }
    if name.chars().count() > TEMPLATE_NAME_MAX {
        return Err(Error::BadRequest(format!(
            "template name must be at most {TEMPLATE_NAME_MAX} characters"
        )));
    }
    if body.chars().count() > TEMPLATE_BODY_MAX {
        return Err(Error::BadRequest(format!(
            "template body must be at most {TEMPLATE_BODY_MAX} characters"
        )));
    }
    Ok(())
}

fn row_to_template(r: &Row<'_>) -> rusqlite::Result<PromptTemplate> {
    Ok(PromptTemplate {
        id: r.get(0)?,
        name: r.get(1)?,
        body: r.get(2)?,
        version: r.get(3)?,
        created_at: r.get(4)?,
        updated_at: r.get(5)?,
    })
}

/// Append one revision. Callers own the transaction, so the history row and the live row
/// commit together.
fn insert_template_version(
    tx: &rusqlite::Transaction<'_>,
    template_id: &str,
    version: i64,
    body: &str,
    created_at: i64,
) -> Result<()> {
    tx.execute(
        "INSERT INTO prompt_template_versions (template_id, version, body, created_at) \
         VALUES (?1, ?2, ?3, ?4)",
        params![template_id, version, body, created_at],
    )
    .map_err(migrations::db_err)?;
    Ok(())
}

/// Weighted-strategy bounds. The column is clamped on read and write so a hand-edited
/// database cannot produce a zero-traffic account or an unbounded weighted cycle.
const MIN_WEIGHT: u32 = 1;
const MAX_WEIGHT: u32 = 1000;

/// Row with the credential blob still sealed.
struct RawConnection {
    id: Uuid,
    provider: String,
    auth_type: String,
    name: Option<String>,
    email: Option<String>,
    priority: i32,
    is_active: bool,
    data: Vec<u8>,
    backoff_level: u32,
    last_error: Option<String>,
    last_error_code: Option<u16>,
    last_error_at: Option<i64>,
    created_at: i64,
    updated_at: i64,
    weight: u32,
}

fn uuid_col(s: String) -> rusqlite::Result<Uuid> {
    Uuid::parse_str(&s).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })
}

/// Columns of a [`UsageRecord`], in the order [`row_to_usage`] reads them. Shared by
/// every reader so a new column cannot desynchronize one query from the rest.
const USAGE_COLUMNS: &str = "id, ts, request_id, provider, model, connection_id, endpoint, \
     input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, latency_ms, ttft_ms, \
     status, streamed, error, key_id, cost_micros";

/// Empty `request_id` is the v8 "unknown" default written before request ids existed.
fn row_to_usage(r: &Row<'_>) -> rusqlite::Result<UsageRecord> {
    let cid: Option<String> = r.get(5)?;
    let rid: String = r.get(2)?;
    let kid: Option<String> = r.get(16)?;
    Ok(UsageRecord {
        id: r.get(0)?,
        ts: r.get(1)?,
        request_id: Uuid::parse_str(&rid).ok(),
        provider: r.get(3)?,
        model: r.get(4)?,
        connection_id: cid.as_deref().and_then(|s| Uuid::parse_str(s).ok()),
        endpoint: r.get(6)?,
        input_tokens: r.get(7)?,
        output_tokens: r.get(8)?,
        cache_read_tokens: r.get(9)?,
        cache_write_tokens: r.get(10)?,
        latency_ms: r.get(11)?,
        ttft_ms: r.get(12)?,
        status: r.get(13)?,
        streamed: r.get::<_, i64>(14)? != 0,
        error: r.get(15)?,
        key_id: kid.as_deref().and_then(|s| Uuid::parse_str(s).ok()),
        cost_micros: r.get(17)?,
    })
}

fn row_to_connection(r: &Row<'_>) -> rusqlite::Result<RawConnection> {
    Ok(RawConnection {
        id: uuid_col(r.get(0)?)?,
        provider: r.get(1)?,
        auth_type: r.get(2)?,
        name: r.get(3)?,
        email: r.get(4)?,
        priority: r.get(5)?,
        is_active: r.get::<_, i64>(6)? != 0,
        data: r.get(7)?,
        backoff_level: r.get(8)?,
        last_error: r.get(9)?,
        last_error_code: r.get(10)?,
        last_error_at: r.get(11)?,
        created_at: r.get(12)?,
        updated_at: r.get(13)?,
        weight: (r.get::<_, i64>(14)?).clamp(i64::from(MIN_WEIGHT), i64::from(MAX_WEIGHT)) as u32,
    })
}

fn row_to_api_key(r: &Row<'_>) -> rusqlite::Result<ApiKey> {
    // `allowed_models` is a JSON array of refs; a malformed blob is a storage failure
    // rather than a silent "allow everything".
    let raw_models: Option<String> = r.get(8)?;
    let allowed_models = match raw_models {
        Some(text) => Some(serde_json::from_str(&text).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(
                8,
                rusqlite::types::Type::Text,
                Box::new(e),
            )
        })?),
        None => None,
    };
    let raw_guardrails: Option<String> = r.get(12)?;
    // A malformed policy is a storage failure, never a silent "no policy": the key's
    // traffic would then run unfiltered while the operator believes otherwise.
    let guardrails = match raw_guardrails {
        Some(text) => Some(serde_json::from_str(&text).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(
                12,
                rusqlite::types::Type::Text,
                Box::new(e),
            )
        })?),
        None => None,
    };
    Ok(ApiKey {
        id: uuid_col(r.get(0)?)?,
        key_hash: r.get(1)?,
        name: r.get(2)?,
        is_active: r.get::<_, i64>(3)? != 0,
        created_at: r.get(4)?,
        rpm: r.get(5)?,
        tpm: r.get(6)?,
        budget_tokens: r.get(7)?,
        allowed_models,
        expires_at: r.get(9)?,
        budget_micros: r.get(10)?,
        budget_window: BudgetWindow::from_stored(&r.get::<_, String>(11)?),
        guardrails,
        scope: KeyScope::from_stored(&r.get::<_, String>(13)?),
    })
}

struct RawCombo {
    id: Uuid,
    name: String,
    models: String,
    created_at: i64,
}

fn row_to_combo(r: &Row<'_>) -> rusqlite::Result<RawCombo> {
    Ok(RawCombo {
        id: uuid_col(r.get(0)?)?,
        name: r.get(1)?,
        models: r.get(2)?,
        created_at: r.get(3)?,
    })
}

fn decode_combo(raw: RawCombo) -> Result<Combo> {
    let models: Vec<String> = serde_json::from_str(&raw.models).map_err(|e| {
        Error::Store(format!("combo {} has invalid models JSON: {e}", raw.name))
    })?;
    Ok(Combo {
        id: raw.id,
        name: raw.name,
        models,
        created_at: raw.created_at,
    })
}

/// Field-wise merge: every `Some` in `patch` overwrites; `None` never erases.
/// `extra` is merged key by key and a `null` value deletes a key.
fn merge_into(dst: &mut Credentials, patch: Credentials) {
    macro_rules! merge {
        ($($f:ident),* $(,)?) => {$(
            if patch.$f.is_some() {
                dst.$f = patch.$f;
            }
        )*};
    }
    merge!(
        access_token,
        refresh_token,
        api_key,
        id_token,
        expires_at,
        copilot_token,
        copilot_expires_at,
        account_label,
        base_url_override,
    );
    for (k, v) in patch.extra {
        if v.is_null() {
            dst.extra.remove(&k);
        } else {
            dst.extra.insert(k, v);
        }
    }
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn data_dir_for(path: &Path) -> Result<PathBuf> {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .ok_or_else(|| {
            Error::Store(format!(
                "database path {} has no parent directory to hold the key file",
                path.display()
            ))
        })
}

/// `sk-rr-` + 32 hex chars from the OS CSPRNG.
fn generate_api_key() -> String {
    let mut raw = [0u8; API_KEY_SECRET_HEX_LEN / 2];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut raw);
    let mut key = String::with_capacity(API_KEY_PREFIX.len() + API_KEY_SECRET_HEX_LEN);
    key.push_str(API_KEY_PREFIX);
    key.push_str(&hex::encode(raw));
    key
}

/// Key for one response-cache entry: SHA-256 hex of the canonical request payload.
///
/// Derived here, next to the table it keys, so a caller cannot store under one key and
/// read under another. The payload is the full canonical request (unbounded in length);
/// the stored key stays 64 hex characters.
pub fn cache_key(payload: &str) -> String {
    hex::encode(Sha256::digest(payload.as_bytes()))
}

fn hash_api_key(plaintext: &str) -> String {
    hex::encode(Sha256::digest(plaintext.as_bytes()))
}

/// Upsert that never lowers a row's source precedence. Kept as one statement so the
/// read-compare-write happens under SQLite's write lock.
const UPSERT_MODEL_SQL: &str = r#"
INSERT INTO models (provider, model, display, context, max_output, caps, cost, source, updated_at)
VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
ON CONFLICT(provider, model) DO UPDATE SET
  display    = excluded.display,
  context    = excluded.context,
  max_output = excluded.max_output,
  caps       = excluded.caps,
  cost       = coalesce(excluded.cost, models.cost),
  source     = excluded.source,
  updated_at = excluded.updated_at
WHERE CASE excluded.source WHEN 'custom' THEN 3 WHEN 'upstream' THEN 2 WHEN 'synced' THEN 1 ELSE 0 END
   >= CASE models.source   WHEN 'custom' THEN 3 WHEN 'upstream' THEN 2 WHEN 'synced' THEN 1 ELSE 0 END
"#;

/// The stored `cost` blob is JSON this crate wrote; a hand-edited value that does not
/// parse reads as "no price" instead of failing the accounting read.
fn parse_cost_blob(raw: String) -> serde_json::Value {
    serde_json::from_str(&raw).unwrap_or(serde_json::Value::Null)
}

/// Capability blob as a JSON object. Only this crate writes the column, so a malformed
/// value means a hand-edited database; treat it as "no overrides" rather than failing
/// the whole registry listing.
fn parse_caps(raw: String) -> serde_json::Value {
    match serde_json::from_str::<serde_json::Value>(&raw) {
        Ok(v @ serde_json::Value::Object(_)) => v,
        _ => serde_json::Value::Object(serde_json::Map::new()),
    }
}

#[cfg(test)]
mod window_tests {
    use super::{window_start, BudgetWindow};

    /// Unix seconds of a UTC wall-clock instant, written out so the expected values are
    /// independent of the code under test.
    const fn ts(y: i64, mo: i64, d: i64, h: i64, mi: i64) -> i64 {
        // Hinnant days-from-civil, duplicated here on purpose (not the code under test).
        let yy = if mo <= 2 { y - 1 } else { y };
        let era = yy / 400;
        let yoe = yy - era * 400;
        let mp = (mo + 9) % 12;
        let doy = (153 * mp + 2) / 5 + d - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        let days = era * 146_097 + doe - 719_468;
        days * 86_400 + h * 3_600 + mi * 60
    }

    #[test]
    fn total_is_all_time() {
        assert_eq!(window_start(BudgetWindow::Total, ts(2025, 6, 15, 12, 0)), None);
    }

    #[test]
    fn daily_starts_at_utc_midnight() {
        // Sun 2025-06-15 12:34 UTC -> Sun 2025-06-15 00:00 UTC.
        let now = ts(2025, 6, 15, 12, 34);
        assert_eq!(window_start(BudgetWindow::Daily, now), Some(ts(2025, 6, 15, 0, 0)));
        // Exactly midnight belongs to the new day.
        assert_eq!(
            window_start(BudgetWindow::Daily, ts(2025, 6, 16, 0, 0)),
            Some(ts(2025, 6, 16, 0, 0))
        );
        // One second before midnight is still the previous day.
        assert_eq!(
            window_start(BudgetWindow::Daily, ts(2025, 6, 16, 0, 0) - 1),
            Some(ts(2025, 6, 15, 0, 0))
        );
    }

    #[test]
    fn weekly_starts_on_monday() {
        // 2025-06-16 is a Monday; a Monday mid-day starts its own week.
        assert_eq!(
            window_start(BudgetWindow::Weekly, ts(2025, 6, 16, 9, 0)),
            Some(ts(2025, 6, 16, 0, 0))
        );
        // Sunday 2025-06-22 belongs to the week that began Monday 2025-06-16.
        assert_eq!(
            window_start(BudgetWindow::Weekly, ts(2025, 6, 22, 23, 59)),
            Some(ts(2025, 6, 16, 0, 0))
        );
        // Monday 00:00 starts a new week, Sunday 23:59:59 before it closes the old one.
        assert_eq!(
            window_start(BudgetWindow::Weekly, ts(2025, 6, 23, 0, 0)),
            Some(ts(2025, 6, 23, 0, 0))
        );
        assert_eq!(
            window_start(BudgetWindow::Weekly, ts(2025, 6, 23, 0, 0) - 1),
            Some(ts(2025, 6, 16, 0, 0))
        );
    }

    #[test]
    fn weekly_handles_the_unix_epoch_weekday() {
        // 1970-01-01 was a Thursday, so the week of Mon 1969-12-29 is the epoch's week.
        assert_eq!(window_start(BudgetWindow::Weekly, 0), Some(ts(1969, 12, 29, 0, 0)));
        assert_eq!(
            window_start(BudgetWindow::Weekly, ts(1970, 1, 4, 23, 0)),
            Some(ts(1969, 12, 29, 0, 0))
        );
    }

    #[test]
    fn monthly_starts_on_the_first() {
        assert_eq!(
            window_start(BudgetWindow::Monthly, ts(2025, 6, 15, 12, 0)),
            Some(ts(2025, 6, 1, 0, 0))
        );
        // The 1st itself opens the month; the last second of the previous month does not.
        assert_eq!(
            window_start(BudgetWindow::Monthly, ts(2025, 7, 1, 0, 0)),
            Some(ts(2025, 7, 1, 0, 0))
        );
        assert_eq!(
            window_start(BudgetWindow::Monthly, ts(2025, 7, 1, 0, 0) - 1),
            Some(ts(2025, 6, 1, 0, 0))
        );
    }

    #[test]
    fn monthly_rolls_over_january_first() {
        assert_eq!(
            window_start(BudgetWindow::Monthly, ts(2026, 1, 1, 0, 0) - 1),
            Some(ts(2025, 12, 1, 0, 0))
        );
        assert_eq!(
            window_start(BudgetWindow::Monthly, ts(2026, 1, 1, 0, 0)),
            Some(ts(2026, 1, 1, 0, 0))
        );
    }

    #[test]
    fn daily_and_weekly_roll_over_the_year_end() {
        // Wed 2025-12-31 is the last day of 2025; its day and week start in 2025.
        let now = ts(2025, 12, 31, 18, 0);
        assert_eq!(window_start(BudgetWindow::Daily, now), Some(ts(2025, 12, 31, 0, 0)));
        assert_eq!(window_start(BudgetWindow::Weekly, now), Some(ts(2025, 12, 29, 0, 0)));
        // The next day crosses into 2026 without a gap.
        assert_eq!(
            window_start(BudgetWindow::Daily, ts(2026, 1, 1, 0, 0)),
            Some(ts(2026, 1, 1, 0, 0))
        );
    }

    #[test]
    fn leap_day_is_a_real_month_boundary() {
        // 2024 is a leap year: Feb has 29 days, so Mar 1 opens after Feb 29.
        assert_eq!(
            window_start(BudgetWindow::Monthly, ts(2024, 3, 1, 0, 0) - 1),
            Some(ts(2024, 2, 1, 0, 0))
        );
        assert_eq!(
            window_start(BudgetWindow::Monthly, ts(2024, 2, 29, 12, 0)),
            Some(ts(2024, 2, 1, 0, 0))
        );
    }

    #[test]
    fn stored_unknown_window_reads_as_total() {
        assert_eq!(BudgetWindow::from_stored("hourly"), BudgetWindow::Total);
        assert_eq!(BudgetWindow::parse("hourly"), None);
        assert_eq!(BudgetWindow::parse("monthly"), Some(BudgetWindow::Monthly));
    }
}
