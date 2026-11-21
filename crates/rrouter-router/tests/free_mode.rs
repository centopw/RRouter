//! Free-mode routing: the reserved refs (`free`, `free:<provider>`, `free/<model>`,
//! `free:<provider>/<model>`), the pool they expand to, and the rotation that spreads a
//! request across accounts instead of draining one.

use rrouter_core::Error;
use rrouter_router::{Catalog, Plan, PlanRequest, Planner};
use rrouter_store::{Credentials, FreeModel, Store};
use serde_json::json;
use uuid::Uuid;

/// Instance-wide planner; the store is per test.
fn planner() -> Planner {
    Planner::new()
}

async fn plan(planner: &Planner, store: &Store, model: &str) -> Plan {
    planner
        .plan(&Catalog::builtin(), store, PlanRequest::new(model))
        .await
        .unwrap_or_else(|error| panic!("plan for {model}: {error}"))
}

async fn plan_error(planner: &Planner, store: &Store, model: &str) -> Error {
    planner
        .plan(&Catalog::builtin(), store, PlanRequest::new(model))
        .await
        .expect_err("the ref must not resolve")
}

/// An account the free pool may spend.
async fn free_account(store: &Store, provider: &str, rpm: Option<u32>) -> Uuid {
    store
        .create_connection(
            provider,
            "api_key",
            Credentials {
                api_key: Some("test-key".to_string()),
                free_mode: true,
                free_rpm: rpm,
                ..Credentials::default()
            },
            None,
        )
        .await
        .expect("free connection")
        .id
}

/// An ordinary paying account: it must never appear in a free plan.
async fn paid_account(store: &Store, provider: &str) -> Uuid {
    store
        .create_connection(
            provider,
            "api_key",
            Credentials {
                api_key: Some("test-key".to_string()),
                ..Credentials::default()
            },
            None,
        )
        .await
        .expect("paid connection")
        .id
}

/// One discovery row, the only thing that can put a model in the pool.
async fn discover(store: &Store, provider: &str, model: &str, free: bool) {
    store
        .upsert_free_model(&FreeModel {
            provider: provider.to_string(),
            model: model.to_string(),
            free,
            price: Some(0.0),
            source: "provider-list".to_string(),
            connection_id: None,
            discovered_at: 0,
        })
        .await
        .expect("free row");
}

fn connections(plan: &Plan) -> Vec<Uuid> {
    plan.candidates.iter().map(|c| c.connection).collect()
}

fn models(plan: &Plan) -> Vec<&str> {
    plan.candidates.iter().map(|c| c.model.as_str()).collect()
}

fn providers(plan: &Plan) -> Vec<&str> {
    plan.candidates
        .iter()
        .map(|c| c.provider.as_str())
        .collect()
}

#[tokio::test]
async fn free_expands_only_to_free_mode_accounts() {
    let store = Store::open_in_memory().unwrap();
    let openrouter = free_account(&store, "openrouter", None).await;
    let openrouter_paid = paid_account(&store, "openrouter").await;
    let openai_paid = paid_account(&store, "openai").await;
    discover(&store, "openrouter", "deepseek-r1:free", true).await;
    // A discovered row whose provider only has a paying account is not part of the pool:
    // the ref must not resurrect a paid account, not even with a free-looking id.
    discover(&store, "openai", "gpt-5", true).await;

    let plan = plan(&planner(), &store, "free").await;

    assert_eq!(
        connections(&plan),
        vec![openrouter],
        "only the free-mode account may serve a free request: {:?}",
        plan.candidates
    );
    assert!(!connections(&plan).contains(&openrouter_paid));
    assert!(!connections(&plan).contains(&openai_paid));
    assert_eq!(providers(&plan), vec!["openrouter"]);
    assert_eq!(models(&plan), vec!["deepseek-r1:free"]);
}

#[tokio::test]
async fn a_non_free_model_is_never_served_through_free() {
    let store = Store::open_in_memory().unwrap();
    let account = free_account(&store, "openrouter", None).await;
    discover(&store, "openrouter", "deepseek-r1:free", true).await;
    discover(&store, "openrouter", "gpt-4.1", false).await;

    let plan = plan(&planner(), &store, "free").await;
    assert_eq!(models(&plan), vec!["deepseek-r1:free"]);
    assert!(
        !models(&plan).contains(&"gpt-4.1"),
        "a row discovery did not mark free is not in the pool"
    );

    // Pinning it cannot reach it either: the row exists, but not as free.
    let error = plan_error(&planner(), &store, "free/gpt-4.1").await;
    let Error::NoRoute { detail, .. } = error else {
        panic!("expected NoRoute");
    };
    assert!(detail.contains("not in the free pool"), "{detail}");
    assert_eq!(account, plan.candidates[0].connection);
}

#[tokio::test]
async fn two_sequential_free_requests_start_on_different_models() {
    let store = Store::open_in_memory().unwrap();
    free_account(&store, "openrouter", None).await;
    free_account(&store, "together", None).await;
    discover(&store, "openrouter", "deepseek-r1:free", true).await;
    discover(&store, "together", "qwen3:free", true).await;

    let planner = planner();
    let first = plan(&planner, &store, "free").await;
    let second = plan(&planner, &store, "free").await;

    assert_ne!(
        first.candidates[0].model, second.candidates[0].model,
        "consecutive free requests must not open with the same model"
    );
    assert_ne!(
        first.candidates[0].connection,
        second.candidates[0].connection
    );
}

#[tokio::test]
async fn an_account_over_its_rpm_cap_sorts_last_but_stays() {
    let store = Store::open_in_memory().unwrap();
    let capped = free_account(&store, "openrouter", Some(2)).await;
    let idle = free_account(&store, "openrouter", Some(2)).await;
    discover(&store, "openrouter", "deepseek-r1:free", true).await;

    let planner = planner();
    planner.free_rotation().note(capped);
    planner.free_rotation().note(capped);
    assert!(!planner.free_rotation().admit(capped, Some(2)));
    assert_eq!(planner.free_rotation().pressure(capped), 1.0);

    let plan = plan(&planner, &store, "free/deepseek-r1:free").await;
    assert_eq!(
        connections(&plan).len(),
        2,
        "an account at its cap stays in the plan: {:?}",
        plan.candidates
    );
    assert_eq!(
        plan.candidates[0].connection, idle,
        "the account with room must be tried first"
    );
    assert_eq!(
        plan.candidates[1].connection, capped,
        "the capped account is demoted, never dropped"
    );
}

#[tokio::test]
async fn an_unpressured_account_sorts_first() {
    let store = Store::open_in_memory().unwrap();
    let used = free_account(&store, "openrouter", Some(1)).await;
    let unused = free_account(&store, "openrouter", Some(1)).await;
    discover(&store, "openrouter", "deepseek-r1:free", true).await;

    let planner = planner();
    planner.free_rotation().note(used);
    assert!(!planner.free_rotation().admit(used, Some(1)));
    assert_eq!(planner.free_rotation().pressure(used), 1.0);
    assert!(planner.free_rotation().admit(unused, Some(1)));
    assert_eq!(planner.free_rotation().pressure(unused), 0.0);

    let plan = plan(&planner, &store, "free/deepseek-r1:free").await;
    assert_eq!(plan.candidates[0].connection, unused);
}

#[tokio::test]
async fn free_openrouter_excludes_another_provider() {
    let store = Store::open_in_memory().unwrap();
    let openrouter = free_account(&store, "openrouter", None).await;
    let together = free_account(&store, "together", None).await;
    discover(&store, "openrouter", "deepseek-r1:free", true).await;
    discover(&store, "together", "qwen3:free", true).await;

    let plan = plan(&planner(), &store, "free:openrouter").await;
    assert_eq!(providers(&plan), vec!["openrouter"]);
    assert_eq!(connections(&plan), vec![openrouter]);
    assert!(!connections(&plan).contains(&together));
}

#[tokio::test]
async fn empty_discovery_is_a_404_naming_discovery() {
    let store = Store::open_in_memory().unwrap();
    free_account(&store, "openrouter", None).await;

    let error = plan_error(&planner(), &store, "free").await;
    let Error::NoRoute { model, detail } = error else {
        panic!("empty discovery must be a 404");
    };
    assert_eq!(model, "free");
    assert!(detail.contains("discovery"), "{detail}");
    assert!(detail.contains("/api/free-mode/discover"), "{detail}");
}

#[tokio::test]
async fn unknown_provider_suffix_names_the_provider() {
    let store = Store::open_in_memory().unwrap();
    free_account(&store, "openrouter", None).await;
    discover(&store, "openrouter", "deepseek-r1:free", true).await;

    let error = plan_error(&planner(), &store, "free:no-such-provider").await;
    let Error::NoRoute { detail, .. } = error else {
        panic!("an unknown provider must be a 404");
    };
    assert!(detail.contains("no-such-provider"), "{detail}");
    assert!(detail.contains("provider id or short code"), "{detail}");
}

#[tokio::test]
async fn known_provider_without_a_free_account_names_the_account() {
    let store = Store::open_in_memory().unwrap();
    free_account(&store, "openrouter", None).await;
    paid_account(&store, "together").await;
    discover(&store, "openrouter", "deepseek-r1:free", true).await;
    discover(&store, "together", "qwen3:free", true).await;

    let error = plan_error(&planner(), &store, "free:together").await;
    let Error::NoRoute { detail, .. } = error else {
        panic!("a provider with no free-mode account must be a 404");
    };
    assert!(detail.contains("together"), "{detail}");
    assert!(detail.contains("free-mode account"), "{detail}");
}

#[tokio::test]
async fn a_pinned_model_routes_only_that_model_across_free_accounts() {
    let store = Store::open_in_memory().unwrap();
    let a = free_account(&store, "openrouter", None).await;
    let b = free_account(&store, "together", None).await;
    free_account(&store, "openrouter", None).await;
    discover(&store, "openrouter", "deepseek-v4.1-flash", true).await;
    discover(&store, "together", "deepseek-v4.1-flash", true).await;
    discover(&store, "openrouter", "qwen3:free", true).await;

    let plan = plan(&planner(), &store, "free/deepseek-v4.1-flash").await;
    assert!(
        models(&plan)
            .iter()
            .all(|model| *model == "deepseek-v4.1-flash"),
        "a pin must not broaden to the rest of the pool: {:?}",
        plan.candidates
    );
    let seen = connections(&plan);
    assert!(seen.contains(&a) && seen.contains(&b), "{seen:?}");
    assert_eq!(
        providers(&plan)
            .iter()
            .copied()
            .collect::<std::collections::HashSet<_>>(),
        std::collections::HashSet::from(["openrouter", "together"])
    );
    assert_eq!(
        seen.len(),
        3,
        "every free-mode account of the model is kept"
    );
}

#[tokio::test]
async fn a_pinned_model_accepts_the_provider_prefixed_spelling() {
    let store = Store::open_in_memory().unwrap();
    let account = free_account(&store, "openrouter", None).await;
    discover(&store, "openrouter", "deepseek-r1:free", true).await;

    let plan = plan(&planner(), &store, "free/openrouter/deepseek-r1:free").await;
    assert_eq!(models(&plan), vec!["deepseek-r1:free"]);
    assert_eq!(connections(&plan), vec![account]);
}

#[tokio::test]
async fn a_second_pinned_request_starts_on_a_different_account() {
    let store = Store::open_in_memory().unwrap();
    let a = free_account(&store, "openrouter", Some(5)).await;
    let b = free_account(&store, "openrouter", Some(5)).await;
    discover(&store, "openrouter", "deepseek-v4.1-flash", true).await;

    let planner = planner();
    let first = plan(&planner, &store, "free/deepseek-v4.1-flash").await;
    let second = plan(&planner, &store, "free/deepseek-v4.1-flash").await;

    assert_ne!(
        first.candidates[0].connection, second.candidates[0].connection,
        "the account (and its proxy) must rotate across requests"
    );
    assert_eq!(
        std::collections::HashSet::from([
            first.candidates[0].connection,
            second.candidates[0].connection
        ]),
        std::collections::HashSet::from([a, b])
    );
}

#[tokio::test]
async fn a_pinned_model_with_provider_excludes_another_provider() {
    let store = Store::open_in_memory().unwrap();
    let openrouter = free_account(&store, "openrouter", None).await;
    let together = free_account(&store, "together", None).await;
    discover(&store, "openrouter", "deepseek-v4.1-flash", true).await;
    discover(&store, "together", "deepseek-v4.1-flash", true).await;

    let plan = plan(&planner(), &store, "free:openrouter/deepseek-v4.1-flash").await;
    assert_eq!(providers(&plan), vec!["openrouter"]);
    assert_eq!(connections(&plan), vec![openrouter]);
    assert!(!connections(&plan).contains(&together));
}

#[tokio::test]
async fn an_unknown_pinned_model_is_a_404_mentioning_discovery() {
    let store = Store::open_in_memory().unwrap();
    free_account(&store, "openrouter", None).await;
    discover(&store, "openrouter", "deepseek-r1:free", true).await;

    let error = plan_error(&planner(), &store, "free/no-such-model").await;
    let Error::NoRoute { detail, .. } = error else {
        panic!("an unknown pinned model must be a 404");
    };
    assert!(detail.contains("no-such-model"), "{detail}");
    assert!(detail.contains("discovery"), "{detail}");
}

#[tokio::test]
async fn a_pinned_model_without_an_active_account_names_the_accounts() {
    let store = Store::open_in_memory().unwrap();
    let account = free_account(&store, "openrouter", None).await;
    discover(&store, "openrouter", "deepseek-v4.1-flash", true).await;
    store.set_connection_active(account, false).await.unwrap();

    let error = plan_error(&planner(), &store, "free/deepseek-v4.1-flash").await;
    let Error::NoRoute { detail, .. } = error else {
        panic!("a model with no active free-mode account must be a 404");
    };
    assert!(detail.contains("openrouter"), "{detail}");
    assert!(detail.contains("no free-mode account"), "{detail}");
}

#[tokio::test]
async fn a_pinned_model_scoped_to_a_provider_that_lacks_it_names_the_provider() {
    let store = Store::open_in_memory().unwrap();
    free_account(&store, "openrouter", None).await;
    free_account(&store, "together", None).await;
    discover(&store, "openrouter", "deepseek-v4.1-flash", true).await;

    // The model is in the pool, just not on the provider the client pinned: the message
    // must name that provider, not the ones that do serve it.
    let error = plan_error(&planner(), &store, "free:together/deepseek-v4.1-flash").await;
    let Error::NoRoute { detail, .. } = error else {
        panic!("a pinned model missing on the provider must be a 404");
    };
    assert!(detail.contains("together"), "{detail}");
    assert!(detail.contains("no discovered free model"), "{detail}");
}

#[tokio::test]
async fn routing_models_narrows_bare_free_and_removing_it_restores_the_pool() {
    let store = Store::open_in_memory().unwrap();
    free_account(&store, "openrouter", None).await;
    discover(&store, "openrouter", "deepseek-v4.1-flash", true).await;
    discover(&store, "openrouter", "qwen3:free", true).await;

    store
        .save_free_mode_config(
            &json!({ "version": 1 }),
            &json!({ "models": ["deepseek-v4.1-flash"] }),
        )
        .await
        .unwrap();

    let narrowed = plan(&planner(), &store, "free").await;
    assert_eq!(models(&narrowed), vec!["deepseek-v4.1-flash"]);

    // A pin is the client's own choice and is not narrowed by the operator knob.
    let pinned = plan(&planner(), &store, "free/qwen3:free").await;
    assert_eq!(models(&pinned), vec!["qwen3:free"]);

    // An empty knob is the whole pool again, without a second allowlist mechanism.
    store
        .save_free_mode_config(&json!({ "version": 1 }), &json!({}))
        .await
        .unwrap();
    let full = plan(&planner(), &store, "free").await;
    let mut seen = models(&full);
    seen.sort_unstable();
    assert_eq!(seen, vec!["deepseek-v4.1-flash", "qwen3:free"]);
}

#[tokio::test]
async fn max_attempts_caps_the_free_plan() {
    let store = Store::open_in_memory().unwrap();
    for _ in 0..3 {
        free_account(&store, "openrouter", None).await;
    }
    discover(&store, "openrouter", "deepseek-v4.1-flash", true).await;
    store
        .save_free_mode_config(&json!({ "version": 1 }), &json!({ "max_attempts": 2 }))
        .await
        .unwrap();

    let plan = plan(&planner(), &store, "free/deepseek-v4.1-flash").await;
    assert_eq!(plan.candidates.len(), 2, "{:?}", plan.candidates);
}
