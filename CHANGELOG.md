# Changelog

All notable changes. Dates are local; versions follow semver once tagged.

## 0.4.3 — 2026-11-09

### Added
- Google Antigravity / Cloud Code Assist provider: Google OAuth + PKCE, project discovery/onboarding, `project_id` credential metadata, email label, CCA envelope and SSE translation, provider model discovery.
- Remote OAuth deployments: manual start without a loopback listener and paste-the-callback completion; branded loopback success/failure pages.
- Webhook/audit, session device metadata and model preset follow-ups from the 0.4.2 line.

### Changed
- Provider connection table no longer overflows its card; names wrap, auth is one badge with a tooltip, and row actions live in a keyboard menu.
- Shell/sidebar/content scroll ownership and provider marks/shell layout fixes; number-input spinners removed; quiet buttons outlined.

## 0.4.2 — 2026-11-09

### Added
- Model presets: named layouts with drag-and-drop ordering, hidden/restore lists, archived restore positions, auto-include policy, revision-guarded saves, 409 conflict recovery, and per-account active preset selection.
- GitHub update checks now use an optional read-only token, persistent conditional metadata, rate-limit reset backoff, and a release-redirect fallback so routine checks do not consume the unauthenticated API budget.

### Changed
- Release pipeline now builds Docker architectures natively instead of QEMU, merges per-arch manifests, and creates one changelog-derived release body instead of four generated compare blocks.
- CI runs actionlint for workflow files.

## 0.4.1 — 2026-11-09

### Documentation and media
- Added the 10-second product demo (MP4/WebM/GIF), dashboard screenshots for desktop and mobile, and a Mermaid architecture diagram with a numbered runtime flow.
- README now leads with the demo and screenshots; install and roadmap references point at the new release.
- This release carries the same gateway features as 0.4.0; no runtime behavior changed.

## 0.4.0 — 2026-11-09

### Added
- Webhooks: an admin-only endpoint registry (`/api/webhooks`) and a leased SQLite outbox
  that delivers HMAC-SHA256-signed events (key/team/connection/audit) from a background
  worker with backoff, dead-lettering and a terminal `unmatched` state for events with no
  subscriber. The dashboard page shows subscription, secret readiness and queue status.
- W3C trace context: `traceparent`/`tracestate` are accepted on every `/v1` entry, `x-trace-id`
  is echoed on every response next to `x-request-id`, the access line carries `trace_id`,
  every attempt logs a child span, and `usage.trace_id` joins a request's rows. Every
  outbound attempt forwards a child `traceparent` (and the accepted `tracestate`) with the
  same span id it logs — retries and hedged legs get distinct children — and rejected
  inbound contexts are counted in `rrouter_trace_context_rejected_total{reason}`.
- Teams own provider accounts (`connections.team_id`): a team-scoped key is routed only to
  its team's accounts, and the upgrade note lives in `docs/DEPLOY.md` §5.1.
- Evals: suites and cases executed through the real `/v1` path, with results and summaries
  (`/api/evals/*` and the Evals page).
- Per-key PII redaction (email/phone/card/credential shapes) for prompts, non-streaming
  responses and streamed text, off by default, counted per stage and documented in
  `docs/GUARDRAILS.md`. Streams rewrite text/reasoning deltas through a 512-byte carry
  (bounded visible lag; a stream ending without its terminal frame still flushes); tool-call
  arguments and opaque payloads are deliberately left byte-identical.
- Semantic response cache: `RROUTER_SEMANTIC_CACHE=sqlite` embeds a canonical prompt with
  an operator-supplied `RROUTER_SEMANTIC_EMBEDDING_MODEL` and serves a stored response when
  cosine similarity reaches `RROUTER_SEMANTIC_CACHE_THRESHOLD`, within the scope, TTL and
  row bounds. Non-streaming chat/messages/responses only.
- Operator canary and non-streaming hedging: `RROUTER_CANARY=model=provider:percent`
  prefers a provider for a share of request ids inside its priority tier (reloadable), and
  `RROUTER_HEDGE_MS` starts the next same-priority candidate in parallel after the delay,
  first success wins, loser cancelled. Streams are never hedged.
- OpenAI-compatible files and batches: `/v1/files` and `/v1/batches` are forwarded
  byte-identically to providers that declare the capability (multipart boundary and batch
  JSON preserved), never executed or stored here; a provider without it answers `501`.
- Optional OTLP/HTTP span export behind the `otel` cargo feature (default off, so default
  builds link no OpenTelemetry crates): `RROUTER_OTEL_ENDPOINT` (env-only) enables
  request/attempt spans with parent-based sampling, and shutdown flushes with a 5 s bound.
  Only ids, provider/model, status and latency are exported.
- Session device metadata (schema v28): every sign-in path records the resolved client IP,
  raw User-Agent (bounded to 512 chars) and a derived `device_label`, returned for the
  signed-in user's own sessions.
- Token Saver (RTK): inbound tool results are compressed before routing — grouped greps,
  capped git status/diff/log, collapsed build logs and repeated lines, head/tail truncation
  with markers. On by default, toggle in Settings, bypass with `x-rrouter-rtk: off`; savings
  are logged and exported as `rrouter_rtk_saved_bytes_total`.
- Production identity tooling: persisted audit trail with an Audit page, self-service
  account page (rename, password change with session rotation, per-device revoke and device
  labels from the session metadata), recovery
  throttling, `/ready`, and `rrouter admin backup <file>` (consistent `VACUUM INTO`). CI runs
  clippy, tests and the dashboard build.
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

### Added
- Logs now include failed requests: every terminal failure writes one zero-token usage row
  with its HTTP status and client-facing error, and the request drawer lists each attempt's
  upstream error. A new **System logs** page captures the gateway's own tracing events in
  memory for debugging; it is off by default, never written to disk, and can be enabled or
  cleared at runtime.

### Added
- Activity has a **Live routing** map: the gateway in the centre, one node per provider
  with an active connection, request/error counts for the last 15 minutes, and an animated
  edge while a provider served traffic in the last 90 seconds. It refreshes every 5 seconds.
- Request detail now records and draws the latency phases — connect + upstream headers,
  model → first token, generation — instead of only inferring them from TTFT.

### Changed
- Interactive primitives are fully Radix-backed: Dialog, AlertDialog, Tabs, Switch,
  Checkbox, Popover, Toast, Accordion, Tooltip-group/Toggle, Separator and Label. The
  searchable Select keeps its filter UX on a Radix Popover; no hand-rolled dismiss/focus
  logic remains.

### Security
- Dashboard sessions are HS256 JWTs signed with a key stored beside the data volume.
  The database still validates every session, so logout and password changes revoke a
  mathematically valid token. A new `/api/auth/refresh` rotates the session id and the
  client retries once on 401, invalidating the previous token. Sessions created without
  “keep me signed in” are pruned on process start, so a redeploy ends them; remembered
  sessions survive.

### Added
- Community providers: the Add-provider → API key screen links to API and OAuth issue
  forms. The API form is parsed into `providers/community/<id>.json`, compiled into the
  next binary by `crates/rrouter-auth/build.rs`, and opened as a reviewed pull request by
  the `provider-request` workflow. OAuth providers are reviewed manually with their flow.

### Added
- A semantic design system: one type scale in `index.css`, one component per file under
  `components/atoms` and `components/molecules`, and Radix-backed Dialog, Tabs, Switch,
  Separator and Label primitives. Shared molecules use Text/Heading/Stack/Inline; raw font
  sizes were removed from the dashboard so typography is changed in one place.

### Added
- The Add-provider → API key screen now carries the two custom endpoints, tinted with the
  OpenAI and Anthropic brand colours. Every provider row shows its real mark from
  simple-icons where available, with the provider's favicon as the fallback.
- OTA updates: **Settings → Software updates** checks GitHub daily, shows release notes and
  the detected deployment, optionally downloads and verifies the release archive, and offers
  a confirmed restart. Binary/systemd installs replace and exec in place; Homebrew runs
  `brew upgrade`. Docker, Kubernetes and Helm report their supervisor's exact upgrade
  command because a container or pod cannot replace its own image or release.
- OpenCode is a native provider (`https://opencode.ai/inference/openai/v1`), with its shared
  model catalogue and an add-provider guide for the Free, pay-as-you-go, Go/Go Plus/Go Teams
  plans, including direct API-key links.
- The sidebar has two custom-endpoint shortcuts (OpenAI-compatible and Anthropic) that open
  the key form with the protocol fixed and a required base URL.

### Fixed
- Choosing a provider in the API-key flow now carries that selection into the form instead of
  always opening with the first registered provider.
- Activity model rows select on a click anywhere, not only on the model name.

### Changed
- In the collapsed sidebar the logo itself is the expand control: hovering swaps it for the
  expand icon, with no second button competing for the rail's width. Activity request rows
  are clickable across every cell, the inspector is a right-hand drawer, and the tokens
  column no longer wraps its header.
- Rate-limited accounts fail over immediately: a 429 is no longer retried on the same
  sub-account before trying the next one, and the breaker's 429 cooldown now starts at 30 s
  and escalates to one hour instead of expiring after four minutes. Providers shows an
  explicit **quota exhausted** badge when a subscription window reports 100%.

### Fixed
- Codex rejects `temperature` and `top_p` instead of ignoring them, failing every
  Playground turn; the gateway strips those sampling controls for the codex provider on
  both the chat→Responses translation and same-dialect Responses passthrough.
- Sign-in reads the submitted DOM values instead of React state, so browser autofill and
  password managers (which can fill fields without firing `input`) work on the first submit.
- Dialogs are right-hand drawers; test results and other transient outcomes use a toast
  stack. The Logs row is clickable everywhere with a clock time + relative age. Models can be
  edited per row (display, context, output, price, capabilities), metadata sync/fetch moved to
  Settings with an explanation of each, and the API key is configured in a creation drawer and
  revealed there once. Aliases and combos gained edit flows. Chart tooltips appear only on
  hover, and the supplied brand logo replaces the interim mark.
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
