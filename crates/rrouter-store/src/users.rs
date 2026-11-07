//! Dashboard users: identity, sessions, recovery codes and invitations.
//!
//! This is deliberately separate from gateway API keys. `/v1` traffic authenticates with
//! keys; the dashboard and the admin API authenticate a *person*. The admin token and
//! admin keys keep working for automation and break-glass access.
//!
//! Password hashes are argon2id (random salt, PHC string). Session cookies, recovery
//! codes and invitation tokens are stored as SHA-256 hex only: a copy of the database
//! cannot be replayed as a live session or an unused code.

use std::time::{SystemTime, UNIX_EPOCH};

use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use rand::RngCore;
use rusqlite::{params, OptionalExtension, Row};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use rrouter_core::{Error, Result};

use crate::migrations;
use crate::store::Store;

/// What a dashboard user may do. Enforced by the gateway's admin gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserRole {
    Admin,
    Member,
    Viewer,
}

impl UserRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Admin => "admin",
            Self::Member => "member",
            Self::Viewer => "viewer",
        }
    }

    /// Strict parse for client input.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "admin" => Some(Self::Admin),
            "member" => Some(Self::Member),
            "viewer" => Some(Self::Viewer),
            _ => None,
        }
    }

    /// Lenient read for stored values; anything unknown is the least privileged role.
    pub fn from_stored(raw: &str) -> Self {
        Self::parse(raw).unwrap_or(Self::Viewer)
    }
}

/// One dashboard account. `password_hash` stays in the table and is never part of this
/// type, so a handler cannot leak it by serialising a user by accident.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct User {
    pub id: Uuid,
    pub name: String,
    /// Always lowercased; the unique index is the case-insensitive rule.
    pub email: String,
    pub role: UserRole,
    pub is_owner: bool,
    pub disabled: bool,
    pub created_at: i64,
    pub last_sign_in_at: Option<i64>,
}

impl User {
    pub fn is_admin(&self) -> bool {
        self.role == UserRole::Admin
    }
}

/// One live session row. The hash never leaves the gateway.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionInfo {
    pub token_hash: String,
    pub created_at: i64,
    pub expires_at: i64,
}

/// A pending (or consumed) invitation. `token_hash` is not exposed back to the caller
/// after creation except from the endpoint that issues the link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invite {
    pub id: Uuid,
    pub email: String,
    pub role: UserRole,
    pub created_by: Uuid,
    pub created_at: i64,
    pub expires_at: i64,
    pub used_at: Option<i64>,
}

/// Lowercased, trimmed email — the only spelling this module writes.
pub fn normalize_email(raw: &str) -> String {
    raw.trim().to_lowercase()
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// argon2id, random salt, PHC string.
pub fn hash_password(password: &str) -> Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|e| Error::Store(format!("password hash: {e}")))
}

/// Constant-time argon2 verification; a malformed stored hash is simply "no match".
pub fn verify_password(password: &str, stored: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(stored) else {
        return false;
    };
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok()
}

/// SHA-256 hex of a bearer secret (session token, recovery code, invitation token).
pub fn hash_secret(secret: &str) -> String {
    let digest = Sha256::digest(secret.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// 32 random bytes, base64url without padding: session cookies and invitation tokens.
pub fn random_token() -> String {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// One setup code: `ABCD-EFGH`, printed once and required to create the first user.
pub fn random_setup_code() -> String {
    let mut bytes = [0u8; 4];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    let hex: String = bytes.iter().map(|b| format!("{b:02X}")).collect();
    format!("{}-{}", &hex[..4], &hex[4..])
}

/// Ten single-use recovery codes in the `ABCDE-FGHIJ` shape the design shows.
pub fn random_recovery_codes() -> Vec<String> {
    (0..10)
        .map(|_| {
            let mut bytes = [0u8; 5];
            rand::rngs::OsRng.fill_bytes(&mut bytes);
            let hex: String = bytes.iter().map(|b| format!("{b:02X}")).collect();
            format!("{}-{}", &hex[..5], &hex[5..])
        })
        .collect()
}

fn row_to_user(r: &Row<'_>) -> rusqlite::Result<User> {
    Ok(User {
        id: Uuid::parse_str(&r.get::<_, String>(0)?).unwrap_or_default(),
        name: r.get(1)?,
        email: r.get(2)?,
        role: UserRole::from_stored(&r.get::<_, String>(3)?),
        is_owner: r.get::<_, i64>(4)? != 0,
        disabled: r.get::<_, i64>(5)? != 0,
        created_at: r.get(6)?,
        last_sign_in_at: r.get(7)?,
    })
}

fn row_to_invite(r: &Row<'_>) -> rusqlite::Result<Invite> {
    Ok(Invite {
        id: Uuid::parse_str(&r.get::<_, String>(0)?).unwrap_or_default(),
        email: r.get(1)?,
        role: UserRole::from_stored(&r.get::<_, String>(2)?),
        created_by: Uuid::parse_str(&r.get::<_, String>(3)?).unwrap_or_default(),
        created_at: r.get(4)?,
        expires_at: r.get(5)?,
        used_at: r.get(6)?,
    })
}

const USER_COLUMNS: &str =
    "id, name, email, role, is_owner, disabled, created_at, last_sign_in_at";

impl Store {
    // ----- users ---------------------------------------------------------------------

    /// Whether any user exists. `false` means the instance still needs its owner.
    pub async fn users_exist(&self) -> Result<bool> {
        let count: i64 = self
            .conn()
            .query_row("SELECT count(*) FROM users", [], |r| r.get(0))
            .map_err(migrations::db_err)?;
        Ok(count > 0)
    }

    pub async fn users(&self) -> Result<Vec<User>> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {USER_COLUMNS} FROM users ORDER BY is_owner DESC, created_at"
            ))
            .map_err(migrations::db_err)?;
        let rows = stmt.query_map([], row_to_user).map_err(migrations::db_err)?;
        rows.map(|r| r.map_err(migrations::db_err)).collect()
    }

    pub async fn user(&self, id: Uuid) -> Result<Option<User>> {
        self.conn()
            .query_row(
                &format!("SELECT {USER_COLUMNS} FROM users WHERE id = ?1"),
                params![id.to_string()],
                row_to_user,
            )
            .optional()
            .map_err(migrations::db_err)
    }

    pub async fn user_by_email(&self, email: &str) -> Result<Option<User>> {
        self.conn()
            .query_row(
                &format!("SELECT {USER_COLUMNS} FROM users WHERE email = ?1"),
                params![normalize_email(email)],
                row_to_user,
            )
            .optional()
            .map_err(migrations::db_err)
    }

    /// Create a user. The email is normalised here; a duplicate is a `400` naming the
    /// field rather than a raw constraint message.
    pub async fn create_user(
        &self,
        name: &str,
        email: &str,
        password_hash: &str,
        role: UserRole,
        is_owner: bool,
    ) -> Result<User> {
        let email = normalize_email(email);
        if self.user_by_email(&email).await?.is_some() {
            return Err(Error::BadRequest(
                "that email already has an account".into(),
            ));
        }
        let id = Uuid::new_v4();
        self.conn()
            .execute(
                "INSERT INTO users (id, name, email, password_hash, role, is_owner, \
                 disabled, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, ?7)",
                params![
                    id.to_string(),
                    name.trim(),
                    email,
                    password_hash,
                    role.as_str(),
                    if is_owner { 1 } else { 0 },
                    now(),
                ],
            )
            .map_err(migrations::db_err)?;
        self.user(id)
            .await?
            .ok_or_else(|| Error::Store("user vanished after insert".into()))
    }

    /// Create the owner only when no user exists yet. One statement, so two concurrent
    /// setup requests cannot both win; `None` means someone else was first.
    pub async fn create_first_user(
        &self,
        name: &str,
        email: &str,
        password_hash: &str,
    ) -> Result<Option<User>> {
        let email = normalize_email(email);
        let id = Uuid::new_v4();
        let inserted = self
            .conn()
            .execute(
                "INSERT INTO users (id, name, email, password_hash, role, is_owner, disabled, \
                 created_at) SELECT ?1, ?2, ?3, ?4, 'admin', 1, 0, ?5 \
                 WHERE NOT EXISTS (SELECT 1 FROM users)",
                params![id.to_string(), name.trim(), email, password_hash, now()],
            )
            .map_err(migrations::db_err)?;
        if inserted == 0 {
            return Ok(None);
        }
        self.user(id).await
    }

    /// Role and disabled flag, with the owner invariant enforced here so no caller can
    /// demote or disable the owner by forgetting the check.
    pub async fn update_user(
        &self,
        id: Uuid,
        role: Option<UserRole>,
        disabled: Option<bool>,
    ) -> Result<Option<User>> {
        let Some(existing) = self.user(id).await? else {
            return Ok(None);
        };
        if existing.is_owner && (role.is_some_and(|r| r != UserRole::Admin) || disabled == Some(true))
        {
            return Err(Error::Forbidden {
                code: "owner_protected",
                message: "the owner account cannot be demoted or disabled".into(),
            });
        }
        self.conn()
            .execute(
                "UPDATE users SET role = coalesce(?2, role), \
                 disabled = coalesce(?3, disabled) WHERE id = ?1",
                params![
                    id.to_string(),
                    role.map(UserRole::as_str),
                    disabled.map(|d| if d { 1 } else { 0 }),
                ],
            )
            .map_err(migrations::db_err)?;
        self.user(id).await
    }

    /// Delete a user and everything that authenticates them. The owner is refused.
    pub async fn delete_user(&self, id: Uuid) -> Result<bool> {
        let Some(existing) = self.user(id).await? else {
            return Ok(false);
        };
        if existing.is_owner {
            return Err(Error::Forbidden {
                code: "owner_protected",
                message: "the owner account cannot be deleted".into(),
            });
        }
        let mut conn = self.conn();
        let tx = conn.transaction().map_err(migrations::db_err)?;
        for statement in [
            "DELETE FROM user_sessions WHERE user_id = ?1",
            "DELETE FROM user_recovery_codes WHERE user_id = ?1",
            "DELETE FROM invites WHERE created_by = ?1",
            "DELETE FROM users WHERE id = ?1",
        ] {
            tx.execute(statement, params![id.to_string()])
                .map_err(migrations::db_err)?;
        }
        tx.commit().map_err(migrations::db_err)?;
        Ok(true)
    }

    pub async fn set_user_password(&self, id: Uuid, password_hash: &str) -> Result<()> {
        let changed = self
            .conn()
            .execute(
                "UPDATE users SET password_hash = ?2 WHERE id = ?1",
                params![id.to_string(), password_hash],
            )
            .map_err(migrations::db_err)?;
        if changed == 0 {
            return Err(Error::NotFound(format!("user {id}")));
        }
        // A password change ends every other session of that user.
        self.delete_user_sessions(id).await
    }

    /// The argon2 PHC string. Only sign-in needs it, which is why it is not on `User`.
    pub async fn password_hash(&self, id: Uuid) -> Result<Option<String>> {
        self.conn()
            .query_row(
                "SELECT password_hash FROM users WHERE id = ?1",
                params![id.to_string()],
                |r| r.get(0),
            )
            .optional()
            .map_err(migrations::db_err)
    }

    /// Display-name change. Email is the identity and is not editable in v1.
    pub async fn rename_user(&self, id: Uuid, name: &str) -> Result<Option<User>> {
        let name = name.trim();
        if name.is_empty() || name.chars().count() > 100 {
            return Err(Error::BadRequest("name must be 1–100 characters".into()));
        }
        self.conn()
            .execute(
                "UPDATE users SET name = ?2 WHERE id = ?1",
                params![id.to_string(), name],
            )
            .map_err(migrations::db_err)?;
        self.user(id).await
    }

    pub async fn delete_sessions_except(&self, user_id: Uuid, keep: &str) -> Result<usize> {
        self.conn()
            .execute(
                "DELETE FROM user_sessions WHERE user_id = ?1 AND token_hash <> ?2",
                params![user_id.to_string(), keep],
            )
            .map_err(migrations::db_err)
    }

    pub async fn mark_signed_in(&self, id: Uuid) -> Result<()> {
        self.conn()
            .execute(
                "UPDATE users SET last_sign_in_at = ?2 WHERE id = ?1",
                params![id.to_string(), now()],
            )
            .map_err(migrations::db_err)?;
        Ok(())
    }

    // ----- recovery codes ------------------------------------------------------------

    pub async fn add_recovery_codes(&self, user_id: Uuid, hashes: &[String]) -> Result<()> {
        let conn = self.conn();
        for hash in hashes {
            conn.execute(
                "INSERT INTO user_recovery_codes (user_id, code_hash) VALUES (?1, ?2)",
                params![user_id.to_string(), hash],
            )
            .map_err(migrations::db_err)?;
        }
        Ok(())
    }

    /// Mark a code used and return its owner; `None` means unknown or already used.
    pub async fn consume_recovery_code(&self, code_hash: &str, at: i64) -> Result<Option<Uuid>> {
        let owner: Option<String> = self
            .conn()
            .query_row(
                "UPDATE user_recovery_codes SET used_at = ?2 \
                 WHERE code_hash = ?1 AND used_at IS NULL RETURNING user_id",
                params![code_hash, at],
                |r| r.get(0),
            )
            .optional()
            .map_err(migrations::db_err)?;
        Ok(owner.and_then(|raw| Uuid::parse_str(&raw).ok()))
    }

    // ----- sessions ------------------------------------------------------------------

    pub async fn create_session(
        &self,
        token_hash: &str,
        user_id: Uuid,
        expires_at: i64,
    ) -> Result<()> {
        self.conn()
            .execute(
                "INSERT INTO user_sessions (token_hash, user_id, created_at, expires_at) \
                 VALUES (?1, ?2, ?3, ?4)",
                params![token_hash, user_id.to_string(), now(), expires_at],
            )
            .map_err(migrations::db_err)?;
        Ok(())
    }

    /// The live session's user. Expired or disabled users authenticate as nobody.
    pub async fn session_user(&self, token_hash: &str, at: i64) -> Result<Option<User>> {
        self.conn()
            .query_row(
                "SELECT u.id, u.name, u.email, u.role, u.is_owner, u.disabled, \
                 u.created_at, u.last_sign_in_at FROM user_sessions s \
                 JOIN users u ON u.id = s.user_id \
                 WHERE s.token_hash = ?1 AND s.expires_at > ?2 AND u.disabled = 0",
                params![token_hash, at],
                row_to_user,
            )
            .optional()
            .map_err(migrations::db_err)
    }

    /// Every live session of one user, newest first. The token hash stays inside the
    /// gateway: callers derive a public id from it, never expose it.
    pub async fn sessions_for_user(&self, user_id: Uuid) -> Result<Vec<SessionInfo>> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(
                "SELECT token_hash, created_at, expires_at FROM user_sessions \
                 WHERE user_id = ?1 AND expires_at > ?2 ORDER BY created_at DESC",
            )
            .map_err(migrations::db_err)?;
        let now = now();
        let rows = stmt
            .query_map(params![user_id.to_string(), now], |r| {
                Ok(SessionInfo {
                    token_hash: r.get(0)?,
                    created_at: r.get(1)?,
                    expires_at: r.get(2)?,
                })
            })
            .map_err(migrations::db_err)?;
        rows.map(|row| row.map_err(migrations::db_err)).collect()
    }

    /// Revoke one session, scoped to its owner so a leaked public id cannot end someone
    /// else's session.
    pub async fn delete_user_session(&self, user_id: Uuid, token_hash: &str) -> Result<bool> {
        let changed = self
            .conn()
            .execute(
                "DELETE FROM user_sessions WHERE user_id = ?1 AND token_hash = ?2",
                params![user_id.to_string(), token_hash],
            )
            .map_err(migrations::db_err)?;
        Ok(changed == 1)
    }

    pub async fn delete_session(&self, token_hash: &str) -> Result<()> {
        self.conn()
            .execute(
                "DELETE FROM user_sessions WHERE token_hash = ?1",
                params![token_hash],
            )
            .map_err(migrations::db_err)?;
        Ok(())
    }

    pub async fn delete_user_sessions(&self, user_id: Uuid) -> Result<()> {
        self.conn()
            .execute(
                "DELETE FROM user_sessions WHERE user_id = ?1",
                params![user_id.to_string()],
            )
            .map_err(migrations::db_err)?;
        Ok(())
    }

    /// Expired sessions are dead weight; the retention sweep calls this.
    pub async fn prune_sessions(&self, at: i64) -> Result<usize> {
        self.conn()
            .execute("DELETE FROM user_sessions WHERE expires_at <= ?1", params![at])
            .map_err(migrations::db_err)
    }

    // ----- invitations ---------------------------------------------------------------

    pub async fn create_invite(
        &self,
        email: &str,
        role: UserRole,
        token_hash: &str,
        created_by: Uuid,
        expires_at: i64,
    ) -> Result<Invite> {
        let id = Uuid::new_v4();
        self.conn()
            .execute(
                "INSERT INTO invites (id, email, role, token_hash, created_by, created_at, \
                 expires_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    id.to_string(),
                    normalize_email(email),
                    role.as_str(),
                    token_hash,
                    created_by.to_string(),
                    now(),
                    expires_at,
                ],
            )
            .map_err(migrations::db_err)?;
        self.invite(id)
            .await?
            .ok_or_else(|| Error::Store("invite vanished after insert".into()))
    }

    pub async fn invite(&self, id: Uuid) -> Result<Option<Invite>> {
        self.conn()
            .query_row(
                "SELECT id, email, role, created_by, created_at, expires_at, used_at \
                 FROM invites WHERE id = ?1",
                params![id.to_string()],
                row_to_invite,
            )
            .optional()
            .map_err(migrations::db_err)
    }

    /// An invitation that may still be used: unused and not expired.
    pub async fn invite_by_token(&self, token_hash: &str, at: i64) -> Result<Option<Invite>> {
        self.conn()
            .query_row(
                "SELECT id, email, role, created_by, created_at, expires_at, used_at \
                 FROM invites WHERE token_hash = ?1 AND used_at IS NULL AND expires_at > ?2",
                params![token_hash, at],
                row_to_invite,
            )
            .optional()
            .map_err(migrations::db_err)
    }

    /// Single-use claim; `false` means it was already consumed (or never existed).
    pub async fn consume_invite(&self, id: Uuid, at: i64) -> Result<bool> {
        let changed = self
            .conn()
            .execute(
                "UPDATE invites SET used_at = ?2 WHERE id = ?1 AND used_at IS NULL",
                params![id.to_string(), at],
            )
            .map_err(migrations::db_err)?;
        Ok(changed == 1)
    }

    pub async fn delete_invite(&self, id: Uuid) -> Result<bool> {
        let changed = self
            .conn()
            .execute("DELETE FROM invites WHERE id = ?1", params![id.to_string()])
            .map_err(migrations::db_err)?;
        Ok(changed == 1)
    }

    pub async fn pending_invites(&self) -> Result<Vec<Invite>> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(
                "SELECT id, email, role, created_by, created_at, expires_at, used_at \
                 FROM invites ORDER BY created_at DESC",
            )
            .map_err(migrations::db_err)?;
        let rows = stmt.query_map([], row_to_invite).map_err(migrations::db_err)?;
        rows.map(|r| r.map_err(migrations::db_err)).collect()
    }
}
