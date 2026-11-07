//! Persisted security audit trail.
//!
//! Handlers call [`Store::record_audit`] for identity and administration events. Rows are
//! plain text: no passwords, tokens, hashes or recovery codes belong here. Retention runs
//! with the usage sweep so the table cannot grow without bound.

use rusqlite::{params, OptionalExtension, Row};

use rrouter_core::{Error, Result};

use crate::migrations;
use crate::store::Store;

/// One audit row. `actor` is an email or the literal `setup`; `detail` is handler prose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditEvent {
    pub id: i64,
    pub ts: i64,
    pub kind: String,
    pub actor: Option<String>,
    pub ip: Option<String>,
    pub detail: Option<String>,
}

fn row_to_event(r: &Row<'_>) -> rusqlite::Result<AuditEvent> {
    Ok(AuditEvent {
        id: r.get(0)?,
        ts: r.get(1)?,
        kind: r.get(2)?,
        actor: r.get(3)?,
        ip: r.get(4)?,
        detail: r.get(5)?,
    })
}

impl Store {
    pub async fn record_audit(
        &self,
        kind: &str,
        actor: Option<&str>,
        ip: Option<&str>,
        detail: Option<&str>,
    ) -> Result<()> {
        let kind = kind.trim();
        if kind.is_empty() {
            return Err(Error::BadRequest("audit kind is required".into()));
        }
        let now = self.audit_now();
        self.conn()
            .execute(
                "INSERT INTO audit_events (ts, kind, actor, ip, detail) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![now, kind, actor, ip, detail],
            )
            .map_err(migrations::db_err)?;
        Ok(())
    }

    /// Newest first, optionally narrowed to one kind. `before` pages on the row id.
    pub async fn audit_events(
        &self,
        kind: Option<&str>,
        limit: usize,
        before: Option<i64>,
    ) -> Result<Vec<AuditEvent>> {
        let limit = i64::try_from(limit.clamp(1, 500)).unwrap_or(200);
        let conn = self.conn();
        let rows: Vec<AuditEvent> = match before {
            Some(before) => {
                let mut stmt = conn
                    .prepare(
                        "SELECT id, ts, kind, actor, ip, detail FROM audit_events \
                         WHERE (?1 IS NULL OR kind = ?1) AND id < ?3 ORDER BY id DESC LIMIT ?2",
                    )
                    .map_err(migrations::db_err)?;
                let rows = stmt
                    .query_map(params![kind, limit, before], row_to_event)
                    .map_err(migrations::db_err)?;
                rows.collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(migrations::db_err)?
            }
            None => {
                let mut stmt = conn
                    .prepare(
                        "SELECT id, ts, kind, actor, ip, detail FROM audit_events \
                         WHERE (?1 IS NULL OR kind = ?1) ORDER BY id DESC LIMIT ?2",
                    )
                    .map_err(migrations::db_err)?;
                let rows = stmt
                    .query_map(params![kind, limit], row_to_event)
                    .map_err(migrations::db_err)?;
                rows.collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(migrations::db_err)?
            }
        };
        Ok(rows)
    }

    pub async fn prune_audit(&self, cutoff: i64) -> Result<usize> {
        self.conn()
            .execute("DELETE FROM audit_events WHERE ts < ?1", params![cutoff])
            .map_err(migrations::db_err)
    }

    /// One row, mostly for tests and future single-event views.
    pub async fn audit_event(&self, id: i64) -> Result<Option<AuditEvent>> {
        self.conn()
            .query_row(
                "SELECT id, ts, kind, actor, ip, detail FROM audit_events WHERE id = ?1",
                params![id],
                row_to_event,
            )
            .optional()
            .map_err(migrations::db_err)
    }

    fn audit_now(&self) -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
    }
}
