# Changelog

All notable changes. Dates are local; versions follow semver once tagged.

## Unreleased

### Added
- Model lists are newest-first (version- and date-aware) and retired families (Claude 3.x,
  GPT-4 turbo, gpt-5-codex) sort last, carry a `deprecated` badge, and are never chosen by a
  connectivity probe. Probes prefer the cheapest current model, using the provider's own list
  or a synced price, so testing an account no longer spends `claude-3-7-sonnet`. Claude's
  list fetch asks for one maximum-size page, so newly released models appear without cursor
  paging.
- Subscription usage: `/api/accounts/usage` reads the plan windows the provider already
  reports (Claude's 5-hour and weekly buckets, Codex's primary/secondary windows), cached for
  a minute, and the Providers page shows them as read-only bars next to the optional operator
  caps. GitHub Copilot's monthly request snapshots are read too. The provider stays the
  source of truth; RRouter never invents a percentage.
- Dashboard accounts: first-run owner creation with the one-time setup code printed at startup,
  argon2id passwords, HttpOnly session cookies, ten single-use recovery codes, invite-only
  sign-up, and the admin/member/viewer roles (`/api/auth/*`, `/api/users`, `/api/users/invites`).
  The owner cannot be demoted, disabled or deleted; role and CSRF refusals name their reason.
  Automation keeps working: `RROUTER_ADMIN_TOKEN` and admin-scoped keys are unchanged, and
  `rrouter admin show-setup-code` / `rrouter admin reset-password <email>` run locally.
- A light, flat dashboard system: near-black navigation rail, off-white canvas, white surfaces,
  a single muted navy accent, pill buttons, 4px inputs, 8px cards, hairline borders instead of
  shadows, Phosphor icons, and 75/400/500ms ease-out motion that respects reduced motion.
- Guardrails per key: prompt-size cap, case-insensitive blocked phrases, and response scanning
  (streams are cut with one error frame). Blocks return 403 and are counted per stage.
- `RROUTER_STRATEGY=cheapest` orders equal-priority accounts by the model's catalog price.
- Retries before failover: `RROUTER_RETRY_ATTEMPTS`, `RROUTER_RETRY_BASE_MS`,
  `RROUTER_RETRY_MAX_MS` drive exponential backoff with jitter, capped by `Retry-After`, the
  headers deadline and the send budget. Streaming attempts retry only before the first byte.
- Prompt templates with version history (`/api/templates`), and a Templates page plus Playground
  insert / save-as-template.
- Key scopes: an admin key manages the gateway through `/api` and `/metrics` without loopback or
  a shared admin token, and is refused on `/v1`; inference keys keep limits, budgets and
  guardrails. The key editor sets the scope and the list badges admin keys.
- Account quotas: each upstream account can cap requests, tokens and spend per UTC day, week or
  month (`/api/accounts/quotas`); the router skips an exhausted account, returns
  `429 quota_exhausted` with `Retry-After` when every candidate is spent, and the Providers page
  shows each account's window usage with a quota editor. Usage is aggregated from the request
  log, so nothing counts, resets or repairs a counter.

### Changed
- The shell is light throughout: white rail, collapsible to icons, light-mode logo, and every
  dialog rebuilt as a flat card with a header, scrollable body and sticky footer. Destructive
  actions (provider, key, user, invitation, template, alias, combo, custom model, quota clear)
  confirm in a dialog instead of `window.confirm`, and every dropdown is the dashboard's own
  searchable, keyboard-navigable list.
- Logs filters sit in one toolbar (range, status, provider, model, key) with a clear action,
  instead of wrapping into a stack, and the Playground reads as a conversation: centered thread,
  suggestion starters, and a composer with the model and send control inline.
- The dashboard now signs you in as a user. The pasted admin token moved behind "Use an admin
  token instead" on the sign-in screen, and loopback/break-glass access is unchanged for the API.
- The request detail opens as a dialog when a row is clicked (Activity and Logs) instead of
  occupying a column of the page; nothing is selected until you click, and Escape or the
  backdrop closes it.

## 0.2.3

### Security
- Every response carries `x-content-type-options: nosniff`, `x-frame-options: DENY`,
  `referrer-policy: no-referrer` and a restrictive `permissions-policy`; HTML documents add a
  same-origin CSP (https images and operator-configured extension-panel frames allowed,
  scripts and fetches same-origin). Insert-only: headers already set upstream win.

## 0.2.2

### Added
- CSV export of the filtered request log (`/api/usage/export.csv`) and an Export CSV action on
  the Logs page; rows stream in 1000-row cursor pages.
- Usage retention: `RROUTER_USAGE_RETENTION_DAYS` deletes old usage/attempts in a daily sweep
  (expired cache rows always go) and runs `PRAGMA optimize`; schema v13 indexes `attempts(ts)`.

## 0.2.1

### Added
- Server-side filters on the request log (`provider`, `model` ref or bare id, `key_id`,
  `status` 2xx/error) that compose with the cursor.
- Logs page: filterable, cursor-paginated request history with the same attempts detail as
  Activity.
- Structured logs: `RROUTER_LOG_FORMAT=json` and one access line per finished request
  (request id, provider, model, status, latency, first-output time, endpoint, key id).
- Cursor pagination for the request log (`/api/usage` `before`/`before_id` and
  `next_before`), so history can be paged without time-window guessing.
- Budget windows: per-key budgets reset daily, weekly or monthly (UTC), enforced alongside the
  lifetime cap; `spent_*` on the key row is scoped to the current window.
- Opt-in exact-match response cache for non-streaming requests (`RROUTER_CACHE=sqlite`), with
  TTL, size bound, `x-rrouter-cache` hit/miss/bypass and hit rows that record zero tokens.
- The dashboard prompts for `RROUTER_ADMIN_TOKEN` and keeps it per tab, so a proxy-fronted
  deployment can authenticate its own admin API.

## 0.2.0 — first public release

### Gateway

- One binary serving OpenAI Chat, OpenAI Responses and Anthropic Messages on `/v1`, with
  incremental translation in both directions (streaming and non-streaming).
- `/v1/embeddings`, `/v1/models` (list and by id), and forwarded
  `/v1/messages/count_tokens`, `/v1/responses/compact`, `/v1/images/generations`,
  `/v1/audio/speech`, `/v1/audio/transcriptions` and `/v1/audio/voices` for providers that
  implement them (501 otherwise, never an estimate).
- Subscription sign-in for Claude and Codex, GitHub Copilot device flow, API keys for OpenAI,
  Anthropic, Gemini, DeepSeek, Groq, OpenRouter, xAI, Mistral, Together, Cerebras, Ollama.
- Credential refresh ahead of expiry with a per-account lock; encrypted at rest (AES-256-GCM).

### Routing

- `provider/model`, aliases (one hop), and combos; priority-ordered fallback with a circuit
  breaker, `Retry-After` handling, and half-open probes.
- Conversation affinity: equal-priority accounts are pinned per conversation so prompt caches
  stay warm; unrelated conversations still spread.
- Same-dialect requests are forwarded as the client wrote them — Anthropic `cache_control` and
  OpenAI cache fields survive; only routing fields are patched.
- Explicit `provider/model` never widens to other providers, even for ids the catalog does not
  know.

### Keys and limits

- Per-key RPM, TPM, token budget, model allowlist (exact refs and `provider/*`), expiry, and
  per-key spend from recorded usage.
- Hashed at rest, plaintext shown once; enforcement returns `403`/`429` with stable codes in
  the caller's dialect.

### Model registry

- Merged catalog + provider `/models` + models.dev metadata + custom rows; capabilities, limits,
  pricing; a real 16-token test through the same planner and dispatcher a client would use.
- Only connected providers are listed; models.dev enriches, never adds.

### Dashboard

- Activity redesign: readouts with window deltas, stacked traffic chart with error lane and
  tooltip, ranked models, request log with attempt timeline, tokens/latency/first-output split.
- Setup wizard for the first run: connect an account, create a key, send and watch the first
  request.
- Playground for streaming Chat/Messages; Settings for branding and runtime facts; white-label
  branding applied live.
- Requests can be filtered by model, errors, and key; every number is server-side.

### Operations

- `usage`/`attempts` accounting, Prometheus `/metrics`, `/health` and a self-probe
  (`rrouter --healthcheck`), graceful shutdown with a bounded drain.
- Request ids (`x-request-id`) and per-attempt records, so a fallback is visible in
  `GET /api/usage/{id}` and in Activity instead of inferred.
- `RROUTER_ADMIN_TOKEN` for the admin API, `RROUTER_MAX_CONCURRENCY` backpressure (503 +
  Retry-After), `RROUTER_MAX_ATTEMPTS`, `RROUTER_SHUTDOWN_GRACE_SECS`.
- Docker (distroless, nonroot, healthcheck), compose, single-replica Kubernetes manifest,
  systemd unit, Caddy example with admin auth.
- `scripts/backup.sh` (consistent SQLite snapshot), `scripts/load_test.py`, `scripts/verify.sh`,
  live e2e and OAuth smokes.

### Fixed

- Claude OAuth: the registered callback host (`localhost`, not `127.0.0.1`), `state` in the
  token exchange, and an immediate loopback answer instead of a hung page.
- OAuth requests now claim a current Claude Code version, which newer models require.
- Cache markers were dropped when a request was rebuilt; same-dialect bodies are now forwarded.
- Activity counted requests one bucket early; windows now end in the bucket containing "now".
- Streamed responses recorded `streamed=false`; the flag and time-to-first-output are recorded.
- The shutdown grace started at process boot instead of on the signal, so every gateway
  exited after 20 s; the timer now starts with SIGTERM/SIGINT and exits in milliseconds when
  nothing is streaming.

### Known limitations

- Codex's subscription backend does not serve `/responses/compact` (OpenAI API keys do).
- OAuth sign-in needs the browser to reach the loopback callback; remote hosts need an SSH
  port-forward (documented) — there is no paste-the-URL fallback yet.
- Limits are in tokens; USD budgets track the catalog price in a later release.
