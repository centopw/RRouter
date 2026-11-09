//! `/api/teams` — the tenancy unit above a gateway key.
//!
//! Reads are open to every signed-in account but scoped: an admin sees every team, a
//! member or viewer sees only the teams they belong to, and an account with no team sees
//! nothing.
//!
//! Two mutations are delegated. A `team_admin` may manage membership of their own team
//! (never their own row) and read that team's key rows; everything else — creating,
//! renaming or deleting a team, changing a budget, touching another team, and the whole
//! admin surface — stays global-admin. The gate lives in `lib::require_team_admin`; the
//! handlers below add the "not yourself" rule and the per-team read check.
//!
//! The scoping helpers here (`reader_teams`, `scoped_teams`) are shared with
//! `/api/usage*`: tenancy means the same thing on every read.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use rrouter_core::Error;
use rrouter_store::{BudgetWindow, Team, TeamPatch, TeamRole};
use serde_json::{json, Value};
use uuid::Uuid;

use super::auth::current_user;
use super::{admin_error, body_json, ok_json};
use crate::state::AppState;

/// A team as the API returns it: budget, roster size and spend.
fn team_json(team: &Team, members: usize, spend: rrouter_store::TeamSpend) -> Value {
    json!({
        "id": team.id.to_string(),
        "name": team.name,
        "created_at": team.created_at,
        "budget_tokens": team.budget_tokens,
        "budget_micros": team.budget_micros,
        "budget_window": team.budget_window.as_str(),
        "members": members,
        "spend": {
            "requests": spend.requests,
            "tokens": spend.tokens,
            "cost_micros": spend.cost_micros,
        },
    })
}

/// Build the row for one team, reading its roster size and spend.
async fn view(state: &AppState, team: &Team) -> Result<Value, Error> {
    let members = state.store.team_members(team.id).await?;
    let since = rrouter_store::window_start(team.budget_window, crate::now_unix());
    let spend = state.store.team_spend(team.id, since).await?;
    Ok(team_json(team, members.len(), spend))
}

/// The teams a caller may read. `None` = admin authority (every team); `Some(ids)` = the
/// caller's own memberships, possibly empty.
pub(crate) async fn reader_teams(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<Option<Vec<Uuid>>, Box<Response>> {
    let Some(user) = current_user(state, headers).await else {
        // No dashboard session: the request came through the admin token, an admin key or
        // loopback, all of which are admin authority by definition.
        return Ok(None);
    };
    if user.is_admin() {
        return Ok(None);
    }
    let teams = state
        .store
        .teams_for_user(user.id)
        .await
        .map_err(|e| Box::new(admin_error(&e)))?;
    Ok(Some(teams.into_iter().map(|team| team.id).collect()))
}

/// Narrow the reader's scope by an explicit `?team_id=`.
///
/// An admin asking for one team gets exactly that team. A reader asking for a team they do
/// not belong to is refused — the alternative (silently returning their own teams) would
/// make a typo look like a permissions bug.
pub(crate) async fn scoped_teams(
    state: &AppState,
    headers: &HeaderMap,
    requested: Option<Uuid>,
) -> Result<Option<Vec<Uuid>>, Box<Response>> {
    let scope = reader_teams(state, headers).await?;
    match (scope, requested) {
        (None, None) => Ok(None),
        (None, Some(team)) => Ok(Some(vec![team])),
        (Some(mine), None) => Ok(Some(mine)),
        (Some(mine), Some(team)) => {
            if mine.contains(&team) {
                Ok(Some(vec![team]))
            } else {
                Err(Box::new(admin_error(&Error::Forbidden {
                    code: "team_forbidden",
                    message: "this account is not a member of that team".into(),
                })))
            }
        }
    }
}

/// How much authority the caller has over one team.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TeamAuthority {
    /// Gateway admin: every team, member or not.
    Admin,
    /// `team_admin` on this team: roster and key visibility, nothing else.
    TeamAdmin,
}

/// The caller on a team-scoped request: what they may do, and who they are.
pub(crate) struct TeamCaller {
    pub authority: TeamAuthority,
    /// The signed-in account, `None` for the admin token, an admin key or loopback.
    pub user: Option<Uuid>,
}

impl TeamCaller {
    /// A team admin cannot change their own membership: with no gateway admin on the team
    /// that is how a roster locks itself out. A global admin is unaffected.
    pub fn may_manage(&self, member: Uuid) -> bool {
        match self.authority {
            TeamAuthority::Admin => true,
            TeamAuthority::TeamAdmin => self.user != Some(member),
        }
    }

    pub fn cannot_manage_self(&self) -> Error {
        Error::Forbidden {
            code: "cannot_manage_self",
            message: "a team admin cannot change their own membership; ask a gateway admin".into(),
        }
    }
}

/// Resolve the caller's authority over `team_id`.
///
/// `Admin` for a request with no dashboard session (the admin token, an admin key or
/// loopback all reach here as admin) and for a session whose account has the `admin` role.
/// Otherwise the membership decides: `team_admin` passes, every other answer is a `403`
/// that does not distinguish "not a member" from "no such team".
pub(crate) async fn team_authority(
    state: &AppState,
    headers: &HeaderMap,
    team_id: Uuid,
) -> Result<TeamCaller, Box<Response>> {
    let Some(user) = current_user(state, headers).await else {
        return Ok(TeamCaller {
            authority: TeamAuthority::Admin,
            user: None,
        });
    };
    if user.is_admin() {
        return Ok(TeamCaller {
            authority: TeamAuthority::Admin,
            user: Some(user.id),
        });
    }
    match state.store.team_member_role(team_id, user.id).await {
        Ok(Some(TeamRole::TeamAdmin)) => Ok(TeamCaller {
            authority: TeamAuthority::TeamAdmin,
            user: Some(user.id),
        }),
        Ok(_) => Err(Box::new(admin_error(&Error::Forbidden {
            code: "role_required",
            message: "this account cannot manage that team".into(),
        }))),
        Err(e) => Err(Box::new(admin_error(&e))),
    }
}

/// `GET /api/teams/{id}/keys` — one team's key rows.
///
/// An admin, or that team's admin. Never the plaintext or the hash: the rows come from
/// [`super::keys::rows_for_team`], the same projection `GET /api/keys` uses.
pub async fn keys(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let Ok(team_id) = Uuid::parse_str(&id) else {
        return admin_error(&Error::NotFound("team".into()));
    };
    if let Err(response) = team_authority(&state, &headers, team_id).await {
        return *response;
    }
    match super::keys::rows_for_team(&state, team_id).await {
        Ok(rows) => ok_json(json!({ "data": rows })),
        Err(e) => admin_error(&e),
    }
}

/// `GET /api/teams` — every team for an admin, the caller's teams otherwise.
pub async fn list(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let rows = match reader_teams(&state, &headers).await {
        Ok(scope) => match scope {
            None => state.store.teams().await,
            Some(ids) => {
                let mut out = Vec::with_capacity(ids.len());
                for id in ids {
                    match state.store.team(id).await {
                        Ok(Some(team)) => out.push(team),
                        Ok(None) => {}
                        Err(e) => return admin_error(&e),
                    }
                }
                Ok(out)
            }
        },
        Err(response) => return *response,
    };
    let teams = match rows {
        Ok(teams) => teams,
        Err(e) => return admin_error(&e),
    };
    let mut data = Vec::with_capacity(teams.len());
    for team in &teams {
        match view(&state, team).await {
            Ok(row) => data.push(row),
            Err(e) => return admin_error(&e),
        }
    }
    ok_json(json!({ "data": data }))
}

/// `POST /api/teams` — `{name, budget_tokens?, budget_micros?, budget_window?}`.
pub async fn create(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let value = match body_json(body).await {
        Ok(v) => v,
        Err(resp) => return *resp,
    };
    let Some(name) = value.get("name").and_then(Value::as_str) else {
        return admin_error(&Error::BadRequest("a team name is required".into()));
    };
    let mut budget_tokens = 0;
    let mut budget_micros = 0;
    let mut window = BudgetWindow::Total;
    if let Err(e) = merge_budget(&value, &mut budget_tokens, &mut budget_micros, &mut window) {
        return admin_error(&e);
    }
    match state
        .store
        .create_team(name, budget_tokens, budget_micros, window)
        .await
    {
        Ok(team) => {
            let actor = super::auth::actor(&state, &headers).await;
            super::auth::audit(
                &state,
                "team_created",
                Some(&actor),
                &crate::auth::client_ip(&headers, None),
                &format!("team {} · name {}", team.id, team.name),
            )
            .await;
            match view(&state, &team).await {
                Ok(row) => {
                    (StatusCode::CREATED, axum::Json(json!({ "data": row }))).into_response()
                }
                Err(e) => admin_error(&e),
            }
        }
        Err(e) => admin_error(&e),
    }
}

/// Read the three budget fields out of a create/patch body.
fn merge_budget(
    value: &Value,
    tokens: &mut u64,
    micros: &mut u64,
    window: &mut BudgetWindow,
) -> Result<(), Error> {
    for (field, slot) in [
        ("budget_tokens", &mut *tokens),
        ("budget_micros", &mut *micros),
    ] {
        let Some(raw) = value.get(field).filter(|v| !v.is_null()) else {
            continue;
        };
        let Some(number) = raw.as_u64() else {
            return Err(Error::BadRequest(format!(
                "{field} must be a non-negative integer"
            )));
        };
        *slot = number;
    }
    if let Some(raw) = value.get("budget_window").filter(|v| !v.is_null()) {
        let Some(parsed) = raw.as_str().and_then(BudgetWindow::parse) else {
            return Err(Error::BadRequest(
                "budget_window must be total, daily, weekly or monthly".into(),
            ));
        };
        *window = parsed;
    }
    Ok(())
}

/// `GET /api/teams/{id}` — the team, its members and its spend.
pub async fn get(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let Ok(id) = Uuid::parse_str(&id) else {
        return admin_error(&Error::NotFound("team".into()));
    };
    let allowed = match scoped_teams(&state, &headers, Some(id)).await {
        Ok(scope) => scope.as_deref().is_none_or(|ids| ids.contains(&id)),
        Err(response) => return *response,
    };
    if !allowed {
        return admin_error(&Error::NotFound(format!("team {id}")));
    }
    let team = match state.store.team(id).await {
        Ok(Some(team)) => team,
        Ok(None) => return admin_error(&Error::NotFound(format!("team {id}"))),
        Err(e) => return admin_error(&e),
    };
    let members = match members_json(&state, id).await {
        Ok(members) => members,
        Err(e) => return admin_error(&e),
    };
    match view(&state, &team).await {
        Ok(row) => {
            let mut row = row;
            row["members_list"] = Value::Array(members);
            ok_json(json!({ "data": row }))
        }
        Err(e) => admin_error(&e),
    }
}

/// `PATCH /api/teams/{id}` — any subset of the budget fields and `name`.
pub async fn update(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Body,
) -> Response {
    let Ok(id) = Uuid::parse_str(&id) else {
        return admin_error(&Error::NotFound("team".into()));
    };
    let value = match body_json(body).await {
        Ok(v) => v,
        Err(resp) => return *resp,
    };
    let Some(team) = (match state.store.team(id).await {
        Ok(team) => team,
        Err(e) => return admin_error(&e),
    }) else {
        return admin_error(&Error::NotFound(format!("team {id}")));
    };
    let mut tokens = team.budget_tokens;
    let mut micros = team.budget_micros;
    let mut window = team.budget_window;
    if let Err(e) = merge_budget(&value, &mut tokens, &mut micros, &mut window) {
        return admin_error(&e);
    }
    let patch = TeamPatch {
        name: value
            .get("name")
            .and_then(Value::as_str)
            .map(str::to_string),
        budget_tokens: Some(tokens),
        budget_micros: Some(micros),
        budget_window: Some(window),
    };
    match state.store.update_team(id, &patch).await {
        Ok(Some(team)) => {
            let actor = super::auth::actor(&state, &headers).await;
            super::auth::audit(
                &state,
                "team_updated",
                Some(&actor),
                &crate::auth::client_ip(&headers, None),
                &format!(
                    "team {} · name {} · budget_tokens {} · budget_micros {} · window {}",
                    team.id,
                    team.name,
                    team.budget_tokens,
                    team.budget_micros,
                    team.budget_window.as_str()
                ),
            )
            .await;
            match view(&state, &team).await {
                Ok(row) => ok_json(json!({ "data": row })),
                Err(e) => admin_error(&e),
            }
        }
        Ok(None) => admin_error(&Error::NotFound(format!("team {id}"))),
        Err(e) => admin_error(&e),
    }
}

/// `DELETE /api/teams/{id}` — memberships go, keys are released, usage history stays.
pub async fn remove(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let Ok(id) = Uuid::parse_str(&id) else {
        return admin_error(&Error::NotFound("team".into()));
    };
    match state.store.delete_team(id).await {
        Ok(true) => {
            let actor = super::auth::actor(&state, &headers).await;
            super::auth::audit(
                &state,
                "team_deleted",
                Some(&actor),
                &crate::auth::client_ip(&headers, None),
                &format!("team {id}"),
            )
            .await;
            ok_json(json!({ "deleted": true, "id": id.to_string() }))
        }
        Ok(false) => admin_error(&Error::NotFound(format!("team {id}"))),
        Err(e) => admin_error(&e),
    }
}

/// `GET /api/teams/{id}/members`
pub async fn members(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let Ok(id) = Uuid::parse_str(&id) else {
        return admin_error(&Error::NotFound("team".into()));
    };
    let allowed = match scoped_teams(&state, &headers, Some(id)).await {
        Ok(scope) => scope.as_deref().is_none_or(|ids| ids.contains(&id)),
        Err(response) => return *response,
    };
    if !allowed {
        return admin_error(&Error::NotFound(format!("team {id}")));
    }
    match state.store.team(id).await {
        Ok(Some(_)) => {}
        Ok(None) => return admin_error(&Error::NotFound(format!("team {id}"))),
        Err(e) => return admin_error(&e),
    }
    match members_json(&state, id).await {
        Ok(members) => ok_json(json!({ "data": members })),
        Err(e) => admin_error(&e),
    }
}

/// Roster rows, resolved against `users` so the dashboard never needs a second call.
async fn members_json(state: &AppState, team_id: Uuid) -> Result<Vec<Value>, Error> {
    let members = state.store.team_members(team_id).await?;
    let users = state.store.users().await?;
    Ok(members
        .iter()
        .map(|member| {
            let user = users.iter().find(|user| user.id == member.user_id);
            json!({
                "team_id": member.team_id.to_string(),
                "user_id": member.user_id.to_string(),
                "email": user.map(|u| u.email.clone()),
                "name": user.map(|u| u.name.clone()),
                "role": member.role.as_str(),
                "created_at": member.created_at,
            })
        })
        .collect())
}

/// `POST /api/teams/{id}/members` — `{user_id | email, role}`. Adding an existing member
/// changes their role, so the same call is add and edit.
pub async fn add_member(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Body,
) -> Response {
    let Ok(team_id) = Uuid::parse_str(&id) else {
        return admin_error(&Error::NotFound("team".into()));
    };
    let value = match body_json(body).await {
        Ok(v) => v,
        Err(resp) => return *resp,
    };
    let Some(role) = value
        .get("role")
        .and_then(Value::as_str)
        .and_then(TeamRole::parse)
    else {
        return admin_error(&Error::BadRequest(
            "role must be team_admin, member or viewer".into(),
        ));
    };
    let user_id = match value.get("user_id").and_then(Value::as_str) {
        Some(raw) => match Uuid::parse_str(raw) {
            Ok(id) => Some(id),
            Err(_) => return admin_error(&Error::BadRequest("user_id must be a uuid".into())),
        },
        None => match value.get("email").and_then(Value::as_str) {
            Some(email) => match state.store.user_by_email(email).await {
                Ok(Some(user)) => Some(user.id),
                Ok(None) => return admin_error(&Error::NotFound(format!("user {email}"))),
                Err(e) => return admin_error(&e),
            },
            None => return admin_error(&Error::BadRequest("user_id or email is required".into())),
        },
    };
    let Some(user_id) = user_id else {
        return admin_error(&Error::BadRequest("user_id or email is required".into()));
    };
    // The gate proved the caller may manage this team; this is the one personal rule.
    let caller = match team_authority(&state, &headers, team_id).await {
        Ok(caller) => caller,
        Err(response) => return *response,
    };
    if !caller.may_manage(user_id) {
        return admin_error(&caller.cannot_manage_self());
    }
    match state.store.add_team_member(team_id, user_id, role).await {
        Ok(member) => {
            let actor = super::auth::actor(&state, &headers).await;
            super::auth::audit(
                &state,
                "team_member_added",
                Some(&actor),
                &crate::auth::client_ip(&headers, None),
                &format!(
                    "team {team_id} · user {user_id} · role {}",
                    member.role.as_str()
                ),
            )
            .await;
            (
                StatusCode::CREATED,
                axum::Json(json!({
                    "data": {
                        "team_id": member.team_id.to_string(),
                        "user_id": member.user_id.to_string(),
                        "role": member.role.as_str(),
                        "created_at": member.created_at,
                    }
                })),
            )
                .into_response()
        }
        Err(e) => admin_error(&e),
    }
}

/// `DELETE /api/teams/{id}/members/{user_id}`
pub async fn remove_member(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((id, user_id)): Path<(String, String)>,
) -> Response {
    let (Ok(team_id), Ok(user_id)) = (Uuid::parse_str(&id), Uuid::parse_str(&user_id)) else {
        return admin_error(&Error::NotFound("member".into()));
    };
    let caller = match team_authority(&state, &headers, team_id).await {
        Ok(caller) => caller,
        Err(response) => return *response,
    };
    if !caller.may_manage(user_id) {
        return admin_error(&caller.cannot_manage_self());
    }
    match state.store.remove_team_member(team_id, user_id).await {
        Ok(true) => {
            let actor = super::auth::actor(&state, &headers).await;
            super::auth::audit(
                &state,
                "team_member_removed",
                Some(&actor),
                &crate::auth::client_ip(&headers, None),
                &format!("team {team_id} · user {user_id}"),
            )
            .await;
            ok_json(json!({ "removed": true }))
        }
        Ok(false) => admin_error(&Error::NotFound(format!("team {team_id} member {user_id}"))),
        Err(e) => admin_error(&e),
    }
}
