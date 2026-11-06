//! Schema migrations, guarded by `PRAGMA user_version` so opening an existing DB is a no-op.
//!
//! Every migration runs inside one transaction together with its `user_version` bump, so a
//! crash mid-migration leaves the database at the previous version.

use rusqlite::Connection;

use rrouter_core::{Error, Result};

/// Schema version this build expects. Bump when appending to [`MIGRATIONS`].
pub(crate) const SCHEMA_VERSION: i64 = 3;

/// Ordered migrations; index 0 takes the DB from version 0 to version 1.
const MIGRATIONS: &[&str] = &[V1, V2, V3];

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
                 ('connections','api_keys','model_aliases','disabled_models','combos','usage')",
                [],
                |r| r.get(0),
            )
            .expect("count");
        assert_eq!(tables, 6);
    }

    #[test]
    fn newer_schema_is_refused() {
        let mut c = Connection::open_in_memory().expect("open");
        c.execute_batch("PRAGMA user_version = 99").expect("set");
        assert!(migrate(&mut c).is_err());
    }
}
