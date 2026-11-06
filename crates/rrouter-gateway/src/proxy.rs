//! The proxy hot path: authenticate → parse once → plan → attempt → translate back.
//!
//! Non-streaming bodies are parsed exactly once and re-serialized once per dialect hop.
//! Streaming bodies are never buffered: each upstream chunk is decoded to canonical
//! events and re-encoded for the client, and whatever bytes the encoder produced are
//! yielded immediately (at most one `Bytes` per upstream chunk).

use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Instant;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use rrouter_auth::{Credentials, ProviderSpec};
use rrouter_core::{ChatRequest, ChatResponse, Dialect, Error, Result, StreamEvent, Usage};
use rrouter_router::Candidate;
use rrouter_store::UsageRecord;
use serde_json::Value;

use crate::error::{error_frame, error_response, not_implemented};
use crate::state::{dialect_of, AppState};

/// Request body limit: a 100k-token request plus base64 images still fits.
const MAX_BODY_BYTES: usize = 64 * 1024 * 1024;
/// Upstream error bodies are truncated to this many bytes before being passed on.
const MAX_ERROR_BODY: usize = 4096;

type ByteStream = Pin<Box<dyn Stream<Item = Result<Bytes>> + Send>>;

/// Entry point for `/v1/chat/completions`, `/v1/messages`, `/v1/responses`.
pub async fn proxy(State(state): State<Arc<AppState>>, req: Request) -> Response {
    let inbound = dialect_of(req.uri());
    let (parts, body) = req.into_parts();
    let raw = match axum::body::to_bytes(body, MAX_BODY_BYTES).await {
        Ok(b) => b,
        Err(e) => {
            return error_response(
                inbound,
                &Error::BadRequest(format!("request body could not be read: {e}")),
            )
        }
    };
    match handle(&state, inbound, &parts.headers, &raw).await {
        Ok(resp) => resp,
        Err(e) => error_response(inbound, &e),
    }
}

/// `/v1/embeddings`: passthrough for [OI]-compatible providers, `501` otherwise.
pub async fn embeddings(State(state): State<Arc<AppState>>, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    let raw = match axum::body::to_bytes(body, MAX_BODY_BYTES).await {
        Ok(b) => b,
        Err(e) => {
            return error_response(
                Dialect::OpenAiEmbeddings,
                &Error::BadRequest(format!("request body could not be read: {e}")),
            )
        }
    };
    match embeddings_inner(&state, &parts.headers, &raw).await {
        Ok(resp) => resp,
        Err(e) => error_response(Dialect::OpenAiEmbeddings, &e),
    }
}

/// Endpoints RRouter deliberately does not implement (`/v1/responses/compact`,
/// `/v1/messages/count_tokens`). The path decides the error envelope, because
/// `Dialect::from_path` only maps the four proxied endpoints.
pub async fn unsupported(req: Request) -> Response {
    let path = req.uri().path();
    let inbound = if path.starts_with("/v1/messages") {
        Dialect::AnthropicMessages
    } else if path.starts_with("/v1/responses") {
        Dialect::OpenAiResponses
    } else {
        Dialect::OpenAiChat
    };
    not_implemented(inbound, path)
}

async fn handle(
    state: &Arc<AppState>,
    inbound: Dialect,
    headers: &axum::http::HeaderMap,
    raw: &[u8],
) -> Result<Response> {
    guard_api_key(state, headers).await?;
    let started = Instant::now();

    let body: Value = serde_json::from_slice(raw)?;
    let canonical = rrouter_translate::parse_request(inbound, &body)?;
    let plan = state
        .planner
        .plan(state.catalog.as_ref(), state.store.as_ref(), &canonical.model, canonical.stream)
        .await?;
    let client_stream = plan.stream || canonical.stream;
    // A stream-only upstream (codex) refuses every non-streaming request, so a
    // non-streaming client is served by streaming upstream and reassembling.
    let upstream_stream = client_stream || plan.candidates.iter().any(stream_only);

    let attempt = Attempt {
        state,
        inbound,
        canonical,
        upstream_stream,
    };

    if !client_stream && !upstream_stream {
        let resp = complete(state, inbound, &body).await?;
        return Ok(json_response(StatusCode::OK, rrouter_translate::build_response(inbound, &resp)?));
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

    if !client_stream {
        // Stream-only upstream serving a non-streaming client: consume the whole
        // stream, rebuild the canonical response, then answer in the client dialect.
        let assembled = reassemble(&open, bytes).await?;
        record_once(state, &candidate, &plan.model, assembled.usage, started);
        return Ok(json_response(
            StatusCode::OK,
            rrouter_translate::build_response(inbound, &assembled)?,
        ));
    }
    Ok(stream_response(state, inbound, &candidate, &plan.model, open, bytes, started))
}

/// One non-streaming completion through the full routing path.
///
/// Shared by the proxy and the admin probe so a green "test" button means exactly what
/// a client request would do: same planner, same fallback, same forced streaming for
/// providers that reject non-streaming calls, same reassembly.
pub async fn complete(
    state: &Arc<AppState>,
    inbound: Dialect,
    body: &Value,
) -> Result<rrouter_core::ChatResponse> {
    let started = Instant::now();
    let canonical = rrouter_translate::parse_request(inbound, body)?;
    let plan = state
        .planner
        .plan(state.catalog.as_ref(), state.store.as_ref(), &canonical.model, canonical.stream)
        .await?;
    let upstream_stream = plan.stream || canonical.stream || plan.candidates.iter().any(stream_only);
    let attempt = Attempt {
        state,
        inbound,
        canonical,
        upstream_stream,
    };

    if !upstream_stream {
        let outcome = state.dispatcher.run(&plan, |c| attempt.send_once(c)).await?;
        let candidate = outcome.candidate.clone();
        let resp = outcome.result?;
        record_once(state, &candidate, &plan.model, resp.usage, started);
        return Ok(resp);
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
    record_once(state, &candidate, &plan.model, assembled.usage, started);
    Ok(assembled)
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

async fn guard_api_key(state: &AppState, headers: &axum::http::HeaderMap) -> Result<()> {
    if !state.config.require_api_key {
        return Ok(());
    }
    let Some(key) = crate::auth_header(headers) else {
        return Err(Error::Unauthorized("missing API key".into()));
    };
    match state.store.verify_api_key(&key).await? {
        Some(_) => Ok(()),
        None => Err(Error::Unauthorized("invalid API key".into())),
    }
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
    /// Body the provider can accept unmodified because it speaks the inbound dialect.
    /// Upstream must stream even if the client asked for one JSON response.
    upstream_stream: bool,
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
        // Every request goes through the canonical model, even when the client and the
        // provider speak the same dialect. Forwarding the client body verbatim looked
        // cheaper but skipped protocol normalisation: a string `input` stayed a string
        // and codex rejected it with "Input must be a list", and required fields such as
        // `store:false` were never added.
        let mut request = self.canonical.clone();
        request.model = candidate.model.clone();
        // Unknown inbound fields travel with the same-dialect hint only, and the
        // canonical request already carries them with that hint. Replacing `extra`
        // with the raw client body re-inserted fields the translator had rewritten:
        // a string `input` overwrote the translated list ("Input must be a list").
        rrouter_translate::build_request(dialect, &request, self.upstream_stream)
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

/// Decode + encode in one stage: upstream bytes in, client bytes out.
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
}

impl Wire {
    fn new(inbound: Dialect, upstream: Dialect, client_model: &str, incoming: ByteStream) -> Self {
        Self {
            incoming,
            decoder: rrouter_translate::StreamDecoder::new(upstream, client_model),
            encoder: rrouter_translate::StreamEncoder::new(inbound, client_model),
            inbound,
            pending: std::collections::VecDeque::new(),
            usage: None,
            finished: false,
            failed: false,
        }
    }

    fn drain_events(&mut self, events: Vec<StreamEvent>) -> Option<Bytes> {
        self.pending.extend(events);
        let mut out: Option<Bytes> = None;
        while let Some(ev) = self.pending.pop_front() {
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

/// The `Wire` plus the bookkeeping needed to record one usage row when it ends,
/// whether normally or because the client disconnected.
struct ResponseStream {
    inner: Wire,
    usage: Option<(Arc<AppState>, Candidate, String, Instant)>,
}

impl ResponseStream {
    fn commit(&mut self, ok: bool) {
        let Some((state, candidate, model, started)) = self.usage.take() else {
            return;
        };
        let tokens = self.inner.usage.unwrap_or_default();
        record_once(&state, &candidate, &model, tokens, started);
        if !ok {
            tracing::debug!(provider = %candidate.provider, "client disconnected mid-stream");
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
    state: &Arc<AppState>,
    inbound: Dialect,
    candidate: &Candidate,
    plan_model: &str,
    open: StreamRoute,
    bytes: ByteStream,
    started: Instant,
) -> Response {
    let wire = Wire::new(inbound, open.upstream, &open.client_model, bytes);
    let stream = ResponseStream {
        inner: wire,
        usage: Some((Arc::clone(state), candidate.clone(), plan_model.to_string(), started)),
    };
    let mut resp = Response::new(Body::from_stream(stream));
    let headers = resp.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/event-stream"));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    headers.insert(header::CONNECTION, HeaderValue::from_static("keep-alive"));
    headers.insert("x-accel-buffering", HeaderValue::from_static("no"));
    resp
}

fn record_once(
    state: &AppState,
    candidate: &Candidate,
    model: &str,
    usage: Usage,
    started: Instant,
) {
    let rec = UsageRecord {
        ts: crate::now_unix(),
        provider: candidate.provider.clone(),
        model: model.to_string(),
        connection_id: Some(candidate.connection),
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        latency_ms: started.elapsed().as_millis().min(u32::MAX as u128) as u32,
        status: 200,
        streamed: false,
        error: None,
    };
    spawn_record(state, rec);
}

fn spawn_record(state: &AppState, rec: UsageRecord) {
    let store = Arc::clone(&state.store);
    // Usage accounting must never fail a request or delay the response.
    tokio::spawn(async move {
        if let Err(e) = store.record_usage(&rec).await {
            tracing::warn!(error = %e, "usage record failed");
        }
    });
}

async fn embeddings_inner(
    state: &Arc<AppState>,
    headers: &axum::http::HeaderMap,
    raw: &[u8],
) -> Result<Response> {
    guard_api_key(state, headers).await?;
    let body: Value = serde_json::from_slice(raw)?;
    let model = body
        .get("model")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::BadRequest("missing model".into()))?
        .to_string();
    let plan = state
        .planner
        .plan(state.catalog.as_ref(), state.store.as_ref(), &model, false)
        .await?;
    let candidate = plan
        .candidates
        .first()
        .ok_or_else(|| Error::NoRoute { model: model.clone(), detail: "no candidates".into() })?;
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
    let resp = state
        .http
        .post(&url)
        .headers(request_headers(spec, &creds)?)
        .body(raw.to_vec())
        .timeout(state.request_timeout())
        .send()
        .await
        .map_err(transport_error)?;
    passthrough_response(resp).await
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

/// Relay an upstream JSON response verbatim: status, content type, bytes.
async fn passthrough_response(resp: reqwest::Response) -> Result<Response> {
    let status = resp.status();
    let content_type = resp.headers().get(header::CONTENT_TYPE).cloned();
    let bytes = resp.bytes().await.map_err(transport_error)?;
    let mut out = Response::new(Body::from(bytes));
    *out.status_mut() = status;
    if let Some(ct) = content_type {
        out.headers_mut().insert(header::CONTENT_TYPE, ct);
    }
    Ok(out)
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
}
