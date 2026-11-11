# RRouter roadmap — deployment readiness

RRouter is a self-hosted AI gateway in one Rust binary: one endpoint in front of many
provider accounts, translating between the OpenAI Chat, OpenAI Responses and Anthropic
Messages protocols, failing over between accounts, and enforcing key/team governance with
cost accounting. SQLite and the dashboard are embedded; there is no Node runtime, Redis or
external database required, and no hosted telemetry — OTLP export is the optional exception
(§ optional tail).

This page records **what ships in this tree**, the switches and limits of the optional
features, and the **key-governance contract** worth reading before changing enforcement.
It is a capability inventory, not a completion score.

## Capability set (shipped)

| Area | What ships |
| --- | --- |
| Protocol surface | `/v1/chat/completions`, `/v1/messages`, `/v1/responses` (+ `compact`), `/v1/messages/count_tokens`, `/v1/embeddings`, `/v1/images/generations`, `/v1/audio/speech`, `/v1/audio/transcriptions`, `/v1/rerank`, `/v1/search`, `/v1/models`, `/v1/models/search`, `/v1/models/{id}`; streaming translation across the three dialects, including same-dialect passthrough. Control-plane `/v1/files` and `/v1/batches` are forwarded verbatim to providers that declare the capability (OpenAI-compatible manifests only); execution stays with the provider |
| Routing | `RROUTER_STRATEGY` = `priority`, `weighted`, `least_latency`, `cheapest`; priority is authoritative and the strategy orders within a priority group; combos (`combo:`), model aliases and disabled models; operator canary (`RROUTER_CANARY=model=provider:percent`) and non-streaming hedging (`RROUTER_HEDGE_MS`); subscription-aware reserve (`RROUTER_SUBSCRIPTION_RESERVE_PERCENT`, default 90) skips accounts near a provider plan window while a healthier candidate remains; `POST /api/route/explain` returns the ordered plan, priorities, dialects and cached window headroom without dispatching |
| Reliability | per-attempt retries with jitter capped by `Retry-After`, the request deadline and the send budget (streaming retries only before the first byte); circuit breaker with half-open probe; per-provider timeouts; global concurrency cap; graceful shutdown; `/health` and `/ready` |
| Identity & sessions | first-run owner setup code, argon2id passwords, HttpOnly JWT sessions validated against the database with refresh/rotation, single-use recovery codes, invite-only signup, `admin`/`member`/`viewer` roles, CSRF on mutations, audit trail and Audit page; each session records the client IP, raw User-Agent and a derived device label (schema v28); admin token and admin-scoped keys remain the automation path |
| Teams & tenancy | teams with members and team-scoped keys; provider accounts carry `connections.team_id`, and a team key is routed only to its own team's accounts (shared accounts are instance-wide) |
| Key governance | per-key RPM/TPM, token budgets, USD budgets over daily/weekly/monthly/lifetime windows, model allowlists (exact refs and `provider/*` globs), expiry, per-key spend; inference versus admin key scopes |
| Providers | API-key and OAuth accounts with AES-256-GCM credentials at rest, subscription-window usage (`/api/accounts/usage`), per-account quotas on requests/tokens/spend per day/week/month, connectivity probes, community provider requests |
| Observability | Prometheus `/metrics` (`rrouter_requests_total`, `rrouter_request_duration_seconds`, `rrouter_attempts_total`, `rrouter_breaker_open`, `rrouter_cache_hits_total`/`misses`, `rrouter_guardrail_blocks_total`, `rrouter_pii_redactions_total`, `rrouter_rtk_saved_bytes_total`); W3C `traceparent`/`tracestate` continuation with `x-request-id`/`x-trace-id` and a child `traceparent` forwarded to every upstream attempt; `rrouter_trace_context_rejected_total{reason}` for malformed inbound context; one usage row per request and one attempt row per upstream try (attempt span ids in logs); text/JSON access log; in-memory System logs page; Grafana dashboard in `deploy/`; optional OTLP/HTTP span export behind the `otel` cargo feature |
| Cost accounting | USD per request from catalog prices (`cost_micros`), spend per key/model/team/day, usage series/CSV export, retention sweep |
| Caching | opt-in exact-match SQLite response cache with TTL and `x-rrouter-cache: bypass`; optional semantic cache (`RROUTER_SEMANTIC_CACHE=sqlite`) with an operator-supplied embedding model, cosine threshold, TTL and row bound; non-streaming chat/messages/responses only |
| Guardrails | per-key prompt-size cap, case-insensitive blocked phrases, response scanning (streams cut with one error frame), opt-in per-key PII redaction (email/phone/card/credential shapes) with per-stage counters; opted-in streams rewrite text deltas through a 512-byte carry and flush before the terminal frame |
| Webhooks | endpoint registry with HMAC-SHA256 signed deliveries from a leased SQLite outbox, retries/backoff/dead-lettering, terminal `unmatched` state, secret readiness in the API, admin-only CRUD and delivery status |
| Evals | suites and cases, in-process runs through the real `/v1` path, results and summaries |
| Prompts & dashboard | versioned prompt templates with a Templates page and Playground integration; Activity/Logs/request detail; Settings (branding, runtime, logs, software updates); embedded single-page dashboard |
| Token Saver | RTK compresses inbound tool results before routing, on by default, toggled in Settings and bypassable per request (`x-rrouter-rtk: off`), with savings logged and counted |
| Ops & deployment | distroless multi-arch Docker image, compose, systemd unit, Kubernetes manifest, Caddy example, Grafana dashboard; `rrouter admin backup` (`VACUUM INTO`), `--check-config`, `--print-config`, `--healthcheck`, `--ready`; signed release artifacts with checksums, SBOMs and provenance |

## Status

Published releases through **0.2.3** were verified live (installer, multi-arch image,
release binaries). **0.7.1** is the current release: everything listed above is
implemented in this tree, and `CHANGELOG.md` → `0.7.1` is the authoritative list of
what changed since 0.2.3. This page deliberately does not claim a completion percentage;
release readiness is the coordinator's verification.

## Optional-tail features (shipped behind switches)

These are present in this tree but opt-in or compiler-gated. Each carries a real limit;
none is a release blocker.

| Feature | Switch | Shipped behaviour and limit |
| --- | --- | --- |
| Semantic response cache | `RROUTER_SEMANTIC_CACHE=sqlite` + `RROUTER_SEMANTIC_EMBEDDING_MODEL=provider/model` | Embeds a canonical prompt, serves a stored response when cosine ≥ `RROUTER_SEMANTIC_CACHE_THRESHOLD` (default 0.92) within the scope/TTL/row bounds. **Non-streaming chat/messages/responses only**; needs the embedding provider to answer before the lookup, and its spend is operational, not billed to the key |
| Canary / A-B | `RROUTER_CANARY=model=provider:percent` (reloadable) | Tries the named provider first for that share of request ids inside its priority tier. One rule per process; no multi-arm experiments or metrics split beyond the existing per-provider counters |
| Hedging | `RROUTER_HEDGE_MS` (restart-only, 0 disables) | After N ms without a response, the next same-priority candidate starts in parallel; the first success wins and the loser is cancelled. **Non-streaming only** — a stream can only retry before its first byte |
| Files and batches | provider declares `files_url`/`batches_url` | only the built-in OpenAI provider declares these today; bodies are relayed byte-identically (multipart boundary and batch JSON preserved) and **never executed or stored here**. A provider without the capability answers `501`; pin one provider when a batch references a file; uploads are buffered under a 64 MiB cap |
| Optional OTLP/HTTP export | build with `--features otel`; `RROUTER_OTEL_ENDPOINT` | Default builds contain no OpenTelemetry crates. **Env-only** (the config file rejects unknown keys), off until an endpoint is set, sampled parent-based, and shutdown flushes with a 5 s bound. Exports request/attempt spans only, with ids/provider/model/status/latency — never bodies |
| Stream-body PII redaction | per-key `guardrails.redact_pii` | Text and reasoning deltas are rewritten; tool-call arguments/opaque payloads are deliberately **not** rewritten. The carry is **512 bytes**, so visible text can lag by that much and a longer value can straddle an emission cut; an interrupted stream drops an unterminated PII-looking tail |
| Session device metadata | none (schema v28) | Every session creation path records the resolved client IP (forwarded first hop) and raw User-Agent bounded to 512 chars; the API derives `device_label`. Legacy rows stay null; the forwarded IP is trustworthy only behind a proxy that overwrites `X-Forwarded-For` |
| Evals | admin API/UI | Suites and cases run in-process through the real `/v1` path; results/summaries are stored. No scheduler and no CI trigger — runs are operator-started |
| Webhooks | admin API/UI (`/api/webhooks`) | HMAC-signed deliveries from a leased SQLite outbox with retries, dead-lettering and a terminal `unmatched` state. One event row fans out to every matching enabled endpoint; a retry re-sends the whole event, so receivers must dedupe on `x-rrouter-delivery` |

### Still not shipped

- A span-id column on attempt rows: the attempt span ids are forwarded upstream and logged
  (the two match), but the database has no column for them yet.

### Contract note — team isolation

Team accounts keep strict isolation: a team key is planned only from its own team's provider
accounts; shared accounts are never eligible, so a team with no assignment fails with
`404 model_not_found` rather than borrowing the operator's credentials.

## Not planned — prepaid credits (removed)

A credit balance / top-up / reseller slice was planned and is now removed: RRouter is
self-managed and owns the provider accounts, so key and team token/USD budgets plus the
usage/spend reporting are the cost controls. The slice was never released, and no later
work includes it. Rationale and the resale path:
[ADR 0001](decisions/0001-no-prepaid-credits.md).

## Key governance contract (shipped)

Schema v7 on `api_keys`: `rpm`, `tpm`, `budget_tokens` (0 = unlimited), `allowed_models`
(JSON array, NULL = all), `expires_at` (NULL = never). `usage.key_id` attributes every row.

`GET /api/keys` rows:

```json
{"id":"…","name":"laptop","created_at":1710000000,"is_active":true,
 "rpm":60,"tpm":90000,"budget_tokens":5000000,"spent_tokens":120000,
 "allowed_models":["claude/claude-haiku-4-5-20251001"],"expires_at":null}
```

`PATCH /api/keys/{id}` accepts any subset of `is_active`, `rpm`, `tpm`, `budget_tokens`,
`allowed_models` (null clears), `expires_at` (null clears) and returns the updated row.

Enforcement, on `/v1/*`:

- a presented key that is inactive or expired is rejected (`403`, code `api_key_inactive` /
  `api_key_expired`), even when `RROUTER_REQUIRE_API_KEY=false`;
- an unknown key is ignored when keys are not required (dev ergonomics), rejected when they are;
- `allowed_models` accepts exact refs and `provider/*` globs; anything else is `403`
  `model_not_allowed`;
- RPM and budget are checked before dispatch (`429` `rate_limited` / `budget_exhausted`),
  TPM is checked from the tokens recorded in the last 60 s (`429` `rate_limited`);
- every usage row records `key_id`, so spend and per-key activity are queryable.
