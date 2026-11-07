//! Prometheus text exposition, written by hand: no client crate, no label churn.
//!
//! The registry is process-global because a scrape describes the process, not a
//! request, and because the hot path must not carry a handle just to count. Counters
//! and the histogram live under one mutex: every update is a map bump, and rendering
//! happens once per scrape.
//!
//! Label values are escaped so a model id such as `evil"model` (or one containing a
//! newline) cannot break the exposition format — the label is quoted, `\` and `"` are
//! backslash-escaped, and a newline becomes `\n`.

use std::collections::BTreeMap;
use std::sync::{LazyLock, Mutex, MutexGuard};
use std::time::Duration;

use axum::extract::State;
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use std::sync::Arc;

use crate::state::AppState;

/// Histogram upper bounds in seconds. `+Inf` is implicit (the total count).
const BUCKETS: [f64; 10] = [0.05, 0.1, 0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 30.0, 60.0];

/// Content type Prometheus scrapers require for version 0.0.4 text.
const CONTENT_TYPE: &str = "text/plain; version=0.0.4";

/// Label used when the provider or model is not known (a request refused before any
/// dispatch): an empty label value would read as "known and blank".
const UNKNOWN: &str = "-";

#[derive(Debug, Default)]
struct Registry {
    /// `(provider, model, status)` → finished requests.
    requests: BTreeMap<(String, String, u16), u64>,
    /// `(provider, status)` → dispatched attempts.
    attempts: BTreeMap<(String, u16), u64>,
    /// Non-cumulative bucket counts; index `BUCKETS.len()` is `+Inf`.
    buckets: [u64; BUCKETS.len() + 1],
    duration_sum: f64,
    count: u64,
    /// Exact-match cache lookups that were served from the store.
    cache_hits: u64,
    /// Lookups that found nothing servable (absent, expired, or unreadable).
    cache_misses: u64,
    /// Requests refused by a key's guardrails, split by the stage that caught them.
    guardrail_input_blocks: u64,
    guardrail_output_blocks: u64,
}

/// Which side of a request a guardrail refused. The metric labels this `stage`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// The prompt (system text plus message text) was too long or contained a term.
    Input,
    /// The response text contained a blocked term; the client never saw it.
    Output,
}

impl Stage {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Input => "input",
            Self::Output => "output",
        }
    }
}

/// Only poisoned by a panic while counting; the numbers are still readable, so recover.
static REGISTRY: LazyLock<Mutex<Registry>> = LazyLock::new(|| Mutex::new(Registry::default()));

fn lock() -> MutexGuard<'static, Registry> {
    REGISTRY.lock().unwrap_or_else(|e| e.into_inner())
}

/// One finished `/v1` request: its counters and its place in the latency histogram.
///
/// Called exactly once per request, including the ones that failed, so
/// `sum(rate(rrouter_requests_total))` matches the client's request rate.
pub fn request_finished(provider: &str, model: &str, status: u16, elapsed: Duration) {
    let elapsed = elapsed.as_secs_f64();
    let mut reg = lock();
    let slot = (label(provider), label(model), status);
    *reg.requests.entry(slot).or_insert(0) += 1;
    let idx = BUCKETS
        .iter()
        .position(|bound| elapsed <= *bound)
        .unwrap_or(BUCKETS.len());
    reg.buckets[idx] += 1;
    reg.duration_sum += elapsed;
    reg.count += 1;
}

/// One dispatched candidate. `status` is `0` when nobody answered.
pub fn attempt_finished(provider: &str, status: u16) {
    let mut reg = lock();
    *reg.attempts.entry((label(provider), status)).or_insert(0) += 1;
}

/// One exact-match cache lookup served from the store: no upstream call happened.
pub fn cache_hit() {
    lock().cache_hits += 1;
}

/// One exact-match cache lookup that found nothing servable and went upstream.
pub fn cache_miss() {
    lock().cache_misses += 1;
}

/// One request a key's guardrails refused, at the stage that caught it.
pub fn guardrail_blocked(stage: Stage) {
    let mut reg = lock();
    match stage {
        Stage::Input => reg.guardrail_input_blocks += 1,
        Stage::Output => reg.guardrail_output_blocks += 1,
    }
}

/// Replace an unknown label payload with [`UNKNOWN`]; everything else is escaped at
/// render time so a counter key stays the raw value.
fn label(value: &str) -> String {
    if value.is_empty() {
        UNKNOWN.to_string()
    } else {
        value.to_string()
    }
}

/// `GET /metrics` — Prometheus text for this process.
///
/// Breaker state needs the live connection list: a connection that no longer exists
/// must not keep a stale series alive, and one that exists but was never used is
/// simply closed.
static RTK_REQUESTS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static RTK_SAVED_BYTES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Record one compressed request and its byte saving.
pub fn rtk(stats: &rrouter_rtk::Stats) {
    use std::sync::atomic::Ordering;
    RTK_REQUESTS.fetch_add(1, Ordering::Relaxed);
    RTK_SAVED_BYTES.fetch_add(stats.saved() as u64, Ordering::Relaxed);
}

pub async fn scrape(State(state): State<Arc<AppState>>) -> Response {
    let connections = match state.store.list_connections(None).await {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(error = %e, "metrics: connection list unavailable");
            Vec::new()
        }
    };
    let breaker = state.dispatcher.breaker();
    let open: Vec<(String, bool)> = connections
        .iter()
        .map(|c| (c.id.to_string(), breaker.check(c.id).is_some()))
        .collect();
    let body = render(env!("CARGO_PKG_VERSION"), &open);
    let mut resp = (StatusCode::OK, body).into_response();
    resp.headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(CONTENT_TYPE));
    resp
}

/// Render the whole exposition. Split from [`scrape`] so the escaping rules are
/// testable without a database.
pub fn render(version: &str, open_connections: &[(String, bool)]) -> String {
    let reg = lock();
    let mut out = String::with_capacity(1024 + reg.requests.len() * 64);

    out.push_str("# HELP rrouter_build_info Build of the running gateway.\n");
    out.push_str("# TYPE rrouter_build_info gauge\n");
    out.push_str(&format!(
        "rrouter_build_info{{version=\"{}\"}} 1\n",
        escape(version)
    ));

    out.push_str(
        "# HELP rrouter_requests_total Finished requests, by provider, model and status (\"-\" = no provider).\n",
    );
    out.push_str("# TYPE rrouter_requests_total counter\n");
    for ((provider, model, status), count) in &reg.requests {
        let labels = format!(
            "provider=\"{}\",model=\"{}\",status=\"{status}\"",
            escape(provider),
            escape(model)
        );
        out.push_str(&format!("rrouter_requests_total{{{labels}}} {count}\n"));
    }

    out.push_str("# HELP rrouter_attempts_total Upstream attempts, by provider and status (0 = no answer).\n");
    out.push_str("# TYPE rrouter_attempts_total counter\n");
    for ((provider, status), count) in &reg.attempts {
        let labels = format!("provider=\"{}\",status=\"{status}\"", escape(provider));
        out.push_str(&format!("rrouter_attempts_total{{{labels}}} {count}\n"));
    }

    out.push_str("# HELP rrouter_guardrail_blocks_total Requests refused by an API key's guardrails, by stage.\n");
    out.push_str("# TYPE rrouter_guardrail_blocks_total counter\n");
    // Both stages are always exposed, so a scrape answers "how many" without a
    // second query and a dashboard series does not appear only when it first fires.
    out.push_str(&format!(
        "rrouter_guardrail_blocks_total{{stage=\"{}\"}} {}\n",
        Stage::Input.as_str(),
        reg.guardrail_input_blocks
    ));
    out.push_str(&format!(
        "rrouter_guardrail_blocks_total{{stage=\"{}\"}} {}\n",
        Stage::Output.as_str(),
        reg.guardrail_output_blocks
    ));

    out.push_str("# HELP rrouter_rtk_requests_total Requests compressed by the Token Saver.\n");
    out.push_str("# TYPE rrouter_rtk_requests_total counter\n");
    out.push_str(&format!(
        "rrouter_rtk_requests_total {}\n",
        RTK_REQUESTS.load(std::sync::atomic::Ordering::Relaxed)
    ));
    out.push_str(
        "# HELP rrouter_rtk_saved_bytes_total Context bytes removed by the Token Saver.\n",
    );
    out.push_str("# TYPE rrouter_rtk_saved_bytes_total counter\n");
    out.push_str(&format!(
        "rrouter_rtk_saved_bytes_total {}\n",
        RTK_SAVED_BYTES.load(std::sync::atomic::Ordering::Relaxed)
    ));
    out.push_str("# HELP rrouter_cache_hits_total Non-streaming requests served from the exact-match cache.\n");
    out.push_str("# TYPE rrouter_cache_hits_total counter\n");
    out.push_str(&format!("rrouter_cache_hits_total {}\n", reg.cache_hits));

    out.push_str("# HELP rrouter_cache_misses_total Cache lookups that found no servable entry.\n");
    out.push_str("# TYPE rrouter_cache_misses_total counter\n");
    out.push_str(&format!(
        "rrouter_cache_misses_total {}\n",
        reg.cache_misses
    ));

    out.push_str("# HELP rrouter_request_duration_seconds End-to-end request latency.\n");
    out.push_str("# TYPE rrouter_request_duration_seconds histogram\n");
    let mut cumulative = 0u64;
    for (index, bound) in BUCKETS.iter().enumerate() {
        cumulative += reg.buckets[index];
        out.push_str(&format!(
            "rrouter_request_duration_seconds_bucket{{le=\"{bound}\"}} {cumulative}\n"
        ));
    }
    cumulative += reg.buckets[BUCKETS.len()];
    out.push_str(&format!(
        "rrouter_request_duration_seconds_bucket{{le=\"+Inf\"}} {cumulative}\n"
    ));
    out.push_str(&format!(
        "rrouter_request_duration_seconds_sum {}\n",
        reg.duration_sum
    ));
    out.push_str(&format!(
        "rrouter_request_duration_seconds_count {}\n",
        reg.count
    ));

    out.push_str("# HELP rrouter_breaker_open Whether a connection's circuit breaker is open.\n");
    out.push_str("# TYPE rrouter_breaker_open gauge\n");
    for (connection, is_open) in open_connections {
        out.push_str(&format!(
            "rrouter_breaker_open{{connection=\"{}\"}} {}\n",
            escape(connection),
            u8::from(*is_open)
        ));
    }
    out
}

/// Escape a label value per the exposition format: `\`, `"` and newlines.
///
/// A bare CR would end the line for some parsers, so it is folded into the newline
/// escape instead of being passed through.
fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' | '\r' => out.push_str("\\n"),
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A model id is client input. If it could close the label the whole scrape would
    /// be garbage, so quotes, backslashes and newlines must be escaped, not copied.
    #[test]
    fn hostile_label_values_cannot_break_the_exposition() {
        request_finished(
            "acme",
            "evil\"model\\with\nnewline",
            200,
            Duration::from_millis(120),
        );
        let text = render("0.0.1", &[]);
        let line = text
            .lines()
            .find(|l| l.starts_with("rrouter_requests_total{provider=\"acme\""))
            .expect("series rendered");

        assert!(
            line.contains(r#"model="evil\"model\\with\nnewline""#),
            "label not escaped: {line}"
        );
        // Every label value is opened and closed exactly once: the quotes that are not
        // escaped pair up, so a parser sees well-formed labels.
        assert_eq!(unescaped_quotes(line) % 2, 0, "unbalanced quotes: {line}");
        assert_eq!(
            unescaped_quotes(line),
            6,
            "provider/model/status pairs: {line}"
        );
        assert_eq!(
            line.matches('\n').count(),
            0,
            "label leaked a newline: {line}"
        );
        assert!(line.trim_end().ends_with(" 1"), "count lost: {line}");
    }

    /// Quotes that are not escaped by a preceding backslash.
    fn unescaped_quotes(line: &str) -> usize {
        let mut count = 0;
        let mut chars = line.chars();
        while let Some(c) = chars.next() {
            match c {
                '\\' => {
                    chars.next();
                }
                '"' => count += 1,
                _ => {}
            }
        }
        count
    }

    /// The registry is process-global and the test harness runs these in parallel
    /// threads, so the assertions are per-series exact (a model id no other test writes)
    /// plus invariants that hold for any concurrent traffic.
    #[test]
    fn histogram_buckets_are_cumulative_and_end_at_inf() {
        // Unique label, so no other test can move this series.
        request_finished("hist", "metrics-unit-model", 200, Duration::from_millis(10));
        request_finished("hist", "metrics-unit-model", 200, Duration::from_secs(3));
        let text = render("0.0.1", &[]);

        let series = |needle: &str| {
            text.lines()
                .find_map(|l| {
                    l.strip_prefix(needle)
                        .and_then(|rest| rest.trim().parse::<u64>().ok())
                })
                .unwrap_or_else(|| panic!("missing series {needle}"))
        };
        assert_eq!(
            series(
                r#"rrouter_requests_total{provider="hist",model="metrics-unit-model",status="200"}"#
            ),
            2,
            "both observations are counted"
        );

        let bucket = |le: &str| {
            series(&format!(
                "rrouter_request_duration_seconds_bucket{{le=\"{le}\"}} "
            ))
        };
        let bounds = [
            "0.05", "0.1", "0.25", "0.5", "1", "2", "5", "10", "30", "60", "+Inf",
        ];
        for pair in bounds.windows(2) {
            assert!(
                bucket(pair[0]) <= bucket(pair[1]),
                "buckets must be cumulative: {} > {}",
                pair[0],
                pair[1]
            );
        }
        // The scrape reads one snapshot, so +Inf always equals the observation count.
        let count = series("rrouter_request_duration_seconds_count ");
        assert_eq!(bucket("+Inf"), count, "+Inf covers every observation");
        assert!(
            bucket("0.05") >= 1,
            "a 10 ms observation fits the first bucket"
        );
    }

    #[test]
    fn breaker_gauge_is_zero_or_one_per_connection() {
        let text = render(
            "9.9.9",
            &[("c-1".to_string(), false), ("c-2".to_string(), true)],
        );
        assert!(
            text.contains(r#"rrouter_breaker_open{connection="c-1"} 0"#),
            "{text}"
        );
        assert!(
            text.contains(r#"rrouter_breaker_open{connection="c-2"} 1"#),
            "{text}"
        );
        assert!(
            text.contains(r#"rrouter_build_info{version="9.9.9"} 1"#),
            "{text}"
        );
    }
}
