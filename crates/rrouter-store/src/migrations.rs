//! Schema migrations, guarded by `PRAGMA user_version` so opening an existing DB is a no-op.
//!
//! Every migration runs inside one transaction together with its `user_version` bump, so a
//! crash mid-migration leaves the database at the previous version.

use rusqlite::Connection;

use rrouter_core::{Error, Result};

/// Schema version this build expects. Bump when appending to [`MIGRATIONS`].
pub(crate) const SCHEMA_VERSION: i64 = 15;

/// Ordered migrations; index 0 takes the DB from version 0 to version 1.
const MIGRATIONS: &[&str] = &[
    V1, V2, V3, V4, V5, V6, V7, V8, V9, V10, V11, V12, V13, V14, V15,
];

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
                  'prompt_templates','prompt_template_versions')",
                [],
                |r| r.get(0),
            )
            .expect("count");
        assert_eq!(tables, 12);
    }

    #[test]
    fn newer_schema_is_refused() {
        let mut c = Connection::open_in_memory().expect("open");
        c.execute_batch("PRAGMA user_version = 99").expect("set");
        assert!(migrate(&mut c).is_err());
    }
}
