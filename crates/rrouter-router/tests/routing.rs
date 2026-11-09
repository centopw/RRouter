//! Tests for routing: resolution grammar, round-robin, breaker, fallback,
//! streaming commit semantics, and the attempt cap.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures_util::StreamExt;

use rrouter_core::{ChatResponse, Error, FinishReason, Usage};
use rrouter_router::breaker::{BreakerState, ProbeTicket};
use rrouter_router::dispatch::{AttemptOutcome, RetryPolicy};
use rrouter_router::stream_dispatch::{byte_stream, collect_stream};
use rrouter_router::{
    Breaker, Candidate, Catalog, Dispatcher, Plan, PlanRequest, Planner, Strategy,
};
use rrouter_store::{ModelRow, Store};
use uuid::Uuid;

fn resp(model: &str) -> ChatResponse {
    ChatResponse {
        id: "id-1".into(),
        model: model.into(),
        parts: Vec::new(),
        finish_reason: FinishReason::Stop,
        usage: Usage::default(),
    }
}

fn upstream(status: u16) -> Error {
    Error::Upstream {
        provider: "p".into(),
        status,
        body: String::new(),
        payload: None,
    }
}

/// An upstream failure carrying the `Retry-After` header the proxy would parse.
fn upstream_retry_after(status: u16, secs: u64) -> Error {
    Error::Upstream {
        provider: "p".into(),
        status,
        body: String::new(),
        payload: Some(serde_json::json!({ "retry_after": secs })),
    }
}

/// Dispatcher with a fixed retry curve, in milliseconds so the suite stays fast.
fn retrying(attempts: usize, base_ms: u64, max_ms: u64) -> Dispatcher {
    Dispatcher::new(Breaker::new()).with_retry_policy(RetryPolicy {
        attempts,
        base: Duration::from_millis(base_ms),
        max: Duration::from_millis(max_ms),
        budget: None,
    })
}

async fn conn(store: &Store, provider: &str, model_hint: &str, priority: i32) -> Uuid {
    let c = store
        .create_connection(provider, "api_key", Default::default(), Some(model_hint))
        .await
        .expect("create connection");
    store
        .set_connection_priority(c.id, priority)
        .await
        .expect("priority");
    c.id
}

/// Equal-priority order of the builtin `openai/gpt-5` connections under the
/// least-latency strategy.
async fn least_latency_order(planner: &Planner, store: &Store) -> Vec<Uuid> {
    planner
        .plan(
            &Catalog::builtin(),
            store,
            PlanRequest {
                model: "openai/gpt-5",
                stream: false,
                affinity: None,
                strategy: Strategy::LeastLatency,
                policy: None,
                allowed_models: None,
                team_id: None,
                canary: None,
                canary_seed: 0,
                combo_start: None,
            },
        )
        .await
        .expect("plan")
        .candidates
        .iter()
        .map(|c| c.connection)
        .collect()
}

fn plan_of(n: usize) -> Plan {
    Plan {
        model: "m".into(),
        candidates: (0..n)
            .map(|i| Candidate {
                provider: "p".into(),
                model: "m".into(),
                connection: Uuid::new_v4(),
                priority: i as i32,
                dialect: rrouter_core::Dialect::OpenAiChat,
            })
            .collect(),
        stream: false,
    }
}

#[tokio::test]
async fn alias_resolves_one_hop_without_looping() {
    let store = Store::open_in_memory().unwrap();
    let id = conn(&store, "claude", "primary", 0).await;
    store
        .upsert_alias("fast", "claude/primary-x")
        .await
        .unwrap();
    store
        .upsert_alias("claude/primary-x", "claude/never-reached")
        .await
        .unwrap();
    let planner = Planner::new();
    let plan = planner
        .plan(
            &Catalog::builtin(),
            &store,
            PlanRequest {
                model: "fast",
                stream: false,
                affinity: None,
                strategy: Strategy::Priority,
                policy: None,
                allowed_models: None,
                team_id: None,
                canary: None,
                canary_seed: 0,
                combo_start: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(plan.candidates.len(), 1);
    assert_eq!(plan.candidates[0].provider, "claude");
    // Exactly one hop: the second alias is not followed.
    assert_eq!(plan.candidates[0].model, "primary-x");
    assert_eq!(plan.candidates[0].connection, id);
}

#[tokio::test]
async fn self_referential_alias_does_not_loop() {
    let store = Store::open_in_memory().unwrap();
    let _ = conn(&store, "openai", "loop", 0).await;
    store.upsert_alias("loop", "loop").await.unwrap();
    let planner = Planner::new();
    // Resolves via catalog fallback/lookup and terminates.
    let plan = planner
        .plan(
            &Catalog::builtin(),
            &store,
            PlanRequest {
                model: "loop",
                stream: false,
                affinity: None,
                strategy: Strategy::Priority,
                policy: None,
                allowed_models: None,
                team_id: None,
                canary: None,
                canary_seed: 0,
                combo_start: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(plan.candidates.len(), 1);
    assert_eq!(plan.candidates[0].model, "loop");
}

#[tokio::test]
async fn explicit_provider_is_not_widened_for_unknown_ids() {
    // A bare id the catalog does not know is normally handed to every active
    // provider; naming the provider must stop that, or a Claude model reaches a
    // Codex account and the test button surfaces the wrong upstream's 400.
    let store = Store::open_in_memory().unwrap();
    assert!(
        !Catalog::builtin().is_known("claude-opus-5-5"),
        "the test only means something while this id is outside the builtin catalog"
    );
    let claude = conn(&store, "claude", "claude-opus-5-5", 0).await;
    let _codex = conn(&store, "codex", "gpt-5", 0).await;
    let planner = Planner::new();
    let plan = planner
        .plan(
            &Catalog::builtin(),
            &store,
            PlanRequest {
                model: "claude/claude-opus-5-5",
                stream: false,
                affinity: None,
                strategy: Strategy::Priority,
                policy: None,
                allowed_models: None,
                team_id: None,
                canary: None,
                canary_seed: 0,
                combo_start: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(plan.candidates.len(), 1);
    assert_eq!(plan.candidates[0].provider, "claude");
    assert_eq!(plan.candidates[0].model, "claude-opus-5-5");
    assert_eq!(plan.candidates[0].connection, claude);

    // Unprefixed ids keep the old behaviour: any active provider may serve them.
    let bare = planner
        .plan(
            &Catalog::builtin(),
            &store,
            PlanRequest {
                model: "mystery-model",
                stream: false,
                affinity: None,
                strategy: Strategy::Priority,
                policy: None,
                allowed_models: None,
                team_id: None,
                canary: None,
                canary_seed: 0,
                combo_start: None,
            },
        )
        .await
        .unwrap();
    let providers: Vec<&str> = bare
        .candidates
        .iter()
        .map(|c| c.provider.as_str())
        .collect();
    assert!(providers.contains(&"claude"));
    assert!(providers.contains(&"codex"));
}

/// Antigravity is registered but has no builtin catalog: its ids are operator-defined.
/// Naming the provider must still resolve the model explicitly (never widened to every
/// active account), carry the CCA dialect, and stay behind the key's allowlist.
#[tokio::test]
async fn antigravity_explicit_id_resolves_with_the_cca_dialect() {
    let store = Store::open_in_memory().unwrap();
    let antigravity = conn(&store, "google-antigravity", "gemini-3-pro", 0).await;
    let _openai = conn(&store, "openai", "gpt-4.1", 0).await;
    let planner = Planner::new();
    let plan = planner
        .plan(
            &Catalog::builtin(),
            &store,
            PlanRequest {
                model: "google-antigravity/gemini-3-pro",
                stream: false,
                affinity: None,
                strategy: Strategy::Priority,
                policy: None,
                allowed_models: None,
                team_id: None,
                canary: None,
                canary_seed: 0,
                combo_start: None,
            },
        )
        .await
        .expect("explicit antigravity id resolves");
    assert_eq!(plan.candidates.len(), 1);
    assert_eq!(plan.candidates[0].provider, "google-antigravity");
    assert_eq!(plan.candidates[0].model, "gemini-3-pro");
    assert_eq!(plan.candidates[0].connection, antigravity);
    assert_eq!(
        plan.candidates[0].dialect,
        rrouter_core::Dialect::GoogleCodeAssist
    );

    let allowed = [String::from("openai/gpt-4.1")];
    let denied = planner
        .plan(
            &Catalog::builtin(),
            &store,
            PlanRequest {
                model: "google-antigravity/gemini-3-pro",
                stream: false,
                affinity: None,
                strategy: Strategy::Priority,
                policy: None,
                allowed_models: Some(&allowed),
                team_id: None,
                canary: None,
                canary_seed: 0,
                combo_start: None,
            },
        )
        .await
        .expect_err("the allowlist excludes antigravity");
    assert!(matches!(denied, Error::Forbidden { .. }), "{denied}");
}

/// A request-scoped combo start rotates the stored list after combo expansion. The
/// allowlist is still judged on the expanded candidates, not on the combo name.
#[tokio::test]
async fn combo_start_rotates_the_expanded_candidates() {
    let store = Store::open_in_memory().unwrap();
    let xai = conn(&store, "xai", "grok-4", 9).await;
    let openai = conn(&store, "openai", "gpt-5", 9).await;
    let claude = conn(&store, "claude", "claude-sonnet-4-5", 9).await;
    store
        .upsert_combo(
            "chain",
            &[
                "xai/grok-4".into(),
                "openai/gpt-5".into(),
                "claude/claude-sonnet-4-5".into(),
            ],
        )
        .await
        .unwrap();

    let plan = Planner::new()
        .plan(
            &Catalog::builtin(),
            &store,
            PlanRequest {
                model: "chain",
                stream: false,
                affinity: None,
                strategy: Strategy::Priority,
                policy: None,
                allowed_models: None,
                team_id: None,
                canary: None,
                canary_seed: 0,
                combo_start: Some(1),
            },
        )
        .await
        .unwrap();
    let order: Vec<uuid::Uuid> = plan.candidates.iter().map(|c| c.connection).collect();
    assert_eq!(order, vec![openai, claude, xai], "start 1 wraps the list");

    // Out-of-range starts wrap as well: 4 % 3 == 1.
    let wrapped = Planner::new()
        .plan(
            &Catalog::builtin(),
            &store,
            PlanRequest {
                model: "chain",
                stream: false,
                affinity: None,
                strategy: Strategy::Priority,
                policy: None,
                allowed_models: None,
                team_id: None,
                canary: None,
                canary_seed: 0,
                combo_start: Some(4),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        wrapped
            .candidates
            .iter()
            .map(|c| c.connection)
            .collect::<Vec<_>>(),
        vec![openai, claude, xai]
    );

    let allowed = [String::from("openai/gpt-5")];
    let filtered = Planner::new()
        .plan(
            &Catalog::builtin(),
            &store,
            PlanRequest {
                model: "chain",
                stream: false,
                affinity: None,
                strategy: Strategy::Priority,
                policy: None,
                allowed_models: Some(&allowed),
                team_id: None,
                canary: None,
                canary_seed: 0,
                combo_start: Some(1),
            },
        )
        .await
        .unwrap();
    assert_eq!(filtered.candidates.len(), 1);
    assert_eq!(filtered.candidates[0].connection, openai);
}

#[tokio::test]
async fn explicit_provider_wins_over_catalog_guess() {
    let store = Store::open_in_memory().unwrap();
    let _ = conn(&store, "openai", "gpt-4.1", 0).await;
    let id = conn(&store, "github", "gpt-4.1", 0).await;
    let planner = Planner::new();
    let plan = planner
        .plan(
            &Catalog::builtin(),
            &store,
            PlanRequest {
                model: "github/gpt-4.1",
                stream: false,
                affinity: None,
                strategy: Strategy::Priority,
                policy: None,
                allowed_models: None,
                team_id: None,
                canary: None,
                canary_seed: 0,
                combo_start: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(plan.candidates.len(), 1);
    assert_eq!(plan.candidates[0].provider, "github");
    assert_eq!(plan.candidates[0].connection, id);
}

#[tokio::test]
async fn combo_preserves_declared_order() {
    let store = Store::open_in_memory().unwrap();
    let a = conn(&store, "xai", "grok-4", 9).await;
    let b = conn(&store, "openai", "gpt-5", 9).await;
    store
        .upsert_combo("chain", &["xai/grok-4".into(), "openai/gpt-5".into()])
        .await
        .unwrap();
    let planner = Planner::new();
    let plan = planner
        .plan(
            &Catalog::builtin(),
            &store,
            PlanRequest {
                model: "combo:chain",
                stream: false,
                affinity: None,
                strategy: Strategy::Priority,
                policy: None,
                allowed_models: None,
                team_id: None,
                canary: None,
                canary_seed: 0,
                combo_start: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(plan.candidates.len(), 2);
    assert_eq!(plan.candidates[0].connection, a);
    assert_eq!(plan.candidates[1].connection, b);

    // Bare combo name resolves the same way.
    let bare = planner
        .plan(
            &Catalog::builtin(),
            &store,
            PlanRequest {
                model: "chain",
                stream: false,
                affinity: None,
                strategy: Strategy::Priority,
                policy: None,
                allowed_models: None,
                team_id: None,
                canary: None,
                canary_seed: 0,
                combo_start: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(bare.candidates, plan.candidates);
}

#[tokio::test]
async fn round_robin_alternates_equal_priority_connections() {
    let store = Store::open_in_memory().unwrap();
    let a = conn(&store, "openai", "gpt-5", 0).await;
    let b = conn(&store, "openai", "gpt-5", 0).await;
    let c = conn(&store, "openai", "gpt-5", 5).await;
    let planner = Planner::new();
    let catalog = Catalog::builtin();
    let first = planner
        .plan(
            &catalog,
            &store,
            PlanRequest {
                model: "openai/gpt-5",
                stream: false,
                affinity: None,
                strategy: Strategy::Priority,
                policy: None,
                allowed_models: None,
                team_id: None,
                canary: None,
                canary_seed: 0,
                combo_start: None,
            },
        )
        .await
        .unwrap();
    let second = planner
        .plan(
            &catalog,
            &store,
            PlanRequest {
                model: "openai/gpt-5",
                stream: false,
                affinity: None,
                strategy: Strategy::Priority,
                policy: None,
                allowed_models: None,
                team_id: None,
                canary: None,
                canary_seed: 0,
                combo_start: None,
            },
        )
        .await
        .unwrap();
    assert_ne!(
        first.candidates[0].connection,
        second.candidates[0].connection
    );
    assert_eq!(
        std::collections::HashSet::from([
            first.candidates[0].connection,
            second.candidates[0].connection
        ]),
        std::collections::HashSet::from([a, b])
    );
    // Priority still dominates: the priority-5 connection is always last.
    assert_eq!(first.candidates[2].connection, c);
    assert_eq!(second.candidates[2].connection, c);
}

/// Weighted selection: 3:1 means the heavy account leads eight plans out of ten,
/// deterministically, because the sequence is driven by the planner cursor.
#[tokio::test]
async fn weighted_puts_the_heavy_account_first_eight_times_in_ten() {
    let store = Store::open_in_memory().unwrap();
    let heavy = conn(&store, "openai", "gpt-5", 0).await;
    let light = conn(&store, "openai", "gpt-5", 0).await;
    store.set_connection_weight(heavy, 3).await.unwrap();
    store.set_connection_weight(light, 1).await.unwrap();
    let planner = Planner::new();
    let catalog = Catalog::builtin();

    let mut heavy_first = 0;
    for _ in 0..10 {
        let plan = planner
            .plan(
                &catalog,
                &store,
                PlanRequest {
                    model: "openai/gpt-5",
                    stream: false,
                    affinity: None,
                    strategy: Strategy::Weighted,
                    policy: None,
                    allowed_models: None,
                    team_id: None,
                    canary: None,
                    canary_seed: 0,
                    combo_start: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(plan.candidates.len(), 2, "both accounts stay in the plan");
        assert!(
            plan.candidates.iter().any(|c| c.connection == light),
            "the light account must never be dropped"
        );
        if plan.candidates[0].connection == heavy {
            heavy_first += 1;
        }
    }
    assert!(
        (7..=8).contains(&heavy_first),
        "3:1 should lead 7-8 plans out of 10, got {heavy_first}"
    );
}

/// A strategy only orders inside an equal-priority group. An operator who sets
/// priority 1 on a heavy account still gets the priority-0 account first.
#[tokio::test]
async fn weighted_and_latency_never_override_priority() {
    let store = Store::open_in_memory().unwrap();
    let first = conn(&store, "openai", "gpt-5", 0).await;
    let second = conn(&store, "openai", "gpt-5", 1).await;
    // The priority-1 account is heavier and already measured as fast: neither fact
    // may promote it above the priority-0 account.
    store.set_connection_weight(second, 1000).await.unwrap();
    store.set_connection_weight(first, 1).await.unwrap();
    store.record_connection_latency(first, 5_000).await.unwrap();
    store.record_connection_latency(second, 10).await.unwrap();

    let planner = Planner::new();
    let catalog = Catalog::builtin();
    for strategy in [Strategy::Weighted, Strategy::LeastLatency] {
        let plan = planner
            .plan(
                &catalog,
                &store,
                PlanRequest {
                    model: "openai/gpt-5",
                    stream: false,
                    affinity: None,
                    strategy,
                    policy: None,
                    allowed_models: None,
                    team_id: None,
                    canary: None,
                    canary_seed: 0,
                    combo_start: None,
                },
            )
            .await
            .unwrap();
        let order: Vec<Uuid> = plan.candidates.iter().map(|c| c.connection).collect();
        assert_eq!(
            order,
            vec![first, second],
            "{strategy:?} reordered priority groups"
        );
    }
}

/// Least latency: measured connections first, fastest first; never-measured ones
/// after them, heavier first. A new sample moves the connection immediately, and the
/// EWMA is what decides, not the last raw sample.
#[tokio::test]
async fn least_latency_orders_by_ewma_and_puts_unknown_connections_last() {
    let store = Store::open_in_memory().unwrap();
    let fast = conn(&store, "openai", "gpt-5", 0).await;
    let slow = conn(&store, "openai", "gpt-5", 0).await;
    let unknown_light = conn(&store, "openai", "gpt-5", 0).await;
    let unknown_heavy = conn(&store, "openai", "gpt-5", 0).await;
    store.set_connection_weight(unknown_light, 1).await.unwrap();
    store.set_connection_weight(unknown_heavy, 9).await.unwrap();
    store.set_connection_weight(fast, 1).await.unwrap();
    store.set_connection_weight(slow, 1).await.unwrap();

    for _ in 0..3 {
        store.record_connection_latency(fast, 100).await.unwrap();
    }
    store.record_connection_latency(slow, 1_000).await.unwrap();

    let planner = Planner::new();
    assert_eq!(
        least_latency_order(&planner, &store).await,
        vec![fast, slow, unknown_heavy, unknown_light],
        "measured latencies first, then unknown connections by weight"
    );

    // 100*0.8 + 10_000*0.2 = 2_080 ms, now slower than the 1_000 ms connection.
    store.record_connection_latency(fast, 10_000).await.unwrap();
    assert_eq!(
        least_latency_order(&planner, &store).await,
        vec![slow, fast, unknown_heavy, unknown_light],
        "one slow sample must reorder, and only that connection's EWMA moved"
    );
}

/// Equal-priority order of every candidate for `model` under `strategy`.
async fn order(store: &Store, model: &str, strategy: Strategy) -> Vec<Uuid> {
    Planner::new()
        .plan(
            &Catalog::builtin(),
            store,
            PlanRequest {
                model,
                stream: false,
                affinity: None,
                strategy,
                policy: None,
                allowed_models: None,
                team_id: None,
                canary: None,
                canary_seed: 0,
                combo_start: None,
            },
        )
        .await
        .expect("plan")
        .candidates
        .iter()
        .map(|c| c.connection)
        .collect()
}

/// Publish a price for one `(provider, model)` pair the way the registry does: `input`
/// and `output` USD per million tokens on a `custom` row.
async fn price(store: &Store, provider: &str, model: &str, input: f64, output: f64) {
    price_from(store, provider, model, "custom", input, output).await;
}

/// Same, with an explicit registry source, so precedence between the provider's own row
/// and the models.dev vendor row can be exercised.
async fn price_from(
    store: &Store,
    provider: &str,
    model: &str,
    source: &str,
    input: f64,
    output: f64,
) {
    store
        .upsert_model(&ModelRow {
            provider: provider.into(),
            model: model.into(),
            display: None,
            context: None,
            max_output: None,
            caps: serde_json::json!({}),
            cost: Some(serde_json::json!({ "input": input, "output": output })),
            source: source.into(),
            updated_at: 0,
        })
        .await
        .expect("price row");
}

/// Cheapest picks the provider that publishes the lower `input + output` price, not the
/// catalog order the other strategies follow.
#[tokio::test]
async fn cheapest_puts_the_cheaper_provider_first() {
    let store = Store::open_in_memory().unwrap();
    let expensive = conn(&store, "openai", "gpt-4.1", 0).await;
    let cheap = conn(&store, "github", "gpt-4.1", 0).await;
    price(&store, "openai", "gpt-4.1", 10.0, 30.0).await;
    price(&store, "github", "gpt-4.1", 1.0, 2.0).await;

    assert_eq!(
        order(&store, "gpt-4.1", Strategy::Cheapest).await,
        vec![cheap, expensive],
        "40.0 vs 3.0 USD per million tokens"
    );
    assert_eq!(
        order(&store, "gpt-4.1", Strategy::Priority).await,
        vec![expensive, cheap],
        "catalog order must stay untouched under the default strategy"
    );
}

/// Equal prices are equal preference: the strategy must not invent an order, so the
/// catalog/priority order survives unchanged.
#[tokio::test]
async fn cheapest_keeps_input_order_when_prices_tie() {
    let store = Store::open_in_memory().unwrap();
    let first = conn(&store, "openai", "gpt-4.1", 0).await;
    let second = conn(&store, "github", "gpt-4.1", 0).await;
    price(&store, "openai", "gpt-4.1", 2.0, 4.0).await;
    price(&store, "github", "gpt-4.1", 1.0, 5.0).await;

    assert_eq!(
        order(&store, "gpt-4.1", Strategy::Cheapest).await,
        vec![first, second],
        "6.0 == 6.0 must keep the pre-strategy order"
    );
}

/// A pair with no published price sorts after every priced pair: routing never guesses a
/// rate, so an unpriced provider never wins by looking free.
#[tokio::test]
async fn cheapest_puts_the_unpriced_provider_last() {
    let store = Store::open_in_memory().unwrap();
    let priced = conn(&store, "openai", "gpt-4.1", 0).await;
    let unpriced = conn(&store, "github", "gpt-4.1", 0).await;
    price(&store, "openai", "gpt-4.1", 100.0, 100.0).await;

    assert_eq!(
        order(&store, "gpt-4.1", Strategy::Cheapest).await,
        vec![priced, unpriced]
    );
}

/// Priority dominates the price: a cheap account an operator deliberately demoted is
/// still tried after the expensive priority-0 one.
#[tokio::test]
async fn cheapest_never_overrides_priority() {
    let store = Store::open_in_memory().unwrap();
    let expensive = conn(&store, "openai", "gpt-4.1", 0).await;
    let cheap = conn(&store, "github", "gpt-4.1", 1).await;
    price(&store, "openai", "gpt-4.1", 100.0, 100.0).await;
    price(&store, "github", "gpt-4.1", 0.01, 0.01).await;

    assert_eq!(
        order(&store, "gpt-4.1", Strategy::Cheapest).await,
        vec![expensive, cheap]
    );
}

/// Price precedence follows the cost accounting: the provider's own registry row decides,
/// and only its absence falls through to the models.dev vendor row it mirrors.
#[tokio::test]
async fn cheapest_prefers_the_providers_row_over_the_vendor_row() {
    let store = Store::open_in_memory().unwrap();
    let openai = conn(&store, "openai", "gpt-4.1", 0).await;
    let github = conn(&store, "github", "gpt-4.1", 0).await;
    price(&store, "openai", "gpt-4.1", 10.0, 0.0).await;
    // `github` mirrors models.dev's `github-copilot`; that vendor row is far cheaper, but
    // the provider's own row is what the accounting prices with, so it decides.
    price_from(&store, "github", "gpt-4.1", "synced", 50.0, 0.0).await;
    price_from(&store, "github-copilot", "gpt-4.1", "custom", 0.5, 0.0).await;

    assert_eq!(
        order(&store, "gpt-4.1", Strategy::Cheapest).await,
        vec![openai, github]
    );
}

/// Conversations must stay on one account: KV caches are per upstream account, so
/// rotating between turns turns every prompt into a cold cache write.
#[tokio::test]
async fn affinity_pins_one_conversation_but_spreads_conversations() {
    let store = Store::open_in_memory().unwrap();
    let a = conn(&store, "openai", "gpt-5", 0).await;
    let b = conn(&store, "openai", "gpt-5", 0).await;
    let planner = Planner::new();
    let catalog = Catalog::builtin();
    let first_for = |key: u64| {
        planner.plan(
            &catalog,
            &store,
            PlanRequest {
                model: "openai/gpt-5",
                stream: false,
                affinity: Some(key),
                strategy: Strategy::Priority,
                policy: None,
                allowed_models: None,
                team_id: None,
                canary: None,
                canary_seed: 0,
                combo_start: None,
            },
        )
    };

    let pinned = first_for(7).await.unwrap().candidates[0].connection;
    for _ in 0..5 {
        assert_eq!(
            first_for(7).await.unwrap().candidates[0].connection,
            pinned,
            "the same conversation rotated accounts"
        );
    }

    let mut accounts = std::collections::HashSet::new();
    for key in 0..32u64 {
        accounts.insert(first_for(key).await.unwrap().candidates[0].connection);
    }
    assert_eq!(
        accounts,
        std::collections::HashSet::from([a, b]),
        "affinity stopped spreading"
    );
}

#[tokio::test]
async fn disabled_model_is_excluded() {
    let store = Store::open_in_memory().unwrap();
    let _ = conn(&store, "openai", "gpt-5", 0).await;
    store.set_disabled("gpt-5", true).await.unwrap();
    let planner = Planner::new();
    let err = planner
        .plan(
            &Catalog::builtin(),
            &store,
            PlanRequest {
                model: "openai/gpt-5",
                stream: false,
                affinity: None,
                strategy: Strategy::Priority,
                policy: None,
                allowed_models: None,
                team_id: None,
                canary: None,
                canary_seed: 0,
                combo_start: None,
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(err, Error::NoRoute { .. }), "{err:?}");
}

#[test]
fn breaker_trips_opens_probes_and_reopens() {
    let breaker = Breaker::new();
    let id = Uuid::new_v4();
    assert!(breaker.check(id).is_none());

    for _ in 0..2 {
        breaker.record_failure(id, 500, None);
    }
    assert!(breaker.is_available(id), "2 failures must not trip");
    breaker.record_failure(id, 500, None);
    let wait = breaker.check(id).expect("open after 3 failures");
    assert!(
        wait > Duration::ZERO && wait <= Duration::from_secs(30),
        "{wait:?}"
    );
    assert!(matches!(breaker.state(id), BreakerState::Open { .. }));

    // After the window: first call allowed as the single half-open probe.
    let later = Instant::now() + Duration::from_secs(31);
    assert!(breaker.check_at(id, later).is_none());
    assert!(matches!(
        breaker.state_at(id, later),
        BreakerState::HalfOpen { .. }
    ));
    breaker.record_success(id);
    assert_eq!(breaker.state(id), BreakerState::Closed);

    // Trip again, probe, fail the probe → immediately open again.
    for _ in 0..3 {
        breaker.record_failure(id, 500, None);
    }
    let later = Instant::now() + Duration::from_secs(31);
    assert!(breaker.check_at(id, later).is_none());
    breaker.record_failure(id, 500, None);
    assert!(matches!(breaker.state(id), BreakerState::Open { .. }));
    assert!(breaker.check_at(id, Instant::now()).is_some());
}

#[test]
fn half_open_admits_exactly_one_probe() {
    let breaker = Breaker::new();
    let id = Uuid::new_v4();
    for _ in 0..3 {
        breaker.record_failure(id, 500, None);
    }
    let later = Instant::now() + Duration::from_secs(31);

    // Reading eligibility never consumes the probe slot: every reader sees it free.
    assert!(breaker.check_at(id, later).is_none());
    assert!(breaker.check_at(id, later).is_none());

    // First caller wins the probe slot...
    assert!(matches!(
        breaker.claim_at(id, later),
        Ok(ProbeTicket::Probe { .. })
    ));
    // ...everyone else is told to wait, including via the read path.
    assert!(breaker.claim_at(id, later).is_err());
    assert!(breaker.check_at(id, later).is_some());
    assert!(!breaker.is_available(id));

    // A failed probe re-opens the window for the full 30 s.
    breaker.record_failure_at(id, 500, None, later);
    assert_eq!(breaker.check_at(id, later).unwrap().as_secs(), 30);

    // A successful probe closes it and restores full eligibility.
    let later = later + Duration::from_secs(31);
    assert!(matches!(
        breaker.claim_at(id, later),
        Ok(ProbeTicket::Probe { .. })
    ));
    breaker.record_success(id);
    assert!(breaker.check(id).is_none());
}

#[test]
fn unclaimed_candidate_never_leaks_a_probe_slot() {
    // Regression: `check` used to claim the half-open slot, so a candidate that was
    // ordered but never sent (attempt cap, earlier success) parked the account for
    // an hour. Ordering must be side-effect free.
    let breaker = Breaker::new();
    let id = Uuid::new_v4();
    for _ in 0..3 {
        breaker.record_failure(id, 500, None);
    }
    let later = Instant::now() + Duration::from_secs(31);
    assert!(breaker.check_at(id, later).is_none());
    // No claim happened, so the slot is still free for a real attempt.
    assert!(matches!(
        breaker.claim_at(id, later),
        Ok(ProbeTicket::Probe { .. })
    ));
    breaker.record_success(id);
    assert!(breaker.check(id).is_none());
}

#[tokio::test]
async fn streamed_success_settles_the_breaker_on_drop() {
    // Regression: the gateway stops polling upstream once it decodes the terminal
    // event, so the winning stream is dropped without ever returning `Ready(None)`.
    // If success were only recorded on `Ready(None)`, two recorded failures plus the
    // leaked probe slot would leave the account unusable (HalfOpen with the slot taken).
    let breaker = Arc::new(Breaker::new());
    let plan = plan_of(1);
    let conn = plan.candidates[0].connection;

    // Two prior failures: one more would trip a 3-failure policy.
    breaker.record_failure(conn, 500, None);
    breaker.record_failure(conn, 500, None);

    let (_, committed) = Dispatcher::with_shared_breaker(breaker.clone())
        .run_streaming(&plan, |_c| async {
            Ok(byte_stream(futures_util::stream::iter(vec![Ok(
                Bytes::from_static(b"data: x\n\n"),
            )])))
        })
        .await
        .expect("stream accepted");

    // Never polled: dropped as soon as the gateway is done with it.
    drop(committed);

    // The dropped stream was a success, so it must have reset the failure count.
    // Two fresh failures then leave the breaker closed; a leaked/unsettled claim
    // would show up as HalfOpen or a taken probe slot.
    for _ in 0..2 {
        breaker.record_failure(conn, 500, None);
    }
    assert!(
        matches!(breaker.state(conn), BreakerState::Closed),
        "dropping a healthy committed stream must count as a success; got {:?}",
        breaker.state(conn)
    );
    assert!(
        breaker.claim(conn).is_ok(),
        "no probe slot may be left taken"
    );
}

#[test]
fn rate_limit_backoff_is_exponential_and_honors_retry_after() {
    let breaker = Breaker::new();
    let id = Uuid::new_v4();
    let now = Instant::now();
    let mut last = Duration::ZERO;
    for expected in [30u64, 60, 120] {
        breaker.record_failure_at(id, 429, None, now);
        let wait = breaker.check_at(id, now).expect("429 opens");
        assert_eq!(wait.as_secs(), expected);
        assert!(wait >= last);
        last = wait;
    }

    // Retry-After wins over the computed backoff, capped at one hour.
    breaker.record_failure_at(id, 429, Some(Duration::from_secs(7)), now);
    assert_eq!(breaker.check_at(id, now).unwrap().as_secs(), 7);
    breaker.record_success(id);
    breaker.record_failure_at(id, 429, Some(Duration::from_secs(9_000)), now);
    assert_eq!(breaker.check_at(id, now).unwrap().as_secs(), 3600);

    // Auth failures take the long window.
    breaker.record_failure_at(id, 401, None, now);
    assert_eq!(breaker.check_at(id, now).unwrap().as_secs(), 300);
}

#[tokio::test]
async fn fallback_uses_second_candidate_and_opens_first_breaker() {
    let dispatcher = Dispatcher::new(Breaker::new());
    let plan = plan_of(2);
    let first = plan.candidates[0].connection;
    let seen = AtomicUsize::new(0);
    let outcome: AttemptOutcome<ChatResponse> = dispatcher
        .run(&plan, |c| {
            seen.fetch_add(1, Ordering::SeqCst);
            async move {
                if c.connection == first {
                    Err(upstream(503))
                } else {
                    Ok(resp("m"))
                }
            }
        })
        .await
        .unwrap();
    assert_eq!(outcome.result.unwrap().model, "m");
    assert_eq!(outcome.candidate.connection, plan.candidates[1].connection);
    // The default policy gives the first candidate one retry before falling back.
    assert_eq!(seen.load(Ordering::SeqCst), 3);
    // One 503 is a failure but not a trip: the breaker stays closed and the
    // account remains a fallback for the next request.
    assert_eq!(dispatcher.breaker().state(first), BreakerState::Closed);
}

#[tokio::test]
async fn dead_credential_falls_through_to_the_next_account() {
    // Regression: the refresher reports a dead refresh token / missing credential as
    // `Error::Unauthorized`. That is an account problem, not a request problem, so the
    // dispatcher must try the next candidate instead of failing the whole request.
    let dispatcher = Dispatcher::new(Breaker::new());
    let plan = plan_of(2);
    let dead = plan.candidates[0].connection;
    let seen = AtomicUsize::new(0);
    let outcome = dispatcher
        .run(&plan, |c| {
            seen.fetch_add(1, Ordering::SeqCst);
            async move {
                if c.connection == dead {
                    Err(Error::Unauthorized("provider claude refresh failed".into()))
                } else {
                    Ok(resp("m"))
                }
            }
        })
        .await
        .unwrap();
    assert_eq!(
        seen.load(Ordering::SeqCst),
        3,
        "one retry, then the next account"
    );
    assert_eq!(outcome.candidate.connection, plan.candidates[1].connection);
    assert_eq!(outcome.result.unwrap().model, "m");
    // A 401-class failure parks the dead account hard (5 min).
    assert!(matches!(
        dispatcher.breaker().state(dead),
        BreakerState::Open { .. }
    ));
}

#[tokio::test]
async fn non_retryable_error_stops_immediately() {
    let dispatcher = Dispatcher::new(Breaker::new());
    let plan = plan_of(3);
    let seen = AtomicUsize::new(0);
    let outcome = dispatcher
        .run(&plan, |_c| {
            seen.fetch_add(1, Ordering::SeqCst);
            async move { Err::<ChatResponse, _>(upstream(400)) }
        })
        .await
        .unwrap();
    assert_eq!(seen.load(Ordering::SeqCst), 1);
    assert!(matches!(
        outcome.result,
        Err(Error::Upstream { status: 400, .. })
    ));
    assert!(dispatcher
        .breaker()
        .is_available(plan.candidates[0].connection));
}

/// A 503 from `open` has not committed a byte, so the same candidate is retried
/// instead of spending the fallback (same rule as the JSON path).
#[tokio::test]
async fn streaming_retries_the_same_candidate_before_first_byte() {
    let dispatcher = retrying(2, 1, 50);
    let plan = plan_of(2);
    let first = plan.candidates[0].connection;
    let sent = Mutex::new(Vec::new());
    let (winner, stream) = dispatcher
        .run_streaming(&plan, |c| {
            let n = {
                let mut sent = sent.lock().unwrap();
                sent.push(c.connection);
                sent.len()
            };
            async move {
                if n < 3 {
                    Err(upstream(503))
                } else {
                    Ok(byte_stream(futures_util::stream::iter(vec![Ok(
                        Bytes::from_static(b"data: x\n\n"),
                    )])))
                }
            }
        })
        .await
        .unwrap();
    assert_eq!(winner.connection, first);
    assert_eq!(
        sent.into_inner().unwrap(),
        vec![first, first, first],
        "two retries stay on the same candidate, then it succeeds"
    );
    let body = collect_stream(stream).await.unwrap();
    assert_eq!(
        body, b"data: x\n\n",
        "the frame reaches the client exactly once"
    );
}

/// Zero retries keeps the pre-phase behaviour for streams too: one open per
/// candidate, then the next one.
#[tokio::test]
async fn streaming_zero_retries_fails_over_immediately() {
    let dispatcher = retrying(0, 1, 50);
    let plan = plan_of(2);
    let first = plan.candidates[0].connection;
    let sent = Mutex::new(Vec::new());
    let (winner, stream) = dispatcher
        .run_streaming(&plan, |c| {
            sent.lock().unwrap().push(c.connection);
            async move {
                if c.connection == first {
                    Err(upstream(503))
                } else {
                    Ok(byte_stream(futures_util::stream::iter(vec![Ok(
                        Bytes::from_static(b"data: y\n\n"),
                    )])))
                }
            }
        })
        .await
        .unwrap();
    assert_eq!(winner.connection, plan.candidates[1].connection);
    assert_eq!(
        sent.into_inner().unwrap(),
        vec![plan.candidates[0].connection, plan.candidates[1].connection],
        "one open per candidate, no retry"
    );
    assert_eq!(collect_stream(stream).await.unwrap(), b"data: y\n\n");
}

/// A 400 is the client's fault: retrying another account cannot fix it.
#[tokio::test]
async fn streaming_client_error_is_never_retried() {
    let dispatcher = retrying(2, 1, 50);
    let plan = plan_of(2);
    let sent = Mutex::new(Vec::new());
    let err = match dispatcher
        .run_streaming(&plan, |c| {
            sent.lock().unwrap().push(c.connection);
            async move { Err(upstream(400)) }
        })
        .await
    {
        Ok(_) => panic!("a 400 must fail the request"),
        Err(err) => err,
    };
    assert!(matches!(err, Error::Upstream { status: 400, .. }));
    assert_eq!(
        sent.into_inner().unwrap(),
        vec![plan.candidates[0].connection],
        "a 400 must stop at the first send"
    );
    assert!(dispatcher
        .breaker()
        .is_available(plan.candidates[0].connection));
}

/// Bytes already reached the client, so the error is terminal — one error, no
/// re-send, even with retries configured.
#[tokio::test]
async fn streaming_midstream_error_is_terminal_and_not_retried() {
    let dispatcher = retrying(2, 1, 50);
    let plan = plan_of(2);
    let sent = Mutex::new(Vec::new());
    let (winner, mut stream) = dispatcher
        .run_streaming(&plan, |c| {
            sent.lock().unwrap().push(c.connection);
            async move {
                Ok(byte_stream(futures_util::stream::iter(vec![
                    Ok(Bytes::from_static(b"partial")),
                    Err(upstream(502)),
                    Ok(Bytes::from_static(b"never")),
                ])))
            }
        })
        .await
        .unwrap();
    assert_eq!(winner.connection, plan.candidates[0].connection);
    assert_eq!(
        stream.next().await.expect("chunk").unwrap(),
        Bytes::from_static(b"partial")
    );
    let err = stream.next().await.expect("terminal error").unwrap_err();
    assert!(matches!(err, Error::Upstream { status: 502, .. }));
    assert!(
        stream.next().await.is_none(),
        "the error is terminal and surfaces once"
    );
    assert_eq!(
        sent.into_inner().unwrap(),
        vec![winner.connection],
        "no retry once bytes were emitted"
    );
    // One 502 is recorded against the connection (three would open the breaker).
    assert_eq!(
        dispatcher.breaker().state(winner.connection),
        BreakerState::Closed
    );
}

/// A retry is not another account failure: three opens of one candidate are one strike.
#[tokio::test]
async fn streaming_retries_count_as_one_breaker_failure() {
    let dispatcher = Dispatcher::with_shared_breaker(Arc::new(Breaker::with_policy(
        2,
        Duration::from_secs(30),
        1,
    )))
    .with_retry_policy(RetryPolicy {
        attempts: 2,
        base: Duration::from_millis(0),
        max: Duration::from_millis(0),
        budget: None,
    });
    let plan = plan_of(1);
    let conn = plan.candidates[0].connection;
    let seen = AtomicUsize::new(0);
    let err = match dispatcher
        .run_streaming(&plan, |_c| {
            seen.fetch_add(1, Ordering::SeqCst);
            async move { Err(upstream(503)) }
        })
        .await
    {
        Ok(_) => panic!("every open failed"),
        Err(err) => err,
    };
    assert!(matches!(err, Error::Upstream { status: 503, .. }));
    assert_eq!(seen.load(Ordering::SeqCst), 3);
    assert_eq!(
        dispatcher.breaker().state(conn),
        BreakerState::Closed,
        "three sends of one candidate must not trip a threshold-2 breaker"
    );
}

/// Retries spend the same `RROUTER_MAX_ATTEMPTS` budget as fallbacks.
#[tokio::test]
async fn streaming_retries_stay_inside_the_total_send_budget() {
    let dispatcher = retrying(5, 0, 0).with_max_attempts(3);
    let plan = plan_of(4);
    let seen = AtomicUsize::new(0);
    let err = match dispatcher
        .run_streaming(&plan, |_c| {
            seen.fetch_add(1, Ordering::SeqCst);
            async move { Err(upstream(503)) }
        })
        .await
    {
        Ok(_) => panic!("every open failed"),
        Err(err) => err,
    };
    assert!(matches!(err, Error::Upstream { status: 503, .. }));
    assert_eq!(
        seen.load(Ordering::SeqCst),
        3,
        "the cap counts stream opens too"
    );
}

#[tokio::test]
async fn repeated_failures_open_the_first_candidate_and_skip_it() {
    // Spec item 8: three 503s on candidate #1 open its breaker, so the next
    // request goes straight to candidate #2 without a wasted send.
    let dispatcher = Dispatcher::new(Breaker::new());
    let plan = plan_of(2);
    let first = plan.candidates[0].connection;
    for _ in 0..3 {
        dispatcher.breaker().record_failure(first, 503, None);
    }
    assert!(!dispatcher.breaker().is_available(first));

    let seen = AtomicUsize::new(0);
    let outcome = dispatcher
        .run(&plan, |c| {
            seen.fetch_add(1, Ordering::SeqCst);
            async move {
                if c.connection == first {
                    Err(upstream(503))
                } else {
                    Ok(resp("m"))
                }
            }
        })
        .await
        .unwrap();
    assert_eq!(
        seen.load(Ordering::SeqCst),
        1,
        "open breaker must not be sent to"
    );
    assert_eq!(outcome.candidate.connection, plan.candidates[1].connection);
}

#[tokio::test]
async fn attempt_cap_is_honored() {
    let dispatcher = Dispatcher::new(Breaker::new());
    let plan = plan_of(6);
    let seen = AtomicUsize::new(0);
    let err = dispatcher
        .run(&plan, |_c| {
            seen.fetch_add(1, Ordering::SeqCst);
            async move { Err::<ChatResponse, _>(upstream(503)) }
        })
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Upstream { status: 503, .. }));
    assert_eq!(seen.load(Ordering::SeqCst), 4);
}

#[tokio::test]
async fn rate_limited_candidate_does_not_block_an_eligible_one() {
    let breaker = Arc::new(Breaker::new());
    let dispatcher = Dispatcher::with_shared_breaker(Arc::clone(&breaker));
    let plan = plan_of(2);
    let blocked = plan.candidates[0].connection;
    let healthy = plan.candidates[1].connection;
    breaker.record_failure(blocked, 429, Some(Duration::from_secs(30)));

    let started = Instant::now();
    let outcome = dispatcher
        .run(&plan, move |c| async move {
            if c.connection == blocked {
                Err(upstream(429))
            } else {
                Ok(resp("m"))
            }
        })
        .await
        .unwrap();
    assert!(
        started.elapsed() < Duration::from_millis(100),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(outcome.candidate.connection, healthy);
}

#[tokio::test]
async fn rate_limited_account_is_not_retried_before_failing_over() {
    let breaker = Arc::new(Breaker::new());
    let dispatcher = Dispatcher::with_shared_breaker(Arc::clone(&breaker));
    let plan = plan_of(2);
    let limited = plan.candidates[0].connection;
    let healthy = plan.candidates[1].connection;
    let first_sends = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&first_sends);

    let started = Instant::now();
    let outcome = dispatcher
        .run(&plan, move |c| {
            let first_sends = Arc::clone(&counter);
            async move {
                if c.connection == limited {
                    first_sends.fetch_add(1, Ordering::SeqCst);
                    Err(upstream(429))
                } else if c.connection == healthy {
                    Ok(resp("m"))
                } else {
                    unreachable!()
                }
            }
        })
        .await
        .unwrap();

    assert_eq!(outcome.candidate.connection, healthy);
    assert_eq!(
        first_sends.load(Ordering::SeqCst),
        1,
        "a 429 must not be retried on the same account"
    );
    assert!(
        started.elapsed() < Duration::from_millis(100),
        "{:?}",
        started.elapsed()
    );
    assert!(
        !breaker.is_available(limited),
        "the rate-limited account must be parked"
    );
}

#[tokio::test]
async fn skipped_candidate_is_waited_for_once_when_nothing_else_is_eligible() {
    // Open window shorter than MAX_SKIP_WAIT so the bounded wait applies.
    let breaker = Arc::new(Breaker::with_policy(1, Duration::from_millis(300), 1));
    let dispatcher = Dispatcher::with_shared_breaker(Arc::clone(&breaker));
    let plan = plan_of(1);
    let only = plan.candidates[0].connection;
    breaker.record_failure(only, 500, None);
    assert!(breaker.check(only).is_some());

    let started = Instant::now();
    let outcome = dispatcher
        .run(&plan, |_c| async move { Ok(resp("m")) })
        .await
        .unwrap();
    assert_eq!(outcome.candidate.connection, only);
    assert!(
        started.elapsed() >= Duration::from_millis(250),
        "{:?}",
        started.elapsed()
    );
    assert!(
        started.elapsed() <= Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn cancelled_attempt_releases_the_probe_slot() {
    // Regression: a client disconnect drops the handler future mid-await. Without an
    // RAII claim the half-open slot would stay taken and park the account for an hour.
    // A zero-width open window makes every attempt a half-open probe from the start.
    let breaker = Arc::new(Breaker::with_policy(1, Duration::ZERO, 1));
    let plan = plan_of(1);
    let conn = plan.candidates[0].connection;

    // Trip it: the next eligibility check sees an already-elapsed window → half-open.
    breaker.record_failure(conn, 500, None);
    // A zero-width window means the state is already past-open: the next attempt is a probe.
    assert!(matches!(
        breaker.state(conn),
        BreakerState::Open { .. } | BreakerState::HalfOpen { .. }
    ));

    let dispatcher = Dispatcher::with_shared_breaker(breaker.clone());
    let cancelled = tokio::time::timeout(
        Duration::from_millis(50),
        dispatcher.run(&plan, |_c| async {
            std::future::pending::<Result<ChatResponse, Error>>().await
        }),
    )
    .await;
    assert!(
        cancelled.is_err(),
        "the attempt must have been cancelled mid-flight"
    );

    // The probe slot must be free again; a leaked claim would report PROBE_TAKEN.
    assert!(
        breaker.check(conn).is_none(),
        "a cancelled attempt must not leave the probe slot taken"
    );
    assert!(
        breaker.claim(conn).is_ok(),
        "account must be probeable again"
    );
}

#[tokio::test]
async fn client_error_does_not_trip_a_healthy_account() {
    // Regression: non-retryable errors (a malformed client request) were recorded as
    // account failures, so three bad requests opened a working account's breaker.
    let dispatcher = Dispatcher::new(Breaker::new());
    let plan = plan_of(1);
    let conn = plan.candidates[0].connection;

    for _ in 0..3 {
        let outcome = dispatcher
            .run(&plan, |_c| async {
                Err::<ChatResponse, _>(Error::BadRequest("messages is required".into()))
            })
            .await
            .unwrap();
        assert!(outcome.result.is_err());
    }
    assert!(
        matches!(dispatcher.breaker().state(conn), BreakerState::Closed),
        "client errors must not count toward the account's failure streak"
    );
    assert!(
        dispatcher.breaker().claim(conn).is_ok(),
        "account must stay eligible"
    );
}

/// A 503 is transient: the same candidate is retried before the fallback is spent.
#[tokio::test]
async fn transient_503_is_retried_on_the_same_candidate() {
    let dispatcher = retrying(2, 1, 50);
    let plan = plan_of(1);
    let seen = AtomicUsize::new(0);
    let outcome = dispatcher
        .run(&plan, |_c| {
            let n = seen.fetch_add(1, Ordering::SeqCst);
            async move {
                if n < 2 {
                    Err(upstream(503))
                } else {
                    Ok(resp("m"))
                }
            }
        })
        .await
        .unwrap();
    assert_eq!(seen.load(Ordering::SeqCst), 3, "two retries then success");
    assert_eq!(outcome.candidate.connection, plan.candidates[0].connection);
    assert_eq!(outcome.result.unwrap().model, "m");
}

/// Zero retries keeps the pre-phase behaviour: one send per candidate, then fail over.
#[tokio::test]
async fn zero_retries_send_each_candidate_once() {
    let dispatcher = retrying(0, 1, 50);
    let plan = plan_of(2);
    let sent = Mutex::new(Vec::new());
    let err = dispatcher
        .run(&plan, |c| {
            sent.lock().unwrap().push(c.connection);
            async move { Err::<ChatResponse, _>(upstream(503)) }
        })
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Upstream { status: 503, .. }));
    assert_eq!(
        sent.into_inner().unwrap(),
        vec![plan.candidates[0].connection, plan.candidates[1].connection]
    );
}

/// A 400 is the client's fault; another account or another try cannot fix it.
#[tokio::test]
async fn client_error_is_never_retried() {
    let dispatcher = retrying(5, 1, 50);
    let plan = plan_of(1);
    let seen = AtomicUsize::new(0);
    let outcome = dispatcher
        .run(&plan, |_c| {
            seen.fetch_add(1, Ordering::SeqCst);
            async move { Err::<ChatResponse, _>(upstream(400)) }
        })
        .await
        .unwrap();
    assert_eq!(
        seen.load(Ordering::SeqCst),
        1,
        "a 400 must stop at the first send"
    );
    assert!(matches!(
        outcome.result,
        Err(Error::Upstream { status: 400, .. })
    ));
}

/// `Retry-After: 60` must not stall a request whose ceiling is milliseconds.
#[tokio::test]
async fn retry_after_never_exceeds_the_backoff_ceiling() {
    let dispatcher = retrying(1, 1, 10);
    let plan = plan_of(1);
    let seen = AtomicUsize::new(0);
    let started = Instant::now();
    let err = dispatcher
        .run(&plan, |_c| {
            seen.fetch_add(1, Ordering::SeqCst);
            async move { Err::<ChatResponse, _>(upstream_retry_after(503, 60)) }
        })
        .await
        .unwrap_err();
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(
        seen.load(Ordering::SeqCst),
        2,
        "capped wait, then one retry"
    );
    assert!(matches!(err, Error::Upstream { status: 503, .. }));
}

/// Retries spend the same `RROUTER_MAX_ATTEMPTS` budget as fallbacks.
#[tokio::test]
async fn retries_stay_inside_the_total_send_budget() {
    let dispatcher = retrying(5, 0, 0).with_max_attempts(3);
    let plan = plan_of(4);
    let seen = AtomicUsize::new(0);
    let err = dispatcher
        .run(&plan, |_c| {
            seen.fetch_add(1, Ordering::SeqCst);
            async move { Err::<ChatResponse, _>(upstream(503)) }
        })
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Upstream { status: 503, .. }));
    assert_eq!(seen.load(Ordering::SeqCst), 3, "the cap counts retries too");
}

/// A retry is not another account failure: three sends of one candidate are one strike.
#[tokio::test]
async fn retries_count_as_one_breaker_failure() {
    let dispatcher = retrying(2, 0, 0);
    let plan = plan_of(1);
    let conn = plan.candidates[0].connection;
    let seen = AtomicUsize::new(0);
    let err = dispatcher
        .run(&plan, |_c| {
            seen.fetch_add(1, Ordering::SeqCst);
            async move { Err::<ChatResponse, _>(upstream(503)) }
        })
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Upstream { status: 503, .. }));
    assert_eq!(seen.load(Ordering::SeqCst), 3);
    assert_eq!(
        dispatcher.breaker().state(conn),
        BreakerState::Closed,
        "three sends of one candidate must not trip a threshold-3 breaker"
    );
}

/// Retries never sleep past the headers-phase budget the caller granted.
#[tokio::test]
async fn retry_sleep_never_passes_the_headers_deadline() {
    let dispatcher = Dispatcher::new(Breaker::new()).with_retry_policy(RetryPolicy {
        attempts: 2,
        base: Duration::from_millis(500),
        max: Duration::from_secs(5),
        budget: Some(Duration::from_millis(5)),
    });
    let plan = plan_of(1);
    let seen = AtomicUsize::new(0);
    let started = Instant::now();
    let err = dispatcher
        .run(&plan, |_c| {
            seen.fetch_add(1, Ordering::SeqCst);
            async move { Err::<ChatResponse, _>(upstream(503)) }
        })
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Upstream { status: 503, .. }));
    assert_eq!(
        seen.load(Ordering::SeqCst),
        1,
        "the 500 ms backoff cannot fit"
    );
    assert!(
        started.elapsed() < Duration::from_millis(200),
        "{:?}",
        started.elapsed()
    );
}

/// The pure form of the curve: `min(base * 2^n, max)`, ±20%, capped by `Retry-After`.
#[test]
fn backoff_doubles_caps_and_jitters_within_twenty_percent() {
    let policy = RetryPolicy {
        attempts: 5,
        base: Duration::from_millis(100),
        max: Duration::from_millis(500),
        budget: None,
    };
    assert_eq!(policy.delay(0, None, 0.5), Duration::from_millis(100));
    assert_eq!(policy.delay(1, None, 0.5), Duration::from_millis(200));
    assert_eq!(policy.delay(2, None, 0.5), Duration::from_millis(400));
    assert_eq!(
        policy.delay(3, None, 0.5),
        Duration::from_millis(500),
        "800 ms capped at the ceiling"
    );
    assert_eq!(
        policy.delay(3, None, 1.0),
        Duration::from_millis(500),
        "jitter never exceeds the ceiling"
    );
    assert_eq!(
        policy.delay(0, None, 0.0),
        Duration::from_millis(80),
        "-20%"
    );
    assert_eq!(
        policy.delay(0, None, 1.0),
        Duration::from_millis(120),
        "+20%"
    );
    assert_eq!(
        policy.delay(2, Some(Duration::from_millis(5)), 0.5),
        Duration::from_millis(5),
        "Retry-After caps the wait"
    );
    assert_eq!(
        policy.delay(2, Some(Duration::from_secs(60)), 0.5),
        Duration::from_millis(400),
        "a huge Retry-After cannot lengthen the wait past the curve"
    );
    assert_eq!(
        policy.delay(3, Some(Duration::from_secs(60)), 0.5),
        Duration::from_millis(500),
        "Retry-After is capped by the ceiling, not the other way round"
    );
}

// ----- hedged requests -------------------------------------------------------------

/// One candidate per entry, with the given provider label and priority.
fn plan_with(entries: &[(&str, i32)]) -> Plan {
    Plan {
        model: "m".into(),
        candidates: entries
            .iter()
            .map(|(provider, priority)| Candidate {
                provider: (*provider).to_string(),
                model: "m".into(),
                connection: Uuid::new_v4(),
                priority: *priority,
                dialect: rrouter_core::Dialect::OpenAiChat,
            })
            .collect(),
        stream: false,
    }
}

fn hedging(attempts: usize, hedge_ms: u64) -> Dispatcher {
    Dispatcher::new(Breaker::new())
        .with_max_attempts(attempts)
        .with_hedge(Some(Duration::from_millis(hedge_ms)))
        .with_retry_policy(RetryPolicy {
            attempts: 0,
            ..RetryPolicy::default()
        })
}

/// Counted on drop, so a cancelled hedge future is visible.
struct DropFlag(Arc<AtomicUsize>);

impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn a_hedge_starts_after_the_threshold_and_wins() {
    let plan = plan_with(&[("slow", 0), ("fast", 0)]);
    let dispatcher = hedging(4, 20);
    let sent = Arc::new(AtomicUsize::new(0));
    let started = Instant::now();

    let outcome = dispatcher
        .run(&plan, |c| {
            let sent = Arc::clone(&sent);
            async move {
                sent.fetch_add(1, Ordering::SeqCst);
                if c.provider == "slow" {
                    tokio::time::sleep(Duration::from_millis(300)).await;
                }
                Ok::<String, Error>(c.provider.clone())
            }
        })
        .await
        .expect("the hedge answers");

    let elapsed = started.elapsed();
    assert_eq!(outcome.candidate.provider, "fast");
    assert!(
        elapsed >= Duration::from_millis(20),
        "the hedge held for the threshold: {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_millis(300),
        "the slow leg was not awaited: {elapsed:?}"
    );
    assert_eq!(sent.load(Ordering::SeqCst), 2, "both legs were sent");
}

#[tokio::test]
async fn an_immediate_primary_wins_without_starting_the_hedge() {
    let plan = plan_with(&[("a", 0), ("b", 0)]);
    let dispatcher = hedging(4, 100);
    let sent = Arc::new(AtomicUsize::new(0));

    let outcome = dispatcher
        .run(&plan, |c| {
            let sent = Arc::clone(&sent);
            async move {
                sent.fetch_add(1, Ordering::SeqCst);
                Ok::<String, Error>(c.provider.clone())
            }
        })
        .await
        .expect("the primary answers");

    assert_eq!(outcome.candidate.provider, "a");
    assert_eq!(sent.load(Ordering::SeqCst), 1, "no hedge was launched");
}

#[tokio::test]
async fn a_cancelled_hedge_leg_is_dropped() {
    let plan = plan_with(&[("a", 0), ("b", 0)]);
    let dispatcher = hedging(4, 10);
    let drops = Arc::new(AtomicUsize::new(0));

    let outcome = dispatcher
        .run(&plan, |c| {
            let drops = Arc::clone(&drops);
            async move {
                let _guard = DropFlag(drops);
                if c.provider == "a" {
                    // Started before the threshold, answers after it: both legs are live.
                    tokio::time::sleep(Duration::from_millis(60)).await;
                } else {
                    tokio::time::sleep(Duration::from_millis(400)).await;
                }
                Ok::<String, Error>(c.provider.clone())
            }
        })
        .await
        .expect("the primary answers");

    assert_eq!(outcome.candidate.provider, "a");
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        drops.load(Ordering::SeqCst),
        2,
        "the losing hedge future was cancelled, not awaited"
    );
}

#[tokio::test]
async fn a_hedge_never_crosses_a_priority_group() {
    let plan = plan_with(&[("a", 0), ("b", 1)]);
    let dispatcher = hedging(4, 10);
    let sent = Arc::new(AtomicUsize::new(0));
    let started = Instant::now();

    let outcome = dispatcher
        .run(&plan, |c| {
            let sent = Arc::clone(&sent);
            async move {
                sent.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(60)).await;
                Ok::<String, Error>(c.provider.clone())
            }
        })
        .await
        .expect("the primary answers");

    assert_eq!(outcome.candidate.provider, "a");
    assert!(started.elapsed() >= Duration::from_millis(60));
    assert_eq!(sent.load(Ordering::SeqCst), 1, "a lower tier is not hedged");
}

#[tokio::test]
async fn a_single_candidate_is_never_hedged() {
    let plan = plan_with(&[("only", 0)]);
    let dispatcher = hedging(4, 10);
    let sent = Arc::new(AtomicUsize::new(0));

    let outcome = dispatcher
        .run(&plan, |c| {
            let sent = Arc::clone(&sent);
            async move {
                sent.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(40)).await;
                Ok::<String, Error>(c.provider.clone())
            }
        })
        .await
        .expect("the only candidate answers");

    assert_eq!(outcome.candidate.provider, "only");
    assert_eq!(sent.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_failed_hedge_race_falls_back_inside_the_budget() {
    let plan = plan_with(&[("a", 0), ("b", 0), ("c", 0)]);
    let dispatcher = hedging(4, 10);
    let sent = Arc::new(AtomicUsize::new(0));

    let outcome = dispatcher
        .run(&plan, |c| {
            let sent = Arc::clone(&sent);
            async move {
                sent.fetch_add(1, Ordering::SeqCst);
                match c.provider.as_str() {
                    // The primary outlives the threshold, so the race actually happens.
                    "a" => {
                        tokio::time::sleep(Duration::from_millis(30)).await;
                        Err(upstream(503))
                    }
                    "b" => Err(upstream(503)),
                    _ => Ok("c".to_string()),
                }
            }
        })
        .await
        .expect("the third candidate answers");

    assert_eq!(outcome.candidate.provider, "c");
    assert_eq!(
        sent.load(Ordering::SeqCst),
        3,
        "two raced legs then fallback"
    );
}

#[tokio::test]
async fn a_failed_hedge_race_never_exceeds_the_send_budget() {
    let plan = plan_with(&[("a", 0), ("b", 0), ("c", 0)]);
    let dispatcher = hedging(2, 10);
    let sent = Arc::new(AtomicUsize::new(0));

    let err = dispatcher
        .run(&plan, |c| {
            let sent = Arc::clone(&sent);
            async move {
                sent.fetch_add(1, Ordering::SeqCst);
                if c.provider == "a" {
                    tokio::time::sleep(Duration::from_millis(30)).await;
                }
                Err::<String, Error>(upstream(503))
            }
        })
        .await
        .expect_err("the budget is spent on the race");

    match err {
        Error::Upstream { status, .. } => assert_eq!(status, 503),
        other => panic!("unexpected error: {other}"),
    }
    assert_eq!(
        sent.load(Ordering::SeqCst),
        2,
        "the race spent the whole two-send budget"
    );
}
