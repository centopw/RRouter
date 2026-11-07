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
use std::time::Instant;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use rrouter_auth::{Credentials, ProviderSpec};
use rrouter_core::{ChatRequest, ChatResponse, Dialect, Error, Part, Result, StreamEvent, Usage};
use rrouter_router::{Candidate, Plan};
use rrouter_store::{AttemptRecord, Guardrails, UsageRecord};
use serde_json::Value;
use tokio::sync::OwnedSemaphorePermit;
use uuid::Uuid;

use crate::config::CacheMode;
use crate::error::{error_frame, error_response, not_implemented};
use crate::metrics::Stage;
use crate::overloaded_response;
use crate::state::{check_prompt, dialect_of, prompt_text, AppState, Caller, Slot};

/// Request body limit: a 100k-token request plus base64 images still fits.
const MAX_BODY_BYTES: usize = 64 * 1024 * 1024;
/// Upstream error bodies are truncated to this many bytes before being passed on.
const MAX_ERROR_BODY: usize = 4096;
/// Exact-match cache control: `hit`/`miss` on the response, and the client's `bypass`
/// opt-out on the request.
const CACHE_HEADER: &str = "x-rrouter-cache";

type ByteStream = Pin<Box<dyn Stream<Item = Result<Bytes>> + Send>>;

/// Entry point for `/v1/chat/completions`, `/v1/messages`, `/v1/responses`.
pub async fn proxy(State(state): State<Arc<AppState>>, req: Request) -> Response {
    let inbound = dialect_of(req.uri());
    let trace = RequestTrace::new(Uuid::new_v4());
    let started = Instant::now();
    // The slot is taken before the body is read: an overloaded gateway must not spend
    // memory buffering a request it has already decided to refuse.
    let permit = match state.try_slot() {
        Slot::Granted(permit) => permit,
        Slot::Full => return trace.respond(started, overloaded_response(inbound)),
    };
    let (parts, body) = req.into_parts();
    let raw = match axum::body::to_bytes(body, MAX_BODY_BYTES).await {
        Ok(b) => b,
        Err(e) => {
            return trace.fail(
                started,
                inbound,
                &Error::BadRequest(format!("request body could not be read: {e}")),
            );
        }
    };
    match handle(&state, inbound, &parts.headers, &raw, &trace, permit).await {
        Ok(resp) => trace.respond(started, resp),
        Err(e) => trace.fail(started, inbound, &e),
    }
}

/// `/v1/embeddings`: passthrough for [OI]-compatible providers, `501` otherwise.
pub async fn embeddings(State(state): State<Arc<AppState>>, req: Request) -> Response {
    let trace = RequestTrace::new(Uuid::new_v4());
    let started = Instant::now();
    // Held for this whole scope: a non-streaming response releases it when it is built.
    let _permit = match state.try_slot() {
        Slot::Granted(permit) => permit,
        Slot::Full => return trace.respond(started, overloaded_response(Dialect::OpenAiEmbeddings)),
    };
    let (parts, body) = req.into_parts();
    let raw = match axum::body::to_bytes(body, MAX_BODY_BYTES).await {
        Ok(b) => b,
        Err(e) => {
            return trace.fail(
                started,
                Dialect::OpenAiEmbeddings,
                &Error::BadRequest(format!("request body could not be read: {e}")),
            );
        }
    };
    match embeddings_inner(&state, &parts.headers, &raw, &trace).await {
        // Non-streaming: the permit dies with this scope, after the body is built.
        Ok(resp) => trace.respond(started, resp),
        Err(e) => trace.fail(started, Dialect::OpenAiEmbeddings, &e),
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
    let trace = RequestTrace::new(Uuid::new_v4());
    let started = Instant::now();
    // Held for this whole scope: a non-streaming response releases it when it is built.
    let _permit = match state.try_slot() {
        Slot::Granted(permit) => permit,
        Slot::Full => return trace.respond(started, overloaded_response(kind.dialect())),
    };
    let (parts, body) = req.into_parts();
    let raw = match axum::body::to_bytes(body, MAX_BODY_BYTES).await {
        Ok(b) => b,
        Err(e) => {
            return trace.fail(
                started,
                kind.dialect(),
                &Error::BadRequest(format!("request body could not be read: {e}")),
            );
        }
    };
    match special_inner(state, &parts.headers, &raw, kind, &trace).await {
        // Non-streaming: the permit dies with this scope, after the body is built.
        Ok(resp) => trace.respond(started, resp),
        Err(e) => trace.fail(started, kind.dialect(), &e),
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
    state.admit(&caller, &model).await?;
    let plan = state
        .planner
        .plan(
            state.catalog.as_ref(),
            state.store.as_ref(),
            &model,
            false,
            None,
            state.config.strategy,
        )
        .await?;
    // Fallback only makes sense between providers that have the endpoint; asking the
    // next candidate would answer with a NoRoute that stops the chain anyway.
    let candidates: Vec<Candidate> = plan
        .candidates
        .iter()
        .filter(|c| {
            rrouter_auth::get(&c.provider).is_some_and(|spec| kind.base(spec).is_some())
        })
        .cloned()
        .collect();
    if candidates.is_empty() {
        return Ok(not_implemented(kind.dialect(), kind.unsupported()));
    }
    let plan = Plan { candidates, ..plan };

    let started = Instant::now();
    let outcome = state
        .dispatcher
        .run(&plan, |c| trace.time(state, c, |c| send_special(state, &body, kind, c)))
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
                started,
                ttft: None,
                streamed: false,
                status: 200,
                error: None,
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
    let input = count("input_tokens").or_else(|| count("prompt_tokens")).unwrap_or(0);
    let output = count("output_tokens").or_else(|| count("completion_tokens")).unwrap_or(0);
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
    body: &Value,
    kind: Special,
    candidate: Candidate,
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
    let mut wire = body.clone();
    wire["model"] = Value::String(candidate.model.clone());
    let resp = state
        .http
        .post(&url)
        .headers(request_headers(spec, &creds)?)
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
    let trace = RequestTrace::new(Uuid::new_v4());
    let started = Instant::now();
    let permit = match state.try_slot() {
        Slot::Granted(permit) => permit,
        Slot::Full => return trace.respond(started, overloaded_response(kind.dialect())),
    };
    let (parts, body) = req.into_parts();
    let raw = match axum::body::to_bytes(body, MAX_BODY_BYTES).await {
        Ok(b) => b,
        Err(e) => {
            return trace.fail(
                started,
                kind.dialect(),
                &Error::BadRequest(format!("request body could not be read: {e}")),
            );
        }
    };
    match passthrough_inner(state, &parts.headers, &raw, kind, &trace).await {
        // Audio streams: the slot has to outlive this handler and die with the body.
        Ok(resp) => trace.respond(started, hold_slot(resp, permit)),
        Err(e) => trace.fail(started, kind.dialect(), &e),
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
        Pass::Voices => voices_inner(state, trace, &caller).await,
        Pass::Transcriptions => transcriptions_inner(state, headers, raw, trace, &caller).await,
        Pass::Images | Pass::Speech => {
            let body: Value = serde_json::from_slice(raw)?;
            let Some(plan) = json_plan(state, &body, kind, trace, &caller).await? else {
                return Ok(not_implemented(kind.dialect(), kind.unsupported()));
            };
            let started = Instant::now();
            if kind == Pass::Speech {
                let outcome = state
                    .dispatcher
                    .run(&plan, |c| {
                        trace.time(state, c, |c| send_speech(state, &body, c))
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
                    trace.time(state, c, |c| send_json(state, &body, kind, c))
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
    state.admit(caller, &model).await?;
    let plan = state
        .planner
        .plan(
            state.catalog.as_ref(),
            state.store.as_ref(),
            &model,
            false,
            None,
            state.config.strategy,
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
    Some(Plan {
        candidates,
        ..plan
    })
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
    kind: Pass,
    candidate: &Candidate,
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
    Ok((url, request_headers(spec, &creds)?))
}

/// One JSON passthrough attempt. The model is the provider's own bare id.
async fn send_json(
    state: &Arc<AppState>,
    body: &Value,
    kind: Pass,
    candidate: Candidate,
) -> Result<(StatusCode, Value)> {
    let (url, headers) = prepare_pass(state, kind, &candidate).await?;
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
    body: &Value,
    candidate: Candidate,
) -> Result<Audio> {
    let (url, headers) = prepare_pass(state, Pass::Speech, &candidate).await?;
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
    raw: &[u8],
    content_type: &HeaderValue,
    candidate: Candidate,
) -> Result<(StatusCode, Value)> {
    let (url, mut headers) = prepare_pass(state, Pass::Transcriptions, &candidate).await?;
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

async fn send_voices(state: &Arc<AppState>, candidate: Candidate) -> Result<(StatusCode, Value)> {
    let (url, headers) = prepare_pass(state, Pass::Voices, &candidate).await?;
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
            state.admit(caller, &model).await?;
            state
                .planner
                .plan(
                    state.catalog.as_ref(),
                    state.store.as_ref(),
                    &model,
                    false,
                    None,
                    state.config.strategy,
                )
                .await?
        }
        None => {
            // No model in the form: the endpoint needs an account, not a model. The
            // allowlist is checked against the empty model, so a key restricted to
            // specific models is told to name one rather than bypassing its list.
            let Some(candidate) = hint_candidate(state, Pass::Transcriptions).await? else {
                return Err(Error::BadRequest(
                    "missing model and no connected provider serves /v1/audio/transcriptions"
                        .into(),
                ));
            };
            state.admit(caller, "").await?;
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
            trace.time(state, c, |c| {
                send_multipart(state, raw, content_type, c)
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
    trace: &RequestTrace,
    caller: &Caller,
) -> Result<Response> {
    let Some(candidate) = hint_candidate(state, Pass::Voices).await? else {
        return Ok(not_implemented(
            Pass::Voices.dialect(),
            Pass::Voices.unsupported(),
        ));
    };
    state.admit(caller, "").await?;
    let plan = Plan {
        model: candidate.model.clone(),
        candidates: vec![candidate],
        stream: false,
    };
    let started = Instant::now();
    let outcome = state
        .dispatcher
        .run(&plan, |c| trace.time(state, c, |c| send_voices(state, c)))
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
/// model in its request has to borrow.
async fn hint_candidate(state: &Arc<AppState>, kind: Pass) -> Result<Option<Candidate>> {
    let connections = state.store.list_connections(None).await?;
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
            started,
            ttft: None,
            streamed: false,
            status: 200,
            error: None,
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

    let mut body: Value = serde_json::from_slice(raw)?;
    let mut canonical = rrouter_translate::parse_request(inbound, &body)?;
    // This key's model mapping is one hop that runs before alias/combo resolution.
    let model = state.mapped_model(caller.key_id(), &canonical.model).await?;
    trace.set_model(&model);
    // Guardrails run on the canonical text after authentication and before any
    // governance bookkeeping or dispatch: a refused prompt costs no provider call, no
    // rate-limit slot and no cache entry.
    check_prompt(&caller, &prompt_text(&canonical))?;
    // Governance runs before planning: an inactive/expired key, a model outside the
    // allowlist, or an exhausted quota must not reach a provider.
    state.admit(&caller, &model).await?;
    // Token Saver runs on the wire body after guardrails: same-dialect forwarding sends
    // this body unchanged, and the canonical parse is refreshed from the compressed text.
    if rtk_enabled(headers) {
        if let Some(stats) = rrouter_rtk::compress_source(&mut body, inbound) {
            record_rtk(&stats);
            canonical = rrouter_translate::parse_request(inbound, &body)?;
        }
    }
    let key_id = caller.key_id();
    let plan = state
        .planner
        .plan(
            state.catalog.as_ref(),
            state.store.as_ref(),
            &model,
            canonical.stream,
            Some(affinity_of(&canonical)),
            state.config.strategy,
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
    };

    // Cache eligibility is decided once, before anything is sent: a non-streaming
    // client (a stream has no body to store), an enabled backend, no explicit bypass,
    // and a candidate to key the exact upstream body against.
    let cache = if state.config.cache == CacheMode::Sqlite
        && !client_stream
        && !cache_bypassed(headers)
        && !plan.candidates.is_empty()
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

    if let Some(key) = &cache {
        match state.store.cache_get(&key.hash, crate::now_unix()).await {
            Ok(Some(body)) => {
                // A stored body is client-facing JSON: the text view for this path is
                // the serialized response, so a response cached before this key's policy
                // existed (or by another key) is still refused here rather than served.
                if let Some(policy) = caller.output_guardrails() {
                    guard_output(
                        state,
                        policy,
                        &body,
                        Recorded {
                            candidate: &plan.candidates[0],
                            model: &plan.model,
                            endpoint: endpoint_of(inbound),
                            usage: Usage::default(),
                            key: key_id,
                            request_id: trace.request_id,
                            started,
                            ttft: None,
                            streamed: false,
                            status: 200,
                            error: None,
                        },
                    )?;
                }
                crate::metrics::cache_hit();
                trace.set_provider(&plan.candidates[0].provider);
                record_cache_hit(
                    state,
                    &plan.candidates[0],
                    &plan.model,
                    inbound,
                    key_id,
                    trace,
                    started,
                );
                return Ok(cached_response(body));
            }
            Ok(None) => crate::metrics::cache_miss(),
            // A cache is an optimisation: a store failure must never fail the request.
            Err(e) => {
                tracing::warn!(error = %e, "response cache read failed");
                crate::metrics::cache_miss();
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
            let assembled = reassemble(&open, bytes).await?;
            let recorded = Recorded {
                candidate: &candidate,
                model: &plan.model,
                endpoint: endpoint_of(inbound),
                usage: assembled.usage,
                key: key_id,
                request_id: trace.request_id,
                started,
                ttft: None,
                streamed: false,
                status: 200,
                error: None,
            };
            // Scan before the row is written, so a refusal is logged as one 403 row
            // instead of a 200 that the client never received.
            if let Some(policy) = caller.output_guardrails() {
                guard_output(state, policy, &response_text(&assembled), recorded)?;
            }
            record_once(state, recorded);
            (
                candidate,
                rrouter_translate::build_response(inbound, &assembled)?,
            )
        } else {
            let (candidate, resp) = complete_traced(
                state,
                inbound,
                &body,
                key_id,
                caller.output_guardrails(),
                trace,
            )
            .await?;
            (candidate, rrouter_translate::build_response(inbound, &resp)?)
        };

        // Serialize once: the bytes stored are exactly the bytes the client receives,
        // so a later hit is byte-identical to this response.
        let text = out.to_string();
        if let Some(key) = &cache {
            // Only what this key describes may be stored: a fallback to another model,
            // or a provider that speaks another dialect, is a different request.
            if served.model == key.model && attempt.route_dialect(&served) == key.dialect {
                let now = crate::now_unix();
                let expires_at = now.saturating_add(state.config.cache_ttl_secs as i64);
                if let Err(e) = state
                    .store
                    .cache_put(
                        &key.hash,
                        &text,
                        now,
                        expires_at,
                        state.config.cache_max_entries,
                    )
                    .await
                {
                    tracing::warn!(error = %e, "response cache write failed");
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
            key: key_id,
            request_id: trace.request_id,
            // The slot belongs to the stream, not to this handler: it is released when
            // the body ends or the client disconnects, whichever comes first.
            _permit: permit,
        },
        open,
        bytes,
        caller.output_guardrails().cloned(),
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
    let trace = RequestTrace::new(Uuid::new_v4());
    complete_traced(state, inbound, body, key, None, &trace)
        .await
        .map(|(_, resp)| resp)
}

/// Both the candidate that answered and its canonical response. The candidate is what
/// lets the caller decide whether the answer may be cached under the key it planned.
///
/// `guardrails` is the paying key's output policy: scanning happens here, before the
/// usage row is written, so a refusal is one `403` row and never a `200` that the
/// client did not receive.
async fn complete_traced(
    state: &Arc<AppState>,
    inbound: Dialect,
    body: &Value,
    key: Option<Uuid>,
    guardrails: Option<&Guardrails>,
    trace: &RequestTrace,
) -> Result<(Candidate, rrouter_core::ChatResponse)> {
    trace.set_key(key);
    let started = Instant::now();
    let canonical = rrouter_translate::parse_request(inbound, body)?;
    let model = state.mapped_model(key, &canonical.model).await?;
    trace.set_model(&model);
    let plan = state
        .planner
        .plan(
            state.catalog.as_ref(),
            state.store.as_ref(),
            &model,
            canonical.stream,
            Some(affinity_of(&canonical)),
            state.config.strategy,
        )
        .await?;
    let upstream_stream = plan.stream || canonical.stream || plan.candidates.iter().any(stream_only);
    let attempt = Attempt {
        state,
        inbound,
        canonical,
        raw: body,
        upstream_stream,
        trace,
    };

    if !upstream_stream {
        let outcome = state.dispatcher.run(&plan, |c| attempt.send_once(c)).await?;
        let candidate = outcome.candidate.clone();
        let resp = outcome.result?;
        let recorded = Recorded {
            candidate: &candidate,
            model: &plan.model,
            endpoint: endpoint_of(inbound),
            usage: resp.usage,
            key,
            request_id: trace.request_id,
            started,
            ttft: None,
            streamed: false,
            status: 200,
            error: None,
        };
        if let Some(policy) = guardrails {
            guard_output(state, policy, &response_text(&resp), recorded)?;
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
        started,
        ttft: None,
        streamed: false,
        status: 200,
        error: None,
    };
    if let Some(policy) = guardrails {
        guard_output(state, policy, &response_text(&assembled), recorded)?;
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
                StreamEvent::Start { id: sid, model: smodel } => {
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
                StreamEvent::ToolCallArgsDelta { index, partial_json } => {
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
            events, &mut text, &mut reasoning, &mut tools, &mut usage, &mut finish, &mut id,
            &mut model,
        );
    }
    let events = decoder.finish()?;
    absorb(
        events, &mut text, &mut reasoning, &mut tools, &mut usage, &mut finish, &mut id, &mut model,
    );

    let mut parts: Vec<rrouter_core::Part> = Vec::with_capacity(tools.len() + 2);
    if !reasoning.is_empty() {
        parts.push(rrouter_core::Part::Reasoning { text: reasoning, signature: None });
    }
    if !text.is_empty() {
        parts.push(rrouter_core::Part::Text(text));
    }
    for (_, tid, name, args) in tools {
        parts.push(rrouter_core::Part::ToolUse {
            id: tid,
            name,
            input: serde_json::from_str(&args).unwrap_or(serde_json::Value::Null),
        });
    }
    Ok(rrouter_core::ChatResponse { id, model, parts, finish_reason: finish, usage })
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
fn same_dialect_body(dialect: Dialect, raw: &Value, model: &str, stream: bool) -> Value {
    let mut out = raw.clone();
    let Some(obj) = out.as_object_mut() else {
        return out;
    };
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
    }
}

/// Outbound URL and the dialect that URL speaks.
///
/// A connection's `base_url_override` always wins: it is the deployment's statement about
/// where this account's traffic goes (self-hosted gateway, regional endpoint, mock). The
/// native endpoint's path is grafted onto the override's origin, so a provider that speaks
/// the inbound dialect keeps being talked to in that dialect.
fn endpoint(spec: &ProviderSpec, override_url: Option<&str>, inbound: Dialect) -> (String, Dialect) {
    let (native, dialect) = match native_url(spec, inbound) {
        Some(url) => (url, inbound),
        None => (spec.base_url, spec.default_dialect),
    };
    let url = match override_url {
        Some(base) => rewrite_base(native, base),
        None => native.to_string(),
    };
    (url, dialect)
}

/// Keep the native endpoint's path but move it to the override's origin, so tests and
/// self-hosted gateways can redirect a provider without lying about its dialect.
fn rewrite_base(native: &str, override_base: &str) -> String {
    let (Ok(native_url), Ok(over)) =
        (reqwest::Url::parse(native), reqwest::Url::parse(override_base))
    else {
        return override_base.to_string();
    };
    let mut out = over;
    out.set_path(native_url.path());
    out.to_string()
}

/// A body the provider can take unmodified because it already speaks that dialect.
fn request_headers(
    spec: &ProviderSpec,
    creds: &Credentials,
) -> Result<axum::http::HeaderMap> {
    let mut headers = axum::http::HeaderMap::new();
    for (name, value) in rrouter_auth::build_headers(spec, creds)? {
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
struct CacheKey {
    hash: String,
    model: String,
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
    resp.headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
    resp
}

fn insert_cache_header(resp: &mut Response, state: &'static str) {
    resp.headers_mut()
        .insert(CACHE_HEADER, HeaderValue::from_static(state));
}

/// A stored response: same JSON body the original client received, plus the header
/// that tells this client it came from the cache.
fn cached_response(body: String) -> Response {
    let mut resp = json_text_response(body);
    insert_cache_header(&mut resp, "hit");
    resp
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
) {
    spawn_record(
        state,
        UsageRecord {
            id: None,
            ts: crate::now_unix(),
            request_id: Some(trace.request_id),
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
            status: 200,
            streamed: false,
            error: None,
        },
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
}

impl Attempt<'_> {
    fn provider(&self, candidate: &Candidate) -> Result<&'static ProviderSpec> {
        rrouter_auth::get(&candidate.provider).ok_or_else(|| Error::NoRoute {
            model: candidate.model.clone(),
            detail: format!("unknown provider {}", candidate.provider),
        })
    }

    /// Dialect the upstream speaks for this candidate and inbound path.
    fn route_dialect(&self, candidate: &Candidate) -> Dialect {
        match rrouter_auth::get(&candidate.provider) {
            Some(spec) => endpoint(spec, None, self.inbound).1,
            None => candidate.dialect,
        }
    }

    /// Build the upstream body for one attempt. Never mutates the canonical request, so
    /// concurrent attempts all see the client's original values.
    fn outbound(&self, candidate: &Candidate, dialect: Dialect) -> Result<Value> {
        if dialect == self.inbound {
            return Ok(same_dialect_body(
                dialect,
                self.raw,
                &candidate.model,
                self.upstream_stream,
            ));
        }
        let mut request = self.canonical.clone();
        request.model = candidate.model.clone();
        // Unknown inbound fields travel with the same-dialect hint only, and the
        // canonical request already carries them with that hint. Replacing `extra`
        // with the raw client body re-inserted fields the translator had rewritten:
        // a string `input` overwrote the translated list ("Input must be a list").
        rrouter_translate::build_request(dialect, &request, self.upstream_stream)
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
        let body = self.outbound(candidate, dialect)?;
        let mut canonical = String::with_capacity(1024);
        canonical.push_str(self.inbound.as_str());
        canonical.push('\n');
        canonical.push_str(dialect.as_str());
        canonical.push('\n');
        write_canonical(&body, &mut canonical);
        Ok(CacheKey {
            hash: rrouter_store::cache_key(&canonical),
            model: candidate.model.clone(),
            dialect,
        })
    }

    async fn prepare(
        &self,
        candidate: &Candidate,
    ) -> Result<(String, Dialect, axum::http::HeaderMap)> {
        let spec = self.provider(candidate)?;
        let creds = self
            .state
            .refresher
            .fresh(self.state.store.as_ref(), spec, candidate.connection)
            .await?;
        let (url, dialect) = endpoint(spec, creds.base_url_override.as_deref(), self.inbound);
        Ok((url, dialect, request_headers(spec, &creds)?))
    }

    /// Non-streaming attempt.
    async fn send_once(&self, candidate: Candidate) -> Result<ChatResponse> {
        self.trace
            .time(self.state, candidate, |c| self.send_once_inner(c))
            .await
    }

    async fn send_once_inner(&self, candidate: Candidate) -> Result<ChatResponse> {
        let (url, dialect, headers) = self.prepare(&candidate).await?;
        let wire = self.outbound(&candidate, dialect)?;
        let resp = self
            .state
            .http
            .post(&url)
            .headers(headers)
            .json(&wire)
            .timeout(self.state.request_timeout())
            .send()
            .await
            .map_err(transport_error)?;
        let status = resp.status();
        if !status.is_success() {
            let bytes = resp.bytes().await.map_err(transport_error)?;
            return Err(upstream_error(&candidate, status.as_u16(), &bytes));
        }
        let bytes = resp.bytes().await.map_err(transport_error)?;
        let value: Value = serde_json::from_slice(&bytes)?;
        rrouter_translate::parse_response(dialect, &value)
    }

    /// Streaming attempt. A non-2xx upstream fails before any byte is committed, so the
    /// dispatcher can still fall back to the next candidate.
    async fn open_stream(&self, candidate: Candidate) -> Result<ByteStream> {
        self.trace
            .time(self.state, candidate, |c| self.open_stream_inner(c))
            .await
    }

    async fn open_stream_inner(&self, candidate: Candidate) -> Result<ByteStream> {
        let (url, dialect, headers) = self.prepare(&candidate).await?;
        let wire = self.outbound(&candidate, dialect)?;
        let resp = self
            .state
            .http
            .post(&url)
            .headers(headers)
            .json(&wire)
            .timeout(self.state.request_timeout())
            .send()
            .await
            .map_err(transport_error)?;
        let status = resp.status();
        if !status.is_success() {
            let bytes = resp.bytes().await.map_err(transport_error)?;
            return Err(upstream_error(&candidate, status.as_u16(), &bytes));
        }
        Ok(Box::pin(OpenStream {
            bytes: Box::pin(resp.bytes_stream().map(|c| c.map_err(transport_error))),
        }))
    }
}

/// Per-request bookkeeping: the id the client sees, the attempt counter that numbers
/// the attempt log, and the labels the request-level metrics need.
///
/// One trace is shared by every fallback attempt of one client request, so the rows it
/// writes can be read back in dispatch order under a single id — and, when every
/// candidate fails, the attempts are the only record that the request existed.
struct RequestTrace {
    request_id: Uuid,
    /// Next attempt number; also gives the label of the last one.
    attempts: AtomicU32,
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
}

impl RequestTrace {
    fn new(request_id: Uuid) -> Self {
        Self {
            request_id,
            attempts: AtomicU32::new(0),
            inner: Mutex::new(TraceLabels::default()),
        }
    }

    fn labels(&self) -> MutexGuard<'_, TraceLabels> {
        // A poisoned lock only means another task panicked; the labels are still whole.
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn set_key(&self, key: Option<Uuid>) {
        self.labels().key = key;
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
        (self.attempts.fetch_add(1, Ordering::Relaxed) + 1, Instant::now())
    }

    /// Record one finished attempt: its row, its counter, and the last-tried provider.
    fn attempted(
        &self,
        state: &Arc<AppState>,
        candidate: &Candidate,
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
    async fn time<T, F, Fut>(&self, state: &Arc<AppState>, candidate: Candidate, send: F) -> Result<T>
    where
        F: FnOnce(Candidate) -> Fut,
        Fut: Future<Output = Result<T>>,
    {
        let (number, started) = self.next_attempt();
        let target = candidate.clone();
        let result = send(candidate).await;
        match &result {
            Ok(_) => self.attempted(state, &target, number, started, 200, None),
            Err(e) => self.attempted(
                state,
                &target,
                number,
                started,
                attempt_status(e),
                Some(e.to_string()),
            ),
        }
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
        resp
    }

    /// Log a failed request once, under its id, then answer with the error response.
    ///
    /// Failed requests write no usage row, so this warn is their only log line. Attempts
    /// are not logged per attempt; the attempts table and metrics carry those.
    fn fail(&self, started: Instant, inbound: Dialect, err: &Error) -> Response {
        let (provider, model) = {
            let labels = self.labels();
            (labels.provider.clone(), labels.model.clone())
        };
        tracing::warn!(
            request_id = %self.request_id,
            provider = %provider,
            model = %model,
            status = err.status(),
            error = %err,
            "request failed"
        );
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
        if let Err(e) = store.record_connection_latency(connection, latency_ms).await {
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
    /// The message the client was sent when a guardrail ended the stream. `Some` means
    /// nothing else was forwarded and the usage row says `403`.
    blocked: Option<String>,
}

impl Wire {
    fn new(
        inbound: Dialect,
        upstream: Dialect,
        client_model: &str,
        incoming: ByteStream,
        guardrails: Option<Guardrails>,
    ) -> Self {
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
            blocked: None,
        }
    }

    /// Judge one text delta against the output policy.
    ///
    /// Matching is per delta, because a delta is all that exists at that instant: bytes
    /// already written cannot be recalled, and buffering the whole response to catch a
    /// term split across two frames would defeat streaming. On a hit the offending delta
    /// is dropped whole — the conservative direction — and the caller emits one terminal
    /// error frame and stops.
    fn blocked_by(&self, event: &StreamEvent) -> Option<String> {
        let policy = self.guardrails.as_ref()?;
        let text = match event {
            StreamEvent::TextDelta(text) | StreamEvent::ReasoningDelta(text) => text,
            _ => return None,
        };
        policy
            .hit(text)
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
            if let Some(message) = self.blocked_by(&ev) {
                // Everything still queued belongs to the response after the hit, so it is
                // dropped instead of encoded. `failed` ends the stream after this frame.
                crate::metrics::guardrail_blocked(Stage::Output);
                self.blocked = Some(message.clone());
                self.pending.clear();
                self.failed = true;
                let err = Error::Forbidden {
                    code: "output_blocked",
                    message,
                };
                return Some(join(out, Bytes::from(error_frame(self.inbound, &err))));
            }
            if let StreamEvent::Done { usage, .. } = &ev {
                self.usage = Some(usage.unwrap_or_default());
                self.finished = true;
            }
            match self.encoder.encode(&ev) {
                Ok(bytes) if bytes.is_empty() => continue,
                Ok(bytes) => out = Some(join(out, bytes)),
                Err(e) => {
                    self.failed = true;
                    self.pending.clear();
                    return Some(join(out, Bytes::from(error_frame(self.inbound, &e))));
                }
            }
        }
        out
    }
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
                        return Poll::Ready(Some(Bytes::from(error_frame(this.inbound, &e))));
                    }
                },
                Poll::Ready(Some(Err(e))) => {
                    this.failed = true;
                    return Poll::Ready(Some(Bytes::from(error_frame(this.inbound, &e))));
                }
                Poll::Ready(None) => {
                    this.finished = true;
                    return match this.decoder.finish() {
                        Ok(events) => Poll::Ready(this.drain_events(events)),
                        Err(e) => {
                            Poll::Ready(Some(Bytes::from(error_frame(this.inbound, &e))))
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
    /// Gateway key that paid for the request; `None` for anonymous traffic.
    key: Option<Uuid>,
    /// Id already echoed to the client, so the usage row joins the attempt log.
    request_id: Uuid,
    /// This request's concurrency slot. `ResponseStream` owns it until the body ends or
    /// is dropped, which is the whole release protocol.
    _permit: Option<OwnedSemaphorePermit>,
}

/// The `Wire` plus the bookkeeping needed to record one usage row when it ends,
/// whether normally or because the client disconnected.
struct ResponseStream {
    inner: Wire,
    pending: Option<PendingRecord>,
}

impl ResponseStream {
    fn commit(&mut self, ok: bool) {
        let Some(pending) = self.pending.take() else {
            return;
        };
        let tokens = self.inner.usage.unwrap_or_default();
        let ttft = self
            .inner
            .first_output
            .map(|at| at.saturating_duration_since(pending.started));
        // A stream a guardrail stopped is a refusal that happens to have a body: the row
        // says 403 with the reason the client read in the terminal error frame.
        let (status, error) = match self.inner.blocked.as_deref() {
            Some(message) => (403, Some(message)),
            None => (200, None),
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
                started: pending.started,
                ttft,
                streamed: true,
                status,
                error,
            },
        );
        if !ok {
            tracing::debug!(
                provider = %pending.candidate.provider,
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
) -> Response {
    let wire = Wire::new(inbound, open.upstream, &open.client_model, bytes, guardrails);
    let stream = ResponseStream {
        inner: wire,
        pending: Some(pending),
    };
    let mut resp = Response::new(Body::from_stream(stream));
    let headers = resp.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/event-stream"));
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
    }
}

/// One finished request, ready to turn into a usage row.
///
/// `Copy` so a refusal can record the row it just described (`..recorded`) rather than
/// rebuilding it field by field.
#[derive(Clone, Copy)]
struct Recorded<'a> {
    candidate: &'a Candidate,
    model: &'a str,
    endpoint: &'static str,
    usage: Usage,
    /// Gateway key that paid for the request; `None` for anonymous traffic.
    key: Option<Uuid>,
    /// Id already echoed to the client, so the row can be found from its header.
    request_id: Uuid,
    started: Instant,
    /// Time to the first model output; `None` when the client got one JSON body.
    ttft: Option<std::time::Duration>,
    streamed: bool,
    /// `200` for a served request, `403` when a guardrail refused the response.
    status: u16,
    /// What the client was told, when the request failed. Prompt text never goes here.
    error: Option<&'a str>,
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
        status: r.status,
        streamed: r.streamed,
        error: r.error.map(str::to_owned),
    };
    spawn_record(state, rec);
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
    recorded: Recorded<'_>,
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
            ..recorded
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

fn spawn_record(state: &AppState, mut rec: UsageRecord) {
    let store = Arc::clone(&state.store);
    let access_log = state.config.access_log;
    // Usage accounting must never fail a request or delay the response — and the same
    // applies to pricing it. The price is read here rather than on the response path,
    // which is why no memo cache (and no invalidation hook in `routes/models.rs`) is
    // needed: one indexed registry read per completed request, always current.
    tokio::spawn(async move {
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
    state.admit(&caller, &model).await?;
    let plan = state
        .planner
        .plan(
            state.catalog.as_ref(),
            state.store.as_ref(),
            &model,
            false,
            None,
            state.config.strategy,
        )
        .await?;
    let candidate = plan
        .candidates
        .first()
        .ok_or_else(|| Error::NoRoute { model: model.clone(), detail: "no candidates".into() })?
        .clone();
    let spec = rrouter_auth::get(&candidate.provider).ok_or_else(|| Error::NoRoute {
        model: model.clone(),
        detail: format!("unknown provider {}", candidate.provider),
    })?;
    if !matches!(spec.default_dialect, Dialect::OpenAiChat | Dialect::OpenAiEmbeddings) {
        return Ok(not_implemented(Dialect::OpenAiEmbeddings, "embeddings for this provider"));
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
    // The upstream body is relayed verbatim, status included, so the attempt is logged
    // around the send itself and its status is the upstream's own.
    let (number, started) = trace.next_attempt();
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
            trace.attempted(state, &candidate, number, started, resp.status().as_u16(), None);
            resp
        }
        Err(e) => {
            let err = transport_error(e);
            trace.attempted(
                state,
                &candidate,
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
                    started,
                    ttft: None,
                    streamed: false,
                    status: 200,
                    error: None,
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
            self.0.lock().unwrap_or_else(|e| e.into_inner()).extend_from_slice(buf);
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
        let line: Value = serde_json::from_str(text.lines().next().expect("one line"))
            .expect("json line");
        let fields = &line["fields"];
        assert_eq!(line["level"], "INFO");
        assert_eq!(fields["message"], "request");
        assert_eq!(fields["request_id"], request_id.to_string());
        assert_eq!(fields["provider"], "openai");
        assert_eq!(fields["model"], "gpt-4o");
        assert_eq!(fields["status"], 200);
        assert_eq!(fields["latency_ms"], 42);
        assert_eq!(fields["ttft_ms"], 7);
        assert_eq!(fields["endpoint"], "/v1/chat/completions");
        assert!(fields.get("key_id").is_none(), "no key, no key_id: {fields}");
        assert!(!text.contains("input_tokens"), "no token material: {text}");
    }
}
