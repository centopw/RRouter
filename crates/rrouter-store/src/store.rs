//! `Store`: the single embedded SQLite database behind the gateway.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::types::Value as SqlValue;
use rusqlite::{params, params_from_iter, Connection as Sql, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use rrouter_core::{Error, Result};

use crate::teams::Team;

use crate::crypto::Cipher;
use crate::migrations;
use rrouter_auth::{CredentialStore, Credentials};

/// Prefix of every gateway API key. The remainder is 32 hex chars of CSPRNG output.
pub const API_KEY_PREFIX: &str = "sk-rr-";
const API_KEY_SECRET_HEX_LEN: usize = 32;
/// Longest human-readable API-key name.
pub const MAX_API_KEY_NAME: usize = 100;
/// Longest smart-router model reference.
pub const MAX_COMBO_ROUTER_MODEL: usize = 200;
/// Longest bare model id in a provider preference.
pub const MAX_MODEL_PREFERENCE_MODEL: usize = 200;
/// Longest provider id in a provider preference.
pub const MAX_MODEL_PREFERENCE_PROVIDER: usize = 100;

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
    /// Owning team. `None` means the account is shared/instance-wide: it serves global
    /// (non-team) keys and admin probes, and is deliberately **not** visible to any team
    /// scope — a team key must not spend an operator's shared account.
    pub team_id: Option<Uuid>,
    /// Dashboard attribution only: the user who added the account. Not an access grant,
    /// so it is never consulted for routing or visibility.
    pub owner_user_id: Option<Uuid>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Which provider accounts a read may see.
///
/// Two scopes, not a filter with an "include shared" flag: the difference between "the
/// whole instance" and "one tenant" is a security boundary, so it is a type rather than a
/// nullable argument whose `None` could be read either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionScope {
    /// Instance-wide view: every account, team-owned or shared. Admin/reporting reads and
    /// the routing of keys that belong to no team.
    Global,
    /// Strict tenant view: only rows whose `team_id` is exactly this team. Shared rows
    /// (`team_id IS NULL`) belong to the instance, not to any team, so they are excluded.
    Team(Uuid),
}

impl ConnectionScope {
    /// The scope a caller's team implies: a key outside every team is instance-wide.
    pub fn for_team(team_id: Option<Uuid>) -> Self {
        match team_id {
            Some(team) => Self::Team(team),
            None => Self::Global,
        }
    }
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
    /// Owning team; `None` = an instance-wide key. A team key is routed only to that
    /// team's provider accounts (see [`ConnectionScope`]).
    pub team_id: Option<Uuid>,
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
    /// Redact email addresses, phone numbers, payment cards and credential-shaped strings
    /// in this key's prompt and response. Off by default: redaction changes what the model
    /// and the client see, so it is an explicit per-key opt-in.
    #[serde(default, skip_serializing_if = "is_false")]
    pub redact_pii: bool,
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
        self.max_prompt_chars.is_none()
            && self.blocked.is_empty()
            && !self.scan_output
            && !self.redact_pii
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
    /// `Some(None)` clears the display name; a string is trimmed before storage.
    pub name: Option<Option<String>>,
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
        if let Some(name) = &self.name {
            key.name = name
                .as_deref()
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_string);
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

/// Limits for one upstream account (a connection) over a [`BudgetWindow`]. The window
/// shares the key budgets' UTC calendar boundaries, so `daily` means "since 00:00 UTC",
/// and every `0` limit means unlimited — a quota row with three zeros still records the
/// window the UI shows usage for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountQuota {
    pub connection_id: Uuid,
    pub window: BudgetWindow,
    pub max_requests: u64,
    pub max_tokens: u64,
    pub max_cost_micros: u64,
}

/// What one account spent inside a window. `tokens` counts every billable token the
/// usage row recorded (input, output and both cache directions), matching the key
/// budgets.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AccountUsage {
    pub connection_id: Uuid,
    pub requests: u64,
    pub tokens: u64,
    pub cost_micros: u64,
}

/// A quota row with the spend it is measured against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountQuotaState {
    pub quota: AccountQuota,
    pub usage: AccountUsage,
    pub exhausted: bool,
    /// End of the current window (unix seconds); `None` for an all-time quota.
    pub resets_at: Option<i64>,
}

/// Accounts the planner must skip, plus the shortest wait until one of them is usable
/// again — the `Retry-After` of the `429` when nobody else can serve the request.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExhaustedAccounts {
    pub connections: std::collections::HashSet<Uuid>,
    pub retry_after_secs: Option<u64>,
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

/// End of the window that starts at `start` (unix seconds), i.e. when a quota measured
/// from that start resets. `None` for all time.
pub fn window_reset(window: BudgetWindow, start: i64) -> Option<i64> {
    const DAY: i64 = 86_400;
    match window {
        BudgetWindow::Total => None,
        BudgetWindow::Daily => Some(start + DAY),
        BudgetWindow::Weekly => Some(start + 7 * DAY),
        BudgetWindow::Monthly => {
            let (year, month, _) = civil_from_days(start.div_euclid(DAY));
            let next_month = if month == 12 {
                days_from_civil(year + 1, 1, 1)
            } else {
                days_from_civil(year, month + 1, 1)
            };
            Some(next_month * DAY)
        }
    }
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

/// One operator choice: a bare model id is routed to this provider first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelPreference {
    pub model: String,
    pub provider: String,
    pub updated_at: i64,
}

/// Longest ping model reference.
pub const MAX_QUOTA_PING_MODEL: usize = 200;
/// Schema spelling of the auto-ping mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuotaPingMode {
    Off,
    Reset,
    Schedule,
    Smart,
}

impl QuotaPingMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Reset => "reset",
            Self::Schedule => "schedule",
            Self::Smart => "smart",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim() {
            "off" => Some(Self::Off),
            "reset" => Some(Self::Reset),
            "schedule" => Some(Self::Schedule),
            "smart" => Some(Self::Smart),
            _ => None,
        }
    }

    pub fn from_stored(raw: &str) -> Self {
        Self::parse(raw).unwrap_or(Self::Off)
    }
}

/// One connection's auto-ping configuration plus its last outcome.
#[derive(Debug, Clone, PartialEq)]
pub struct QuotaPing {
    pub connection_id: Uuid,
    pub mode: QuotaPingMode,
    pub anchor: String,
    pub timezone: String,
    pub window_hours: f64,
    pub slots_per_day: u32,
    pub ping_model: Option<String>,
    pub last_ping_at: Option<i64>,
    pub last_ping_reset_at: Option<i64>,
    pub last_ping_slot: Option<String>,
    pub last_error: Option<String>,
    pub updated_at: i64,
}

impl QuotaPing {
    pub fn defaults(connection_id: Uuid) -> Self {
        Self {
            connection_id,
            mode: QuotaPingMode::Off,
            anchor: "09:30".into(),
            timezone: "UTC".into(),
            window_hours: 5.0,
            slots_per_day: 3,
            ping_model: None,
            last_ping_at: None,
            last_ping_reset_at: None,
            last_ping_slot: None,
            last_error: None,
            updated_at: 0,
        }
    }
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
    pub strategy: ComboStrategy,
    /// Decision model for [`ComboStrategy::Smart`]; always `None` for the other
    /// strategies. Never a combo itself (that would recurse).
    pub router_model: Option<String>,
    /// Durable round-robin cursor. Internal: the API shows it, but create/update reset it.
    pub cursor: i64,
    pub created_at: i64,
}

/// How a combo orders its models for one request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComboStrategy {
    /// The stored order is the fallback chain.
    Fallback,
    /// Durable rotation: each request starts one model later and wraps.
    RoundRobin,
    /// A decision model picks one listed model; failure falls back to the first.
    Smart,
}

impl ComboStrategy {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Fallback => "fallback",
            Self::RoundRobin => "round_robin",
            Self::Smart => "smart",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim() {
            "fallback" => Some(Self::Fallback),
            "round_robin" => Some(Self::RoundRobin),
            "smart" => Some(Self::Smart),
            _ => None,
        }
    }

    /// Lenient read for a stored row: an unrecognised value is the pre-v31 behaviour.
    pub fn from_stored(raw: &str) -> Self {
        Self::parse(raw).unwrap_or(Self::Fallback)
    }
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
    /// W3C trace id this request ran under (v25 column), so the usage row joins the
    /// request log and its attempts. `None` for rows written before v25, and for a writer
    /// that has no trace context at all (a rerank row today; store/router tests).
    pub trace_id: Option<String>,
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
    /// Milliseconds from sending the upstream request to its response headers. `0` when
    /// the transport was not measured (passthroughs, old rows).
    pub upstream_ms: u32,
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
/// failure, timeout, breaker). A row is the per-candidate record of a request that
/// failed in the end; the request itself also gets one terminal usage row carrying the
/// client-facing error.
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

/// One row of the semantic response cache: a stored response and the prompt embedding
/// that justified it. `embedding` is raw little-endian `f32`s, opaque to SQLite.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticCacheEntry {
    /// SHA-256 of the canonical prompt: the exact-text dedupe key inside the scope.
    pub prompt_hash: String,
    pub embedding_model: String,
    pub provider: String,
    pub model: String,
    /// Owning team; `None` is the instance view, stored as the empty string so the
    /// unique index sees one key per scope. Scopes never overlap.
    pub team_id: Option<Uuid>,
    pub embedding: Vec<u8>,
    pub body: String,
    pub created_at: i64,
    pub expires_at: i64,
}

/// What one semantic lookup may see. Every dimension is an equality filter, so a team
/// never matches another team's row and a different embedding model never matches a
/// vector from another space.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SemanticCacheScope<'a> {
    pub embedding_model: &'a str,
    pub provider: &'a str,
    pub model: &'a str,
    pub team_id: Option<Uuid>,
}

/// One scan row: the stored vector stays raw so the caller compares in `f32`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticCacheCandidate {
    pub id: i64,
    pub prompt_hash: String,
    pub embedding: Vec<u8>,
    pub body: String,
    pub created_at: i64,
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
        std::fs::create_dir_all(&data_dir)
            .map_err(|e| Error::Store(format!("create data dir {}: {e}", data_dir.display())))?;
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
        let mut conn = Sql::open_with_flags(path, flags)
            .map_err(|e| Error::Store(format!("open database {}: {e}", path.display())))?;
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
    pub(crate) fn conn(&self) -> MutexGuard<'_, Sql> {
        self.conn.lock().unwrap_or_else(|p| p.into_inner())
    }

    // ----- account quotas ----------------------------------------------------------

    /// Create or replace one account's quota.
    pub async fn set_account_quota(&self, quota: &AccountQuota) -> Result<()> {
        self.conn()
            .execute(
                "INSERT INTO account_quotas \
                 (connection_id, window, max_requests, max_tokens, max_cost_micros, updated_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
                 ON CONFLICT(connection_id) DO UPDATE SET window = excluded.window, \
                 max_requests = excluded.max_requests, max_tokens = excluded.max_tokens, \
                 max_cost_micros = excluded.max_cost_micros, updated_at = excluded.updated_at",
                params![
                    quota.connection_id.to_string(),
                    quota.window.as_str(),
                    quota.max_requests.min(i64::MAX as u64) as i64,
                    quota.max_tokens.min(i64::MAX as u64) as i64,
                    quota.max_cost_micros.min(i64::MAX as u64) as i64,
                    now(),
                ],
            )
            .map_err(migrations::db_err)?;
        Ok(())
    }

    /// Drop one account's quota; idempotent, and the id is not validated (a row for a
    /// deleted connection may still exist and must be removable).
    pub async fn clear_account_quota(&self, id: Uuid) -> Result<()> {
        self.conn()
            .execute(
                "DELETE FROM account_quotas WHERE connection_id = ?1",
                params![id.to_string()],
            )
            .map_err(migrations::db_err)?;
        Ok(())
    }

    /// Every configured quota row. A row whose connection was deleted is still returned
    /// here; callers join against `list_connections` and ignore the orphans.
    pub async fn account_quotas(&self) -> Result<Vec<AccountQuota>> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(
                "SELECT connection_id, window, max_requests, max_tokens, max_cost_micros \
                 FROM account_quotas ORDER BY connection_id",
            )
            .map_err(migrations::db_err)?;
        let rows = stmt
            .query_map([], |r| {
                Ok(AccountQuota {
                    connection_id: Uuid::parse_str(&r.get::<_, String>(0)?).unwrap_or_default(),
                    window: BudgetWindow::from_stored(&r.get::<_, String>(1)?),
                    max_requests: r.get::<_, i64>(2)?.max(0) as u64,
                    max_tokens: r.get::<_, i64>(3)?.max(0) as u64,
                    max_cost_micros: r.get::<_, i64>(4)?.max(0) as u64,
                })
            })
            .map_err(migrations::db_err)?;
        rows.map(|r| r.map_err(migrations::db_err)).collect()
    }

    /// Usage per connection since one window start, aggregated in a single query so a
    /// caller that measures many quotas over the same window pays once.
    pub async fn account_usage_by_connection(
        &self,
        since: Option<i64>,
    ) -> Result<std::collections::HashMap<Uuid, AccountUsage>> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(
                "SELECT connection_id, count(*), sum(input_tokens + output_tokens + \
                 cache_read_tokens + cache_write_tokens), sum(cost_micros) FROM usage \
                 WHERE connection_id IS NOT NULL AND ts >= ?1 GROUP BY connection_id",
            )
            .map_err(migrations::db_err)?;
        let rows = stmt
            .query_map(params![since.unwrap_or(i64::MIN)], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, Option<i64>>(2)?.unwrap_or(0),
                    r.get::<_, Option<i64>>(3)?.unwrap_or(0),
                ))
            })
            .map_err(migrations::db_err)?;
        let mut out = std::collections::HashMap::new();
        for row in rows {
            let (raw, requests, tokens, cost_micros) = row.map_err(migrations::db_err)?;
            let Ok(id) = Uuid::parse_str(&raw) else {
                continue;
            };
            out.insert(
                id,
                AccountUsage {
                    connection_id: id,
                    requests: requests.max(0) as u64,
                    tokens: tokens.max(0) as u64,
                    cost_micros: cost_micros.max(0) as u64,
                },
            );
        }
        Ok(out)
    }

    /// Every quota row with the spend of its own window. Aggregates are grouped by
    /// distinct window start (at most three), never one query per account.
    pub async fn account_quota_states(&self) -> Result<Vec<AccountQuotaState>> {
        let now = now();
        let quotas = self.account_quotas().await?;
        let mut spent: std::collections::HashMap<
            Option<i64>,
            std::collections::HashMap<Uuid, AccountUsage>,
        > = std::collections::HashMap::new();
        let mut states = Vec::with_capacity(quotas.len());
        for quota in quotas {
            let since = window_start(quota.window, now);
            if let std::collections::hash_map::Entry::Vacant(slot) = spent.entry(since) {
                slot.insert(self.account_usage_by_connection(since).await?);
            }
            let usage = spent
                .get(&since)
                .and_then(|m| m.get(&quota.connection_id))
                .cloned()
                .unwrap_or(AccountUsage {
                    connection_id: quota.connection_id,
                    ..AccountUsage::default()
                });
            let exhausted = (quota.max_requests > 0 && usage.requests >= quota.max_requests)
                || (quota.max_tokens > 0 && usage.tokens >= quota.max_tokens)
                || (quota.max_cost_micros > 0 && usage.cost_micros >= quota.max_cost_micros);
            let resets_at = since.and_then(|start| window_reset(quota.window, start));
            states.push(AccountQuotaState {
                quota,
                usage,
                exhausted,
                resets_at,
            });
        }
        Ok(states)
    }

    /// Accounts over their quota right now, and the wait until the first one resets.
    /// Empty (and query-free) when no account has a quota row.
    pub async fn exhausted_accounts(&self) -> Result<ExhaustedAccounts> {
        let now = now();
        let mut out = ExhaustedAccounts::default();
        for state in self.account_quota_states().await? {
            if !state.exhausted {
                continue;
            }
            out.connections.insert(state.quota.connection_id);
            // An all-time quota never resets, so the wait is an operator-action hint.
            let wait = state
                .resets_at
                .map(|reset| (reset - now).max(1) as u64)
                .unwrap_or(60);
            out.retry_after_secs = Some(out.retry_after_secs.map_or(wait, |s| s.min(wait)));
        }
        Ok(out)
    }

    pub async fn create_connection(
        &self,
        provider: &str,
        auth_type: &str,
        credentials: Credentials,
        name: Option<&str>,
    ) -> Result<Connection> {
        self.create_connection_owned(provider, auth_type, credentials, name, None, None)
            .await
    }

    /// Create an account owned by `team_id` and attributed to `owner_user_id`.
    ///
    /// A named team must exist: an account pointing at a missing team would be visible to
    /// no team key and findable only by scanning the global list. `None` for `team_id`
    /// creates the shared/instance-wide account [`Self::create_connection`] always made.
    pub async fn create_connection_owned(
        &self,
        provider: &str,
        auth_type: &str,
        credentials: Credentials,
        name: Option<&str>,
        team_id: Option<Uuid>,
        owner_user_id: Option<Uuid>,
    ) -> Result<Connection> {
        if let Some(team) = team_id {
            if self.team(team).await?.is_none() {
                return Err(Error::NotFound(format!("team {team}")));
            }
        }
        let id = Uuid::new_v4();
        let blob = self.seal(&id, &credentials)?;
        let now = now();
        let email = credentials.account_label.clone();
        {
            let conn = self.conn();
            conn.execute(
                "INSERT INTO connections \
                 (id, provider, auth_type, name, email, priority, is_active, data, backoff_level, \
                  created_at, updated_at, team_id, owner_user_id) \
                 VALUES (?1, ?2, ?3, ?4, ?5, 0, 1, ?6, 0, ?7, ?7, ?8, ?9)",
                params![
                    id.to_string(),
                    provider,
                    auth_type,
                    name,
                    email,
                    blob,
                    now,
                    team_id.map(|t| t.to_string()),
                    owner_user_id.map(|u| u.to_string())
                ],
            )
            .map_err(migrations::db_err)?;
        }
        self.get_connection(id)
            .await?
            .ok_or_else(|| Error::Store("connection vanished after insert".to_string()))
    }

    /// Assign the owning team and dashboard attribution, or clear either with `None`.
    ///
    /// `team_id = None` makes the account shared/instance-wide again; it is the only way a
    /// team-owned account becomes visible to global keys other than the team that owned it.
    pub async fn set_connection_owner(
        &self,
        id: Uuid,
        team_id: Option<Uuid>,
        owner_user_id: Option<Uuid>,
    ) -> Result<()> {
        if let Some(team) = team_id {
            if self.team(team).await?.is_none() {
                return Err(Error::NotFound(format!("team {team}")));
            }
        }
        let conn = self.conn();
        let n = conn
            .execute(
                "UPDATE connections SET team_id = ?2, owner_user_id = ?3, updated_at = ?4 \
                 WHERE id = ?1",
                params![
                    id.to_string(),
                    team_id.map(|t| t.to_string()),
                    owner_user_id.map(|u| u.to_string()),
                    now()
                ],
            )
            .map_err(migrations::db_err)?;
        if n == 0 {
            return Err(Error::NotFound(format!("connection {id}")));
        }
        Ok(())
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

    /// Every account, team-owned or shared: the admin/reporting view.
    ///
    /// Ordering is `provider, priority, created_at` (or `priority, created_at` when one
    /// provider is named), which is what the dashboard and probes read. Routing asks for
    /// the scope it is entitled to through [`Self::list_connections_scoped`].
    pub async fn list_connections(&self, provider: Option<&str>) -> Result<Vec<Connection>> {
        self.list_connections_scoped(ConnectionScope::Global, provider)
            .await
    }

    /// The accounts visible to `scope`, in the same order [`Self::list_connections`] uses.
    ///
    /// `Global` returns every row. `Team(id)` returns only rows whose `team_id` is exactly
    /// `id`: the shared rows (`team_id IS NULL`) are the instance's, not that team's, so a
    /// team key cannot be routed to an operator's account by accident. The filter is in
    /// SQL, so no caller can forget to apply it in memory.
    pub async fn list_connections_scoped(
        &self,
        scope: ConnectionScope,
        provider: Option<&str>,
    ) -> Result<Vec<Connection>> {
        let raws = {
            let conn = self.conn();
            let mut where_: Vec<String> = Vec::new();
            let mut args: Vec<SqlValue> = Vec::new();
            if let Some(provider) = provider {
                args.push(SqlValue::Text(provider.to_string()));
                where_.push(format!("provider = ?{}", args.len()));
            }
            if let ConnectionScope::Team(team) = scope {
                args.push(SqlValue::Text(team.to_string()));
                where_.push(format!("team_id = ?{}", args.len()));
            }
            let filter = if where_.is_empty() {
                String::new()
            } else {
                format!(" WHERE {}", where_.join(" AND "))
            };
            // The provider-scoped read is ordered by priority alone; the unfiltered one
            // groups by provider first. Keeping both orders stable is what lets the
            // planner's no-quota ordering stay a property, not a coincidence.
            let order = if provider.is_some() {
                " ORDER BY priority ASC, created_at ASC"
            } else {
                " ORDER BY provider ASC, priority ASC, created_at ASC"
            };
            let mut stmt = conn
                .prepare(&format!(
                    "SELECT {CONN_COLUMNS} FROM connections{filter}{order}"
                ))
                .map_err(migrations::db_err)?;
            let rows = stmt
                .query_map(params_from_iter(args.iter()), row_to_connection)
                .map_err(migrations::db_err)?;
            let mut out = Vec::new();
            for r in rows {
                out.push(r.map_err(migrations::db_err)?);
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
            .execute(params![
                id.to_string(),
                backoff_level,
                last_error,
                code,
                now
            ])
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
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?)))
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
            Error::Store(format!(
                "credential blob for {} is not valid JSON: {e}",
                raw.id
            ))
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
            team_id: raw.team_id,
            owner_user_id: raw.owner_user_id,
            created_at: raw.created_at,
            updated_at: raw.updated_at,
        })
    }

    // ----- gateway API keys --------------------------------------------------------

    /// The key columns every read selects, in [`row_to_api_key`]'s order.
    const KEY_COLUMNS: &'static str =
        "id, key_hash, name, is_active, created_at, rpm, tpm, budget_tokens, allowed_models, \
         expires_at, budget_micros, budget_window, guardrails, scope, team_id";

    /// Generate `sk-rr-<32 hex>`, persist `SHA-256(plaintext)` only, return both.
    ///
    /// A new key is an inference key with no limits, no allowlist, no expiry and no team.
    pub async fn create_api_key(&self, name: Option<&str>) -> Result<(String, ApiKey)> {
        self.create_api_key_in_team(name, None).await
    }

    /// Same, assigned to `team_id` at insert time so a key is never briefly unattributed.
    pub async fn create_api_key_in_team(
        &self,
        name: Option<&str>,
        team_id: Option<Uuid>,
    ) -> Result<(String, ApiKey)> {
        // A key must never point at a team that does not exist: the row would be invisible
        // to every team report and impossible to release.
        if let Some(team) = team_id {
            if self.team(team).await?.is_none() {
                return Err(Error::NotFound(format!("team {team}")));
            }
        }
        let plaintext = generate_api_key();
        let key_hash = hash_api_key(&plaintext);
        let id = Uuid::new_v4();
        let created_at = now();
        let conn = self.conn();
        conn.execute(
            "INSERT INTO api_keys (id, key_hash, name, is_active, created_at, team_id) \
             VALUES (?1, ?2, ?3, 1, ?4, ?5)",
            params![
                id.to_string(),
                key_hash,
                name,
                created_at,
                team_id.map(|t| t.to_string())
            ],
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
                team_id,
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
        let rows = stmt
            .query_map([], row_to_api_key)
            .map_err(migrations::db_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(migrations::db_err)?);
        }
        Ok(out)
    }

    /// Keys assigned to one team, oldest first. `team_id` is never `None` here: the
    /// instance-wide keys are the ones a team report must not include.
    pub async fn list_api_keys_in_team(&self, team_id: Uuid) -> Result<Vec<ApiKey>> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {} FROM api_keys WHERE team_id = ?1 ORDER BY created_at ASC",
                Self::KEY_COLUMNS
            ))
            .map_err(migrations::db_err)?;
        let rows = stmt
            .query_map(params![team_id.to_string()], row_to_api_key)
            .map_err(migrations::db_err)?;
        rows.map(|r| r.map_err(migrations::db_err)).collect()
    }

    /// Assign a key to a team, or clear the assignment with `None`.
    ///
    /// Separate from [`ApiKeyPatch`] because tenancy is not part of the traffic policy: a
    /// patch that only moved a key between teams must not rewrite the limits, and a team
    /// must exist before a key can point at it.
    pub async fn set_api_key_team(&self, id: Uuid, team_id: Option<Uuid>) -> Result<bool> {
        if let Some(team) = team_id {
            if self.team(team).await?.is_none() {
                return Err(Error::NotFound(format!("team {team}")));
            }
        }
        let n = self
            .conn()
            .execute(
                "UPDATE api_keys SET team_id = ?2 WHERE id = ?1",
                params![id.to_string(), team_id.map(|t| t.to_string())],
            )
            .map_err(migrations::db_err)?;
        Ok(n > 0)
    }

    /// The team a key is assigned to, or `None` for instance-wide.
    ///
    /// Tenancy is read separately from [`ApiKey`] because it is not traffic policy: the
    /// governance struct stays the size the hot path wants, and a team report reads one
    /// indexed column instead of joining the whole key row.
    pub async fn key_team(&self, id: Uuid) -> Result<Option<Uuid>> {
        let raw: Option<Option<String>> = self
            .conn()
            .query_row(
                "SELECT team_id FROM api_keys WHERE id = ?1",
                params![id.to_string()],
                |r| r.get(0),
            )
            .optional()
            .map_err(migrations::db_err)?;
        Ok(raw.flatten().and_then(|raw| Uuid::parse_str(&raw).ok()))
    }

    /// The team a key belongs to, as a row, in one query.
    ///
    /// This is the single call the admission path needs to enforce a team budget: the key
    /// row on the hot path stays its old size, and nothing is fetched when the key is
    /// instance-wide (the common case).
    pub async fn key_team_row(&self, key_id: Uuid) -> Result<Option<Team>> {
        self.conn()
            .query_row(
                "SELECT t.id, t.name, t.created_at, t.budget_tokens, t.budget_micros, \
                        t.budget_window \
                 FROM teams t JOIN api_keys k ON k.team_id = t.id WHERE k.id = ?1",
                params![key_id.to_string()],
                |r| {
                    Ok(Team {
                        id: uuid_col(r.get(0)?)?,
                        name: r.get(1)?,
                        created_at: r.get(2)?,
                        budget_tokens: r.get(3)?,
                        budget_micros: r.get(4)?,
                        budget_window: BudgetWindow::from_stored(&r.get::<_, String>(5)?),
                    })
                },
            )
            .optional()
            .map_err(migrations::db_err)
    }

    /// Every key's team in one query, keyed by key id; a key absent from the map is
    /// instance-wide. Used by the list endpoint so N keys cost one statement.
    pub async fn key_teams(&self) -> Result<std::collections::HashMap<Uuid, Option<Uuid>>> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare("SELECT id, team_id FROM api_keys")
            .map_err(migrations::db_err)?;
        let rows = stmt
            .query_map([], |r| {
                let id: String = r.get(0)?;
                let team: Option<String> = r.get(1)?;
                Ok((id, team))
            })
            .map_err(migrations::db_err)?;
        let mut out = std::collections::HashMap::new();
        for row in rows {
            let (id, team) = row.map_err(migrations::db_err)?;
            if let Ok(id) = Uuid::parse_str(&id) {
                out.insert(id, team.and_then(|raw| Uuid::parse_str(&raw).ok()));
            }
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
        if let Some(Some(name)) = &patch.name {
            if name.trim().chars().count() > MAX_API_KEY_NAME {
                return Err(Error::BadRequest(format!(
                    "a key name is limited to {MAX_API_KEY_NAME} characters"
                )));
            }
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
             guardrails = ?10, scope = ?11, name = ?12 WHERE id = ?1",
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
                key.name,
            ],
        )
        .map_err(migrations::db_err)?;
        Ok(Some(key))
    }

    pub async fn delete_api_key(&self, id: Uuid) -> Result<bool> {
        let conn = self.conn();
        let n = conn
            .execute(
                "DELETE FROM api_keys WHERE id = ?1",
                params![id.to_string()],
            )
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
                Ok(KeyModel {
                    from: r.get(0)?,
                    to: r.get(1)?,
                })
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
    pub async fn key_models_all(&self) -> Result<std::collections::HashMap<Uuid, Vec<KeyModel>>> {
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
                    KeyModel {
                        from: r.get(1)?,
                        to: r.get(2)?,
                    },
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

    /// One alias target, without scanning the table. Routing reads this on the combo
    /// path, where only the exact name matters.
    pub async fn alias_target(&self, alias: &str) -> Result<Option<String>> {
        self.conn()
            .query_row(
                "SELECT target FROM model_aliases WHERE alias = ?1",
                params![alias],
                |r| r.get(0),
            )
            .optional()
            .map_err(migrations::db_err)
    }

    // ----- per-model provider preferences ------------------------------------------

    /// The preference for one bare model id.
    pub async fn model_preference(&self, model: &str) -> Result<Option<ModelPreference>> {
        self.conn()
            .query_row(
                "SELECT model, provider, updated_at FROM model_preferences WHERE model = ?1",
                params![model.trim()],
                |r| {
                    Ok(ModelPreference {
                        model: r.get(0)?,
                        provider: r.get(1)?,
                        updated_at: r.get(2)?,
                    })
                },
            )
            .optional()
            .map_err(migrations::db_err)
    }

    /// Every preference, by model id.
    pub async fn model_preferences(&self) -> Result<Vec<ModelPreference>> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare("SELECT model, provider, updated_at FROM model_preferences ORDER BY model ASC")
            .map_err(migrations::db_err)?;
        let rows = stmt
            .query_map([], |r| {
                Ok(ModelPreference {
                    model: r.get(0)?,
                    provider: r.get(1)?,
                    updated_at: r.get(2)?,
                })
            })
            .map_err(migrations::db_err)?;
        rows.map(|row| row.map_err(migrations::db_err)).collect()
    }

    /// Set or replace one model's preferred provider. The provider does not have to be
    /// connected now; the preference is a routing hint that survives disconnect.
    pub async fn set_model_preference(
        &self,
        model: &str,
        provider: &str,
    ) -> Result<ModelPreference> {
        let model = model.trim();
        let provider = provider.trim();
        if model.is_empty() {
            return Err(Error::BadRequest("a model id is required".into()));
        }
        if model.chars().count() > MAX_MODEL_PREFERENCE_MODEL {
            return Err(Error::BadRequest(format!(
                "a model id is limited to {MAX_MODEL_PREFERENCE_MODEL} characters"
            )));
        }
        if provider.is_empty() {
            return Err(Error::BadRequest("a provider is required".into()));
        }
        if provider.chars().count() > MAX_MODEL_PREFERENCE_PROVIDER {
            return Err(Error::BadRequest(format!(
                "a provider is limited to {MAX_MODEL_PREFERENCE_PROVIDER} characters"
            )));
        }
        let stamp = now();
        self.conn()
            .execute(
                "INSERT INTO model_preferences (model, provider, updated_at) \
                 VALUES (?1, ?2, ?3) \
                 ON CONFLICT(model) DO UPDATE SET \
                 provider = excluded.provider, updated_at = excluded.updated_at",
                params![model, provider, stamp],
            )
            .map_err(migrations::db_err)?;
        Ok(ModelPreference {
            model: model.to_string(),
            provider: provider.to_string(),
            updated_at: stamp,
        })
    }

    /// Remove one preference. `true` means a row existed.
    pub async fn clear_model_preference(&self, model: &str) -> Result<bool> {
        let n = self
            .conn()
            .execute(
                "DELETE FROM model_preferences WHERE model = ?1",
                params![model.trim()],
            )
            .map_err(migrations::db_err)?;
        Ok(n > 0)
    }

    // ----- quota auto-ping ---------------------------------------------------------

    /// Every stored ping configuration, ordered by connection id.
    pub async fn quota_pings(&self) -> Result<Vec<QuotaPing>> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(
                "SELECT connection_id, mode, anchor, timezone, window_hours, slots_per_day, \
                 ping_model, last_ping_at, last_ping_reset_at, last_ping_slot, last_error, \
                 updated_at FROM quota_pings ORDER BY connection_id ASC",
            )
            .map_err(migrations::db_err)?;
        let rows = stmt
            .query_map([], row_to_quota_ping)
            .map_err(migrations::db_err)?;
        rows.map(|row| row.map_err(migrations::db_err)).collect()
    }

    pub async fn quota_ping(&self, connection_id: Uuid) -> Result<Option<QuotaPing>> {
        self.conn()
            .query_row(
                "SELECT connection_id, mode, anchor, timezone, window_hours, slots_per_day, \
                 ping_model, last_ping_at, last_ping_reset_at, last_ping_slot, last_error, \
                 updated_at FROM quota_pings WHERE connection_id = ?1",
                params![connection_id.to_string()],
                row_to_quota_ping,
            )
            .optional()
            .map_err(migrations::db_err)
    }

    /// Insert or update the configuration half of a ping row. Last-ping fields are never
    /// overwritten by a settings save, so a concurrent scheduler result survives.
    pub async fn upsert_quota_ping(&self, ping: &QuotaPing) -> Result<QuotaPing> {
        let anchor = validate_anchor(&ping.anchor)?;
        let timezone = ping.timezone.trim();
        if timezone.is_empty() || timezone.chars().count() > 100 {
            return Err(Error::BadRequest(
                "a timezone is required (at most 100 characters)".into(),
            ));
        }
        if !(0.5..=24.0).contains(&ping.window_hours) {
            return Err(Error::BadRequest(
                "window_hours must be between 0.5 and 24".into(),
            ));
        }
        let max_slots = (24.0 / ping.window_hours).floor() as u32;
        if ping.slots_per_day == 0 || ping.slots_per_day > max_slots.max(1) {
            return Err(Error::BadRequest(format!(
                "slots_per_day must be between 1 and {max_slots}"
            )));
        }
        let ping_model = ping
            .ping_model
            .as_deref()
            .map(str::trim)
            .filter(|model| !model.is_empty());
        if ping_model.is_some_and(|model| model.chars().count() > MAX_QUOTA_PING_MODEL) {
            return Err(Error::BadRequest(format!(
                "ping_model is limited to {MAX_QUOTA_PING_MODEL} characters"
            )));
        }
        let stamp = now();
        self.conn()
            .execute(
                "INSERT INTO quota_pings \
                 (connection_id, mode, anchor, timezone, window_hours, slots_per_day, \
                  ping_model, last_ping_at, last_ping_reset_at, last_ping_slot, last_error, \
                  updated_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12) \
                 ON CONFLICT(connection_id) DO UPDATE SET mode = excluded.mode, \
                 anchor = excluded.anchor, timezone = excluded.timezone, \
                 window_hours = excluded.window_hours, slots_per_day = excluded.slots_per_day, \
                 ping_model = excluded.ping_model, updated_at = excluded.updated_at",
                params![
                    ping.connection_id.to_string(),
                    ping.mode.as_str(),
                    anchor,
                    timezone,
                    ping.window_hours,
                    ping.slots_per_day as i64,
                    ping_model,
                    ping.last_ping_at,
                    ping.last_ping_reset_at,
                    ping.last_ping_slot,
                    ping.last_error,
                    stamp
                ],
            )
            .map_err(migrations::db_err)?;
        self.quota_ping(ping.connection_id)
            .await?
            .ok_or_else(|| Error::Store("quota ping vanished after upsert".into()))
    }

    /// Record one ping outcome. `last_error = None` clears it; absent timestamps keep the
    /// stored value so a schedule success does not erase a reset marker and vice versa.
    pub async fn record_quota_ping(
        &self,
        connection_id: Uuid,
        last_ping_at: Option<i64>,
        last_ping_reset_at: Option<i64>,
        last_ping_slot: Option<&str>,
        last_error: Option<&str>,
    ) -> Result<()> {
        self.conn()
            .execute(
                "UPDATE quota_pings SET last_ping_at = COALESCE(?2, last_ping_at), \
                 last_ping_reset_at = COALESCE(?3, last_ping_reset_at), \
                 last_ping_slot = COALESCE(?4, last_ping_slot), last_error = ?5, \
                 updated_at = ?6 WHERE connection_id = ?1",
                params![
                    connection_id.to_string(),
                    last_ping_at,
                    last_ping_reset_at,
                    last_ping_slot,
                    last_error,
                    now()
                ],
            )
            .map_err(migrations::db_err)?;
        Ok(())
    }

    /// Forget the last reset/slot markers, used when a mode is switched off.
    pub async fn clear_quota_ping_schedule(&self, connection_id: Uuid) -> Result<()> {
        self.conn()
            .execute(
                "UPDATE quota_pings SET last_ping_reset_at = NULL, last_ping_slot = NULL, \
                 last_error = NULL, updated_at = ?2 WHERE connection_id = ?1",
                params![connection_id.to_string(), now()],
            )
            .map_err(migrations::db_err)?;
        Ok(())
    }

    pub async fn insert_quota_ping_sample(
        &self,
        connection_id: Uuid,
        window_id: &str,
        used_percent: f64,
        resets_at: Option<i64>,
        sampled_at: i64,
    ) -> Result<()> {
        self.conn()
            .execute(
                "INSERT INTO quota_ping_samples \
                 (connection_id, window_id, used_percent, resets_at, sampled_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    connection_id.to_string(),
                    window_id,
                    used_percent,
                    resets_at,
                    sampled_at
                ],
            )
            .map_err(migrations::db_err)?;
        Ok(())
    }

    pub async fn prune_quota_ping_samples(&self, before: i64) -> Result<usize> {
        self.conn()
            .execute(
                "DELETE FROM quota_ping_samples WHERE sampled_at < ?1",
                params![before],
            )
            .map_err(migrations::db_err)
    }

    /// Requests per connection since `since` (unix seconds). This is real gateway
    /// traffic only: pings deliberately write no usage rows.
    pub async fn recent_request_counts(
        &self,
        since: i64,
    ) -> Result<std::collections::HashMap<Uuid, u64>> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(
                "SELECT connection_id, count(*) FROM usage \
                 WHERE connection_id IS NOT NULL AND ts >= ?1 GROUP BY connection_id",
            )
            .map_err(migrations::db_err)?;
        let rows = stmt
            .query_map(params![since], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
            })
            .map_err(migrations::db_err)?;
        let mut out = std::collections::HashMap::new();
        for row in rows {
            let (raw, count) = row.map_err(migrations::db_err)?;
            if let Ok(id) = Uuid::parse_str(&raw) {
                out.insert(id, count.max(0) as u64);
            }
        }
        Ok(out)
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
            conn.execute(
                "DELETE FROM disabled_models WHERE model = ?1",
                params![model],
            )
            .map_err(migrations::db_err)?;
        }
        Ok(())
    }

    pub async fn disabled_models(&self) -> Result<Vec<String>> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare("SELECT model FROM disabled_models ORDER BY model ASC")
            .map_err(migrations::db_err)?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(migrations::db_err)?;
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

    /// Insert or replace the chain named `name` with the legacy fallback strategy.
    pub async fn upsert_combo(&self, name: &str, models: &[String]) -> Result<Combo> {
        self.upsert_combo_with(name, models, ComboStrategy::Fallback, None)
            .await
    }

    /// Insert or replace a combo and its strategy.
    ///
    /// Validation lives here so a direct database writer gets the same rules as the API:
    /// `smart` names one decision model, that model is not this combo and does not
    /// resolve to a combo (including through one alias hop), and the other strategies
    /// store no decision model. An update resets the round-robin cursor.
    pub async fn upsert_combo_with(
        &self,
        name: &str,
        models: &[String],
        strategy: ComboStrategy,
        router_model: Option<&str>,
    ) -> Result<Combo> {
        let mut unique = Vec::with_capacity(models.len());
        for model in models {
            if !unique.contains(model) {
                unique.push(model.clone());
            }
        }
        let json = serde_json::to_string(&unique)?;
        let router_model = match strategy {
            ComboStrategy::Smart => {
                let raw = router_model
                    .ok_or_else(|| Error::BadRequest("smart combos require router_model".into()))?;
                let router = raw.trim();
                if router.is_empty() {
                    return Err(Error::BadRequest(
                        "smart combos require router_model".into(),
                    ));
                }
                if router.chars().count() > MAX_COMBO_ROUTER_MODEL {
                    return Err(Error::BadRequest(format!(
                        "router_model is limited to {MAX_COMBO_ROUTER_MODEL} characters"
                    )));
                }
                if router == name {
                    return Err(Error::BadRequest(
                        "router_model must not be the combo name".into(),
                    ));
                }
                Some(router.to_string())
            }
            ComboStrategy::Fallback | ComboStrategy::RoundRobin => None,
        };

        let mut conn = self.conn();
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(migrations::db_err)?;
        let existing: Option<(Uuid, i64)> = tx
            .query_row(
                "SELECT id, created_at FROM combos WHERE name = ?1",
                params![name],
                |r| Ok((uuid_col(r.get(0)?)?, r.get(1)?)),
            )
            .optional()
            .map_err(migrations::db_err)?;
        if let Some(router) = &router_model {
            if combo_reference(&tx, router, name)? {
                return Err(Error::BadRequest(
                    "router_model resolves to a combo; a smart combo cannot recurse".into(),
                ));
            }
        }
        let (id, created_at) = existing.unwrap_or_else(|| (Uuid::new_v4(), now()));
        tx.execute(
            "INSERT INTO combos (id, name, models, strategy, router_model, cursor, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6) \
             ON CONFLICT(name) DO UPDATE SET models = excluded.models, \
             strategy = excluded.strategy, router_model = excluded.router_model, cursor = 0",
            params![
                id.to_string(),
                name,
                json,
                strategy.as_str(),
                router_model,
                created_at
            ],
        )
        .map_err(migrations::db_err)?;
        tx.commit().map_err(migrations::db_err)?;
        Ok(Combo {
            id,
            name: name.to_string(),
            models: unique,
            strategy,
            router_model,
            cursor: 0,
            created_at,
        })
    }

    /// Durable round-robin start for one request.
    ///
    /// The cursor is read, the start is taken modulo the current model count, and the
    /// cursor advances in one immediate transaction, so two concurrent requests cannot
    /// observe the same position. `None` means no combo by that name.
    pub async fn next_combo_start(&self, name: &str) -> Result<Option<usize>> {
        let mut conn = self.conn();
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(migrations::db_err)?;
        let row: Option<(i64, String)> = tx
            .query_row(
                "SELECT cursor, models FROM combos WHERE name = ?1",
                params![name],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(migrations::db_err)?;
        let Some((cursor, models)) = row else {
            return Ok(None);
        };
        let count = serde_json::from_str::<Vec<String>>(&models)
            .map_err(|e| Error::Store(format!("combo {name} models: {e}")))?
            .len();
        if count == 0 {
            return Ok(Some(0));
        }
        let start = cursor.rem_euclid(count as i64) as usize;
        tx.execute(
            "UPDATE combos SET cursor = ?2 WHERE name = ?1",
            params![name, ((start + 1) % count) as i64],
        )
        .map_err(migrations::db_err)?;
        tx.commit().map_err(migrations::db_err)?;
        Ok(Some(start))
    }

    pub async fn get_combo(&self, name: &str) -> Result<Option<Combo>> {
        let conn = self.conn();
        let row = conn
            .query_row(
                "SELECT id, name, models, strategy, router_model, cursor, created_at FROM combos WHERE name = ?1",
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
                .prepare("SELECT id, name, models, strategy, router_model, cursor, created_at FROM combos ORDER BY name ASC")
                .map_err(migrations::db_err)?;
            let rows = stmt
                .query_map([], row_to_combo)
                .map_err(migrations::db_err)?;
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
        let rows = stmt
            .query_map([], row_to_template)
            .map_err(migrations::db_err)?;
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

    // ----- semantic response cache -------------------------------------------------

    /// Store (or refresh) one semantic entry and keep the table bounded.
    ///
    /// The unique `(embedding_model, provider, model, team, prompt_hash)` index means the
    /// same canonical prompt is stored once per scope, and re-storing it resets its age
    /// and hit count. Expired rows and over-cap rows are deleted with the same insert,
    /// so a reader in this process never observes a table above `max_entries`.
    pub async fn semantic_cache_put(
        &self,
        entry: &SemanticCacheEntry,
        max_entries: usize,
    ) -> Result<()> {
        let max_entries = i64::try_from(max_entries.max(1)).unwrap_or(i64::MAX);
        let conn = self.conn();
        conn.execute(
            "INSERT INTO semantic_cache \
             (prompt_hash, embedding_model, provider, model, team_id, embedding, body, \
              created_at, expires_at, hits) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 0) \
             ON CONFLICT(embedding_model, provider, model, team_id, prompt_hash) DO UPDATE SET \
               embedding = excluded.embedding, body = excluded.body, \
               created_at = excluded.created_at, expires_at = excluded.expires_at, hits = 0",
            params![
                entry.prompt_hash,
                entry.embedding_model,
                entry.provider,
                entry.model,
                entry
                    .team_id
                    .map(|team| team.to_string())
                    .unwrap_or_default(),
                entry.embedding,
                entry.body,
                entry.created_at,
                entry.expires_at,
            ],
        )
        .map_err(migrations::db_err)?;
        conn.execute(
            "DELETE FROM semantic_cache WHERE expires_at <= ?1",
            params![entry.created_at],
        )
        .map_err(migrations::db_err)?;
        conn.execute(
            "DELETE FROM semantic_cache WHERE id IN ( \
               SELECT id FROM semantic_cache ORDER BY created_at ASC, id ASC \
               LIMIT max(0, (SELECT count(*) FROM semantic_cache) - ?1))",
            params![max_entries],
        )
        .map_err(migrations::db_err)?;
        Ok(())
    }

    /// Un-expired entries visible to `scope`, newest first, bounded by `limit`.
    ///
    /// The filters are the scope contract, in SQL: a different embedding model is a
    /// different vector space, and a team row is invisible to the instance view (and
    /// vice versa). Expiry is filtered here too, so an expired row can never win a scan.
    pub async fn semantic_cache_scan(
        &self,
        scope: &SemanticCacheScope<'_>,
        now: i64,
        limit: usize,
    ) -> Result<Vec<SemanticCacheCandidate>> {
        let limit = i64::try_from(limit.clamp(1, 4096)).unwrap_or(1024);
        let conn = self.conn();
        let mut stmt = conn
            .prepare(
                "SELECT id, prompt_hash, embedding, body, created_at FROM semantic_cache \
                 WHERE embedding_model = ?1 AND provider = ?2 AND model = ?3 \
                   AND team_id = ?4 AND expires_at > ?5 \
                 ORDER BY id DESC LIMIT ?6",
            )
            .map_err(migrations::db_err)?;
        let rows = stmt
            .query_map(
                params![
                    scope.embedding_model,
                    scope.provider,
                    scope.model,
                    scope
                        .team_id
                        .map(|team| team.to_string())
                        .unwrap_or_default(),
                    now,
                    limit
                ],
                |r| {
                    Ok(SemanticCacheCandidate {
                        id: r.get(0)?,
                        prompt_hash: r.get(1)?,
                        embedding: r.get(2)?,
                        body: r.get(3)?,
                        created_at: r.get(4)?,
                    })
                },
            )
            .map_err(migrations::db_err)?;
        rows.map(|row| row.map_err(migrations::db_err)).collect()
    }

    /// Count one semantic hit. A missing row (evicted between scan and update) is a
    /// no-op: the response was already served.
    pub async fn semantic_cache_hit(&self, id: i64) -> Result<()> {
        self.conn()
            .execute(
                "UPDATE semantic_cache SET hits = hits + 1 WHERE id = ?1",
                params![id],
            )
            .map_err(migrations::db_err)?;
        Ok(())
    }

    /// Every semantic row's scope and age, for tests and ops. Bodies and vectors stay
    /// out of the view: nothing needs to read an operator's prompts from a debug tool.
    pub async fn semantic_cache_entries(&self) -> Result<Vec<(i64, String, String, String, i64)>> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(
                "SELECT id, embedding_model, provider, model, expires_at FROM semantic_cache \
                 ORDER BY id ASC",
            )
            .map_err(migrations::db_err)?;
        let rows = stmt
            .query_map([], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })
            .map_err(migrations::db_err)?;
        rows.map(|row| row.map_err(migrations::db_err)).collect()
    }

    // ----- usage -------------------------------------------------------------------
    /// Append one usage row. Only counters and error text are stored, never payloads.
    pub async fn record_usage(&self, r: &UsageRecord) -> Result<()> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO usage \
             (ts, request_id, provider, model, connection_id, endpoint, key_id, input_tokens, \
              output_tokens, cache_read_tokens, cache_write_tokens, latency_ms, ttft_ms, status, \
              streamed, error, cost_micros, upstream_ms, trace_id, team_id) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, \
                     ?19, (SELECT team_id FROM api_keys WHERE id = ?7))",
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
                r.upstream_ms,
                r.trace_id,
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

    // ----- team reporting -----------------------------------------------------------

    /// The request log restricted to `teams`.
    ///
    /// `None` is instance-wide (an admin), `Some(&[])` is deliberately empty (a signed-in
    /// account with no team must not fall back to "everything"), and a non-empty slice
    /// restricts to those teams. `usage.team_id` is attributed from the key at write time,
    /// so anonymous traffic and keys without a team stay out of every team view.
    pub async fn recent_usage_in_teams(
        &self,
        query: &UsageQuery,
        teams: Option<&[Uuid]>,
    ) -> Result<Vec<UsageRecord>> {
        let (team_sql, team_args) = team_filter(teams, 10);
        let key_id = query.key_id.map(|k| k.to_string());
        let status = query
            .status
            .map(|status| SqlValue::Text(StatusFilter::as_sql(status).to_string()))
            .unwrap_or(SqlValue::Null);
        let mut args: Vec<SqlValue> = vec![
            i64::try_from(query.limit).unwrap_or(i64::MAX).into(),
            query.since.into(),
            query.before_ts.into(),
            query.before_id.into(),
            query.provider.clone().into(),
            query.model.clone().into(),
            query.bare_model.clone().into(),
            key_id.into(),
            status,
        ];
        args.extend(team_args);
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
                 {team_sql} \
                 ORDER BY ts DESC, id DESC LIMIT ?1"
            ))
            .map_err(migrations::db_err)?;
        let rows = stmt
            .query_map(params_from_iter(args), row_to_usage)
            .map_err(migrations::db_err)?;
        rows.map(|r| r.map_err(migrations::db_err)).collect()
    }

    /// Per provider+model totals restricted to `teams`; see [`Store::recent_usage_in_teams`].
    pub async fn usage_summary_in_teams(
        &self,
        since: Option<i64>,
        teams: Option<&[Uuid]>,
    ) -> Result<Vec<UsageSummary>> {
        let (team_sql, team_args) = team_filter(teams, 2);
        let mut args: Vec<SqlValue> = vec![since.into()];
        args.extend(team_args);
        let conn = self.conn();
        let mut stmt = conn
            .prepare(&format!(
                "SELECT provider, model, count(*), \
                        sum(CASE WHEN status >= 400 OR error IS NOT NULL THEN 1 ELSE 0 END), \
                        sum(input_tokens), sum(output_tokens), \
                        sum(cache_read_tokens), sum(cache_write_tokens), \
                        sum(cost_micros), avg(latency_ms) \
                 FROM usage WHERE (?1 IS NULL OR ts >= ?1) {team_sql} \
                 GROUP BY provider, model ORDER BY provider ASC, model ASC"
            ))
            .map_err(migrations::db_err)?;
        let rows = stmt
            .query_map(params_from_iter(args), |r| {
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
        rows.map(|r| r.map_err(migrations::db_err)).collect()
    }

    /// Time buckets restricted to `teams`; see [`Store::recent_usage_in_teams`].
    pub async fn usage_series_in_teams(
        &self,
        since: i64,
        bucket_secs: i64,
        teams: Option<&[Uuid]>,
    ) -> Result<Vec<UsageBucket>> {
        let bucket_secs = bucket_secs.max(60);
        let (team_sql, team_args) = team_filter(teams, 3);
        let mut args: Vec<SqlValue> = vec![bucket_secs.into(), since.into()];
        args.extend(team_args);
        let conn = self.conn();
        let mut stmt = conn
            .prepare(&format!(
                "SELECT (ts / ?1) * ?1 AS bucket, count(*), \
                        sum(CASE WHEN status >= 400 OR error IS NOT NULL THEN 1 ELSE 0 END), \
                        sum(input_tokens), sum(output_tokens), \
                        sum(cache_read_tokens), sum(cache_write_tokens), \
                        sum(cost_micros), avg(latency_ms) \
                 FROM usage WHERE ts >= ?2 {team_sql} \
                 GROUP BY bucket ORDER BY bucket ASC"
            ))
            .map_err(migrations::db_err)?;
        let rows = stmt
            .query_map(params_from_iter(args), |r| {
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
        rows.map(|r| r.map_err(migrations::db_err)).collect()
    }

    /// Per-model buckets restricted to `teams`; see [`Store::recent_usage_in_teams`].
    pub async fn usage_series_by_model_in_teams(
        &self,
        since: i64,
        bucket_secs: i64,
        teams: Option<&[Uuid]>,
    ) -> Result<Vec<UsageModelBucket>> {
        let bucket_secs = bucket_secs.max(60);
        let (team_sql, team_args) = team_filter(teams, 3);
        let mut args: Vec<SqlValue> = vec![bucket_secs.into(), since.into()];
        args.extend(team_args);
        let conn = self.conn();
        let mut stmt = conn
            .prepare(&format!(
                "SELECT (ts / ?1) * ?1 AS bucket, provider, model, count(*), \
                        sum(CASE WHEN status >= 400 OR error IS NOT NULL THEN 1 ELSE 0 END), \
                        sum(input_tokens), sum(output_tokens), \
                        sum(cache_read_tokens), sum(cache_write_tokens), \
                        sum(cost_micros), avg(latency_ms) \
                 FROM usage WHERE ts >= ?2 {team_sql} \
                 GROUP BY bucket, provider, model ORDER BY bucket ASC, provider ASC, model ASC"
            ))
            .map_err(migrations::db_err)?;
        let rows = stmt
            .query_map(params_from_iter(args), |r| {
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
        rows.map(|r| r.map_err(migrations::db_err)).collect()
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
    /// this one range delete. The semantic cache follows the same rule in the same
    /// transaction — one sweep, two tables, so an operator sees one retention policy.
    pub async fn prune_expired_cache(&self, now: i64) -> Result<usize> {
        let mut conn = self.conn();
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(migrations::db_err)?;
        let deleted = tx
            .execute(
                "DELETE FROM response_cache WHERE expires_at < ?1",
                params![now],
            )
            .map_err(migrations::db_err)?;
        tx.execute(
            "DELETE FROM semantic_cache WHERE expires_at < ?1",
            params![now],
        )
        .map_err(migrations::db_err)?;
        tx.commit().map_err(migrations::db_err)?;
        Ok(deleted)
    }

    /// Let SQLite refresh its query-planner statistics.
    ///
    /// `PRAGMA optimize` is cheap when it has nothing to do — it checks whether the
    /// schema or workload changed since the last run — and is the documented way to
    /// keep plans sane after the large deletes a retention sweep performs.
    /// Consistent online backup to a new file (`VACUUM INTO`). The target must not
    /// exist, so a mistaken path cannot overwrite another database.
    pub async fn backup_to(&self, path: &std::path::Path) -> Result<()> {
        if path.exists() {
            return Err(Error::BadRequest(format!(
                "{} already exists; choose a new path",
                path.display()
            )));
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        self.conn()
            .execute("VACUUM INTO ?1", params![path.to_string_lossy()])
            .map_err(migrations::db_err)?;
        Ok(())
    }

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
                            created_at, updated_at, weight, team_id, owner_user_id";

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
    team_id: Option<Uuid>,
    owner_user_id: Option<Uuid>,
}

/// `(sql, args)` restricting a `usage` scan to `teams`, with numbered placeholders that
/// start at `first`.
///
/// `None` means "no restriction" (an admin's view). `Some(&[])` becomes `AND 0`, so an
/// account with no team gets an empty report instead of everybody else's traffic — the
/// difference between "unscoped" and "scoped to nothing" must never be a typo away.
fn team_filter(teams: Option<&[Uuid]>, first: usize) -> (String, Vec<SqlValue>) {
    match teams {
        None => (String::new(), Vec::new()),
        Some([]) => (" AND 0".to_string(), Vec::new()),
        Some(ids) => {
            let holes = (first..first + ids.len())
                .map(|n| format!("?{n}"))
                .collect::<Vec<_>>()
                .join(", ");
            (
                format!(" AND team_id IN ({holes})"),
                ids.iter()
                    .map(|id| SqlValue::Text(id.to_string()))
                    .collect(),
            )
        }
    }
}

fn uuid_col(s: String) -> rusqlite::Result<Uuid> {
    Uuid::parse_str(&s).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })
}

/// A nullable UUID column. `NULL` stays `None`; a malformed value is a storage failure,
/// never a silent `None` — for `connections.team_id`, `None` means "shared with every
/// global key", so a corrupt id must fail the read rather than widen visibility.
fn opt_uuid_col(raw: Option<String>) -> rusqlite::Result<Option<Uuid>> {
    raw.map(uuid_col).transpose()
}

/// Columns of a [`UsageRecord`], in the order [`row_to_usage`] reads them. Shared by
/// every reader so a new column cannot desynchronize one query from the rest.
const USAGE_COLUMNS: &str = "id, ts, request_id, provider, model, connection_id, endpoint, \
     input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, latency_ms, ttft_ms, \
     status, streamed, error, key_id, cost_micros, upstream_ms, trace_id";

/// Empty `request_id` is the v8 "unknown" default written before request ids existed.
fn row_to_usage(r: &Row<'_>) -> rusqlite::Result<UsageRecord> {
    let cid: Option<String> = r.get(5)?;
    let rid: String = r.get(2)?;
    let kid: Option<String> = r.get(16)?;
    Ok(UsageRecord {
        id: r.get(0)?,
        ts: r.get(1)?,
        request_id: Uuid::parse_str(&rid).ok(),
        trace_id: r.get(19)?,
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
        upstream_ms: r.get(18)?,
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
        team_id: opt_uuid_col(r.get(15)?)?,
        owner_user_id: opt_uuid_col(r.get(16)?)?,
    })
}

fn row_to_api_key(r: &Row<'_>) -> rusqlite::Result<ApiKey> {
    // `allowed_models` is a JSON array of refs; a malformed blob is a storage failure
    // rather than a silent "allow everything".
    let raw_models: Option<String> = r.get(8)?;
    let allowed_models = match raw_models {
        Some(text) => Some(serde_json::from_str(&text).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(8, rusqlite::types::Type::Text, Box::new(e))
        })?),
        None => None,
    };
    let raw_guardrails: Option<String> = r.get(12)?;
    // A malformed policy is a storage failure, never a silent "no policy": the key's
    // traffic would then run unfiltered while the operator believes otherwise.
    let guardrails = match raw_guardrails {
        Some(text) => Some(serde_json::from_str(&text).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(12, rusqlite::types::Type::Text, Box::new(e))
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
        team_id: opt_uuid_col(r.get(14)?)?,
    })
}

struct RawCombo {
    id: Uuid,
    name: String,
    models: String,
    strategy: String,
    router_model: Option<String>,
    cursor: i64,
    created_at: i64,
}

fn row_to_quota_ping(r: &Row<'_>) -> rusqlite::Result<QuotaPing> {
    Ok(QuotaPing {
        connection_id: uuid_col(r.get(0)?)?,
        mode: QuotaPingMode::from_stored(&r.get::<_, String>(1)?),
        anchor: r.get(2)?,
        timezone: r.get(3)?,
        window_hours: r.get(4)?,
        slots_per_day: r.get::<_, i64>(5)?.max(0) as u32,
        ping_model: r.get(6)?,
        last_ping_at: r.get(7)?,
        last_ping_reset_at: r.get(8)?,
        last_ping_slot: r.get(9)?,
        last_error: r.get(10)?,
        updated_at: r.get(11)?,
    })
}

/// `HH:MM` in 24-hour form, exactly two digits each.
fn validate_anchor(raw: &str) -> Result<String> {
    let anchor = raw.trim();
    let (hour, minute) = anchor
        .split_once(':')
        .ok_or_else(|| Error::BadRequest("anchor must be HH:MM".into()))?;
    let valid = hour.len() == 2
        && minute.len() == 2
        && hour.bytes().all(|b| b.is_ascii_digit())
        && minute.bytes().all(|b| b.is_ascii_digit())
        && hour.parse::<u8>().is_ok_and(|h| h < 24)
        && minute.parse::<u8>().is_ok_and(|m| m < 60);
    if !valid {
        return Err(Error::BadRequest("anchor must be HH:MM".into()));
    }
    Ok(format!("{hour}:{minute}"))
}

fn row_to_combo(r: &Row<'_>) -> rusqlite::Result<RawCombo> {
    Ok(RawCombo {
        id: uuid_col(r.get(0)?)?,
        name: r.get(1)?,
        models: r.get(2)?,
        strategy: r.get(3)?,
        router_model: r.get(4)?,
        cursor: r.get(5)?,
        created_at: r.get(6)?,
    })
}

fn decode_combo(raw: RawCombo) -> Result<Combo> {
    let models: Vec<String> = serde_json::from_str(&raw.models)
        .map_err(|e| Error::Store(format!("combo {} has invalid models JSON: {e}", raw.name)))?;
    Ok(Combo {
        id: raw.id,
        name: raw.name,
        models,
        strategy: ComboStrategy::from_stored(&raw.strategy),
        router_model: raw.router_model,
        cursor: raw.cursor,
        created_at: raw.created_at,
    })
}

/// Does `reference` name or resolve (through one alias hop) to a combo other than
/// `self_name`? Smart combos use this to reject a router model that would re-enter a
/// combo. Mirrors the planner's one-hop alias rule.
fn combo_reference(
    tx: &rusqlite::Transaction<'_>,
    reference: &str,
    self_name: &str,
) -> Result<bool> {
    let mut current = reference.trim().to_string();
    if let Some(target) = tx
        .query_row(
            "SELECT target FROM model_aliases WHERE alias = ?1",
            params![current],
            |r| r.get::<_, String>(0),
        )
        .optional()
        .map_err(migrations::db_err)?
    {
        current = target;
    }
    if current.strip_prefix("combo:").is_some() {
        return Ok(true);
    }
    if current == self_name {
        return Ok(true);
    }
    tx.query_row(
        "SELECT 1 FROM combos WHERE name = ?1",
        params![current],
        |r| r.get::<_, i64>(0),
    )
    .optional()
    .map(|hit| hit.is_some())
    .map_err(migrations::db_err)
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
    use super::{window_reset, window_start, BudgetWindow};

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
        assert_eq!(
            window_start(BudgetWindow::Total, ts(2025, 6, 15, 12, 0)),
            None
        );
    }

    #[test]
    fn daily_starts_at_utc_midnight() {
        // Sun 2025-06-15 12:34 UTC -> Sun 2025-06-15 00:00 UTC.
        let now = ts(2025, 6, 15, 12, 34);
        assert_eq!(
            window_start(BudgetWindow::Daily, now),
            Some(ts(2025, 6, 15, 0, 0))
        );
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

    /// The reset is the end of the window that starts at `window_start`: daily and
    /// weekly add a fixed span, monthly rolls to the first day of the next month
    /// (including a 31-day month and a year boundary).
    #[test]
    fn reset_is_the_end_of_the_window() {
        assert_eq!(
            window_reset(BudgetWindow::Total, ts(2025, 6, 15, 0, 0)),
            None
        );
        assert_eq!(
            window_reset(BudgetWindow::Daily, ts(2025, 6, 15, 0, 0)),
            Some(ts(2025, 6, 16, 0, 0))
        );
        assert_eq!(
            window_reset(BudgetWindow::Weekly, ts(2025, 6, 16, 0, 0)),
            Some(ts(2025, 6, 23, 0, 0))
        );
        // June has 30 days; December rolls into the next year.
        assert_eq!(
            window_reset(BudgetWindow::Monthly, ts(2025, 6, 1, 0, 0)),
            Some(ts(2025, 7, 1, 0, 0))
        );
        assert_eq!(
            window_reset(BudgetWindow::Monthly, ts(2025, 12, 1, 0, 0)),
            Some(ts(2026, 1, 1, 0, 0))
        );
        // A month's reset is exactly where the next month's window starts.
        let start = window_start(BudgetWindow::Monthly, ts(2025, 1, 31, 23, 0)).expect("start");
        assert_eq!(
            window_reset(BudgetWindow::Monthly, start),
            window_start(BudgetWindow::Monthly, ts(2025, 2, 1, 0, 0))
        );
    }

    #[test]
    fn weekly_handles_the_unix_epoch_weekday() {
        // 1970-01-01 was a Thursday, so the week of Mon 1969-12-29 is the epoch's week.
        assert_eq!(
            window_start(BudgetWindow::Weekly, 0),
            Some(ts(1969, 12, 29, 0, 0))
        );
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
        assert_eq!(
            window_start(BudgetWindow::Daily, now),
            Some(ts(2025, 12, 31, 0, 0))
        );
        assert_eq!(
            window_start(BudgetWindow::Weekly, now),
            Some(ts(2025, 12, 29, 0, 0))
        );
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
