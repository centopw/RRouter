//! Model preset persistence: layout validation, CRUD, optimistic revisions, snapshots and
//! the per-user active selection.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::LazyLock;

use rrouter_store::{
    LayoutDocument, LayoutGroup, LayoutItem, PresetUpdateOutcome, Store, DEFAULT_MODEL_PRESET_ID,
    LAYOUT_SCHEMA_VERSION, MAX_LAYOUT_GROUPS, MAX_LAYOUT_GROUP_ID, MAX_LAYOUT_GROUP_NAME,
    MAX_LAYOUT_ITEMS, MAX_LAYOUT_MODEL_ID, MAX_LAYOUT_PROVIDER, MAX_PRESET_NAME,
    MAX_PRESET_VERSIONS, SCHEMA_VERSION,
};
use tokio::runtime::Runtime;
use uuid::Uuid;

// ----- harness ---------------------------------------------------------------------

static RT: LazyLock<Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("runtime")
});

fn rt() -> &'static Runtime {
    &RT
}

/// Minimal unique temp dir; `tempfile` is not a workspace dependency.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        static SEQ: AtomicU32 = AtomicU32::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("rrouter-presets-{tag}-{}-{n}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("temp dir");
        Self(dir)
    }

    fn db(&self) -> PathBuf {
        self.0.join("rrouter.db")
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

fn item(provider: &str, id: &str, position: u32, visible: bool) -> LayoutItem {
    LayoutItem {
        provider: provider.to_string(),
        id: id.to_string(),
        group: None,
        position,
        visible,
        last_visible_position: None,
        hidden_at: None,
    }
}

fn layout(items: Vec<LayoutItem>) -> LayoutDocument {
    LayoutDocument {
        schema_version: LAYOUT_SCHEMA_VERSION,
        auto_include_new: true,
        groups: Vec::new(),
        items,
    }
}

fn one(provider: &str, id: &str) -> LayoutDocument {
    layout(vec![item(provider, id, 0, true)])
}

// ----- layout document -------------------------------------------------------------

#[test]
fn layout_validation_accepts_dense_groups_and_rejects_broken_documents() {
    let valid = layout(vec![
        item("openai", "gpt-4.1", 0, true),
        item("anthropic", "claude", 1, true),
        item("google", "gemini", 0, false),
        item("x", "y", 1, false),
    ]);
    valid.validate().expect("dense visible and inactive groups");

    let mut wrong_version = valid.clone();
    wrong_version.schema_version = 2;
    assert!(
        wrong_version.validate().is_err(),
        "only schema 1 is accepted"
    );

    assert!(
        layout(vec![item("a", "b", 1, true)]).validate().is_err(),
        "a visible group must start at 0"
    );
    assert!(
        layout(vec![item("a", "b", 0, false), item("c", "d", 0, false)])
            .validate()
            .is_err(),
        "positions are unique inside a group"
    );
    assert!(
        layout(vec![item("a", "b", 0, true), item("c", "d", 1, false)])
            .validate()
            .is_err(),
        "inactive positions are numbered independently from 0"
    );
    assert!(
        layout(vec![
            item("openai", "gpt", 0, true),
            item("openai", "gpt", 0, false)
        ])
        .validate()
        .is_err(),
        "(provider, id) is unique across both groups"
    );
    assert!(
        layout(vec![item("   ", "gpt", 0, true)])
            .validate()
            .is_err(),
        "a provider is required"
    );
    assert!(
        layout(vec![item("openai", "  ", 0, true)])
            .validate()
            .is_err(),
        "a model id is required"
    );
    assert!(layout(vec![item(
        &"p".repeat(MAX_LAYOUT_PROVIDER + 1),
        "m",
        0,
        true
    )])
    .validate()
    .is_err());
    assert!(layout(vec![item(
        "p",
        &"m".repeat(MAX_LAYOUT_MODEL_ID + 1),
        0,
        true
    )])
    .validate()
    .is_err());

    layout(vec![item(" openai ", " gpt ", 0, true)])
        .validate()
        .expect("trimmed identities are valid");

    // Restore memory is optional in either group and accepted when in range.
    let remembered = LayoutDocument {
        items: vec![
            LayoutItem {
                provider: "openai".into(),
                id: "hidden".into(),
                group: None,
                position: 0,
                visible: false,
                last_visible_position: Some(MAX_LAYOUT_ITEMS as u32 - 1),
                hidden_at: Some(1_700_000_000),
            },
            LayoutItem {
                provider: "openai".into(),
                id: "visible".into(),
                group: None,
                position: 0,
                visible: true,
                last_visible_position: Some(0),
                hidden_at: None,
            },
        ],
        ..LayoutDocument::default()
    };
    remembered
        .validate()
        .expect("restore memory is accepted and not required");

    for out_of_range in [MAX_LAYOUT_ITEMS as u32, u32::MAX] {
        let broken = LayoutDocument {
            items: vec![LayoutItem {
                provider: "openai".into(),
                id: "hidden".into(),
                group: None,
                position: 0,
                visible: false,
                last_visible_position: Some(out_of_range),
                hidden_at: None,
            }],
            ..LayoutDocument::default()
        };
        assert!(
            broken.validate().is_err(),
            "last_visible_position {out_of_range} is outside a layout"
        );
    }
}

#[test]
fn layout_groups_are_unique_ordered_and_bound_to_providers() {
    let group = |id: &str, name: &str, provider: Option<&str>| LayoutGroup {
        id: id.to_string(),
        name: name.to_string(),
        provider: provider.map(str::to_string),
        visible: true,
        access: None,
    };
    let valid = LayoutDocument {
        schema_version: LAYOUT_SCHEMA_VERSION,
        auto_include_new: true,
        groups: vec![
            group("g-fast", "Fast", None),
            group("provider:openai", "OpenAI", Some("openai")),
        ],
        items: vec![
            LayoutItem {
                provider: "openai".into(),
                id: "gpt-4.1".into(),
                group: Some("g-fast".into()),
                position: 0,
                visible: true,
                last_visible_position: None,
                hidden_at: None,
            },
            LayoutItem {
                provider: "openai".into(),
                id: "gpt-5".into(),
                group: None,
                position: 1,
                visible: true,
                last_visible_position: None,
                hidden_at: None,
            },
        ],
    };
    valid
        .validate()
        .expect("explicit custom and provider groups");

    let mut duplicate = valid.clone();
    duplicate.groups.push(group("g-fast", "Again", None));
    assert!(duplicate.validate().is_err(), "group ids are unique");

    let mut duplicate_provider = valid.clone();
    duplicate_provider
        .groups
        .push(group("provider:openai-2", "Again", Some("openai")));
    assert!(
        duplicate_provider.validate().is_err(),
        "one bucket per provider, and the id is fixed"
    );

    let mut reserved = valid.clone();
    reserved
        .groups
        .push(group("provider:manual", "Manual", None));
    assert!(reserved.validate().is_err(), "provider: is reserved");

    let mut unknown = valid.clone();
    unknown.items[0].group = Some("missing".into());
    assert!(unknown.validate().is_err(), "a group reference must exist");

    let mut foreign = valid.clone();
    foreign.items[0].provider = "anthropic".into();
    foreign.items[0].group = Some("provider:openai".into());
    assert!(
        foreign.validate().is_err(),
        "a model cannot join another provider's bucket"
    );

    let mut blank = valid.clone();
    blank.groups[0].name = "  ".into();
    assert!(blank.validate().is_err(), "a group needs a name");

    let mut long = valid.clone();
    long.groups[0].name = "g".repeat(MAX_LAYOUT_GROUP_NAME + 1);
    assert!(long.validate().is_err(), "a group name is bounded");

    let mut long_id = valid.clone();
    long_id.groups[0].id = "g".repeat(MAX_LAYOUT_GROUP_ID + 1);
    assert!(long_id.validate().is_err(), "a group id is bounded");

    let mut too_many = valid.clone();
    too_many.groups = (0..=MAX_LAYOUT_GROUPS)
        .map(|i| group(&format!("g-{i}"), "g", None))
        .collect();
    assert!(too_many.validate().is_err(), "the group count is bounded");

    // Array order is the display order and is not rewritten by validation.
    let ordered = valid.clone();
    assert_eq!(ordered.groups[0].id, "g-fast");
    assert_eq!(ordered.groups[1].id, "provider:openai");
}

#[test]
fn layout_item_limit_is_enforced_at_the_boundary() {
    let at_limit = LayoutDocument {
        items: (0..MAX_LAYOUT_ITEMS)
            .map(|i| item("p", &format!("m{i}"), i as u32, true))
            .collect(),
        ..LayoutDocument::default()
    };
    at_limit.validate().expect("exactly at the limit");

    let mut over_limit = at_limit.clone();
    over_limit
        .items
        .push(item("p", "extra", MAX_LAYOUT_ITEMS as u32, true));
    assert!(over_limit.validate().is_err());
}

#[test]
fn layout_json_defaults_missing_optional_fields() {
    let doc: LayoutDocument = serde_json::from_str(
        r#"{"schema_version":1,"items":[{"provider":"openai","id":"gpt-4.1","position":0}]}"#,
    )
    .expect("optional fields are optional");
    assert!(doc.auto_include_new, "missing means merge the live catalog");
    assert!(doc.items[0].visible, "a listed item is visible by default");
    assert_eq!(doc.items[0].position, 0);
    assert_eq!(doc.items[0].last_visible_position, None);
    assert_eq!(doc.items[0].hidden_at, None);

    let remembered: LayoutDocument = serde_json::from_str(
        r#"{"schema_version":1,"items":[{"provider":"openai","id":"gpt-4.1","position":0,
            "visible":false,"last_visible_position":7,"hidden_at":1700000000}]}"#,
    )
    .expect("restore memory deserializes");
    let hidden = &remembered.items[0];
    assert_eq!(hidden.last_visible_position, Some(7));
    assert_eq!(hidden.hidden_at, Some(1_700_000_000));
    assert_eq!(
        serde_json::from_str::<LayoutItem>(&serde_json::to_string(hidden).expect("serialize"))
            .expect("round trip"),
        hidden.clone(),
        "restore memory survives a serialize/deserialize round trip"
    );

    let plain = serde_json::to_value(item("openai", "gpt-4.1", 0, true)).expect("serialize");
    assert!(
        plain.get("last_visible_position").is_none() && plain.get("hidden_at").is_none(),
        "empty restore memory stays out of the document"
    );

    let sparse: LayoutDocument =
        serde_json::from_str(r#"{"schema_version":1}"#).expect("items default to empty");
    assert!(sparse.auto_include_new);
    assert!(sparse.items.is_empty());

    assert!(
        serde_json::from_str::<LayoutDocument>(r#"{"items":[]}"#).is_err(),
        "schema_version is required, not assumed"
    );
    assert_eq!(
        serde_json::to_string(&LayoutDocument::default()).expect("serialize"),
        r#"{"schema_version":1,"auto_include_new":true,"items":[]}"#
    );
}

// ----- CRUD --------------------------------------------------------------------------

#[test]
fn preset_crud_covers_meta_duplicate_default_and_delete() {
    let store = Store::open_in_memory().expect("mem");
    rt().block_on(async {
        let seed = store
            .default_model_preset()
            .await
            .expect("default")
            .expect("v30 seeds the default");
        assert_eq!(seed.id, DEFAULT_MODEL_PRESET_ID);
        assert_eq!(seed.name, "Default");
        assert!(seed.is_default);
        assert_eq!(seed.revision, 1);
        assert_eq!(seed.layout, LayoutDocument::default());
        assert_eq!(
            store.active_model_preset(None).await.expect("active").id,
            seed.id
        );
        assert_eq!(
            store
                .active_model_preset(Some(Uuid::new_v4()))
                .await
                .expect("an unknown user falls back")
                .id,
            seed.id
        );

        let doc = one("openai", "gpt-4.1");
        let preset = store
            .create_model_preset("Focus", Some("daily driver"), &doc, "admin:1")
            .await
            .expect("create");
        assert_eq!(preset.revision, 1);
        assert_eq!(preset.scope, "global");
        assert!(!preset.is_default);
        assert_eq!(preset.created_by.as_deref(), Some("admin:1"));
        assert_eq!(
            store.get_model_preset(preset.id).await.expect("get"),
            Some(preset.clone())
        );
        let listed = store.list_model_presets().await.expect("list");
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].id, seed.id, "the default sorts first");

        let renamed = store
            .update_model_preset_meta(preset.id, Some("Focus 2"), Some(None), "admin:1")
            .await
            .expect("meta")
            .expect("exists");
        assert_eq!(renamed.name, "Focus 2");
        assert_eq!(renamed.description, None, "Some(None) clears");
        assert_eq!(renamed.revision, 1, "metadata does not bump the revision");
        let described = store
            .update_model_preset_meta(preset.id, None, Some(Some("  newer  ")), "admin:1")
            .await
            .expect("meta")
            .expect("exists");
        assert_eq!(described.name, "Focus 2", "absent fields keep their value");
        assert_eq!(described.description.as_deref(), Some("newer"));
        assert!(store
            .update_model_preset_meta(Uuid::new_v4(), None, None, "admin:1")
            .await
            .expect("meta")
            .is_none());

        let copy = store
            .duplicate_model_preset(preset.id, "Focus copy", "admin:2")
            .await
            .expect("duplicate");
        assert_ne!(copy.id, preset.id);
        assert_eq!(copy.revision, 1);
        assert_eq!(copy.layout, described.layout);
        assert_eq!(copy.description.as_deref(), Some("newer"));
        assert_eq!(copy.created_by.as_deref(), Some("admin:2"));
        assert!(!copy.is_default);

        assert!(
            store.delete_model_preset(seed.id, "admin:1").await.is_err(),
            "the default cannot be deleted"
        );
        assert!(store
            .get_model_preset(seed.id)
            .await
            .expect("get")
            .is_some());

        let promoted = store
            .set_default_model_preset(preset.id, "admin:1")
            .await
            .expect("promote");
        assert!(promoted.is_default);
        assert!(
            !store
                .get_model_preset(seed.id)
                .await
                .expect("get")
                .expect("still exists")
                .is_default,
            "the previous default is demoted"
        );
        assert_eq!(
            store
                .default_model_preset()
                .await
                .expect("default")
                .expect("promoted default")
                .id,
            preset.id
        );

        assert!(store
            .delete_model_preset(copy.id, "admin:1")
            .await
            .expect("delete"));
        assert!(store
            .get_model_preset(copy.id)
            .await
            .expect("get")
            .is_none());
        assert!(
            !store
                .delete_model_preset(copy.id, "admin:1")
                .await
                .expect("idempotent"),
            "a second delete reports no row"
        );
        assert_eq!(
            store
                .model_preset_versions(copy.id)
                .await
                .expect("versions")
                .len(),
            0,
            "a delete removes its snapshots"
        );
    });
}

#[test]
fn mutations_validate_names_actors_and_layouts() {
    let store = Store::open_in_memory().expect("mem");
    rt().block_on(async {
        let doc = LayoutDocument::default();
        assert!(store
            .create_model_preset("   ", None, &doc, "admin")
            .await
            .is_err());
        assert!(store
            .create_model_preset(&"n".repeat(MAX_PRESET_NAME + 1), None, &doc, "admin")
            .await
            .is_err());
        assert!(store
            .create_model_preset("x", None, &doc, "   ")
            .await
            .is_err());

        let bad = LayoutDocument {
            schema_version: 2,
            ..LayoutDocument::default()
        };
        assert!(store
            .create_model_preset("x", None, &bad, "admin")
            .await
            .is_err());

        let preset = store
            .create_model_preset("ok", Some("  "), &doc, "admin")
            .await
            .expect("create");
        assert_eq!(preset.description, None, "blank descriptions are empty");
        assert!(store
            .update_model_preset_meta(preset.id, Some("  "), None, "admin")
            .await
            .is_err());
        assert!(store
            .update_model_preset_layout(preset.id, 1, &bad, "admin")
            .await
            .is_err());
        assert!(store
            .update_model_preset_layout(preset.id, 1, &doc, "  ")
            .await
            .is_err());
    });
}

// ----- revisions and history ---------------------------------------------------------

#[test]
fn layout_update_conflicts_leave_the_old_document_in_place() {
    let store = Store::open_in_memory().expect("mem");
    rt().block_on(async {
        let first = one("openai", "gpt-4.1");
        let preset = store
            .create_model_preset("Focus", None, &first, "admin")
            .await
            .expect("create");
        let second = one("anthropic", "claude");
        let outcome = store
            .update_model_preset_layout(preset.id, 1, &second, "editor")
            .await
            .expect("update");
        let PresetUpdateOutcome::Updated(updated) = outcome else {
            panic!("expected an update, got {outcome:?}");
        };
        assert_eq!(updated.revision, 2);
        assert_eq!(updated.layout, second);

        let stale = one("google", "gemini");
        let conflict = store
            .update_model_preset_layout(preset.id, 1, &stale, "editor")
            .await
            .expect("conflict is an outcome, not an error");
        assert_eq!(
            conflict,
            PresetUpdateOutcome::Conflict {
                current_revision: 2
            }
        );
        let current = store
            .get_model_preset(preset.id)
            .await
            .expect("get")
            .expect("exists");
        assert_eq!(current.revision, 2, "a stale write changes nothing");
        assert_eq!(current.layout, second, "a stale write leaves the layout");

        let versions = store
            .model_preset_versions(preset.id)
            .await
            .expect("versions");
        assert_eq!(
            versions.iter().map(|v| v.revision).collect::<Vec<_>>(),
            vec![2, 1],
            "newest first, one snapshot per accepted revision"
        );
        assert_eq!(versions[0].layout, second);
        assert_eq!(versions[0].created_by.as_deref(), Some("editor"));
        assert_eq!(versions[1].layout, first);
        assert_eq!(versions[1].created_by.as_deref(), Some("admin"));

        assert_eq!(
            store
                .update_model_preset_layout(Uuid::new_v4(), 1, &stale, "editor")
                .await
                .expect("not found"),
            PresetUpdateOutcome::NotFound
        );
    });
}

#[test]
fn layout_history_keeps_the_newest_twenty_snapshots() {
    let store = Store::open_in_memory().expect("mem");
    rt().block_on(async {
        let preset = store
            .create_model_preset("Focus", None, &LayoutDocument::default(), "admin")
            .await
            .expect("create");
        let mut last = LayoutDocument::default();
        for expected in 1..=25 {
            last = one("openai", &format!("m{expected}"));
            let outcome = store
                .update_model_preset_layout(preset.id, expected, &last, "editor")
                .await
                .expect("update");
            assert!(matches!(outcome, PresetUpdateOutcome::Updated(_)));
        }

        let versions = store
            .model_preset_versions(preset.id)
            .await
            .expect("versions");
        assert_eq!(versions.len() as i64, MAX_PRESET_VERSIONS);
        assert_eq!(versions.first().expect("newest").revision, 26);
        assert_eq!(
            versions.last().expect("oldest kept").revision,
            26 - MAX_PRESET_VERSIONS + 1
        );
        assert_eq!(versions.first().expect("newest").layout, last);
        assert_eq!(
            store
                .get_model_preset(preset.id)
                .await
                .expect("get")
                .expect("exists")
                .revision,
            26
        );
    });
}

// ----- active selection --------------------------------------------------------------

#[test]
fn active_preset_prefers_the_users_selection_then_falls_back_to_default() {
    let store = Store::open_in_memory().expect("mem");
    rt().block_on(async {
        let seed = store
            .default_model_preset()
            .await
            .expect("default")
            .expect("seeded");
        let custom = store
            .create_model_preset("Custom", None, &one("openai", "gpt-4.1"), "admin")
            .await
            .expect("create");
        let user = Uuid::new_v4();

        assert_eq!(
            store
                .active_model_preset(Some(user))
                .await
                .expect("fallback")
                .id,
            seed.id
        );
        store
            .set_active_model_preset(user, custom.id)
            .await
            .expect("select");
        assert_eq!(
            store
                .active_model_preset(Some(user))
                .await
                .expect("selected")
                .id,
            custom.id
        );
        store
            .set_active_model_preset(user, seed.id)
            .await
            .expect("reselect");
        assert_eq!(
            store
                .active_model_preset(Some(user))
                .await
                .expect("reselect")
                .id,
            seed.id
        );
        assert!(
            store
                .set_active_model_preset(user, Uuid::new_v4())
                .await
                .is_err(),
            "a selection must name a real preset"
        );

        store
            .set_active_model_preset(user, custom.id)
            .await
            .expect("select again");
        store
            .delete_model_preset(custom.id, "admin")
            .await
            .expect("delete selected");
        assert_eq!(
            store
                .active_model_preset(Some(user))
                .await
                .expect("deleted selection falls back")
                .id,
            seed.id
        );
    });
}

// ----- legacy upgrade ----------------------------------------------------------------

#[test]
fn v29_database_upgrades_to_v30_and_seeds_the_default() {
    let dir = TempDir::new("v29");
    // Build the current schema, then remove exactly what v30 adds so the file is what a
    // v29 build shipped.
    drop(Store::open(&dir.db(), None).expect("current schema"));
    {
        let conn = rusqlite::Connection::open(dir.db()).expect("open raw");
        conn.execute_batch(
            "DROP TABLE IF EXISTS quota_ping_samples; \
             DROP TABLE IF EXISTS quota_pings; \
             DROP TABLE IF EXISTS model_preferences; \
             DROP TABLE IF EXISTS user_model_preferences; \
             DROP TABLE IF EXISTS model_preset_versions; \
             DROP TABLE IF EXISTS model_presets; \
             ALTER TABLE combos DROP COLUMN strategy; \
             ALTER TABLE combos DROP COLUMN router_model; \
             ALTER TABLE combos DROP COLUMN cursor; \
             ALTER TABLE eval_suites DROP COLUMN system_prompt; \
             ALTER TABLE eval_runs DROP COLUMN judge_model; \
             ALTER TABLE api_keys DROP COLUMN owner_user_id; \
             PRAGMA user_version = 29;",
        )
        .expect("rewind to v29");
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .expect("version");
        assert_eq!(version, 29);
    }

    let store = Store::open(&dir.db(), None).expect("open runs the v30 step");
    let preset = rt()
        .block_on(store.default_model_preset())
        .expect("default")
        .expect("the upgrade seeds a default");
    assert_eq!(preset.id, DEFAULT_MODEL_PRESET_ID);
    assert_eq!(preset.name, "Default");
    assert!(preset.is_default);
    assert_eq!(preset.revision, 1);
    assert_eq!(preset.layout, LayoutDocument::default());
    assert_eq!(
        rt().block_on(store.active_model_preset(None))
            .expect("active")
            .id,
        DEFAULT_MODEL_PRESET_ID
    );
    drop(store);

    let conn = rusqlite::Connection::open(dir.db()).expect("reopen");
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .expect("version");
    assert_eq!(version, SCHEMA_VERSION);
    assert_eq!(version, 35);
}
