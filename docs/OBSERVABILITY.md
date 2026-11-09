# Observability and dashboard

## Request traces

Every `/v1` response carries `x-request-id`; the same id is stored on the usage row and on
each candidate attempt. `GET /api/usage/{id}` returns the request and its attempts
(provider, status, latency, error), so fallback is visible instead of inferred.

W3C trace context is accepted and continued (`x-trace-id`); exporting spans over OTLP/HTTP
is optional — build with `--features otel` and set `RROUTER_OTEL_ENDPOINT`. Default builds
link no OpenTelemetry crates. See [Telemetry](TELEMETRY.md).

## Metrics

`GET /metrics` is Prometheus text behind the admin gate: build info, requests by
provider/model/status, attempts by provider/status, a latency histogram, breaker state, and
guardrail/redaction counters. Scrape from the same host or authenticate at the proxy. A
ready-to-import dashboard is `deploy/grafana-dashboard.json`.

Useful queries:

```promql
sum by (status) (rate(rrouter_requests_total[5m]))
sum(rate(rrouter_attempts_total[5m])) - sum(rate(rrouter_requests_total[5m]))
histogram_quantile(0.95, sum by (le) (rate(rrouter_request_duration_seconds_bucket[5m])))
sum(rrouter_breaker_open)
```

## Dashboard

The Vite + React dashboard is served at `/app` and embedded in the binary. It needs Node
only to build.

- **Activity** — requests, tokens, latency, error rate, traffic chart, model table and
  request detail with the attempt timeline.
- **Logs** — the full request log, filterable by range/status/provider/model/key, with
  backward paging and the same request detail.
- **Models** — registry, provider coverage, disabled models, presets and preferred provider.
- **Providers** — accounts, subscription limits, quota windows and auto-ping.
- **Evals** — suites with shared context, optional judge, cases and expandable results.
- **Playground** — a real streamed request through the gateway.
- **Settings** — branding, runtime facts, logs and update checks.

A checkout without `ui/dist` still serves a smaller dependency-free shell composed from
`GET /api/ui/panels`. Content-hashed assets make a stale tab reload once when a chunk
disappears.
