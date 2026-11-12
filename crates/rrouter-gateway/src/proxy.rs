//! The proxy hot path: authenticate → parse once → plan → attempt → translate back.
//!
//! Non-streaming bodies are parsed exactly once and re-serialized once per dialect hop.
//! Streaming bodies are never buffered: each upstream chunk is decoded to canonical
//! events and re-encoded for the client, and whatever bytes the encoder produced are
//! yielded immediately (at most one `Bytes` per upstream chunk).

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use rrouter_auth::{Credentials, ProviderSpec};
use rrouter_core::{ChatRequest, ChatResponse, Dialect, Error, Part, Result, StreamEvent, Usage};
use rrouter_router::{Candidate, Plan, PlanRequest, ProviderPolicy};
use rrouter_store::{AttemptRecord, ConnectionScope, Guardrails, UsageRecord};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::sync::OwnedSemaphorePermit;
use tracing::Instrument;
use uuid::Uuid;

use crate::config::CacheMode;
use crate::error::{error_frame, error_response, not_implemented};
use crate::metrics::Stage;
use crate::overloaded_response;
use crate::state::{
    check_prompt, dialect_of, prompt_text, AppState, Caller, RateLimits, Slot, WindowUsage,
};

/// Request body limit: a 100k-token request plus base64 images still fits.
const MAX_BODY_BYTES: usize = 64 * 1024 * 1024;
/// Upstream error bodies are truncated to this many bytes before being passed on.
const MAX_ERROR_BODY: usize = 4096;

/// Rows one semantic lookup compares. The scan is already scope-filtered in SQL; this
/// bound keeps a pathological table from turning one request into a million cosine calls.
const SEMANTIC_SCAN_LIMIT: usize = 512;
/// Smart-combo decision budget. The decision is an optimization, never a dependency.
const SMART_DECISION_TIMEOUT: Duration = Duration::from_secs(8);
/// Output cap for the decision call: one short JSON object.
const SMART_DECISION_MAX_TOKENS: u32 = 128;
/// User text included in the decision prompt.
const SMART_PROMPT_CHARS: usize = 4_000;
/// Exact-match cache control: `hit`/`miss` on the response, and the client's `bypass`
/// opt-out on the request.
const CACHE_HEADER: &str = "x-rrouter-cache";

type ByteStream = Pin<Box<dyn Stream<Item = Result<Bytes>> + Send>>;

/// Registry id of the Antigravity (Google Cloud Code Assist) provider.
const GOOGLE_ANTIGRAVITY: &str = "google-antigravity";

fn is_google_cca(provider: &str) -> bool {
    provider == GOOGLE_ANTIGRAVITY
}

/// Project id the CCA envelope must carry, from the credential's provider-specific data.
///
/// A missing project is a configuration error the operator can fix; answer `400` with the
/// field's name instead of sending an envelope the backend will reject.
fn cca_project(creds: &Credentials) -> Result<String> {
    creds
        .extra
        .get("project_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|project| !project.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            Error::BadRequest(
                "google-antigravity connection is missing credentials.extra.project_id".into(),
            )
        })
}

/// One attempt's outbound target: primary URL, the CCA production fallback (when the
/// primary is not already it), the dialect that URL speaks, its headers and, for CCA, the
/// project id the envelope needs.
struct Prepared {
    urls: Vec<String>,
    dialect: Dialect,
    headers: HeaderMap,
    project: Option<String>,
}

fn google_cca_headers(
    spec: &ProviderSpec,
    creds: &Credentials,
    client: Option<&HeaderMap>,
) -> Result<HeaderMap> {
    let mut headers = outbound_headers(spec, creds, client)?;
    headers.insert(
        header::USER_AGENT,
        HeaderValue::from_static(rrouter_translate::google_cca::USER_AGENT),
    );
    headers.insert(
        "x-goog-api-client",
        HeaderValue::from_static(rrouter_translate::google_cca::CLIENT_HEADER),
    );
    Ok(headers)
}

/// The store combo a model reference resolves to, mirroring the planner's one alias
/// hop and `combo:` prefix rule.
async fn resolve_combo(
    state: &AppState,
    model: &str,
) -> Result<Option<(String, rrouter_store::Combo)>> {
    let direct = model.strip_prefix("combo:").unwrap_or(model);
    if let Some(combo) = state.store.get_combo(direct).await? {
        return Ok(Some((direct.to_string(), combo)));
    }
    if model == direct {
        if let Some(target) = state.store.alias_target(model).await? {
            let name = target.strip_prefix("combo:").unwrap_or(&target);
            if let Some(combo) = state.store.get_combo(name).await? {
                return Ok(Some((name.to_string(), combo)));
            }
        }
    }
    Ok(None)
}

/// Request-scoped combo rotation: `None` keeps stored order (fallback), `Some(i)` starts
/// the model list at `i`. Round-robin advances the durable cursor exactly once here.
async fn combo_rotation(
    state: &Arc<AppState>,
    model: &str,
    canonical: &ChatRequest,
    key: Option<Uuid>,
    trace: &RequestTrace,
) -> Result<Option<usize>> {
    let Some((name, combo)) = resolve_combo(state, model).await? else {
        return Ok(None);
    };
    match combo.strategy {
        rrouter_store::ComboStrategy::Fallback => Ok(None),
        rrouter_store::ComboStrategy::RoundRobin => Ok(state.store.next_combo_start(&name).await?),
        rrouter_store::ComboStrategy::Smart => Ok(Some(
            smart_start(state, &name, &combo, canonical, key, trace).await?,
        )),
    }
}

async fn smart_start(
    state: &Arc<AppState>,
    name: &str,
    combo: &rrouter_store::Combo,
    canonical: &ChatRequest,
    key: Option<Uuid>,
    trace: &RequestTrace,
) -> Result<usize> {
    let Some(router) = combo
        .router_model
        .as_deref()
        .map(str::trim)
        .filter(|r| !r.is_empty())
    else {
        tracing::warn!(request_id = %trace.request_id, combo = %name, "smart combo has no router_model; using the first candidate");
        return Ok(0);
    };
    // Runtime recursion guard in addition to create-time validation: a later edit could
    // make the router model a combo, and a decision must never re-enter one.
    if router == name || resolve_combo(state, router).await?.is_some() {
        tracing::warn!(request_id = %trace.request_id, combo = %name, router = %router, "smart combo router_model resolves to a combo; using the first candidate");
        return Ok(0);
    }
    let prompt = smart_prompt(canonical, &combo.models);
    let body = serde_json::json!({
        "model": router,
        "messages": [{"role": "user", "content": prompt}],
        "max_tokens": SMART_DECISION_MAX_TOKENS,
        "temperature": 0.0,
        "stream": false,
    });
    // Boxed: the decision path re-enters the proxy, and boxing breaks the async
    // recursion cycle (runtime recursion itself is blocked by validation/guards).
    let call = Box::pin(complete_for(state, Dialect::OpenAiChat, &body, key));
    let decision = tokio::time::timeout(SMART_DECISION_TIMEOUT, call).await;
    let start = match decision {
        Ok(Ok(response)) => match smart_choice(&response, &combo.models) {
            Some(index) => index,
            None => {
                tracing::warn!(request_id = %trace.request_id, combo = %name, router = %router, "smart combo returned no listed choice; using the first candidate");
                0
            }
        },
        Ok(Err(error)) => {
            tracing::warn!(request_id = %trace.request_id, combo = %name, router = %router, error = %error, "smart combo decision failed; using the first candidate");
            0
        }
        Err(_) => {
            tracing::warn!(request_id = %trace.request_id, combo = %name, router = %router, "smart combo decision timed out; using the first candidate");
            0
        }
    };
    Ok(start)
}

fn smart_prompt(canonical: &ChatRequest, models: &[String]) -> String {
    let request = canonical
        .messages
        .iter()
        .rev()
        .find(|message| message.role == rrouter_core::Role::User)
        .map(|message| message.joined_text())
        .unwrap_or_default();
    let request: String = request.chars().take(SMART_PROMPT_CHARS).collect();
    let mut prompt = String::from(
        "You are a routing policy for a language-model gateway. Choose exactly one \
         candidate for the user request. Reply with JSON only, in the form \
         {\"choice\":\"<exact candidate>\"}.\n\nCandidates:\n",
    );
    for (index, model) in models.iter().enumerate() {
        prompt.push_str(&format!("{}. {model}\n", index + 1));
    }
    prompt.push_str("\nUser request:\n");
    prompt.push_str(&request);
    prompt
}

fn smart_choice(response: &ChatResponse, models: &[String]) -> Option<usize> {
    let text: String = response
        .parts
        .iter()
        .filter_map(|part| match part {
            Part::Text(text) => Some(text.as_str()),
            _ => None,
        })
        .collect();
    let text = text.trim();
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    let value: Value = serde_json::from_str(&text[start..=end]).ok()?;
    let choice = value.get("choice")?.as_str()?.trim();
    models.iter().position(|model| model == choice)
}

/// The request's provider policy: the body's `provider` object with the `x-rrouter-*`
/// headers laid over it. `None` means the request expressed no preference.
///
/// Parsed once per request so a validation error is a `400` before any planning, and so
/// the policy cannot disagree with the body field that will be stripped from the
/// outbound request (the gateway consumes `provider`; a provider must not see it).
pub(crate) fn provider_policy(body: &Value, headers: &HeaderMap) -> Result<Option<ProviderPolicy>> {
    ProviderPolicy::from_request(body, |name| {
        headers.get(name).and_then(|value| value.to_str().ok())
    })
}

/// Admit a request, and record where the key stands whatever the answer.
///
/// A request refused by a rate limit still carries the headers: "remaining 0, reset in 1s"
/// is the actionable form of the 429 the client is about to receive, and it is the same
/// numbers the next successful response would report.
async fn admit_or_explain(
    state: &crate::state::AppState,
    caller: &Caller,
    model: &str,
    trace: &RequestTrace,
) -> Result<()> {
    // Budgeted keys/teams take their lock before the spend check; the guard moves into
    // the response and is released only after the usage row is durable.
    let hold = state.budget_hold(caller).await?;
    match state.admit(caller, model).await {
        Ok(limits) => {
            trace.set_budget_hold(hold);
            if let Some(limits) = limits {
                trace.set_rate_limits(limits);
            }
            Ok(())
        }
        Err(err) => {
            if let Some(limits) = state.refused_limits(caller) {
                trace.set_rate_limits(limits);
            }
            Err(err)
        }
    }
}

/// Entry point for `/v1/chat/completions`, `/v1/messages`, `/v1/responses`.
pub async fn proxy(State(state): State<Arc<AppState>>, req: Request) -> Response {
    let inbound = dialect_of(req.uri());
    let (parts, body) = req.into_parts();
    let trace = RequestTrace::new(Uuid::new_v4(), inbound, Some(&parts.headers));
    let started = Instant::now();
    // The slot is taken before the body is read: an overloaded gateway must not spend
    // memory buffering a request it has already decided to refuse.
    let permit = match state.try_slot() {
        Slot::Granted(permit) => permit,
        Slot::Full => return trace.respond(started, overloaded_response(inbound)),
    };
    let raw = match axum::body::to_bytes(body, MAX_BODY_BYTES).await {
        Ok(b) => b,
        Err(e) => {
            return trace.fail(
                &state,
                started,
                inbound,
                &Error::BadRequest(format!("request body could not be read: {e}")),
            );
        }
    };
    match handle(&state, inbound, &parts.headers, &raw, &trace, permit).await {
        Ok(resp) => trace.respond(started, resp),
        Err(e) => trace.fail(&state, started, inbound, &e),
    }
}

/// `/v1/embeddings`: passthrough for [OI]-compatible providers, `501` otherwise.
pub async fn embeddings(State(state): State<Arc<AppState>>, req: Request) -> Response {
    let inbound = dialect_of(req.uri());
    let (parts, body) = req.into_parts();
    let trace = RequestTrace::new(Uuid::new_v4(), inbound, Some(&parts.headers));
    let started = Instant::now();
    // Held for this whole scope: a non-streaming response releases it when it is built.
    let _permit = match state.try_slot() {
        Slot::Granted(permit) => permit,
        Slot::Full => {
            return trace.respond(started, overloaded_response(Dialect::OpenAiEmbeddings))
        }
    };
    let raw = match axum::body::to_bytes(body, MAX_BODY_BYTES).await {
        Ok(b) => b,
        Err(e) => {
            return trace.fail(
                &state,
                started,
                Dialect::OpenAiEmbeddings,
                &Error::BadRequest(format!("request body could not be read: {e}")),
            );
        }
    };
    match embeddings_inner(&state, &parts.headers, &raw, &trace).await {
        // Non-streaming: the permit dies with this scope, after the body is built.
        Ok(resp) => trace.respond(started, resp),
        Err(e) => trace.fail(&state, started, Dialect::OpenAiEmbeddings, &e),
    }
}

/// Endpoints that only some providers implement. RRouter forwards the client body to the
/// resolved provider instead of translating it: both are provider-native JSON shapes the
/// canonical request cannot represent, and pretending with an estimate would be worse
/// than a clear 501 for providers that do not have the endpoint.
#[derive(Clone, Copy)]
enum Special {
    /// `POST /v1/messages/count_tokens` — Anthropic counts exactly.
    CountTokens,
    /// `POST /v1/responses/compact` — OpenAI/Codex conversation compaction.
    Compact,
}

impl Special {
    fn dialect(self) -> Dialect {
        match self {
            Self::CountTokens => Dialect::AnthropicMessages,
            Self::Compact => Dialect::OpenAiResponses,
        }
    }

    /// Provider base URL for this endpoint family, when the provider has one.
    fn base(self, spec: &ProviderSpec) -> Option<&'static str> {
        match self {
            Self::CountTokens => spec.messages_url,
            Self::Compact => spec.responses_url,
        }
    }

    fn url(self, base: &str) -> String {
        let base = base.trim_end_matches('/');
        match self {
            Self::CountTokens => format!("{base}/count_tokens"),
            Self::Compact => format!("{base}/compact"),
        }
    }

    fn unsupported(self) -> &'static str {
        match self {
            Self::CountTokens => "count_tokens for this provider",
            Self::Compact => "compaction for this provider",
        }
    }

    /// The client-facing path, recorded on usage rows: these endpoints have no canonical
    /// dialect endpoint to fall back on.
    fn endpoint(self) -> &'static str {
        match self {
            Self::CountTokens => "/v1/messages/count_tokens",
            Self::Compact => "/v1/responses/compact",
        }
    }
}

pub async fn count_tokens(State(state): State<Arc<AppState>>, req: Request) -> Response {
    special(&state, req, Special::CountTokens).await
}

pub async fn compact(State(state): State<Arc<AppState>>, req: Request) -> Response {
    special(&state, req, Special::Compact).await
}

async fn special(state: &Arc<AppState>, req: Request, kind: Special) -> Response {
    let (parts, body) = req.into_parts();
    let trace = RequestTrace::new(Uuid::new_v4(), kind.dialect(), Some(&parts.headers));
    let started = Instant::now();
    // Held for this whole scope: a non-streaming response releases it when it is built.
    let _permit = match state.try_slot() {
        Slot::Granted(permit) => permit,
        Slot::Full => return trace.respond(started, overloaded_response(kind.dialect())),
    };
    let raw = match axum::body::to_bytes(body, MAX_BODY_BYTES).await {
        Ok(b) => b,
        Err(e) => {
            return trace.fail_at(
                state,
                started,
                kind.dialect(),
                kind.endpoint(),
                &Error::BadRequest(format!("request body could not be read: {e}")),
            );
        }
    };
    match special_inner(state, &parts.headers, &raw, kind, &trace).await {
        // Non-streaming: the permit dies with this scope, after the body is built.
        Ok(resp) => trace.respond(started, resp),
        Err(e) => trace.fail_at(state, started, kind.dialect(), kind.endpoint(), &e),
    }
}

async fn special_inner(
    state: &Arc<AppState>,
    headers: &axum::http::HeaderMap,
    raw: &[u8],
    kind: Special,
    trace: &RequestTrace,
) -> Result<Response> {
    let caller = state.authenticate(headers).await?;
    trace.set_key(caller.key_id());
    let body: Value = serde_json::from_slice(raw)?;
    let requested = body
        .get("model")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::BadRequest("missing model".into()))?
        .to_string();
    // Per-key mapping first, then governance and planning, so the allowlist and the
    // routed ref both see the model this key actually asked for.
    let model = state.mapped_model(caller.key_id(), &requested).await?;
    trace.set_model(&model);
    // These endpoints forward a provider-native body, so the prompt is read through the
    // canonical parser — but only for a key that has a policy: a body these parsers do
    // not model must keep streaming through untouched for every other key. Only the
    // input side applies here: a count/compaction reply is provider-shaped JSON that
    // never becomes a canonical response, so `scan_output` guards chat responses only.
    if caller.guardrails().is_some() {
        let canonical = rrouter_translate::parse_request(kind.dialect(), &body)?;
        check_prompt(&caller, &prompt_text(&canonical))?;
    }
    admit_or_explain(state, &caller, &model, trace).await?;
    let policy = provider_policy(&body, headers)?;
    let config = state.config.load();
    let canary_seed = canary_seed_for(trace, None);
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
                policy: policy.as_ref(),
                allowed_models: caller.allowed_models(),
                team_id: caller.team_id(),
                canary: config.canary.as_ref(),
                canary_seed,
                combo_start: None,
            },
        )
        .await?;
    // Fallback only makes sense between providers that have the endpoint; asking the
    // next candidate would answer with a NoRoute that stops the chain anyway.
    let candidates: Vec<Candidate> = plan
        .candidates
        .iter()
        .filter(|c| rrouter_auth::get(&c.provider).is_some_and(|spec| kind.base(spec).is_some()))
        .cloned()
        .collect();
    if candidates.is_empty() {
        return Ok(not_implemented(kind.dialect(), kind.unsupported()));
    }
    let plan = Plan { candidates, ..plan };

    let started = Instant::now();
    let outcome = state
        .dispatcher
        .run(&plan, |c| {
            trace.time(state, c, |c, trace| {
                send_special(state, headers, &body, kind, c, trace)
            })
        })
        .await?;
    let value = outcome.result?;
    // These endpoints are not chat requests, so a body without usage is not a zero-cost
    // row: it means the upstream reported nothing to account for.
    if let Some(usage) = reported_usage(&value) {
        record_once(
            state,
            Recorded {
                candidate: &outcome.candidate,
                model: &plan.model,
                endpoint: kind.endpoint(),
                usage,
                key: caller.key_id(),
                request_id: trace.request_id,
                trace_id: trace.trace_id(),
                started,
                upstream_ms: 0,
                ttft: None,
                streamed: false,
                status: 200,
                error: None,
                budget_hold: trace.take_budget_hold(),
            },
        );
    }
    Ok(json_response(StatusCode::OK, value))
}

/// Token counts a passthrough body reports, in either protocol's naming.
///
/// `None` means the upstream said nothing about cost, which is the normal answer for
/// `count_tokens`; `Some` means the call did billable work and belongs in the usage log.
fn reported_usage(value: &Value) -> Option<Usage> {
    let usage = value.get("usage")?;
    let count = |name: &str| {
        usage
            .get(name)
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok())
    };
    let input = count("input_tokens")
        .or_else(|| count("prompt_tokens"))
        .unwrap_or(0);
    let output = count("output_tokens")
        .or_else(|| count("completion_tokens"))
        .unwrap_or(0);
    let cache_read = usage
        .pointer("/input_tokens_details/cached_tokens")
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .unwrap_or(0);
    Some(Usage {
        input_tokens: input,
        output_tokens: output,
        cache_read_tokens: cache_read,
        cache_write_tokens: 0,
    })
}

async fn send_special(
    state: &Arc<AppState>,
    client: &HeaderMap,
    body: &Value,
    kind: Special,
    candidate: Candidate,
    trace: std::sync::Arc<AttemptTrace>,
) -> Result<Value> {
    let spec = rrouter_auth::get(&candidate.provider).ok_or_else(|| Error::NoRoute {
        model: candidate.model.clone(),
        detail: format!("unknown provider {}", candidate.provider),
    })?;
    let native = kind.base(spec).ok_or_else(|| Error::NoRoute {
        model: candidate.model.clone(),
        detail: format!("{} has no {}", candidate.provider, kind.unsupported()),
    })?;
    let creds = state
        .refresher
        .fresh(state.store.as_ref(), spec, candidate.connection)
        .await?;
    let url = match creds.base_url_override.as_deref() {
        Some(base) => rewrite_base(&kind.url(native), base),
        None => kind.url(native),
    };
    let url = rrouter_auth::with_query(&url, spec);
    let mut wire = body.clone();
    wire["model"] = Value::String(candidate.model.clone());
    let resp = state
        .http
        .post(&url)
        .headers({
            let mut headers = outbound_headers(spec, &creds, Some(client))?;
            inject_trace_headers(&mut headers, &trace);
            headers
        })
        .json(&wire)
        .timeout(state.request_timeout())
        .send()
        .await
        .map_err(transport_error)?;
    let status = resp.status();
    if !status.is_success() {
        let bytes = resp.bytes().await.map_err(transport_error)?;
        return Err(upstream_error(&candidate, status.as_u16(), &bytes));
    }
    let bytes = resp.bytes().await.map_err(transport_error)?;
    Ok(serde_json::from_slice(&bytes)?)
}

/// Endpoints some providers implement and others do not, forwarded without translation:
/// the body and the response are both the provider's own wire shapes.
///
/// Translation is impossible here (the canonical request has no image, audio or
/// multipart representation), and an invented one would be worse than the `501` a
/// provider without the endpoint earns.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Pass {
    /// `POST /v1/images/generations` — JSON in, JSON out.
    Images,
    /// `POST /v1/audio/speech` — JSON in, audio bytes out.
    Speech,
    /// `POST /v1/audio/transcriptions` — multipart in, JSON out.
    Transcriptions,
    /// `GET /v1/audio/voices` — JSON out.
    Voices,
}

impl Pass {
    /// These endpoints are OpenAI protocol, so their errors are too.
    fn dialect(self) -> Dialect {
        Dialect::OpenAiChat
    }

    /// The provider's native URL, when it documents the endpoint.
    fn native(self, spec: &ProviderSpec) -> Option<&'static str> {
        match self {
            Self::Images => spec.images_url,
            Self::Speech => spec.audio_speech_url,
            Self::Transcriptions => spec.audio_transcriptions_url,
            Self::Voices => spec.audio_voices_url,
        }
    }

    /// Client-facing path, recorded on usage rows and named in a `501`.
    fn endpoint(self) -> &'static str {
        match self {
            Self::Images => "/v1/images/generations",
            Self::Speech => "/v1/audio/speech",
            Self::Transcriptions => "/v1/audio/transcriptions",
            Self::Voices => "/v1/audio/voices",
        }
    }

    /// `501` message prefix: names the endpoint the provider does not serve.
    fn unsupported(self) -> &'static str {
        match self {
            Self::Images => "POST /v1/images/generations for this provider",
            Self::Speech => "POST /v1/audio/speech for this provider",
            Self::Transcriptions => "POST /v1/audio/transcriptions for this provider",
            Self::Voices => "GET /v1/audio/voices for this provider",
        }
    }
}

pub async fn images(State(state): State<Arc<AppState>>, req: Request) -> Response {
    passthrough(&state, req, Pass::Images).await
}

pub async fn speech(State(state): State<Arc<AppState>>, req: Request) -> Response {
    passthrough(&state, req, Pass::Speech).await
}

pub async fn transcriptions(State(state): State<Arc<AppState>>, req: Request) -> Response {
    passthrough(&state, req, Pass::Transcriptions).await
}

pub async fn voices(State(state): State<Arc<AppState>>, req: Request) -> Response {
    passthrough(&state, req, Pass::Voices).await
}

async fn passthrough(state: &Arc<AppState>, req: Request, kind: Pass) -> Response {
    let (parts, body) = req.into_parts();
    let trace = RequestTrace::new(Uuid::new_v4(), kind.dialect(), Some(&parts.headers));
    let started = Instant::now();
    let permit = match state.try_slot() {
        Slot::Granted(permit) => permit,
        Slot::Full => return trace.respond(started, overloaded_response(kind.dialect())),
    };
    let raw = match axum::body::to_bytes(body, MAX_BODY_BYTES).await {
        Ok(b) => b,
        Err(e) => {
            return trace.fail_at(
                state,
                started,
                kind.dialect(),
                kind.endpoint(),
                &Error::BadRequest(format!("request body could not be read: {e}")),
            );
        }
    };
    match passthrough_inner(state, &parts.headers, &raw, kind, &trace).await {
        // Audio streams: the slot has to outlive this handler and die with the body.
        Ok(resp) => trace.respond(started, hold_slot(resp, permit)),
        Err(e) => trace.fail_at(state, started, kind.dialect(), kind.endpoint(), &e),
    }
}

/// Keep a response's concurrency slot until its body is consumed or dropped.
fn hold_slot(resp: Response, permit: Option<OwnedSemaphorePermit>) -> Response {
    let Some(permit) = permit else {
        return resp;
    };
    let (parts, body) = resp.into_parts();
    let held = Held {
        inner: body.into_data_stream(),
        _permit: permit,
    };
    Response::from_parts(parts, Body::from_stream(held))
}

/// A body that holds a semaphore permit for its whole life.
struct Held {
    inner: axum::body::BodyDataStream,
    _permit: OwnedSemaphorePermit,
}

impl Stream for Held {
    type Item = std::result::Result<Bytes, axum::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.inner).poll_next(cx)
    }
}

async fn passthrough_inner(
    state: &Arc<AppState>,
    headers: &axum::http::HeaderMap,
    raw: &[u8],
    kind: Pass,
    trace: &RequestTrace,
) -> Result<Response> {
    let caller = state.authenticate(headers).await?;
    trace.set_key(caller.key_id());
    match kind {
        Pass::Voices => voices_inner(state, headers, trace, &caller).await,
        Pass::Transcriptions => transcriptions_inner(state, headers, raw, trace, &caller).await,
        Pass::Images | Pass::Speech => {
            let body: Value = serde_json::from_slice(raw)?;
            let Some(plan) = json_plan(state, headers, &body, kind, trace, &caller).await? else {
                return Ok(not_implemented(kind.dialect(), kind.unsupported()));
            };
            let started = Instant::now();
            if kind == Pass::Speech {
                let outcome = state
                    .dispatcher
                    .run(&plan, |c| {
                        trace.time(state, c, |c, trace| {
                            send_speech(state, headers, &body, c, trace)
                        })
                    })
                    .await?;
                let audio = outcome.result?;
                // An audio body reports no token counts: the row says 0, not a guess.
                record_passthrough(
                    state,
                    &outcome.candidate,
                    &plan.model,
                    kind,
                    Usage::default(),
                    caller.key_id(),
                    trace,
                    started,
                );
                return Ok(audio_response(audio));
            }
            let outcome = state
                .dispatcher
                .run(&plan, |c| {
                    trace.time(state, c, |c, trace| {
                        send_json(state, headers, &body, kind, c, trace)
                    })
                })
                .await?;
            let (status, value) = outcome.result?;
            record_passthrough(
                state,
                &outcome.candidate,
                &plan.model,
                kind,
                reported_usage(&value).unwrap_or_default(),
                caller.key_id(),
                trace,
                started,
            );
            Ok(json_response(status, value))
        }
    }
}

/// Token Saver: compress noisy tool results in a canonical request. Headers may force
/// it on or off per request (`x-rrouter-rtk: on|off`); otherwise the stored setting wins.
/// Guardrails run before this, so a blocked phrase cannot hide inside a dropped middle.
fn rtk_enabled(headers: &axum::http::HeaderMap) -> bool {
    headers
        .get("x-rrouter-rtk")
        .and_then(|v| v.to_str().ok())
        .map(|value| !matches!(value.to_ascii_lowercase().as_str(), "off" | "0" | "false"))
        .unwrap_or_else(crate::settings::rtk)
}

fn record_rtk(stats: &rrouter_rtk::Stats) {
    crate::metrics::rtk(stats);
    tracing::info!(
        saved = stats.saved(),
        before = stats.before,
        percent = stats.percent(),
        filters = ?stats.filters,
        "token saver compressed tool output"
    );
}

/// Plan for a JSON passthrough: the model is required, this key's mapping and limits
/// apply, and only providers that serve the endpoint stay in the plan.
async fn json_plan(
    state: &Arc<AppState>,
    headers: &HeaderMap,
    body: &Value,
    kind: Pass,
    trace: &RequestTrace,
    caller: &Caller,
) -> Result<Option<Plan>> {
    let requested = body
        .get("model")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::BadRequest("missing model".into()))?
        .to_string();
    let model = state.mapped_model(caller.key_id(), &requested).await?;
    trace.set_model(&model);
    admit_or_explain(state, caller, &model, trace).await?;
    let policy = provider_policy(body, headers)?;
    let config = state.config.load();
    let canary_seed = canary_seed_for(trace, None);
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
                policy: policy.as_ref(),
                allowed_models: caller.allowed_models(),
                team_id: caller.team_id(),
                canary: config.canary.as_ref(),
                canary_seed,
                combo_start: None,
            },
        )
        .await?;
    Ok(supported(plan, kind))
}

/// Drop the candidates whose provider does not have the endpoint. `None` means nobody
/// does, so the route answers `501` and nothing is sent.
fn supported(plan: Plan, kind: Pass) -> Option<Plan> {
    let candidates: Vec<Candidate> = plan
        .candidates
        .iter()
        .filter(|c| rrouter_auth::get(&c.provider).is_some_and(|spec| kind.native(spec).is_some()))
        .cloned()
        .collect();
    if candidates.is_empty() {
        return None;
    }
    Some(Plan { candidates, ..plan })
}

/// An upstream audio body and the content type that describes it, relayed untouched.
struct Audio {
    content_type: Option<HeaderValue>,
    bytes: ByteStream,
}

/// Relay audio bytes, never buffering the clip.
fn audio_response(audio: Audio) -> Response {
    let mut resp = Response::new(Body::from_stream(audio.bytes));
    if let Some(ct) = audio.content_type {
        resp.headers_mut().insert(header::CONTENT_TYPE, ct);
    }
    resp
}

/// Native URL (account override applied) and outbound headers for one passthrough attempt.
async fn prepare_pass(
    state: &Arc<AppState>,
    client: &HeaderMap,
    kind: Pass,
    candidate: &Candidate,
    trace: &AttemptTrace,
) -> Result<(String, axum::http::HeaderMap)> {
    let spec = rrouter_auth::get(&candidate.provider).ok_or_else(|| Error::NoRoute {
        model: candidate.model.clone(),
        detail: format!("unknown provider {}", candidate.provider),
    })?;
    let native = kind.native(spec).ok_or_else(|| Error::NoRoute {
        model: candidate.model.clone(),
        detail: format!("{} has no {}", candidate.provider, kind.unsupported()),
    })?;
    let creds = state
        .refresher
        .fresh(state.store.as_ref(), spec, candidate.connection)
        .await?;
    let url = match creds.base_url_override.as_deref() {
        Some(base) => rewrite_base(native, base),
        None => native.to_string(),
    };
    let mut headers = outbound_headers(spec, &creds, Some(client))?;
    inject_trace_headers(&mut headers, trace);
    Ok((rrouter_auth::with_query(&url, spec), headers))
}

/// One JSON passthrough attempt. The model is the provider's own bare id.
async fn send_json(
    state: &Arc<AppState>,
    client: &HeaderMap,
    body: &Value,
    kind: Pass,
    candidate: Candidate,
    trace: std::sync::Arc<AttemptTrace>,
) -> Result<(StatusCode, Value)> {
    let (url, headers) = prepare_pass(state, client, kind, &candidate, &trace).await?;
    let mut wire = body.clone();
    wire["model"] = Value::String(candidate.model.clone());
    let resp = state
        .http
        .post(&url)
        .headers(headers)
        .json(&wire)
        .timeout(state.request_timeout())
        .send()
        .await
        .map_err(transport_error)?;
    read_json(&candidate, resp).await
}

/// One speech attempt: headers are enough to commit, the body streams to the client.
async fn send_speech(
    state: &Arc<AppState>,
    client: &HeaderMap,
    body: &Value,
    candidate: Candidate,
    trace: std::sync::Arc<AttemptTrace>,
) -> Result<Audio> {
    let (url, headers) = prepare_pass(state, client, Pass::Speech, &candidate, &trace).await?;
    let mut wire = body.clone();
    wire["model"] = Value::String(candidate.model.clone());
    let resp = state
        .http
        .post(&url)
        .headers(headers)
        .json(&wire)
        .timeout(state.request_timeout())
        .send()
        .await
        .map_err(transport_error)?;
    let status = resp.status();
    if !status.is_success() {
        let bytes = resp.bytes().await.map_err(transport_error)?;
        return Err(upstream_error(&candidate, status.as_u16(), &bytes));
    }
    let content_type = resp.headers().get(header::CONTENT_TYPE).cloned();
    Ok(Audio {
        content_type,
        bytes: Box::pin(resp.bytes_stream().map(|c| c.map_err(transport_error))),
    })
}

/// One transcription attempt. The client's own `content-type` (boundary included) and
/// raw bytes are forwarded: re-encoding the form would invalidate the boundary.
async fn send_multipart(
    state: &Arc<AppState>,
    client: &HeaderMap,
    raw: &[u8],
    content_type: &HeaderValue,
    candidate: Candidate,
    trace: std::sync::Arc<AttemptTrace>,
) -> Result<(StatusCode, Value)> {
    let (url, mut headers) =
        prepare_pass(state, client, Pass::Transcriptions, &candidate, &trace).await?;
    headers.insert(header::CONTENT_TYPE, content_type.clone());
    let resp = state
        .http
        .post(&url)
        .headers(headers)
        .body(raw.to_vec())
        .timeout(state.request_timeout())
        .send()
        .await
        .map_err(transport_error)?;
    read_json(&candidate, resp).await
}

async fn send_voices(
    state: &Arc<AppState>,
    client: &HeaderMap,
    candidate: Candidate,
    trace: std::sync::Arc<AttemptTrace>,
) -> Result<(StatusCode, Value)> {
    let (url, headers) = prepare_pass(state, client, Pass::Voices, &candidate, &trace).await?;
    let resp = state
        .http
        .get(&url)
        .headers(headers)
        .timeout(state.request_timeout())
        .send()
        .await
        .map_err(transport_error)?;
    read_json(&candidate, resp).await
}

/// A passthrough reply: 4xx/5xx become the proxy's upstream error, a JSON success is
/// relayed with the upstream status (some providers answer `202` with a handle).
async fn read_json(candidate: &Candidate, resp: reqwest::Response) -> Result<(StatusCode, Value)> {
    let status = resp.status();
    if !status.is_success() {
        let bytes = resp.bytes().await.map_err(transport_error)?;
        return Err(upstream_error(candidate, status.as_u16(), &bytes));
    }
    let bytes = resp.bytes().await.map_err(transport_error)?;
    Ok((status, serde_json::from_slice(&bytes)?))
}

/// `POST /v1/audio/transcriptions` with the client's multipart body untouched.
async fn transcriptions_inner(
    state: &Arc<AppState>,
    headers: &axum::http::HeaderMap,
    raw: &[u8],
    trace: &RequestTrace,
    caller: &Caller,
) -> Result<Response> {
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .ok_or_else(|| Error::BadRequest("missing content-type".into()))?;
    let is_multipart = content_type
        .to_str()
        .map(|v| v.to_ascii_lowercase().starts_with("multipart/form-data"))
        .unwrap_or(false);
    if !is_multipart {
        return Err(Error::BadRequest(
            "expected multipart/form-data with a boundary".into(),
        ));
    }
    let plan = match multipart_field(raw, "model") {
        Some(requested) => {
            let model = state.mapped_model(caller.key_id(), &requested).await?;
            trace.set_model(&model);
            admit_or_explain(state, caller, &model, trace).await?;
            // A multipart body has no `provider` object; only the headers can carry
            // routing preferences on this endpoint.
            let policy = provider_policy(&Value::Null, headers)?;
            let config = state.config.load();
            let canary_seed = canary_seed_for(trace, None);
            state
                .planner
                .plan(
                    state.catalog.as_ref(),
                    state.store.as_ref(),
                    PlanRequest {
                        model: &model,
                        stream: false,
                        affinity: None,
                        strategy: config.strategy,
                        policy: policy.as_ref(),
                        allowed_models: caller.allowed_models(),
                        team_id: caller.team_id(),
                        canary: config.canary.as_ref(),
                        canary_seed,
                        combo_start: None,
                    },
                )
                .await?
        }
        None => {
            // No model in the form: the endpoint needs an account, not a model. The
            // allowlist is checked against the empty model, so a key restricted to
            // specific models is told to name one rather than bypassing its list.
            let Some(candidate) =
                hint_candidate(state, Pass::Transcriptions, caller.team_id()).await?
            else {
                return Err(Error::BadRequest(
                    "missing model and no connected provider serves /v1/audio/transcriptions"
                        .into(),
                ));
            };
            admit_or_explain(state, caller, "", trace).await?;
            Plan {
                model: candidate.model.clone(),
                candidates: vec![candidate],
                stream: false,
            }
        }
    };
    let Some(plan) = supported(plan, Pass::Transcriptions) else {
        return Ok(not_implemented(
            Pass::Transcriptions.dialect(),
            Pass::Transcriptions.unsupported(),
        ));
    };

    let started = Instant::now();
    let outcome = state
        .dispatcher
        .run(&plan, |c| {
            trace.time(state, c, |c, trace| {
                send_multipart(state, headers, raw, content_type, c, trace)
            })
        })
        .await?;
    let (status, value) = outcome.result?;
    record_passthrough(
        state,
        &outcome.candidate,
        &plan.model,
        Pass::Transcriptions,
        reported_usage(&value).unwrap_or_default(),
        caller.key_id(),
        trace,
        started,
    );
    Ok(json_response(status, value))
}

async fn voices_inner(
    state: &Arc<AppState>,
    client: &HeaderMap,
    trace: &RequestTrace,
    caller: &Caller,
) -> Result<Response> {
    let Some(candidate) = hint_candidate(state, Pass::Voices, caller.team_id()).await? else {
        return Ok(not_implemented(
            Pass::Voices.dialect(),
            Pass::Voices.unsupported(),
        ));
    };
    admit_or_explain(state, caller, "", trace).await?;
    let plan = Plan {
        model: candidate.model.clone(),
        candidates: vec![candidate],
        stream: false,
    };
    let started = Instant::now();
    let outcome = state
        .dispatcher
        .run(&plan, |c| {
            trace.time(state, c, |c, trace| send_voices(state, client, c, trace))
        })
        .await?;
    let (status, value) = outcome.result?;
    record_passthrough(
        state,
        &outcome.candidate,
        &plan.model,
        Pass::Voices,
        reported_usage(&value).unwrap_or_default(),
        caller.key_id(),
        trace,
        started,
    );
    Ok(json_response(status, value))
}

/// First active connection whose provider serves `kind`: the account an endpoint with no
/// model in its request has to borrow. A team key borrows only its own team's account.
async fn hint_candidate(
    state: &Arc<AppState>,
    kind: Pass,
    team_id: Option<Uuid>,
) -> Result<Option<Candidate>> {
    let connections = state
        .store
        .list_connections_scoped(ConnectionScope::for_team(team_id), None)
        .await?;
    Ok(connections
        .into_iter()
        .filter(|c| c.is_active)
        .find_map(|c| {
            let spec = rrouter_auth::get(&c.provider)?;
            kind.native(spec)?;
            Some(Candidate {
                provider: c.provider.clone(),
                model: String::new(),
                connection: c.id,
                priority: c.priority,
                dialect: spec.default_dialect,
            })
        }))
}

/// One form field from a raw `multipart/form-data` body.
///
/// A real multipart parser exists to re-encode the form, and re-encoding would break the
/// boundary the client signed its `content-type` with. This reads the one value the route
/// needs and leaves the bytes alone.
fn multipart_field(raw: &[u8], name: &str) -> Option<String> {
    let needle = format!("name=\"{name}\"");
    let needle = needle.as_bytes();
    let mut at = 0usize;
    while let Some(rel) = find_bytes(&raw[at..], needle) {
        let pos = at + rel;
        let value_start = pos + needle.len();
        at = value_start;
        // `filename="model"` contains the needle; only a real attribute counts.
        if pos > 0 && (raw[pos - 1].is_ascii_alphanumeric() || raw[pos - 1] == b'-') {
            continue;
        }
        let header_end = value_start + find_bytes(&raw[value_start..], b"\r\n\r\n")? + 4;
        let value_end = header_end + find_bytes(&raw[header_end..], b"\r\n--")?;
        let value = String::from_utf8_lossy(&raw[header_end..value_end])
            .trim()
            .to_string();
        if !value.is_empty() {
            return Some(value);
        }
    }
    None
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// One usage row per delivered passthrough. Token counts are the upstream's own; a body
/// that reports nothing records 0 instead of a guess.
#[allow(clippy::too_many_arguments)]
fn record_passthrough(
    state: &AppState,
    candidate: &Candidate,
    model: &str,
    kind: Pass,
    usage: Usage,
    key: Option<Uuid>,
    trace: &RequestTrace,
    started: Instant,
) {
    record_once(
        state,
        Recorded {
            candidate,
            model,
            endpoint: kind.endpoint(),
            usage,
            key,
            request_id: trace.request_id,
            trace_id: trace.trace_id(),
            started,
            upstream_ms: trace.upstream_ms(),
            ttft: None,
            streamed: false,
            status: 200,
            error: None,
            budget_hold: trace.take_budget_hold(),
        },
    );
}

/// `permit` is this request's concurrency slot. It is moved into the streamed response
/// when there is one (the stream owns it), and simply dropped otherwise — after the body
/// has been built.
async fn handle(
    state: &Arc<AppState>,
    inbound: Dialect,
    headers: &axum::http::HeaderMap,
    raw: &[u8],
    trace: &RequestTrace,
    permit: Option<OwnedSemaphorePermit>,
) -> Result<Response> {
    let caller = state.authenticate(headers).await?;
    trace.set_key(caller.key_id());
    let started = Instant::now();
    // One snapshot for the whole request: planning, cache and usage bookkeeping below all
    // read the same configuration even if a reload lands mid-request.
    let config = state.config.load();

    let mut body: Value = serde_json::from_slice(raw)?;
    let mut canonical = rrouter_translate::parse_request(inbound, &body)?;
    // This key's model mapping is one hop that runs before alias/combo resolution.
    let model = state
        .mapped_model(caller.key_id(), &canonical.model)
        .await?;
    trace.set_model(&model);
    // Guardrails run on the canonical text after authentication and before any
    // governance bookkeeping or dispatch: a refused prompt costs no provider call, no
    // rate-limit slot and no cache entry.
    check_prompt(&caller, &prompt_text(&canonical))?;
    // PII redaction is an explicit per-key opt-in and runs after the guardrail decision
    // (a blocked phrase inside an email must still block) and before planning, the cache
    // key and dispatch. The wire body is redacted, so same-dialect forwarding is covered
    // too, then the canonical request is refreshed from it.
    let redact_pii = caller.guardrails().is_some_and(|policy| policy.redact_pii);
    if redact_pii {
        let report = crate::redaction::redact_json_body(&mut body);
        if report.redacted {
            crate::metrics::pii_redacted(crate::metrics::RedactStage::Input, &report);
            tracing::debug!(stage = "input", %report, "pii redacted");
            canonical = rrouter_translate::parse_request(inbound, &body)?;
        }
    }
    // Governance runs before planning: an inactive/expired key, a model outside the
    // allowlist, or an exhausted quota must not reach a provider.
    admit_or_explain(state, &caller, &model, trace).await?;
    // Token Saver runs on the wire body after guardrails: same-dialect forwarding sends
    // this body unchanged, and the canonical parse is refreshed from the compressed text.
    if rtk_enabled(headers) {
        if let Some(stats) = rrouter_rtk::compress_source(&mut body, inbound) {
            record_rtk(&stats);
            canonical = rrouter_translate::parse_request(inbound, &body)?;
        }
    }
    let key_id = caller.key_id();
    // Routing preferences are read after authentication and after the wire body settled
    // (Token Saver may have rewritten it), so the policy and the stripped body agree.
    let policy = provider_policy(&body, headers)?;
    let affinity = affinity_of(&canonical);
    let canary_seed = canary_seed_for(trace, Some(affinity));
    // Combo strategy is request-scoped: round-robin advances its durable cursor and a
    // smart decision runs exactly once here, before the plan that dispatch consumes.
    let combo_start = combo_rotation(state, &model, &canonical, key_id, trace).await?;
    // Subscription headroom is a cached, per-account hint. Reading it never performs a
    // provider call: the background refresh owns that traffic.
    let capacity = crate::provider_usage::cached_hints();
    let plan = state
        .planner
        .plan_with_capacity(
            state.catalog.as_ref(),
            state.store.as_ref(),
            PlanRequest {
                model: &model,
                stream: canonical.stream,
                affinity: Some(affinity),
                strategy: config.strategy,
                policy: policy.as_ref(),
                allowed_models: caller.allowed_models(),
                team_id: caller.team_id(),
                canary: config.canary.as_ref(),
                canary_seed,
                combo_start,
            },
            Some(&capacity),
            f64::from(config.subscription_reserve_percent),
        )
        .await?;
    let client_stream = plan.stream || canonical.stream;
    // A stream-only upstream (codex) refuses every non-streaming request, so a
    // non-streaming client is served by streaming upstream and reassembling.
    let upstream_stream = client_stream || plan.candidates.iter().any(stream_only);

    let attempt = Attempt {
        state,
        inbound,
        canonical,
        raw: &body,
        upstream_stream,
        trace,
        strip_provider: policy.is_some(),
        client_headers: Some(headers),
        redact_pii,
    };

    // Cache eligibility is decided once, before anything is sent: a non-streaming
    // client (a stream has no body to store), no explicit bypass, and a candidate to
    // key the canonical upstream body against. The exact cache and its semantic
    // fallback share this key; the semantic cache additionally needs an embedding model.
    let caching = !client_stream
        && !cache_bypassed(headers)
        && !plan.candidates.is_empty()
        // CCA's envelope carries a per-attempt request id and a credential project, so
        // its exact bytes are neither stable nor cacheable across accounts.
        && !plan
            .candidates
            .iter()
            .any(|candidate| is_google_cca(&candidate.provider));
    let cache_key = if caching
        && (config.cache == CacheMode::Sqlite || config.semantic_cache == CacheMode::Sqlite)
    {
        match attempt.cache_key(&plan.candidates[0]) {
            Ok(key) => Some(key),
            Err(e) => {
                tracing::warn!(error = %e, "response cache: canonical request unavailable");
                None
            }
        }
    } else {
        None
    };
    let cache = if config.cache == CacheMode::Sqlite {
        cache_key.clone()
    } else {
        None
    };

    // Exact entries always win: a byte-identical request never pays for an embedding.
    if let Some(key) = &cache {
        match state.store.cache_get(&key.hash, crate::now_unix()).await {
            Ok(Some(body)) => {
                return serve_cache_hit(
                    state,
                    &caller,
                    &plan.candidates[0],
                    &plan.model,
                    inbound,
                    key_id,
                    trace,
                    started,
                    body,
                    "hit",
                    redact_pii,
                )
                .await;
            }
            Ok(None) => crate::metrics::cache_miss(),
            // A cache is an optimisation: a store failure must never fail the request.
            Err(e) => {
                tracing::warn!(error = %e, "response cache read failed");
                crate::metrics::cache_miss();
            }
        }
    }

    // Semantic fallback, only after an exact miss. Every failure here is a miss, never a
    // request failure: the embedding provider being down must not take the gateway down
    // with it. The embedding is kept for the store step so a miss costs one call, not two.
    let mut semantic_embedding: Option<Vec<f32>> = None;
    if let (Some(key), Some(embedding_model), CacheMode::Sqlite) = (
        &cache_key,
        config.semantic_embedding_model.as_deref(),
        config.semantic_cache,
    ) {
        let prompt = crate::semantic::canonical_prompt(&attempt.canonical);
        match embed_text(state, caller.team_id(), embedding_model, &prompt).await {
            Ok(vector) => {
                let scope = rrouter_store::SemanticCacheScope {
                    embedding_model,
                    provider: &key.provider,
                    model: &key.model,
                    team_id: caller.team_id(),
                };
                match state
                    .store
                    .semantic_cache_scan(&scope, crate::now_unix(), SEMANTIC_SCAN_LIMIT)
                    .await
                {
                    Ok(candidates) => {
                        let mut best: Option<(f32, i64, String)> = None;
                        for candidate in candidates {
                            let stored = crate::semantic::decode_embedding(&candidate.embedding);
                            let score = crate::semantic::cosine(&vector, &stored);
                            if score >= config.semantic_cache_threshold as f32
                                && best.as_ref().is_none_or(|(seen, _, _)| score > *seen)
                            {
                                best = Some((score, candidate.id, candidate.body));
                            }
                        }
                        if let Some((_score, id, body)) = best {
                            // Best effort: a lost hit counter must not fail the response.
                            if let Err(e) = state.store.semantic_cache_hit(id).await {
                                tracing::warn!(error = %e, "semantic cache hit counter failed");
                            }
                            return serve_cache_hit(
                                state,
                                &caller,
                                &plan.candidates[0],
                                &plan.model,
                                inbound,
                                key_id,
                                trace,
                                started,
                                body,
                                "semantic-hit",
                                redact_pii,
                            )
                            .await;
                        }
                    }
                    Err(e) => tracing::warn!(error = %e, "semantic cache read failed"),
                }
                semantic_embedding = Some(vector);
            }
            Err(e) => {
                // The prompt is never part of this line: only the failure class, which is
                // all an operator needs to know the cache is degraded.
                tracing::warn!(
                    status = e.status(),
                    "semantic cache embedding failed; serving without it"
                );
            }
        }
    }

    if !client_stream {
        // Two ways to get one JSON body: a plain non-streaming dispatch, or a
        // stream-only upstream whose stream is folded back into one response.
        let (served, out) = if upstream_stream {
            let (candidate, bytes) = state
                .dispatcher
                .run_streaming(&plan, |c| attempt.open_stream(c))
                .await?;
            let open = StreamRoute {
                upstream: attempt.route_dialect(&candidate),
                client_model: candidate.model.clone(),
            };
            let mut assembled = reassemble(&open, bytes).await?;
            let recorded = Recorded {
                candidate: &candidate,
                model: &plan.model,
                endpoint: endpoint_of(inbound),
                usage: assembled.usage,
                key: key_id,
                request_id: trace.request_id,
                trace_id: trace.trace_id(),
                started,
                upstream_ms: trace.upstream_ms(),
                ttft: None,
                streamed: false,
                status: 200,
                error: None,
                budget_hold: trace.take_budget_hold(),
            };
            // Scan before the row is written, so a refusal is logged as one 403 row
            // instead of a 200 that the client never received.
            if let Some(policy) = caller.output_guardrails() {
                guard_output(state, policy, &response_text(&assembled), &recorded)?;
            }
            // Redaction happens after the guardrail decision (which must see the original
            // text) and before the response is built, cached or returned.
            if redact_pii {
                let report = crate::redaction::redact_chat_response(&mut assembled);
                if report.redacted {
                    crate::metrics::pii_redacted(crate::metrics::RedactStage::Output, &report);
                    tracing::debug!(stage = "output", %report, "pii redacted");
                }
            }
            record_once(state, recorded);
            (
                candidate,
                rrouter_translate::build_response(inbound, &assembled)?,
            )
        } else {
            let (candidate, mut resp) = complete_traced(
                state,
                inbound,
                &body,
                key_id,
                caller.output_guardrails(),
                Some(headers),
                policy.as_ref(),
                caller.allowed_models(),
                caller.team_id(),
                combo_start,
                trace,
                redact_pii,
            )
            .await?;
            if redact_pii {
                let report = crate::redaction::redact_chat_response(&mut resp);
                if report.redacted {
                    crate::metrics::pii_redacted(crate::metrics::RedactStage::Output, &report);
                    tracing::debug!(stage = "output", %report, "pii redacted");
                }
            }
            (
                candidate,
                rrouter_translate::build_response(inbound, &resp)?,
            )
        };

        // Serialize once: the bytes stored are exactly the bytes the client receives,
        // so a later hit is byte-identical to this response.
        let text = out.to_string();
        if let Some(key) = &cache {
            // Only what this key describes may be stored: a fallback to another model,
            // or a provider that speaks another dialect, is a different request.
            if served.model == key.model && attempt.route_dialect(&served) == key.dialect {
                let now = crate::now_unix();
                let expires_at = now.saturating_add(config.cache_ttl_secs as i64);
                if let Err(e) = state
                    .store
                    .cache_put(&key.hash, &text, now, expires_at, config.cache_max_entries)
                    .await
                {
                    tracing::warn!(error = %e, "response cache write failed");
                }
            }
        }
        // Semantic write: only when this request paid for an embedding (a miss), and only
        // for the provider the scope was computed from. A fallback to another provider is
        // not stored — a later request looks in the first provider's scope and would
        // never find it.
        if let (Some(key), Some(vector), Some(embedding_model)) = (
            &cache_key,
            semantic_embedding.as_ref(),
            config.semantic_embedding_model.as_deref(),
        ) {
            if config.semantic_cache == CacheMode::Sqlite
                && served.provider == key.provider
                && served.model == key.model
                && attempt.route_dialect(&served) == key.dialect
            {
                let prompt = crate::semantic::canonical_prompt(&attempt.canonical);
                let now = crate::now_unix();
                let entry = rrouter_store::SemanticCacheEntry {
                    prompt_hash: crate::semantic::prompt_hash(&prompt),
                    embedding_model: embedding_model.to_string(),
                    provider: served.provider.clone(),
                    model: served.model.clone(),
                    team_id: caller.team_id(),
                    embedding: crate::semantic::encode_embedding(vector),
                    body: text.clone(),
                    created_at: now,
                    expires_at: now.saturating_add(config.semantic_cache_ttl_secs as i64),
                };
                if let Err(e) = state
                    .store
                    .semantic_cache_put(&entry, config.semantic_cache_max_entries)
                    .await
                {
                    tracing::warn!(error = %e, "semantic cache write failed");
                }
            }
        }
        let mut resp = json_text_response(text);
        if cache.is_some() {
            insert_cache_header(&mut resp, "miss");
        }
        return Ok(resp);
    }

    // The dispatcher only sees `Result<ByteStream>`; which dialect that stream speaks is
    // carried alongside by [`OpenStream`], so the wrapper never guesses.
    let (candidate, bytes) = state
        .dispatcher
        .run_streaming(&plan, |c| attempt.open_stream(c))
        .await?;
    let open = StreamRoute {
        upstream: attempt.route_dialect(&candidate),
        client_model: candidate.model.clone(),
    };
    Ok(stream_response(
        inbound,
        PendingRecord {
            state: Arc::clone(state),
            candidate,
            model: plan.model.clone(),
            started,
            upstream_ms: trace.upstream_ms(),
            key: key_id,
            request_id: trace.request_id,
            trace_id: trace.trace_id().to_string(),
            // The slot belongs to the stream, not to this handler: it is released when
            // the body ends or the client disconnects, whichever comes first.
            _permit: permit,
            budget_hold: trace.take_budget_hold(),
        },
        open,
        bytes,
        caller.output_guardrails().cloned(),
        redact_pii,
    ))
}

/// One non-streaming completion through the full routing path, attributed to no key.
/// Used by the admin probe, whose traffic belongs to the operator, not to a virtual key.
pub async fn complete(
    state: &Arc<AppState>,
    inbound: Dialect,
    body: &Value,
) -> Result<rrouter_core::ChatResponse> {
    complete_for(state, inbound, body, None).await
}

/// [`complete`] with usage attributed to `key` (the gateway key that paid for it).
///
/// Shared by the proxy and the admin probe so a green "test" button means exactly what
/// a client request would do: same planner, same fallback, same forced streaming for
/// providers that reject non-streaming calls, same reassembly.
pub async fn complete_for(
    state: &Arc<AppState>,
    inbound: Dialect,
    body: &Value,
    key: Option<Uuid>,
) -> Result<rrouter_core::ChatResponse> {
    // A probe is still a request: it gets its own id, its attempt rows and its usage
    // row, so nothing it does is invisible in the log. It carries no key's policy: the
    // probe is the operator's own traffic, not a team's.
    let trace = RequestTrace::new(Uuid::new_v4(), inbound, None);
    // No inbound request means no client headers and no header-form policy; a `provider`
    // object in the probe's own body is still honoured. There is no allowlist either: a
    // probe is the operator's own traffic, not a key's, so no key's model policy applies
    // to it (the admin gate already decided the caller may probe at all).
    let policy = ProviderPolicy::from_body(body)?;
    // A probe runs with no key and therefore instance-wide; a keyed completion is routed
    // inside that key's team, exactly as the proxy path is.
    let team_id = match key {
        Some(id) => state.store.key_team(id).await?,
        None => None,
    };
    // A probe is a real request: its combo strategy resolves once here, and the same
    // start is handed to the planner below (no second round-robin advance).
    let canonical = rrouter_translate::parse_request(inbound, body)?;
    let model = state.mapped_model(key, &canonical.model).await?;
    let combo_start = combo_rotation(state, &model, &canonical, key, &trace).await?;
    complete_traced(
        state,
        inbound,
        body,
        key,
        None,
        None,
        policy.as_ref(),
        None,
        team_id,
        combo_start,
        &trace,
        false,
    )
    .await
    .map(|(_, resp)| resp)
}

/// Both the candidate that answered and its canonical response. The candidate is what
/// lets the caller decide whether the answer may be cached under the key it planned.
///
/// `guardrails` is the paying key's output policy: scanning happens here, before the
/// usage row is written, so a refusal is one `403` row and never a `200` that the
/// client did not receive. `client` is the inbound request's headers (`None` for the
/// admin probe, which has no client), and `policy` is the routing policy the caller
/// already parsed from the body plus `x-rrouter-*` headers — parsed once, so the
/// non-streaming path cannot disagree with the planner's inputs.
///
/// `allowed_models` is the paying key's model allowlist. It is a required input here,
/// not an optional extra: this function builds the plan that actually dispatches on the
/// non-streaming path, so planning without it would let a provider-scoped allowlist be
/// bypassed by exactly the requests it exists to constrain (a lower-priority or
/// differently-spelled provider serving the same model id).
///
/// `redact_pii` is the key's redaction opt-in, kept separate from `guardrails`: a key can
/// opt into redaction without opting into output scanning, and the upstream error path
/// needs it even when no output policy exists.
#[allow(clippy::too_many_arguments)]
async fn complete_traced(
    state: &Arc<AppState>,
    inbound: Dialect,
    body: &Value,
    key: Option<Uuid>,
    guardrails: Option<&Guardrails>,
    client: Option<&HeaderMap>,
    policy: Option<&ProviderPolicy>,
    allowed_models: Option<&[String]>,
    team_id: Option<Uuid>,
    combo_start: Option<usize>,
    trace: &RequestTrace,
    redact_pii: bool,
) -> Result<(Candidate, rrouter_core::ChatResponse)> {
    trace.set_key(key);
    let started = Instant::now();
    let canonical = rrouter_translate::parse_request(inbound, body)?;
    let model = state.mapped_model(key, &canonical.model).await?;
    trace.set_model(&model);
    let config = state.config.load();
    let affinity = affinity_of(&canonical);
    let canary_seed = canary_seed_for(trace, Some(affinity));
    let capacity = crate::provider_usage::cached_hints();
    let plan = state
        .planner
        .plan_with_capacity(
            state.catalog.as_ref(),
            state.store.as_ref(),
            PlanRequest {
                model: &model,
                stream: canonical.stream,
                affinity: Some(affinity),
                strategy: config.strategy,
                policy,
                allowed_models,
                team_id,
                canary: config.canary.as_ref(),
                canary_seed,
                combo_start,
            },
            Some(&capacity),
            f64::from(config.subscription_reserve_percent),
        )
        .await?;
    let upstream_stream =
        plan.stream || canonical.stream || plan.candidates.iter().any(stream_only);
    let attempt = Attempt {
        state,
        inbound,
        canonical,
        raw: body,
        upstream_stream,
        trace,
        strip_provider: policy.is_some(),
        client_headers: client,
        redact_pii,
    };

    if !upstream_stream {
        let outcome = state
            .dispatcher
            .run(&plan, |c| attempt.send_once(c))
            .await?;
        let candidate = outcome.candidate.clone();
        let resp = outcome.result?;
        let recorded = Recorded {
            candidate: &candidate,
            model: &plan.model,
            endpoint: endpoint_of(inbound),
            usage: resp.usage,
            key,
            request_id: trace.request_id,
            trace_id: trace.trace_id(),
            started,
            upstream_ms: trace.upstream_ms(),
            ttft: None,
            streamed: false,
            status: 200,
            error: None,
            budget_hold: trace.take_budget_hold(),
        };
        if let Some(policy) = guardrails {
            guard_output(state, policy, &response_text(&resp), &recorded)?;
        }
        record_once(state, recorded);
        return Ok((candidate, resp));
    }

    let (candidate, bytes) = state
        .dispatcher
        .run_streaming(&plan, |c| attempt.open_stream(c))
        .await?;
    let open = StreamRoute {
        upstream: attempt.route_dialect(&candidate),
        client_model: candidate.model.clone(),
    };
    let assembled = reassemble(&open, bytes).await?;
    let recorded = Recorded {
        candidate: &candidate,
        model: &plan.model,
        endpoint: endpoint_of(inbound),
        usage: assembled.usage,
        key,
        request_id: trace.request_id,
        trace_id: trace.trace_id(),
        started,
        upstream_ms: trace.upstream_ms(),
        ttft: None,
        streamed: false,
        status: 200,
        error: None,
        budget_hold: trace.take_budget_hold(),
    };
    if let Some(policy) = guardrails {
        guard_output(state, policy, &response_text(&assembled), &recorded)?;
    }
    record_once(state, recorded);
    Ok((candidate, assembled))
}

/// Fold a streamed response back into a single canonical response.
///
/// Used only when the upstream cannot answer non-streaming. Every event is consumed
/// through the same decoder the streaming path uses, so the two paths cannot drift.
async fn reassemble(open: &StreamRoute, bytes: ByteStream) -> Result<rrouter_core::ChatResponse> {
    use futures_util::StreamExt as _;

    let mut decoder = rrouter_translate::StreamDecoder::new(open.upstream, &open.client_model);
    let mut text = String::new();
    let mut reasoning = String::new();
    let mut tools: Vec<(u32, String, String, String)> = Vec::new();
    let mut usage = Usage::default();
    let mut finish = rrouter_core::FinishReason::Stop;
    let mut id = String::new();
    let mut model = open.client_model.clone();
    let mut stream = bytes;

    let absorb = |events: Vec<StreamEvent>,
                  text: &mut String,
                  reasoning: &mut String,
                  tools: &mut Vec<(u32, String, String, String)>,
                  usage: &mut Usage,
                  finish: &mut rrouter_core::FinishReason,
                  id: &mut String,
                  model: &mut String| {
        for ev in events {
            match ev {
                StreamEvent::Start {
                    id: sid,
                    model: smodel,
                } => {
                    if !sid.is_empty() {
                        *id = sid;
                    }
                    if !smodel.is_empty() {
                        *model = smodel;
                    }
                }
                StreamEvent::TextDelta(t) => text.push_str(&t),
                StreamEvent::ReasoningDelta(t) => reasoning.push_str(&t),
                StreamEvent::ToolCallStart { index, id, name } => {
                    tools.push((index, id, name, String::new()))
                }
                StreamEvent::ToolCallArgsDelta {
                    index,
                    partial_json,
                } => {
                    if let Some(slot) = tools.iter_mut().find(|t| t.0 == index) {
                        slot.3.push_str(&partial_json);
                    }
                }
                StreamEvent::Done { reason, usage: u } => {
                    *finish = reason;
                    if let Some(u) = u {
                        *usage = u;
                    }
                }
            }
        }
    };

    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        let events = decoder.push(&chunk)?;
        absorb(
            events,
            &mut text,
            &mut reasoning,
            &mut tools,
            &mut usage,
            &mut finish,
            &mut id,
            &mut model,
        );
    }
    let events = decoder.finish()?;
    absorb(
        events,
        &mut text,
        &mut reasoning,
        &mut tools,
        &mut usage,
        &mut finish,
        &mut id,
        &mut model,
    );

    let mut parts: Vec<rrouter_core::Part> = Vec::with_capacity(tools.len() + 2);
    if !reasoning.is_empty() {
        parts.push(rrouter_core::Part::Reasoning {
            text: reasoning,
            signature: None,
        });
    }
    if !text.is_empty() {
        parts.push(rrouter_core::Part::Text(text));
    }
    for (_, tid, name, args) in tools {
        let input = serde_json::from_str(&args).map_err(|e| {
            rrouter_core::Error::Translation(format!(
                "tool call {name:?} ({tid}) returned invalid JSON arguments: {e}"
            ))
        })?;
        parts.push(rrouter_core::Part::ToolUse {
            id: tid,
            name,
            input,
        });
    }
    Ok(rrouter_core::ChatResponse {
        id,
        model,
        parts,
        finish_reason: finish,
        usage,
    })
}

/// Stable per-conversation key for account selection.
///
/// Hashing the model, the system prompt, the first turn, and an explicit
/// `prompt_cache_key` gives every turn of one conversation the same upstream account
/// while still spreading unrelated conversations across the pool.
fn affinity_of(req: &ChatRequest) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    req.model.hash(&mut hasher);
    req.system.hash(&mut hasher);
    if let Some(first) = req.messages.first() {
        first.joined_text().hash(&mut hasher);
    }
    if let Some((_, extra)) = &req.extra {
        if let Some(key) = extra.get("prompt_cache_key").and_then(Value::as_str) {
            key.hash(&mut hasher);
        }
    }
    hasher.finish()
}

/// Codex answers through the Responses endpoint with sampling controls removed: a
/// ChatGPT-backed account rejects `temperature`/`top_p` outright instead of ignoring
/// them ("Unsupported parameter: temperature"), which failed every Playground turn.
fn drop_unsupported_sampling(provider: &str, body: &mut Value) {
    if provider != "codex" {
        return;
    }
    if let Some(obj) = body.as_object_mut() {
        obj.remove("temperature");
        obj.remove("top_p");
    }
}

/// Forward a client body unchanged in its own dialect, patching only routing fields.
///
/// The canonical model cannot represent every protocol feature, and two of the losses
/// are expensive: Anthropic `cache_control` (automatic and block-level breakpoints) and
/// the OpenAI Responses caching fields (`prompt_cache_options`, `prompt_cache_key`,
/// per-block `prompt_cache_breakpoint`). Rebuilding the body drops them, so a warm
/// 0.1x cache read silently becomes a full-price prompt.
///
/// Only what the provider itself demands is changed: the resolved model id, streaming,
/// `store:false` for Codex, and a bare-string `input` expanded into a message list.
///
/// `strip_provider` removes a client `provider` object the gateway already interpreted:
/// routing preferences are RRouter's to honour, and forwarding them to a single upstream
/// only invites a strict schema to reject the whole request.
fn same_dialect_body(
    dialect: Dialect,
    raw: &Value,
    model: &str,
    stream: bool,
    strip_provider: bool,
) -> Value {
    let mut out = raw.clone();
    let Some(obj) = out.as_object_mut() else {
        return out;
    };
    if strip_provider {
        obj.remove("provider");
    }
    obj.insert("model".into(), Value::String(model.to_string()));
    match dialect {
        Dialect::OpenAiResponses => {
            // Codex rejects stored responses ("Store must be set to false").
            obj.insert("store".into(), Value::Bool(false));
            if let Some(Value::String(text)) = obj.get("input").cloned() {
                obj.insert(
                    "input".into(),
                    serde_json::json!([{
                        "role": "user",
                        "content": [{"type": "input_text", "text": text}],
                    }]),
                );
            }
        }
        Dialect::OpenAiChat => {
            if stream {
                // Usage only arrives in the final chunk when asked for explicitly.
                let mut opts = obj
                    .get("stream_options")
                    .and_then(Value::as_object)
                    .cloned()
                    .unwrap_or_default();
                opts.insert("include_usage".into(), Value::Bool(true));
                obj.insert("stream_options".into(), Value::Object(opts));
            }
        }
        Dialect::AnthropicMessages | Dialect::OpenAiEmbeddings => {}
        // Upstream-only: the proxy never forwards an inbound CCA body.
        Dialect::GoogleCodeAssist => {}
    }
    obj.insert("stream".into(), Value::Bool(stream));
    out
}

/// Outbound URL + dialect for a candidate's provider. Public so the admin probe uses
/// exactly the same routing rules as the hot path.
pub fn endpoint_for(
    spec: &ProviderSpec,
    override_url: Option<&str>,
    inbound: Dialect,
) -> (String, Dialect) {
    endpoint(spec, override_url, inbound)
}

/// Provider's native endpoint for the inbound dialect, when it has one.
fn native_url(spec: &ProviderSpec, inbound: Dialect) -> Option<&'static str> {
    match inbound {
        Dialect::AnthropicMessages => spec.messages_url,
        Dialect::OpenAiResponses => spec.responses_url,
        // `base_url` speaks Chat only when the provider's native dialect is Chat.
        // Codex exposes a Responses URL as its base; claiming it speaks Chat sent a
        // chat-completions body to the Responses endpoint.
        Dialect::OpenAiChat if spec.default_dialect == Dialect::OpenAiChat => Some(spec.base_url),
        Dialect::OpenAiChat => None,
        Dialect::OpenAiEmbeddings => None,
        Dialect::GoogleCodeAssist => None,
    }
}

/// Outbound URL and the dialect that URL speaks.
///
/// A connection's `base_url_override` always wins: it is the deployment's statement about
/// where this account's traffic goes (self-hosted gateway, regional endpoint, mock). The
/// native endpoint's path is grafted onto the override's origin, so a provider that speaks
/// the inbound dialect keeps being talked to in that dialect.
fn endpoint(
    spec: &ProviderSpec,
    override_url: Option<&str>,
    inbound: Dialect,
) -> (String, Dialect) {
    let (native, dialect) = match native_url(spec, inbound) {
        Some(url) => (url, inbound),
        None => (spec.base_url, spec.default_dialect),
    };
    let url = match override_url {
        Some(base) => rewrite_base(native, base),
        None => native.to_string(),
    };
    // Manifest-declared query parameters (`api-version` and friends). A parameter the
    // connection's own URL already sets wins, which is how a tenant pins a version.
    (rrouter_auth::with_query(&url, spec), dialect)
}

/// Keep the native endpoint's path but move it to the override's origin, so tests and
/// self-hosted gateways can redirect a provider without lying about its dialect.
fn rewrite_base(native: &str, override_base: &str) -> String {
    let (Ok(native_url), Ok(over)) = (
        reqwest::Url::parse(native),
        reqwest::Url::parse(override_base),
    ) else {
        return override_base.to_string();
    };
    let mut out = over;
    out.set_path(native_url.path());
    out.to_string()
}

/// Outbound headers for one upstream call: the provider's auth and static headers, plus
/// the client's own protocol headers that this provider declares it understands.
///
/// `client` is the inbound request's header map. Only names in
/// [`ProviderSpec::passthrough_headers`] are read at all (`anthropic-beta` for the
/// Claude/Anthropic surface, `openai-beta` for OpenAI's), so a client's `authorization`,
/// `x-api-key`, `cookie` or `host` can never reach a provider. A header the gateway
/// already set is unioned, never replaced, so neither side's beta flags are lost.
fn outbound_headers(
    spec: &ProviderSpec,
    creds: &Credentials,
    client: Option<&axum::http::HeaderMap>,
) -> Result<axum::http::HeaderMap> {
    let mut pairs = rrouter_auth::build_headers(spec, creds)?;
    if let Some(client) = client {
        rrouter_auth::merge_client_headers(spec, &mut pairs, client);
    }
    let mut headers = axum::http::HeaderMap::new();
    for (name, value) in pairs {
        if let (Ok(name), Ok(value)) = (
            header::HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_str(&value),
        ) {
            headers.append(name, value);
        }
    }
    headers
        .entry(header::CONTENT_TYPE)
        .or_insert(HeaderValue::from_static("application/json"));
    Ok(headers)
}

/// Stable `x-opencode-session` for one conversation.
///
/// OpenCode asks clients for a stable conversation id and rejects a missing header. A
/// native client header wins when it is a sane value; otherwise the gateway hashes its
/// conversation affinity into the `ses_<32 hex>` shape OpenCode uses.
fn opencode_session(client: Option<&HeaderMap>, affinity: u64) -> String {
    if let Some(value) = client
        .and_then(|headers| headers.get("x-opencode-session"))
        .and_then(|value| value.to_str().ok())
    {
        let value = value.trim();
        if !value.is_empty() && value.len() <= 256 {
            return value.to_string();
        }
    }
    let mut digest = Sha256::new();
    digest.update(b"opencode-go\0rrouter\0");
    digest.update(affinity.to_le_bytes());
    let digest = digest.finalize();
    let mut out = String::with_capacity(36);
    out.push_str("ses_");
    use std::fmt::Write;
    for byte in &digest[..16] {
        // Writing a two-digit hex byte into a String cannot fail.
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// Provider headers with nothing of the client's added, for calls that have no inbound
/// request (the admin probe).
fn request_headers(spec: &ProviderSpec, creds: &Credentials) -> Result<axum::http::HeaderMap> {
    outbound_headers(spec, creds, None)
}

fn transport_error(e: reqwest::Error) -> Error {
    if e.is_timeout() {
        Error::Timeout(e.to_string())
    } else {
        Error::Transport(e.to_string())
    }
}

fn upstream_error(candidate: &Candidate, status: u16, bytes: &Bytes) -> Error {
    let text = String::from_utf8_lossy(bytes);
    let body = if text.len() > MAX_ERROR_BODY {
        let mut cut = MAX_ERROR_BODY;
        while !text.is_char_boundary(cut) {
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

fn json_response(status: StatusCode, body: Value) -> Response {
    (status, axum::Json(body)).into_response()
}

/// `true` when this candidate's provider cannot answer non-streaming requests.
fn stream_only(candidate: &Candidate) -> bool {
    rrouter_auth::get(&candidate.provider).is_some_and(|spec| spec.stream_only)
}

/// One cache lookup: the hashed key plus the exact model id and wire dialect it was
/// built from. A response may only be stored under it when the candidate that answered
/// matches both, so a fallback to a different model never poisons the entry.
#[derive(Debug, Clone)]
struct CacheKey {
    hash: String,
    model: String,
    /// Provider the plan would call first: the semantic cache scope dimension.
    provider: String,
    dialect: Dialect,
}

/// Write `value` with object keys sorted, so two spellings of the same JSON request
/// hash to the same key. Arrays keep their order: message and tool order are part of
/// the request. Scalars are rendered by `serde_json`, so numbers keep their value.
fn write_canonical(value: &Value, out: &mut String) {
    match value {
        Value::Object(map) => {
            out.push('{');
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_unstable();
            for (index, key) in keys.into_iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(key).expect("a JSON string always serializes"));
                out.push(':');
                write_canonical(&map[key], out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_canonical(item, out);
            }
            out.push(']');
        }
        scalar => out.push_str(&scalar.to_string()),
    }
}

/// `x-rrouter-cache: bypass` skips the read *and* the write: a bypassed request never
/// observes a stored response and never extends one.
fn cache_bypassed(headers: &HeaderMap) -> bool {
    headers
        .get(CACHE_HEADER)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.trim().eq_ignore_ascii_case("bypass"))
}

/// A JSON body that is already serialized, so the bytes stored in the cache and the
/// bytes the client receives are the same string.
fn json_text_response(body: String) -> Response {
    let mut resp = Response::new(Body::from(body));
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    resp
}

fn insert_cache_header(resp: &mut Response, state: &'static str) {
    resp.headers_mut()
        .insert(CACHE_HEADER, HeaderValue::from_static(state));
}

/// A stored response: same JSON body the original client received, plus the header
/// that tells this client it came from the cache (`hit` or `semantic-hit`).
fn cached_response(body: String, label: &'static str) -> Response {
    let mut resp = json_text_response(body);
    insert_cache_header(&mut resp, label);
    resp
}

/// Serve one cache hit — exact or semantic — through the same policy path a fresh
/// response takes.
///
/// A stored body is client-facing JSON, so it is still subject to this key's output
/// guardrails (a guardrail added after the row was written must not be bypassed) and to
/// on-the-way-out redaction. The usage row is a zero-token cache-hit row: the upstream
/// was not called for this request.
#[allow(clippy::too_many_arguments)]
async fn serve_cache_hit(
    state: &Arc<AppState>,
    caller: &Caller,
    candidate: &Candidate,
    model: &str,
    inbound: Dialect,
    key_id: Option<Uuid>,
    trace: &RequestTrace,
    started: Instant,
    body: String,
    label: &'static str,
    redact_pii: bool,
) -> Result<Response> {
    // Take the hold once: a guardrail refusal and the cache-hit row share it, and the
    // local handle keeps it alive until `record_cache_hit` moves it into the writer.
    let budget_hold = trace.take_budget_hold();
    if let Some(policy) = caller.output_guardrails() {
        let recorded = Recorded {
            candidate,
            model,
            endpoint: endpoint_of(inbound),
            usage: Usage::default(),
            key: key_id,
            request_id: trace.request_id,
            trace_id: trace.trace_id(),
            started,
            upstream_ms: 0,
            ttft: None,
            streamed: false,
            status: 200,
            error: None,
            budget_hold: budget_hold.clone(),
        };
        guard_output(state, policy, &body, &recorded)?;
    }
    crate::metrics::cache_hit();
    trace.set_provider(&candidate.provider);
    record_cache_hit(
        state,
        candidate,
        model,
        inbound,
        key_id,
        trace,
        started,
        budget_hold,
    );
    // A cached body may have been written by a key without redaction (or before this
    // policy existed): re-redact on the way out. Idempotent.
    let body = if redact_pii {
        let (redacted, report) = crate::redaction::redact(&body);
        if report.redacted {
            crate::metrics::pii_redacted(crate::metrics::RedactStage::Output, &report);
            tracing::debug!(stage = "output", %report, "pii redacted from cache");
        }
        redacted.into_owned()
    } else {
        body
    };
    Ok(cached_response(body, label))
}

/// One usage row for a cache hit: the request happened and is joinable to the activity
/// log by the `x-request-id` the client saw, but it cost nothing. Tokens stay zero —
/// no prompt was sent and nothing was generated, so inventing usage here would corrupt
/// spend, budgets and TPM accounting.
#[allow(clippy::too_many_arguments)]
fn record_cache_hit(
    state: &AppState,
    candidate: &Candidate,
    model: &str,
    inbound: Dialect,
    key: Option<Uuid>,
    trace: &RequestTrace,
    started: Instant,
    budget_hold: Option<Arc<crate::state::BudgetHold>>,
) {
    spawn_record(
        state,
        UsageRecord {
            id: None,
            ts: crate::now_unix(),
            request_id: Some(trace.request_id),
            trace_id: Some(trace.trace_id().to_string()),
            provider: candidate.provider.clone(),
            model: model.to_string(),
            connection_id: Some(candidate.connection),
            endpoint: Some(endpoint_of(inbound).to_string()),
            key_id: key,
            cost_micros: 0,
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            latency_ms: started.elapsed().as_millis().min(u32::MAX as u128) as u32,
            ttft_ms: 0,
            upstream_ms: 0,
            status: 200,
            streamed: false,
            error: None,
        },
        budget_hold,
    );
}

/// Which dialect the opened stream speaks, and the model id to label it with.
struct StreamRoute {
    upstream: Dialect,
    client_model: String,
}

/// An opened upstream byte stream. `Wire` re-derives the dialect from the route, so the
/// stream itself only needs to stay Send + 'static.
struct OpenStream {
    bytes: ByteStream,
}

impl Stream for OpenStream {
    type Item = Result<Bytes>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Result<Bytes>>> {
        self.bytes.as_mut().poll_next(cx)
    }
}

/// Per-request attempt state, shared across fallback attempts.
struct Attempt<'a> {
    state: &'a Arc<AppState>,
    inbound: Dialect,
    canonical: ChatRequest,
    /// The client's body, untouched. Same-dialect requests are forwarded from this,
    /// because the canonical model cannot represent every protocol feature.
    raw: &'a Value,
    /// Body the provider can accept unmodified because it speaks the inbound dialect.
    /// Upstream must stream even if the client asked for one JSON response.
    upstream_stream: bool,
    /// Request id and attempt counter shared by every attempt of this request.
    trace: &'a RequestTrace,
    /// The body carried a `provider` object that the gateway consumed as routing policy:
    /// strip it before the body reaches an upstream, which has no use for it and may
    /// reject the field outright.
    strip_provider: bool,
    /// The client's inbound headers, for the protocol headers a provider opts into
    /// forwarding (see [`outbound_headers`]). `None` for the admin probe, which has no
    /// client.
    client_headers: Option<&'a axum::http::HeaderMap>,
    /// The paying key opted into PII redaction: provider error text is redacted before it
    /// is stored or returned, because an upstream error can echo the prompt.
    redact_pii: bool,
}

impl Attempt<'_> {
    /// Wrap an upstream failure, redacting the text first when the key opted in: a
    /// provider error often echoes part of the prompt, and this string is both stored on
    /// the usage row and returned to the client.
    fn upstream_error(&self, candidate: &Candidate, status: u16, bytes: &Bytes) -> Error {
        if self.redact_pii {
            let text = String::from_utf8_lossy(bytes);
            let (redacted, report) = crate::redaction::redact(&text);
            if report.redacted {
                crate::metrics::pii_redacted(crate::metrics::RedactStage::Output, &report);
                let redacted = Bytes::from(redacted.into_owned());
                return upstream_error(candidate, status, &redacted);
            }
        }
        upstream_error(candidate, status, bytes)
    }

    fn provider(&self, candidate: &Candidate) -> Result<&'static ProviderSpec> {
        rrouter_auth::get(&candidate.provider).ok_or_else(|| Error::NoRoute {
            model: candidate.model.clone(),
            detail: format!("unknown provider {}", candidate.provider),
        })
    }

    /// Dialect the upstream speaks for this candidate and inbound path.
    fn route_dialect(&self, candidate: &Candidate) -> Dialect {
        if is_google_cca(&candidate.provider) {
            return Dialect::GoogleCodeAssist;
        }
        match rrouter_auth::get(&candidate.provider) {
            Some(spec) => endpoint(spec, None, self.inbound).1,
            None => candidate.dialect,
        }
    }

    /// Build the upstream body for one attempt. Never mutates the canonical request, so
    /// concurrent attempts all see the client's original values.
    fn outbound(
        &self,
        candidate: &Candidate,
        dialect: Dialect,
        project: Option<&str>,
    ) -> Result<Value> {
        if dialect == Dialect::GoogleCodeAssist {
            let project = project.ok_or_else(|| {
                Error::BadRequest(
                    "google-antigravity connection is missing credentials.extra.project_id".into(),
                )
            })?;
            return rrouter_translate::google_cca::build_request(
                &self.canonical,
                &candidate.model,
                project,
            );
        }
        let mut body = if dialect == self.inbound {
            same_dialect_body(
                dialect,
                self.raw,
                &candidate.model,
                self.upstream_stream,
                self.strip_provider,
            )
        } else {
            let mut request = self.canonical.clone();
            request.model = candidate.model.clone();
            // The provider, not the canonical model, decides how the output cap is
            // spelled (`max_tokens` vs `max_completion_tokens`) and what to send when the
            // client named none. `cache_key` goes through this same call, so a cached
            // entry always matches the bytes this attempt would send.
            let wire = self.provider(candidate)?.wire_profile();
            rrouter_translate::build_request_with(dialect, &request, self.upstream_stream, &wire)?
        };
        drop_unsupported_sampling(&candidate.provider, &mut body);
        Ok(body)
    }

    /// Identity of the exact upstream request this attempt would send for `candidate`.
    ///
    /// The key covers the canonical request the gateway would put on the wire — the
    /// resolved model (already patched into the body), messages, system, tools, sampling
    /// fields, and any dialect passthrough — plus both dialects. Two requests share an
    /// entry only when they are the same request to the same API in the same shape.
    ///
    /// A per-key model mapping is deliberately *not* part of the key: a mapping changes
    /// routing, and that routing is already baked into the resolved model and dialect,
    /// so a key with a mapping and a key without one share an entry when they resolve
    /// to the same upstream request.
    fn cache_key(&self, candidate: &Candidate) -> Result<CacheKey> {
        let dialect = self.route_dialect(candidate);
        if dialect == Dialect::GoogleCodeAssist {
            return Err(Error::Translation(
                "google code assist envelopes are request-scoped and not cacheable".into(),
            ));
        }
        let body = self.outbound(candidate, dialect, None)?;
        let mut canonical = String::with_capacity(1024);
        canonical.push_str(self.inbound.as_str());
        canonical.push('\n');
        canonical.push_str(dialect.as_str());
        canonical.push('\n');
        write_canonical(&body, &mut canonical);
        Ok(CacheKey {
            hash: rrouter_store::cache_key(&canonical),
            model: candidate.model.clone(),
            provider: candidate.provider.clone(),
            dialect,
        })
    }

    async fn prepare(&self, candidate: &Candidate, trace: &AttemptTrace) -> Result<Prepared> {
        let spec = self.provider(candidate)?;
        let creds = self
            .state
            .refresher
            .fresh(self.state.store.as_ref(), spec, candidate.connection)
            .await?;
        if is_google_cca(&candidate.provider) {
            let project = cca_project(&creds)?;
            // A credential override is a deployment/test statement about where this
            // account goes; the env override applies only to the built-in daily host.
            let daily = match creds.base_url_override.as_deref() {
                Some(base) => base.trim_end_matches('/').to_string(),
                None => rrouter_translate::google_cca::daily_host(spec.base_url),
            };
            let prod = rrouter_translate::google_cca::prod_host();
            let mut urls = vec![rrouter_translate::google_cca::endpoint(
                &daily,
                self.upstream_stream,
            )];
            if prod != daily {
                urls.push(rrouter_translate::google_cca::endpoint(
                    &prod,
                    self.upstream_stream,
                ));
            }
            let mut headers = google_cca_headers(spec, &creds, self.client_headers)?;
            inject_trace_headers(&mut headers, trace);
            return Ok(Prepared {
                urls,
                dialect: Dialect::GoogleCodeAssist,
                headers,
                project: Some(project),
            });
        }
        let (url, dialect) = endpoint(spec, creds.base_url_override.as_deref(), self.inbound);
        let mut headers = outbound_headers(spec, &creds, self.client_headers)?;
        // OpenCode Go rejects requests without a session id (`MissingSessionID`). The
        // affinity hash is stable for one conversation and unique across conversations,
        // which is exactly the routing/prompt-cache key OpenCode asks for.
        if matches!(candidate.provider.as_str(), "opencode" | "opencode-go") {
            let session = opencode_session(self.client_headers, affinity_of(&self.canonical));
            if let Ok(value) = HeaderValue::from_str(&session) {
                headers.insert("x-opencode-session", value);
            }
        }
        inject_trace_headers(&mut headers, trace);
        Ok(Prepared {
            urls: vec![url],
            dialect,
            headers,
            project: None,
        })
    }

    /// POST one attempt, trying the CCA production fallback after a transport failure or
    /// a 5xx. Only safe before any successful response body was consumed: the streaming
    /// path has committed nothing at this point, so the retry cannot duplicate output.
    async fn post_with_fallback(
        &self,
        candidate: &Candidate,
        prepared: &Prepared,
        wire: &Value,
    ) -> Result<reqwest::Response> {
        let mut transport: Option<Error> = None;
        let mut last_server_error: Option<(u16, Bytes)> = None;
        for url in &prepared.urls {
            let sent = Instant::now();
            match self
                .state
                .http
                .post(url)
                .headers(prepared.headers.clone())
                .json(wire)
                .timeout(self.state.request_timeout())
                .send()
                .await
            {
                Ok(resp) => {
                    self.trace.set_upstream_ms(sent.elapsed());
                    let status = resp.status();
                    if status.is_success() {
                        return Ok(resp);
                    }
                    let bytes = resp.bytes().await.map_err(transport_error)?;
                    if status.as_u16() < 500 {
                        return Err(self.upstream_error(candidate, status.as_u16(), &bytes));
                    }
                    last_server_error = Some((status.as_u16(), bytes));
                    transport = None;
                }
                Err(e) => transport = Some(transport_error(e)),
            }
        }
        if let Some((status, bytes)) = last_server_error {
            return Err(self.upstream_error(candidate, status, &bytes));
        }
        Err(transport.unwrap_or_else(|| Error::Transport("no upstream endpoint".into())))
    }

    /// Non-streaming attempt.
    async fn send_once(&self, candidate: Candidate) -> Result<ChatResponse> {
        self.trace
            .time(self.state, candidate, |c, trace| {
                self.send_once_inner(c, trace)
            })
            .await
    }

    async fn send_once_inner(
        &self,
        candidate: Candidate,
        trace: std::sync::Arc<AttemptTrace>,
    ) -> Result<ChatResponse> {
        let prepared = self.prepare(&candidate, &trace).await?;
        let wire = self.outbound(&candidate, prepared.dialect, prepared.project.as_deref())?;
        let resp = self
            .post_with_fallback(&candidate, &prepared, &wire)
            .await?;
        let bytes = resp.bytes().await.map_err(transport_error)?;
        let value: Value = serde_json::from_slice(&bytes)?;
        rrouter_translate::parse_response(prepared.dialect, &value)
    }

    /// Streaming attempt. A non-2xx upstream fails before any byte is committed, so the
    /// dispatcher can still fall back to the next candidate.
    async fn open_stream(&self, candidate: Candidate) -> Result<ByteStream> {
        self.trace
            .time(self.state, candidate, |c, trace| {
                self.open_stream_inner(c, trace)
            })
            .await
    }

    async fn open_stream_inner(
        &self,
        candidate: Candidate,
        trace: std::sync::Arc<AttemptTrace>,
    ) -> Result<ByteStream> {
        let prepared = self.prepare(&candidate, &trace).await?;
        let wire = self.outbound(&candidate, prepared.dialect, prepared.project.as_deref())?;
        let resp = self
            .post_with_fallback(&candidate, &prepared, &wire)
            .await?;
        let dialect = prepared.dialect;
        let content_type = resp
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        // Some OpenAI-compatible providers ignore `stream: true` and answer with one JSON
        // body. Forwarding those bytes to the SSE decoder produces an empty, successful
        // stream; convert the body to chat-completion chunks instead. CCA gets the same
        // treatment by wrapping its JSON as one `data:` frame for its own decoder.
        if content_type.starts_with("application/json")
            && matches!(dialect, Dialect::OpenAiChat | Dialect::GoogleCodeAssist)
        {
            let body: Value = resp.json().await.map_err(transport_error)?;
            if dialect == Dialect::OpenAiChat {
                let frames = json_chat_as_sse(&body, &candidate.model);
                return Ok(Box::pin(OpenStream {
                    bytes: Box::pin(futures_util::stream::once(async move { Ok(frames) })),
                }));
            }
            let frame = format!(
                "data: {}\n\n",
                serde_json::to_string(&body).unwrap_or_else(|_| "{}".into())
            );
            return Ok(Box::pin(OpenStream {
                bytes: Box::pin(futures_util::stream::once(
                    async move { Ok(Bytes::from(frame)) },
                )),
            }));
        }
        Ok(Box::pin(OpenStream {
            bytes: Box::pin(resp.bytes_stream().map(|c| c.map_err(transport_error))),
        }))
    }
}

/// A non-streaming OpenAI chat body as the SSE frames a streaming client expects: one
/// content chunk, one finish chunk, `[DONE]`.
fn json_chat_as_sse(body: &Value, model: &str) -> Bytes {
    let id = body.get("id").and_then(Value::as_str).unwrap_or("");
    let text = body
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .unwrap_or("");
    let mut frames = String::new();
    if !text.is_empty() {
        let chunk = serde_json::json!({
            "id": id,
            "object": "chat.completion.chunk",
            "model": model,
            "choices": [{"index": 0, "delta": {"role": "assistant", "content": text}, "finish_reason": null}]
        });
        frames.push_str(&format!("data: {chunk}\n\n"));
    }
    let mut finish = serde_json::json!({
        "id": id,
        "object": "chat.completion.chunk",
        "model": model,
        "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]
    });
    if let Some(usage) = body.get("usage").filter(|value| value.is_object()) {
        finish["usage"] = usage.clone();
    }
    frames.push_str(&format!("data: {finish}\n\n"));
    frames.push_str("data: [DONE]\n\n");
    Bytes::from(frames)
}

/// Stable canary seed for one request: the conversation's affinity when the caller has
/// one (a chat turn keeps its side of an A/B), otherwise a hash of the request id, which
/// is fixed for the life of the request.
fn canary_seed_for(trace: &RequestTrace, affinity: Option<u64>) -> u64 {
    affinity.unwrap_or_else(|| rrouter_router::canary_seed(trace.request_id.as_bytes()))
}

/// Records an attempt that was cancelled before it produced a verdict.
///
/// Dropped when the send future is dropped — a hedged loser, or a client disconnect
/// during the headers phase. Status `499` means "sent, no verdict"; the number and start
/// time are the ones the attempt was dispatched with, so attempt numbering stays
/// contiguous even though the send never finished.
struct AttemptAbort<'a> {
    trace: &'a RequestTrace,
    state: &'a Arc<AppState>,
    candidate: Candidate,
    /// The attempt's child context, so the aborted row names the span that was forwarded.
    child: crate::telemetry::TraceContext,
    number: u32,
    started: Instant,
    settled: bool,
}

impl Drop for AttemptAbort<'_> {
    fn drop(&mut self) {
        if self.settled {
            return;
        }
        self.trace.attempted(
            self.state,
            &self.candidate,
            &self.child,
            self.number,
            self.started,
            499,
            Some(
                "attempt aborted before a verdict (hedged loser or client disconnect)".to_string(),
            ),
        );
    }
}

/// Per-request bookkeeping: the id the client sees, the attempt counter that numbers
/// the attempt log, and the labels the request-level metrics need.
///
/// One trace is shared by every fallback attempt of one client request, so the rows it
/// writes can be read back in dispatch order under a single id. A final failure also
/// writes one terminal usage row, so the dashboard can show the client-facing reason.
/// One attempt's trace data, as forwarded upstream: the child span plus the inbound
/// `tracestate` when (and only when) the traceparent was accepted. Owned so the send
/// future can hold it across await points.
#[derive(Clone)]
struct AttemptTrace {
    context: crate::telemetry::TraceContext,
    tracestate: Option<String>,
}

/// Inject this attempt's W3C headers into provider headers.
///
/// `insert` semantics on purpose: a provider's static header cannot be duplicated, and an
/// inbound client `traceparent` is never copied — the value is always our child span. The
/// `tracestate` is forwarded only when the inbound traceparent was accepted.
fn inject_trace_headers(headers: &mut axum::http::HeaderMap, trace: &AttemptTrace) {
    headers.remove("traceparent");
    headers.remove("tracestate");
    if let Ok(value) = HeaderValue::from_str(&trace.context.outbound_traceparent()) {
        headers.insert("traceparent", value);
    }
    if let Some(tracestate) = trace.tracestate.as_deref() {
        if !tracestate.is_empty() {
            if let Ok(value) = HeaderValue::from_str(tracestate) {
                headers.insert("tracestate", value);
            }
        }
    }
}

struct RequestTrace {
    request_id: Uuid,
    /// Dialect the client spoke, for the dialect-specific rate-limit header names.
    inbound: Dialect,
    /// W3C trace context: the inbound trace continued, or a fresh one. Shared by every
    /// attempt of this request; each attempt mints a child span.
    trace: crate::telemetry::TraceContext,
    /// Inbound `tracestate`, kept only when the traceparent was accepted. Forwarded
    /// verbatim with every attempt's child; absent when the context was generated.
    tracestate: Option<String>,
    /// The request span: one per client request, the parent of every attempt span. It
    /// exists even without the `otel` feature (an inactive span costs almost nothing),
    /// so enabling export needs no request-path change.
    span: tracing::Span,
    /// Next attempt number; also gives the label of the last one.
    attempts: AtomicU32,
    /// Milliseconds from sending the upstream request to its response headers. Set by
    /// every dispatch attempt and read once the winner is known.
    upstream_ms: AtomicU32,
    /// Budget locks for this request. Taken by admission, moved into the response/record
    /// path so the lock outlives the handler; a request that fails early drops it here.
    budget_hold: Mutex<Option<Arc<crate::state::BudgetHold>>>,
    inner: Mutex<TraceLabels>,
}

#[derive(Default)]
struct TraceLabels {
    /// Model the client asked for, empty until the body is parsed.
    model: String,
    /// Provider of the most recent attempt, empty before any dispatch.
    provider: String,
    /// Gateway key that paid for the request.
    key: Option<Uuid>,
    /// What the key may still spend, once admission succeeded. `None` before admission
    /// (and for anonymous traffic), so an error raised earlier carries no rate headers.
    limits: Option<RateLimits>,
}

impl RequestTrace {
    /// Start the request's span from the client's headers.
    ///
    /// A valid `traceparent` continues that trace with the inbound span as parent; an
    /// absent or invalid one starts a fresh trace (the fallback is counted in
    /// `telemetry`). Never fails: a broken header must not fail a request.
    fn new(request_id: Uuid, inbound: Dialect, headers: Option<&HeaderMap>) -> Self {
        let traceparent = headers
            .and_then(|headers| headers.get("traceparent"))
            .and_then(|value| value.to_str().ok());
        let tracestate = headers
            .and_then(|headers| headers.get("tracestate"))
            .and_then(|value| value.to_str().ok());
        let started = crate::telemetry::start(traceparent, tracestate);
        Self {
            request_id,
            inbound,
            span: crate::telemetry::request_span(request_id, inbound, &started.context),
            trace: started.context,
            tracestate: started.tracestate,
            attempts: AtomicU32::new(0),
            upstream_ms: AtomicU32::new(0),
            budget_hold: Mutex::new(None),
            inner: Mutex::new(TraceLabels::default()),
        }
    }

    /// The 32-hex trace id the client sees as `x-trace-id`.
    fn set_budget_hold(&self, hold: Option<Arc<crate::state::BudgetHold>>) {
        *self.budget_hold.lock().unwrap_or_else(|e| e.into_inner()) = hold;
    }

    fn take_budget_hold(&self) -> Option<Arc<crate::state::BudgetHold>> {
        self.budget_hold
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
    }

    fn trace_id(&self) -> &str {
        &self.trace.trace_id
    }

    fn labels(&self) -> MutexGuard<'_, TraceLabels> {
        // A poisoned lock only means another task panicked; the labels are still whole.
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn set_key(&self, key: Option<Uuid>) {
        self.labels().key = key;
    }

    /// Remember what the key may still spend. Called once, right after admission.
    fn set_rate_limits(&self, limits: RateLimits) {
        self.labels().limits = Some(limits);
    }

    /// Attach the rate-limit headers admission produced.
    ///
    /// Two vocabularies, one source: OpenAI-style `x-ratelimit-*` for every inbound
    /// dialect, and Anthropic's `anthropic-ratelimit-*` as well when the client speaks
    /// Messages (its SDK reads only those). A limit the key does not have is omitted
    /// rather than reported as zero. Budgets use `x-rrouter-budget-remaining-*` because
    /// they are durable spend, not a per-minute window.
    fn apply_rate_headers(&self, headers: &mut HeaderMap) {
        let limits = self.labels().limits;
        let Some(limits) = limits else {
            return;
        };
        let mut set = |name: &'static str, value: String| {
            if let (Ok(name), Ok(value)) = (
                header::HeaderName::from_bytes(name.as_bytes()),
                HeaderValue::from_str(&value),
            ) {
                headers.insert(name, value);
            }
        };
        let reset = |window: WindowUsage| format!("{}s", window.reset_in.as_secs().max(1));
        if let Some(window) = limits.requests {
            set("x-ratelimit-limit-requests", window.limit.to_string());
            set(
                "x-ratelimit-remaining-requests",
                window.remaining.to_string(),
            );
            set("x-ratelimit-reset-requests", reset(window));
            if self.inbound == Dialect::AnthropicMessages {
                set(
                    "anthropic-ratelimit-requests-limit",
                    window.limit.to_string(),
                );
                set(
                    "anthropic-ratelimit-requests-remaining",
                    window.remaining.to_string(),
                );
                set("anthropic-ratelimit-requests-reset", reset(window));
            }
        }
        if let Some(window) = limits.tokens {
            set("x-ratelimit-limit-tokens", window.limit.to_string());
            set("x-ratelimit-remaining-tokens", window.remaining.to_string());
            set("x-ratelimit-reset-tokens", reset(window));
            if self.inbound == Dialect::AnthropicMessages {
                set("anthropic-ratelimit-tokens-limit", window.limit.to_string());
                set(
                    "anthropic-ratelimit-tokens-remaining",
                    window.remaining.to_string(),
                );
                set("anthropic-ratelimit-tokens-reset", reset(window));
            }
        }
        if let Some(remaining) = limits.budget_tokens_remaining {
            set("x-rrouter-budget-remaining-tokens", remaining.to_string());
        }
        if let Some(remaining) = limits.budget_micros_remaining {
            set("x-rrouter-budget-remaining-micros", remaining.to_string());
        }
    }

    fn set_upstream_ms(&self, elapsed: std::time::Duration) {
        // 0 means "not measured"; a sub-millisecond local answer still happened.
        self.upstream_ms.store(
            (elapsed.as_millis().min(u32::MAX as u128) as u32).max(1),
            Ordering::Relaxed,
        );
    }

    fn upstream_ms(&self) -> u32 {
        self.upstream_ms.load(Ordering::Relaxed)
    }

    fn set_model(&self, model: &str) {
        self.labels().model = model.to_string();
    }

    /// Label the request with the provider that would have served it. A cache hit has
    /// no attempt, so this is the only provider signal its counters would otherwise
    /// lose.
    fn set_provider(&self, provider: &str) {
        self.labels().provider = provider.to_string();
    }

    /// Number this attempt (1-based, dispatch order) and start its stopwatch.
    fn next_attempt(&self) -> (u32, Instant) {
        (
            self.attempts.fetch_add(1, Ordering::Relaxed) + 1,
            Instant::now(),
        )
    }

    /// Record one finished attempt: its row, its counter, and the last-tried provider.
    // The identity of the attempt (target, child span, number, start) plus its outcome is
    // deliberately one call: splitting it would let a caller log a row with a different
    // span than the one forwarded.
    #[allow(clippy::too_many_arguments)]
    fn attempted(
        &self,
        state: &Arc<AppState>,
        candidate: &Candidate,
        child: &crate::telemetry::TraceContext,
        number: u32,
        started: Instant,
        status: u16,
        error: Option<String>,
    ) {
        let key = {
            let mut labels = self.labels();
            labels.provider.clone_from(&candidate.provider);
            labels.key
        };
        crate::metrics::attempt_finished(&candidate.provider, status);
        // The ids make one request's fallbacks joinable, and they match the traceparent
        // forwarded to the provider for this attempt.
        tracing::info!(
            trace_id = %self.trace.trace_id,
            span_id = %child.span_id,
            parent_span_id = %child.parent_span_id.as_deref().unwrap_or(""),
            provider = %candidate.provider,
            model = %candidate.model,
            status,
            latency_ms = started.elapsed().as_millis().min(u32::MAX as u128) as u32,
            "attempt"
        );
        // Every successful attempt feeds `least_latency`. Fire and forget: a routing
        // hint must never delay or fail the response.
        if status == 200 {
            spawn_latency(
                state,
                candidate.connection,
                started.elapsed().as_millis().min(u32::MAX as u128) as u32,
            );
        }
        spawn_attempt(
            state,
            AttemptRecord {
                id: None,
                request_id: self.request_id,
                ts: crate::now_unix(),
                attempt: number,
                provider: candidate.provider.clone(),
                model: candidate.model.clone(),
                connection_id: Some(candidate.connection),
                status,
                error,
                latency_ms: started.elapsed().as_millis().min(u32::MAX as u128) as u32,
                key_id: key,
            },
        );
    }

    /// Run one attempt under the trace: number it, time it, log it, hand back its result.
    ///
    /// The candidate is owned so the send can consume it while the row still knows
    /// which provider and model it belonged to.
    async fn time<T, F, Fut>(
        &self,
        state: &Arc<AppState>,
        candidate: Candidate,
        send: F,
    ) -> Result<T>
    where
        F: FnOnce(Candidate, std::sync::Arc<AttemptTrace>) -> Fut,
        Fut: Future<Output = Result<T>>,
    {
        let (number, started) = self.next_attempt();
        let target = candidate.clone();
        // One child context per attempt: the header forwarded upstream and the attempt
        // log below name the same span, and hedged/retried attempts each get a new one.
        let child = self.trace.child(&mut crate::telemetry::RandomIds);
        let attempt_trace = std::sync::Arc::new(AttemptTrace {
            context: child.clone(),
            tracestate: self.tracestate.clone(),
        });
        let logged_child = child;
        let mut abort = AttemptAbort {
            trace: self,
            state,
            candidate: target.clone(),
            child: logged_child.clone(),
            number,
            started,
            settled: false,
        };
        // One span per attempt, parented on the request span. The send runs inside it so
        // the exported duration is the upstream call; the outcome is recorded before the
        // span closes.
        let span =
            crate::telemetry::attempt_span(&self.span, &target.provider, &target.model, number);
        let result = send(candidate, std::sync::Arc::clone(&attempt_trace))
            .instrument(span.clone())
            .await;
        // The send produced a verdict: the guard must not report a second outcome.
        abort.settled = true;
        drop(abort);
        let status = match &result {
            Ok(_) => 200,
            Err(e) => attempt_status(e),
        };
        span.record("status", status);
        if let Err(e) = &result {
            span.record("error", tracing::field::display(e));
        }
        span.in_scope(|| match &result {
            Ok(_) => self.attempted(state, &target, &logged_child, number, started, status, None),
            Err(e) => self.attempted(
                state,
                &target,
                &logged_child,
                number,
                started,
                status,
                Some(e.to_string()),
            ),
        });
        result
    }

    /// Attach the request id to a response and count the request once.
    ///
    /// Every response carries the id — success, stream, or error — because it is the
    /// only handle a client has for `GET /api/usage/{id}`.
    fn respond(&self, started: Instant, mut resp: Response) -> Response {
        self.finish(started, resp.status().as_u16());
        if let Ok(value) = HeaderValue::from_str(&self.request_id.to_string()) {
            resp.headers_mut().insert("x-request-id", value);
        }
        // The trace id joins this response to the access line and to every consumer that
        // continues the trace.
        if let Ok(value) = HeaderValue::from_str(self.trace_id()) {
            resp.headers_mut().insert("x-trace-id", value);
        }
        self.apply_rate_headers(resp.headers_mut());
        resp
    }

    /// Log a failed request once, under its id, then answer with the error response.
    ///
    /// A failure writes one terminal usage row with zero tokens so the dashboard can
    /// show *why* a request never arrived; per-candidate detail lives in `attempts`.
    fn fail(
        &self,
        state: &Arc<AppState>,
        started: Instant,
        inbound: Dialect,
        err: &Error,
    ) -> Response {
        self.fail_at(state, started, inbound, endpoint_of(inbound), err)
    }

    /// [`Self::fail`] for endpoints that are not a canonical dialect endpoint
    /// (`count_tokens`, `compact`, passthroughs).
    fn fail_at(
        &self,
        state: &Arc<AppState>,
        started: Instant,
        inbound: Dialect,
        endpoint: &'static str,
        err: &Error,
    ) -> Response {
        let (provider, model, key) = {
            let labels = self.labels();
            (labels.provider.clone(), labels.model.clone(), labels.key)
        };
        tracing::warn!(
            request_id = %self.request_id,
            provider = %provider,
            model = %model,
            status = err.status(),
            error = %err,
            "request failed"
        );
        let rec = UsageRecord {
            id: None,
            ts: crate::now_unix(),
            request_id: Some(self.request_id),
            trace_id: Some(self.trace_id().to_string()),
            provider,
            model,
            connection_id: None,
            endpoint: Some(endpoint.to_string()),
            key_id: key,
            cost_micros: 0,
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            latency_ms: started.elapsed().as_millis().min(u32::MAX as u128) as u32,
            ttft_ms: 0,
            upstream_ms: self.upstream_ms(),
            status: err.status(),
            streamed: false,
            error: Some(err.to_string()),
        };
        let store = Arc::clone(&state.store);
        let access_log = state.config.load().access_log;
        let budget_hold = self.take_budget_hold();
        tokio::spawn(async move {
            let _budget_hold = budget_hold;
            match store.record_usage(&rec).await {
                Err(e) => tracing::warn!(error = %e, "failure record failed"),
                Ok(()) if access_log => access_line(&rec),
                Ok(()) => {}
            }
        });
        self.respond(started, error_response(inbound, err))
    }

    fn finish(&self, started: Instant, status: u16) {
        let (provider, model) = {
            let labels = self.labels();
            (labels.provider.clone(), labels.model.clone())
        };
        crate::metrics::request_finished(&provider, &model, status, started.elapsed());
    }
}

/// Upstream HTTP status of a failed attempt; `0` when nobody answered.
fn attempt_status(e: &Error) -> u16 {
    match e {
        Error::Upstream { status, .. } => *status,
        _ => 0,
    }
}

/// Fold one successful attempt into the connection's EWMA, off the request path.
fn spawn_latency(state: &AppState, connection: Uuid, latency_ms: u32) {
    let store = Arc::clone(&state.store);
    tokio::spawn(async move {
        if let Err(e) = store
            .record_connection_latency(connection, latency_ms)
            .await
        {
            tracing::warn!(error = %e, "latency sample failed");
        }
    });
}

fn spawn_attempt(state: &AppState, rec: AttemptRecord) {
    let store = Arc::clone(&state.store);
    // The audit trail must never fail a request or delay the response.
    tokio::spawn(async move {
        if let Err(e) = store.record_attempt(&rec).await {
            tracing::warn!(error = %e, "attempt record failed");
        }
    });
}

///
/// Polling is strictly one-chunk-at-a-time, so a client frame is produced as soon as
/// the upstream chunk that carries it arrives. A decode error emits one terminal
/// error frame in the inbound wire shape and ends the stream.
struct Wire {
    incoming: ByteStream,
    decoder: rrouter_translate::StreamDecoder,
    encoder: rrouter_translate::StreamEncoder,
    inbound: Dialect,
    pending: std::collections::VecDeque<StreamEvent>,
    usage: Option<Usage>,
    finished: bool,
    failed: bool,
    /// When the first model output (not the envelope) was decoded: the honest end of
    /// "time to first token".
    first_output: Option<Instant>,
    /// The key's output policy, when the response text must be scanned.
    guardrails: Option<Guardrails>,
    /// The key opted into PII redaction: text deltas are transformed before encoding,
    /// with a bounded carry so matches split across deltas are still caught.
    stream_redactor: Option<crate::redaction::StreamRedactor>,
    /// The message the client was sent when a guardrail ended the stream. `Some` means
    /// nothing else was forwarded and the usage row says `403`.
    blocked: Option<String>,
    /// The terminal error sent after a decode/transport failure. Without this the usage
    /// row would say `200` even though the client received an error frame.
    terminal_error: Option<(u16, String)>,
    /// Longest blocked term, in characters; zero when output scanning is off.
    guard_max: usize,
    /// Trailing response text from the previous delta, per channel, so a term split
    /// across two SSE frames is still matched.
    guard_text_tail: String,
    guard_reasoning_tail: String,
}

impl Wire {
    fn new(
        inbound: Dialect,
        upstream: Dialect,
        client_model: &str,
        incoming: ByteStream,
        guardrails: Option<Guardrails>,
        redact_pii: bool,
    ) -> Self {
        let guard_max = guardrails
            .as_ref()
            .map(|policy| {
                policy
                    .blocked
                    .iter()
                    .map(|term| term.chars().count())
                    .max()
                    .unwrap_or(0)
            })
            .unwrap_or(0);
        Self {
            incoming,
            decoder: rrouter_translate::StreamDecoder::new(upstream, client_model),
            encoder: rrouter_translate::StreamEncoder::new(inbound, client_model),
            inbound,
            pending: std::collections::VecDeque::new(),
            usage: None,
            finished: false,
            failed: false,
            first_output: None,
            guardrails,
            stream_redactor: redact_pii.then(crate::redaction::StreamRedactor::new),
            blocked: None,
            terminal_error: None,
            guard_max,
            guard_text_tail: String::new(),
            guard_reasoning_tail: String::new(),
        }
    }

    /// Judge one text delta against the output policy.
    ///
    /// The previous delta's tail is prepended to the current one, so a term split across
    /// two SSE frames is still matched. Bytes already written cannot be recalled, but the
    /// delta that completes the term is dropped and the stream stops before it is sent.
    /// The tail is per channel: mixing reasoning into answer text would invent matches.
    fn blocked_by(&mut self, event: &StreamEvent) -> Option<String> {
        let policy = self.guardrails.as_ref()?;
        let (text, tail) = match event {
            StreamEvent::TextDelta(text) => (text, &mut self.guard_text_tail),
            StreamEvent::ReasoningDelta(text) => (text, &mut self.guard_reasoning_tail),
            _ => return None,
        };
        let combined = if tail.is_empty() {
            text.clone()
        } else {
            format!("{tail}{text}")
        };
        // Keep only the longest term minus one character: that is the largest prefix
        // that could still become a match when the next delta arrives.
        *tail = suffix_chars(&combined, self.guard_max.saturating_sub(1));
        policy
            .hit(&combined)
            .map(|term| format!("the response contains the blocked term {term:?}"))
    }

    fn drain_events(&mut self, events: Vec<StreamEvent>) -> Option<Bytes> {
        self.pending.extend(events);
        let mut out: Option<Bytes> = None;
        while let Some(ev) = self.pending.pop_front() {
            if self.first_output.is_none()
                && matches!(
                    ev,
                    StreamEvent::TextDelta(_)
                        | StreamEvent::ReasoningDelta(_)
                        | StreamEvent::ToolCallStart { .. }
                        | StreamEvent::ToolCallArgsDelta { .. }
                )
            {
                self.first_output = Some(Instant::now());
            }
            // Redact text before it is encoded. A delta may be entirely held (empty
            // output) until enough following text arrives to prove no match spans the
            // cut; tool-call arguments and other structured payloads are protocol data
            // and are deliberately not rewritten.
            let mut ev = ev;
            if let Some(redactor) = self.stream_redactor.as_mut() {
                match &ev {
                    StreamEvent::TextDelta(text) => {
                        let (ready, report) = redactor.push(text);
                        record_stream_redaction(&report);
                        if ready.is_empty() {
                            continue;
                        }
                        ev = StreamEvent::TextDelta(ready);
                    }
                    StreamEvent::ReasoningDelta(text) => {
                        let (ready, report) = redactor.push(text);
                        record_stream_redaction(&report);
                        if ready.is_empty() {
                            continue;
                        }
                        ev = StreamEvent::ReasoningDelta(ready);
                    }
                    _ => {}
                }
            }
            if let Some(message) = self.blocked_by(&ev) {
                // A guardrail hit discards the carry on purpose: the held tail may be the
                // beginning of the blocked term, and a policy refusal never emits it.
                crate::metrics::guardrail_blocked(Stage::Output);
                self.blocked = Some(message.clone());
                self.guard_text_tail.clear();
                self.guard_reasoning_tail.clear();
                self.stream_redactor = None;
                self.pending.clear();
                self.failed = true;
                let err = Error::Forbidden {
                    code: "output_blocked",
                    message,
                };
                return Some(join(out, Bytes::from(error_frame(self.inbound, &err))));
            }
            if let StreamEvent::Done { usage, .. } = &ev {
                // Flush the redaction carry before the terminal frame so the client gets
                // the redacted tail in the same event order as the provider sent it.
                match self.flush_stream_tail() {
                    Ok(Some(bytes)) => out = Some(join(out, bytes)),
                    Ok(None) => {}
                    Err(e) => {
                        self.failed = true;
                        self.pending.clear();
                        return Some(join(out, Bytes::from(error_frame(self.inbound, &e))));
                    }
                }
                self.usage = Some(usage.unwrap_or_default());
                self.finished = true;
            }
            match self.encoder.encode(&ev) {
                Ok(bytes) if bytes.is_empty() => continue,
                Ok(bytes) => out = Some(join(out, bytes)),
                Err(e) => {
                    self.failed = true;
                    self.stream_redactor = None;
                    self.pending.clear();
                    return Some(join(out, Bytes::from(error_frame(self.inbound, &e))));
                }
            }
        }
        out
    }
    /// Encode whatever the redactor still holds, as a normal text delta. Called before a
    /// terminal `Done`, and before an error frame, so buffered text is neither leaked nor
    /// silently dropped when a stream ends early.
    fn flush_stream_tail(&mut self) -> rrouter_core::Result<Option<Bytes>> {
        self.flush_stream_tail_mode(false)
    }

    /// `interrupted` drops an unterminated PII-looking tail instead of emitting it: after
    /// an error there is no complete match to replace, and a prefix must not leak.
    fn flush_stream_tail_mode(&mut self, interrupted: bool) -> rrouter_core::Result<Option<Bytes>> {
        let Some(redactor) = self.stream_redactor.as_mut() else {
            return Ok(None);
        };
        let (tail, report) = if interrupted {
            redactor.finish_interrupted()
        } else {
            redactor.finish()
        };
        record_stream_redaction(&report);
        if tail.is_empty() {
            return Ok(None);
        }
        let bytes = self.encoder.encode(&StreamEvent::TextDelta(tail))?;
        Ok((!bytes.is_empty()).then_some(bytes))
    }

    /// Flush the redaction tail, then build one terminal error frame.
    fn close_with_error(&mut self, err: Error) -> Bytes {
        self.terminal_error = Some((err.status(), err.to_string()));
        let tail = match self.flush_stream_tail_mode(true) {
            Ok(tail) => tail,
            Err(e) => return Bytes::from(error_frame(self.inbound, &e)),
        };
        let frame = Bytes::from(error_frame(self.inbound, &err));
        tail.map_or(frame.clone(), |bytes| join(Some(bytes), frame))
    }
}

/// Count one streaming redaction and log the class breakdown, never the text.
fn record_stream_redaction(report: &crate::redaction::RedactionReport) {
    if report.redacted {
        crate::metrics::pii_redacted(crate::metrics::RedactStage::Stream, report);
        tracing::debug!(stage = "stream", %report, "pii redacted from stream delta");
    }
}

/// Last `n` characters of `text`, preserving UTF-8 boundaries.
fn suffix_chars(text: &str, n: usize) -> String {
    if n == 0 {
        return String::new();
    }
    let mut chars: Vec<char> = text.chars().rev().take(n).collect();
    chars.reverse();
    chars.into_iter().collect()
}

fn join(acc: Option<Bytes>, next: Bytes) -> Bytes {
    match acc {
        None => next,
        // Rare: one upstream chunk carrying several frames destined for this client.
        Some(acc) => {
            let mut joined = Vec::with_capacity(acc.len() + next.len());
            joined.extend_from_slice(&acc);
            joined.extend_from_slice(&next);
            Bytes::from(joined)
        }
    }
}

impl Stream for Wire {
    type Item = Bytes;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Bytes>> {
        let this = &mut *self;
        loop {
            if this.finished || this.failed {
                return Poll::Ready(None);
            }
            match this.incoming.as_mut().poll_next(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Some(Ok(chunk))) => match this.decoder.push(&chunk) {
                    Ok(events) => {
                        if let Some(out) = this.drain_events(events) {
                            return Poll::Ready(Some(out));
                        }
                    }
                    Err(e) => {
                        this.failed = true;
                        let frame = this.close_with_error(e);
                        return Poll::Ready(Some(frame));
                    }
                },
                Poll::Ready(Some(Err(e))) => {
                    this.failed = true;
                    let frame = this.close_with_error(e);
                    return Poll::Ready(Some(frame));
                }
                Poll::Ready(None) => {
                    this.finished = true;
                    return match this.decoder.finish() {
                        Ok(events) => Poll::Ready(this.drain_events(events)),
                        Err(e) => {
                            this.failed = true;
                            Poll::Ready(Some(this.close_with_error(e)))
                        }
                    };
                }
            }
        }
    }
}

/// Everything the usage row for one in-flight response needs, captured before the
/// body starts streaming.
struct PendingRecord {
    state: Arc<AppState>,
    candidate: Candidate,
    model: String,
    started: Instant,
    /// Time to the winning attempt's response headers.
    upstream_ms: u32,
    /// Gateway key that paid for the request; `None` for anonymous traffic.
    key: Option<Uuid>,
    /// Id already echoed to the client, so the usage row joins the attempt log.
    request_id: Uuid,
    /// W3C trace id, captured while the request is still in scope.
    trace_id: String,
    /// This request's concurrency slot. `ResponseStream` owns it until the body ends or
    /// is dropped, which is the whole release protocol.
    _permit: Option<OwnedSemaphorePermit>,
    /// Budget locks held until this stream's usage row is written.
    budget_hold: Option<Arc<crate::state::BudgetHold>>,
}

/// The `Wire` plus the bookkeeping needed to record one usage row when it ends,
/// whether normally or because the client disconnected.
struct ResponseStream {
    inner: Wire,
    pending: Option<PendingRecord>,
}

impl ResponseStream {
    /// Record the one usage row. Called exactly once: a normal end takes `pending`, and
    /// the `Drop` that follows finds `None`.
    fn commit(&mut self, completed: bool) {
        let Some(pending) = self.pending.take() else {
            return;
        };
        // Counts are known only once the upstream's terminal usage frame decoded. `None`
        // means the client left (or the upstream stopped) before any cost could be
        // attributed; `Some` is what the row must report.
        let usage = self.inner.usage;
        let tokens = usage.unwrap_or_default();
        let ttft = self
            .inner
            .first_output
            .map(|at| at.saturating_duration_since(pending.started));
        // A stream a guardrail stopped is a refusal that happens to have a body: the row
        // says 403 with the reason the client read in the terminal error frame.
        let (status, error) = match self.inner.blocked.as_deref() {
            Some(message) => (403, Some(message)),
            None => match self.inner.terminal_error.as_ref() {
                Some((status, message)) => (*status, Some(message.as_str())),
                None => (200, None),
            },
        };
        record_once(
            &pending.state,
            Recorded {
                candidate: &pending.candidate,
                model: &pending.model,
                endpoint: endpoint_of(self.inner.inbound),
                usage: tokens,
                key: pending.key,
                request_id: pending.request_id,
                trace_id: &pending.trace_id,
                started: pending.started,
                upstream_ms: pending.upstream_ms,
                ttft,
                streamed: true,
                status,
                error,
                budget_hold: pending.budget_hold,
            },
        );
        if !completed {
            tracing::debug!(
                provider = %pending.candidate.provider,
                usage_known = usage.is_some(),
                "client disconnected mid-stream"
            );
        }
    }
}

impl Stream for ResponseStream {
    type Item = Result<Bytes>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let polled = match Pin::new(&mut self.inner).poll_next(cx) {
            Poll::Ready(Some(bytes)) => Poll::Ready(Some(Ok(bytes))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        };
        if matches!(polled, Poll::Ready(None)) {
            self.commit(true);
        }
        polled
    }
}

impl Drop for ResponseStream {
    fn drop(&mut self) {
        // Dropping the body aborts the upstream request: reqwest drops its connection.
        self.commit(false);
    }
}

fn stream_response(
    inbound: Dialect,
    pending: PendingRecord,
    open: StreamRoute,
    bytes: ByteStream,
    guardrails: Option<Guardrails>,
    redact_pii: bool,
) -> Response {
    let wire = Wire::new(
        inbound,
        open.upstream,
        &open.client_model,
        bytes,
        guardrails,
        redact_pii,
    );
    let stream = ResponseStream {
        inner: wire,
        pending: Some(pending),
    };
    let mut resp = Response::new(Body::from_stream(stream));
    let headers = resp.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    headers.insert(header::CONNECTION, HeaderValue::from_static("keep-alive"));
    headers.insert("x-accel-buffering", HeaderValue::from_static("no"));
    resp
}

/// The client endpoint that a dialect is served from. Usage rows record it so the
/// request log can say which protocol a call used without guessing from the model.
fn endpoint_of(dialect: Dialect) -> &'static str {
    match dialect {
        Dialect::AnthropicMessages => "/v1/messages",
        Dialect::OpenAiResponses => "/v1/responses",
        Dialect::OpenAiChat => "/v1/chat/completions",
        Dialect::OpenAiEmbeddings => "/v1/embeddings",
        Dialect::GoogleCodeAssist => "/v1internal:generateContent",
    }
}

/// One finished request, ready to turn into a usage row.
#[derive(Clone)]
struct Recorded<'a> {
    candidate: &'a Candidate,
    model: &'a str,
    endpoint: &'static str,
    usage: Usage,
    /// Gateway key that paid for the request; `None` for anonymous traffic.
    key: Option<Uuid>,
    /// Id already echoed to the client, so the row can be found from its header.
    request_id: Uuid,
    /// W3C trace id of the request, so the access line joins the trace.
    trace_id: &'a str,
    started: Instant,
    /// Time to the winning attempt's upstream response headers.
    upstream_ms: u32,
    /// Time to the first model output; `None` when the client got one JSON body.
    ttft: Option<std::time::Duration>,
    streamed: bool,
    /// `200` for a served request, `403` when a guardrail refused the response.
    status: u16,
    /// What the client was told, when the request failed. Prompt text never goes here.
    error: Option<&'a str>,
    /// Budget locks released after this row is durable.
    budget_hold: Option<Arc<crate::state::BudgetHold>>,
}

fn record_once(state: &AppState, r: Recorded<'_>) {
    let candidate = r.candidate;
    let usage = r.usage;
    // The per-minute token window is fed the same numbers the usage row stores, so
    // `tpm` and `spent_tokens` can never disagree about what a request consumed.
    let spent = u64::from(usage.input_tokens)
        + u64::from(usage.output_tokens)
        + u64::from(usage.cache_read_tokens)
        + u64::from(usage.cache_write_tokens);
    state.record_tokens(r.key, spent);
    let rec = UsageRecord {
        id: None,
        ts: crate::now_unix(),
        request_id: Some(r.request_id),
        trace_id: Some(r.trace_id.to_string()),
        provider: candidate.provider.clone(),
        model: r.model.to_string(),
        connection_id: Some(candidate.connection),
        endpoint: Some(r.endpoint.to_string()),
        key_id: r.key,
        // Priced in `spawn_record`: the registry read happens off the response path.
        cost_micros: 0,
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cache_read_tokens: usage.cache_read_tokens,
        cache_write_tokens: usage.cache_write_tokens,
        latency_ms: r.started.elapsed().as_millis().min(u32::MAX as u128) as u32,
        ttft_ms: r
            .ttft
            .map(|d| d.as_millis().min(u32::MAX as u128) as u32)
            .unwrap_or(0),
        upstream_ms: r.upstream_ms,
        status: r.status,
        streamed: r.streamed,
        error: r.error.map(str::to_owned),
    };
    spawn_record(state, rec, r.budget_hold);
}

/// Output side of a key's guardrails, for the non-streaming paths.
///
/// `text` is what the client would read (see [`response_text`]). On a hit the request is
/// refused and the row says `403` with the reason; the caller returns the error before
/// any response is built, so the offending text is never serialized nor cached.
fn guard_output(
    state: &AppState,
    policy: &Guardrails,
    text: &str,
    recorded: &Recorded<'_>,
) -> Result<()> {
    let Some(term) = policy.hit(text) else {
        return Ok(());
    };
    crate::metrics::guardrail_blocked(Stage::Output);
    let message = format!("the response contains the blocked term {term:?}");
    record_once(
        state,
        Recorded {
            status: 403,
            error: Some(&message),
            ..recorded.clone()
        },
    );
    Err(Error::Forbidden {
        code: "output_blocked",
        message,
    })
}

/// The text a client actually reads from a canonical response: every text and reasoning
/// part. Tool calls are protocol payloads, not prose, and are never scanned.
fn response_text(response: &ChatResponse) -> String {
    let mut segments: Vec<&str> = Vec::new();
    for part in &response.parts {
        match part {
            Part::Text(text) => segments.push(text),
            Part::Reasoning { text, .. } => segments.push(text),
            _ => {}
        }
    }
    segments.join("\n")
}

/// Write one usage row off the response path, priced first.
///
/// The row carries its own `trace_id`, so the access line and the stored row cannot
/// disagree about which trace the request belonged to.
fn spawn_record(
    state: &AppState,
    mut rec: UsageRecord,
    budget_hold: Option<Arc<crate::state::BudgetHold>>,
) {
    let store = Arc::clone(&state.store);
    let access_log = state.config.load().access_log;
    // Usage accounting must never fail a request or delay the response — and the same
    // applies to pricing it. The price is read here rather than on the response path,
    // which is why no memo cache (and no invalidation hook in `routes/models.rs`) is
    // needed: one indexed registry read per completed request, always current.
    tokio::spawn(async move {
        // Keep the budget lock until the usage row is durable; the next request for this
        // key/team then sees the spend it just made.
        let _budget_hold = budget_hold;
        let usage = rrouter_core::Usage {
            input_tokens: rec.input_tokens,
            output_tokens: rec.output_tokens,
            cache_read_tokens: rec.cache_read_tokens,
            cache_write_tokens: rec.cache_write_tokens,
        };
        rec.cost_micros =
            crate::models_registry::cost_micros(&store, &rec.provider, &rec.model, usage).await;
        match store.record_usage(&rec).await {
            Err(e) => tracing::warn!(error = %e, "usage record failed"),
            Ok(()) if access_log => access_line(&rec),
            Ok(()) => {}
        }
    });
}

/// The per-request access line, emitted by the usage writer so it joins the usage row
/// and attempt log on `request_id`. Carries no prompt, response or token material.
fn access_line(rec: &UsageRecord) {
    let request_id = rec.request_id.map(|id| id.to_string()).unwrap_or_default();
    tracing::info!(
        request_id = %request_id,
        trace_id = rec.trace_id.as_deref().unwrap_or(""),
        provider = %rec.provider,
        model = %rec.model,
        status = rec.status,
        latency_ms = rec.latency_ms,
        ttft_ms = rec.ttft_ms,
        key_id = rec.key_id.map(|k| k.to_string()),
        endpoint = rec.endpoint.as_deref().unwrap_or(""),
        "request"
    );
}

/// Embed one text with the operator's configured embedding model.
///
/// This is an *internal* call: it goes through the planner and dispatcher directly, so it
/// can never re-enter the response cache (no recursion), and it records no usage row or
/// attempt row of its own — the semantic cache's embedding spend is operational, not the
/// client's tokens. Failures go back to the caller, which treats them as a plain miss.
async fn embed_text(
    state: &Arc<AppState>,
    team_id: Option<Uuid>,
    model_ref: &str,
    text: &str,
) -> Result<Vec<f32>> {
    let config = state.config.load();
    let plan = state
        .planner
        .plan(
            state.catalog.as_ref(),
            state.store.as_ref(),
            PlanRequest {
                model: model_ref,
                stream: false,
                affinity: None,
                strategy: config.strategy,
                policy: None,
                allowed_models: None,
                team_id,
                canary: None,
                canary_seed: 0,
                combo_start: None,
            },
        )
        .await?;
    // Exactly one send, no retries: the cache is an optimisation, and a retry storm
    // behind a degraded embedding provider would spend money for a request that can
    // simply miss. The breaker is shared, so an unhealthy embedding account is still
    // skipped for the attempts that follow.
    let dispatcher =
        rrouter_router::Dispatcher::with_shared_breaker(state.dispatcher.breaker_arc())
            .with_max_attempts(1)
            .with_hedge(None)
            .with_retry_policy(rrouter_router::dispatch::RetryPolicy {
                attempts: 0,
                ..rrouter_router::dispatch::RetryPolicy::default()
            });
    let state = Arc::clone(state);
    let text = text.to_string();
    let outcome = dispatcher
        .run(&plan, move |candidate| {
            let state = Arc::clone(&state);
            let text = text.clone();
            async move { send_embedding_once(&state, candidate, &text).await }
        })
        .await?;
    outcome.result
}

/// One embeddings POST, parsed down to its vector.
///
/// The provider's own `/embeddings` surface is used (the manifest's chat URL rewritten),
/// exactly as `/v1/embeddings` does; a provider without an embeddings dialect fails
/// cleanly so the caller can fall back to a miss.
async fn send_embedding_once(
    state: &AppState,
    candidate: Candidate,
    text: &str,
) -> Result<Vec<f32>> {
    let spec = rrouter_auth::get(&candidate.provider).ok_or_else(|| Error::NoRoute {
        model: candidate.model.clone(),
        detail: format!("unknown provider {}", candidate.provider),
    })?;
    if !matches!(
        spec.default_dialect,
        Dialect::OpenAiChat | Dialect::OpenAiEmbeddings
    ) {
        return Err(Error::NoRoute {
            model: candidate.model.clone(),
            detail: format!("{} has no embeddings endpoint", candidate.provider),
        });
    }
    let creds = state
        .refresher
        .fresh(state.store.as_ref(), spec, candidate.connection)
        .await?;
    let url = creds
        .base_url_override
        .clone()
        .map(|base| rewrite_embeddings(&base, spec.base_url))
        .unwrap_or_else(|| embeddings_url(spec.base_url));
    let outbound = request_headers(spec, &creds)?;
    let body = serde_json::json!({ "model": candidate.model, "input": text });
    let resp = state
        .http
        .post(&url)
        .headers(outbound)
        .json(&body)
        .timeout(state.request_timeout())
        .send()
        .await
        .map_err(transport_error)?;
    let status = resp.status();
    let bytes = resp.bytes().await.map_err(transport_error)?;
    if !status.is_success() {
        return Err(Error::Upstream {
            provider: candidate.provider.clone(),
            status: status.as_u16(),
            body: String::from_utf8_lossy(&bytes).chars().take(512).collect(),
            payload: None,
        });
    }
    let value: Value = serde_json::from_slice(&bytes)?;
    let vector = value
        .get("data")
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .and_then(|row| row.get("embedding"))
        .and_then(Value::as_array)
        .ok_or_else(|| Error::Upstream {
            provider: candidate.provider.clone(),
            status: status.as_u16(),
            body: "embedding response carried no vector".to_string(),
            payload: None,
        })?;
    let mut out = Vec::with_capacity(vector.len());
    for item in vector {
        let Some(number) = item.as_f64() else {
            return Err(Error::Upstream {
                provider: candidate.provider.clone(),
                status: status.as_u16(),
                body: "embedding vector is not numeric".to_string(),
                payload: None,
            });
        };
        out.push(number as f32);
    }
    if out.is_empty() {
        return Err(Error::Upstream {
            provider: candidate.provider.clone(),
            status: status.as_u16(),
            body: "empty embedding vector".to_string(),
            payload: None,
        });
    }
    Ok(out)
}

async fn embeddings_inner(
    state: &Arc<AppState>,
    headers: &axum::http::HeaderMap,
    raw: &[u8],
    trace: &RequestTrace,
) -> Result<Response> {
    let caller = state.authenticate(headers).await?;
    trace.set_key(caller.key_id());
    let body: Value = serde_json::from_slice(raw)?;
    let requested = body
        .get("model")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::BadRequest("missing model".into()))?
        .to_string();
    let model = state.mapped_model(caller.key_id(), &requested).await?;
    trace.set_model(&model);
    admit_or_explain(state, &caller, &model, trace).await?;
    let policy = provider_policy(&body, headers)?;
    let config = state.config.load();
    let canary_seed = canary_seed_for(trace, None);
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
                policy: policy.as_ref(),
                allowed_models: caller.allowed_models(),
                team_id: caller.team_id(),
                canary: config.canary.as_ref(),
                canary_seed,
                combo_start: None,
            },
        )
        .await?;
    let candidate = plan
        .candidates
        .first()
        .ok_or_else(|| Error::NoRoute {
            model: model.clone(),
            detail: "no candidates".into(),
        })?
        .clone();
    let spec = rrouter_auth::get(&candidate.provider).ok_or_else(|| Error::NoRoute {
        model: model.clone(),
        detail: format!("unknown provider {}", candidate.provider),
    })?;
    if !matches!(
        spec.default_dialect,
        Dialect::OpenAiChat | Dialect::OpenAiEmbeddings
    ) {
        return Ok(not_implemented(
            Dialect::OpenAiEmbeddings,
            "embeddings for this provider",
        ));
    }
    let creds = state
        .refresher
        .fresh(state.store.as_ref(), spec, candidate.connection)
        .await?;
    let url = creds
        .base_url_override
        .clone()
        .map(|base| rewrite_embeddings(&base, spec.base_url))
        .unwrap_or_else(|| embeddings_url(spec.base_url));
    let (number, started) = trace.next_attempt();
    let child = trace.trace.child(&mut crate::telemetry::RandomIds);
    let attempt_trace = AttemptTrace {
        context: child.clone(),
        tracestate: trace.tracestate.clone(),
    };
    let mut outbound = request_headers(spec, &creds)?;
    inject_trace_headers(&mut outbound, &attempt_trace);
    // The upstream body is relayed verbatim, status included, so the attempt is logged
    // around the send itself and its status is the upstream's own.
    let sent = state
        .http
        .post(&url)
        .headers(outbound)
        .body(raw.to_vec())
        .timeout(state.request_timeout())
        .send()
        .await;
    let resp = match sent {
        Ok(resp) => {
            trace.attempted(
                state,
                &candidate,
                &child,
                number,
                started,
                resp.status().as_u16(),
                None,
            );
            resp
        }
        Err(e) => {
            let err = transport_error(e);
            trace.attempted(
                state,
                &candidate,
                &child,
                number,
                started,
                attempt_status(&err),
                Some(err.to_string()),
            );
            return Err(err);
        }
    };
    let status = resp.status();
    let content_type = resp.headers().get(header::CONTENT_TYPE).cloned();
    let bytes = resp.bytes().await.map_err(transport_error)?;
    // One parse of the body, used only for accounting: an embeddings call otherwise
    // leaves no usage row, so it would be invisible in Activity and free against a
    // key's budget. A body that does not parse is still relayed byte for byte.
    if status.is_success() {
        if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
            record_once(
                state,
                Recorded {
                    candidate: &candidate,
                    model: &model,
                    endpoint: "/v1/embeddings",
                    usage: embedding_usage(&value),
                    key: caller.key_id(),
                    request_id: trace.request_id,
                    trace_id: trace.trace_id(),
                    started,
                    upstream_ms: 0,
                    ttft: None,
                    streamed: false,
                    status: 200,
                    error: None,
                    budget_hold: trace.take_budget_hold(),
                },
            );
        }
    }
    let mut out = Response::new(Body::from(bytes));
    *out.status_mut() = status;
    if let Some(ct) = content_type {
        out.headers_mut().insert(header::CONTENT_TYPE, ct);
    }
    Ok(out)
}

/// Tokens an embeddings response reports. Output is always 0: an embedding is one
/// vector, not a completion, so there are no generated tokens to bill.
fn embedding_usage(value: &Value) -> Usage {
    Usage {
        input_tokens: value
            .get("usage")
            .and_then(|u| u.get("prompt_tokens"))
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok())
            .unwrap_or(0),
        ..Usage::default()
    }
}

fn rewrite_embeddings(base: &str, native_chat_url: &str) -> String {
    let rewritten = embeddings_url(native_chat_url);
    rewrite_base(&rewritten, base)
}

/// `/v1/chat/completions` → `/v1/embeddings` for [OI]-shaped base URLs.
fn embeddings_url(base_url: &str) -> String {
    base_url
        .strip_suffix("/chat/completions")
        .map(|prefix| format!("{prefix}/embeddings"))
        .unwrap_or_else(|| format!("{base_url}/embeddings"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A provider whose manifest declares a transport query parameter (`api-version`),
    /// which the built-in registry has no instance of today.
    fn spec_with_api_version() -> ProviderSpec {
        ProviderSpec {
            id: "acme",
            display: "Acme AI",
            kind: rrouter_auth::registry::ProviderKind::ApiKey,
            base_url: "https://api.acme.ai/v1/chat/completions",
            base_url_requires_override: false,
            responses_url: None,
            models_url: None,
            messages_url: None,
            images_url: None,
            audio_speech_url: None,
            audio_transcriptions_url: None,
            audio_voices_url: None,
            default_dialect: Dialect::OpenAiChat,
            auth: rrouter_auth::registry::AuthStyle::Bearer,
            extra_headers: &[],
            auth_header: "",
            query: &[("api-version", "2024-10-21")],
            max_tokens_key: rrouter_translate::MaxTokensKey::MaxTokens,
            passthrough_headers: &[],
            default_max_tokens: None,
            refresh: None,
            profile_url: None,
            login: None,
            stream_only: false,
        }
    }

    /// A terminal stream failure must be visible to the usage row, not recorded as a
    /// successful `200` response.
    #[test]
    fn terminal_stream_error_is_recorded_for_usage() {
        let incoming: ByteStream = Box::pin(futures_util::stream::empty());
        let mut wire = Wire::new(
            Dialect::OpenAiChat,
            Dialect::OpenAiChat,
            "mock-model",
            incoming,
            None,
            false,
        );
        let frame = wire.close_with_error(Error::Translation("bad stream".into()));
        assert!(String::from_utf8_lossy(&frame).contains("bad stream"));
        let (status, message) = wire.terminal_error.as_ref().expect("terminal error");
        assert_eq!(*status, 400);
        assert!(message.contains("bad stream"));
    }

    /// Manifest-declared query parameters are part of the transport contract.
    #[test]
    fn endpoint_appends_a_manifest_declared_query_parameter() {
        let (url, dialect) = endpoint(&spec_with_api_version(), None, Dialect::OpenAiChat);
        assert_eq!(dialect, Dialect::OpenAiChat);
        assert!(
            url.ends_with("chat/completions?api-version=2024-10-21"),
            "{url}"
        );
    }

    /// A tenant that pins a version in its connection URL wins, and the parameter is
    /// never sent twice.
    #[test]
    fn endpoint_keeps_a_query_the_connection_already_sets() {
        let (url, _) = endpoint(
            &spec_with_api_version(),
            Some("http://127.0.0.1:9?api-version=preview"),
            Dialect::OpenAiChat,
        );
        assert!(url.contains("api-version=preview"), "{url}");
        assert_eq!(url.matches("api-version").count(), 1, "{url}");
        assert!(
            url.starts_with("http://127.0.0.1:9/v1/chat/completions"),
            "{url}"
        );
    }

    /// Azure's v1 GA surface declares no `api-version`, so none may be invented.
    #[test]
    fn azure_declares_no_query_parameter() {
        let spec = rrouter_auth::get("azure").expect("azure manifest");
        assert!(spec.query.is_empty(), "the GA surface needs no api-version");
        let (url, _) = endpoint(spec, None, Dialect::OpenAiChat);
        assert!(!url.contains("api-version"), "{url}");
    }

    #[test]
    fn embeddings_url_rewrites_chat_suffix() {
        assert_eq!(
            embeddings_url("https://api.openai.com/v1/chat/completions"),
            "https://api.openai.com/v1/embeddings"
        );
        assert_eq!(
            embeddings_url("http://localhost:1234"),
            "http://localhost:1234/embeddings"
        );
    }

    #[test]
    fn override_moves_origin_but_keeps_native_path() {
        assert_eq!(
            rewrite_base(
                "https://api.openai.com/v1/chat/completions",
                "http://127.0.0.1:9/"
            ),
            "http://127.0.0.1:9/v1/chat/completions"
        );
    }

    /// Captures what the subscriber writes, so the access line is checked as emitted.
    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for Capture {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn sample_row(request_id: Option<Uuid>, key_id: Option<Uuid>) -> UsageRecord {
        UsageRecord {
            id: None,
            ts: 0,
            request_id,
            trace_id: Some("0123456789abcdef0123456789abcdef".into()),
            provider: "openai".into(),
            model: "gpt-4o".into(),
            connection_id: None,
            endpoint: Some("/v1/chat/completions".into()),
            key_id,
            cost_micros: 0,
            input_tokens: 1,
            output_tokens: 2,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            latency_ms: 42,
            ttft_ms: 7,
            upstream_ms: 0,
            status: 200,
            streamed: true,
            error: None,
        }
    }

    /// The access line carries the join keys and the timings, and no token material.
    /// With no key the `key_id` field is absent rather than failing the line.
    #[test]
    fn access_line_has_the_contract_fields_and_survives_no_key() {
        let captured = Capture::default();
        let subscriber = tracing_subscriber::fmt()
            .json()
            .with_writer({
                let captured = captured.clone();
                move || captured.clone()
            })
            .finish();
        let request_id = Uuid::new_v4();
        tracing::subscriber::with_default(subscriber, || {
            access_line(&sample_row(Some(request_id), None));
        });
        let text = String::from_utf8(captured.0.lock().unwrap().clone()).expect("utf8");
        let line: Value =
            serde_json::from_str(text.lines().next().expect("one line")).expect("json line");
        let fields = &line["fields"];
        assert_eq!(line["level"], "INFO");
        assert_eq!(fields["message"], "request");
        assert_eq!(fields["request_id"], request_id.to_string());
        assert_eq!(
            fields["trace_id"], "0123456789abcdef0123456789abcdef",
            "the access line joins the trace"
        );
        assert_eq!(fields["provider"], "openai");
        assert_eq!(fields["model"], "gpt-4o");
        assert_eq!(fields["status"], 200);
        assert_eq!(fields["latency_ms"], 42);
        assert_eq!(fields["ttft_ms"], 7);
        assert_eq!(fields["endpoint"], "/v1/chat/completions");
        assert!(
            fields.get("key_id").is_none(),
            "no key, no key_id: {fields}"
        );
        assert!(!text.contains("input_tokens"), "no token material: {text}");
    }

    /// The gateway interprets `provider` itself, so the upstream body must not carry it:
    /// a strict provider schema would reject the field, and no provider could act on it.
    #[test]
    fn a_missing_traceparent_starts_a_fresh_trace() {
        let trace = RequestTrace::new(Uuid::new_v4(), Dialect::OpenAiChat, None);
        let id = trace.trace_id();
        assert_eq!(id.len(), 32);
        assert!(id
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
        assert!(trace.trace.parent_span_id.is_none());
        assert_eq!(trace.trace.span_id.len(), 16);
    }

    #[test]
    fn an_inbound_traceparent_is_continued_and_echoed() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            "traceparent",
            HeaderValue::from_static("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"),
        );
        let trace = RequestTrace::new(Uuid::new_v4(), Dialect::OpenAiChat, Some(&headers));

        assert_eq!(trace.trace_id(), "4bf92f3577b34da6a3ce929d0e0e4736");
        assert_eq!(
            trace.trace.parent_span_id.as_deref(),
            Some("00f067aa0ba902b7")
        );
        assert_ne!(trace.trace.span_id, "00f067aa0ba902b7", "a new server span");
        assert!(trace.trace.sampled);

        let response = trace.respond(Instant::now(), Response::new(Body::empty()));
        assert_eq!(
            response
                .headers()
                .get("x-trace-id")
                .and_then(|value| value.to_str().ok()),
            Some("4bf92f3577b34da6a3ce929d0e0e4736")
        );
        assert!(response.headers().get("x-request-id").is_some());
    }

    #[test]
    fn an_invalid_traceparent_is_not_continued() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("traceparent", HeaderValue::from_static("00-nope"));
        let trace = RequestTrace::new(Uuid::new_v4(), Dialect::OpenAiChat, Some(&headers));
        assert!(trace.trace.parent_span_id.is_none());
        assert_eq!(trace.trace_id().len(), 32);
    }

    /// The gateway interprets `provider` itself, so the upstream body must not carry it:
    /// a strict provider schema would reject the field, and no provider could act on it.
    #[test]
    fn provider_object_is_stripped_from_the_upstream_body() {
        let raw = serde_json::json!({
            "model": "client/name",
            "provider": {"order": ["openai"], "zdr": true},
            "messages": [{"role": "user", "content": "hi"}]
        });
        let kept = same_dialect_body(Dialect::OpenAiChat, &raw, "gpt-4.1", false, false);
        assert!(kept.get("provider").is_some());
        let stripped = same_dialect_body(Dialect::OpenAiChat, &raw, "gpt-4.1", false, true);
        assert!(stripped.get("provider").is_none());
        // Only the routing field is removed.
        assert_eq!(stripped["model"], "gpt-4.1");
        assert_eq!(stripped["messages"][0]["content"], "hi");
        // The client's body is never mutated in place: attempts clone it.
        assert!(raw.get("provider").is_some());
    }

    /// The passthrough allowlist is what keeps a client's credentials home: only the
    /// names a provider declares are read, and the gateway's own flags are unioned.
    #[test]
    fn outbound_headers_forward_only_declared_client_headers() {
        let spec = rrouter_auth::get("claude").expect("claude");
        let creds = Credentials {
            access_token: Some("gw-token".into()),
            ..Credentials::default()
        };
        let mut client = HeaderMap::new();
        client.insert("anthropic-beta", HeaderValue::from_static("client-flag"));
        client.insert("openai-beta", HeaderValue::from_static("assistants=v2"));
        client.insert("authorization", HeaderValue::from_static("Bearer CLIENT"));

        let headers = outbound_headers(spec, &creds, Some(&client)).expect("headers");
        let beta = headers
            .get("anthropic-beta")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        assert!(beta.contains("client-flag"), "beta was {beta:?}");
        assert!(beta.contains("oauth-2025-04-20"), "beta was {beta:?}");
        assert!(
            headers.get("openai-beta").is_none(),
            "claude declares only anthropic-beta"
        );
        assert!(
            headers
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| v.contains("gw-token") && !v.contains("CLIENT")),
            "the client's authorization must never replace the gateway's"
        );
    }

    #[test]
    fn request_policy_reads_headers_and_body() {
        let mut headers = HeaderMap::new();
        headers.insert("x-rrouter-data-policy", HeaderValue::from_static("deny"));
        let body = serde_json::json!({"provider": {"only": ["anthropic"]}});
        let policy = provider_policy(&body, &headers)
            .expect("policy")
            .expect("some");
        assert_eq!(policy.only, ["anthropic"]);
        assert!(matches!(
            policy.data_collection,
            Some(rrouter_router::DataCollection::Deny)
        ));
        assert!(provider_policy(&serde_json::json!({}), &HeaderMap::new())
            .expect("policy")
            .is_none());
    }

    #[test]
    fn malformed_request_policy_is_a_bad_request() {
        let body = serde_json::json!({"provider": {"allow_fallbacks": "sometimes"}});
        let err = provider_policy(&body, &HeaderMap::new()).expect_err("bad value");
        assert_eq!(err.status(), 400);
    }

    #[test]
    fn google_cca_requires_a_project_and_sends_its_own_headers() {
        let mut creds = Credentials {
            access_token: Some("antigravity-token".into()),
            ..Credentials::default()
        };
        let err = cca_project(&creds).expect_err("no project");
        assert_eq!(err.status(), 400);
        assert!(err.to_string().contains("project_id"), "{err}");

        creds
            .extra
            .insert("project_id".into(), Value::from("  proj-1  "));
        assert_eq!(cca_project(&creds).expect("project"), "proj-1");

        let spec = rrouter_auth::get("claude").expect("claude");
        let headers = google_cca_headers(spec, &creds, None).expect("headers");
        assert_eq!(
            headers
                .get(header::USER_AGENT)
                .and_then(|value| value.to_str().ok()),
            Some(rrouter_translate::google_cca::USER_AGENT)
        );
        assert_eq!(
            headers
                .get("x-goog-api-client")
                .and_then(|value| value.to_str().ok()),
            Some("google-cloud-sdk vscode_cloudshelleditor/0.1")
        );
        assert_eq!(
            headers
                .get(header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok()),
            Some("Bearer antigravity-token")
        );
        assert_eq!(
            headers
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("application/json")
        );
    }
}
