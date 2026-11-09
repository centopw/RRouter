//! Eval suites, cases, runs and per-case results.
//!
//! A suite is a named list of cases (an input, and optionally the text a correct answer
//! should contain). A run pins one suite to one model: the gateway executes every case
//! through its normal `/v1` path and writes one result row per case. The tables have no
//! foreign keys (the same choice `prompt_templates` makes), so a delete is an explicit,
//! transactional cascade here rather than a per-connection pragma.
//!
//! # Ordering
//!
//! Cases read in `(created_at, id)` order and results are joined back to their case, so a
//! run's results always appear in case order even though the runner finishes them in
//! whatever order the completions return. That is the only stable ordering a table of
//! UUIDs can offer.
//!
//! # Bounds
//!
//! Sizes are validated here, not only in the API: a case's input and expected text are
//! capped, a suite holds a bounded number of cases, and a run's model name is capped. A
//! run that fits those bounds can still be long; the runner (gateway side) bounds its own
//! concurrency and per-case deadline.

use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use rrouter_core::{Error, Result};

use crate::migrations;
use crate::store::Store;

/// Longest suite or case name.
pub const MAX_EVAL_NAME: usize = 80;
/// Longest `input` or `expected` body, in characters.
pub const MAX_EVAL_TEXT: usize = 8_000;
/// Longest model reference a run may name.
pub const MAX_EVAL_MODEL: usize = 200;
/// Cases a suite may hold. A suite is a smoke set, not a benchmark corpus.
pub const MAX_EVAL_CASES: usize = 200;
/// Runs returned by a list call.
pub const EVAL_RUN_PAGE: usize = 50;

/// Where a run is in its lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvalRunStatus {
    /// Created, not yet picked up by the runner.
    Pending,
    /// The runner is executing cases.
    Running,
    /// Every case produced a result (some may have failed individually).
    Completed,
    /// The run itself failed — no case results are trustworthy.
    Failed,
}

impl EvalRunStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim() {
            "pending" => Some(Self::Pending),
            "running" => Some(Self::Running),
            "completed" => Some(Self::Completed),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }

    pub fn from_stored(raw: &str) -> Self {
        Self::parse(raw).unwrap_or(Self::Failed)
    }

    /// Is this run still moving? The dashboard polls only while this is true.
    pub fn is_active(self) -> bool {
        matches!(self, Self::Pending | Self::Running)
    }
}

/// One suite.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalSuite {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// One case: an input and the text a correct answer should contain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalCase {
    pub id: Uuid,
    pub suite_id: Uuid,
    pub name: Option<String>,
    pub input: String,
    /// `None` means "record the output without scoring it".
    pub expected: Option<String>,
    pub created_at: i64,
}

/// One run of one suite against one model.
#[derive(Debug, Clone, PartialEq)]
pub struct EvalRun {
    pub id: Uuid,
    pub suite_id: Uuid,
    pub model: String,
    pub status: EvalRunStatus,
    pub created_at: i64,
    pub completed_at: Option<i64>,
    /// `{cases, passed, failed, errors, scored, duration_ms}` once the run finishes.
    pub summary: Option<serde_json::Value>,
}

/// One case's outcome, with the case label joined in for display and stable ordering.
#[derive(Debug, Clone, PartialEq)]
pub struct EvalResult {
    pub id: Uuid,
    pub run_id: Uuid,
    pub case_id: Uuid,
    pub case_name: Option<String>,
    pub case_input: String,
    pub output: Option<String>,
    pub score: Option<f64>,
    pub latency_ms: Option<u32>,
    pub error: Option<String>,
    pub created_at: i64,
}

/// The numbers a finished run reports.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct EvalSummary {
    pub cases: usize,
    /// Cases whose output contained the expected text.
    pub passed: usize,
    /// Scored cases whose output did not.
    pub failed: usize,
    /// Cases with no expectation: the output was recorded, not judged.
    pub unscored: usize,
    /// Cases that could not be executed at all.
    pub errors: usize,
    pub duration_ms: u64,
    /// Set only when the run itself failed (the suite could not be read).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl EvalSummary {
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "cases": self.cases,
            "passed": self.passed,
            "failed": self.failed,
            "unscored": self.unscored,
            "errors": self.errors,
            "duration_ms": self.duration_ms,
            "error": self.error,
        })
    }
}

const SUITE_COLUMNS: &str = "id, name, description, created_at, updated_at";
const CASE_COLUMNS: &str = "id, suite_id, name, input, expected, created_at";
const RUN_COLUMNS: &str = "id, suite_id, model, status, created_at, completed_at, summary";

fn row_to_suite(r: &Row<'_>) -> rusqlite::Result<EvalSuite> {
    Ok(EvalSuite {
        id: uuid_col(0, r.get(0)?)?,
        name: r.get(1)?,
        description: r.get(2)?,
        created_at: r.get(3)?,
        updated_at: r.get(4)?,
    })
}

fn row_to_case(r: &Row<'_>) -> rusqlite::Result<EvalCase> {
    Ok(EvalCase {
        id: uuid_col(0, r.get(0)?)?,
        suite_id: uuid_col(1, r.get(1)?)?,
        name: r.get(2)?,
        input: r.get(3)?,
        expected: r.get(4)?,
        created_at: r.get(5)?,
    })
}

fn row_to_run(r: &Row<'_>) -> rusqlite::Result<EvalRun> {
    let status: String = r.get(3)?;
    let summary: Option<String> = r.get(6)?;
    Ok(EvalRun {
        id: uuid_col(0, r.get(0)?)?,
        suite_id: uuid_col(1, r.get(1)?)?,
        model: r.get(2)?,
        status: EvalRunStatus::from_stored(&status),
        created_at: r.get(4)?,
        completed_at: r.get(5)?,
        summary: summary.and_then(|raw| serde_json::from_str(&raw).ok()),
    })
}

fn row_to_result(r: &Row<'_>) -> rusqlite::Result<EvalResult> {
    Ok(EvalResult {
        id: uuid_col(0, r.get(0)?)?,
        run_id: uuid_col(1, r.get(1)?)?,
        case_id: uuid_col(2, r.get(2)?)?,
        case_name: r.get(3)?,
        case_input: r.get(4)?,
        output: r.get(5)?,
        score: r.get(6)?,
        latency_ms: r.get(7)?,
        error: r.get(8)?,
        created_at: r.get(9)?,
    })
}

fn uuid_col(index: usize, raw: String) -> rusqlite::Result<Uuid> {
    Uuid::parse_str(&raw).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(index, rusqlite::types::Type::Text, Box::new(e))
    })
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Trim, reject empty and over-long names.
fn check_name(raw: &str, what: &str) -> Result<String> {
    let name = raw.trim().to_string();
    if name.is_empty() {
        return Err(Error::BadRequest(format!("a {what} is required")));
    }
    if name.chars().count() > MAX_EVAL_NAME {
        return Err(Error::BadRequest(format!(
            "a {what} is limited to {MAX_EVAL_NAME} characters"
        )));
    }
    Ok(name)
}

/// Validate a body of text (an input or an expectation).
fn check_text(raw: &str, what: &str) -> Result<String> {
    let text = raw.trim().to_string();
    if text.is_empty() {
        return Err(Error::BadRequest(format!("a case {what} is required")));
    }
    if text.chars().count() > MAX_EVAL_TEXT {
        return Err(Error::BadRequest(format!(
            "a case {what} is limited to {MAX_EVAL_TEXT} characters"
        )));
    }
    Ok(text)
}

/// Does `output` satisfy `expected`? Case-insensitive containment: an expectation names
/// the fact the answer must state, not the whole sentence it must be.
pub fn eval_passes(output: &str, expected: &str) -> bool {
    let expected = expected.trim();
    if expected.is_empty() {
        return true;
    }
    output.to_lowercase().contains(&expected.to_lowercase())
}

impl Store {
    // ----- suites ------------------------------------------------------------------

    /// Create a suite. The name is unique; a duplicate is a `400`, not a raw constraint.
    pub async fn create_eval_suite(
        &self,
        name: &str,
        description: Option<&str>,
    ) -> Result<EvalSuite> {
        let name = check_name(name, "suite name")?;
        if self.eval_suite_by_name(&name).await?.is_some() {
            return Err(Error::BadRequest("that suite name is already taken".into()));
        }
        let stamp = now();
        let suite = EvalSuite {
            id: Uuid::new_v4(),
            name,
            description: description
                .map(str::trim)
                .filter(|d| !d.is_empty())
                .map(str::to_string),
            created_at: stamp,
            updated_at: stamp,
        };
        self.conn()
            .execute(
                "INSERT INTO eval_suites (id, name, description, created_at, updated_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    suite.id.to_string(),
                    suite.name,
                    suite.description,
                    suite.created_at,
                    suite.updated_at
                ],
            )
            .map_err(migrations::db_err)?;
        Ok(suite)
    }

    /// Every suite, by name — the order a dashboard list wants.
    pub async fn eval_suites(&self) -> Result<Vec<EvalSuite>> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {SUITE_COLUMNS} FROM eval_suites ORDER BY name ASC"
            ))
            .map_err(migrations::db_err)?;
        let rows = stmt
            .query_map([], row_to_suite)
            .map_err(migrations::db_err)?;
        rows.map(|r| r.map_err(migrations::db_err)).collect()
    }

    pub async fn eval_suite(&self, id: Uuid) -> Result<Option<EvalSuite>> {
        self.conn()
            .query_row(
                &format!("SELECT {SUITE_COLUMNS} FROM eval_suites WHERE id = ?1"),
                params![id.to_string()],
                row_to_suite,
            )
            .optional()
            .map_err(migrations::db_err)
    }

    pub async fn eval_suite_by_name(&self, name: &str) -> Result<Option<EvalSuite>> {
        self.conn()
            .query_row(
                &format!("SELECT {SUITE_COLUMNS} FROM eval_suites WHERE name = ?1"),
                params![name.trim()],
                row_to_suite,
            )
            .optional()
            .map_err(migrations::db_err)
    }

    /// Rename or re-describe a suite. Absent fields keep their value.
    pub async fn update_eval_suite(
        &self,
        id: Uuid,
        name: Option<&str>,
        description: Option<Option<&str>>,
    ) -> Result<Option<EvalSuite>> {
        let Some(mut suite) = self.eval_suite(id).await? else {
            return Ok(None);
        };
        if let Some(name) = name {
            let name = check_name(name, "suite name")?;
            if let Some(existing) = self.eval_suite_by_name(&name).await? {
                if existing.id != id {
                    return Err(Error::BadRequest("that suite name is already taken".into()));
                }
            }
            suite.name = name;
        }
        if let Some(description) = description {
            suite.description = description
                .map(str::trim)
                .filter(|d| !d.is_empty())
                .map(str::to_string);
        }
        suite.updated_at = now();
        self.conn()
            .execute(
                "UPDATE eval_suites SET name = ?2, description = ?3, updated_at = ?4 WHERE id = ?1",
                params![
                    id.to_string(),
                    suite.name,
                    suite.description,
                    suite.updated_at
                ],
            )
            .map_err(migrations::db_err)?;
        Ok(Some(suite))
    }

    /// Delete a suite and everything under it, in one transaction.
    pub async fn delete_eval_suite(&self, id: Uuid) -> Result<bool> {
        let mut conn = self.conn();
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(migrations::db_err)?;
        let suite = id.to_string();
        let n = tx
            .execute("DELETE FROM eval_suites WHERE id = ?1", params![suite])
            .map_err(migrations::db_err)?;
        if n == 0 {
            return Ok(false);
        }
        tx.execute(
            "DELETE FROM eval_results WHERE run_id IN (SELECT id FROM eval_runs WHERE suite_id = ?1)",
            params![suite],
        )
        .map_err(migrations::db_err)?;
        tx.execute("DELETE FROM eval_runs WHERE suite_id = ?1", params![suite])
            .map_err(migrations::db_err)?;
        tx.execute("DELETE FROM eval_cases WHERE suite_id = ?1", params![suite])
            .map_err(migrations::db_err)?;
        tx.commit().map_err(migrations::db_err)?;
        Ok(true)
    }

    // ----- cases -------------------------------------------------------------------

    /// Add a case. A suite is bounded, so the check is here and not only in the API.
    pub async fn create_eval_case(
        &self,
        suite_id: Uuid,
        name: Option<&str>,
        input: &str,
        expected: Option<&str>,
    ) -> Result<EvalCase> {
        if self.eval_suite(suite_id).await?.is_none() {
            return Err(Error::NotFound(format!("suite {suite_id}")));
        }
        let count = self.eval_case_count(suite_id).await?;
        if count >= MAX_EVAL_CASES {
            return Err(Error::BadRequest(format!(
                "a suite is limited to {MAX_EVAL_CASES} cases"
            )));
        }
        let case = EvalCase {
            id: Uuid::new_v4(),
            suite_id,
            name: name
                .map(str::trim)
                .filter(|n| !n.is_empty())
                .map(str::to_string),
            input: check_text(input, "input")?,
            expected: match expected {
                None => None,
                Some(raw) if raw.trim().is_empty() => None,
                Some(raw) => Some(check_text(raw, "expectation")?),
            },
            created_at: now(),
        };
        self.conn()
            .execute(
                "INSERT INTO eval_cases (id, suite_id, name, input, expected, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    case.id.to_string(),
                    suite_id.to_string(),
                    case.name,
                    case.input,
                    case.expected,
                    case.created_at
                ],
            )
            .map_err(migrations::db_err)?;
        Ok(case)
    }

    /// A suite's cases in stable order.
    pub async fn eval_cases(&self, suite_id: Uuid) -> Result<Vec<EvalCase>> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {CASE_COLUMNS} FROM eval_cases WHERE suite_id = ?1 \
                 ORDER BY created_at ASC, rowid ASC"
            ))
            .map_err(migrations::db_err)?;
        let rows = stmt
            .query_map(params![suite_id.to_string()], row_to_case)
            .map_err(migrations::db_err)?;
        rows.map(|r| r.map_err(migrations::db_err)).collect()
    }

    pub async fn eval_case_count(&self, suite_id: Uuid) -> Result<usize> {
        self.conn()
            .query_row(
                "SELECT count(*) FROM eval_cases WHERE suite_id = ?1",
                params![suite_id.to_string()],
                |r| r.get::<_, i64>(0),
            )
            .map(|n| n as usize)
            .map_err(migrations::db_err)
    }

    pub async fn delete_eval_case(&self, id: Uuid) -> Result<bool> {
        let n = self
            .conn()
            .execute(
                "DELETE FROM eval_cases WHERE id = ?1",
                params![id.to_string()],
            )
            .map_err(migrations::db_err)?;
        Ok(n > 0)
    }

    // ----- runs --------------------------------------------------------------------

    /// Create a run in `pending`. The caller starts the runner; the store only records.
    pub async fn create_eval_run(&self, suite_id: Uuid, model: &str) -> Result<EvalRun> {
        if self.eval_suite(suite_id).await?.is_none() {
            return Err(Error::NotFound(format!("suite {suite_id}")));
        }
        let model = model.trim();
        if model.is_empty() {
            return Err(Error::BadRequest("a model is required".into()));
        }
        if model.chars().count() > MAX_EVAL_MODEL {
            return Err(Error::BadRequest(format!(
                "a model reference is limited to {MAX_EVAL_MODEL} characters"
            )));
        }
        let run = EvalRun {
            id: Uuid::new_v4(),
            suite_id,
            model: model.to_string(),
            status: EvalRunStatus::Pending,
            created_at: now(),
            completed_at: None,
            summary: None,
        };
        self.conn()
            .execute(
                "INSERT INTO eval_runs (id, suite_id, model, status, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    run.id.to_string(),
                    suite_id.to_string(),
                    run.model,
                    run.status.as_str(),
                    run.created_at
                ],
            )
            .map_err(migrations::db_err)?;
        Ok(run)
    }

    /// Newest first; optionally one suite's.
    pub async fn eval_runs(&self, suite_id: Option<Uuid>, limit: usize) -> Result<Vec<EvalRun>> {
        let conn = self.conn();
        let limit = limit.clamp(1, EVAL_RUN_PAGE) as i64;
        let mut out = Vec::new();
        match suite_id {
            None => {
                let mut stmt = conn
                    .prepare(&format!(
                        "SELECT {RUN_COLUMNS} FROM eval_runs ORDER BY created_at DESC LIMIT ?1"
                    ))
                    .map_err(migrations::db_err)?;
                let rows = stmt
                    .query_map(params![limit], row_to_run)
                    .map_err(migrations::db_err)?;
                for row in rows {
                    out.push(row.map_err(migrations::db_err)?);
                }
            }
            Some(suite) => {
                let mut stmt = conn
                    .prepare(&format!(
                        "SELECT {RUN_COLUMNS} FROM eval_runs WHERE suite_id = ?1 \
                         ORDER BY created_at DESC LIMIT ?2"
                    ))
                    .map_err(migrations::db_err)?;
                let rows = stmt
                    .query_map(params![suite.to_string(), limit], row_to_run)
                    .map_err(migrations::db_err)?;
                for row in rows {
                    out.push(row.map_err(migrations::db_err)?);
                }
            }
        }
        Ok(out)
    }

    pub async fn eval_run(&self, id: Uuid) -> Result<Option<EvalRun>> {
        self.conn()
            .query_row(
                &format!("SELECT {RUN_COLUMNS} FROM eval_runs WHERE id = ?1"),
                params![id.to_string()],
                row_to_run,
            )
            .optional()
            .map_err(migrations::db_err)
    }

    /// Move a run from `pending` to `running`. `None` when it is not pending, so a
    /// duplicated start cannot run the same suite twice.
    pub async fn start_eval_run(&self, id: Uuid) -> Result<Option<EvalRun>> {
        let n = self
            .conn()
            .execute(
                "UPDATE eval_runs SET status = ?2 WHERE id = ?1 AND status = ?3",
                params![
                    id.to_string(),
                    EvalRunStatus::Running.as_str(),
                    EvalRunStatus::Pending.as_str()
                ],
            )
            .map_err(migrations::db_err)?;
        if n == 0 {
            return Ok(None);
        }
        self.eval_run(id).await
    }

    /// Fail runs that were still active *before* `cutoff`, with the reason. Called once at
    /// startup: a run is in-process work, so anything `pending`/`running` when the process
    /// boots belongs to a process that is gone, and leaving it that way would make the
    /// dashboard poll forever.
    ///
    /// `created_at < cutoff` is what makes the sweep safe to run concurrently with serving:
    /// a run created after the cutoff is live work and is never touched. A run created in
    /// the same second as the boot survives this sweep and is caught by the next one.
    pub async fn fail_orphan_eval_runs(&self, cutoff: i64, reason: &str) -> Result<usize> {
        let summary = EvalSummary {
            cases: 0,
            errors: 1,
            error: Some(reason.to_string()),
            ..EvalSummary::default()
        };
        self.conn()
            .execute(
                "UPDATE eval_runs SET status = ?1, completed_at = ?2, summary = ?3 \
                 WHERE status IN (?4, ?5) AND created_at < ?6",
                params![
                    EvalRunStatus::Failed.as_str(),
                    now(),
                    summary.to_json().to_string(),
                    EvalRunStatus::Pending.as_str(),
                    EvalRunStatus::Running.as_str(),
                    cutoff
                ],
            )
            .map_err(migrations::db_err)
    }

    /// Append one case outcome.
    pub async fn insert_eval_result(
        &self,
        run_id: Uuid,
        case_id: Uuid,
        output: Option<&str>,
        score: Option<f64>,
        latency_ms: Option<u32>,
        error: Option<&str>,
    ) -> Result<EvalResult> {
        let id = Uuid::new_v4();
        let created_at = now();
        self.conn()
            .execute(
                "INSERT INTO eval_results (id, run_id, case_id, output, score, latency_ms, error, \
                 created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    id.to_string(),
                    run_id.to_string(),
                    case_id.to_string(),
                    output,
                    score,
                    latency_ms,
                    error,
                    created_at
                ],
            )
            .map_err(migrations::db_err)?;
        Ok(EvalResult {
            id,
            run_id,
            case_id,
            case_name: None,
            case_input: String::new(),
            output: output.map(str::to_string),
            score,
            latency_ms,
            error: error.map(str::to_string),
            created_at,
        })
    }

    /// Finish a run: status, timestamp and summary in one transaction.
    ///
    /// Only an active run is completed, so a late write cannot reopen a finished one.
    pub async fn complete_eval_run(
        &self,
        id: Uuid,
        status: EvalRunStatus,
        summary: EvalSummary,
    ) -> Result<Option<EvalRun>> {
        // The guard is confined to this block: a `MutexGuard` held across the `await`
        // below would make the future non-`Send`, and the runner spawns this on tokio.
        let updated = {
            let mut conn = self.conn();
            let tx = conn
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(migrations::db_err)?;
            let n = tx
                .execute(
                    "UPDATE eval_runs SET status = ?2, completed_at = ?3, summary = ?4 \
                     WHERE id = ?1 AND status IN (?5, ?6)",
                    params![
                        id.to_string(),
                        status.as_str(),
                        now(),
                        summary.to_json().to_string(),
                        EvalRunStatus::Pending.as_str(),
                        EvalRunStatus::Running.as_str()
                    ],
                )
                .map_err(migrations::db_err)?;
            tx.commit().map_err(migrations::db_err)?;
            n > 0
        };
        if !updated {
            return Ok(None);
        }
        self.eval_run(id).await
    }

    /// A run's results in case order, with the case label joined in.
    ///
    /// Joined rather than read per result so the dashboard can render a table from one
    /// query; ordered by the case's `(created_at, id)` so a run always reads the same way.
    pub async fn eval_results(&self, run_id: Uuid) -> Result<Vec<EvalResult>> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(
                "SELECT r.id, r.run_id, r.case_id, c.name, c.input, r.output, r.score, \
                        r.latency_ms, r.error, r.created_at \
                 FROM eval_results r JOIN eval_cases c ON c.id = r.case_id \
                 WHERE r.run_id = ?1 ORDER BY c.created_at ASC, c.rowid ASC",
            )
            .map_err(migrations::db_err)?;
        let rows = stmt
            .query_map(params![run_id.to_string()], row_to_result)
            .map_err(migrations::db_err)?;
        rows.map(|r| r.map_err(migrations::db_err)).collect()
    }

    /// Count of results already written for a run — the runner's progress signal.
    pub async fn eval_result_count(&self, run_id: Uuid) -> Result<usize> {
        self.conn()
            .query_row(
                "SELECT count(*) FROM eval_results WHERE run_id = ?1",
                params![run_id.to_string()],
                |r| r.get::<_, i64>(0),
            )
            .map(|n| n as usize)
            .map_err(migrations::db_err)
    }
}
