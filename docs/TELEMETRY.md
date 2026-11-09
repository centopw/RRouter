# Trace context (W3C) — foundation

Status: the rules live in `crates/rrouter-gateway/src/telemetry.rs` and are unit-tested,
and the chat path is wired:

- inbound `traceparent`/`tracestate` parsed at every `/v1` entry; an absent or invalid header
  starts a fresh trace (rejections counted in `telemetry`);
- every response carries `x-trace-id` (the 32-hex trace id) next to `x-request-id`;
- the access line carries `trace_id`, so a request joins its trace;
- each dispatch attempt mints a child span, logs `trace_id`/`span_id`/`parent_span_id`
  with provider, model, status and latency, and forwards that child as the provider's
  `traceparent` (the logged span id and the forwarded span id are the same).

Persisted: every usage row carries `trace_id` (schema v25). Every outbound attempt forwards
a **child** `traceparent` (plus the accepted `tracestate`), and inbound rejections are counted
in `rrouter_trace_context_rejected_total{reason}`. Still missing: a span-id column on attempt
rows (the ids are in the logs, which match the forwarded header). Optional OTLP/HTTP export
ships behind the `otel` feature (default off; §6).

The goal is narrow on purpose: accept a W3C `traceparent` when it is valid, continue that
trace with a server span, and hand the request path a value it can forward and log. There is
no span storage; sampling and OTLP export are the opt-in layer in §6.

## 1. What is accepted

### `traceparent`

```
00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01
^^ ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ ^^^^^^^^^^^^^^^^ ^^
version            trace id (32 hex)        span id (16 hex)  flags
```

| Rule | Behaviour |
| --- | --- |
| exactly 55 bytes, four `-` separated fields | otherwise `reason="length"` |
| version `00` only | `01`, `ff` and anything else → `reason="version"` |
| lowercase hex of the exact width | upper case or non-hex → `reason="hex"` |
| trace id and span id not all zero | otherwise `reason="zero_id"` |
| flags: bit 0 is `sampled`; other bits are reserved | parsed, bit 0 carried, rest ignored |

Surrounding whitespace is trimmed (HTTP header values may carry it); nothing else is
tolerated. Re-emitted flags are `00` or `01`: this gateway does not invent reserved bits.

### `tracestate`

Opaque and bounded — **no key semantics**:

- trimmed; empty means absent;
- at most 512 bytes (`reason="tracestate_length"`);
- printable ASCII only (`0x20..=0x7e`), so a control byte or newline is
  `reason="tracestate_bytes"`;
- forwarded byte for byte, only when the `traceparent` in the same request was accepted.

## 2. Fallback (never fails a request)

| Inbound | Result |
| --- | --- |
| no `traceparent` | new trace: fresh 32-hex trace id and 16-hex span id, no parent, `sampled: true` |
| invalid `traceparent` | same as above, plus `Started.rejected = Some(reason)` and the counter for that reason |
| valid `traceparent`, invalid `tracestate` | trace continues with the inbound parent; `tracestate` dropped; reason reported |
| `tracestate` without `traceparent` | dropped silently — there is no trace to attach it to, and it is not a client error |

Rejections are counted per reason in atomics (`rejected_count(reason)`) and also returned
on the result, so the caller can log the exact cause without a second lookup. The label
strings are a metric contract:

```
version · length · hex · zero_id · tracestate_length · tracestate_bytes
```

## 3. API

```rust
pub fn start(traceparent: Option<&str>, tracestate: Option<&str>) -> Started;
pub fn start_with(
    traceparent: Option<&str>,
    tracestate: Option<&str>,
    ids: &mut dyn IdGenerator,       // deterministic in tests
) -> Started;

pub struct Started {
    pub context: TraceContext,
    pub tracestate: Option<String>,
    pub rejected: Option<RejectReason>,
}

pub struct TraceContext {
    pub trace_id: String,                 // 32 lowercase hex
    pub span_id: String,                  // 16 lowercase hex, this server span
    pub parent_span_id: Option<String>,   // the inbound span when continued
    pub sampled: bool,
}

impl TraceContext {
    pub fn outbound_traceparent(&self) -> String;       // "00-<trace>-<span>-<flags>"
    pub fn child(&self, ids: &mut dyn IdGenerator) -> Self;  // attempt span
    pub fn is_continued(&self) -> bool;
}

pub fn parse_traceparent(raw: &str) -> Result<TraceParent, RejectReason>;
pub fn parse_tracestate(raw: &str) -> Result<Option<String>, RejectReason>;
pub fn rejected_count(reason: RejectReason) -> u64;
```

`RandomIds` (UUID v4 bytes) is the production generator and needs no new dependency. A
generator that returns an all-zero id is repaired defensively, so an invalid id can never
be emitted.

## 4. Guarantees worth relying on

- **Purity.** No `AppState`, no I/O, no locks (atomics only), no allocation beyond four
  short strings per request.
- **Continuity.** Inbound trace id and sampling are preserved; the inbound span id becomes
  `parent_span_id`; a new span id is always generated for this server hop.
- **Forwarding is not copying.** The outbound value is derived from our span, never the
  inbound header verbatim, so the next hop sees the gateway as its parent.
- **Bounded.** Header length checks happen before any allocation-heavy work; `tracestate`
  is capped at 512 bytes and never parsed.

## 5. Integration points (ingest and usage trace id shipped; forwarding and the rejected metric remain)

### 5.1 Proxy ingest — `crates/rrouter-gateway/src/proxy.rs` (shipped)

1. At request entry (next to `RequestTrace::new`), read the inbound headers:

   ```rust
   let started = telemetry::start(
       headers.get("traceparent").and_then(|v| v.to_str().ok()),
       headers.get("tracestate").and_then(|v| v.to_str().ok()),
   );
   ```

   Store `started` in `RequestTrace` (context, tracestate, rejected). A bad header is
   already converted to a fresh trace here — do not branch on it.
2. Echo `x-trace-id: <context.trace_id>` on **every** response, next to `x-request-id`,
   including error responses and streams (the same place `x-request-id` is inserted in
   `RequestTrace::respond`).
3. Log on the access line and on the failed-request line: `trace_id`, `span_id`,
   `parent_span_id`, `sampled` (`sampled=1/0`), and `trace_rejected=<reason>` when present.
   Do not log `tracestate` (opaque and up to 512 bytes).
4. Metrics (`metrics.rs`): shipped —
   `rrouter_trace_context_rejected_total{reason="…"}` renders one line per
   `RejectReason` (six, including zeros) from `telemetry::rejected_count`.
   `RejectReason::as_str()` is the label value; `start()` increments exactly once per
   request, so one malformed inbound header moves one series by one.

### 5.2 Attempt spans — `RequestTrace` / `Attempt` (shipped, logs only)

Before dispatching candidate *N*, the request path creates a child span
(`trace.context.child(&mut telemetry::RandomIds)`) and logs its `span_id` with the
attempt's provider, model, status and latency. The attempt row has no span column yet, so
the join is by request id in the logs; adding one is a schema bump. **v25 shipped
`usage.trace_id` (plus the `events_outbox`/`webhook_endpoints` tables), v26 shipped
tenant-scoped connection ownership and v27 the eval harness; the next free number is
v28.**

### 5.3 Upstream forwarding — `Attempt::prepare` / `send_*` (shipped)

On every outbound attempt (JSON, stream, passthrough, embeddings):

- `traceparent: 00-<trace from the request>-<this attempt's child span>-<sampled>`;
- `tracestate: <accepted inbound tracestate>` verbatim, only when the inbound traceparent
  was accepted. A generated trace forwards no `tracestate`.

The client's raw `traceparent` is never copied — the next hop sees this attempt's span as
its parent, which is what makes fallbacks and hedges legible. Headers are inserted (not
appended), so a provider's static header cannot duplicate them. Retried and hedged attempts
each mint a new child span under the same request trace; the attempt log prints the same
span id. Client `authorization`/`cookie`/`x-api-key` are never read for this: only the
gateway's own child context is written.

### 5.4 What stays out of scope

- No sampling decision of our own: `sampled` is copied from the inbound header, and new
  traces default to `true` (revisit if head sampling is added).
- No `baggage` propagation and no span export; the only stored id is `usage.trace_id`.
- No parser for `tracestate` keys.

## 6. Optional OTLP/HTTP export (`otel` feature)

Export is **off by default and adds no dependency to the default build**:
`cargo tree -p rrouter-gateway -e normal | grep -c opentelemetry` prints `0`. The feature
set is asserted by `crates/rrouter-gateway/tests/otel.rs`.

```sh
# build and run with export enabled
RROUTER_OTEL_ENDPOINT=http://collector:4318/v1/traces \
RROUTER_OTEL_SERVICE_NAME=rrouter \
RROUTER_OTEL_SAMPLE_RATIO=0.1 \
cargo run --release -p rrouter-gateway --features otel
```

| Setting | Meaning | Default |
| --- | --- | --- |
| `RROUTER_OTEL_ENDPOINT` | Full OTLP/HTTP traces URL (`http` or `https`); the exporter appends no path. Unset disables export. | unset |
| `RROUTER_OTEL_SERVICE_NAME` | `service.name` resource attribute, 1–128 bytes. | `rrouter` |
| `RROUTER_OTEL_SAMPLE_RATIO` | Local trace ratio in `0.0..=1.0`. | `1.0` |

These are **environment variables only**: the config file rejects unknown keys, so OTel
settings cannot yet be written there (a file schema change is a follow-up). Every value is
validated at startup and an invalid one stops the process naming the setting — export is
never silently disabled.

**Sampling.** The sampler is parent-based: an inbound `traceparent`'s sampled flag decides,
so a client that sampled a trace is never dropped by a lower local ratio; only locally
rooted traces use `RROUTER_OTEL_SAMPLE_RATIO`.

**What is exported.** Two span kinds, both created on the request path:

- `request` — `request_id`, `dialect`, `trace_id`; parented on the inbound W3C context, so
  the exported trace id is the same one `x-trace-id` returns and the access line logs.
- `attempt` — child of the request span, with `provider`, `model`, `attempt`, `status`,
  `error` (when present) and the upstream call duration.

**Privacy.** Span attributes are ids, provider/model names, status and latency only. Prompt
and response bodies, headers, API keys, credentials and upstream URLs are never recorded,
and the exporter stores nothing locally; the collector is the only destination.

**Shutdown.** After SIGTERM/SIGINT the server drains as before, then the provider is
flushed and shut down with a 5 s bound (off the async workers). A dead collector therefore
cannot hold the process open, and spans buffered at exit get one last export attempt.
Runtime export failures are logged at `warn` and never affect a request.

## 7. Tests


`cargo test -p rrouter-gateway telemetry` (14 unit tests) covers: valid parse; whitespace
tolerance; sampled flag preserved end to end; unknown and forbidden versions; malformed
lengths, upper-case and non-hex; all-zero ids; generated id shape and self-consistency of
the outbound header; inbound parent preserved with a new server span; child spans sharing
trace/sampling; `tracestate` forwarded opaquely and only with an accepted traceparent;
oversized and binary `tracestate` rejected; invalid inbound falling back to a new trace
with counters moving; an empty header counted as `length`; stable metric labels; and a
zero-producing generator repaired rather than emitted.

Upstream forwarding is covered in `crates/rrouter-gateway/tests/proxy.rs`: a valid inbound
trace produces a child `traceparent` with the same trace id and a new span id plus the
accepted `tracestate`; absent/invalid inbound context still forwards a generated valid child
and counts the rejection once; retries and hedged legs get distinct span ids; the embeddings
attempt forwards too; and client credentials never become trace headers.
