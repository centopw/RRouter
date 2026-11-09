//! Schema migrations, guarded by `PRAGMA user_version` so opening an existing DB is a no-op.
//!
//! Every migration runs inside one transaction together with its `user_version` bump, so a
//! crash mid-migration leaves the database at the previous version.

use rusqlite::Connection;

use rrouter_core::{Error, Result};

/// Schema version this build expects. Bump when appending to [`MIGRATIONS`].
///
/// Public because an upgrade is a deployment-visible fact: the gateway reports it, and
/// the migration tests assert against it so a schema bump cannot leave a stale number in
/// the suite.
pub const SCHEMA_VERSION: i64 = 29;

/// Ordered migrations; index 0 takes the DB from version 0 to version 1.
const MIGRATIONS: &[&str] = &[
    V1, V2, V3, V4, V5, V6, V7, V8, V9, V10, V11, V12, V13, V14, V15, V16, V17, V18, V19, V20, V21,
    V22, V23, V24, V25, V26, V27, V28, V29,
];

/// V29: semantic response cache. One row is one stored response plus the prompt
/// embedding that justified it; a lookup scans the rows of one `(embedding_model,
/// provider, model, team)` scope and compares vectors in the gateway, so the table needs
/// no vector extension.
///
/// `prompt_hash` is the SHA-256 of the canonical prompt: two requests with the same text
/// collide exactly there and are stored once. `embedding` is raw little-endian `f32`s,
/// which is the wire format both sides use; it is opaque to SQLite and never transported
/// as text. `team_id` is nullable and means the instance, exactly like
/// `connections.team_id`; a team lookup never sees a shared row and vice versa.
///
/// `expires_at` bounds every row's lifetime and `id` orders eviction (oldest first) when
/// the configured entry cap is exceeded. No prompt or response text is ever logged; the
/// response itself is stored because that is the point of the cache.
const V29: &str = r#"
CREATE TABLE IF NOT EXISTS semantic_cache (
  id              INTEGER PRIMARY KEY AUTOINCREMENT,
  prompt_hash     TEXT NOT NULL,
  embedding_model TEXT NOT NULL,
  provider        TEXT NOT NULL,
  model           TEXT NOT NULL,
  team_id         TEXT NOT NULL DEFAULT '',
  embedding       BLOB NOT NULL,
  body            TEXT NOT NULL,
  created_at      INTEGER NOT NULL,
  expires_at      INTEGER NOT NULL,
  hits            INTEGER NOT NULL DEFAULT 0
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_semantic_cache_prompt
  ON semantic_cache(embedding_model, provider, model, team_id, prompt_hash);
CREATE INDEX IF NOT EXISTS idx_semantic_cache_scan
  ON semantic_cache(embedding_model, provider, model, expires_at);
"#;

/// V28: session device metadata. A session row can now say which address and
/// browser signed in, so the account page can show "Chrome on macOS" next to a
/// device instead of a bare timestamp. Both columns are nullable and no existing
/// row is rewritten: legacy sessions (and API-created ones) simply have no
/// metadata, which the projection renders as null, never as invented values.
///
/// `user_agent` is the raw header bounded to 512 characters by the writer; there
/// is no parser here and no `device_label` column — the label is derived at read
/// time from the raw string, so improving it never needs a migration. `ip` is
/// the resolved client address (`X-Forwarded-For`'s first hop when present),
/// bounded to 64 characters. Neither column stores a cookie, token or header
/// dump.
const V28: &str = r#"
ALTER TABLE user_sessions ADD COLUMN ip TEXT;
ALTER TABLE user_sessions ADD COLUMN user_agent TEXT;
"#;

/// V27: evaluation harness. A suite names a group of cases; a case is one input and the
/// output it should produce; a run is one model answering a whole suite at one moment; a
/// result is that model's answer to one case. `score`, `latency_ms` and `error` are per
/// result because a run mixes successes and failures, and `summary` is the run-level
/// aggregate the API decides (pass rate, mean score, cost) — it is not derived here.
///
/// `input`/`expected`/`output` are opaque text: prompt, JSON and plain text all fit, and
/// the harness that produced them owns the format. No foreign keys, for the reason V15
/// gives: a delete removes the dependent rows in one transaction and `ON DELETE CASCADE`
/// would buy nothing over that. The indexes are the lookups every screen needs —
/// a suite's cases, a suite's runs, a run's results, a case's results across runs.
const V27: &str = r#"
CREATE TABLE IF NOT EXISTS eval_suites (
  id          TEXT PRIMARY KEY,
  name        TEXT NOT NULL UNIQUE,
  description TEXT,
  created_at  INTEGER NOT NULL,
  updated_at  INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS eval_cases (
  id         TEXT PRIMARY KEY,
  suite_id   TEXT NOT NULL,
  name       TEXT,
  input      TEXT NOT NULL,
  expected   TEXT,
  created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS eval_runs (
  id           TEXT PRIMARY KEY,
  suite_id     TEXT NOT NULL,
  model        TEXT NOT NULL,
  status       TEXT NOT NULL,
  created_at   INTEGER NOT NULL,
  completed_at INTEGER,
  summary      TEXT
);
CREATE TABLE IF NOT EXISTS eval_results (
  id         TEXT PRIMARY KEY,
  run_id     TEXT NOT NULL,
  case_id    TEXT NOT NULL,
  output     TEXT,
  score      REAL,
  latency_ms INTEGER,
  error      TEXT,
  created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_eval_cases_suite ON eval_cases(suite_id);
CREATE INDEX IF NOT EXISTS idx_eval_runs_suite ON eval_runs(suite_id);
CREATE INDEX IF NOT EXISTS idx_eval_results_run ON eval_results(run_id);
CREATE INDEX IF NOT EXISTS idx_eval_results_case ON eval_results(case_id);
"#;

/// V26: tenant-scoped provider ownership. Both columns are nullable and no existing
/// connection changes: a row written before this migration has `team_id IS NULL` and
/// `owner_user_id IS NULL`, which means "shared, instance-wide", exactly its current
/// meaning. Nothing is rewritten and the `connections` uniqueness does not change — there
/// is still one row per connection id, and a connection is still identified by that id
/// rather than by owner.
///
/// Routing eligibility is enforced by `rrouter-router`'s candidate planner, which filters a
/// key's candidates by `connections.team_id = key.team_id`: a team-scoped key is routed only
/// to that team's accounts, and the shared rows (`team_id IS NULL`) stay available **only**
/// to global/admin credentials. The index above is the lookup that filter uses, so the
/// migration and the planner are one decision recorded in two places.
///
/// `owner_user_id` is attribution for the dashboard only — an owner is not an access
/// grant, so it carries no index of its own.
const V26: &str = r#"
ALTER TABLE connections ADD COLUMN team_id TEXT;
ALTER TABLE connections ADD COLUMN owner_user_id TEXT;
CREATE INDEX IF NOT EXISTS idx_connections_team ON connections(team_id);
"#;

/// V25: telemetry and webhook state. Three pieces, all additive:
///
/// * `usage.trace_id` joins one request's log row to the trace it was sent under, so a
///   debugging screen can group a request and its fallbacks without guessing from
///   timestamps. Nullable: rows written before tracing existed, and requests that carried
///   no trace, have no id, and nothing invents one after the fact. The index is the lookup
///   that screen does (`WHERE trace_id = ?`); it is cheap because the column is mostly
///   NULL and SQLite indexes only real values.
/// * `events_outbox` is a delivery queue, not an audit trail: one row per event that must
///   reach a receiver, keyed by the event id so an enqueue retry cannot duplicate a
///   delivery. `payload` is the already-redacted body — no credentials or `Authorization`
///   values belong in it — and it is stored **unsigned**: the signature is computed per
///   delivery attempt (the timestamp is part of the header), so a row that waits or retries
///   is signed when it is actually sent. `status` is the queue state (the API owns the
///   vocabulary; `pending` is the only default), `attempts` counts delivery tries,
///   `next_attempt_at` is when the dispatcher may try next and is what the `(status,
///   next_attempt_at)` index scans, `created_at` drives the retention sweep, and
///   `delivered_at`/`last_error` record the terminal outcome. Nothing retries forever: a
///   terminal state is the dispatcher's decision, recorded here.
/// * `webhook_endpoints` is the receiver registry. `event_types` is the subscription
///   document the API parses (it is stored as text, not a child table, because it is
///   read as a unit and never joined); `secret_env` names where the signing secret comes
///   from, so the secret itself is never in the database; `enabled` is the operator's
///   switch, defaulting to on so a row is live the moment it is created; `updated_at` is
///   NULL until the first edit. Uniqueness and URL/format validation are the API's rules
///   and deliberately not CHECKs here, so a validation change is not a migration.
const V25: &str = r#"
ALTER TABLE usage ADD COLUMN trace_id TEXT;
CREATE INDEX IF NOT EXISTS idx_usage_trace ON usage(trace_id);
CREATE TABLE IF NOT EXISTS events_outbox (
  id              TEXT PRIMARY KEY,
  type            TEXT NOT NULL,
  payload         TEXT NOT NULL,
  created_at      INTEGER NOT NULL,
  next_attempt_at INTEGER NOT NULL,
  attempts        INTEGER NOT NULL DEFAULT 0,
  status          TEXT NOT NULL DEFAULT 'pending',
  last_error      TEXT,
  delivered_at    INTEGER
);
CREATE INDEX IF NOT EXISTS idx_events_outbox_due ON events_outbox(status, next_attempt_at);
CREATE INDEX IF NOT EXISTS idx_events_outbox_created ON events_outbox(created_at);
CREATE TABLE IF NOT EXISTS webhook_endpoints (
  id          TEXT PRIMARY KEY,
  url         TEXT NOT NULL,
  event_types TEXT NOT NULL,
  secret_env  TEXT,
  enabled     INTEGER NOT NULL DEFAULT 1,
  created_at  INTEGER NOT NULL,
  updated_at  INTEGER
);
"#;

/// V24: retired compatibility migration. It created the prepaid-credit tables
/// (`credit_accounts`, `credit_ledger`) and is kept only because a database created by an
/// earlier build is already at `user_version` 24: removing the migration would leave those
/// files with tables this build cannot explain, and rewriting it would make two schemas
/// share one version. Nothing in the product reads or writes these tables any more — the
/// credit feature was removed — and they MUST NOT be extended: no column, index, or table
/// may be added here, and any new persistence goes in a fresh migration above v24.
///
/// Original shape, for the record: `credit_accounts` held one balance per subject (a
/// gateway key or a team), and `credit_ledger` was the append-only history that explained
/// it — every entry records the delta and the balance it produced, so a statement was a
/// read and a reconciliation a sum.
///
/// `subject_kind` was deliberately *not* a SQL `CHECK`: 'key' and 'team' were the subjects
/// that release validated at the API layer, and a CHECK would turn every future subject
/// kind into a migration. `balance_micros` could go negative only up to `overdraft_micros`,
/// a rule the (now removed) settlement path enforced when it debited.
///
/// `reserved_micros` was money already promised to in-flight requests: a reservation was
/// taken when a request was admitted against credits and released when it settled, so the
/// spendable amount was `balance_micros - reserved_micros`. It was a plain counter on the
/// account — the ledger recorded only what actually moved.
///
/// `UNIQUE(kind, request_id)` was the idempotency key: one debit or refund per request, so
/// a retried settlement could not double-charge. SQLite treats NULLs as distinct, so the
/// many adjustments that no request caused (top-ups, manual credits) coexisted.
const V24: &str = r#"
CREATE TABLE IF NOT EXISTS credit_accounts (
  subject_kind        TEXT NOT NULL,
  subject_id          TEXT NOT NULL,
  balance_micros      INTEGER NOT NULL DEFAULT 0,
  reserved_micros     INTEGER NOT NULL DEFAULT 0,
  overdraft_micros    INTEGER NOT NULL DEFAULT 0,
  low_balance_micros  INTEGER NOT NULL DEFAULT 0,
  updated_at          INTEGER NOT NULL,
  PRIMARY KEY (subject_kind, subject_id)
);
CREATE TABLE IF NOT EXISTS credit_ledger (
  id                    TEXT PRIMARY KEY,
  ts                    INTEGER NOT NULL,
  subject_kind          TEXT NOT NULL,
  subject_id            TEXT NOT NULL,
  kind                  TEXT NOT NULL,
  delta_micros          INTEGER NOT NULL,
  balance_after_micros  INTEGER NOT NULL,
  request_id            TEXT,
  note                  TEXT,
  actor                 TEXT,
  UNIQUE(kind, request_id)
);
CREATE INDEX IF NOT EXISTS idx_credit_ledger_subject_ts
  ON credit_ledger(subject_kind, subject_id, ts DESC);
CREATE INDEX IF NOT EXISTS idx_credit_ledger_request
  ON credit_ledger(request_id);
"#;

/// V23: teams. Tenancy is additive and nullable: a key or a usage row without a team is
/// exactly what it was before this migration, so nothing changes until an operator
/// creates a team and assigns one. `team_members.role` mirrors the dashboard vocabulary
/// (`owner`/`admin`/`member`/`viewer`) but is scoped to one team. Team budgets reuse the
/// key budgets' shape — `0` means unlimited and `budget_window` uses [`BudgetWindow`]'s
/// spellings — so one set of window arithmetic serves both.
///
/// The `usage` index is the one every team report needs: `(team_id, ts)` answers "this
/// team's spend in a window" without scanning the request log, and it is the same shape
/// as the v7 key index.
const V23: &str = r#"
CREATE TABLE IF NOT EXISTS teams (
  id            TEXT PRIMARY KEY,
  name          TEXT NOT NULL UNIQUE,
  created_at    INTEGER NOT NULL,
  budget_tokens INTEGER NOT NULL DEFAULT 0,
  budget_micros INTEGER NOT NULL DEFAULT 0,
  budget_window TEXT NOT NULL DEFAULT 'total'
);
CREATE TABLE IF NOT EXISTS team_members (
  team_id    TEXT NOT NULL,
  user_id    TEXT NOT NULL,
  role       TEXT NOT NULL DEFAULT 'member',
  created_at INTEGER NOT NULL,
  PRIMARY KEY (team_id, user_id)
);
CREATE INDEX IF NOT EXISTS idx_team_members_user ON team_members(user_id);
ALTER TABLE api_keys ADD COLUMN team_id TEXT;
ALTER TABLE usage ADD COLUMN team_id TEXT;
CREATE INDEX IF NOT EXISTS idx_usage_team_ts ON usage(team_id, ts);
"#;

/// V22: the index behind per-account quota reads. `exhausted_accounts` aggregates
/// `usage` by `connection_id` on the request path; without this index that is a full
/// table scan of the whole retention window. One index, no column or data change.
const V22: &str = r#"
CREATE INDEX IF NOT EXISTS idx_usage_connection_ts ON usage(connection_id, ts);
"#;

/// V21: remember-me. A session created without the checkbox is deleted on the next
/// process start, so a redeploy cannot keep a browser-session login alive.
const V21: &str = r#"
ALTER TABLE user_sessions ADD COLUMN remember INTEGER NOT NULL DEFAULT 1;
"#;

/// V20: request phase timing. Time to upstream response headers is transport information
/// the gateway can measure but the model cannot report; storing it next to `ttft_ms`
/// separates connect/queueing from model thinking and generation in the request drawer.
const V20: &str = r#"
ALTER TABLE usage ADD COLUMN upstream_ms INTEGER NOT NULL DEFAULT 0;
"#;

/// V19: security audit trail. One row per interesting identity/administration action —
/// sign-in, failure, setup, invitation, role/disable change, password reset, session
/// revoke. `detail` is free text written by the handler; no tokens, hashes or passwords
/// are ever stored. Retention follows the usage sweep.
const V19: &str = r#"
CREATE TABLE IF NOT EXISTS audit_events (
  id     INTEGER PRIMARY KEY AUTOINCREMENT,
  ts     INTEGER NOT NULL,
  kind   TEXT NOT NULL,
  actor  TEXT,
  ip     TEXT,
  detail TEXT
);
CREATE INDEX IF NOT EXISTS idx_audit_ts ON audit_events(ts DESC);
"#;

/// V18: dashboard users. Identity is a dashboard concept only — `/v1` clients still
/// authenticate with gateway keys, and the admin token and admin keys remain the
/// automation credential. `email` is stored lowercased so the unique index is the
/// case-insensitive rule; sessions persist as SHA-256 hashes of the cookie token, so a
/// database copy cannot be replayed as a live session. Recovery codes and invitation
/// tokens are hashed the same way.
const V18: &str = r#"
CREATE TABLE IF NOT EXISTS users (
  id               TEXT PRIMARY KEY,
  name             TEXT NOT NULL,
  email            TEXT NOT NULL UNIQUE,
  password_hash    TEXT NOT NULL,
  role             TEXT NOT NULL DEFAULT 'member',
  is_owner         INTEGER NOT NULL DEFAULT 0,
  disabled         INTEGER NOT NULL DEFAULT 0,
  created_at       INTEGER NOT NULL,
  last_sign_in_at  INTEGER
);
CREATE TABLE IF NOT EXISTS user_sessions (
  token_hash TEXT PRIMARY KEY,
  user_id    TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  expires_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_user_sessions_user ON user_sessions(user_id);
CREATE TABLE IF NOT EXISTS user_recovery_codes (
  id        INTEGER PRIMARY KEY AUTOINCREMENT,
  user_id   TEXT NOT NULL,
  code_hash TEXT NOT NULL UNIQUE,
  used_at   INTEGER
);
CREATE INDEX IF NOT EXISTS idx_user_recovery_user ON user_recovery_codes(user_id);
CREATE TABLE IF NOT EXISTS invites (
  id         TEXT PRIMARY KEY,
  email      TEXT NOT NULL,
  role       TEXT NOT NULL,
  token_hash TEXT NOT NULL UNIQUE,
  created_by TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  expires_at INTEGER NOT NULL,
  used_at    INTEGER
);
"#;

/// V17: per-account quotas. Each upstream connection can carry one row of limits — requests,
/// tokens, cost — measured over a `BudgetWindow` (`daily`/`weekly`/`monthly`, UTC calendar
/// boundaries like the key budgets, or `total` for all time). `0` means unlimited; usage is
/// aggregated from the `usage` table at read time, so there is nothing to reset or repair.
const V17: &str = r#"
CREATE TABLE IF NOT EXISTS account_quotas (
  connection_id TEXT PRIMARY KEY,
  window TEXT NOT NULL DEFAULT 'daily',
  max_requests INTEGER NOT NULL DEFAULT 0,
  max_tokens INTEGER NOT NULL DEFAULT 0,
  max_cost_micros INTEGER NOT NULL DEFAULT 0,
  updated_at INTEGER NOT NULL
);
"#;

/// V16: key scope. `inference` (spend through `/v1/*`) or `admin` (administer the
/// gateway). The default keeps every existing key exactly what it was: an inference
/// key, which the admin gate refuses.
const V16: &str = r#"
ALTER TABLE api_keys ADD COLUMN scope TEXT NOT NULL DEFAULT 'inference';
"#;

/// V15: reusable prompt templates. `prompt_templates` holds the live name and latest
/// body; `prompt_template_versions` is the append-only history, keyed by
/// `(template_id, version)` so re-reading one revision is an index hit. The history has
/// no foreign key: a delete removes both tables in one transaction (see
/// [`Store::delete_template`]), and `ON DELETE CASCADE` would require a per-connection
/// pragma this store already sets, for no gain over the explicit delete.
const V15: &str = r#"
CREATE TABLE IF NOT EXISTS prompt_templates (
  id TEXT PRIMARY KEY,
  name TEXT UNIQUE NOT NULL,
  body TEXT NOT NULL,
  version INTEGER NOT NULL DEFAULT 1,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS prompt_template_versions (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  template_id TEXT NOT NULL,
  version INTEGER NOT NULL,
  body TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  UNIQUE(template_id, version)
);
"#;

/// V14: per-key guardrails. One JSON blob rather than a column per field: the policy is
/// a small document an operator edits as a unit, and every field is optional, so NULL
/// (no policy) and `{}` (also no policy) mean the same thing — the store normalises the
/// empty document to NULL so there is only one spelling.
const V14: &str = r#"
ALTER TABLE api_keys ADD COLUMN guardrails TEXT;
"#;

/// V13: retention. The sweep deletes `attempts` by `ts`, which the v8 index
/// (`request_id, attempt`) cannot serve, so without this every sweep scans the table.
/// One index, no column or data change.
const V13: &str = r#"
CREATE INDEX IF NOT EXISTS idx_attempts_ts ON attempts(ts);
"#;

/// V11: the opt-in exact-match response cache. `key` is the SHA-256 of the canonical
/// request (see `Store::cache_key`), `body` is the client-facing JSON that was sent,
/// and `expires_at` bounds how long a byte-identical request may reuse it. `hits` is
/// bookkeeping only; eviction is by `created_at`, oldest first, when the table grows
/// past `RROUTER_CACHE_MAX_ENTRIES`. Nothing here holds prompt text beyond the
/// response the client already received.
const V11: &str = r#"
CREATE TABLE IF NOT EXISTS response_cache (
  key TEXT PRIMARY KEY, body TEXT NOT NULL, created_at INTEGER NOT NULL,
  expires_at INTEGER NOT NULL, hits INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_cache_expiry ON response_cache(expires_at);
"#;

/// V12: budget reset windows. `api_keys.budget_window` says which span of time the
/// token and USD budgets are measured over: `total` (lifetime, the pre-v12 behaviour),
/// `daily`, `weekly` or `monthly`. The default keeps every existing key lifetime-capped.
const V12: &str = r#"
ALTER TABLE api_keys ADD COLUMN budget_window TEXT NOT NULL DEFAULT 'total';
"#;

/// V10: money. `usage.cost_micros` is what one request cost in millionths of a USD,
/// priced from the registry's per-million-token rates (0 = the price was unknown, never
/// a guess); `api_keys.budget_micros` is a lifetime USD budget in the same unit
/// (0 = unlimited), so an operator can budget in dollars next to tokens. Existing rows
/// default to "unknown price" and "unlimited", which is exactly their pre-v10 behaviour.
const V10: &str = r#"
ALTER TABLE usage ADD COLUMN cost_micros INTEGER NOT NULL DEFAULT 0;
ALTER TABLE api_keys ADD COLUMN budget_micros INTEGER NOT NULL DEFAULT 0;
"#;

/// V9: routing inputs. `connections.weight` is the traffic share used by the weighted
/// strategy (clamped to 1..=1000 on read); `connection_stats` keeps one EWMA latency per
/// connection, which survives a restart and needs no in-process state; `key_models`
/// pins a client model name to a provider ref for one virtual key, so a client keeps a
/// stable name while the operator repoints it.
const V9: &str = r#"
ALTER TABLE connections ADD COLUMN weight INTEGER NOT NULL DEFAULT 100;
CREATE TABLE IF NOT EXISTS connection_stats (
  connection_id   TEXT PRIMARY KEY,
  ewma_latency_ms REAL NOT NULL,
  updated_at      INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS key_models (
  key_id   TEXT NOT NULL,
  from_ref TEXT NOT NULL,
  to_ref   TEXT NOT NULL,
  PRIMARY KEY (key_id, from_ref)
);
"#;

/// V8: per-attempt audit trail. A fallback is only operable if every candidate that
/// was tried is visible, so attempts get their own table keyed by the request id the
/// client saw in `x-request-id`; the usage row carries the same id. `status` is 0 when
/// nobody answered (transport failure, breaker), and the empty string is the "unknown"
/// request id for rows written before this migration.
const V8: &str = r#"
ALTER TABLE usage ADD COLUMN request_id TEXT NOT NULL DEFAULT '';
CREATE INDEX IF NOT EXISTS idx_usage_request ON usage(request_id);
CREATE TABLE IF NOT EXISTS attempts (
  id            INTEGER PRIMARY KEY AUTOINCREMENT,
  request_id    TEXT NOT NULL,
  ts            INTEGER NOT NULL,
  attempt       INTEGER NOT NULL,
  provider      TEXT NOT NULL,
  model         TEXT NOT NULL,
  connection_id TEXT,
  status        INTEGER NOT NULL DEFAULT 0,
  error         TEXT,
  latency_ms    INTEGER NOT NULL DEFAULT 0,
  key_id        TEXT
);
CREATE INDEX IF NOT EXISTS idx_attempts_request ON attempts(request_id, attempt);
"#;

/// V7: virtual-key governance. A key carries the limits an operator sells — requests
/// and tokens per minute, a lifetime token budget, a model allowlist, and an expiry —
/// and every usage row is attributed to the key that spent it. Zero/NULL means
/// "unlimited" throughout, so existing rows keep today's behaviour after the migration.
const V7: &str = r#"
ALTER TABLE api_keys ADD COLUMN rpm INTEGER NOT NULL DEFAULT 0;
ALTER TABLE api_keys ADD COLUMN tpm INTEGER NOT NULL DEFAULT 0;
ALTER TABLE api_keys ADD COLUMN budget_tokens INTEGER NOT NULL DEFAULT 0;
ALTER TABLE api_keys ADD COLUMN allowed_models TEXT;
ALTER TABLE api_keys ADD COLUMN expires_at INTEGER;
ALTER TABLE usage ADD COLUMN key_id TEXT;
CREATE INDEX IF NOT EXISTS idx_usage_key_ts ON usage(key_id, ts);
"#;

/// The client endpoint that produced a usage row (`/v1/messages`, …), so the log can
/// answer "which protocol was this?" without inferring it from the model name.
const V5: &str = r#"
ALTER TABLE usage ADD COLUMN endpoint TEXT;
"#;

/// Time from request start to the first model output, so a slow request can be split
/// into "upstream thought for 2 s" and "streamed for 500 ms". Zero means unmeasured
/// (non-streaming requests and failures).
const V6: &str = r#"
ALTER TABLE usage ADD COLUMN ttft_ms INTEGER NOT NULL DEFAULT 0;
"#;

const V1: &str = r#"
CREATE TABLE IF NOT EXISTS connections (
  id              TEXT PRIMARY KEY,
  provider        TEXT NOT NULL,
  auth_type       TEXT NOT NULL,
  name            TEXT,
  email           TEXT,
  priority        INTEGER NOT NULL DEFAULT 0,
  is_active       INTEGER NOT NULL DEFAULT 1,
  data            BLOB NOT NULL,
  backoff_level   INTEGER NOT NULL DEFAULT 0,
  last_error      TEXT,
  last_error_code INTEGER,
  last_error_at   INTEGER,
  created_at      INTEGER NOT NULL,
  updated_at      INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_conn_provider ON connections(provider, is_active, priority);

CREATE TABLE IF NOT EXISTS api_keys (
  id         TEXT PRIMARY KEY,
  key_hash   TEXT NOT NULL UNIQUE,
  name       TEXT,
  is_active  INTEGER NOT NULL DEFAULT 1,
  created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS model_aliases (
  alias  TEXT PRIMARY KEY,
  target TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS disabled_models (
  model TEXT PRIMARY KEY
);

CREATE TABLE IF NOT EXISTS combos (
  id         TEXT PRIMARY KEY,
  name       TEXT UNIQUE NOT NULL,
  models     TEXT NOT NULL,
  created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS usage (
  id            INTEGER PRIMARY KEY AUTOINCREMENT,
  ts            INTEGER NOT NULL,
  provider      TEXT NOT NULL,
  model         TEXT NOT NULL,
  connection_id TEXT,
  input_tokens  INTEGER NOT NULL DEFAULT 0,
  output_tokens INTEGER NOT NULL DEFAULT 0,
  latency_ms    INTEGER NOT NULL DEFAULT 0,
  status        INTEGER NOT NULL,
  streamed      INTEGER NOT NULL DEFAULT 0,
  error         TEXT
);
CREATE INDEX IF NOT EXISTS idx_usage_ts ON usage(ts DESC);
"#;

/// V2: the model registry. One row per `(provider, model)`; `source` says where it
/// came from and therefore how much authority it carries (see `Store::upsert_model`).
const V2: &str = r#"
CREATE TABLE IF NOT EXISTS models (
  provider   TEXT NOT NULL,
  model      TEXT NOT NULL,
  display    TEXT,
  context    INTEGER,
  max_output INTEGER,
  caps       TEXT NOT NULL DEFAULT '{}',
  source     TEXT NOT NULL,
  updated_at INTEGER NOT NULL,
  PRIMARY KEY (provider, model)
);
"#;

/// V3: pricing. models.dev publishes per-million-token cost, which is metadata about a
/// model rather than a fact the provider lists, so it lives beside the caps blob.
const V3: &str = r#"
ALTER TABLE models ADD COLUMN cost TEXT;
"#;

/// V4: prompt-cache accounting. Cache reads and writes are priced differently from
/// ordinary input, so they are stored separately to make a hit ratio measurable.
const V4: &str = r#"
ALTER TABLE usage ADD COLUMN cache_read_tokens INTEGER NOT NULL DEFAULT 0;
ALTER TABLE usage ADD COLUMN cache_write_tokens INTEGER NOT NULL DEFAULT 0;
"#;

/// Bring `conn` up to [`SCHEMA_VERSION`]. Idempotent.
pub(crate) fn migrate(conn: &mut Connection) -> Result<()> {
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(db_err)?;
    if version > SCHEMA_VERSION {
        return Err(Error::Store(format!(
            "database schema version {version} is newer than this build supports \
             ({SCHEMA_VERSION}); upgrade rrouter"
        )));
    }
    for (idx, sql) in MIGRATIONS.iter().enumerate() {
        let target = idx as i64 + 1;
        if version >= target {
            continue;
        }
        let tx = conn.transaction().map_err(db_err)?;
        tx.execute_batch(sql).map_err(db_err)?;
        // `PRAGMA user_version` does not accept a bound parameter.
        tx.execute_batch(&format!("PRAGMA user_version = {target}"))
            .map_err(db_err)?;
        tx.commit().map_err(db_err)?;
    }
    Ok(())
}

/// Map a `rusqlite` failure onto the crate error. Unique/PK conflicts are client-visible
/// `BadRequest`, everything else is a storage failure.
pub(crate) fn db_err(e: rusqlite::Error) -> Error {
    match e {
        rusqlite::Error::SqliteFailure(err, _)
            if err.code == rusqlite::ErrorCode::ConstraintViolation =>
        {
            Error::BadRequest(format!("conflicting record: {e}"))
        }
        other => Error::Store(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrate_is_idempotent_and_versioned() {
        let mut c = Connection::open_in_memory().expect("open");
        migrate(&mut c).expect("first");
        let v: i64 = c
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .expect("version");
        assert_eq!(v, SCHEMA_VERSION);
        // Second run must not fail or duplicate anything.
        migrate(&mut c).expect("second");
        let tables: i64 = c
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name IN \
                 ('connections','api_keys','model_aliases','disabled_models','combos','usage',\
                  'attempts','connection_stats','key_models','response_cache',\
                  'prompt_templates','prompt_template_versions','events_outbox',\
                  'webhook_endpoints','eval_suites','eval_cases','eval_runs','eval_results')",
                [],
                |r| r.get(0),
            )
            .expect("count");
        assert_eq!(tables, 18);
    }

    #[test]
    fn newer_schema_is_refused() {
        let mut c = Connection::open_in_memory().expect("open");
        c.execute_batch("PRAGMA user_version = 99").expect("set");
        assert!(migrate(&mut c).is_err());
    }

    /// Apply exactly the first `version` migrations, so an upgrade can be tested from a
    /// real older schema instead of only from a fresh install.
    fn at_version(c: &mut Connection, version: i64) {
        for (index, sql) in MIGRATIONS.iter().enumerate().take(version as usize) {
            c.execute_batch(sql).expect("apply migration");
            c.execute_batch(&format!("PRAGMA user_version = {}", index + 1))
                .expect("bump version");
        }
    }

    fn user_version(c: &Connection) -> i64 {
        c.query_row("PRAGMA user_version", [], |r| r.get(0))
            .expect("version")
    }

    fn has_table(c: &Connection, table: &str) -> bool {
        c.query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [table],
            |r| r.get::<_, i64>(0),
        )
        .expect("table lookup")
            == 1
    }

    fn has_column(c: &Connection, table: &str, column: &str) -> bool {
        let mut stmt = c
            .prepare(&format!("PRAGMA table_info({table})"))
            .expect("prepare");
        let names: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(1))
            .expect("columns")
            .flatten()
            .collect();
        names.iter().any(|name| name == column)
    }

    fn has_index(c: &Connection, table: &str, index: &str) -> bool {
        let mut stmt = c
            .prepare(&format!("PRAGMA index_list({table})"))
            .expect("prepare");
        let names: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(1))
            .expect("indexes")
            .flatten()
            .collect();
        names.iter().any(|name| name == index)
    }

    #[test]
    fn upgrades_from_v21_landing_v22_and_v23() {
        let mut c = Connection::open_in_memory().expect("open");
        at_version(&mut c, 21);
        assert_eq!(user_version(&c), 21);

        migrate(&mut c).expect("upgrade");
        assert_eq!(user_version(&c), SCHEMA_VERSION);

        // v22: the per-account quota aggregate becomes an index scan.
        assert!(has_index(&c, "usage", "idx_usage_connection_ts"));

        // v23: tenancy tables, nullable ownership columns, team budgets.
        let tables: i64 = c
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name IN \
                 ('teams','team_members')",
                [],
                |r| r.get(0),
            )
            .expect("count");
        assert_eq!(tables, 2);
        for column in ["team_id", "user_id", "role", "created_at"] {
            assert!(
                has_column(&c, "team_members", column),
                "team_members.{column}"
            );
        }
        for column in [
            "id",
            "name",
            "created_at",
            "budget_tokens",
            "budget_micros",
            "budget_window",
        ] {
            assert!(has_column(&c, "teams", column), "teams.{column}");
        }
        for (table, column) in [("api_keys", "team_id"), ("usage", "team_id")] {
            assert!(has_column(&c, table, column), "{table}.{column}");
        }
        assert!(has_index(&c, "usage", "idx_usage_team_ts"));
    }

    #[test]
    fn upgrades_from_v23_landing_v24_credits() {
        let mut c = Connection::open_in_memory().expect("open");
        at_version(&mut c, 23);
        assert_eq!(user_version(&c), 23);

        migrate(&mut c).expect("upgrade");
        assert_eq!(user_version(&c), SCHEMA_VERSION);

        let tables: i64 = c
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name IN \
                 ('credit_accounts','credit_ledger')",
                [],
                |r| r.get(0),
            )
            .expect("count");
        assert_eq!(tables, 2);
        for column in [
            "subject_kind",
            "subject_id",
            "balance_micros",
            "reserved_micros",
            "overdraft_micros",
            "low_balance_micros",
            "updated_at",
        ] {
            assert!(
                has_column(&c, "credit_accounts", column),
                "credit_accounts.{column}"
            );
        }
        for column in [
            "id",
            "ts",
            "subject_kind",
            "subject_id",
            "kind",
            "delta_micros",
            "balance_after_micros",
            "request_id",
            "note",
            "actor",
        ] {
            assert!(
                has_column(&c, "credit_ledger", column),
                "credit_ledger.{column}"
            );
        }
        assert!(has_index(
            &c,
            "credit_ledger",
            "idx_credit_ledger_subject_ts"
        ));
        assert!(has_index(&c, "credit_ledger", "idx_credit_ledger_request"));
    }

    /// A key that predates teams keeps `team_id = NULL` and still works; a credit account
    /// can be opened for it because `subject_kind` is validated in the API layer, not by a
    /// CHECK that would block a future subject.
    #[test]
    fn v24_accepts_a_legacy_key_as_a_credit_subject() {
        let mut c = Connection::open_in_memory().expect("open");
        migrate(&mut c).expect("migrate");

        c.execute(
            "INSERT INTO api_keys (id, key_hash, is_active, created_at) VALUES ('k', 'h', 1, 1)",
            [],
        )
        .expect("legacy key");
        c.execute(
            "INSERT INTO usage (ts, provider, model, status) VALUES (1, 'openai', 'gpt-4.1', 200)",
            [],
        )
        .expect("legacy usage row");

        let team: Option<String> = c
            .query_row("SELECT team_id FROM api_keys WHERE id = 'k'", [], |r| {
                r.get(0)
            })
            .expect("team_id");
        assert_eq!(team, None, "a legacy key has no team");

        c.execute(
            "INSERT INTO credit_accounts (subject_kind, subject_id, updated_at) \
             VALUES ('key', 'k', 1)",
            [],
        )
        .expect("credit account for a key");
        let (balance, reserved, overdraft, low): (i64, i64, i64, i64) = c
            .query_row(
                "SELECT balance_micros, reserved_micros, overdraft_micros, low_balance_micros \
                 FROM credit_accounts WHERE subject_kind = 'key' AND subject_id = 'k'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .expect("account defaults");
        assert_eq!(
            (balance, reserved, overdraft, low),
            (0, 0, 0, 0),
            "a new account holds nothing, reserves nothing and has no overdraft"
        );

        // One account per subject.
        assert!(c
            .execute(
                "INSERT INTO credit_accounts (subject_kind, subject_id, updated_at) \
                 VALUES ('key', 'k', 2)",
                [],
            )
            .is_err());

        // Adjustments with no request are independent (NULL request_id is not a conflict)…
        let insert = "INSERT INTO credit_ledger \
             (id, ts, subject_kind, subject_id, kind, delta_micros, balance_after_micros, request_id) \
             VALUES (?1, 1, 'key', 'k', ?2, 1000, 1000, NULL)";
        c.execute(insert, rusqlite::params!["topup-1", "topup"])
            .expect("first top-up");
        c.execute(insert, rusqlite::params!["topup-2", "topup"])
            .expect("second top-up");

        // …while a debit is idempotent per (kind, request_id).
        let debit = "INSERT INTO credit_ledger \
             (id, ts, subject_kind, subject_id, kind, delta_micros, balance_after_micros, request_id) \
             VALUES (?1, 1, 'key', 'k', 'debit', -10, 990, 'req-1')";
        c.execute(debit, rusqlite::params!["debit-1"])
            .expect("debit");
        assert!(
            c.execute(debit, rusqlite::params!["debit-2"]).is_err(),
            "the same request cannot be charged twice"
        );
    }

    /// The additive promise: a key and a usage row written by a caller that knows nothing
    /// about teams must still insert, and a team's budget must default to unlimited.
    #[test]
    fn team_columns_are_optional_and_budgets_default_to_unlimited() {
        let mut c = Connection::open_in_memory().expect("open");
        migrate(&mut c).expect("migrate");

        c.execute(
            "INSERT INTO api_keys (id, key_hash, is_active, created_at) VALUES ('k', 'h', 1, 1)",
            [],
        )
        .expect("api_keys without a team");
        c.execute(
            "INSERT INTO usage (ts, provider, model, status) VALUES (1, 'openai', 'gpt-4.1', 200)",
            [],
        )
        .expect("usage without a team");
        c.execute(
            "INSERT INTO teams (id, name, created_at) VALUES ('t', 'Eng', 1)",
            [],
        )
        .expect("team");
        c.execute(
            "INSERT INTO team_members (team_id, user_id, created_at) VALUES ('t', 'u', 1)",
            [],
        )
        .expect("member");

        let (tokens, micros, window): (i64, i64, String) = c
            .query_row(
                "SELECT budget_tokens, budget_micros, budget_window FROM teams WHERE id = 't'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .expect("budgets");
        assert_eq!((tokens, micros, window.as_str()), (0, 0, "total"));

        let role: String = c
            .query_row(
                "SELECT role FROM team_members WHERE team_id = 't' AND user_id = 'u'",
                [],
                |r| r.get(0),
            )
            .expect("role");
        assert_eq!(role, "member");

        // One membership per (team, user).
        assert!(c
            .execute(
                "INSERT INTO team_members (team_id, user_id, created_at) VALUES ('t', 'u', 2)",
                [],
            )
            .is_err());
    }

    /// v24 → v27 in one upgrade: every new table, column and index exists, and the retired
    /// credit tables are left exactly where v24 put them.
    #[test]
    fn upgrades_from_v24_landing_v25_v26_v27() {
        let mut c = Connection::open_in_memory().expect("open");
        at_version(&mut c, 24);
        assert_eq!(user_version(&c), 24);
        for table in ["credit_accounts", "credit_ledger"] {
            assert!(has_table(&c, table), "{table} exists before the upgrade");
        }

        migrate(&mut c).expect("upgrade");
        assert_eq!(user_version(&c), SCHEMA_VERSION);

        // v25: trace join column, delivery queue, receiver registry.
        assert!(has_column(&c, "usage", "trace_id"));
        assert!(has_index(&c, "usage", "idx_usage_trace"));
        for column in [
            "id",
            "type",
            "payload",
            "created_at",
            "next_attempt_at",
            "attempts",
            "status",
            "last_error",
            "delivered_at",
        ] {
            assert!(
                has_column(&c, "events_outbox", column),
                "events_outbox.{column}"
            );
        }
        assert!(has_index(&c, "events_outbox", "idx_events_outbox_due"));
        assert!(has_index(&c, "events_outbox", "idx_events_outbox_created"));
        for column in [
            "id",
            "url",
            "event_types",
            "secret_env",
            "enabled",
            "created_at",
            "updated_at",
        ] {
            assert!(
                has_column(&c, "webhook_endpoints", column),
                "webhook_endpoints.{column}"
            );
        }

        // v26: tenant ownership on connections, nullable, indexed by team.
        for column in ["team_id", "owner_user_id"] {
            assert!(
                has_column(&c, "connections", column),
                "connections.{column}"
            );
        }
        assert!(has_index(&c, "connections", "idx_connections_team"));

        // v27: the eval harness.
        for column in ["id", "name", "description", "created_at", "updated_at"] {
            assert!(
                has_column(&c, "eval_suites", column),
                "eval_suites.{column}"
            );
        }
        for column in ["id", "suite_id", "name", "input", "expected", "created_at"] {
            assert!(has_column(&c, "eval_cases", column), "eval_cases.{column}");
        }
        for column in [
            "id",
            "suite_id",
            "model",
            "status",
            "created_at",
            "completed_at",
            "summary",
        ] {
            assert!(has_column(&c, "eval_runs", column), "eval_runs.{column}");
        }
        for column in [
            "id",
            "run_id",
            "case_id",
            "output",
            "score",
            "latency_ms",
            "error",
            "created_at",
        ] {
            assert!(
                has_column(&c, "eval_results", column),
                "eval_results.{column}"
            );
        }
        for (table, index) in [
            ("eval_cases", "idx_eval_cases_suite"),
            ("eval_runs", "idx_eval_runs_suite"),
            ("eval_results", "idx_eval_results_run"),
            ("eval_results", "idx_eval_results_case"),
        ] {
            assert!(has_index(&c, table, index), "{table}.{index}");
        }

        // v24 stays retired: present, and still not extended by this upgrade.
        for table in ["credit_accounts", "credit_ledger"] {
            assert!(has_table(&c, table), "{table} survives the upgrade");
        }
    }

    /// v27 → v28: the two nullable device columns land and a session written by the
    /// previous schema keeps reading as "no metadata" instead of an invented value.
    #[test]
    fn upgrades_from_v27_landing_v28_session_metadata() {
        let mut c = Connection::open_in_memory().expect("open");
        at_version(&mut c, 27);
        assert_eq!(user_version(&c), 27);
        c.execute(
            "INSERT INTO user_sessions \
             (token_hash, user_id, created_at, expires_at, remember) \
             VALUES ('legacy', 'user-1', 1, 9999999999, 0)",
            [],
        )
        .expect("legacy session before the upgrade");

        migrate(&mut c).expect("upgrade");
        assert_eq!(user_version(&c), SCHEMA_VERSION);
        for column in ["ip", "user_agent"] {
            assert!(
                has_column(&c, "user_sessions", column),
                "user_sessions.{column}"
            );
        }
        let (ip, user_agent): (Option<String>, Option<String>) = c
            .query_row(
                "SELECT ip, user_agent FROM user_sessions WHERE token_hash = 'legacy'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("legacy row survives");
        assert_eq!((ip, user_agent), (None, None));
    }

    /// The additive promise, on a file that already has rows: a v24 connection, key and
    /// usage row survive, the v26 owner columns read as "shared", and every new table
    /// accepts an insert with only its required columns.
    #[test]
    fn v24_rows_survive_and_new_tables_accept_defaults() {
        let mut c = Connection::open_in_memory().expect("open");
        at_version(&mut c, 24);
        c.execute(
            "INSERT INTO connections (id, provider, auth_type, data, created_at, updated_at) \
             VALUES ('c', 'openai', 'api_key', x'00', 1, 1)",
            [],
        )
        .expect("legacy connection");
        c.execute(
            "INSERT INTO api_keys (id, key_hash, is_active, created_at) VALUES ('k', 'h', 1, 1)",
            [],
        )
        .expect("legacy key");
        c.execute(
            "INSERT INTO usage (ts, provider, model, status, connection_id) \
             VALUES (1, 'openai', 'gpt-4.1', 200, 'c')",
            [],
        )
        .expect("legacy usage row");

        migrate(&mut c).expect("upgrade");

        // Rows written before v25/v26 still load, with the new columns unset.
        let (team, owner): (Option<String>, Option<String>) = c
            .query_row(
                "SELECT team_id, owner_user_id FROM connections WHERE id = 'c'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("connection row");
        assert_eq!(
            (team.as_deref(), owner.as_deref()),
            (None, None),
            "a legacy connection stays shared and unowned"
        );
        let trace: Option<String> = c
            .query_row(
                "SELECT trace_id FROM usage WHERE connection_id = 'c'",
                [],
                |r| r.get(0),
            )
            .expect("usage row");
        assert_eq!(trace, None, "a legacy request has no trace id");
        let key_team: Option<String> = c
            .query_row("SELECT team_id FROM api_keys WHERE id = 'k'", [], |r| {
                r.get(0)
            })
            .expect("key row");
        assert_eq!(key_team, None);
        assert!(
            c.execute(
                "INSERT INTO connections (id, provider, auth_type, data, created_at, updated_at) \
                 VALUES ('c', 'openai', 'api_key', x'00', 1, 1)",
                [],
            )
            .is_err(),
            "v26 adds no second identity: a connection id is still unique"
        );

        // v25: an outbox row starts pending, untried and undelivered; its id is the
        // idempotency key.
        let insert_event =
            "INSERT INTO events_outbox (id, type, payload, created_at, next_attempt_at) \
             VALUES ('e', 'connection.degraded', '{}', 1, 1)";
        c.execute(insert_event, []).expect("outbox row");
        let (attempts, status, delivered, error): (i64, String, Option<i64>, Option<String>) = c
            .query_row(
                "SELECT attempts, status, delivered_at, last_error FROM events_outbox WHERE id = 'e'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .expect("outbox defaults");
        assert_eq!((attempts, status.as_str()), (0, "pending"));
        assert_eq!((delivered, error), (None, None));
        assert!(
            c.execute(insert_event, []).is_err(),
            "one row per event id: a retried enqueue cannot duplicate a delivery"
        );

        c.execute(
            "INSERT INTO webhook_endpoints (id, url, event_types, created_at) \
             VALUES ('w', 'https://example.test/hook', '[\"connection.degraded\"]', 1)",
            [],
        )
        .expect("endpoint");
        let (enabled, secret, updated): (i64, Option<String>, Option<i64>) = c
            .query_row(
                "SELECT enabled, secret_env, updated_at FROM webhook_endpoints WHERE id = 'w'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .expect("endpoint defaults");
        assert_eq!(enabled, 1, "a new endpoint is live");
        assert_eq!(
            (secret, updated),
            (None, None),
            "no secret is stored, never edited"
        );

        // v27: a suite/case/run/result chain, each with its nullable fields unset.
        c.execute(
            "INSERT INTO eval_suites (id, name, created_at, updated_at) VALUES ('s', 'smoke', 1, 1)",
            [],
        )
        .expect("suite");
        assert!(
            c.execute(
                "INSERT INTO eval_suites (id, name, created_at, updated_at) \
                 VALUES ('s2', 'smoke', 1, 1)",
                [],
            )
            .is_err(),
            "suite names are unique"
        );
        c.execute(
            "INSERT INTO eval_cases (id, suite_id, input, created_at) VALUES ('c1', 's', 'hi', 1)",
            [],
        )
        .expect("case");
        c.execute(
            "INSERT INTO eval_runs (id, suite_id, model, status, created_at) \
             VALUES ('r', 's', 'gpt-4.1', 'running', 1)",
            [],
        )
        .expect("run");
        c.execute(
            "INSERT INTO eval_results (id, run_id, case_id, created_at) VALUES ('res', 'r', 'c1', 1)",
            [],
        )
        .expect("result");
        let (output, score, latency, error): (
            Option<String>,
            Option<f64>,
            Option<i64>,
            Option<String>,
        ) = c
            .query_row(
                "SELECT output, score, latency_ms, error FROM eval_results WHERE id = 'res'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .expect("result defaults");
        assert_eq!(
            (output, score, latency, error),
            (None, None, None, None),
            "a queued result has no answer yet"
        );
    }
}
