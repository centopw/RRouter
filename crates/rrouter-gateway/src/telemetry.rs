//! W3C trace context: parse the inbound headers, mint the server span, emit the next hop.
//!
//! Deliberately pure: no `AppState`, no proxy types, no metrics registry. The request
//! path consumes the ids through [`request_span`]/[`attempt_span`]; export is optional
//! (`otel` feature, [`otel`]), and `docs/TELEMETRY.md` is the operator-facing contract.
//!
//! What is accepted is the version-`00` `traceparent` grammar and an *opaque* bounded
//! `tracestate`. Bad inbound context never fails a request: the gateway starts a new
//! trace, reports [`Started::rejected`], and counts the reason so an operator can see a
//! broken mesh without reading every log line.

use std::sync::atomic::{AtomicU64, Ordering};

use uuid::Uuid;

/// Fixed wire length of a version-`00` `traceparent`: `2+1+32+1+16+1+2`.
pub const TRACEPARENT_LEN: usize = 55;

/// Longest accepted `tracestate`, in bytes (the specification's recommendation).
pub const MAX_TRACESTATE_BYTES: usize = 512;

/// Why inbound context was ignored. Not a client error — the request is served with a
/// fresh trace — but a signal worth counting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectReason {
    /// A version other than `00` (including `ff`, which the specification forbids).
    Version,
    /// The header is not exactly `TRACEPARENT_LEN` bytes with four fields.
    Length,
    /// A field is not lowercase hexadecimal of its exact length.
    Hex,
    /// An all-zero trace id or span id, which the specification forbids.
    ZeroId,
    /// `tracestate` is longer than [`MAX_TRACESTATE_BYTES`].
    TracestateLength,
    /// `tracestate` contains a byte outside printable ASCII.
    TracestateBytes,
}

impl RejectReason {
    /// Every reason, in documentation order, so the metric exposes one series per reason
    /// (including zeros) before the first rejection.
    pub const ALL: [RejectReason; 6] = [
        Self::Version,
        Self::Length,
        Self::Hex,
        Self::ZeroId,
        Self::TracestateLength,
        Self::TracestateBytes,
    ];

    /// Stable label for a metric such as
    /// `rrouter_trace_context_rejected_total{reason="zero_id"}`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Version => "version",
            Self::Length => "length",
            Self::Hex => "hex",
            Self::ZeroId => "zero_id",
            Self::TracestateLength => "tracestate_length",
            Self::TracestateBytes => "tracestate_bytes",
        }
    }
}

/// Slots in [`REJECTIONS`]; one per [`RejectReason`].
const REASONS: usize = 6;

impl RejectReason {
    const fn index(self) -> usize {
        match self {
            Self::Version => 0,
            Self::Length => 1,
            Self::Hex => 2,
            Self::ZeroId => 3,
            Self::TracestateLength => 4,
            Self::TracestateBytes => 5,
        }
    }
}

/// Process-wide rejection counts. Atomics, not a registry: the request path must be able
/// to count a bad header without carrying a metrics handle, and a scrape can read these
/// at any time.
static REJECTIONS: [AtomicU64; REASONS] = [
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
];

/// How many inbound contexts were rejected for `reason` since the process started.
pub fn rejected_count(reason: RejectReason) -> u64 {
    REJECTIONS[reason.index()].load(Ordering::Relaxed)
}

fn count_rejection(reason: RejectReason) {
    REJECTIONS[reason.index()].fetch_add(1, Ordering::Relaxed);
}

/// One accepted inbound `traceparent`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceParent {
    /// 32 lowercase hex characters; never all zero.
    pub trace_id: String,
    /// 16 lowercase hex characters; never all zero.
    pub span_id: String,
    pub sampled: bool,
}

/// The context of *this* server span, plus what the next hop needs.
///
/// `span_id` is always the span the gateway is starting. `parent_span_id` is the inbound
/// span when the trace was continued, `None` when this is a new trace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceContext {
    pub trace_id: String,
    pub span_id: String,
    pub parent_span_id: Option<String>,
    pub sampled: bool,
}

impl TraceContext {
    /// The `traceparent` to send upstream: this server span becomes the next hop's parent.
    pub fn outbound_traceparent(&self) -> String {
        format!(
            "00-{}-{}-{:02x}",
            self.trace_id,
            self.span_id,
            u8::from(self.sampled)
        )
    }

    /// A child span for one upstream attempt: same trace and sampling, new span id,
    /// parent pointing at this request span.
    pub fn child(&self, ids: &mut dyn IdGenerator) -> Self {
        Self {
            trace_id: self.trace_id.clone(),
            span_id: hex(&nonzero(ids.span_id())),
            parent_span_id: Some(self.span_id.clone()),
            sampled: self.sampled,
        }
    }

    /// Whether this span continues an inbound trace.
    pub fn is_continued(&self) -> bool {
        self.parent_span_id.is_some()
    }
}

/// What one inbound request provided, resolved into the server span and forwarding data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Started {
    pub context: TraceContext,
    /// Inbound `tracestate`, validated and bounded. Present only when the traceparent was
    /// accepted: a `tracestate` belonging to a trace we are not continuing is dropped.
    pub tracestate: Option<String>,
    /// Why inbound context was ignored; `None` when the request's context was clean.
    pub rejected: Option<RejectReason>,
}

/// Byte source for trace and span ids. Injected so tests are deterministic and replayable;
/// production uses [`RandomIds`].
pub trait IdGenerator {
    /// 16 bytes for a trace id. Must not be all zero; the module repairs a violation
    /// defensively rather than emitting an invalid id.
    fn trace_id(&mut self) -> [u8; 16];

    /// 8 bytes for a span id. Same non-zero contract.
    fn span_id(&mut self) -> [u8; 8];
}

/// OS-random ids from `uuid` v4 (an existing dependency, so no new crate).
pub struct RandomIds;

impl IdGenerator for RandomIds {
    fn trace_id(&mut self) -> [u8; 16] {
        *Uuid::new_v4().as_bytes()
    }

    fn span_id(&mut self) -> [u8; 8] {
        let bytes = Uuid::new_v4().into_bytes();
        let mut id = [0u8; 8];
        id.copy_from_slice(&bytes[..8]);
        id
    }
}

/// Resolve inbound headers for one request using OS-random ids.
pub fn start(traceparent: Option<&str>, tracestate: Option<&str>) -> Started {
    start_with(traceparent, tracestate, &mut RandomIds)
}

/// Resolve inbound headers with an injected id source.
///
/// Fallback contract: a missing, invalid or unsupported `traceparent` starts a new trace
/// (`parent_span_id: None`, fresh ids, `sampled: true`) and reports the reason; the
/// request is never refused or delayed for it.
pub fn start_with(
    traceparent: Option<&str>,
    tracestate: Option<&str>,
    ids: &mut dyn IdGenerator,
) -> Started {
    let mut rejected = None;

    let inbound = match traceparent {
        None => None,
        Some(raw) => match parse_traceparent(raw) {
            Ok(parent) => Some(parent),
            Err(reason) => {
                count_rejection(reason);
                rejected = Some(reason);
                None
            }
        },
    };

    // A `tracestate` only means something next to a `traceparent` we accepted; a bad one
    // costs the header, not the trace.
    let mut forwarded = None;
    if inbound.is_some() {
        if let Some(raw) = tracestate {
            match parse_tracestate(raw) {
                Ok(state) => forwarded = state,
                Err(reason) => {
                    count_rejection(reason);
                    rejected = rejected.or(Some(reason));
                }
            }
        }
    }

    let span_id = hex(&nonzero(ids.span_id()));
    let (trace_id, parent_span_id, sampled) = match &inbound {
        Some(parent) => (
            parent.trace_id.clone(),
            Some(parent.span_id.clone()),
            parent.sampled,
        ),
        None => (hex(&nonzero(ids.trace_id())), None, true),
    };

    Started {
        context: TraceContext {
            trace_id,
            span_id,
            parent_span_id,
            sampled,
        },
        tracestate: forwarded,
        rejected,
    }
}

/// Parse a version-`00` `traceparent` header.
///
/// Strict on purpose: lowercase hex, exact field widths, non-zero ids, and no other
/// version. Surrounding whitespace is trimmed because HTTP header values may carry it;
/// anything else is a rejection the caller turns into a fresh trace.
pub fn parse_traceparent(raw: &str) -> Result<TraceParent, RejectReason> {
    let raw = raw.trim();
    if raw.is_empty() {
        // An empty header is a framing mistake, not an unsupported version; the operator
        // should look at the peer that sent it, not at their W3C version support.
        return Err(RejectReason::Length);
    }
    let mut parts = raw.split('-');
    let version = parts.next().ok_or(RejectReason::Length)?;
    // Version first: a future version adds fields and would otherwise be reported as a
    // length problem, which sends the operator looking in the wrong place.
    if version != "00" {
        return Err(RejectReason::Version);
    }
    let (Some(trace_id), Some(span_id), Some(flags), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(RejectReason::Length);
    };
    if raw.len() != TRACEPARENT_LEN
        || trace_id.len() != 32
        || span_id.len() != 16
        || flags.len() != 2
    {
        return Err(RejectReason::Length);
    }
    if !is_lower_hex(trace_id) || !is_lower_hex(span_id) || !is_lower_hex(flags) {
        return Err(RejectReason::Hex);
    }
    if is_all_zero(trace_id) || is_all_zero(span_id) {
        return Err(RejectReason::ZeroId);
    }
    let flag_bytes = u8::from_str_radix(flags, 16).map_err(|_| RejectReason::Hex)?;
    Ok(TraceParent {
        trace_id: trace_id.to_string(),
        span_id: span_id.to_string(),
        // Bit 0 is `sampled`; every other bit is reserved and ignored.
        sampled: flag_bytes & 0x01 != 0,
    })
}

/// Validate a `tracestate` header as an opaque, bounded string.
///
/// No key semantics: the specification's list grammar is the client's business, and
/// re-encoding a value this gateway only forwards would be a source of bugs.
pub fn parse_tracestate(raw: &str) -> Result<Option<String>, RejectReason> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(None);
    }
    if raw.len() > MAX_TRACESTATE_BYTES {
        return Err(RejectReason::TracestateLength);
    }
    // Printable ASCII only: a control byte (newline, tab, NUL) in a forwarded header is
    // either a broken peer or an injection attempt, and neither is forwarded.
    if !raw.bytes().all(|byte| (0x20..=0x7e).contains(&byte)) {
        return Err(RejectReason::TracestateBytes);
    }
    Ok(Some(raw.to_string()))
}

/// Lowercase hex of exactly the expected length.
fn is_lower_hex(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn is_all_zero(value: &str) -> bool {
    value.bytes().all(|byte| byte == b'0')
}

/// Hex-encode an id. `String`'s writer cannot fail, so the result is ignored on purpose.
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// Repair an all-zero id from a misbehaving generator instead of emitting one the
/// specification forbids. [`RandomIds`] cannot produce it; the branch keeps the invariant
/// true for every implementation.
fn nonzero<const N: usize>(id: [u8; N]) -> [u8; N] {
    let mut id = id;
    if id.iter().all(|byte| *byte == 0) {
        id[N - 1] = 1;
    }
    id
}

// ---------------------------------------------------------------------------------
// Span hooks and optional export
// ---------------------------------------------------------------------------------

/// The request span every attempt hangs from. Fields are ids and routing facts only —
/// never prompt or response bodies, credentials, or upstream URLs.
///
/// With the `otel` feature the span's parent is the inbound W3C context (the same trace
/// id the client sees in `x-trace-id`), so an exported trace lines up with the access log.
pub fn request_span(
    request_id: Uuid,
    dialect: rrouter_core::Dialect,
    trace: &TraceContext,
) -> tracing::Span {
    let span = tracing::info_span!(
        "request",
        request_id = %request_id,
        dialect = %dialect,
        trace_id = %trace.trace_id,
    );
    #[cfg(feature = "otel")]
    {
        use tracing_opentelemetry::OpenTelemetrySpanExt;
        if let Some(parent) = otel::remote_parent(trace) {
            let _ = span.set_parent(parent);
        }
    }
    span
}

/// One upstream attempt, a child of [`request_span`]. The caller records `status` and
/// `error` when the attempt settles, then drops the span.
pub fn attempt_span(
    parent: &tracing::Span,
    provider: &str,
    model: &str,
    attempt: u32,
) -> tracing::Span {
    tracing::info_span!(
        parent: parent,
        "attempt",
        provider = %provider,
        model = %model,
        attempt = attempt,
        status = tracing::field::Empty,
        error = tracing::field::Empty,
    )
}

/// OTLP/HTTP export (`otel` feature). Built from [`crate::config::OtelConfig`]; the
/// provider is owned by the caller so shutdown can flush what is buffered.
#[cfg(feature = "otel")]
pub mod otel {
    use super::TraceContext;
    use opentelemetry::trace::TracerProvider as _;
    use opentelemetry::trace::{
        SpanContext, SpanId, TraceContextExt, TraceFlags, TraceId, TraceState,
    };
    use opentelemetry::Context;
    use opentelemetry::KeyValue;
    use opentelemetry_otlp::WithExportConfig;
    use opentelemetry_sdk::trace::{Sampler, SdkTracerProvider, Tracer};
    use tracing_opentelemetry::OpenTelemetryLayer;
    use tracing_subscriber::Registry;

    use crate::config::OtelConfig;

    /// The exporter and its provider. Cheap to keep for the process lifetime; dropping it
    /// without [`Exporter::shutdown`] can lose buffered spans.
    pub struct Exporter {
        provider: SdkTracerProvider,
        tracer: Tracer,
    }

    impl Exporter {
        /// Build the OTLP/HTTP exporter. Must run inside the tokio runtime context: the
        /// batch processor exports from runtime tasks.
        pub fn from_config(config: &OtelConfig) -> Result<Self, String> {
            let exporter = opentelemetry_otlp::SpanExporter::builder()
                .with_http()
                .with_protocol(opentelemetry_otlp::Protocol::HttpBinary)
                .with_endpoint(&config.endpoint)
                .build()
                .map_err(|e| format!("otel exporter: {e}"))?;
            let provider = SdkTracerProvider::builder()
                .with_batch_exporter(exporter)
                .with_sampler(Sampler::ParentBased(Box::new(Sampler::TraceIdRatioBased(
                    config.sample_ratio,
                ))))
                .with_resource(
                    opentelemetry_sdk::Resource::builder_empty()
                        .with_attributes([KeyValue::new(
                            "service.name",
                            config.service_name.clone(),
                        )])
                        .build(),
                )
                .build();
            let tracer = provider.tracer("rrouter");
            Ok(Self { provider, tracer })
        }

        /// The layer `main` installs on the tracing registry.
        pub fn layer(&self) -> OpenTelemetryLayer<Registry, Tracer> {
            tracing_opentelemetry::layer().with_tracer(self.tracer.clone())
        }

        /// Block until the batch processor has exported everything buffered so far.
        pub fn force_flush(&self) -> Result<(), String> {
            self.provider
                .force_flush()
                .map_err(|e| format!("otel flush: {e}"))
        }

        /// Flush and stop; after this no span is exported.
        pub fn shutdown(self) -> Result<(), String> {
            self.provider
                .shutdown()
                .map_err(|e| format!("otel shutdown: {e}"))
        }
    }

    /// The inbound context as an OpenTelemetry parent. Invalid hex (already filtered by
    /// [`super::start`]) yields `None`, which leaves the span a local root.
    pub fn remote_parent(trace: &TraceContext) -> Option<Context> {
        let trace_id = TraceId::from_hex(&trace.trace_id).ok()?;
        let span_id = SpanId::from_hex(&trace.span_id).ok()?;
        let flags = if trace.sampled {
            TraceFlags::SAMPLED
        } else {
            TraceFlags::default()
        };
        let context = SpanContext::new(trace_id, span_id, flags, true, TraceState::default());
        Some(Context::new().with_remote_span_context(context))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";
    const TRACE: &str = "4bf92f3577b34da6a3ce929d0e0e4736";
    const SPAN: &str = "00f067aa0ba902b7";

    /// Deterministic ids: trace `trace`, then span `spans[0]`, `spans[1]`, …
    struct SeqIds {
        trace: u8,
        spans: Vec<u8>,
        taken: usize,
    }

    impl SeqIds {
        fn new(trace: u8, spans: &[u8]) -> Self {
            Self {
                trace,
                spans: spans.to_vec(),
                taken: 0,
            }
        }
    }

    impl IdGenerator for SeqIds {
        fn trace_id(&mut self) -> [u8; 16] {
            [self.trace; 16]
        }

        fn span_id(&mut self) -> [u8; 8] {
            let byte = self.spans.get(self.taken).copied().unwrap_or(0xee);
            self.taken += 1;
            [byte; 8]
        }
    }

    #[test]
    fn a_valid_traceparent_is_accepted() {
        let parent = parse_traceparent(SAMPLE).expect("valid");
        assert_eq!(parent.trace_id, TRACE);
        assert_eq!(parent.span_id, SPAN);
        assert!(parent.sampled);
        // Surrounding whitespace is HTTP noise, not a malformed header.
        assert_eq!(
            parse_traceparent(&format!("  {SAMPLE}  ")).expect("trimmed"),
            parent
        );
    }

    #[test]
    fn the_sampled_flag_is_preserved() {
        let unsampled = format!("00-{TRACE}-{SPAN}-00");
        let parent = parse_traceparent(&unsampled).expect("valid");
        assert!(!parent.sampled);

        let mut ids = SeqIds::new(0x11, &[0x22]);
        let started = start_with(Some(&unsampled), None, &mut ids);
        assert!(!started.context.sampled);
        assert!(started.context.outbound_traceparent().ends_with("-00"));
        assert_eq!(started.rejected, None);
    }

    #[test]
    fn unknown_or_forbidden_versions_are_rejected() {
        // A future version adds fields; report the version, not a length mismatch.
        assert_eq!(
            parse_traceparent(&format!("01-{TRACE}-{SPAN}-01-extra")),
            Err(RejectReason::Version)
        );
        // `ff` is explicitly invalid in the specification.
        assert_eq!(
            parse_traceparent(&format!("ff-{TRACE}-{SPAN}-01")),
            Err(RejectReason::Version)
        );
    }

    #[test]
    fn malformed_lengths_and_hex_are_rejected() {
        assert_eq!(parse_traceparent(""), Err(RejectReason::Length));
        assert_eq!(parse_traceparent("00"), Err(RejectReason::Length));
        assert_eq!(
            parse_traceparent(&format!("00-{TRACE}-{SPAN}")),
            Err(RejectReason::Length),
            "a missing flags field"
        );
        assert_eq!(
            parse_traceparent(&format!("00-{TRACE}-{SPAN}-01-ff")),
            Err(RejectReason::Length),
            "a trailing fifth field"
        );
        assert_eq!(
            parse_traceparent(&format!("00-{}-{SPAN}-01", &TRACE[..30])),
            Err(RejectReason::Length),
            "a short trace id"
        );
        // Upper case and non-hex are invalid: the specification requires lower-case hex.
        assert_eq!(
            parse_traceparent(&format!("00-{}-{SPAN}-01", TRACE.to_uppercase())),
            Err(RejectReason::Hex)
        );
        assert_eq!(
            parse_traceparent(&format!("00-{TRACE}-{SPAN}-zz")),
            Err(RejectReason::Hex)
        );
    }

    #[test]
    fn all_zero_ids_are_rejected() {
        let zero_trace = "0".repeat(32);
        let zero_span = "0".repeat(16);
        assert_eq!(
            parse_traceparent(&format!("00-{zero_trace}-{SPAN}-01")),
            Err(RejectReason::ZeroId)
        );
        assert_eq!(
            parse_traceparent(&format!("00-{TRACE}-{zero_span}-01")),
            Err(RejectReason::ZeroId)
        );
    }

    #[test]
    fn a_generated_context_has_the_wire_shape() {
        let started = start(None, None);
        let context = &started.context;

        assert_eq!(context.trace_id.len(), 32);
        assert_eq!(context.span_id.len(), 16);
        assert!(
            is_lower_hex(&context.trace_id),
            "trace id must be lowercase hex: {}",
            context.trace_id
        );
        assert!(is_lower_hex(&context.span_id));
        assert!(!is_all_zero(&context.trace_id));
        assert!(!is_all_zero(&context.span_id));
        assert_eq!(context.parent_span_id, None, "a new trace has no parent");
        assert!(context.sampled, "new traces are recorded by default");
        assert!(!context.is_continued());
        assert_eq!(started.rejected, None);

        // The header we would send upstream is itself valid and describes this span.
        let outbound = context.outbound_traceparent();
        let parsed = parse_traceparent(&outbound).expect("outbound is well formed");
        assert_eq!(parsed.trace_id, context.trace_id);
        assert_eq!(parsed.span_id, context.span_id);
        assert_eq!(parsed.sampled, context.sampled);
    }

    #[test]
    fn an_inbound_parent_is_preserved_while_the_server_span_is_new() {
        let mut ids = SeqIds::new(0x11, &[0x22]);
        let started = start_with(Some(SAMPLE), None, &mut ids);
        let context = &started.context;

        assert_eq!(context.trace_id, TRACE, "the trace id is continued");
        assert_eq!(
            context.parent_span_id.as_deref(),
            Some(SPAN),
            "the inbound span becomes the parent, not our span"
        );
        assert_eq!(context.span_id, "2222222222222222");
        assert!(context.is_continued());
        assert_eq!(
            context.outbound_traceparent(),
            format!("00-{TRACE}-2222222222222222-01")
        );
    }

    #[test]
    fn a_child_span_shares_the_trace_and_points_at_its_parent() {
        let mut ids = SeqIds::new(0x11, &[0x22, 0x33]);
        let started = start_with(Some(SAMPLE), None, &mut ids);
        let child = started.context.child(&mut ids);

        assert_eq!(child.trace_id, started.context.trace_id);
        assert_eq!(child.parent_span_id.as_deref(), Some("2222222222222222"));
        assert_eq!(child.span_id, "3333333333333333");
        assert_eq!(child.sampled, started.context.sampled);
        assert_eq!(
            child.outbound_traceparent(),
            format!("00-{TRACE}-3333333333333333-01")
        );
    }

    #[test]
    fn tracestate_is_bounded_opaque_and_only_forwarded_with_its_traceparent() {
        let mut ids = SeqIds::new(0x11, &[0x22, 0x33]);
        let started = start_with(
            Some(SAMPLE),
            Some("congo=t61rcWkgMzE,rojo=00f067aa0ba902b7"),
            &mut ids,
        );
        assert_eq!(
            started.tracestate.as_deref(),
            Some("congo=t61rcWkgMzE,rojo=00f067aa0ba902b7"),
            "opaque: forwarded byte for byte"
        );

        // Without an accepted traceparent there is no trace to attach state to.
        let mut ids = SeqIds::new(0x11, &[0x22]);
        let orphan = start_with(None, Some("congo=t61rcWkgMzE"), &mut ids);
        assert_eq!(orphan.tracestate, None);
        assert_eq!(orphan.rejected, None, "a stray tracestate is not an error");
    }

    #[test]
    fn oversized_or_binary_tracestate_is_rejected() {
        let too_long = "a".repeat(MAX_TRACESTATE_BYTES + 1);
        assert_eq!(
            parse_tracestate(&too_long),
            Err(RejectReason::TracestateLength)
        );
        assert_eq!(
            parse_tracestate(&"a".repeat(MAX_TRACESTATE_BYTES)).map(|s| s.is_some()),
            Ok(true),
            "the limit itself is allowed"
        );
        assert_eq!(
            parse_tracestate("congo=abc\ninjected: yes"),
            Err(RejectReason::TracestateBytes)
        );
        assert_eq!(parse_tracestate("   "), Ok(None));

        // A bad tracestate costs the header, not the trace.
        let before = rejected_count(RejectReason::TracestateLength);
        let mut ids = SeqIds::new(0x11, &[0x22]);
        let started = start_with(Some(SAMPLE), Some(&too_long), &mut ids);
        assert_eq!(started.context.trace_id, TRACE);
        assert_eq!(started.tracestate, None);
        assert_eq!(started.rejected, Some(RejectReason::TracestateLength));
        assert!(
            rejected_count(RejectReason::TracestateLength) > before,
            "rejections are counted"
        );
    }

    #[test]
    fn invalid_inbound_context_falls_back_to_a_new_trace_and_is_counted() {
        let before = rejected_count(RejectReason::Version);
        let mut ids = SeqIds::new(0x11, &[0x22]);
        let started = start_with(Some(&format!("01-{TRACE}-{SPAN}-01")), None, &mut ids);

        assert_eq!(started.rejected, Some(RejectReason::Version));
        assert_eq!(
            started.context.parent_span_id, None,
            "no parent to continue"
        );
        assert_eq!(started.context.trace_id, "11111111111111111111111111111111");
        assert_eq!(started.context.span_id, "2222222222222222");
        assert!(started.context.sampled);
        assert!(
            rejected_count(RejectReason::Version) > before,
            "the fallback is observable"
        );
    }

    #[test]
    fn an_empty_header_is_a_rejection_not_a_failure() {
        let before = rejected_count(RejectReason::Length);
        let mut ids = SeqIds::new(0x11, &[0x22]);
        let started = start_with(Some(""), None, &mut ids);
        assert_eq!(started.rejected, Some(RejectReason::Length));
        assert_eq!(started.context.trace_id, "11111111111111111111111111111111");
        assert!(rejected_count(RejectReason::Length) > before);
    }

    #[test]
    fn rejection_reasons_have_stable_metric_labels() {
        // These strings are a metric contract; renaming one breaks dashboards.
        assert_eq!(RejectReason::Version.as_str(), "version");
        assert_eq!(RejectReason::Length.as_str(), "length");
        assert_eq!(RejectReason::Hex.as_str(), "hex");
        assert_eq!(RejectReason::ZeroId.as_str(), "zero_id");
        assert_eq!(RejectReason::TracestateLength.as_str(), "tracestate_length");
        assert_eq!(RejectReason::TracestateBytes.as_str(), "tracestate_bytes");
    }

    #[test]
    fn a_zero_id_from_a_broken_generator_is_repaired() {
        struct ZeroIds;
        impl IdGenerator for ZeroIds {
            fn trace_id(&mut self) -> [u8; 16] {
                [0; 16]
            }
            fn span_id(&mut self) -> [u8; 8] {
                [0; 8]
            }
        }

        let started = start_with(None, None, &mut ZeroIds);
        assert!(!is_all_zero(&started.context.trace_id));
        assert!(!is_all_zero(&started.context.span_id));
        assert!(parse_traceparent(&started.context.outbound_traceparent()).is_ok());
    }
}
