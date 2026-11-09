//! Eval suites, cases, runs and results at the store layer: bounds, ordering, the
//! transactional completion, and the cascade.
//!
//! The HTTP surface and the background runner live in `rrouter-gateway/tests/evals.rs`;
//! this file is about the invariants the runner depends on.

use rrouter_store::{
    eval_passes, EvalRunStatus, EvalSummary, Store, MAX_EVAL_CASES, MAX_EVAL_MODEL,
    MAX_EVAL_SYSTEM_PROMPT, MAX_EVAL_TEXT,
};
use tokio::runtime::Runtime;
use uuid::Uuid;

fn store() -> Store {
    Store::open_in_memory().expect("store")
}

fn rt() -> Runtime {
    Runtime::new().expect("runtime")
}

#[test]
fn a_suite_round_trips_and_its_name_is_unique() {
    let rt = rt();
    rt.block_on(async {
        let store = store();
        let suite = store
            .create_eval_suite(
                "  support smoke  ",
                Some("  prompts the bot must answer  "),
                Some("  You are support. Cite policy.  "),
            )
            .await
            .expect("create");
        assert_eq!(suite.name, "support smoke", "trimmed");
        assert_eq!(
            suite.description.as_deref(),
            Some("prompts the bot must answer")
        );
        assert_eq!(
            suite.system_prompt.as_deref(),
            Some("You are support. Cite policy."),
            "system prompt is trimmed and stored"
        );
        assert!(store
            .create_eval_suite(
                "too much context",
                None,
                Some(&"x".repeat(MAX_EVAL_SYSTEM_PROMPT + 1)),
            )
            .await
            .is_err());

        assert!(store
            .create_eval_suite("support smoke", None, None)
            .await
            .is_err());
        assert!(store.create_eval_suite("   ", None, None).await.is_err());
        assert!(store
            .create_eval_suite(&"x".repeat(81), None, None)
            .await
            .is_err());

        let second = store
            .create_eval_suite("alpha", None, None)
            .await
            .expect("second");
        let listed: Vec<String> = store
            .eval_suites()
            .await
            .expect("list")
            .into_iter()
            .map(|suite| suite.name)
            .collect();
        assert_eq!(listed, vec!["alpha", "support smoke"], "ordered by name");

        let renamed = store
            .update_eval_suite(suite.id, Some("renamed"), Some(None), Some(None))
            .await
            .expect("update")
            .expect("exists");
        assert_eq!(renamed.name, "renamed");
        assert_eq!(renamed.description, None, "null clears the description");
        assert_eq!(renamed.system_prompt, None, "null clears the context");
        assert!(renamed.updated_at >= suite.updated_at);
        let contextual = store
            .update_eval_suite(suite.id, None, None, Some(Some("  policy v2  ")))
            .await
            .expect("update")
            .expect("exists");
        assert_eq!(contextual.system_prompt.as_deref(), Some("policy v2"));

        // A duplicate rename is refused, and renaming onto itself is fine.
        assert!(store
            .update_eval_suite(second.id, Some("renamed"), None, None)
            .await
            .is_err());
        assert!(store
            .update_eval_suite(second.id, Some("alpha"), None, None)
            .await
            .expect("self rename")
            .is_some());

        assert!(store.delete_eval_suite(suite.id).await.expect("delete"));
        assert!(!store.delete_eval_suite(suite.id).await.expect("idempotent"));
        assert!(store.eval_suite(suite.id).await.expect("read").is_none());
    });
}

#[test]
fn cases_are_validated_bounded_and_ordered() {
    let rt = rt();
    rt.block_on(async {
        let store = store();
        let suite = store
            .create_eval_suite("core", None, None)
            .await
            .expect("suite");

        let first = store
            .create_eval_case(suite.id, None, "  What is 2+2?  ", Some("4"))
            .await
            .expect("case");
        assert_eq!(first.input, "What is 2+2?", "trimmed");
        assert_eq!(first.expected.as_deref(), Some("4"));
        let second = store
            .create_eval_case(suite.id, Some("unscored"), "Name a colour", None)
            .await
            .expect("case");
        assert_eq!(second.expected, None);

        assert!(store
            .create_eval_case(suite.id, None, "   ", None)
            .await
            .is_err());
        assert!(store
            .create_eval_case(suite.id, None, &"x".repeat(MAX_EVAL_TEXT + 1), None)
            .await
            .is_err());
        assert!(store
            .create_eval_case(Uuid::new_v4(), None, "orphan", None)
            .await
            .is_err());

        let cases = store.eval_cases(suite.id).await.expect("cases");
        assert_eq!(
            cases.iter().map(|case| case.id).collect::<Vec<_>>(),
            vec![first.id, second.id],
            "created order"
        );
        assert_eq!(store.eval_case_count(suite.id).await.expect("count"), 2);

        // The cap is a store invariant, not only an API one.
        let room = MAX_EVAL_CASES - 2;
        for index in 0..room {
            store
                .create_eval_case(suite.id, None, &format!("prompt {index}"), None)
                .await
                .expect("fill");
        }
        assert_eq!(
            store.eval_case_count(suite.id).await.expect("count"),
            MAX_EVAL_CASES
        );
        let over = store
            .create_eval_case(suite.id, None, "one too many", None)
            .await;
        assert!(over.is_err(), "a suite is capped");
        assert!(over.unwrap_err().to_string().contains("limited to"));

        assert!(store.delete_eval_case(first.id).await.expect("delete"));
        assert!(!store.delete_eval_case(first.id).await.expect("idempotent"));
    });
}

#[test]
fn a_run_starts_once_and_completes_once() {
    let rt = rt();
    rt.block_on(async {
        let store = store();
        let suite = store
            .create_eval_suite("core", None, None)
            .await
            .expect("suite");
        let case_a = store
            .create_eval_case(suite.id, None, "a", Some("yes"))
            .await
            .expect("case");
        let case_b = store
            .create_eval_case(suite.id, None, "b", Some("yes"))
            .await
            .expect("case");

        let run = store
            .create_eval_run(suite.id, "openai/gpt-4.1", Some("openai/gpt-4.1-mini"))
            .await
            .expect("run");
        assert_eq!(run.status, EvalRunStatus::Pending);
        assert_eq!(
            run.judge_model.as_deref(),
            Some("openai/gpt-4.1-mini"),
            "judge model round-trips"
        );
        assert!(store
            .create_eval_run(suite.id, "openai/gpt-4.1", Some("   "))
            .await
            .is_err());
        assert!(store
            .create_eval_run(
                suite.id,
                "openai/gpt-4.1",
                Some(&"x".repeat(MAX_EVAL_MODEL + 1))
            )
            .await
            .is_err());
        assert!(store.create_eval_run(suite.id, "  ", None).await.is_err());
        assert!(store
            .create_eval_run(Uuid::new_v4(), "openai/gpt-4.1", None)
            .await
            .is_err());

        let started = store
            .start_eval_run(run.id)
            .await
            .expect("start")
            .expect("run");
        assert_eq!(started.status, EvalRunStatus::Running);
        assert!(
            store
                .start_eval_run(run.id)
                .await
                .expect("start again")
                .is_none(),
            "a run starts once"
        );

        // Results are written in completion order but read in case order.
        store
            .insert_eval_result(
                run.id,
                case_b.id,
                Some("yes indeed"),
                Some(1.0),
                Some(20),
                None,
            )
            .await
            .expect("result b");
        store
            .insert_eval_result(run.id, case_a.id, Some("no"), Some(0.0), Some(10), None)
            .await
            .expect("result a");
        let results = store.eval_results(run.id).await.expect("results");
        assert_eq!(
            results
                .iter()
                .map(|result| result.case_id)
                .collect::<Vec<_>>(),
            vec![case_a.id, case_b.id],
            "stable case order"
        );
        assert_eq!(results[0].case_input, "a");
        assert_eq!(store.eval_result_count(run.id).await.expect("count"), 2);

        let summary = EvalSummary {
            cases: 2,
            passed: 1,
            failed: 1,
            duration_ms: 42,
            ..EvalSummary::default()
        };
        let done = store
            .complete_eval_run(run.id, EvalRunStatus::Completed, summary.clone())
            .await
            .expect("complete")
            .expect("run");
        assert_eq!(done.status, EvalRunStatus::Completed);
        assert!(done.completed_at.is_some());
        let stored = done.summary.expect("summary");
        assert_eq!(stored["passed"], 1);
        assert_eq!(stored["failed"], 1);
        assert_eq!(stored["duration_ms"], 42);

        assert!(
            store
                .complete_eval_run(run.id, EvalRunStatus::Failed, summary)
                .await
                .expect("second completion")
                .is_none(),
            "a finished run cannot be reopened"
        );

        let listed = store.eval_runs(Some(suite.id), 10).await.expect("list");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, run.id);
        assert!(!listed[0].status.is_active());
    });
}

#[test]
fn a_run_can_name_a_judge_model_or_leave_it_null() {
    let rt = rt();
    rt.block_on(async {
        let store = store();
        let suite = store
            .create_eval_suite("judge", None, None)
            .await
            .expect("suite");
        let plain = store
            .create_eval_run(suite.id, "openai/gpt-4.1", None)
            .await
            .expect("plain run");
        assert_eq!(plain.judge_model, None, "no judge keeps the old behavior");
        let judged = store
            .create_eval_run(suite.id, "openai/gpt-4.1", Some(" openai/gpt-4.1-mini "))
            .await
            .expect("judged run");
        assert_eq!(
            judged.judge_model.as_deref(),
            Some("openai/gpt-4.1-mini"),
            "judge model is trimmed and round-trips"
        );
        let read = store.eval_run(judged.id).await.expect("read").expect("run");
        assert_eq!(read.judge_model, judged.judge_model);
    });
}

#[test]
fn deleting_a_suite_removes_cases_runs_and_results() {
    let rt = rt();
    rt.block_on(async {
        let store = store();
        let suite = store
            .create_eval_suite("core", None, None)
            .await
            .expect("suite");
        let case = store
            .create_eval_case(suite.id, None, "prompt", Some("answer"))
            .await
            .expect("case");
        let run = store
            .create_eval_run(suite.id, "openai/gpt-4.1", None)
            .await
            .expect("run");
        store
            .insert_eval_result(run.id, case.id, Some("answer"), Some(1.0), Some(5), None)
            .await
            .expect("result");

        assert!(store.delete_eval_suite(suite.id).await.expect("delete"));
        assert!(store.eval_case_count(suite.id).await.expect("count") == 0);
        assert!(store.eval_run(run.id).await.expect("run").is_none());
        assert!(store
            .eval_results(run.id)
            .await
            .expect("results")
            .is_empty());
    });
}

#[test]
fn expectations_are_a_case_insensitive_containment_test() {
    assert!(eval_passes("The capital is Paris.", "paris"));
    assert!(eval_passes("PARIS", "Paris"));
    assert!(!eval_passes("The capital is Lyon.", "paris"));
    assert!(
        eval_passes("anything", "   "),
        "an empty expectation is not a judgement"
    );
}

#[test]
fn a_restart_fails_the_runs_it_left_active() {
    let rt = rt();
    rt.block_on(async {
        let store = store();
        let suite = store
            .create_eval_suite("core", None, None)
            .await
            .expect("suite");
        let pending = store
            .create_eval_run(suite.id, "openai/gpt-4.1", None)
            .await
            .expect("pending");
        let running = store
            .create_eval_run(suite.id, "openai/gpt-4.1", None)
            .await
            .expect("running");
        store
            .start_eval_run(running.id)
            .await
            .expect("start")
            .expect("running");

        // A run created at the cutoff instant is live work, not an orphan.
        assert_eq!(
            store
                .fail_orphan_eval_runs(pending.created_at, "too early")
                .await
                .expect("no-op"),
            0,
            "a run created at the cutoff survives the sweep"
        );

        let swept = store
            .fail_orphan_eval_runs(pending.created_at + 1, "the gateway restarted")
            .await
            .expect("sweep");
        assert_eq!(swept, 2, "both active runs were swept");
        for id in [pending.id, running.id] {
            let run = store.eval_run(id).await.expect("read").expect("run");
            assert_eq!(run.status, EvalRunStatus::Failed);
            assert!(run.completed_at.is_some());
            let summary = run.summary.expect("summary");
            assert_eq!(summary["error"], "the gateway restarted");
            assert!(
                store
                    .start_eval_run(id)
                    .await
                    .expect("restart attempt")
                    .is_none(),
                "a swept run cannot be picked up again"
            );
        }
        // A second sweep finds nothing.
        assert_eq!(
            store
                .fail_orphan_eval_runs(running.created_at + 60, "again")
                .await
                .expect("idempotent"),
            0
        );
    });
}
