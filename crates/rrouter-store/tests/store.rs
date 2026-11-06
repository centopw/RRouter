//! Integration tests for the SQLite + AES-256-GCM store.
//!
//! Every test that exercises two writes opens a real file-backed database (`TempDir`), since
//! the plaintext-leak and key-file assertions read the raw DB bytes back off disk.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::LazyLock;

use rrouter_store::{
    ApiKey, Credentials, Store, UsageRecord, API_KEY_PREFIX, KEY_FILE_NAME,
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

    // 20 tasks: 9 distinct optional string fields + 11 distinct `extra` keys, so every
    // task patches something no sibling touches. A lost read-merge-write drops one.
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
            ts,
            provider: provider.to_string(),
            model: model.to_string(),
            connection_id: Some(conn_id),
            input_tokens: inp,
            output_tokens: out,
            latency_ms: ms,
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

        let recent = s.recent_usage(3).await.expect("recent");
        assert_eq!(recent.len(), 3);
        assert_eq!(recent[0].ts, 400, "newest first");
        assert_eq!(recent[1].ts, 300);
        assert_eq!(recent[2].ts, 200);
        assert_eq!(recent[2].status, 429);
        assert!(recent[2].streamed);
        assert_eq!(recent[2].error.as_deref(), Some("upstream 429"));
        assert_eq!(recent[0].connection_id, Some(conn_id));

        let sum = s.usage_summary().await.expect("summary");
        assert_eq!(sum.len(), 2);
        let claude = &sum[0];
        assert_eq!((claude.provider.as_str(), claude.model.as_str()), ("claude", "sonnet"));
        assert_eq!(claude.requests, 3);
        assert_eq!(claude.errors, 1, "status >= 400 counts as an error");
        assert_eq!(claude.input_tokens, 40);
        assert_eq!(claude.output_tokens, 60);
        assert_eq!(claude.avg_latency_ms, 200, "(100+300+200)/3");
        let groq = &sum[1];
        assert_eq!((groq.requests, groq.errors, groq.avg_latency_ms), (1, 0, 400));
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
