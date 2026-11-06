# RRouter roadmap — deployment readiness

Five phases, each a shippable increment. Priorities come from the gaps that make a gateway
hard to run in production, compared against LiteLLM (virtual keys, budgets, spend, fallbacks,
logging, metrics) and 9router (dashboard, providers, combos, usage).

| # | Phase | Outcome | Learns from |
|---|-------|---------|-------------|
| 1 | Key governance | Per-key RPM/TPM caps, token budgets, model allowlists, expiry, per-key spend; keys UI can edit them | LiteLLM virtual keys |
| 2 | Observability | `/metrics` (Prometheus), request id, one row per attempt (fallbacks visible), log filters | LiteLLM callbacks, Grafana dashboards |
| 3 | Reliability | Retries with jitter, per-provider timeouts, graceful shutdown, concurrency cap, container healthcheck | LiteLLM router settings |
| 4 | Routing | Weighted and least-latency strategies, per-key model mapping, budget-aware provider choice | LiteLLM router, 9router combos |
| 5 | UX completion | Playground, request attempts, Settings page (branding + runtime) | 9router dashboard, LiteLLM admin UI |
| 6 | Cost accounting | USD per request from catalog prices, spend per key/model/day, USD budgets next to token budgets | LiteLLM spend tracking |
| 7 | Protocol breadth | `/v1/models/{id}`, images, audio speech/transcriptions passthrough | OpenAI-compatible surface |
| 8 | Caching | opt-in exact-match response cache with TTL and per-key bypass | LiteLLM caching |
| 9 | Guardrails | prompt/response hooks (blocklists, max tokens per request, redaction) | LiteLLM guardrails |
| 10 | Ops polish | structured JSON logs, OpenTelemetry export, quota reset schedules | production gateways |

Status: phase 1 landed (backend + UI), phase 2 backend in progress with its UI landed, phase 5
UI landed early (playground, attempts, settings). Phases 3–4 are specced and queued behind
phase 2 on the same files.

## Phase 1 — key governance (contract)

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
