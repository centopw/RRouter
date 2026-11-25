//! Free-mode persistence: the discovery cache and the single imported document.
//!
//! These pin the two properties the routing and import paths lean on — a re-discovery
//! replaces a row in place (never a second row per model) and the stored document survives
//! a reopen unchanged — so a later schema or SQL change cannot quietly break idempotency.

use rrouter_store::{FreeModel, Store};
use serde_json::{json, Value};
use tokio::runtime::Runtime;
use uuid::Uuid;

/// One runtime for the binary; the store calls are async wrappers over a sync mutex.
fn rt() -> &'static Runtime {
    static RT: std::sync::LazyLock<Runtime> = std::sync::LazyLock::new(|| {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
    });
    &RT
}

fn row(provider: &str, model: &str, free: bool, price: Option<f64>, source: &str) -> FreeModel {
    FreeModel {
        provider: provider.to_string(),
        model: model.to_string(),
        free,
        price,
        source: source.to_string(),
        connection_id: None,
        discovered_at: 1_000,
    }
}

#[test]
fn discovery_rows_upsert_in_place() {
    let store = Store::open_in_memory().expect("mem");
    let conn = Uuid::new_v4();

    let mut first = row(
        "openrouter",
        "deepseek-r1:free",
        true,
        Some(0.0),
        "provider-list",
    );
    first.connection_id = Some(conn);
    rt().block_on(store.upsert_free_model(&first))
        .expect("insert");

    let mut second = row(
        "openrouter",
        "deepseek-r1:free",
        false,
        Some(2.5),
        "registry",
    );
    second.discovered_at = 2_000;
    rt().block_on(store.upsert_free_model(&second))
        .expect("update");

    let rows = rt().block_on(store.list_free_models()).expect("list");
    assert_eq!(
        rows.len(),
        1,
        "one row per (provider, model), never history"
    );
    assert!(!rows[0].free, "the later discovery wins");
    assert_eq!(rows[0].price, Some(2.5));
    assert_eq!(rows[0].source, "registry");
    assert_eq!(rows[0].discovered_at, 2_000);
    assert_eq!(
        rows[0].connection_id, None,
        "a refresh without an account must clear the one that proved the old row"
    );
}

#[test]
fn rows_are_listed_in_a_stable_order_and_deleted_by_provider() {
    let store = Store::open_in_memory().expect("mem");
    for (provider, model) in [
        ("openrouter", "b:free"),
        ("groq", "a:free"),
        ("openrouter", "a:free"),
    ] {
        rt().block_on(store.upsert_free_model(&row(provider, model, true, None, "provider-list")))
            .expect("upsert");
    }

    let listed: Vec<(String, String)> = rt()
        .block_on(store.list_free_models())
        .expect("list")
        .into_iter()
        .map(|m| (m.provider, m.model))
        .collect();
    assert_eq!(
        listed,
        vec![
            ("groq".to_string(), "a:free".to_string()),
            ("openrouter".to_string(), "a:free".to_string()),
            ("openrouter".to_string(), "b:free".to_string()),
        ],
        "provider then model, so a plan built from these rows is deterministic"
    );

    let removed = rt()
        .block_on(store.delete_free_models(Some("openrouter")))
        .expect("delete one provider");
    assert_eq!(removed, 2);
    assert_eq!(
        rt().block_on(store.list_free_models()).expect("list").len(),
        1
    );

    let removed = rt()
        .block_on(store.delete_free_models(None))
        .expect("delete all");
    assert_eq!(removed, 1);
}

#[test]
fn the_document_round_trips_verbatim_and_clears_with_discovery() {
    let store = Store::open_in_memory().expect("mem");
    assert!(
        rt().block_on(store.free_mode_config())
            .expect("read")
            .is_none(),
        "nothing imported yet"
    );

    let document = json!({
        "version": 1,
        "routing": {"enabled": true, "per_account_rpm": 10},
        "providers": [{"provider": "openrouter", "accounts": [{"label": "a", "proxy": "p1"}]}],
        // A key this build does not read must survive a re-save untouched.
        "future_field": {"nested": [1, 2, 3]}
    });
    let routing = json!({"enabled": true, "prefer": "spread", "max_attempts": 8});
    rt().block_on(store.save_free_mode_config(&document, &routing))
        .expect("save");

    let stored = rt()
        .block_on(store.free_mode_config())
        .expect("read")
        .expect("saved");
    assert_eq!(stored.document, document);
    assert_eq!(stored.routing, routing);

    // Saving again replaces the row rather than adding one: `free_mode` holds exactly one.
    let routing = json!({"enabled": false, "prefer": "cheapest", "max_attempts": 4});
    rt().block_on(store.save_free_mode_config(&document, &routing))
        .expect("re-save");
    let stored = rt()
        .block_on(store.free_mode_config())
        .expect("read")
        .expect("saved");
    assert_eq!(stored.routing, routing);

    rt().block_on(store.upsert_free_model(&row("openrouter", "x:free", true, None, "document")))
        .expect("row");
    let catalog = json!([{"provider": "openrouter", "url": "u", "discovered_at": 5, "models": 1,
                          "usable": 1, "error": null}]);
    rt().block_on(store.save_free_mode_config(
        &document,
        &json!({
            "enabled": true,
            "prefer": "cheapest",
            "max_attempts": 4,
            "catalogs": catalog,
            "probes": {"a/b": {"at": 1}},
        }),
    ))
    .expect("routing metadata");
    rt().block_on(store.clear_free_mode()).expect("clear");

    let stored = rt()
        .block_on(store.free_mode_config())
        .expect("read")
        .expect("teardown keeps the row");
    assert_eq!(
        stored.document,
        Value::Null,
        "teardown forgets the document"
    );
    assert_eq!(
        stored.routing,
        json!({"enabled": true, "prefer": "cheapest", "max_attempts": 4, "teardown": true}),
        "teardown keeps the operator's knobs, drops discovery metadata and marks the row"
    );
    assert_eq!(
        rt().block_on(store.list_free_models()).expect("list").len(),
        0,
        "teardown forgets discovery"
    );

    rt().block_on(store.clear_free_mode_teardown())
        .expect("ask again");
    let stored = rt()
        .block_on(store.free_mode_config())
        .expect("read")
        .expect("row");
    assert_eq!(
        stored.routing,
        json!({"enabled": true, "prefer": "cheapest", "max_attempts": 4}),
        "an explicit discovery clears the teardown marker and nothing else"
    );
    assert_eq!(stored.document, Value::Null);
}
