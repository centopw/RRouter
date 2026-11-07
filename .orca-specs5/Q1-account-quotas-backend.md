# Task: Account quota tracking — backend (schema, store, enforcement, API)

Repo: /Users/cento/git/personal/RRouter. Read `crates/rrouter-store/src/migrations.rs`,
`store.rs`, `crates/rrouter-gateway/src/{state.rs,lib.rs,proxy.rs,routes/*}` first. Follow the
existing patterns exactly (migration consts V1..V16, `Store` methods with `rusqlite`, axum
routes with `require_admin`, typed request/response structs in `routes`).

## Goal

Each upstream connection (`connections` row) is a provider **account**. Operators can set a
per-account quota over a trailing window (requests, tokens, cost); the router skips exhausted
accounts and the UI shows usage vs limit. Usage comes from the existing `usage` table
(`connection_id`, `input_tokens`, `output_tokens`, `cost_micros`, `ts`). No counters, no
background jobs: aggregate at read time so restarts are free.

## Schema v17 (bump `SCHEMA_VERSION` to 17)

```sql
CREATE TABLE IF NOT EXISTS account_quotas (
  connection_id   TEXT PRIMARY KEY,
  window          TEXT NOT NULL DEFAULT 'daily', -- daily | weekly | monthly | total
  max_requests    INTEGER NOT NULL DEFAULT 0,    -- 0 = unlimited
  max_tokens      INTEGER NOT NULL DEFAULT 0,
  max_cost_micros INTEGER NOT NULL DEFAULT 0,
  updated_at      INTEGER NOT NULL
);
```

Window reuses the key budgets' `BudgetWindow` and `window_start` (UTC calendar boundaries:
midnight, Monday, the 1st), so a reset is predictable and there is no trailing-window
bookkeeping. `window_reset(window, start)` returns the end of the window; unknown stored
spellings read as `total` (all time — the strictest rule, never unlimited).

## Store (`rrouter-store`)

- `AccountQuota { connection_id, window: QuotaWindow, max_requests, max_tokens,
  max_cost_micros }` with `QuotaWindow::{Day, Week, Month}` (`as_str`/`parse`).
- `Store::set_account_quota(&AccountQuota)` (upsert, `updated_at = now`),
  `Store::clear_account_quota(id)`, `Store::account_quotas() -> Vec<AccountQuota>`.
- `Store::account_usage_since(since_ms, now_ms) -> Vec<AccountUsage>`
  `AccountUsage { connection_id, requests, input_tokens, output_tokens, cost_micros }`
  — one grouped query over `usage`, `status >= 200 AND status < 300`? **No**: count every
  usage row that recorded a spend (any status) only if it has a non-zero token/cost; simplest
  and cheapest is to sum all rows with `connection_id IS NOT NULL AND ts >= ?` and count rows
  as requests. Keep it as one `GROUP BY connection_id` query.
- `Store::account_quotas_with_usage(window) -> Vec<AccountQuotaState>` where
  `AccountQuotaState { quota, usage, exhausted: bool, resets_at: Option<i64> }`;
  `exhausted` is true when a non-zero limit is reached (`>=`);
  `resets_at` = timestamp of the oldest usage row still inside the window that contributes to
  the exhausted dimension + window length (i.e. when usage will drop below the limit), or None
  when there is no usage. If that is awkward, a single extra query
  `SELECT MIN(ts) FROM usage WHERE connection_id = ? AND ts >= ?` is fine.
- Tests: upsert/read/clear round-trip, unknown window reads as day, limits 0 mean unlimited,
  usage aggregation ignores `ts < since` and rows with NULL connection_id, exhaustion at
  `>=`, v16 -> v17 migration, `SCHEMA_VERSION == 17`.

## Enforcement (`rrouter-gateway/src/proxy.rs`)

Before a request is dispatched (where candidates are assembled and filters such as disabled
models/keys already apply), drop connections whose quota is exhausted for their own window.
One `account_quotas_with_usage`-style call per request is acceptable; do it only when at least
one quota row exists (`Store::account_quotas()` empty => no query, no cost for the default
setup).

- A request whose every candidate is skipped this way fails like the existing quota error:
  HTTP 429, code `quota_exhausted`, `retry_after_secs` from the smallest `resets_at - now`
  (omit the header when unknown). Reuse `ProviderError::Quota`/existing mapping if it already
  maps to 429; otherwise add the mapping in the same place as other provider errors. Include
  "which accounts were skipped" in the server log at debug level only.
- Do not skip accounts on `/v1` requests with no quota row. Admin routes untouched.
- Test in `tests/proxy.rs` (fake upstream, give it a connection): a connection at its request
  limit is not tried (fake upstream sees 0 hits) and the request 429s with `quota_exhausted`
  when it is the only candidate; with a second healthy connection the request succeeds and the
  exhausted one still sees 0 hits.

## API (`routes/`)

- `GET /api/accounts/quotas` -> `{ quotas: [ { connection_id, name, provider, is_active,
  window, max_requests, max_tokens, max_cost_micros, requests, tokens, cost_micros, exhausted,
  resets_at } ] }` for **every** connection (quota row optional; defaults window=day, limits
  0), ordered by provider then name. `tokens` = input+output. Join against `connections` for
  name/provider/is_active; a quota row for a deleted connection is ignored.
- `PUT /api/accounts/quotas/{connection_id}` body `{ window?, max_requests?, max_tokens?,
  max_cost_micros? }`, missing fields keep existing values (defaults 0/day), returns the same
  row shape as the list. Unknown `connection_id` -> 404 `connection_not_found`; unknown window
  -> 400 `invalid_window`; negative numbers -> 400.
- `DELETE /api/accounts/quotas/{connection_id}` -> 204, idempotent.
- All three are admin routes (`require_admin`), like `/api/keys`.
- Tests for CRUD, validation, and the list join.

## Boundaries

`cargo test -p rrouter-store -p rrouter-gateway` and `cargo clippy --all-targets
--all-features -- -D warnings`. **Do not run `cargo fmt`**: this toolchain's rustfmt 1.10
reformats unrelated files, so the repo fmt check is known-noisy; match surrounding style by
hand. No UI, README/changelog (the coordinator handles docs), no release build, no servers, no
commits. Report API shapes and test counts.
