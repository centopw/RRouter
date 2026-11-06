//! `Store`: the single embedded SQLite database behind the gateway.

use std::path::{Path, PathBuf};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection as Sql, OptionalExtension, Row};
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
    pub is_active: bool,
    pub credentials: Credentials,
    pub backoff_level: u32,
    pub last_error: Option<String>,
    pub last_error_code: Option<u16>,
    pub last_error_at: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
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
    pub created_at: i64,
    /// Requests per minute allowed; 0 = unlimited.
    pub rpm: u32,
    /// Tokens per minute allowed; 0 = unlimited. Measured post-hoc from the tokens
    /// recorded for this key in the last 60 s — a single request can overshoot it.
    pub tpm: u32,
    /// Lifetime token budget; 0 = unlimited. Spend is the durable `usage` sum.
    pub budget_tokens: u64,
    /// Exact model refs or `provider/*` globs; `None` = every model.
    pub allowed_models: Option<Vec<String>>,
    /// Unix seconds after which the key is refused; `None` = never.
    pub expires_at: Option<i64>,
}

/// A partial update of an [`ApiKey`]. Absent = leave alone; `Some(None)` = clear a
/// nullable column. Mirrors the `PATCH /api/keys/{id}` body.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ApiKeyPatch {
    pub is_active: Option<bool>,
    pub rpm: Option<u32>,
    pub tpm: Option<u32>,
    pub budget_tokens: Option<u64>,
    /// `Some(None)` clears the allowlist (back to every model).
    pub allowed_models: Option<Option<Vec<String>>>,
    /// `Some(None)` clears the expiry (back to never).
    pub expires_at: Option<Option<i64>>,
}

impl ApiKeyPatch {
    /// Apply this patch to `key` in place. Pure, so the merge rule is testable on its
    /// own and the SQL only ever writes the merged row.
    pub fn apply(&self, key: &mut ApiKey) {
        if let Some(v) = self.is_active {
            key.is_active = v;
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
        if let Some(v) = &self.allowed_models {
            key.allowed_models = v.clone();
        }
        if let Some(v) = self.expires_at {
            key.expires_at = v;
        }
    }
}

/// Model alias -> `"provider/model"` target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelAlias {
    pub alias: String,
    pub target: String,
}

/// Ordered fallback chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Combo {
    pub id: Uuid,
    pub name: String,
    pub models: Vec<String>,
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

/// One completed request. Never contains prompt or response bodies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageRecord {
    /// Row id, set when read back; ignored on write.
    pub id: Option<i64>,
    pub ts: i64,
    pub provider: String,
    pub model: String,
    pub connection_id: Option<Uuid>,
    /// Client endpoint that carried the request, e.g. `/v1/messages`.
    pub endpoint: Option<String>,
    /// Gateway key that paid for the request; `None` for anonymous traffic.
    pub key_id: Option<Uuid>,
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
         expires_at";

    /// Generate `sk-rr-<32 hex>`, persist `SHA-256(plaintext)` only, return both.
    ///
    /// A new key is unlimited: no caps, no allowlist, no expiry.
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
                created_at,
                rpm: 0,
                tpm: 0,
                budget_tokens: 0,
                allowed_models: None,
                expires_at: None,
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
        patch.apply(&mut key);
        let allowed = match &key.allowed_models {
            Some(models) => Some(serde_json::to_string(models)?),
            None => None,
        };
        conn.execute(
            "UPDATE api_keys SET is_active = ?2, rpm = ?3, tpm = ?4, budget_tokens = ?5, \
             allowed_models = ?6, expires_at = ?7 WHERE id = ?1",
            params![
                id.to_string(),
                key.is_active as i64,
                key.rpm,
                key.tpm,
                key.budget_tokens,
                allowed,
                key.expires_at,
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
        Ok(n > 0)
    }

    /// Lifetime tokens this key has spent: input + output + cache reads + cache writes,
    /// summed in SQL so a budget check never has to read events into Rust.
    pub async fn api_key_spend(&self, id: Uuid) -> Result<u64> {
        let conn = self.conn();
        let total: i64 = conn
            .query_row(
                "SELECT COALESCE(sum(input_tokens + output_tokens + cache_read_tokens + \
                 cache_write_tokens), 0) FROM usage WHERE key_id = ?1",
                params![id.to_string()],
                |r| r.get(0),
            )
            .map_err(migrations::db_err)?;
        Ok(total.max(0) as u64)
    }

    /// Spend for every key in one statement, for `GET /api/keys`.
    pub async fn api_key_spend_all(&self) -> Result<std::collections::HashMap<Uuid, u64>> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(
                "SELECT key_id, sum(input_tokens + output_tokens + cache_read_tokens + \
                 cache_write_tokens) FROM usage WHERE key_id IS NOT NULL GROUP BY key_id",
            )
            .map_err(migrations::db_err)?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
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

    // ----- usage -------------------------------------------------------------------

    /// Append one usage row. Only counters and error text are stored, never payloads.
    pub async fn record_usage(&self, r: &UsageRecord) -> Result<()> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO usage \
             (ts, provider, model, connection_id, endpoint, key_id, input_tokens, output_tokens, \
              cache_read_tokens, cache_write_tokens, latency_ms, ttft_ms, status, streamed, error) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            params![
                r.ts,
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
            ],
        )
        .map_err(migrations::db_err)?;
        Ok(())
    }

    /// Most recent records first.
    pub async fn recent_usage(&self, limit: usize, since: Option<i64>) -> Result<Vec<UsageRecord>> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let conn = self.conn();
        let mut stmt = conn
            .prepare(
                "SELECT id, ts, provider, model, connection_id, endpoint, input_tokens, output_tokens, \
                        cache_read_tokens, cache_write_tokens, latency_ms, ttft_ms, status, streamed, error, \
                        key_id \
                 FROM usage WHERE ?2 IS NULL OR ts >= ?2 \
                 ORDER BY ts DESC, id DESC LIMIT ?1",
            )
            .map_err(migrations::db_err)?;
        let rows = stmt
            .query_map(params![limit, since], |r| {
                let cid: Option<String> = r.get(4)?;
                let kid: Option<String> = r.get(15)?;
                Ok(UsageRecord {
                    id: r.get(0)?,
                    ts: r.get(1)?,
                    provider: r.get(2)?,
                    model: r.get(3)?,
                    connection_id: cid.as_deref().and_then(|s| Uuid::parse_str(s).ok()),
                    endpoint: r.get(5)?,
                    key_id: kid.as_deref().and_then(|s| Uuid::parse_str(s).ok()),
                    input_tokens: r.get(6)?,
                    output_tokens: r.get(7)?,
                    cache_read_tokens: r.get(8)?,
                    cache_write_tokens: r.get(9)?,
                    latency_ms: r.get(10)?,
                    ttft_ms: r.get(11)?,
                    status: r.get(12)?,
                    streamed: r.get::<_, i64>(13)? != 0,
                    error: r.get(14)?,
                })
            })
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
                        avg(latency_ms) \
                 FROM usage WHERE ?1 IS NULL OR ts >= ?1 \
                 GROUP BY provider, model ORDER BY provider ASC, model ASC",
            )
            .map_err(migrations::db_err)?;
        let rows = stmt
            .query_map(params![since], |r| {
                let avg: f64 = r.get(8)?;
                Ok(UsageSummary {
                    provider: r.get(0)?,
                    model: r.get(1)?,
                    requests: r.get(2)?,
                    errors: r.get(3)?,
                    input_tokens: r.get(4)?,
                    output_tokens: r.get(5)?,
                    cache_read_tokens: r.get(6)?,
                    cache_write_tokens: r.get(7)?,
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
                        avg(latency_ms) \
                 FROM usage WHERE ts >= ?1 GROUP BY bucket ORDER BY bucket ASC",
            )
            .map_err(migrations::db_err)?;
        let rows = stmt
            .query_map(params![since, bucket_secs], |r| {
                let avg: f64 = r.get(7)?;
                Ok(UsageBucket {
                    start: r.get(0)?,
                    requests: r.get(1)?,
                    errors: r.get(2)?,
                    input_tokens: r.get(3)?,
                    output_tokens: r.get(4)?,
                    cache_read_tokens: r.get(5)?,
                    cache_write_tokens: r.get(6)?,
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
                        avg(latency_ms) \
                 FROM usage WHERE ts >= ?1 \
                 GROUP BY bucket, provider, model ORDER BY bucket ASC, provider ASC, model ASC",
            )
            .map_err(migrations::db_err)?;
        let rows = stmt
            .query_map(params![since, bucket_secs], |r| {
                let avg: f64 = r.get(9)?;
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
                            created_at, updated_at";

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
}

fn uuid_col(s: String) -> rusqlite::Result<Uuid> {
    Uuid::parse_str(&s).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
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

/// Capability blob as a JSON object. Only this crate writes the column, so a malformed
/// value means a hand-edited database; treat it as "no overrides" rather than failing
/// the whole registry listing.
fn parse_caps(raw: String) -> serde_json::Value {
    match serde_json::from_str::<serde_json::Value>(&raw) {
        Ok(v @ serde_json::Value::Object(_)) => v,
        _ => serde_json::Value::Object(serde_json::Map::new()),
    }
}
