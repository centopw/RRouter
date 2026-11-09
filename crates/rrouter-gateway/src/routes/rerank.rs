//! `POST /v1/rerank`: hosted rerank through the provider that serves it.
//!
//! Cohere and Jina take the same body and answer with the same ordered list, so this route
//! is a thin shell around `rrouter_translate::rerank`: parse the client's request into the
//! canonical form, pick a provider that actually declares a rerank endpoint, post the
//! canonical body to it, and re-frame the answer. Providers without an endpoint are
//! filtered out of the plan instead of having a path guessed for them, and a plan with none
//! answers `501`.
//!
//! Nothing here is metered: rerank reports search units or token counts that the gateway
//! has no price or column for, so the usage row records the request with zero tokens rather
//! than inventing a charge (see [`record_usage`]).

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Instant;

use axum::extract::{Request, State};
use axum::http::header::{HeaderName, CONTENT_TYPE};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use rrouter_auth::{Credentials, ProviderSpec};
use rrouter_core::{Dialect, Error};
use rrouter_router::{Candidate, Plan, PlanRequest, ProviderPolicy};
use rrouter_store::{AttemptRecord, UsageRecord};
use rrouter_translate::rerank::{self, RerankRequest, RerankResponse};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::state::{check_prompt, dialect_of, Slot};
use crate::{error, metrics, overloaded_response, AppState};

/// Client endpoint recorded on usage and attempt rows.
const ENDPOINT: &str = "/v1/rerank";

/// Same ceiling as the chat proxy: a rerank body is text, and a client that sends a corpus
/// gets the limit it would get on `/v1/chat/completions`.
const MAX_BODY_BYTES: usize = 64 * 1024 * 1024;

/// Upstream error bodies are truncated to this many bytes before they are stored or echoed.
const MAX_ERROR_BODY: usize = 4096;

/// What every attempt and every row of one request shares.
#[derive(Clone)]
struct Context {
    request_id: Uuid,
    key_id: Option<Uuid>,
    /// The routed ref the client asked for, as usage rows record it.
    model: String,
    /// Inbound headers, so the provider gets the allowlisted protocol betas.
    client: HeaderMap,
}

/// `POST /v1/rerank`.
pub async fn rerank(State(state): State<Arc<AppState>>, req: Request) -> Response {
    let inbound = dialect_of(req.uri());
    let request_id = Uuid::new_v4();
    let started = Instant::now();
    // The slot is taken before the body is read: an overloaded gateway must not buffer a
    // request it has already decided to refuse.
    let _permit = match state.try_slot() {
        Slot::Granted(permit) => permit,
        Slot::Full => return respond(overloaded_response(inbound), request_id),
    };
    let (parts, body) = req.into_parts();
    let raw = match axum::body::to_bytes(body, MAX_BODY_BYTES).await {
        Ok(bytes) => bytes,
        Err(e) => {
            return respond(
                error::error_response(
                    inbound,
                    &Error::BadRequest(format!("request body could not be read: {e}")),
                ),
                request_id,
            )
        }
    };
    match run(&state, &parts.headers, &raw, request_id, started).await {
        Ok(response) => respond(response, request_id),
        Err(err) => respond(error::error_response(inbound, &err), request_id),
    }
}

/// Every `/v1/*` response carries the id its usage and attempt rows are keyed by.
fn respond(mut response: Response, request_id: Uuid) -> Response {
    if let Ok(value) = HeaderValue::from_str(&request_id.to_string()) {
        response
            .headers_mut()
            .entry("x-request-id")
            .or_insert(value);
    }
    response
}

async fn run(
    state: &Arc<AppState>,
    headers: &HeaderMap,
    raw: &[u8],
    request_id: Uuid,
    started: Instant,
) -> rrouter_core::Result<Response> {
    let caller = state.authenticate(headers).await?;
    // A malformed body is the client's fault, so it is a 400 rather than the 502 a JSON
    // failure maps to elsewhere.
    let body: Value = serde_json::from_slice(raw)
        .map_err(|e| Error::BadRequest(format!("request body is not JSON: {e}")))?;
    let request = rerank::parse_request(&body)?;
    // Per-key mapping first, then governance and planning, exactly as `/v1/embeddings`
    // does: the allowlist and the routed ref both see the model this key asked for.
    let model = state.mapped_model(caller.key_id(), &request.model).await?;
    if caller.guardrails().is_some() {
        check_prompt(&caller, &guard_text(&request))?;
    }
    state.admit(&caller, &model).await?;

    let policy = ProviderPolicy::from_request(&body, |name| {
        headers.get(name).and_then(|value| value.to_str().ok())
    })?;
    let plan = state
        .planner
        .plan(
            state.catalog.as_ref(),
            state.store.as_ref(),
            PlanRequest {
                model: &model,
                stream: false,
                affinity: None,
                strategy: state.config.load().strategy,
                policy: policy.as_ref(),
                allowed_models: caller.allowed_models(),
                team_id: caller.team_id(),
                // Rerank is not part of the chat canary: the A/B rule reorders chat
                // candidates, and this route pins a provider by the endpoint contract.
                canary: None,
                canary_seed: 0,
            },
        )
        .await?;

    // Only a provider that declares the endpoint can answer; asking the next candidate
    // would answer with a NoRoute that stops the chain anyway. A `provider/model` ref is
    // already narrowed to that provider by the planner, which treats every compiled
    // provider — manifests included — as authoritative.
    let candidates: Vec<Candidate> = plan
        .candidates
        .iter()
        .filter(|candidate| rrouter_auth::registry::rerank_endpoint(&candidate.provider).is_some())
        .cloned()
        .collect();
    if candidates.is_empty() {
        return Ok(error::not_implemented(
            Dialect::OpenAiChat,
            "rerank for this provider",
        ));
    }
    let plan = Plan { candidates, ..plan };

    let request = Arc::new(request);
    let attempts = Arc::new(AtomicU32::new(0));
    let ctx = Context {
        request_id,
        key_id: caller.key_id(),
        model: plan.model.clone(),
        client: headers.clone(),
    };
    // The closure owns a copy so the request keeps one for its own rows.
    let attempt_ctx = ctx.clone();
    let outcome = state
        .dispatcher
        .run(&plan, move |candidate: Candidate| {
            let state = Arc::clone(state);
            let request = Arc::clone(&request);
            let attempts = Arc::clone(&attempts);
            let ctx = attempt_ctx.clone();
            async move { send_once(&state, &ctx, candidate, &request, &attempts).await }
        })
        .await;

    let (candidate, parsed) = match outcome {
        Ok(outcome) => match outcome.result {
            Ok(parsed) => (outcome.candidate, Ok(parsed)),
            Err(err) => (outcome.candidate, Err(err)),
        },
        Err(err) => {
            record_usage(
                state,
                &ctx,
                None,
                err.status(),
                Some(err.to_string()),
                elapsed_ms(started),
            )
            .await;
            return Err(err);
        }
    };

    match parsed {
        Ok(parsed) => {
            record_usage(
                state,
                &ctx,
                Some(&candidate),
                200,
                None,
                elapsed_ms(started),
            )
            .await;
            metrics::request_finished(&candidate.provider, &plan.model, 200, started.elapsed());
            Ok((StatusCode::OK, axum::Json(client_body(request_id, &parsed))).into_response())
        }
        Err(err) => {
            record_usage(
                state,
                &ctx,
                Some(&candidate),
                err.status(),
                Some(err.to_string()),
                elapsed_ms(started),
            )
            .await;
            metrics::request_finished(
                &candidate.provider,
                &plan.model,
                err.status(),
                started.elapsed(),
            );
            Err(err)
        }
    }
}

/// One upstream attempt: resolve the endpoint, post the canonical body, parse the answer.
async fn send_once(
    state: &AppState,
    ctx: &Context,
    candidate: Candidate,
    request: &RerankRequest,
    attempts: &AtomicU32,
) -> rrouter_core::Result<RerankResponse> {
    let spec = rrouter_auth::registry::get(&candidate.provider).ok_or_else(|| Error::NoRoute {
        model: candidate.model.clone(),
        detail: format!("unknown provider {}", candidate.provider),
    })?;
    let native = rrouter_auth::registry::rerank_endpoint(&candidate.provider).ok_or_else(|| {
        Error::NoRoute {
            model: candidate.model.clone(),
            detail: format!("{} has no rerank endpoint", candidate.provider),
        }
    })?;
    let creds = state
        .refresher
        .fresh(state.store.as_ref(), spec, candidate.connection)
        .await?;
    // A connection's own endpoint wins, keeping the reviewed path; a manifest-declared
    // query parameter fills the gap. One resolution shared with the files/batches routes.
    let url = rrouter_auth::capability_endpoint(native, creds.base_url_override.as_deref(), spec);
    let headers = outbound_headers(spec, &creds, &ctx.client)?;

    let mut wire = rerank::build_request(request);
    wire["model"] = Value::String(candidate.model.clone());
    let sent = Instant::now();
    let response = state
        .http
        .post(&url)
        .headers(headers)
        .json(&wire)
        .timeout(state.request_timeout())
        .send()
        .await
        .map_err(transport_error)?;
    let upstream_ms = elapsed_ms(sent);
    let status = response.status();
    let bytes = response.bytes().await.map_err(transport_error)?;
    let attempt = attempts.fetch_add(1, Ordering::Relaxed) + 1;

    if !status.is_success() {
        let err = upstream_error(&candidate, status.as_u16(), &bytes);
        record_attempt(
            state,
            ctx,
            attempt,
            &candidate,
            status.as_u16(),
            Some(err.to_string()),
            elapsed_ms(sent),
        )
        .await;
        metrics::attempt_finished(&candidate.provider, status.as_u16());
        return Err(err);
    }

    let value: Value = serde_json::from_slice(&bytes).map_err(|e| Error::Upstream {
        provider: candidate.provider.clone(),
        status: 502,
        body: format!("rerank response was not JSON: {e}"),
        payload: None,
    })?;
    // A 2xx body that is not a rerank answer is the provider's bug, not the client's: it is
    // a 502 so the dispatcher fails over instead of blaming the caller with a 400.
    let parsed = rerank::parse_response(&value, &candidate.model).map_err(|e| Error::Upstream {
        provider: candidate.provider.clone(),
        status: 502,
        body: e.to_string(),
        payload: Some(value),
    })?;

    record_attempt(state, ctx, attempt, &candidate, 200, None, upstream_ms).await;
    metrics::attempt_finished(&candidate.provider, 200);
    Ok(parsed)
}

/// The client-facing body: the canonical result, with the document flattened to text.
fn client_body(request_id: Uuid, parsed: &RerankResponse) -> Value {
    let results: Vec<Value> = parsed
        .results
        .iter()
        .map(|result| {
            let mut item = json!({
                "index": result.index,
                "relevance_score": result.relevance_score,
            });
            if let Some(document) = &result.document {
                item["document"] = Value::String(document.clone());
            }
            item
        })
        .collect();
    json!({
        "id": request_id.to_string(),
        "model": parsed.model,
        "results": results,
        "usage": {
            "input_tokens": parsed.usage.input_tokens,
            "total_tokens": parsed.usage.total_tokens,
            "search_units": parsed.usage.search_units,
        },
    })
}

/// Provider headers, plus the client's allowlisted protocol betas.
fn outbound_headers(
    spec: &ProviderSpec,
    creds: &Credentials,
    client: &HeaderMap,
) -> rrouter_core::Result<HeaderMap> {
    let mut pairs = rrouter_auth::build_headers(spec, creds)?;
    rrouter_auth::merge_client_headers(spec, &mut pairs, client);
    let mut headers = HeaderMap::new();
    for (name, value) in pairs {
        if let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_str(&value),
        ) {
            headers.append(name, value);
        }
    }
    headers
        .entry(CONTENT_TYPE)
        .or_insert(HeaderValue::from_static("application/json"));
    Ok(headers)
}

fn transport_error(e: reqwest::Error) -> Error {
    if e.is_timeout() {
        Error::Timeout(e.to_string())
    } else {
        Error::Transport(e.to_string())
    }
}

/// The provider's own failure, body and parsed payload intact: `error_body` re-frames the
/// payload (Cohere's `{message,id}`, Jina's `{detail,code}`) instead of flattening it.
fn upstream_error(candidate: &Candidate, status: u16, bytes: &[u8]) -> Error {
    let text = String::from_utf8_lossy(bytes);
    let body = if text.len() > MAX_ERROR_BODY {
        let mut cut = MAX_ERROR_BODY;
        while cut > 0 && !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text[..cut].to_string()
    } else {
        text.into_owned()
    };
    Error::Upstream {
        provider: candidate.provider.clone(),
        status,
        body,
        payload: serde_json::from_slice(bytes).ok(),
    }
}

/// What the input guardrails read: the query and every document, as the model would.
fn guard_text(request: &RerankRequest) -> String {
    let mut text = String::with_capacity(request.query.len() + 64);
    text.push_str(&request.query);
    for document in &request.documents {
        text.push('\n');
        text.push_str(document);
    }
    text
}

fn elapsed_ms(started: Instant) -> u32 {
    u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX)
}

/// One usage row per request, with **zero** tokens.
///
/// Rerank is not metered here: Cohere bills search units and Jina reports `total_tokens`,
/// but neither maps to a price the registry knows or to a column this row could carry
/// honestly. Recording the request with `0/0` tokens keeps the activity log, per-key spend
/// windows and team budgets truthful — a fabricated token count would spend a budget for
/// work the gateway cannot price.
async fn record_usage(
    state: &AppState,
    ctx: &Context,
    candidate: Option<&Candidate>,
    status: u16,
    error: Option<String>,
    latency_ms: u32,
) {
    let record = UsageRecord {
        id: None,
        ts: crate::now_unix(),
        request_id: Some(ctx.request_id),
        // This route has no W3C trace context yet: its rows join the activity log on
        // `request_id` only. The column stays NULL rather than inventing an id.
        trace_id: None,
        provider: candidate
            .map(|candidate| candidate.provider.clone())
            .unwrap_or_default(),
        model: ctx.model.clone(),
        connection_id: candidate.map(|candidate| candidate.connection),
        endpoint: Some(ENDPOINT.to_string()),
        key_id: ctx.key_id,
        cost_micros: 0,
        input_tokens: 0,
        output_tokens: 0,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        latency_ms,
        ttft_ms: 0,
        upstream_ms: 0,
        status,
        streamed: false,
        error,
    };
    if let Err(e) = state.store.record_usage(&record).await {
        tracing::warn!(error = %e, "rerank usage row failed");
    }
}

async fn record_attempt(
    state: &AppState,
    ctx: &Context,
    attempt: u32,
    candidate: &Candidate,
    status: u16,
    error: Option<String>,
    latency_ms: u32,
) {
    let record = AttemptRecord {
        id: None,
        request_id: ctx.request_id,
        ts: crate::now_unix(),
        attempt,
        provider: candidate.provider.clone(),
        model: candidate.model.clone(),
        connection_id: Some(candidate.connection),
        status,
        error,
        latency_ms,
        key_id: ctx.key_id,
    };
    if let Err(e) = state.store.record_attempt(&record).await {
        tracing::warn!(error = %e, "rerank attempt row failed");
    }
}
