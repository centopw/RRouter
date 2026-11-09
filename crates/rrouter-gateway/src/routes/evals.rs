//! `/api/evals` — suites, cases and runs executed through the gateway's own `/v1` path.
//!
//! A run is asynchronous on purpose. Executing a suite is many completions; holding an
//! HTTP request open for that is how a dashboard request becomes the thing that times out.
//! `POST /api/evals/runs` records a `pending` run, returns `202`, and a background task
//! drives it — so the client polls one run's status instead of keeping a connection.
//!
//! # How a case is executed
//!
//! The runner calls [`crate::proxy::proxy`] in-process with a synthesized
//! `POST /v1/chat/completions` request. That is the same handler the server mounts on
//! `/v1`, so a case goes through routing, retries, guardrails, budgets and accounting
//! exactly like real traffic — evals are not a side channel around the gateway's rules.
//! The alternative (a raw provider call inside this route) would be a second, divergent
//! request path, which is why it is not used.
//!
//! Two consequences an operator must know:
//!
//! * With `RROUTER_REQUIRE_API_KEY=true` the runner needs a key, supplied per run as
//!   `api_key` in the create body. It is used as the inbound `Authorization` header for
//!   that run only: never stored, never echoed, and scrubbed from any text written to a
//!   result. Without a key the case records the gateway's own `missing API key` error.
//! * A run counts as gateway traffic: it consumes concurrency slots, key budgets and
//!   usage rows.
//!
//! # Bounds
//!
//! At most [`MAX_RUNNING`] runs execute at once (a run beyond that waits in `pending`),
//! each with at most [`RUN_PARALLELISM`] cases in flight, and every case has a
//! [`CASE_TIMEOUT`] deadline. Nothing here waits forever, and the HTTP API never waits at
//! all.

use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};
use futures_util::stream::{self, StreamExt};
use rrouter_core::Error;
use rrouter_store::{EvalCase, EvalResult, EvalRun, EvalRunStatus, EvalSuite, EvalSummary};
use serde_json::{json, Value};
use tokio::sync::Semaphore;
use uuid::Uuid;

use super::{admin_error, body_json, ok_json};
use crate::state::AppState;

/// Runs executing at once, process-wide. A third run waits in `pending`.
pub const MAX_RUNNING: usize = 2;
/// Cases one run has in flight. Enough to hide a slow model, small enough that a suite
/// cannot saturate the gateway's own concurrency budget.
pub const RUN_PARALLELISM: usize = 4;
/// Deadline for one case, including routing and retries.
pub const CASE_TIMEOUT: Duration = Duration::from_secs(90);
/// Tokens a case may answer with: evals judge a fact, not an essay.
const CASE_MAX_TOKENS: u32 = 256;
/// Largest upstream body a case will read before giving up on it.
const MAX_CASE_BODY: usize = 64 * 1024;

/// Process-wide slots for running suites. Fair, so runs start in creation order.
static RUNNING_SLOTS: LazyLock<Arc<Semaphore>> =
    LazyLock::new(|| Arc::new(Semaphore::new(MAX_RUNNING)));

// ----- views ---------------------------------------------------------------------

fn suite_json(suite: &EvalSuite, cases: usize) -> Value {
    json!({
        "id": suite.id.to_string(),
        "name": suite.name,
        "description": suite.description,
        "cases": cases,
        "created_at": suite.created_at,
        "updated_at": suite.updated_at,
    })
}

fn case_json(case: &EvalCase) -> Value {
    json!({
        "id": case.id.to_string(),
        "suite_id": case.suite_id.to_string(),
        "name": case.name,
        "input": case.input,
        "expected": case.expected,
        "created_at": case.created_at,
    })
}

fn run_json(run: &EvalRun, done: usize, total: usize) -> Value {
    json!({
        "id": run.id.to_string(),
        "suite_id": run.suite_id.to_string(),
        "model": run.model,
        "status": run.status.as_str(),
        "created_at": run.created_at,
        "completed_at": run.completed_at,
        "summary": run.summary,
        "progress": { "done": done, "total": total },
    })
}

fn result_json(result: &EvalResult) -> Value {
    json!({
        "id": result.id.to_string(),
        "run_id": result.run_id.to_string(),
        "case_id": result.case_id.to_string(),
        "case_name": result.case_name,
        "case_input": result.case_input,
        "output": result.output,
        "score": result.score,
        "passed": result.score.map(|score| score > 0.5),
        "latency_ms": result.latency_ms,
        "error": result.error,
        "created_at": result.created_at,
    })
}

fn suite_id(raw: &str) -> Result<Uuid, Error> {
    Uuid::parse_str(raw).map_err(|_| Error::BadRequest("suite id must be a uuid".into()))
}

// ----- suites --------------------------------------------------------------------

/// `GET /api/evals/suites` — every suite with its case count.
pub async fn list_suites(State(state): State<Arc<AppState>>) -> Response {
    let suites = match state.store.eval_suites().await {
        Ok(suites) => suites,
        Err(e) => return admin_error(&e),
    };
    let mut data = Vec::with_capacity(suites.len());
    for suite in &suites {
        match state.store.eval_case_count(suite.id).await {
            Ok(count) => data.push(suite_json(suite, count)),
            Err(e) => return admin_error(&e),
        }
    }
    ok_json(json!({ "data": data }))
}

/// `GET /api/evals/suites/{id}` — the suite, its cases and its recent runs.
pub async fn get_suite(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    let id = match suite_id(&id) {
        Ok(id) => id,
        Err(e) => return admin_error(&e),
    };
    let Some(suite) = (match state.store.eval_suite(id).await {
        Ok(suite) => suite,
        Err(e) => return admin_error(&e),
    }) else {
        return admin_error(&Error::NotFound(format!("suite {id}")));
    };
    let cases = match state.store.eval_cases(id).await {
        Ok(cases) => cases,
        Err(e) => return admin_error(&e),
    };
    let runs = match state.store.eval_runs(Some(id), 10).await {
        Ok(runs) => runs,
        Err(e) => return admin_error(&e),
    };
    let mut run_rows = Vec::with_capacity(runs.len());
    for run in &runs {
        let done = state.store.eval_result_count(run.id).await.unwrap_or(0);
        run_rows.push(run_json(run, done, cases.len()));
    }
    ok_json(json!({
        "data": suite_json(&suite, cases.len()),
        "cases": cases.iter().map(case_json).collect::<Vec<_>>(),
        "runs": run_rows,
    }))
}

/// `POST /api/evals/suites` — `{name, description?}`.
pub async fn create_suite(State(state): State<Arc<AppState>>, body: Body) -> Response {
    let value = match body_json(body).await {
        Ok(value) => value,
        Err(response) => return *response,
    };
    let Some(name) = value.get("name").and_then(Value::as_str) else {
        return admin_error(&Error::BadRequest("a suite name is required".into()));
    };
    let description = value.get("description").and_then(Value::as_str);
    match state.store.create_eval_suite(name, description).await {
        Ok(suite) => (
            StatusCode::CREATED,
            axum::Json(json!({ "data": suite_json(&suite, 0) })),
        )
            .into_response(),
        Err(e) => admin_error(&e),
    }
}

/// `PATCH /api/evals/suites/{id}` — `{name?, description?}`; `null` clears the description.
pub async fn update_suite(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    body: Body,
) -> Response {
    let id = match suite_id(&id) {
        Ok(id) => id,
        Err(e) => return admin_error(&e),
    };
    let value = match body_json(body).await {
        Ok(value) => value,
        Err(response) => return *response,
    };
    let name = value.get("name").and_then(Value::as_str);
    let description = value.get("description").map(|raw| raw.as_str());
    match state.store.update_eval_suite(id, name, description).await {
        Ok(Some(suite)) => {
            let cases = state.store.eval_case_count(id).await.unwrap_or(0);
            ok_json(json!({ "data": suite_json(&suite, cases) }))
        }
        Ok(None) => admin_error(&Error::NotFound(format!("suite {id}"))),
        Err(e) => admin_error(&e),
    }
}

/// `DELETE /api/evals/suites/{id}` — the suite, its cases, runs and results.
pub async fn delete_suite(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    let id = match suite_id(&id) {
        Ok(id) => id,
        Err(e) => return admin_error(&e),
    };
    match state.store.delete_eval_suite(id).await {
        Ok(true) => ok_json(json!({ "deleted": true, "id": id.to_string() })),
        Ok(false) => admin_error(&Error::NotFound(format!("suite {id}"))),
        Err(e) => admin_error(&e),
    }
}

// ----- cases ---------------------------------------------------------------------

/// `POST /api/evals/suites/{id}/cases` — `{input, expected?, name?}`.
pub async fn create_case(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    body: Body,
) -> Response {
    let id = match suite_id(&id) {
        Ok(id) => id,
        Err(e) => return admin_error(&e),
    };
    let value = match body_json(body).await {
        Ok(value) => value,
        Err(response) => return *response,
    };
    let Some(input) = value.get("input").and_then(Value::as_str) else {
        return admin_error(&Error::BadRequest("a case input is required".into()));
    };
    let expected = value.get("expected").and_then(Value::as_str);
    let name = value.get("name").and_then(Value::as_str);
    match state
        .store
        .create_eval_case(id, name, input, expected)
        .await
    {
        Ok(case) => (
            StatusCode::CREATED,
            axum::Json(json!({ "data": case_json(&case) })),
        )
            .into_response(),
        Err(e) => admin_error(&e),
    }
}

/// `DELETE /api/evals/suites/{id}/cases/{case_id}` — the suite is only there to scope the
/// URL; the case id is what is deleted.
pub async fn delete_case(
    State(state): State<Arc<AppState>>,
    Path((id, case_id)): Path<(String, String)>,
) -> Response {
    let suite = match suite_id(&id) {
        Ok(id) => id,
        Err(e) => return admin_error(&e),
    };
    let Ok(case_id) = Uuid::parse_str(&case_id) else {
        return admin_error(&Error::BadRequest("case id must be a uuid".into()));
    };
    let owned = match state.store.eval_cases(suite).await {
        Ok(cases) => cases.iter().any(|case| case.id == case_id),
        Err(e) => return admin_error(&e),
    };
    if !owned {
        return admin_error(&Error::NotFound(format!("case {case_id} in suite {suite}")));
    }
    match state.store.delete_eval_case(case_id).await {
        Ok(true) => ok_json(json!({ "deleted": true, "id": case_id.to_string() })),
        Ok(false) => admin_error(&Error::NotFound(format!("case {case_id}"))),
        Err(e) => admin_error(&e),
    }
}

// ----- runs ----------------------------------------------------------------------

/// `POST /api/evals/runs` — `{suite_id, model, api_key?}`, answered with `202`.
pub async fn start_run(State(state): State<Arc<AppState>>, body: Body) -> Response {
    let value = match body_json(body).await {
        Ok(value) => value,
        Err(response) => return *response,
    };
    let Some(suite) = value.get("suite_id").and_then(Value::as_str) else {
        return admin_error(&Error::BadRequest("suite_id is required".into()));
    };
    let suite = match suite_id(suite) {
        Ok(id) => id,
        Err(e) => return admin_error(&e),
    };
    let Some(model) = value.get("model").and_then(Value::as_str) else {
        return admin_error(&Error::BadRequest("model is required".into()));
    };
    let cases = match state.store.eval_case_count(suite).await {
        Ok(count) => count,
        Err(e) => return admin_error(&e),
    };
    if cases == 0 {
        return admin_error(&Error::BadRequest("the suite has no cases to run".into()));
    }
    let run = match state.store.create_eval_run(suite, model).await {
        Ok(run) => run,
        Err(e) => return admin_error(&e),
    };
    // The credential is used for this run's internal requests and nothing else: it is not
    // written, not echoed and not part of the run row.
    let api_key = value
        .get("api_key")
        .and_then(Value::as_str)
        .filter(|key| !key.trim().is_empty())
        .map(str::to_string);
    spawn_run(Arc::clone(&state), run.id, api_key);
    (
        StatusCode::ACCEPTED,
        axum::Json(json!({ "data": run_json(&run, 0, cases) })),
    )
        .into_response()
}

/// `GET /api/evals/runs[?suite_id=]` — newest first.
pub async fn list_runs(
    State(state): State<Arc<AppState>>,
    Query(query): Query<Vec<(String, String)>>,
) -> Response {
    let suite = match query
        .iter()
        .find(|(key, _)| key == "suite_id")
        .map(|(_, value)| value.trim())
        .filter(|value| !value.is_empty())
    {
        None => None,
        Some(raw) => match Uuid::parse_str(raw) {
            Ok(id) => Some(id),
            Err(_) => return admin_error(&Error::BadRequest("suite_id must be a uuid".into())),
        },
    };
    let runs = match state
        .store
        .eval_runs(suite, rrouter_store::EVAL_RUN_PAGE)
        .await
    {
        Ok(runs) => runs,
        Err(e) => return admin_error(&e),
    };
    let mut data = Vec::with_capacity(runs.len());
    for run in &runs {
        let done = state.store.eval_result_count(run.id).await.unwrap_or(0);
        let total = state.store.eval_case_count(run.suite_id).await.unwrap_or(0);
        data.push(run_json(run, done, total));
    }
    ok_json(json!({ "data": data }))
}

/// `GET /api/evals/runs/{id}` — the run, its results and its progress.
pub async fn get_run(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    let Ok(id) = Uuid::parse_str(&id) else {
        return admin_error(&Error::BadRequest("run id must be a uuid".into()));
    };
    let Some(run) = (match state.store.eval_run(id).await {
        Ok(run) => run,
        Err(e) => return admin_error(&e),
    }) else {
        return admin_error(&Error::NotFound(format!("run {id}")));
    };
    let results = match state.store.eval_results(id).await {
        Ok(results) => results,
        Err(e) => return admin_error(&e),
    };
    let total = match state.store.eval_case_count(run.suite_id).await {
        Ok(total) => total,
        Err(e) => return admin_error(&e),
    };
    ok_json(json!({
        "data": run_json(&run, results.len(), total),
        "results": results.iter().map(result_json).collect::<Vec<_>>(),
    }))
}

// ----- runner --------------------------------------------------------------------

/// Fail runs left active by a previous process, once, at startup.
///
/// The runner is in-process: a restart drops it, and a run left `running` would be polled
/// by the dashboard forever. Marking it `failed` with a reason is the honest state, and it
/// also means a duplicated runner cannot pick the run up later (`start_eval_run` only moves
/// `pending`).
pub(crate) fn sweep_orphan_runs(state: &Arc<AppState>) {
    if tokio::runtime::Handle::try_current().is_err() {
        // A library build with no runtime has no runs to sweep either.
        return;
    }
    // Runs created at or after this instant belong to the process we are starting.
    let cutoff = crate::now_unix();
    let store = Arc::clone(&state.store);
    tokio::spawn(async move {
        match store
            .fail_orphan_eval_runs(cutoff, "the gateway restarted before this run finished")
            .await
        {
            Ok(0) => {}
            Ok(n) => tracing::warn!(runs = n, "failed eval runs orphaned by a restart"),
            Err(e) => tracing::warn!(error = %e, "could not sweep orphaned eval runs"),
        }
    });
}

/// One case's outcome, before it becomes a row.
struct CaseOutcome {
    case_id: Uuid,
    output: Option<String>,
    score: Option<f64>,
    latency_ms: u32,
    error: Option<String>,
}

/// Start the background runner. Returns immediately; the run is driven to completion even
/// if the client never polls again.
pub(crate) fn spawn_run(state: Arc<AppState>, run_id: Uuid, api_key: Option<String>) {
    tokio::spawn(async move {
        // A run beyond `MAX_RUNNING` stays `pending` until a slot frees; the semaphore is
        // fair, so runs start in the order they were created.
        let Ok(_slot) = RUNNING_SLOTS.clone().acquire_owned().await else {
            return;
        };
        let run = match state.store.start_eval_run(run_id).await {
            Ok(Some(run)) => run,
            // Not pending any more: another runner owns it (or it was deleted).
            Ok(None) | Err(_) => return,
        };
        let started = Instant::now();
        let cases = match state.store.eval_cases(run.suite_id).await {
            Ok(cases) => cases,
            Err(e) => {
                let summary = EvalSummary {
                    cases: 0,
                    errors: 1,
                    error: Some(e.to_string()),
                    ..EvalSummary::default()
                };
                let _ = state
                    .store
                    .complete_eval_run(run_id, EvalRunStatus::Failed, summary)
                    .await;
                return;
            }
        };
        if cases.is_empty() {
            let summary = EvalSummary::default();
            let _ = state
                .store
                .complete_eval_run(run_id, EvalRunStatus::Completed, summary)
                .await;
            return;
        }

        let model = run.model.clone();
        let store = Arc::clone(&state.store);
        let outcomes: Vec<CaseOutcome> = stream::iter(cases.iter().cloned())
            .map(|case| {
                let state = Arc::clone(&state);
                let store = Arc::clone(&store);
                let model = model.clone();
                let key = api_key.clone();
                async move {
                    let outcome = execute_case(&state, &model, &case, key.as_deref()).await;
                    // Persist as each case finishes, so a long run is observable and a
                    // crash leaves the finished cases recorded.
                    let _ = store
                        .insert_eval_result(
                            run_id,
                            outcome.case_id,
                            outcome.output.as_deref(),
                            outcome.score,
                            Some(outcome.latency_ms),
                            outcome.error.as_deref(),
                        )
                        .await;
                    outcome
                }
            })
            .buffer_unordered(RUN_PARALLELISM)
            .collect()
            .await;

        let summary = summarise(&outcomes, started.elapsed());
        let _ = state
            .store
            .complete_eval_run(run_id, EvalRunStatus::Completed, summary)
            .await;
    });
}

/// Fold outcomes into the numbers a finished run reports.
fn summarise(outcomes: &[CaseOutcome], elapsed: Duration) -> EvalSummary {
    let mut summary = EvalSummary {
        cases: outcomes.len(),
        duration_ms: elapsed.as_millis() as u64,
        ..EvalSummary::default()
    };
    for outcome in outcomes {
        if outcome.error.is_some() {
            summary.errors += 1;
        } else if outcome.score.is_none() {
            summary.unscored += 1;
        } else if outcome.score == Some(1.0) {
            summary.passed += 1;
        } else {
            summary.failed += 1;
        }
    }
    summary
}

/// Run one case through the gateway's own completion handler.
async fn execute_case(
    state: &Arc<AppState>,
    model: &str,
    case: &EvalCase,
    api_key: Option<&str>,
) -> CaseOutcome {
    let body = json!({
        "model": model,
        "max_tokens": CASE_MAX_TOKENS,
        "messages": [{ "role": "user", "content": case.input }],
    });
    let mut builder = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json");
    if let Some(key) = api_key {
        builder = builder.header("authorization", format!("Bearer {key}"));
    }
    let Ok(request) = builder.body(Body::from(body.to_string())) else {
        return CaseOutcome {
            case_id: case.id,
            output: None,
            score: None,
            latency_ms: 0,
            error: Some("could not build the request".into()),
        };
    };
    let started = Instant::now();
    let response = tokio::time::timeout(
        CASE_TIMEOUT,
        crate::proxy::proxy(State(Arc::clone(state)), request),
    )
    .await;
    let latency_ms = started.elapsed().as_millis().min(u32::MAX as u128) as u32;

    let (output, error) = match response {
        Err(_) => (
            None,
            Some(format!("timed out after {}s", CASE_TIMEOUT.as_secs())),
        ),
        Ok(response) => {
            let status = response.status();
            let bytes = axum::body::to_bytes(response.into_body(), MAX_CASE_BODY)
                .await
                .unwrap_or_default();
            let raw = String::from_utf8_lossy(&bytes).to_string();
            let value: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
            if !status.is_success() {
                let message = value
                    .get("error")
                    .and_then(|error| error.get("message").or(Some(error)))
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("gateway answered {status}"));
                (None, Some(message))
            } else {
                match response_text(&value) {
                    Some(text) if !text.trim().is_empty() => (Some(text), None),
                    _ => (None, Some("the model returned no text".into())),
                }
            }
        }
    };

    // The run credential must never reach storage: scrub it from anything a provider or
    // the gateway might have echoed back.
    let output = output.map(|text| scrub(text, api_key));
    let error = error.map(|text| scrub(text, api_key));
    let score = match (&error, &case.expected, &output) {
        (None, Some(expected), Some(output)) => {
            Some(if rrouter_store::eval_passes(output, expected) {
                1.0
            } else {
                0.0
            })
        }
        _ => None,
    };
    CaseOutcome {
        case_id: case.id,
        output,
        score,
        latency_ms,
        error,
    }
}

/// The assistant text from an OpenAI-chat-shaped body.
fn response_text(value: &Value) -> Option<String> {
    let choice = value.get("choices")?.get(0)?;
    let content = choice.get("message").and_then(|m| m.get("content"));
    match content {
        Some(Value::String(text)) => Some(text.clone()),
        Some(Value::Array(parts)) => Some(
            parts
                .iter()
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join(""),
        ),
        _ => choice
            .get("text")
            .and_then(Value::as_str)
            .map(str::to_string),
    }
}

/// Replace the run credential with a marker if it appears in text.
fn scrub(text: String, api_key: Option<&str>) -> String {
    match api_key {
        Some(key) if !key.is_empty() && text.contains(key) => text.replace(key, "[redacted]"),
        _ => text,
    }
}
