//! `POST /v1/search`: provider-native web search through an existing connection.
//!
//! The search *is* a model call: Anthropic's Messages endpoint with the `web_search`
//! server tool, OpenAI's Responses endpoint with the `web_search` tool (also used by
//! Codex and xAI), or Gemini's `generateContent` with Google Search grounding. The route
//! therefore reuses the same planner, credentials, breaker and failover as chat, and
//! records a normal usage row with the tokens the provider reports.
//!
//! Search is not translated into a chat request: the provider's search tool requires its
//! own request shape, and the citation data it returns has no place in a chat body. One
//! provider without a search surface is filtered out of the plan; a plan with none answers
//! `501` instead of receiving a guessed tool.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Instant;

use axum::extract::{Request, State};
use axum::http::header::{HeaderName, CONTENT_TYPE};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use rrouter_auth::{Credentials, ProviderSpec};
use rrouter_core::sse::SseCodec;
use rrouter_core::{Dialect, Error};
use rrouter_router::{Candidate, Plan, PlanRequest};
use rrouter_store::{AttemptRecord, UsageRecord};
use rrouter_translate::search::{self, SearchResponse, SearchUsage};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::state::{check_prompt, Slot};
use crate::{error, metrics, overloaded_response, AppState};

/// Client endpoint recorded on usage and attempt rows.
const ENDPOINT: &str = "/v1/search";

/// Search bodies are a query, not a document.
const MAX_BODY_BYTES: usize = 1024 * 1024;

/// Upstream error bodies are truncated before they are stored or echoed.
const MAX_ERROR_BODY: usize = 4096;

/// Gemini's native search surface. The model is substituted into the reviewed path, then
/// a connection override moves the origin while keeping that path.
const GEMINI_SEARCH_URL: &str =
    "https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent";

/// What every attempt and every row of one request shares.
#[derive(Clone)]
struct Context {
    request_id: Uuid,
    key_id: Option<Uuid>,
    /// The routed ref the client asked for, as usage rows record it.
    model: String,
    query: String,
    max_results: u32,
    domain_filter: Vec<String>,
    /// Serializes budgeted requests until this row is durable.
    budget_hold: Option<Arc<crate::state::BudgetHold>>,
}

/// `POST /v1/search`.
pub async fn search(State(state): State<Arc<AppState>>, req: Request) -> Response {
    let inbound = Dialect::OpenAiChat;
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
    let request = search::parse_request(&body)?;
    // The key's mapping and allowlist apply to the routed ref, exactly as they do for
    // chat. A provider id is expanded to its default search model before admission so
    // the key is judged on the model that will actually be sent.
    let requested = state.mapped_model(caller.key_id(), &request.model).await?;
    // Resolve one alias before provider expansion: an alias may target a provider id
    // (`fast -> claude`), and the default search model must still be filled in.
    let requested = state.resolve_alias(&requested).await?;
    let Some(model) = routed_model(&requested) else {
        return Ok(error::not_implemented(
            Dialect::OpenAiChat,
            &format!("web search for provider {requested}"),
        ));
    };
    if caller.guardrails().is_some() {
        check_prompt(&caller, &request.query)?;
    }
    let budget_hold = state.budget_hold(&caller).await?;
    state.admit(&caller, &model).await?;

    let config = state.config.load();
    let plan = state
        .planner
        .plan(
            state.catalog.as_ref(),
            state.store.as_ref(),
            PlanRequest {
                model: &model,
                stream: false,
                affinity: None,
                strategy: config.strategy,
                // Search has no provider-policy object: the body is a query, not a chat
                // request. The operator's priority, strategy and key allowlist still
                // decide the plan.
                policy: None,
                allowed_models: caller.allowed_models(),
                team_id: caller.team_id(),
                // The chat canary reorders chat candidates. Search is a different
                // capability, so it keeps the operator's static routing.
                canary: None,
                canary_seed: 0,
                combo_start: None,
            },
        )
        .await?;

    // Only a provider with a native search surface can answer. Asking the next candidate
    // would answer with a NoRoute that stops the chain anyway.
    let candidates: Vec<Candidate> = plan
        .candidates
        .iter()
        .filter(|candidate| rrouter_auth::registry::search_spec(&candidate.provider).is_some())
        .cloned()
        .collect();
    if candidates.is_empty() {
        return Ok(error::not_implemented(
            Dialect::OpenAiChat,
            &format!("web search for model {model}"),
        ));
    }
    let plan = Plan { candidates, ..plan };

    let ctx = Context {
        request_id,
        key_id: caller.key_id(),
        model: plan.model.clone(),
        query: request.query,
        max_results: request.max_results,
        domain_filter: request.domain_filter,
        budget_hold,
    };
    let attempts = Arc::new(AtomicU32::new(0));
    let attempt_ctx = ctx.clone();
    let outcome = state
        .dispatcher
        .run(&plan, move |candidate: Candidate| {
            let state = Arc::clone(state);
            let attempts = Arc::clone(&attempts);
            let ctx = attempt_ctx.clone();
            async move { send_once(&state, &ctx, candidate, &attempts).await }
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
                SearchUsage::default(),
                err.status(),
                Some(err.to_string()),
                elapsed_ms(started),
                0,
            )
            .await;
            return Err(err);
        }
    };

    let parsed = match parsed {
        Ok(parsed) => parsed,
        Err(err) => {
            record_usage(
                state,
                &ctx,
                Some(&candidate),
                SearchUsage::default(),
                err.status(),
                Some(err.to_string()),
                elapsed_ms(started),
                0,
            )
            .await;
            metrics::request_finished(
                &candidate.provider,
                &plan.model,
                err.status(),
                started.elapsed(),
            );
            return Err(err);
        }
    };

    // Search answers are model output. A key that scans output must see the answer and
    // every snippet before either reaches the client; a hit is a 403, not a silent drop.
    if let Some(policy) = caller.output_guardrails() {
        if let Some(term) = policy.hit(&search_text(&parsed)) {
            let error = Error::Forbidden {
                code: "output_blocked",
                message: format!("the search response contains the blocked term {term:?}"),
            };
            metrics::guardrail_blocked(metrics::Stage::Output);
            record_usage(
                state,
                &ctx,
                Some(&candidate),
                parsed.usage,
                403,
                Some(error.to_string()),
                elapsed_ms(started),
                0,
            )
            .await;
            metrics::request_finished(&candidate.provider, &plan.model, 403, started.elapsed());
            return Err(error);
        }
    }

    record_usage(
        state,
        &ctx,
        Some(&candidate),
        parsed.usage,
        200,
        None,
        elapsed_ms(started),
        0,
    )
    .await;
    metrics::request_finished(&candidate.provider, &plan.model, 200, started.elapsed());
    Ok((
        StatusCode::OK,
        axum::Json(client_body(&ctx, &candidate, &parsed)),
    )
        .into_response())
}

/// Expand a provider id to its default search model; leave a model ref or combo alone.
///
/// `None` means the client named a provider that has no native search surface, which is a
/// `501` before any planning or upstream call.
fn routed_model(requested: &str) -> Option<String> {
    if let Some((provider, _)) = requested.split_once('/') {
        if rrouter_auth::registry::get(provider).is_some() {
            return Some(requested.to_string());
        }
    }
    if rrouter_auth::registry::get(requested).is_some() {
        return rrouter_auth::registry::search_spec(requested)
            .map(|search| format!("{requested}/{}", search.default_model));
    }
    Some(requested.to_string())
}

/// One upstream attempt: resolve the endpoint, post the provider's search body, parse it.
async fn send_once(
    state: &AppState,
    ctx: &Context,
    candidate: Candidate,
    attempts: &AtomicU32,
) -> rrouter_core::Result<SearchResponse> {
    let spec = rrouter_auth::registry::get(&candidate.provider).ok_or_else(|| Error::NoRoute {
        model: candidate.model.clone(),
        detail: format!("unknown provider {}", candidate.provider),
    })?;
    let search =
        rrouter_auth::registry::search_spec(&candidate.provider).ok_or_else(|| Error::NoRoute {
            model: candidate.model.clone(),
            detail: format!("{} has no native web search", candidate.provider),
        })?;
    let creds = state
        .refresher
        .fresh(state.store.as_ref(), spec, candidate.connection)
        .await?;
    let url = search_url(&candidate, spec, &creds, search.mode)?;
    let headers = outbound_headers(spec, &creds, search.mode)?;
    let request = search::SearchRequest {
        model: candidate.model.clone(),
        query: ctx.query.clone(),
        max_results: ctx.max_results,
        domain_filter: ctx.domain_filter.clone(),
    };
    let mut wire = search::build_request(search.mode, &request);
    // Codex answers a non-streaming Responses request with an error. Its backend is
    // stream-only, so ask for the SSE form and reassemble the completed response below.
    if spec.stream_only {
        wire["stream"] = Value::Bool(true);
    }

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
            upstream_ms,
        )
        .await;
        metrics::attempt_finished(&candidate.provider, status.as_u16());
        return Err(err);
    }

    let value = if spec.stream_only {
        streamed_response(&bytes, &candidate)?
    } else {
        serde_json::from_slice(&bytes).map_err(|e| Error::Upstream {
            provider: candidate.provider.clone(),
            status: 502,
            body: format!("search response was not JSON: {e}"),
            payload: None,
        })?
    };
    // A 2xx body that is not the provider's search shape is the provider's bug, not the
    // client's: it is a 502 so the dispatcher fails over instead of blaming the caller.
    let mut parsed = search::parse_response(search.mode, &value).map_err(|e| Error::Upstream {
        provider: candidate.provider.clone(),
        status: 502,
        body: e.to_string(),
        payload: Some(value),
    })?;
    parsed.results.truncate(ctx.max_results as usize);

    record_attempt(state, ctx, attempt, &candidate, 200, None, upstream_ms).await;
    metrics::attempt_finished(&candidate.provider, 200);
    Ok(parsed)
}

/// The reviewed endpoint, moved to the connection's origin when it overrides one.
fn search_url(
    candidate: &Candidate,
    spec: &ProviderSpec,
    creds: &Credentials,
    mode: search::SearchMode,
) -> rrouter_core::Result<String> {
    let native = match mode {
        search::SearchMode::Anthropic => spec.messages_url,
        search::SearchMode::OpenAiResponses => spec.responses_url,
        search::SearchMode::Gemini => Some(GEMINI_SEARCH_URL),
    }
    .ok_or_else(|| Error::NoRoute {
        model: candidate.model.clone(),
        detail: format!(
            "{} has no endpoint for native web search",
            candidate.provider
        ),
    })?;

    let native = if mode == search::SearchMode::Gemini {
        // The model is part of the path. Restrict it to the character set every Gemini
        // model id uses so a crafted `model` cannot alter the URL.
        if !candidate
            .model
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
        {
            return Err(Error::BadRequest(format!(
                "model {} is not a valid Gemini model id for search",
                candidate.model
            )));
        }
        native.replace("{model}", &candidate.model)
    } else {
        native.to_string()
    };
    Ok(rrouter_auth::capability_endpoint(
        &native,
        creds.base_url_override.as_deref(),
        spec,
    ))
}

/// Provider headers, plus Gemini's native API-key header.
///
/// The generic builder presents an API key as a bearer token, which is right for every
/// provider except Gemini's native endpoint: Google expects `x-goog-api-key` there.
fn outbound_headers(
    spec: &ProviderSpec,
    creds: &Credentials,
    mode: search::SearchMode,
) -> rrouter_core::Result<HeaderMap> {
    let mut pairs = rrouter_auth::build_headers(spec, creds)?;
    if mode == search::SearchMode::Gemini {
        pairs.retain(|(name, _)| !name.eq_ignore_ascii_case("authorization"));
        let key = creds
            .api_key
            .as_deref()
            .or(creds.access_token.as_deref())
            .ok_or_else(|| Error::Unauthorized("Gemini search needs an API key".into()))?;
        pairs.push(("x-goog-api-key".to_string(), key.to_string()));
    }
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

/// Reassemble a stream-only provider's SSE response.
///
/// Codex emits `response.completed` with the full Responses object. The event is the
/// only place the search body exists; the individual deltas are a display concern. A
/// plain JSON body is accepted too, so a provider that ignores `stream` still works.
fn streamed_response(bytes: &[u8], candidate: &Candidate) -> rrouter_core::Result<Value> {
    if let Ok(value) = serde_json::from_slice(bytes) {
        return Ok(value);
    }
    let mut codec = SseCodec::new();
    codec.push(bytes);
    loop {
        let frame = match codec.next_frame() {
            Some(frame) => Some(frame),
            None => codec.finish(),
        };
        let Some(frame) = frame else {
            break;
        };
        if frame.is_done() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(&frame.data) else {
            continue;
        };
        if value.get("type").and_then(Value::as_str) == Some("response.completed") {
            if let Some(response) = value.get("response") {
                return Ok(response.clone());
            }
        }
    }
    Err(Error::Upstream {
        provider: candidate.provider.clone(),
        status: 502,
        body: "stream ended without a completed search response".to_string(),
        payload: None,
    })
}

fn transport_error(e: reqwest::Error) -> Error {
    if e.is_timeout() {
        Error::Timeout(e.to_string())
    } else {
        Error::Transport(e.to_string())
    }
}

/// The provider's own failure, body and payload intact.
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

/// What output guardrails read: the answer and every citation's text.
fn search_text(parsed: &SearchResponse) -> String {
    let mut text = parsed.answer.clone();
    for result in &parsed.results {
        text.push('\n');
        text.push_str(&result.title);
        text.push('\n');
        text.push_str(&result.url);
        text.push('\n');
        text.push_str(&result.snippet);
        if let Some(content) = &result.content {
            text.push('\n');
            text.push_str(content);
        }
    }
    text
}

/// The client-facing body, shaped like 9Router's `/v1/search` answer where the fields
/// overlap: provider, query, results, answer, usage, metrics, errors.
fn client_body(ctx: &Context, candidate: &Candidate, parsed: &SearchResponse) -> Value {
    let retrieved_at = crate::now_unix();
    let results: Vec<Value> = parsed
        .results
        .iter()
        .enumerate()
        .map(|(index, result)| {
            let position = index + 1;
            json!({
                "title": result.title,
                "url": result.url,
                "snippet": result.snippet,
                "position": position,
                "score": null,
                "published_at": result.published_at,
                "favicon_url": null,
                "content": result.content,
                "metadata": {},
                "citation": {
                    "provider": candidate.provider,
                    "retrieved_at": retrieved_at,
                    "rank": position,
                },
            })
        })
        .collect();
    json!({
        "provider": candidate.provider,
        "model": candidate.model,
        "query": ctx.query,
        "results": results,
        "answer": {
            "source": candidate.provider,
            "model": candidate.model,
            "text": parsed.answer,
        },
        "usage": {
            "queries_used": 1,
            "search_cost_usd": 0,
            "input_tokens": parsed.usage.input_tokens,
            "output_tokens": parsed.usage.output_tokens,
            "llm_tokens": parsed.usage.input_tokens
                .saturating_add(parsed.usage.output_tokens),
        },
        "metrics": {
            // Native search does not report a corpus-wide hit count; only the citations
            // actually returned are known.
            "total_results_available": null,
            "returned_results": parsed.results.len(),
        },
        "errors": [],
    })
}

fn elapsed_ms(started: Instant) -> u32 {
    u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX)
}

/// One usage row per request. A failed search records zero tokens; a successful one
/// records what the provider reported and lets the registry price the model tokens. The
/// search tool's own fee is not invented here.
#[allow(clippy::too_many_arguments)]
async fn record_usage(
    state: &AppState,
    ctx: &Context,
    candidate: Option<&Candidate>,
    usage: SearchUsage,
    status: u16,
    error: Option<String>,
    latency_ms: u32,
    upstream_ms: u32,
) {
    let spent = u64::from(usage.input_tokens)
        .saturating_add(u64::from(usage.output_tokens))
        .saturating_add(u64::from(usage.cache_read_tokens));
    if spent > 0 {
        state.record_tokens(ctx.key_id, spent);
    }
    let (provider, connection_id) = match candidate {
        Some(candidate) => (candidate.provider.clone(), Some(candidate.connection)),
        None => (String::new(), None),
    };
    let cost_micros = match candidate {
        Some(candidate) if spent > 0 => {
            crate::models_registry::cost_micros(
                &state.store,
                &candidate.provider,
                &candidate.model,
                rrouter_core::Usage {
                    input_tokens: usage.input_tokens,
                    output_tokens: usage.output_tokens,
                    cache_read_tokens: usage.cache_read_tokens,
                    cache_write_tokens: 0,
                },
            )
            .await
        }
        _ => 0,
    };
    let record = UsageRecord {
        id: None,
        ts: crate::now_unix(),
        request_id: Some(ctx.request_id),
        // This route has no W3C trace context yet: its rows join the activity log on
        // `request_id` only. The column stays NULL rather than inventing an id.
        trace_id: None,
        provider,
        model: ctx.model.clone(),
        connection_id,
        endpoint: Some(ENDPOINT.to_string()),
        key_id: ctx.key_id,
        cost_micros,
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cache_read_tokens: usage.cache_read_tokens,
        cache_write_tokens: 0,
        latency_ms,
        ttft_ms: 0,
        upstream_ms,
        status,
        streamed: false,
        error,
    };
    if let Err(e) = state.store.record_usage(&record).await {
        tracing::warn!(error = %e, "search usage row failed");
    }
    // Keeping the hold until here means the next budgeted request sees this row.
    let _ = &ctx.budget_hold;
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
        tracing::warn!(error = %e, "search attempt row failed");
    }
}

/// `GET /v1/models/search`: provider ids that have native search and an active connection.
///
/// The list mirrors 9Router's discovery shape (`kind: "webSearch"`) without claiming a
/// model is searchable before the provider is connected.
pub async fn models(State(state): State<Arc<AppState>>) -> Response {
    let connections = match state.store.list_connections(None).await {
        Ok(connections) => connections,
        Err(e) => return super::admin_error(&e),
    };
    let active: Vec<&str> = connections
        .iter()
        .filter(|connection| connection.is_active)
        .map(|connection| connection.provider.as_str())
        .collect();
    let created = crate::now_unix();
    let mut data = Vec::new();
    for spec in rrouter_auth::registry::all() {
        if !active.contains(&spec.id) {
            continue;
        }
        let Some(search) = rrouter_auth::registry::search_spec(spec.id) else {
            continue;
        };
        data.push(json!({
            "id": spec.id,
            "object": "model",
            "created": created,
            "owned_by": spec.id,
            "kind": "webSearch",
            "default_model": search.default_model,
        }));
    }
    super::ok_json(json!({ "object": "list", "data": data }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate() -> Candidate {
        Candidate {
            provider: "codex".into(),
            model: "gpt-5.6-luna".into(),
            connection: Uuid::nil(),
            priority: 0,
            dialect: Dialect::OpenAiResponses,
        }
    }

    #[test]
    fn a_stream_only_response_is_reassembled_from_response_completed() {
        let body = concat!(
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"ignored\"}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"output\":[]}}\n\n"
        );
        let value = streamed_response(body.as_bytes(), &candidate()).expect("completed");
        assert_eq!(value, serde_json::json!({"output": []}));
    }

    #[test]
    fn a_stream_only_response_must_carry_a_completed_event() {
        let body = "event: response.output_text.delta\n\
                    data: {\"type\":\"response.output_text.delta\",\"delta\":\"only\"}\n\n";
        let error = streamed_response(body.as_bytes(), &candidate()).expect_err("incomplete");
        assert!(error
            .to_string()
            .contains("without a completed search response"));
    }

    #[test]
    fn a_plain_json_body_is_accepted_even_for_a_stream_only_provider() {
        let body = br#"{"output":[],"usage":{"input_tokens":1,"output_tokens":2}}"#;
        let value = streamed_response(body, &candidate()).expect("plain");
        assert_eq!(value["usage"]["input_tokens"], serde_json::json!(1));
    }
}
