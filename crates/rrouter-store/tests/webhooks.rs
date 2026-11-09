//! Webhook endpoints and the delivery outbox at the store layer: validation, idempotent
//! enqueue, the claim/complete/fail state machine, leases, and pruning.
//!
//! The HTTP surface and the signed delivery live in `rrouter-gateway/tests/webhooks.rs`;
//! this file is about the invariants the worker depends on.

use rrouter_store::{OutboxStatus, Store, WebhookPatch, DELIVERY_LEASE_SECS};
use tokio::runtime::Runtime;

fn store() -> Store {
    Store::open_in_memory().expect("store")
}

fn rt() -> Runtime {
    Runtime::new().expect("runtime")
}

fn types(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| name.to_string()).collect()
}

#[test]
fn endpoint_round_trips_and_validates_its_input() {
    let rt = rt();
    rt.block_on(async {
        let store = store();
        let row = store
            .create_webhook(
                " https://example.test/hook ",
                types(&["key.created", "key.created", "team.*"]),
                Some("RR_HOOK_SECRET"),
                100,
            )
            .await
            .expect("create");
        assert_eq!(row.url, "https://example.test/hook", "trimmed");
        assert_eq!(
            row.event_types,
            types(&["key.created", "team.*"]),
            "deduped"
        );
        assert_eq!(row.secret_env.as_deref(), Some("RR_HOOK_SECRET"));
        assert!(row.enabled, "live on creation");
        assert_eq!(row.updated_at, None);

        assert!(
            store
                .create_webhook(
                    "http://example.test/hook",
                    types(&["key.created"]),
                    Some("S"),
                    100
                )
                .await
                .is_err(),
            "plain http off loopback is refused"
        );
        assert!(
            store
                .create_webhook(
                    "http://127.0.0.1:9000/hook",
                    types(&["key.created"]),
                    Some("S"),
                    100
                )
                .await
                .is_ok(),
            "a loopback receiver is allowed for local use and tests"
        );
        assert!(
            store
                .create_webhook(
                    "https://example.test/hook",
                    types(&["key.created"]),
                    None,
                    100
                )
                .await
                .is_err(),
            "a duplicate url is refused"
        );
        assert!(
            store
                .create_webhook("https://example.test/other", vec![], Some("S"), 100)
                .await
                .is_err(),
            "at least one event type"
        );
        assert!(
            store
                .create_webhook(
                    "https://example.test/other",
                    types(&["Bad Name"]),
                    Some("S"),
                    100
                )
                .await
                .is_err(),
            "an event type is a wire name"
        );
        assert!(
            store
                .create_webhook(
                    "https://example.test/other",
                    types(&["key.created"]),
                    Some(""),
                    100
                )
                .await
                .is_err(),
            "an empty secret_env names nothing"
        );
        assert!(
            store
                .create_webhook(
                    "https://user:pw@example.test/x",
                    types(&["key.created"]),
                    Some("S"),
                    100
                )
                .await
                .is_err(),
            "credentials in the url are refused"
        );

        let listed = store.list_webhooks().await.expect("list");
        assert_eq!(listed.len(), 2);

        let patched = store
            .update_webhook(
                row.id,
                &WebhookPatch {
                    enabled: Some(false),
                    event_types: Some(types(&["team.created"])),
                    ..WebhookPatch::default()
                },
                200,
            )
            .await
            .expect("update")
            .expect("exists");
        assert!(!patched.enabled);
        assert_eq!(patched.event_types, types(&["team.created"]));
        assert_eq!(patched.updated_at, Some(200));
        assert_eq!(patched.created_at, 100);

        // A partial edit cannot produce an invalid endpoint.
        assert!(store
            .update_webhook(
                row.id,
                &WebhookPatch {
                    url: Some("ftp://example.test/hook".into()),
                    ..WebhookPatch::default()
                },
                300,
            )
            .await
            .is_err());

        assert!(store.delete_webhook(row.id).await.expect("delete"));
        assert!(!store.delete_webhook(row.id).await.expect("delete again"));
        assert_eq!(store.list_webhooks().await.expect("list").len(), 1);
    });
}

#[test]
fn subscription_filter_ignores_disabled_and_unrelated_endpoints() {
    let rt = rt();
    rt.block_on(async {
        let store = store();
        let exact = store
            .create_webhook(
                "https://example.test/a",
                types(&["key.created"]),
                Some("S"),
                100,
            )
            .await
            .expect("a");
        let wildcard = store
            .create_webhook("https://example.test/b", types(&["*"]), Some("S"), 100)
            .await
            .expect("b");
        let other = store
            .create_webhook(
                "https://example.test/c",
                types(&["team.created"]),
                Some("S"),
                100,
            )
            .await
            .expect("c");
        let disabled = store
            .create_webhook(
                "https://example.test/d",
                types(&["key.created"]),
                Some("S"),
                100,
            )
            .await
            .expect("d");
        store
            .update_webhook(
                disabled.id,
                &WebhookPatch {
                    enabled: Some(false),
                    ..WebhookPatch::default()
                },
                101,
            )
            .await
            .expect("disable");

        let matched = store.webhooks_for("key.created").await.expect("filter");
        let ids: Vec<_> = matched.iter().map(|row| row.id).collect();
        assert_eq!(ids, vec![exact.id, wildcard.id]);
        assert!(!ids.contains(&other.id));
        assert!(!ids.contains(&disabled.id));
    });
}

#[test]
fn enqueue_is_idempotent_on_the_event_id() {
    let rt = rt();
    rt.block_on(async {
        let store = store();
        assert!(store
            .enqueue_event("evt-1", "key.created", "{}", 100)
            .await
            .expect("first"));
        assert!(!store
            .enqueue_event("evt-1", "key.created", "{}", 101)
            .await
            .expect("duplicate"));
        let status = store.outbox_status().await.expect("status");
        assert_eq!(
            status,
            OutboxStatus {
                pending: 1,
                ..OutboxStatus::default()
            }
        );
        assert!(store
            .enqueue_event("", "key.created", "{}", 100)
            .await
            .is_err());
        assert!(store.enqueue_event("evt-2", "", "{}", 100).await.is_err());
        assert!(store
            .enqueue_event("evt-2", "key.created", "", 100)
            .await
            .is_err());
    });
}

#[test]
fn claim_leases_and_complete_settles_once() {
    let rt = rt();
    rt.block_on(async {
        let store = store();
        store
            .enqueue_event("evt-a", "key.created", "{}", 100)
            .await
            .expect("a");
        store
            .enqueue_event("evt-b", "key.created", "{}", 900)
            .await
            .expect("future");

        let claimed = store.claim_due_events(100, 8).await.expect("claim");
        assert_eq!(claimed.len(), 1, "the future row is not due");
        assert_eq!(claimed[0].id, "evt-a");
        assert_eq!(claimed[0].status, "delivering");
        assert_eq!(claimed[0].attempts, 1, "an attempt is counted at claim");
        assert_eq!(claimed[0].next_attempt_at, 100 + DELIVERY_LEASE_SECS);

        assert!(store
            .claim_due_events(100, 8)
            .await
            .expect("claim")
            .is_empty());

        // A crash while delivering is reclaimable once the lease expires, and the retry
        // still counts as an attempt.
        assert_eq!(
            store
                .recover_stale_events(100 + DELIVERY_LEASE_SECS)
                .await
                .expect("recover"),
            1
        );
        let reclaimed = store
            .claim_due_events(100 + DELIVERY_LEASE_SECS, 8)
            .await
            .expect("reclaim");
        assert_eq!(reclaimed.len(), 1);
        assert_eq!(reclaimed[0].attempts, 2);

        assert!(store.complete_event("evt-a", 300).await.expect("complete"));
        assert!(!store
            .complete_event("evt-a", 301)
            .await
            .expect("complete twice"));
        let status = store.outbox_status().await.expect("status");
        assert_eq!(status.delivered, 1);
        assert_eq!(status.pending, 1, "evt-b is still waiting");
    });
}

#[test]
fn failures_back_off_then_become_dead_at_the_attempt_cap() {
    let rt = rt();
    rt.block_on(async {
        let store = store();
        store
            .enqueue_event("evt-f", "key.created", "{}", 100)
            .await
            .expect("enqueue");

        let mut now = 100;
        let mut last_error = None;
        for attempt in 1..=3u32 {
            let claimed = store.claim_due_events(now, 8).await.expect("claim");
            assert_eq!(claimed.len(), 1, "attempt {attempt} is due at {now}");
            assert_eq!(claimed[0].attempts, attempt);
            let dead = store
                .fail_event("evt-f", now + 10, "endpoint answered 500", 3)
                .await
                .expect("fail");
            if attempt < 3 {
                assert_eq!(dead, Some(false));
                let next = store.recent_events(1).await.expect("rows")[0].next_attempt_at;
                assert_eq!(next, now + 10);
                now += 10;
            } else {
                assert_eq!(dead, Some(true), "the third attempt is terminal");
            }
            last_error = store.recent_events(1).await.expect("rows")[0]
                .last_error
                .clone();
        }
        assert_eq!(last_error.as_deref(), Some("endpoint answered 500"));
        assert_eq!(
            store
                .recover_stale_events(now + 1_000)
                .await
                .expect("recover"),
            0,
            "a dead row is never reclaimed"
        );
        assert!(store
            .claim_due_events(now + 1_000, 8)
            .await
            .expect("claim")
            .is_empty());
    });
}

#[test]
fn pruning_keeps_unfinished_work() {
    let rt = rt();
    rt.block_on(async {
        let store = store();
        store
            .enqueue_event("old-done", "key.created", "{}", 100)
            .await
            .expect("a");
        store
            .enqueue_event("old-pending", "key.created", "{}", 100)
            .await
            .expect("b");
        let claimed = store.claim_due_events(100, 8).await.expect("claim");
        let done = claimed
            .iter()
            .find(|row| row.id == "old-done")
            .expect("done row");
        store.complete_event(&done.id, 100).await.expect("complete");

        assert_eq!(store.prune_events(101).await.expect("prune"), 1);
        let rows = store.recent_events(10).await.expect("rows");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "old-pending");
    });
}

#[test]
fn an_unmatched_event_is_terminal_and_never_counted_as_delivered() {
    let rt = rt();
    rt.block_on(async {
        let store = store();
        store
            .enqueue_event("evt-none", "team.created", "{}", 100)
            .await
            .expect("enqueue");
        let claimed = store.claim_due_events(100, 8).await.expect("claim");
        assert_eq!(claimed.len(), 1);

        assert!(store
            .unmatched_event("evt-none", "no enabled subscriber for this event type")
            .await
            .expect("unmatched"));
        assert!(!store
            .unmatched_event("evt-none", "again")
            .await
            .expect("second settles nothing"));

        let status = store.outbox_status().await.expect("status");
        assert_eq!(status.unmatched, 1);
        assert_eq!(status.delivered, 0, "nothing was sent");
        assert_eq!(status.dead, 0);

        let rows = store.recent_events(1).await.expect("rows");
        assert_eq!(rows[0].status, "unmatched");
        assert_eq!(rows[0].delivered_at, None, "not a delivery");
        assert_eq!(
            rows[0].last_error.as_deref(),
            Some("no enabled subscriber for this event type")
        );

        // Terminal: never retried, but pruned like any other finished row.
        assert!(store
            .claim_due_events(100_000, 8)
            .await
            .expect("claim")
            .is_empty());
        assert_eq!(store.prune_events(101).await.expect("prune"), 1);
    });
}
