//! Tests for routing: resolution grammar, round-robin, breaker, fallback,
//! streaming commit semantics, and the attempt cap.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;

use rrouter_core::{ChatResponse, Error, FinishReason, Usage};
use rrouter_router::breaker::{BreakerState, ProbeTicket};
use rrouter_router::dispatch::AttemptOutcome;
use rrouter_router::stream_dispatch::{byte_stream, collect_stream};
use rrouter_router::{Breaker, Candidate, Catalog, Dispatcher, Plan, Planner};
use rrouter_store::Store;
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
    Error::Upstream { provider: "p".into(), status, body: String::new(), payload: None }
}

async fn conn(store: &Store, provider: &str, model_hint: &str, priority: i32) -> Uuid {
    let c = store
        .create_connection(provider, "api_key", Default::default(), Some(model_hint))
        .await
        .expect("create connection");
    store.set_connection_priority(c.id, priority).await.expect("priority");
    c.id
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
    store.upsert_alias("fast", "claude/primary-x").await.unwrap();
    store.upsert_alias("claude/primary-x", "claude/never-reached").await.unwrap();
    let planner = Planner::new();
    let plan = planner
        .plan(&Catalog::builtin(), &store, "fast", false, None)
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
    let plan = planner.plan(&Catalog::builtin(), &store, "loop", false, None).await.unwrap();
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
        .plan(&Catalog::builtin(), &store, "claude/claude-opus-5-5", false, None)
        .await
        .unwrap();
    assert_eq!(plan.candidates.len(), 1);
    assert_eq!(plan.candidates[0].provider, "claude");
    assert_eq!(plan.candidates[0].model, "claude-opus-5-5");
    assert_eq!(plan.candidates[0].connection, claude);

    // Unprefixed ids keep the old behaviour: any active provider may serve them.
    let bare = planner
        .plan(&Catalog::builtin(), &store, "mystery-model", false, None)
        .await
        .unwrap();
    let providers: Vec<&str> = bare.candidates.iter().map(|c| c.provider.as_str()).collect();
    assert!(providers.contains(&"claude"));
    assert!(providers.contains(&"codex"));
}

#[tokio::test]
async fn explicit_provider_wins_over_catalog_guess() {
    let store = Store::open_in_memory().unwrap();
    let _ = conn(&store, "openai", "gpt-4.1", 0).await;
    let id = conn(&store, "github", "gpt-4.1", 0).await;
    let planner = Planner::new();
    let plan = planner
        .plan(&Catalog::builtin(), &store, "github/gpt-4.1", false, None)
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
    store.upsert_combo("chain", &["xai/grok-4".into(), "openai/gpt-5".into()]).await.unwrap();
    let planner = Planner::new();
    let plan = planner.plan(&Catalog::builtin(), &store, "combo:chain", false, None).await.unwrap();
    assert_eq!(plan.candidates.len(), 2);
    assert_eq!(plan.candidates[0].connection, a);
    assert_eq!(plan.candidates[1].connection, b);

    // Bare combo name resolves the same way.
    let bare = planner.plan(&Catalog::builtin(), &store, "chain", false, None).await.unwrap();
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
    let first = planner.plan(&catalog, &store, "openai/gpt-5", false, None).await.unwrap();
    let second = planner.plan(&catalog, &store, "openai/gpt-5", false, None).await.unwrap();
    assert_ne!(first.candidates[0].connection, second.candidates[0].connection);
    assert_eq!(
        std::collections::HashSet::from([first.candidates[0].connection, second.candidates[0].connection]),
        std::collections::HashSet::from([a, b])
    );
    // Priority still dominates: the priority-5 connection is always last.
    assert_eq!(first.candidates[2].connection, c);
    assert_eq!(second.candidates[2].connection, c);
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
    let first_for = |key: u64| planner.plan(&catalog, &store, "openai/gpt-5", false, Some(key));

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
    assert_eq!(accounts, std::collections::HashSet::from([a, b]), "affinity stopped spreading");
}

#[tokio::test]
async fn disabled_model_is_excluded() {
    let store = Store::open_in_memory().unwrap();
    let _ = conn(&store, "openai", "gpt-5", 0).await;
    store.set_disabled("gpt-5", true).await.unwrap();
    let planner = Planner::new();
    let err = planner.plan(&Catalog::builtin(), &store, "openai/gpt-5", false, None).await.unwrap_err();
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
    assert!(wait > Duration::ZERO && wait <= Duration::from_secs(30), "{wait:?}");
    assert!(matches!(breaker.state(id), BreakerState::Open { .. }));

    // After the window: first call allowed as the single half-open probe.
    let later = Instant::now() + Duration::from_secs(31);
    assert!(breaker.check_at(id, later).is_none());
    assert!(matches!(breaker.state_at(id, later), BreakerState::HalfOpen { .. }));
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
    assert!(matches!(breaker.claim_at(id, later), Ok(ProbeTicket::Probe { .. })));
    // ...everyone else is told to wait, including via the read path.
    assert!(breaker.claim_at(id, later).is_err());
    assert!(breaker.check_at(id, later).is_some());
    assert!(!breaker.is_available(id));

    // A failed probe re-opens the window for the full 30 s.
    breaker.record_failure_at(id, 500, None, later);
    assert_eq!(breaker.check_at(id, later).unwrap().as_secs(), 30);

    // A successful probe closes it and restores full eligibility.
    let later = later + Duration::from_secs(31);
    assert!(matches!(breaker.claim_at(id, later), Ok(ProbeTicket::Probe { .. })));
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
    assert!(matches!(breaker.claim_at(id, later), Ok(ProbeTicket::Probe { .. })));
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
    assert!(breaker.claim(conn).is_ok(), "no probe slot may be left taken");
}

#[test]
fn rate_limit_backoff_is_exponential_and_honors_retry_after() {
    let breaker = Breaker::new();
    let id = Uuid::new_v4();
    let now = Instant::now();
    let mut last = Duration::ZERO;
    for expected in [1u64, 2, 4] {
        breaker.record_failure_at(id, 429, None, now);
        let wait = breaker.check_at(id, now).expect("429 opens");
        assert_eq!(wait.as_secs(), expected);
        assert!(wait >= last);
        last = wait;
    }

    // Retry-After wins over the computed backoff, capped at 240 s.
    breaker.record_failure_at(id, 429, Some(Duration::from_secs(7)), now);
    assert_eq!(breaker.check_at(id, now).unwrap().as_secs(), 7);
    breaker.record_success(id);
    breaker.record_failure_at(id, 429, Some(Duration::from_secs(9_000)), now);
    assert_eq!(breaker.check_at(id, now).unwrap().as_secs(), 240);

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
        .run(&plan, |_c| {
            let n = seen.fetch_add(1, Ordering::SeqCst);
            async move { if n == 0 { Err(upstream(503)) } else { Ok(resp("m")) } }
        })
        .await
        .unwrap();
    assert_eq!(outcome.result.unwrap().model, "m");
    assert_eq!(outcome.candidate.connection, plan.candidates[1].connection);
    assert_eq!(seen.load(Ordering::SeqCst), 2);
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
        .run(&plan, |_c| {
            let n = seen.fetch_add(1, Ordering::SeqCst);
            async move {
                if n == 0 {
                    Err(Error::Unauthorized("provider claude refresh failed".into()))
                } else {
                    Ok(resp("m"))
                }
            }
        })
        .await
        .unwrap();
    assert_eq!(seen.load(Ordering::SeqCst), 2, "the dead account must not end the request");
    assert_eq!(outcome.candidate.connection, plan.candidates[1].connection);
    assert_eq!(outcome.result.unwrap().model, "m");
    // A 401-class failure parks the dead account hard (5 min).
    assert!(matches!(dispatcher.breaker().state(dead), BreakerState::Open { .. }));
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
    assert!(matches!(outcome.result, Err(Error::Upstream { status: 400, .. })));
    assert!(dispatcher.breaker().is_available(plan.candidates[0].connection));
}

#[tokio::test]
async fn streaming_retries_before_first_byte() {
    let dispatcher = Dispatcher::new(Breaker::new());
    let plan = plan_of(2);
    let seen = AtomicUsize::new(0);
    let (winner, mut stream) = dispatcher
        .run_streaming(&plan, |_c| {
            let n = seen.fetch_add(1, Ordering::SeqCst);
            async move {
                if n == 0 {
                    Err(upstream(503))
                } else {
                    Ok(byte_stream(futures_util::stream::iter(vec![Ok(Bytes::from_static(b"hello"))])))
                }
            }
        })
        .await
        .unwrap();
    assert_eq!(seen.load(Ordering::SeqCst), 2);
    assert_eq!(winner.connection, plan.candidates[1].connection);
    let body = collect_stream(std::mem::replace(&mut stream, byte_stream(futures_util::stream::empty())))
        .await
        .unwrap();
    assert_eq!(body, b"hello");
}

#[tokio::test]
async fn streaming_midstream_error_is_terminal_and_not_retried() {
    let dispatcher = Dispatcher::new(Breaker::new());
    let plan = plan_of(2);
    let seen = AtomicUsize::new(0);
    let (winner, stream) = dispatcher
        .run_streaming(&plan, |_c| {
            seen.fetch_add(1, Ordering::SeqCst);
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
    let err = collect_stream(stream).await.unwrap_err();
    assert!(matches!(err, Error::Upstream { status: 502, .. }));
    assert_eq!(seen.load(Ordering::SeqCst), 1, "no retry once bytes were emitted");
    // One 502 is recorded against the connection (three would open the breaker).
    assert_eq!(dispatcher.breaker().state(winner.connection), BreakerState::Closed);
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
    assert_eq!(seen.load(Ordering::SeqCst), 1, "open breaker must not be sent to");
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
    assert!(started.elapsed() < Duration::from_millis(100), "{:?}", started.elapsed());
    assert_eq!(outcome.candidate.connection, healthy);
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
    let outcome = dispatcher.run(&plan, |_c| async move { Ok(resp("m")) }).await.unwrap();
    assert_eq!(outcome.candidate.connection, only);
    assert!(started.elapsed() >= Duration::from_millis(250), "{:?}", started.elapsed());
    assert!(started.elapsed() <= Duration::from_secs(3), "{:?}", started.elapsed());
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
    assert!(cancelled.is_err(), "the attempt must have been cancelled mid-flight");

    // The probe slot must be free again; a leaked claim would report PROBE_TAKEN.
    assert!(
        breaker.check(conn).is_none(),
        "a cancelled attempt must not leave the probe slot taken"
    );
    assert!(breaker.claim(conn).is_ok(), "account must be probeable again");
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
    assert!(dispatcher.breaker().claim(conn).is_ok(), "account must stay eligible");
}
