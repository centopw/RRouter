//! Teams: the tenancy unit above a gateway key.
//!
//! A team is a name, a budget and a membership list. Keys point at one
//! (`api_keys.team_id`), and `usage.team_id` records the team a request was billed to, so
//! "what did this team spend" is an index scan rather than a join through the key table.
//!
//! Tenancy is nullable on purpose: a key without a team, and traffic whose key had none,
//! behave exactly as they did before v23. `None` means "instance-wide", never "unassigned
//! team", so nothing here can silently relabel existing rows.
//!
//! Budgets copy the key budgets' shape — `0` is unlimited and [`BudgetWindow`] spells the
//! period — so one set of window arithmetic serves both. Nothing enforces a team budget
//! yet; see `Store::team_spend` for the read side the gateway's admission path will use.

use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use rrouter_core::{Error, Result};

use crate::migrations;
use crate::store::{BudgetWindow, Store};

/// Longest team name a dashboard can render in a badge without truncating to noise.
pub const MAX_TEAM_NAME: usize = 80;

/// What a membership allows inside its team.
///
/// `TeamAdmin` is scoped administration — the role exists so a member can own their
/// team's roster without a global admin. Mutating a roster is still a global-admin action
/// in this release (the reader gate admits safe methods only); the role is stored and
/// reported so the next release can widen that without a migration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TeamRole {
    TeamAdmin,
    Member,
    Viewer,
}

impl TeamRole {
    /// The column and wire spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TeamAdmin => "team_admin",
            Self::Member => "member",
            Self::Viewer => "viewer",
        }
    }

    /// Strict parse for client input: any other spelling is `None`.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim() {
            "team_admin" => Some(Self::TeamAdmin),
            "member" => Some(Self::Member),
            "viewer" => Some(Self::Viewer),
            _ => None,
        }
    }

    /// Lenient read for a stored row; an unrecognised role is the least privileged one.
    pub fn from_stored(raw: &str) -> Self {
        Self::parse(raw).unwrap_or(Self::Viewer)
    }
}

/// One team, as the admin API and the dashboard render it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Team {
    pub id: Uuid,
    pub name: String,
    pub created_at: i64,
    /// Tokens per `budget_window`; 0 = unlimited.
    pub budget_tokens: u64,
    /// Micro-USD per `budget_window`; 0 = unlimited.
    pub budget_micros: u64,
    pub budget_window: BudgetWindow,
}

/// One membership row. `user_id` is a `users.id`; the row survives a team rename and
/// dies with either the team or the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TeamMember {
    pub team_id: Uuid,
    pub user_id: Uuid,
    pub role: TeamRole,
    pub created_at: i64,
}

/// What a team has spent inside a window, read from `usage` at request time.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TeamSpend {
    pub requests: u64,
    pub tokens: u64,
    pub cost_micros: u64,
}

/// Fields a `PATCH /api/teams/{id}` may change. Absent = leave alone; `name` cannot be
/// null because a team always has one.
#[derive(Debug, Clone, Default)]
pub struct TeamPatch {
    pub name: Option<String>,
    pub budget_tokens: Option<u64>,
    pub budget_micros: Option<u64>,
    pub budget_window: Option<BudgetWindow>,
}

impl TeamPatch {
    /// Apply in place. Pure, so the merge rule is testable without SQL.
    pub fn apply(&self, team: &mut Team) {
        if let Some(name) = &self.name {
            team.name = normalize_name(name);
        }
        if let Some(v) = self.budget_tokens {
            team.budget_tokens = v;
        }
        if let Some(v) = self.budget_micros {
            team.budget_micros = v;
        }
        if let Some(window) = self.budget_window {
            team.budget_window = window;
        }
    }
}

/// Trim, reject empty and over-long names. Trimmed because a badge with a leading space
/// is a bug nobody reports and everybody sees.
pub fn normalize_name(raw: &str) -> String {
    raw.trim().to_string()
}

fn check_name(name: &str) -> Result<String> {
    let name = normalize_name(name);
    if name.is_empty() {
        return Err(Error::BadRequest("a team name is required".into()));
    }
    if name.chars().count() > MAX_TEAM_NAME {
        return Err(Error::BadRequest(format!(
            "a team name is limited to {MAX_TEAM_NAME} characters"
        )));
    }
    Ok(name)
}

const TEAM_COLUMNS: &str = "id, name, created_at, budget_tokens, budget_micros, budget_window";

/// The same columns qualified for the membership join.
const TEAM_COLUMNS_T: &str = "t.id, t.name, t.created_at, t.budget_tokens, t.budget_micros, \
     t.budget_window";

fn row_to_team(r: &Row<'_>) -> rusqlite::Result<Team> {
    Ok(Team {
        id: uuid_col(r.get(0)?)?,
        name: r.get(1)?,
        created_at: r.get(2)?,
        budget_tokens: r.get(3)?,
        budget_micros: r.get(4)?,
        budget_window: BudgetWindow::from_stored(&r.get::<_, String>(5)?),
    })
}

fn row_to_member(r: &Row<'_>) -> rusqlite::Result<TeamMember> {
    Ok(TeamMember {
        team_id: uuid_col(r.get(0)?)?,
        user_id: uuid_col(r.get(1)?)?,
        role: TeamRole::from_stored(&r.get::<_, String>(2)?),
        created_at: r.get(3)?,
    })
}

fn uuid_col(s: String) -> rusqlite::Result<Uuid> {
    Uuid::parse_str(&s).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

impl Store {
    // ----- teams -------------------------------------------------------------------

    /// Create a team. The name is the unique key, so a duplicate is a `400` naming the
    /// field rather than a raw constraint message.
    pub async fn create_team(
        &self,
        name: &str,
        budget_tokens: u64,
        budget_micros: u64,
        budget_window: BudgetWindow,
    ) -> Result<Team> {
        let name = check_name(name)?;
        if self.team_by_name(&name).await?.is_some() {
            return Err(Error::BadRequest("that team name is already taken".into()));
        }
        let team = Team {
            id: Uuid::new_v4(),
            name,
            created_at: now(),
            budget_tokens,
            budget_micros,
            budget_window,
        };
        self.conn()
            .execute(
                "INSERT INTO teams (id, name, created_at, budget_tokens, budget_micros, \
                 budget_window) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    team.id.to_string(),
                    team.name,
                    team.created_at,
                    team.budget_tokens,
                    team.budget_micros,
                    team.budget_window.as_str(),
                ],
            )
            .map_err(migrations::db_err)?;
        Ok(team)
    }

    /// Every team, by name — the order an operator expects in a picker.
    pub async fn teams(&self) -> Result<Vec<Team>> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {TEAM_COLUMNS} FROM teams ORDER BY name ASC"
            ))
            .map_err(migrations::db_err)?;
        let rows = stmt
            .query_map([], row_to_team)
            .map_err(migrations::db_err)?;
        rows.map(|r| r.map_err(migrations::db_err)).collect()
    }

    pub async fn team(&self, id: Uuid) -> Result<Option<Team>> {
        self.conn()
            .query_row(
                &format!("SELECT {TEAM_COLUMNS} FROM teams WHERE id = ?1"),
                params![id.to_string()],
                row_to_team,
            )
            .optional()
            .map_err(migrations::db_err)
    }

    pub async fn team_by_name(&self, name: &str) -> Result<Option<Team>> {
        self.conn()
            .query_row(
                &format!("SELECT {TEAM_COLUMNS} FROM teams WHERE name = ?1"),
                params![normalize_name(name)],
                row_to_team,
            )
            .optional()
            .map_err(migrations::db_err)
    }

    /// Merge `patch` into a team, or `None` when the id does not exist.
    pub async fn update_team(&self, id: Uuid, patch: &TeamPatch) -> Result<Option<Team>> {
        let Some(mut team) = self.team(id).await? else {
            return Ok(None);
        };
        if let Some(name) = &patch.name {
            let name = check_name(name)?;
            if let Some(existing) = self.team_by_name(&name).await? {
                if existing.id != id {
                    return Err(Error::BadRequest("that team name is already taken".into()));
                }
            }
        }
        patch.apply(&mut team);
        self.conn()
            .execute(
                "UPDATE teams SET name = ?2, budget_tokens = ?3, budget_micros = ?4, \
                 budget_window = ?5 WHERE id = ?1",
                params![
                    id.to_string(),
                    team.name,
                    team.budget_tokens,
                    team.budget_micros,
                    team.budget_window.as_str(),
                ],
            )
            .map_err(migrations::db_err)?;
        Ok(Some(team))
    }

    /// Delete a team and its memberships.
    ///
    /// Keys are *released*, not deleted: a key that outlived its team becomes instance-wide
    /// (the pre-v23 shape) instead of silently disappearing from the dashboard. Usage rows
    /// keep their `team_id` — spend history is a record of what happened, and rewriting it
    /// would change reported costs after the fact.
    pub async fn delete_team(&self, id: Uuid) -> Result<bool> {
        let mut conn = self.conn();
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(migrations::db_err)?;
        let team = id.to_string();
        let n = tx
            .execute("DELETE FROM teams WHERE id = ?1", params![team])
            .map_err(migrations::db_err)?;
        if n == 0 {
            return Ok(false);
        }
        tx.execute("DELETE FROM team_members WHERE team_id = ?1", params![team])
            .map_err(migrations::db_err)?;
        tx.execute(
            "UPDATE api_keys SET team_id = NULL WHERE team_id = ?1",
            params![team],
        )
        .map_err(migrations::db_err)?;
        // Provider accounts follow the same rule as keys: released to the instance, not
        // deleted. Leaving a dangling team id would hide the account from every team key
        // while the global list still showed it.
        tx.execute(
            "UPDATE connections SET team_id = NULL WHERE team_id = ?1",
            params![team],
        )
        .map_err(migrations::db_err)?;
        tx.commit().map_err(migrations::db_err)?;
        Ok(true)
    }

    // ----- membership --------------------------------------------------------------

    /// Every member of one team, by role then creation order.
    pub async fn team_members(&self, team_id: Uuid) -> Result<Vec<TeamMember>> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(
                "SELECT team_id, user_id, role, created_at FROM team_members \
                 WHERE team_id = ?1 ORDER BY created_at ASC, user_id ASC",
            )
            .map_err(migrations::db_err)?;
        let rows = stmt
            .query_map(params![team_id.to_string()], row_to_member)
            .map_err(migrations::db_err)?;
        rows.map(|r| r.map_err(migrations::db_err)).collect()
    }

    /// Add a member, or change an existing member's role. Adding an unknown user or to an
    /// unknown team is a client error, not a dangling row.
    pub async fn add_team_member(
        &self,
        team_id: Uuid,
        user_id: Uuid,
        role: TeamRole,
    ) -> Result<TeamMember> {
        if self.team(team_id).await?.is_none() {
            return Err(Error::NotFound(format!("team {team_id}")));
        }
        if self.user(user_id).await?.is_none() {
            return Err(Error::NotFound(format!("user {user_id}")));
        }
        self.conn()
            .execute(
                "INSERT INTO team_members (team_id, user_id, role, created_at) \
                 VALUES (?1, ?2, ?3, ?4) \
                 ON CONFLICT(team_id, user_id) DO UPDATE SET role = excluded.role",
                params![
                    team_id.to_string(),
                    user_id.to_string(),
                    role.as_str(),
                    now()
                ],
            )
            .map_err(migrations::db_err)?;
        // Read the row back: an upsert keeps the original join date.
        self.team_member(team_id, user_id)
            .await?
            .ok_or_else(|| Error::Store("membership vanished after insert".into()))
    }

    /// One membership row.
    pub async fn team_member(&self, team_id: Uuid, user_id: Uuid) -> Result<Option<TeamMember>> {
        self.conn()
            .query_row(
                "SELECT team_id, user_id, role, created_at FROM team_members \
                 WHERE team_id = ?1 AND user_id = ?2",
                params![team_id.to_string(), user_id.to_string()],
                row_to_member,
            )
            .optional()
            .map_err(migrations::db_err)
    }

    pub async fn remove_team_member(&self, team_id: Uuid, user_id: Uuid) -> Result<bool> {
        let n = self
            .conn()
            .execute(
                "DELETE FROM team_members WHERE team_id = ?1 AND user_id = ?2",
                params![team_id.to_string(), user_id.to_string()],
            )
            .map_err(migrations::db_err)?;
        Ok(n > 0)
    }

    /// This user's role on this team, or `None` when they are not a member.
    pub async fn team_member_role(&self, team_id: Uuid, user_id: Uuid) -> Result<Option<TeamRole>> {
        self.conn()
            .query_row(
                "SELECT role FROM team_members WHERE team_id = ?1 AND user_id = ?2",
                params![team_id.to_string(), user_id.to_string()],
                |r| Ok(TeamRole::from_stored(&r.get::<_, String>(0)?)),
            )
            .optional()
            .map_err(migrations::db_err)
    }

    /// The teams a user belongs to — what a reader is allowed to see.
    pub async fn teams_for_user(&self, user_id: Uuid) -> Result<Vec<Team>> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {TEAM_COLUMNS_T} FROM teams t JOIN team_members m ON m.team_id = t.id \
                 WHERE m.user_id = ?1 ORDER BY t.name ASC"
            ))
            .map_err(migrations::db_err)?;
        let rows = stmt
            .query_map(params![user_id.to_string()], row_to_team)
            .map_err(migrations::db_err)?;
        rows.map(|r| r.map_err(migrations::db_err)).collect()
    }

    // ----- spend -------------------------------------------------------------------

    /// Requests, tokens and cost this team spent at or after `since`.
    ///
    /// `usage.team_id` is written when the request is recorded, so this is one index scan
    /// on `(team_id, ts)` — the shape the v23 migration added for exactly this question.
    pub async fn team_spend(&self, team_id: Uuid, since: Option<i64>) -> Result<TeamSpend> {
        self.conn()
            .query_row(
                "SELECT count(*), COALESCE(SUM(input_tokens + output_tokens), 0), \
                        COALESCE(SUM(cost_micros), 0) \
                 FROM usage WHERE team_id = ?1 AND (?2 IS NULL OR ts >= ?2)",
                params![team_id.to_string(), since],
                |r| {
                    Ok(TeamSpend {
                        requests: r.get(0)?,
                        tokens: r.get(1)?,
                        cost_micros: r.get(2)?,
                    })
                },
            )
            .map_err(migrations::db_err)
    }

    /// The team a request was billed to, or `None` when it was anonymous or predates v23.
    ///
    /// Used to keep `/api/usage/{request_id}` inside a reader's scope: a request id is a
    /// guessable capability, so existence must not outrun authority.
    pub async fn usage_team(&self, request_id: Uuid) -> Result<Option<Uuid>> {
        let raw: Option<Option<String>> = self
            .conn()
            .query_row(
                "SELECT team_id FROM usage WHERE request_id = ?1 LIMIT 1",
                params![request_id.to_string()],
                |r| r.get(0),
            )
            .optional()
            .map_err(migrations::db_err)?;
        Ok(raw.flatten().and_then(|id| Uuid::parse_str(&id).ok()))
    }
}
