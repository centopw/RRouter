//! Integration tests for the SQLite + AES-256-GCM store.
//!
//! Every test that exercises two writes opens a real file-backed database (`TempDir`), since
//! the plaintext-leak and key-file assertions read the raw DB bytes back off disk.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::LazyLock;

use rrouter_store::{
    cache_key, ApiKey, ApiKeyPatch, AttemptRecord, Credentials, KeyModel, ModelRow, PruneReport,
    Store, StatusFilter, UsageQuery, UsageRecord, API_KEY_PREFIX, KEY_FILE_NAME,
};
use tokio::runtime::Runtime;
use uuid::Uuid;

// ----- harness ---------------------------------------------------------------------

/// Minimal unique temp dir; `tempfile` is not a workspace dependency.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        static SEQ: AtomicU32 = AtomicU32::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("rrouter-store-{tag}-{}-{n}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("temp dir");
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn db(&self) -> PathBuf {
        self.0.join("rrouter.db")
    }

    /// Fresh store over the file DB, key file in the same dir.
    fn store(&self) -> Store {
        Store::open(&self.db(), None).expect("open store")
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

/// One multi-thread runtime for the whole test binary: `tokio::spawn` (test 3) needs a
/// worker pool, and tests must not build a runtime per call.
static RT: LazyLock<Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .expect("runtime")
});

fn rt() -> &'static Runtime {
    &RT
}

fn creds(token: &str) -> Credentials {
    Credentials {
        access_token: Some(token.to_string()),
        ..Credentials::default()
    }
}

// ----- 1. encryption ---------------------------------------------------------------

#[test]
fn encryption_roundtrip_distinct_ciphertexts_and_tamper_error() {
    let dir = TempDir::new("tamper");
    let store = dir.store();

    let c = rt()
        .block_on(store.create_connection("claude", "oauth", creds("tok-A"), None))
        .expect("create");
    let got = rt()
        .block_on(store.get_connection(c.id))
        .expect("get")
        .expect("row");
    assert_eq!(got.credentials.access_token.as_deref(), Some("tok-A"));

    let blob_a = read_blob(&dir.db(), c.id);
    // Rewrite the identical credentials through the merge path: new nonce, new ciphertext.
    rt().block_on(store.merge_credentials(c.id, creds("tok-A")))
        .expect("merge");
    let blob_b = read_blob(&dir.db(), c.id);
    assert_eq!(blob_a.len(), blob_b.len(), "same plaintext length");
    assert_ne!(blob_a, blob_b, "two encryptions of one value must differ");

    flip_last_byte(&dir.db(), c.id);
    let err = rt().block_on(store.get_connection(c.id));
    assert!(err.is_err(), "tampered blob must error, got {err:?}");
    assert!(rt().block_on(store.list_connections(None)).is_err());
}

// ----- 2. key file -----------------------------------------------------------------

#[test]
fn key_file_is_0600_and_reused_across_opens() {
    let dir = TempDir::new("keyfile");
    {
        let s = dir.store();
        rt().block_on(s.create_connection("openai", "apikey", creds("tok"), None))
            .expect("create");
    }
    let key_path = dir.path().join(KEY_FILE_NAME);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&key_path)
            .expect("stat key")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "key file must be created 0600");
    }
    let first = std::fs::read(&key_path).expect("read key");
    assert_eq!(first.len(), 32, "key file holds 32 raw bytes");

    {
        let s = dir.store();
        let n = rt().block_on(s.count_api_keys()).expect("count");
        assert_eq!(n, 0);
        rt().block_on(s.create_connection("openai", "apikey", creds("tok2"), None))
            .expect("create");
    }
    assert_eq!(std::fs::read(&key_path).expect("reread"), first, "key reused");

    // A wrong-length hex master key is refused with a clear store error.
    let bad = Store::open(&dir.db(), Some("abc123".to_string()));
    match bad {
        Err(e) => assert!(e.to_string().contains("master key"), "unexpected: {e}"),
        Ok(_) => panic!("short hex master key must be rejected"),
    }
    // A 64-char hex key bypasses the key file entirely.
    let hexkey = "11".repeat(32);
    let alt = TempDir::new("hexkey");
    Store::open(&alt.db(), Some(hexkey.clone())).expect("hex key");
    assert!(!alt.path().join(KEY_FILE_NAME).exists());
    let s = Store::open(&alt.db(), Some(hexkey)).expect("reopen");
    assert!(rt().block_on(s.list_connections(None)).is_ok());
}

// ----- 3. concurrent merge ---------------------------------------------------------

#[test]
fn concurrent_merges_do_not_lose_updates() {
    let dir = TempDir::new("merge");
    let store = dir.store();
    let c = rt()
        .block_on(store.create_connection("claude", "oauth", Credentials::default(), None))
        .expect("create");
    let id = c.id;
    let store = &store;

    // 20 tasks: 9 distinct scalar fields + 11 distinct `extra` keys, so every task patches
    // something no sibling touches. A lost read-merge-write drops exactly one of them.
    let expected_extra: Vec<String> = (9..20).map(|n| format!("field_{n}")).collect();
    rt().block_on(async {
        let setter = |i: usize, s: Store| async move {
            let mut patch = Credentials::default();
            match i {
                0 => patch.access_token = Some("AT".into()),
                1 => patch.refresh_token = Some("RT".into()),
                2 => patch.api_key = Some("AK".into()),
                3 => patch.id_token = Some("ID".into()),
                4 => patch.expires_at = Some(1_700_000_000),
                5 => patch.copilot_token = Some("CP".into()),
                6 => patch.copilot_expires_at = Some(1_700_000_100),
                7 => patch.account_label = Some("a@b.c".into()),
                8 => patch.base_url_override = Some("http://127.0.0.1:11434".into()),
                n => {
                    patch
                        .extra
                        .insert(format!("field_{n}"), serde_json::json!(n));
                }
            }
            s.merge_credentials(id, patch).await
        };

        let mut handles = Vec::with_capacity(20);
        for i in 0..20 {
            let s = store.clone();
            handles.push(tokio::spawn(setter(i, s)));
        }
        for h in handles {
            h.await.expect("task join").expect("merge");
        }
    });

    let final_row = rt()
        .block_on(store.get_connection(id))
        .expect("get")
        .expect("row");
    let k = &final_row.credentials;
    assert_eq!(k.access_token.as_deref(), Some("AT"));
    assert_eq!(k.refresh_token.as_deref(), Some("RT"));
    assert_eq!(k.api_key.as_deref(), Some("AK"));
    assert_eq!(k.id_token.as_deref(), Some("ID"));
    assert_eq!(k.expires_at, Some(1_700_000_000));
    assert_eq!(k.copilot_token.as_deref(), Some("CP"));
    assert_eq!(k.copilot_expires_at, Some(1_700_000_100));
    assert_eq!(k.account_label.as_deref(), Some("a@b.c"));
    assert_eq!(k.base_url_override.as_deref(), Some("http://127.0.0.1:11434"));
    assert_eq!(k.extra.len(), expected_extra.len(), "extra: {:?}", k.extra);
    for field in &expected_extra {
        assert!(k.extra.contains_key(field), "lost update on {field}");
    }
    // `account_label` also denormalises into the email column.
    assert_eq!(final_row.email.as_deref(), Some("a@b.c"));
}

// ----- 4. API keys -----------------------------------------------------------------

#[test]
fn api_key_lifecycle_and_no_plaintext_in_db_file() {
    let dir = TempDir::new("apikey");
    let store = dir.store();

    let (plain, key) = rt()
        .block_on(store.create_api_key(Some("cli")))
        .expect("create key");
    assert!(plain.starts_with(API_KEY_PREFIX), "prefix: {plain}");
    assert_eq!(plain.len(), API_KEY_PREFIX.len() + 32);
    assert!(plain[API_KEY_PREFIX.len()..]
        .bytes()
        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
    assert_eq!(key.key_hash.len(), 64, "SHA-256 hex is 64 chars");
    assert_eq!(key.name.as_deref(), Some("cli"));

    let hit = rt().block_on(store.verify_api_key(&plain)).expect("verify");
    assert_eq!(hit.as_ref().map(|k| k.id), Some(key.id));
    assert_eq!(
        rt().block_on(store.verify_api_key("sk-rr-00000000000000000000000000000000"))
            .expect("verify miss"),
        None
    );
    assert_eq!(rt().block_on(store.verify_api_key("")).expect("empty"), None);

    // Second key differs; list is ordered by creation.
    let (plain2, key2) = rt().block_on(store.create_api_key(None)).expect("create");
    assert_ne!(plain, plain2);
    let listed: Vec<ApiKey> = rt().block_on(store.list_api_keys()).expect("list");
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[1].id, key2.id);
    assert_eq!(rt().block_on(store.count_api_keys()).expect("count"), 2);

    // Deleted keys stop verifying.
    assert!(rt().block_on(store.delete_api_key(key.id)).expect("delete"));
    assert!(!rt().block_on(store.delete_api_key(key.id)).expect("re-delete"));
    assert_eq!(rt().block_on(store.verify_api_key(&plain)).expect("verify"), None);
    assert_eq!(rt().block_on(store.count_api_keys()).expect("count"), 1);

    // The raw DB bytes must not contain either plaintext.
    drop(store);
    let bytes = std::fs::read(dir.db()).expect("read db file");
    assert!(!contains(&bytes, plain.as_bytes()), "plaintext key in DB file");
    assert!(!contains(&bytes, plain2.as_bytes()), "plaintext key in DB file");
    assert!(contains(&bytes, key.key_hash.as_bytes()), "hash should be stored");
}

// ----- 4b. virtual-key governance ---------------------------------------------------

#[test]
fn api_key_limits_default_then_patch_merges_and_clears() {
    let store = Store::open_in_memory().expect("mem");
    let s = &store;
    rt().block_on(async {
        let (plain, key) = s.create_api_key(Some("laptop")).await.expect("create");
        // A brand new key carries no limits at all.
        assert_eq!((key.rpm, key.tpm, key.budget_tokens), (0, 0, 0));
        assert_eq!(key.allowed_models, None);
        assert_eq!(key.expires_at, None);
        assert!(key.is_active);
        assert_eq!(s.api_key_spend(key.id, None).await.expect("spend"), 0);

        // A partial patch changes only the named field.
        let patched = s
            .update_api_key(key.id, &ApiKeyPatch { rpm: Some(60), ..Default::default() })
            .await
            .expect("patch")
            .expect("exists");
        assert_eq!(patched.rpm, 60);
        assert_eq!(patched.tpm, 0);
        assert_eq!(patched.name.as_deref(), Some("laptop"));

        let patched = s
            .update_api_key(
                key.id,
                &ApiKeyPatch {
                    tpm: Some(90_000),
                    budget_tokens: Some(5_000_000),
                    allowed_models: Some(Some(vec![
                        "claude/claude-haiku-4-5-20251001".to_string(),
                    ])),
                    expires_at: Some(Some(1_800_000_000)),
                    ..Default::default()
                },
            )
            .await
            .expect("patch")
            .expect("exists");
        assert_eq!(patched.tpm, 90_000);
        assert_eq!(patched.budget_tokens, 5_000_000);
        assert_eq!(
            patched.allowed_models,
            Some(vec!["claude/claude-haiku-4-5-20251001".to_string()])
        );
        assert_eq!(patched.expires_at, Some(1_800_000_000));
        assert_eq!(patched.rpm, 60, "the earlier patch survived");

        // The merged row is what SQLite holds, not just what the call returned.
        assert_eq!(s.list_api_keys().await.expect("list"), vec![patched.clone()]);

        // `Some(None)` clears a nullable column and leaves the rest alone.
        let cleared = s
            .update_api_key(
                key.id,
                &ApiKeyPatch {
                    allowed_models: Some(None),
                    expires_at: Some(None),
                    ..Default::default()
                },
            )
            .await
            .expect("clear")
            .expect("exists");
        assert_eq!(cleared.allowed_models, None);
        assert_eq!(cleared.expires_at, None);
        assert_eq!(cleared.tpm, 90_000, "clearing did not touch other fields");

        // A disabled key is still found, so the gateway can answer 403 instead of 401.
        let off = s
            .update_api_key(key.id, &ApiKeyPatch { is_active: Some(false), ..Default::default() })
            .await
            .expect("disable")
            .expect("exists");
        assert!(!off.is_active);
        let found = s
            .verify_api_key(&plain)
            .await
            .expect("verify")
            .expect("a disabled key is still a known key");
        assert!(!found.is_active);

        assert!(
            s.update_api_key(Uuid::new_v4(), &ApiKeyPatch::default())
                .await
                .expect("unknown")
                .is_none(),
            "patching a missing key reports it, it does not create it"
        );
    });
}

#[test]
fn spent_tokens_sums_only_that_key() {
    let store = Store::open_in_memory().expect("mem");
    let s = &store;
    rt().block_on(async {
        let (_, a) = s.create_api_key(Some("a")).await.expect("a");
        let (_, b) = s.create_api_key(Some("b")).await.expect("b");
        let rec = |key: Option<Uuid>, ts: i64, inp: u32, out: u32, cr: u32, cw: u32| UsageRecord {
            id: None,
            ts,
            request_id: Some(Uuid::new_v4()),
            provider: "claude".to_string(),
            model: "haiku".to_string(),
            connection_id: None,
            endpoint: Some("/v1/messages".to_string()),
            key_id: key,
            cost_micros: 0,
            input_tokens: inp,
            output_tokens: out,
            cache_read_tokens: cr,
            cache_write_tokens: cw,
            latency_ms: 1,
            ttft_ms: 0,
            status: 200,
            streamed: false,
            error: None,
        };
        // a: 100 + 20 + 5 + 5 = 130 across two rows; b: 7; anonymous: excluded.
        s.record_usage(&rec(Some(a.id), 100, 60, 10, 5, 5)).await.expect("a1");
        s.record_usage(&rec(Some(a.id), 200, 40, 10, 0, 0)).await.expect("a2");
        s.record_usage(&rec(Some(b.id), 300, 7, 0, 0, 0)).await.expect("b1");
        s.record_usage(&rec(None, 400, 99, 0, 0, 0)).await.expect("anon");

        assert_eq!(s.api_key_spend(a.id, None).await.expect("a spend"), 130);
        assert_eq!(s.api_key_spend(b.id, None).await.expect("b spend"), 7);
        assert_eq!(s.api_key_spend(Uuid::new_v4(), None).await.expect("none"), 0);

        let all = s.api_key_spend_all(None).await.expect("all");
        assert_eq!(all.get(&a.id).copied(), Some(130));
        assert_eq!(all.get(&b.id).copied(), Some(7));
        assert_eq!(all.len(), 2, "anonymous rows belong to no key");

        // Attribution is readable back, which is what `/api/usage` exposes.
        let recent = s.recent_usage(&query(10)).await.expect("recent");
        assert_eq!(recent[0].key_id, None, "newest row is the anonymous one");
        assert!(recent.iter().any(|r| r.key_id == Some(a.id)));
        assert!(recent.iter().any(|r| r.key_id == Some(b.id)));
    });
}

/// Money is recorded beside the tokens: a row's micro-USD cost round-trips, and a key's
/// spend is the sum of only its own rows.
#[test]
fn cost_micros_round_trips_and_sums_per_key() {
    let store = Store::open_in_memory().expect("mem");
    let s = &store;
    rt().block_on(async {
        let (_, a) = s.create_api_key(Some("a")).await.expect("a");
        let (_, b) = s.create_api_key(Some("b")).await.expect("b");
        let rec = |key: Option<Uuid>, cost: u64| UsageRecord {
            id: None,
            ts: 1_700_000_000,
            request_id: Some(Uuid::new_v4()),
            provider: "claude".to_string(),
            model: "haiku".to_string(),
            connection_id: None,
            endpoint: Some("/v1/messages".to_string()),
            key_id: key,
            cost_micros: cost,
            input_tokens: 1,
            output_tokens: 1,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            latency_ms: 1,
            ttft_ms: 0,
            status: 200,
            streamed: false,
            error: None,
        };
        s.record_usage(&rec(Some(a.id), 1_250)).await.expect("a1");
        s.record_usage(&rec(Some(a.id), 3_000)).await.expect("a2");
        s.record_usage(&rec(Some(b.id), 500)).await.expect("b1");
        s.record_usage(&rec(None, 9_999)).await.expect("anon");

        assert_eq!(s.api_key_cost_spend(a.id, None).await.expect("a"), 4_250);
        assert_eq!(s.api_key_cost_spend(b.id, None).await.expect("b"), 500);
        assert_eq!(
            s.api_key_cost_spend(Uuid::new_v4(), None).await.expect("unknown"),
            0,
            "a key with no rows has spent nothing"
        );

        let all = s.api_key_cost_spend_all(None).await.expect("all");
        assert_eq!(all.get(&a.id).copied(), Some(4_250));
        assert_eq!(all.get(&b.id).copied(), Some(500));
        assert_eq!(all.len(), 2, "anonymous rows belong to no key");

        // The cost is stored with the row, not recomputed on read.
        let recent = s.recent_usage(&query(10)).await.expect("recent");
        assert_eq!(recent[0].cost_micros, 9_999);
        assert!(recent.iter().any(|r| r.cost_micros == 3_000));
    });
}

/// The accounting lookup sees only rows that publish a price, and only for the
/// providers it is asked about — the two ids a request can be priced under.
#[test]
fn model_costs_returns_published_rows_for_the_named_providers() {
    let store = Store::open_in_memory().expect("mem");
    let row = |provider: &str, model: &str, source: &str, cost: Option<serde_json::Value>| {
        ModelRow {
            provider: provider.to_string(),
            model: model.to_string(),
            display: None,
            context: None,
            max_output: None,
            caps: serde_json::json!({}),
            cost,
            source: source.to_string(),
            updated_at: 0,
        }
    };
    rt().block_on(async {
        store
            .upsert_model(&row(
                "openai",
                "gpt-test",
                "synced",
                Some(serde_json::json!({"input": 1.0, "output": 2.0})),
            ))
            .await
            .expect("priced row");
        store
            .upsert_model(&row("codex", "gpt-test", "upstream", None))
            .await
            .expect("unpriced row");
        store
            .upsert_model(&row(
                "anthropic",
                "other-model",
                "synced",
                Some(serde_json::json!({"input": 3.0, "output": 4.0})),
            ))
            .await
            .expect("other model");

        let rows = store
            .model_costs("gpt-test", &["codex", "openai"])
            .await
            .expect("costs");
        assert_eq!(rows.len(), 1, "the unpriced row and the other model are not prices");
        assert_eq!(rows[0].provider, "openai");
        assert_eq!(rows[0].source, "synced");
        assert_eq!(rows[0].cost["input"], 1.0);

        assert!(store.model_costs("gpt-test", &[]).await.expect("none").is_empty());
    });
}

#[test]
fn v6_database_migrates_to_thirteen() {
    let dir = TempDir::new("v6");
    // A real v6 database: api_keys before the governance columns, usage before key_id.
    {
        let conn = rusqlite::Connection::open(dir.db()).expect("open raw");
        conn.execute_batch(
            // V9 alters `connections`, so the fixture must carry the table V1 created.
            "CREATE TABLE connections (
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

             CREATE TABLE api_keys (
               id         TEXT PRIMARY KEY,
               key_hash   TEXT NOT NULL UNIQUE,
               name       TEXT,
               is_active  INTEGER NOT NULL DEFAULT 1,
               created_at INTEGER NOT NULL
             );
             INSERT INTO api_keys (id, key_hash, name, is_active, created_at)
             VALUES ('11111111-1111-4111-8111-111111111111', 'deadbeef', 'legacy', 1, 1700000000);

             CREATE TABLE usage (
               id                 INTEGER PRIMARY KEY AUTOINCREMENT,
               ts                 INTEGER NOT NULL,
               provider           TEXT NOT NULL,
               model              TEXT NOT NULL,
               connection_id      TEXT,
               input_tokens       INTEGER NOT NULL DEFAULT 0,
               output_tokens      INTEGER NOT NULL DEFAULT 0,
               latency_ms         INTEGER NOT NULL DEFAULT 0,
               status             INTEGER NOT NULL,
               streamed           INTEGER NOT NULL DEFAULT 0,
               error              TEXT,
               cache_read_tokens  INTEGER NOT NULL DEFAULT 0,
               cache_write_tokens INTEGER NOT NULL DEFAULT 0,
               ttft_ms            INTEGER NOT NULL DEFAULT 0,
               endpoint           TEXT
             );
             INSERT INTO usage (ts, provider, model, status, input_tokens)
             VALUES (1700000000, 'claude', 'haiku', 200, 42);

             PRAGMA user_version = 6;",
        )
        .expect("v6 schema");
    }

    let store = Store::open(&dir.db(), None).expect("open store runs the v7-v10 steps");
    let keys = rt().block_on(store.list_api_keys()).expect("list");
    assert_eq!(keys.len(), 1);
    let legacy = &keys[0];
    assert_eq!(legacy.name.as_deref(), Some("legacy"));
    assert_eq!(
        (legacy.rpm, legacy.tpm, legacy.budget_tokens),
        (0, 0, 0),
        "existing keys stay unlimited after the migration"
    );
    assert_eq!(legacy.allowed_models, None);
    assert_eq!(legacy.expires_at, None);
    assert_eq!(
        legacy.budget_micros, 0,
        "a pre-v10 key has no money budget: unlimited, exactly as before"
    );

    let recent = rt().block_on(store.recent_usage(&query(10))).expect("recent");
    assert_eq!(recent.len(), 1);
    assert_eq!(recent[0].input_tokens, 42, "pre-v7 rows are untouched");
    assert_eq!(
        recent[0].cost_micros, 0,
        "a pre-v10 row has no recorded price, and none is invented for it"
    );
    assert_eq!(recent[0].key_id, None, "pre-v7 rows are anonymous");
    assert_eq!(
        recent[0].request_id, None,
        "rows from before request ids existed read back as unknown, not as a bogus id"
    );
    // The attempt table exists and is empty: no request had a fallback yet.
    assert_eq!(
        rt().block_on(store.attempts_for(Uuid::new_v4())).expect("attempts").len(),
        0
    );
    drop(store);

    let conn = rusqlite::Connection::open(dir.db()).expect("reopen");
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .expect("version");
    assert_eq!(version, 13);
    for (index, why) in [
        ("idx_usage_key_ts", "per-key spend must be indexed"),
        ("idx_usage_request", "the usage row is looked up by request id"),
        ("idx_attempts_request", "attempts are read per request, in order"),
    ] {
        let found: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type = 'index' AND name = ?1",
                [index],
                |r| r.get(0),
            )
            .expect("index");
        assert_eq!(found, 1, "{why}: {index}");
    }
}

/// A v8 database gains the routing inputs without touching existing rows: every
/// connection defaults to weight 100 (the plain round-robin share), and the two new
/// tables start empty.
#[test]
fn v8_database_migrates_to_thirteen() {
    let dir = TempDir::new("v8");
    {
        let conn = rusqlite::Connection::open(dir.db()).expect("open raw");
        conn.execute_batch(
            "CREATE TABLE connections (
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
             INSERT INTO connections (id, provider, auth_type, data, created_at, updated_at)
             VALUES ('11111111-1111-4111-8111-111111111111', 'openai', 'api_key', x'00',
                     1700000000, 1700000000);

             -- v7/v8 shape: the later steps only add columns, and a real v8 database
             -- always has both tables.
             CREATE TABLE api_keys (
               id             TEXT PRIMARY KEY,
               key_hash       TEXT NOT NULL UNIQUE,
               name           TEXT,
               is_active      INTEGER NOT NULL DEFAULT 1,
               created_at     INTEGER NOT NULL,
               rpm            INTEGER NOT NULL DEFAULT 0,
               tpm            INTEGER NOT NULL DEFAULT 0,
               budget_tokens  INTEGER NOT NULL DEFAULT 0,
               allowed_models TEXT,
               expires_at     INTEGER
             );
             CREATE TABLE attempts (
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
             CREATE TABLE usage (
               id                 INTEGER PRIMARY KEY AUTOINCREMENT,
               ts                 INTEGER NOT NULL,
               request_id         TEXT NOT NULL DEFAULT '',
               provider           TEXT NOT NULL,
               model              TEXT NOT NULL,
               connection_id      TEXT,
               endpoint           TEXT,
               input_tokens       INTEGER NOT NULL DEFAULT 0,
               output_tokens      INTEGER NOT NULL DEFAULT 0,
               cache_read_tokens  INTEGER NOT NULL DEFAULT 0,
               cache_write_tokens INTEGER NOT NULL DEFAULT 0,
               latency_ms         INTEGER NOT NULL DEFAULT 0,
               ttft_ms            INTEGER NOT NULL DEFAULT 0,
               status             INTEGER NOT NULL,
               streamed           INTEGER NOT NULL DEFAULT 0,
               error              TEXT,
               key_id             TEXT
             );
             PRAGMA user_version = 8;",
        )
        .expect("v8 schema");
    }

    drop(Store::open(&dir.db(), None).expect("open store runs the v9-v10 steps"));

    let conn = rusqlite::Connection::open(dir.db()).expect("reopen");
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .expect("version");
    assert_eq!(version, 13);

    let weight: i64 = conn
        .query_row("SELECT weight FROM connections", [], |r| r.get(0))
        .expect("weight");
    assert_eq!(weight, 100, "existing connections join the weighted cycle evenly");

    for (table, why) in [
        ("connection_stats", "least_latency needs a place to keep its EWMA"),
        ("key_models", "per-key model mapping needs its table"),
    ] {
        let found: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                [table],
                |r| r.get(0),
            )
            .expect("table");
        assert_eq!(found, 1, "{why}: {table}");
    }
}

/// A v9 database gains the money columns without touching existing rows: spend recorded
/// before v10 stays at "unknown price", and every key stays unlimited.
#[test]
fn v9_database_migrates_to_thirteen() {
    let dir = TempDir::new("v9");
    {
        let conn = rusqlite::Connection::open(dir.db()).expect("open raw");
        conn.execute_batch(
            "CREATE TABLE api_keys (
               id             TEXT PRIMARY KEY,
               key_hash       TEXT NOT NULL UNIQUE,
               name           TEXT,
               is_active      INTEGER NOT NULL DEFAULT 1,
               created_at     INTEGER NOT NULL,
               rpm            INTEGER NOT NULL DEFAULT 0,
               tpm            INTEGER NOT NULL DEFAULT 0,
               budget_tokens  INTEGER NOT NULL DEFAULT 0,
               allowed_models TEXT,
               expires_at     INTEGER
             );
             INSERT INTO api_keys (id, key_hash, name, is_active, created_at, budget_tokens)
             VALUES ('11111111-1111-4111-8111-111111111111', 'deadbeef', 'v9', 1, 1700000000, 500);

             CREATE TABLE attempts (
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
             CREATE TABLE usage (
               id                 INTEGER PRIMARY KEY AUTOINCREMENT,
               ts                 INTEGER NOT NULL,
               request_id         TEXT NOT NULL DEFAULT '',
               provider           TEXT NOT NULL,
               model              TEXT NOT NULL,
               connection_id      TEXT,
               endpoint           TEXT,
               input_tokens       INTEGER NOT NULL DEFAULT 0,
               output_tokens      INTEGER NOT NULL DEFAULT 0,
               cache_read_tokens  INTEGER NOT NULL DEFAULT 0,
               cache_write_tokens INTEGER NOT NULL DEFAULT 0,
               latency_ms         INTEGER NOT NULL DEFAULT 0,
               ttft_ms            INTEGER NOT NULL DEFAULT 0,
               status             INTEGER NOT NULL,
               streamed           INTEGER NOT NULL DEFAULT 0,
               error              TEXT,
               key_id             TEXT
             );
             INSERT INTO usage (ts, request_id, provider, model, input_tokens, output_tokens,
                                status, key_id)
             VALUES (1700000000, '', 'claude', 'haiku', 42, 8, 200,
                     '11111111-1111-4111-8111-111111111111');

             PRAGMA user_version = 9;",
        )
        .expect("v9 schema");
    }

    let store = Store::open(&dir.db(), None).expect("open store runs the v10 and v11 steps");
    let keys = rt().block_on(store.list_api_keys()).expect("keys");
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].budget_tokens, 500, "the token budget survives");
    assert_eq!(keys[0].budget_micros, 0, "no money budget means unlimited");
    assert_eq!(
        rt().block_on(store.api_key_cost_spend(keys[0].id, None)).expect("spend"),
        0
    );

    let rows = rt().block_on(store.recent_usage(&query(10))).expect("usage");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].input_tokens, 42);
    assert_eq!(rows[0].cost_micros, 0);
    drop(store);

    let conn = rusqlite::Connection::open(dir.db()).expect("reopen");
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .expect("version");
    assert_eq!(version, 13);
}

/// A v10 database gains the response cache in place: the migration only adds a table and
/// its index, so every existing row is untouched and the version reaches 13.
#[test]
fn v10_database_migrates_to_thirteen() {
    let dir = TempDir::new("v10");
    {
        // A real v10 database: the current schema with the v11 table and the v12 column
        // removed, so the upgrade path is the one a deployed v10 database takes.
        drop(Store::open(&dir.db(), None).expect("current schema"));
        let conn = rusqlite::Connection::open(dir.db()).expect("open raw");
        conn.execute_batch(
            "DROP TABLE response_cache; ALTER TABLE api_keys DROP COLUMN budget_window; \
             PRAGMA user_version = 10;",
        )
        .expect("rewind to v10");
    }

    let store = Store::open(&dir.db(), None).expect("open store runs the v11 and v12 steps");
    assert!(
        rt().block_on(store.cache_entries()).expect("cache readable").is_empty(),
        "a fresh v11 cache starts empty"
    );
    drop(store);

    let conn = rusqlite::Connection::open(dir.db()).expect("reopen");
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .expect("version");
    assert_eq!(version, 13);
    for (kind, name) in [("table", "response_cache"), ("index", "idx_cache_expiry")] {
        let found: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type = ?1 AND name = ?2",
                [kind, name],
                |r| r.get(0),
            )
            .expect("sqlite_master");
        assert_eq!(found, 1, "v11 must create the {kind} {name}");
    }
}

// ----- 4c. routing inputs (phase 4) -------------------------------------------------

#[test]
fn connection_weight_is_clamped_into_the_supported_range() {
    let store = Store::open_in_memory().expect("mem");
    rt().block_on(async {
        let c = store
            .create_connection("openai", "api_key", creds("k"), Some("w"))
            .await
            .expect("connection");
        assert_eq!(c.weight, 100, "new connections join the weighted cycle evenly");

        store.set_connection_weight(c.id, 0).await.expect("clamp low");
        let read = store.get_connection(c.id).await.expect("read").expect("exists");
        assert_eq!(read.weight, 1, "zero would starve an account that must still serve");

        store.set_connection_weight(c.id, 10_000).await.expect("clamp high");
        let read = store.get_connection(c.id).await.expect("read").expect("exists");
        assert_eq!(read.weight, 1000);

        assert!(
            store.set_connection_weight(Uuid::new_v4(), 5).await.is_err(),
            "an unknown connection is reported, not silently ignored"
        );
    });
}

#[test]
fn latency_ewma_is_old_times_zero_point_eight_plus_sample_times_zero_point_two() {
    let store = Store::open_in_memory().expect("mem");
    rt().block_on(async {
        let c = store
            .create_connection("openai", "api_key", creds("k"), Some("fast"))
            .await
            .expect("connection");

        // First sample seeds the EWMA; later samples move it by 20%.
        store.record_connection_latency(c.id, 100).await.expect("first");
        let ewma = store.connection_latencies().await.expect("latencies");
        assert_eq!(ewma.get(&c.id).copied(), Some(100.0));

        store.record_connection_latency(c.id, 200).await.expect("second");
        let ewma = store.connection_latencies().await.expect("latencies");
        assert_eq!(ewma.get(&c.id).copied(), Some(120.0), "100*0.8 + 200*0.2");

        store.record_connection_latency(c.id, 300).await.expect("third");
        let ewma = store.connection_latencies().await.expect("latencies");
        assert_eq!(ewma.get(&c.id).copied(), Some(156.0), "120*0.8 + 300*0.2");

        // A deleted connection takes its sample with it.
        assert!(store.delete_connection(c.id).await.expect("delete"));
        assert!(store.connection_latencies().await.expect("latencies").is_empty());
    });
}

#[test]
fn key_models_replace_whole_map_and_reject_duplicates() {
    let store = Store::open_in_memory().expect("mem");
    rt().block_on(async {
        let (_, key) = store.create_api_key(Some("laptop")).await.expect("key");
        let haiku = KeyModel {
            from: "default".into(),
            to: "claude/claude-haiku-4-5-20251001".into(),
        };
        let sonnet = KeyModel {
            from: "smart".into(),
            to: "claude/claude-sonnet-4-5".into(),
        };
        assert!(
            store.replace_key_models(key.id, &[haiku.clone(), sonnet.clone()]).await.expect("put")
        );

        // Read back in `from` order, and findable by exact client model.
        assert_eq!(store.key_models(key.id).await.expect("models"), vec![haiku.clone(), sonnet]);
        assert_eq!(
            store.key_model_target(key.id, "default").await.expect("target").as_deref(),
            Some("claude/claude-haiku-4-5-20251001")
        );
        assert_eq!(store.key_model_target(key.id, "unmapped").await.expect("target"), None);

        // A second PUT replaces the whole map rather than merging into it.
        let only = KeyModel { from: "default".into(), to: "openai/gpt-5".into() };
        store.replace_key_models(key.id, std::slice::from_ref(&only)).await.expect("replace");
        assert_eq!(store.key_models(key.id).await.expect("models"), vec![only]);
        let all = store.key_models_all().await.expect("all");
        assert_eq!(all.get(&key.id).map(Vec::len), Some(1));

        // An empty list clears it.
        store.replace_key_models(key.id, &[]).await.expect("clear");
        assert!(store.key_models(key.id).await.expect("models").is_empty());

        // A duplicate `from` is refused by the primary key, and the previous state is
        // left alone because the whole write is one transaction.
        store.replace_key_models(key.id, std::slice::from_ref(&haiku)).await.expect("put");
        assert!(
            store.replace_key_models(key.id, &[haiku.clone(), haiku]).await.is_err(),
            "one key cannot map the same client model twice"
        );
        assert_eq!(store.key_models(key.id).await.expect("models").len(), 1);

        assert!(
            !store
                .replace_key_models(Uuid::new_v4(), &[KeyModel {
                    from: "a".into(),
                    to: "openai/gpt-5".into()
                }])
                .await
                .expect("unknown key"),
            "an unknown key is reported, not created"
        );

        // Deleting the key drops its mappings.
        assert!(store.delete_api_key(key.id).await.expect("delete key"));
        assert!(store.key_models_all().await.expect("all").is_empty());
    });
}

// ----- 5. credentials never plaintext ----------------------------------------------

#[test]
fn credentials_never_stored_in_plaintext() {
    let dir = TempDir::new("secret");
    let store = dir.store();
    let c = rt()
        .block_on(store.create_connection(
            "claude",
            "oauth",
            Credentials {
                access_token: Some("SECRET-XYZ".to_string()),
                refresh_token: Some("REFRESH-SECRET-ABC".to_string()),
                ..Credentials::default()
            },
            Some("acct"),
        ))
        .expect("create");
    // Round-trips through the cipher...
    let back = rt()
        .block_on(store.get_connection(c.id))
        .expect("get")
        .expect("row");
    assert_eq!(back.credentials.access_token.as_deref(), Some("SECRET-XYZ"));

    // ...but not through the file. Close every handle first so WAL is checkpointed.
    drop(store);
    let bytes = std::fs::read(dir.db()).expect("read db");
    assert!(!contains(&bytes, b"SECRET-XYZ"), "access_token leaked to disk");
    assert!(!contains(&bytes, b"REFRESH-SECRET-ABC"), "refresh token leaked");
    // The key file must not be the plaintext either.
    let key = std::fs::read(dir.path().join(KEY_FILE_NAME)).expect("key");
    assert!(!contains(&key, b"SECRET-XYZ"));
}

// ----- 6. config + usage -----------------------------------------------------------

#[test]
fn aliases_and_disabled_models_upsert_list_delete() {
    let store = Store::open_in_memory().expect("mem");
    let s = &store;
    rt().block_on(async {
        s.upsert_alias("fast", "groq/llama-3.1-8b").await.expect("a1");
        s.upsert_alias("smart", "anthropic/claude-sonnet-4").await.expect("a2");
        s.upsert_alias("fast", "cerebras/llama-3.1-8b").await.expect("a1 update");
        let aliases = s.list_aliases().await.expect("list");
        assert_eq!(aliases.len(), 2);
        assert_eq!(aliases[0].alias, "fast");
        assert_eq!(aliases[0].target, "cerebras/llama-3.1-8b", "upsert replaces target");
        assert!(s.delete_alias("fast").await.expect("del"));
        assert!(!s.delete_alias("fast").await.expect("del again"));
        assert_eq!(s.list_aliases().await.expect("list").len(), 1);

        s.set_disabled("gpt-4o", true).await.expect("disable");
        s.set_disabled("gpt-4o", true).await.expect("disable twice");
        s.set_disabled("o3", true).await.expect("disable");
        assert_eq!(s.disabled_models().await.expect("dis"), vec!["gpt-4o", "o3"]);
        s.set_disabled("gpt-4o", false).await.expect("enable");
        assert_eq!(s.disabled_models().await.expect("dis"), vec!["o3"]);
    });
}

#[test]
fn combos_upsert_preserve_order_and_identity() {
    let store = Store::open_in_memory().expect("mem");
    let s = &store;
    rt().block_on(async {
        let models = vec![
            "claude/claude-sonnet-4".to_string(),
            "codex/gpt-5".to_string(),
            "openai/gpt-4o".to_string(),
        ];
        let created = s.upsert_combo("best", &models).await.expect("create");
        assert_eq!(created.models, models);
        assert_eq!(created.name, "best");

        let updated = s
            .upsert_combo("best", &["gemini/gemini-2.5-pro".to_string()])
            .await
            .expect("update");
        assert_eq!(updated.id, created.id, "upsert keeps the row id");
        assert_eq!(updated.created_at, created.created_at);
        assert_eq!(updated.models, vec!["gemini/gemini-2.5-pro".to_string()]);

        s.upsert_combo("cheap", &["groq/llama-3.1-8b".to_string()])
            .await
            .expect("second");
        let all = s.list_combos().await.expect("list");
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].name, "best");

        let got = s.get_combo("best").await.expect("get").expect("some");
        assert_eq!(got.models, vec!["gemini/gemini-2.5-pro".to_string()]);
        assert!(s.get_combo("missing").await.expect("get").is_none());

        assert!(s.delete_combo("best").await.expect("del"));
        assert!(!s.delete_combo("best").await.expect("del again"));
        assert_eq!(s.list_combos().await.expect("list").len(), 1);
    });
}

#[test]
fn usage_records_summary_and_recent_order() {
    let store = Store::open_in_memory().expect("mem");
    let s = &store;
    let conn_id = Uuid::new_v4();
    let rec = |ts: i64, provider: &str, model: &str, inp: u32, out: u32, ms: u32, st: u16| {
        UsageRecord {
            id: None,
            ts,
            // Deterministic per row, so the audit trail is assertable after the read.
            request_id: Some(Uuid::from_u128(ts as u128)),
            provider: provider.to_string(),
            model: model.to_string(),
            connection_id: Some(conn_id),
            endpoint: Some("/v1/chat/completions".to_string()),
            key_id: None,
            cost_micros: 0,
            input_tokens: inp,
            output_tokens: out,
            // Prompt-cache counters are priced separately; keep them per row.
            cache_read_tokens: if ts == 100 { 900 } else { 0 },
            cache_write_tokens: if ts == 100 { 100 } else { 0 },
            latency_ms: ms,
            ttft_ms: if st == 200 { ms / 2 } else { 0 },
            status: st,
            streamed: true,
            error: (st >= 400).then(|| "upstream 429".to_string()),
        }
    };
    rt().block_on(async {
        s.record_usage(&rec(100, "claude", "sonnet", 10, 20, 100, 200)).await.expect("r1");
        s.record_usage(&rec(300, "claude", "sonnet", 30, 40, 300, 200)).await.expect("r2");
        s.record_usage(&rec(200, "claude", "sonnet", 0, 0, 200, 429)).await.expect("r3");
        s.record_usage(&rec(400, "groq", "llama", 5, 5, 400, 200)).await.expect("r4");

        let recent = s.recent_usage(&query(3)).await.expect("recent");
        assert_eq!(recent.len(), 3);
        assert_eq!(recent[0].ts, 400, "newest first");
        assert_eq!(recent[1].ts, 300);
        assert_eq!(recent[2].ts, 200);
        assert_eq!(recent[2].status, 429);
        assert!(recent[2].streamed);
        assert_eq!(recent[2].error.as_deref(), Some("upstream 429"));
        assert_eq!(recent[0].connection_id, Some(conn_id));
        let cached = s
            .recent_usage(&query(4))
            .await
            .expect("recent all")
            .into_iter()
            .find(|r| r.ts == 100)
            .expect("row 100");
        assert_eq!(cached.cache_read_tokens, 900);
        assert_eq!(cached.cache_write_tokens, 100);
        assert_eq!(cached.ttft_ms, 50, "first-output time round-trips");

        let sum = s.usage_summary(None).await.expect("summary");
        assert_eq!(sum.len(), 2);
        let claude = &sum[0];
        assert_eq!((claude.provider.as_str(), claude.model.as_str()), ("claude", "sonnet"));
        assert_eq!(claude.requests, 3);
        assert_eq!(claude.errors, 1, "status >= 400 counts as an error");
        assert_eq!(claude.input_tokens, 40);
        assert_eq!(claude.output_tokens, 60);
        assert_eq!(claude.cache_read_tokens, 900);
        assert_eq!(claude.cache_write_tokens, 100);
        assert_eq!(claude.avg_latency_ms, 200, "(100+300+200)/3");
        let groq = &sum[1];
        assert_eq!((groq.requests, groq.errors, groq.avg_latency_ms), (1, 0, 400));

        // A range-scoped summary is what the Activity page shows; it must not leak
        // older traffic into the window.
        let recent_only = s.usage_summary(Some(250)).await.expect("scoped");
        assert_eq!(recent_only.len(), 2);
        assert_eq!(recent_only[0].requests, 1, "claude rows at 300 only");
        assert_eq!(recent_only[1].requests, 1, "groq row at 400 only");

        // Rows read back carry the id the log copies, and the endpoint they came from.
        let newest = &recent[0];
        assert!(newest.id.is_some(), "usage ids are exposed for the request log");
        assert_eq!(newest.endpoint.as_deref(), Some("/v1/chat/completions"));
        assert_eq!(
            cached.request_id,
            Some(Uuid::from_u128(100)),
            "the request id the client saw round-trips"
        );

        // Stacked chart data: one row per bucket and model, with its own counters.
        let by_model = s.usage_series_by_model(0, 200).await.expect("by model");
        assert_eq!(by_model.len(), 3, "claude@0, claude@200+300, groq@400");
        let claude_bucket = by_model
            .iter()
            .find(|b| b.provider == "claude" && b.start == 200)
            .expect("bucket 200");
        assert_eq!((claude_bucket.requests, claude_bucket.errors), (2, 1));
        assert_eq!(claude_bucket.input_tokens, 30);
        assert_eq!(claude_bucket.output_tokens, 40);
    });
}

// ----- request ids and the attempt log ----------------------------------------------

/// One request, two candidates: the failing attempt and the one that served are both
/// kept, ordered, and reachable from the same id as the usage row.
#[test]
fn attempts_round_trip_under_the_request_id() {
    let store = Store::open_in_memory().expect("mem");
    let s = &store;
    let request_id = Uuid::new_v4();
    let first_conn = Uuid::new_v4();
    let second_conn = Uuid::new_v4();
    let key = Uuid::new_v4();
    rt().block_on(async {
        for (attempt, conn, model, status, error, latency) in [
            (1u32, first_conn, "gpt-4.1", 503u16, Some("upstream openai returned 503: overloaded"), 12u32),
            (2, second_conn, "gpt-4.1-mini", 200, None, 40),
        ] {
            s.record_attempt(&AttemptRecord {
                id: None,
                request_id,
                ts: 1_700_000_000 + i64::from(attempt),
                attempt,
                provider: "openai".to_string(),
                model: model.to_string(),
                connection_id: Some(conn),
                status,
                error: error.map(str::to_string),
                latency_ms: latency,
                key_id: if attempt == 2 { Some(key) } else { None },
            })
            .await
            .expect("attempt");
        }
        // A different request must not leak into this one's log.
        s.record_attempt(&AttemptRecord {
            id: None,
            request_id: Uuid::new_v4(),
            ts: 1,
            attempt: 1,
            provider: "groq".to_string(),
            model: "llama".to_string(),
            connection_id: None,
            status: 0,
            error: Some("transport failure: connection refused".to_string()),
            latency_ms: 3,
            key_id: None,
        })
        .await
        .expect("other attempt");

        let rows = s.attempts_for(request_id).await.expect("attempts");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].attempt, 1);
        assert_eq!(rows[0].status, 503, "the failing attempt keeps its upstream status");
        assert_eq!(
            rows[0].error.as_deref(),
            Some("upstream openai returned 503: overloaded"),
            "the client-visible message is what the log keeps"
        );
        assert_eq!(rows[0].connection_id, Some(first_conn));
        assert_eq!(rows[0].latency_ms, 12);
        assert_eq!(rows[0].key_id, None, "anonymous traffic has no key");
        assert_eq!(rows[1].attempt, 2);
        assert_eq!(rows[1].status, 200);
        assert_eq!(rows[1].error, None);
        assert_eq!(rows[1].key_id, Some(key));
        assert_eq!(rows[1].connection_id, Some(second_conn));
        assert!(rows.iter().all(|r| r.request_id == request_id));

        // The usage row is found by the same id the client was given.
        s.record_usage(&UsageRecord {
            id: None,
            ts: 1_700_000_010,
            request_id: Some(request_id),
            provider: "openai".to_string(),
            model: "gpt-4.1".to_string(),
            connection_id: Some(second_conn),
            endpoint: Some("/v1/chat/completions".to_string()),
            key_id: Some(key),
            cost_micros: 0,
            input_tokens: 7,
            output_tokens: 3,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            latency_ms: 52,
            ttft_ms: 0,
            status: 200,
            streamed: false,
            error: None,
        })
        .await
        .expect("usage");

        let found = s
            .usage_by_request(request_id)
            .await
            .expect("usage by request")
            .expect("row for this request");
        assert_eq!(found.request_id, Some(request_id));
        assert_eq!(found.provider, "openai");
        assert!(
            s.usage_by_request(Uuid::new_v4()).await.expect("missing").is_none(),
            "an unknown request id has no usage row"
        );
    });
}

// ----- connection CRUD, status, list filter ----------------------------------------

#[test]
fn connection_crud_status_and_list_filter() {
    let store = Store::open_in_memory().expect("mem");
    let s = &store;
    rt().block_on(async {
        let a = s.create_connection("claude", "oauth", creds("a"), Some("one")).await.expect("a");
        let b = s.create_connection("groq", "apikey", creds("b"), None).await.expect("b");
        assert!(a.is_active);
        assert_eq!(a.priority, 0);
        assert_eq!(a.backoff_level, 0);
        assert!(a.last_error.is_none());
        assert!(a.created_at > 0);

        assert_eq!(s.list_connections(None).await.expect("all").len(), 2);
        let claude = s.list_connections(Some("claude")).await.expect("filtered");
        assert_eq!(claude.len(), 1);
        assert_eq!(claude[0].id, a.id);
        assert_eq!(claude[0].name.as_deref(), Some("one"));
        assert!(s.list_connections(Some("nope")).await.expect("none").is_empty());

        s.update_connection_status(a.id, 2, Some("rate limited"), Some(429))
            .await
            .expect("status");
        let a2 = s.get_connection(a.id).await.expect("get").expect("row");
        assert_eq!(a2.backoff_level, 2);
        assert_eq!(a2.last_error.as_deref(), Some("rate limited"));
        assert_eq!(a2.last_error_code, Some(429));
        assert!(a2.last_error_at.is_some());
        assert!(s
            .update_connection_status(Uuid::new_v4(), 1, None, None)
            .await
            .is_err());

        // merge never erases with None
        s.merge_credentials(b.id, Credentials::default()).await.expect("noop merge");
        let b2 = s.get_connection(b.id).await.expect("get").expect("row");
        assert_eq!(b2.credentials.access_token.as_deref(), Some("b"));

        assert!(s.delete_connection(a.id).await.expect("del"));
        assert!(!s.delete_connection(a.id).await.expect("del again"));
        assert!(s.get_connection(a.id).await.expect("get").is_none());
    });
}

// ----- response cache (phase 8) ------------------------------------------------------

/// The table is a bounded, expiring key/value store: a hit is served until `expires_at`,
/// an expired read deletes the row, and inserts past the cap drop the oldest first.
#[test]
fn response_cache_expires_lazily_and_evicts_the_oldest() {
    let store = Store::open_in_memory().expect("mem");
    let s = &store;
    rt().block_on(async {
        let key = cache_key("identical request");
        assert_eq!(s.cache_get(&key, 1_000).await.expect("empty"), None);

        s.cache_put(&key, r#"{"id":"a"}"#, 1_000, 1_100, 10).await.expect("put");
        assert_eq!(
            s.cache_get(&key, 1_050).await.expect("hit"),
            Some(r#"{"id":"a"}"#.to_string())
        );
        let entries = s.cache_entries().await.expect("entries");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].hits, 1, "a served entry counts its hits");
        assert_eq!(entries[0].expires_at, 1_100);

        // The second read is at the expiry second: no longer servable, and gone.
        assert_eq!(s.cache_get(&key, 1_100).await.expect("expired"), None);
        assert!(s.cache_entries().await.expect("entries").is_empty(), "an expired read deletes the row");

        // Three fresh entries under a cap of two: the oldest is evicted, not the newest.
        for (i, payload) in ["a", "b", "c"].iter().enumerate() {
            s.cache_put(
                &cache_key(payload),
                payload,
                2_000 + i as i64,
                9_000,
                2,
            )
            .await
            .expect("put");
        }
        let keys: Vec<String> = s
            .cache_entries()
            .await
            .expect("entries")
            .into_iter()
            .map(|e| e.key)
            .collect();
        assert_eq!(keys, vec![cache_key("b"), cache_key("c")], "oldest created_at loses");
        assert_eq!(
            s.cache_get(&cache_key("a"), 2_500).await.expect("evicted"),
            None
        );
    });
}

/// Re-storing an existing key replaces the body and restarts its clock: a retried
/// request must not keep serving the earlier (possibly failing) answer.
#[test]
fn response_cache_upsert_replaces_the_body_and_hits() {
    let store = Store::open_in_memory().expect("mem");
    let s = &store;
    rt().block_on(async {
        let key = cache_key("x");
        s.cache_put(&key, "one", 100, 9_000, 10).await.expect("first");
        assert_eq!(s.cache_get(&key, 200).await.expect("read"), Some("one".into()));
        s.cache_put(&key, "two", 300, 9_000, 10).await.expect("second");
        assert_eq!(s.cache_get(&key, 400).await.expect("read"), Some("two".into()));
        let entries = s.cache_entries().await.expect("entries");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].body, "two");
        assert_eq!(entries[0].hits, 1, "the counter restarts with the new body");
    });
}

// ----- usage retention (phase 14) ----------------------------------------------------

/// One sweep removes `usage` and `attempts` before the cutoff and keeps the boundary
/// row, in one report; a second sweep over the same cutoff finds nothing.
#[test]
fn prune_usage_deletes_history_older_than_the_cutoff() {
    let store = Store::open_in_memory().expect("mem");
    let s = &store;
    let row = |ts: i64| UsageRecord {
        id: None,
        ts,
        request_id: Some(Uuid::from_u128(ts as u128)),
        provider: "openai".to_string(),
        model: "gpt-4.1".to_string(),
        connection_id: None,
        endpoint: None,
        key_id: None,
        cost_micros: 0,
        input_tokens: 1,
        output_tokens: 1,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        latency_ms: 1,
        ttft_ms: 0,
        status: 200,
        streamed: false,
        error: None,
    };
    // Attempts carry their own request id, so each timestamp is separately reachable.
    let attempt_request = |ts: i64| Uuid::from_u128(0xa000 + ts as u128);
    rt().block_on(async {
        for ts in [100i64, 200, 300] {
            s.record_usage(&row(ts)).await.expect("usage");
            s.record_attempt(&AttemptRecord {
                id: None,
                request_id: attempt_request(ts),
                ts,
                attempt: 1,
                provider: "openai".to_string(),
                model: "gpt-4.1".to_string(),
                connection_id: None,
                status: 200,
                error: None,
                latency_ms: 1,
                key_id: None,
            })
            .await
            .expect("attempt");
        }

        let report = s.prune_usage(200).await.expect("prune");
        assert_eq!(report, PruneReport { usage: 1, attempts: 1 }, "the report counts each table");

        let remaining: Vec<i64> = s
            .recent_usage(&UsageQuery { limit: 10, ..Default::default() })
            .await
            .expect("usage")
            .into_iter()
            .map(|r| r.ts)
            .collect();
        assert_eq!(remaining, vec![300, 200], "the cutoff row and newer survive");
        assert!(s.attempts_for(attempt_request(100)).await.expect("old").is_empty());
        assert_eq!(s.attempts_for(attempt_request(200)).await.expect("boundary").len(), 1);
        assert_eq!(s.attempts_for(attempt_request(300)).await.expect("new").len(), 1);

        assert_eq!(
            s.prune_usage(200).await.expect("second sweep"),
            PruneReport::default(),
            "a repeated sweep is a no-op"
        );
        assert_eq!(s.recent_usage(&UsageQuery { limit: 10, ..Default::default() }).await.expect("usage").len(), 2);

        // The maintenance pass runs against the same connection without failing.
        s.optimize().await.expect("optimize");
    });
}

/// Expired cache rows are swept independently of the usage cutoff, and the unexpired
/// entry stays servable.
#[test]
fn prune_expired_cache_drops_only_expired_rows() {
    let store = Store::open_in_memory().expect("mem");
    let s = &store;
    rt().block_on(async {
        s.cache_put(&cache_key("expired"), "old", 100, 500, 10)
            .await
            .expect("put expired");
        s.cache_put(&cache_key("live"), "new", 100, 900, 10)
            .await
            .expect("put live");

        assert_eq!(s.prune_expired_cache(600).await.expect("prune"), 1);
        let keys: Vec<String> = s
            .cache_entries()
            .await
            .expect("entries")
            .into_iter()
            .map(|e| e.key)
            .collect();
        assert_eq!(keys, vec![cache_key("live")], "the unexpired row stays");
        assert_eq!(
            s.cache_get(&cache_key("live"), 600).await.expect("hit"),
            Some("new".to_string())
        );
        assert_eq!(
            s.prune_expired_cache(600).await.expect("second sweep"),
            0,
            "a repeated sweep deletes nothing"
        );
    });
}

// ----- helpers ---------------------------------------------------------------------

fn read_blob(db: &Path, id: Uuid) -> Vec<u8> {
    let c = rusqlite::Connection::open(db).expect("open");
    c.query_row("SELECT data FROM connections WHERE id = ?1", [id.to_string()], |r| {
        r.get(0)
    })
    .expect("blob")
}

fn flip_last_byte(db: &Path, id: Uuid) {
    let mut blob = read_blob(db, id);
    let last = blob.len() - 1;
    blob[last] ^= 0x01;
    let c = rusqlite::Connection::open(db).expect("open");
    c.execute(
        "UPDATE connections SET data = ?2 WHERE id = ?1",
        rusqlite::params![id.to_string(), blob],
    )
    .expect("tamper");
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

// ----- 4d. budget windows (phase 9) -------------------------------------------------

/// A v11 database gains `budget_window` in place: an existing key reads as `total`
/// (lifetime, unchanged behaviour), and the version reaches 13.
#[test]
fn v11_database_migrates_to_thirteen_with_total_windows() {
    let dir = TempDir::new("v11");
    let id = Uuid::new_v4();
    {
        let conn = rusqlite::Connection::open(dir.db()).expect("open raw");
        // Build the v11 schema by running the real migrations up to 11, then rewind the
        // marker and drop the column so the table is exactly what v11 shipped.
        drop(Store::open(&dir.db(), None).expect("current schema"));
        conn.execute_batch(
            "ALTER TABLE api_keys DROP COLUMN budget_window; PRAGMA user_version = 11;",
        )
        .expect("rewind to v11");
        conn.execute(
            "INSERT INTO api_keys (id, key_hash, name, is_active, created_at, budget_micros, \
             budget_tokens) VALUES (?1, 'h', 'legacy', 1, 1, 5000000, 100)",
            [id.to_string()],
        )
        .expect("legacy key");
    }

    let store = Store::open(&dir.db(), None).expect("open runs the v12 step");
    let key = rt()
        .block_on(store.get_api_key(id))
        .expect("read")
        .expect("legacy key survives");
    assert_eq!(key.budget_window, rrouter_store::BudgetWindow::Total);
    assert_eq!(key.budget_tokens, 100, "existing caps are untouched");
    assert_eq!(key.budget_micros, 5_000_000);
    drop(store);

    let conn = rusqlite::Connection::open(dir.db()).expect("reopen");
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .expect("version");
    assert_eq!(version, 13);
}

/// v12 shipped no `attempts(ts)` index; the retention sweep deletes by `ts`, so opening
/// a v12 database adds the index and bumps the marker.
#[test]
fn v12_database_migrates_to_thirteen_with_the_attempt_ts_index() {
    let dir = TempDir::new("v12");
    {
        let conn = rusqlite::Connection::open(dir.db()).expect("open raw");
        // Start from the current schema, then remove exactly what v13 adds.
        drop(Store::open(&dir.db(), None).expect("current schema"));
        conn.execute_batch("DROP INDEX idx_attempts_ts; PRAGMA user_version = 12;")
            .expect("rewind to v12");
    }

    drop(Store::open(&dir.db(), None).expect("open runs the v13 step"));

    let conn = rusqlite::Connection::open(dir.db()).expect("reopen");
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .expect("version");
    assert_eq!(version, 13);
    let indexes: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'index' \
             AND name = 'idx_attempts_ts'",
            [],
            |r| r.get(0),
        )
        .expect("index");
    assert_eq!(indexes, 1, "the retention sweep needs the attempts ts index");
}

/// Spend queries honour `since`: a key with rows inside and outside the window reports
/// only the inside ones, for tokens and micros alike, and `None` still means all time.
#[test]
fn spend_since_counts_only_rows_inside_the_window() {
    let store = Store::open_in_memory().expect("mem");
    let s = &store;
    rt().block_on(async {
        let (_, a) = s.create_api_key(Some("a")).await.expect("a");
        let rec = |key: Uuid, ts: i64, tokens: u32, micros: u64| UsageRecord {
            id: None,
            ts,
            request_id: Some(Uuid::new_v4()),
            provider: "claude".to_string(),
            model: "haiku".to_string(),
            connection_id: None,
            endpoint: Some("/v1/messages".to_string()),
            key_id: Some(key),
            cost_micros: micros,
            input_tokens: tokens,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            latency_ms: 1,
            ttft_ms: 0,
            status: 200,
            streamed: false,
            error: None,
        };
        let since = 1_000;
        s.record_usage(&rec(a.id, 999, 40, 400)).await.expect("before");
        s.record_usage(&rec(a.id, 1_000, 10, 100)).await.expect("at boundary");
        s.record_usage(&rec(a.id, 2_000, 5, 50)).await.expect("inside");

        assert_eq!(s.api_key_spend(a.id, Some(since)).await.expect("tokens"), 15);
        assert_eq!(s.api_key_cost_spend(a.id, Some(since)).await.expect("micros"), 150);
        assert_eq!(s.api_key_spend(a.id, None).await.expect("all tokens"), 55);
        assert_eq!(s.api_key_cost_spend(a.id, None).await.expect("all micros"), 550);

        let tokens_all = s.api_key_spend_all(Some(since)).await.expect("tokens all");
        assert_eq!(tokens_all.get(&a.id).copied(), Some(15));
        let micros_all = s.api_key_cost_spend_all(Some(since)).await.expect("micros all");
        assert_eq!(micros_all.get(&a.id).copied(), Some(150));
    });
}

/// `budget_window` round-trips through create, patch and read.
#[test]
fn budget_window_round_trips_through_patch() {
    let store = Store::open_in_memory().expect("mem");
    let s = &store;
    rt().block_on(async {
        let (_, key) = s.create_api_key(Some("w")).await.expect("create");
        assert_eq!(key.budget_window, rrouter_store::BudgetWindow::Total);
        let patched = s
            .update_api_key(
                key.id,
                &ApiKeyPatch {
                    budget_window: Some(rrouter_store::BudgetWindow::Monthly),
                    ..Default::default()
                },
            )
            .await
            .expect("patch")
            .expect("exists");
        assert_eq!(patched.budget_window, rrouter_store::BudgetWindow::Monthly);
        let read = s.get_api_key(key.id).await.expect("read").expect("row");
        assert_eq!(read.budget_window, rrouter_store::BudgetWindow::Monthly);
    });
}

/// A minimal usage row at `ts`, identified by `marker` in the model name so a test can
/// tell which rows a page returned.
fn page_row(ts: i64, marker: &str) -> UsageRecord {
    UsageRecord {
        id: None,
        ts,
        request_id: Some(Uuid::from_u128(ts as u128)),
        provider: "claude".to_string(),
        model: marker.to_string(),
        connection_id: None,
        endpoint: None,
        key_id: None,
        cost_micros: 0,
        input_tokens: 1,
        output_tokens: 1,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        latency_ms: 1,
        ttft_ms: 0,
        status: 200,
        streamed: false,
        error: None,
    }
}

/// Walks every page with the `(ts, id)` cursor and returns the rows in page order.
fn walk_pages(s: &Store, limit: usize, since: Option<i64>) -> Vec<UsageRecord> {
    rt().block_on(async {
        let mut all = Vec::new();
        let mut cursor: Option<(i64, i64)> = None;
        loop {
            let (bt, bi) = match cursor {
                Some((t, i)) => (Some(t), Some(i)),
                None => (None, None),
            };
            let q = UsageQuery { limit, since, before_ts: bt, before_id: bi, ..Default::default() };
            let page = s.recent_usage(&q).await.expect("page");
            let short = page.len() < limit;
            cursor = page.last().map(|r| (r.ts, r.id.expect("id read back")));
            all.extend(page);
            if short {
                break;
            }
        }
        all
    })
}

#[test]
fn usage_cursor_pages_disjoint_and_ordered() {
    let store = Store::open_in_memory().expect("mem");
    rt().block_on(async {
        for (ts, m) in [(10, "a"), (20, "b"), (30, "c"), (40, "d"), (50, "e")] {
            store.record_usage(&page_row(ts, m)).await.expect("insert");
        }
        let first = store.recent_usage(&query(2)).await.expect("first");
        assert_eq!(first.iter().map(|r| r.ts).collect::<Vec<_>>(), vec![50, 40]);
        let cursor_ts = first.last().expect("row").ts;
        let cursor_id = first.last().expect("row").id.expect("id");
        let second = store
            .recent_usage(&UsageQuery {
                limit: 2,
                before_ts: Some(cursor_ts),
                before_id: Some(cursor_id),
                ..Default::default()
            })
            .await
            .expect("second");
        assert_eq!(second.iter().map(|r| r.ts).collect::<Vec<_>>(), vec![30, 20]);
    });
    let all = walk_pages(&store, 2, None);
    let ts: Vec<i64> = all.iter().map(|r| r.ts).collect();
    assert_eq!(ts, vec![50, 40, 30, 20, 10], "every row once, newest first");
    let mut ids: Vec<i64> = all.iter().map(|r| r.id.expect("id")).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), 5, "no id repeats across pages");
}

#[test]
fn usage_cursor_equal_timestamps_neither_drop_nor_repeat() {
    let store = Store::open_in_memory().expect("mem");
    rt().block_on(async {
        // Five rows share ts=100; with limit 2 every page boundary splits the group.
        for m in ["a", "b", "c", "d", "e"] {
            store.record_usage(&page_row(100, m)).await.expect("insert");
        }
        store.record_usage(&page_row(200, "newer")).await.expect("insert");
    });
    let all = walk_pages(&store, 2, None);
    assert_eq!(all.len(), 6, "all rows returned: {all:?}");
    let mut ids: Vec<i64> = all.iter().map(|r| r.id.expect("id")).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), 6, "no row repeats across pages");
    assert_eq!(all[0].model, "newer");
    // Within the tied group the id order is strictly descending across pages.
    let tied: Vec<i64> = all.iter().filter(|r| r.ts == 100).map(|r| r.id.expect("id")).collect();
    assert!(tied.windows(2).all(|w| w[0] > w[1]), "tied rows in id-desc order: {tied:?}");
}

#[test]
fn usage_cursor_composes_with_hours_window() {
    let store = Store::open_in_memory().expect("mem");
    rt().block_on(async {
        for ts in [100, 200, 300, 400, 500] {
            store.record_usage(&page_row(ts, "x")).await.expect("insert");
        }
        // `since` keeps ts >= 200; the pair cursor then pages strictly past the ts-400 row.
        let at_400 = store
            .recent_usage(&UsageQuery { limit: 10, since: Some(400), ..Default::default() })
            .await
            .expect("400");
        let row_400 = at_400.iter().find(|r| r.ts == 400).expect("row 400").id.expect("id");
        let page = store
            .recent_usage(&UsageQuery {
                limit: 2,
                since: Some(200),
                before_ts: Some(400),
                before_id: Some(row_400),
                ..Default::default()
            })
            .await
            .expect("page");
        assert_eq!(page.iter().map(|r| r.ts).collect::<Vec<_>>(), vec![300, 200]);
        let none = store
            .recent_usage(&UsageQuery {
                limit: 10,
                since: Some(600),
                before_ts: Some(600),
                ..Default::default()
            })
            .await
            .expect("none");
        assert!(none.is_empty(), "cursor and window agree on empty");
        let below = store
            .recent_usage(&UsageQuery {
                limit: 10,
                since: Some(200),
                before_ts: Some(200),
                before_id: Some(0),
                ..Default::default()
            })
            .await
            .expect("below");
        assert!(below.is_empty(), "nothing in the window is older than ts 200: {below:?}");
    });
}

// ----- usage filters ----------------------------------------------------------------

/// An unfiltered query for the first `limit` rows.
fn query(limit: usize) -> UsageQuery {
    UsageQuery { limit, ..Default::default() }
}

/// A usage row whose provider, model, key, and outcome are what the filters inspect.
fn filter_row(
    ts: i64,
    provider: &str,
    model: &str,
    key: Option<Uuid>,
    status: u16,
    error: Option<&str>,
) -> UsageRecord {
    UsageRecord {
        provider: provider.to_string(),
        model: model.to_string(),
        key_id: key,
        status,
        error: error.map(str::to_string),
        ..page_row(ts, model)
    }
}

/// Timestamps of the rows a query returns, in page order.
fn ts_of(s: &Store, q: &UsageQuery) -> Vec<i64> {
    rt().block_on(s.recent_usage(q)).expect("recent").iter().map(|r| r.ts).collect()
}

/// Walks the filtered set with the `(ts, id)` cursor, `limit` rows at a time.
fn walk_filtered(s: &Store, limit: usize, base: UsageQuery) -> Vec<UsageRecord> {
    rt().block_on(async {
        let mut all = Vec::new();
        let mut q = UsageQuery { limit, ..base };
        loop {
            let page = s.recent_usage(&q).await.expect("page");
            let short = page.len() < limit;
            if let Some(last) = page.last() {
                q.before_ts = Some(last.ts);
                q.before_id = last.id;
            }
            all.extend(page);
            if short {
                break;
            }
        }
        all
    })
}

#[test]
fn usage_filters_select_provider_model_key_and_status() {
    let store = Store::open_in_memory().expect("mem");
    let key = Uuid::new_v4();
    let other = Uuid::new_v4();
    rt().block_on(async {
        let rows = [
            filter_row(10, "openai", "gpt-4.1", Some(key), 200, None),
            filter_row(20, "openai", "gpt-4.1", Some(other), 500, None),
            filter_row(30, "anthropic", "claude", Some(key), 200, Some("stream dropped")),
            filter_row(40, "openai", "gpt-4.1-mini", None, 200, None),
            filter_row(50, "openai", "openai/gpt-4.1", Some(key), 200, None),
        ];
        for r in &rows {
            store.record_usage(r).await.expect("insert");
        }
    });

    // Provider is an exact match.
    let openai = UsageQuery { provider: Some("openai".into()), ..query(10) };
    assert_eq!(ts_of(&store, &openai), vec![50, 40, 20, 10]);
    // Model alone matches the stored string exactly; the ref row (ts 50) is not `gpt-4.1`.
    let exact = UsageQuery { model: Some("gpt-4.1".into()), ..query(10) };
    assert_eq!(ts_of(&store, &exact), vec![20, 10]);
    // A ref matches itself exactly and its bare id.
    let both = UsageQuery {
        model: Some("openai/gpt-4.1".into()),
        bare_model: Some("gpt-4.1".into()),
        ..query(10)
    };
    assert_eq!(ts_of(&store, &both), vec![50, 20, 10]);
    // Key filter.
    let by_key = UsageQuery { key_id: Some(key), ..query(10) };
    assert_eq!(ts_of(&store, &by_key), vec![50, 30, 10]);
    // Error: status >= 400 or error text, even when the status is 2xx.
    let errors = UsageQuery { status: Some(StatusFilter::Error), ..query(10) };
    assert_eq!(ts_of(&store, &errors), vec![30, 20]);
    // Success: 2xx with no error text.
    let success = UsageQuery { status: Some(StatusFilter::Success), ..query(10) };
    assert_eq!(ts_of(&store, &success), vec![50, 40, 10]);
    // Combinations.
    let openai_key = UsageQuery { provider: Some("openai".into()), key_id: Some(key), ..query(10) };
    assert_eq!(ts_of(&store, &openai_key), vec![50, 10]);
    let key_error = UsageQuery { key_id: Some(key), status: Some(StatusFilter::Error), ..query(10) };
    assert_eq!(ts_of(&store, &key_error), vec![30]);
    let openai_error =
        UsageQuery { provider: Some("openai".into()), status: Some(StatusFilter::Error), ..query(10) };
    assert_eq!(ts_of(&store, &openai_error), vec![20]);
    let since_key = UsageQuery { since: Some(25), key_id: Some(key), ..query(10) };
    assert_eq!(ts_of(&store, &since_key), vec![50, 30]);
    let model_success_key = UsageQuery {
        model: Some("gpt-4.1".into()),
        key_id: Some(key),
        status: Some(StatusFilter::Success),
        ..query(10)
    };
    assert_eq!(ts_of(&store, &model_success_key), vec![10]);
    let mismatch = UsageQuery {
        provider: Some("anthropic".into()),
        model: Some("gpt-4.1".into()),
        ..query(10)
    };
    assert!(ts_of(&store, &mismatch).is_empty(), "provider and model must agree");
}

#[test]
fn usage_filters_compose_with_the_cursor() {
    let store = Store::open_in_memory().expect("mem");
    rt().block_on(async {
        // Odd positions fail; timestamps tie so the cursor's id half is exercised.
        for (i, ts) in [100, 100, 100, 200, 200, 300, 300, 300].into_iter().enumerate() {
            let status = if i % 2 == 1 { 500 } else { 200 };
            store
                .record_usage(&filter_row(ts, "openai", "gpt-4.1", None, status, None))
                .await
                .expect("insert");
        }
    });
    let errors = UsageQuery { status: Some(StatusFilter::Error), ..query(2) };
    let first = rt().block_on(store.recent_usage(&errors)).expect("first");
    assert_eq!(first.len(), 2, "a full filtered page");
    let last = first.last().expect("row");
    let second = rt()
        .block_on(store.recent_usage(&UsageQuery {
            before_ts: Some(last.ts),
            before_id: last.id,
            ..errors.clone()
        }))
        .expect("second");
    assert_eq!(second.len(), 2, "the filtered walk continues past the first page");
    let first_ids: Vec<i64> = first.iter().map(|r| r.id.expect("id")).collect();
    let second_ids: Vec<i64> = second.iter().map(|r| r.id.expect("id")).collect();
    assert!(first_ids.iter().all(|i| !second_ids.contains(i)), "pages are disjoint");
    assert!(first.iter().chain(&second).all(|r| r.status == 500), "every page stays filtered");

    // The filtered walk equals the unfiltered walk restricted to failures.
    let filtered: Vec<i64> = walk_filtered(
        &store,
        2,
        UsageQuery { status: Some(StatusFilter::Error), ..Default::default() },
    )
    .iter()
    .map(|r| r.id.expect("id"))
    .collect();
    let oracle: Vec<i64> = walk_filtered(&store, 3, Default::default())
        .iter()
        .filter(|r| r.status == 500)
        .map(|r| r.id.expect("id"))
        .collect();
    assert_eq!(filtered, oracle, "same rows, same order");
    assert_eq!(filtered.len(), 4);
}
