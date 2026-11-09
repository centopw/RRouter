# Webhook events — primitives and integration contract

Status: shipped. The envelope, signing, retry schedule and redaction live in
`crates/rrouter-gateway/src/events.rs`; the endpoint registry and outbox in
`crates/rrouter-store/src/webhooks.rs`; the delivery worker in
`crates/rrouter-gateway/src/webhooks.rs`; the admin API in
`crates/rrouter-gateway/src/routes/webhooks.rs`. §6 is the contract those pieces implement.

## 1. Envelope

Exactly four fields, in this order, with `type` as a string — this is the body that is
signed and delivered:

```json
{
  "id": "0f6f1c1e-6a4e-4f0e-9c3e-2f0c1f9a8b7d",
  "type": "connection.degraded",
  "created_at": 1700000000,
  "data": { "connection_id": "c1", "provider": "openai" }
}
```

- `id` — UUIDv4, stable for the life of the event and identical on every retry. It is the
  receiver's idempotency key.
- `type` — one of the strings in §2.
- `created_at` — unix seconds, when the event happened (not when it was delivered).
- `data` — event-specific object. Keys are serialized sorted, so the body is byte-identical
  across processes and restarts, which is what makes the signature verifiable later.

`Event::to_json()` is the canonical body: it is serialized once at enqueue and stored, and
every delivery signs exactly those stored bytes, so a caller cannot sign one payload and
send another.

## 2. Event types

| Type | When |
| --- | --- |
| `key.created`, `key.updated`, `key.deleted` | an API key is created, its policy changes, or it is deleted |
| `team.created`, `team.updated`, `team.deleted` | team lifecycle |
| `team.member.added`, `team.member.removed` | membership changes |
| `connection.created`, `connection.updated`, `connection.deleted` | provider connection lifecycle (never its credentials) |
| `connection.degraded`, `connection.recovered` | breaker opened / returned to service |
| `audit.recorded` | one persisted audit row (sign-in, role change, key administration, …) |

`credit.*` events were part of the prepaid-credits slice, which is removed
([ADR 0001](decisions/0001-no-prepaid-credits.md)); they are **not** part of this contract.
Cost changes are reported through budgets and the usage API, not as events.

The list is `EventType::ALL`; the strings are the receiver-facing contract. Adding a type
is backward compatible, renaming one is not.

The audit fan-out maps persisted audit kinds onto this vocabulary: `key_*` and `team_*`
produce their dedicated types, `connection_*` produces the connection types, and every
other audited action (sign-in, password change, role change, …) arrives as
`audit.recorded` with `{kind, actor, ip, detail}`. Audit rows carry no entity id, so the
event names the action rather than the row's numeric id.

## 3. Signing

Three headers on every delivery:

| Header | Value | Purpose |
| --- | --- | --- |
| `x-rrouter-event` | `connection.degraded` | route/filter without parsing the body |
| `x-rrouter-delivery` | the event `id` | idempotency key, **stable across retries** |
| `x-rrouter-signature` | `t=1700000000,v1=<64 hex>` | HMAC-SHA256 proof and replay bound |

The MAC covers `"<t>.<body>"` — the timestamp is inside the signature, so a replayed body
cannot be re-dated:

```
v1 = hex(HMAC-SHA256(secret, "{t}." + raw_body))
```

Verification (receiver side):

1. parse `t` and `v1` strictly (one of each; anything else is malformed);
2. reject if `|t − now| > 300 s` (configurable; `verify_with_window`);
3. recompute the MAC over the **received timestamp** and the **exact received bytes**;
4. compare with `hmac::Mac::verify_slice` — constant time, no early exit on the first
   differing byte.

`events::verify(secret, signature, body, now)` does all four and returns a typed
`EventError` (`MalformedSignature`, `StaleSignature`, `BadSignature`). Errors name the
reference, never the secret or the body.

Secrets are references, resolved per delivery, so rotation needs no restart:
`SecretRef::env("RROUTER_WEBHOOK_SECRET")` or `SecretRef::file("/run/secrets/webhook")`.
Endpoint rows store only an environment-variable **name** (`secret_env`); file references
exist for code that constructs deliveries itself. The API resolves the name at read time
and reports `secret_configured`/`ready` (§6.6) without ever returning the value.
The file is trimmed and must be a single line (>1 line is refused). Use at least 32 random
bytes; treat the value as a credential and never log it. Rotation is instantaneous for new
deliveries and does **not** re-sign in-flight retries — a receiver should accept the
current secret only, and a retry after rotation simply carries the new one.

## 4. Retry policy

`RetryPolicy::next_attempt(now, attempt)` is pure. `attempt` is 1-based (the attempt that
just failed), `now` is unix seconds.

```
delay = min(base_delay_ms × 2^(attempt−1), max_delay_ms) ± jitter_percent
```

- jitter is ±`jitter_percent` of the computed delay (clamped to 0–50), derived
  deterministically from `(now, attempt)`, so a test is exact and a fleet still spreads;
- the result is clamped to `1..=max_delay_ms` (never zero — that would be a hot loop);
- attempts are capped: `attempt >= max_attempts` returns
  `NextAttempt::Drop(DropReason::Exhausted)`, the terminal reason recorded on the row.

Defaults: 8 attempts, 1 s base, 300 s cap, 20 % jitter — roughly 1 s, 2 s, 4 s, 8 s, 16 s,
32 s, 64 s, then drop. `now` in seconds means retries in the same second share an offset;
that is intended (the outbox schedules whole seconds).

## 5. Redaction

`redact(&mut Value)` (also applied by `Event::new` and `Event::with_id`, so an `Event` that
exists is already safe to serialize, log or store):

- **by key**, case-insensitive with `_`/`-` ignored: anything ending in `password`,
  `secret`, `token`, `apikey`, `credential(s)`, plus `authorization`, `master_key`,
  `key_hash`. The whole value is replaced, so a credential *blob* cannot leak one field at
  a time;
- **by value**, second line of defence: strings that start with `Bearer `, contain
  `Authorization: Bearer`, a provider prefix (`sk-rr-`, `ghp_`, `xoxb-`), a PEM header, or
  `api_key=` / `apikey=` / `password=`;
- replacement is `"[redacted]"`, so a receiver can tell "present but secret" from "absent".

Limits: a secret under an innocuous key with no recognisable prefix is not caught — the
caller must not put credentials in an opaque field, and event payloads should carry ids and
counts, not credential material. Over-redaction is deliberate: losing `[redacted]` is
cheap, leaking a key is not.

## 6. Integration contract (A/C)

### 6.1 Enqueue point — `rrouter-store` + the audit helper

The audit helper (`routes/auth.rs::audit`) writes the audit row first, then enqueues the
event best-effort. A failed enqueue is logged (`tracing::warn`) and never fails the
mutation whose audit row already committed; the trade-off is explicit: an event can be
lost while its audit row exists, which is visible in the gateway log.

- serialize with `Event::to_json()` (already redacted) and store the string;
- `id` is the primary key (idempotent enqueue, and the delivery idempotency key);
- do not sign at enqueue time: sign at delivery, so a rotated secret applies to pending
  retries;
- no HTTP, no waiting, no `spawn` on the request path.

Mutations that write audit rows are fanned out: keys, teams, users, auth and provider
connections produce their dedicated types; settings changes have no dedicated type and
arrive as `audit.recorded`.

### 6.2 Delivery worker — `crates/rrouter-gateway/src/webhooks.rs` (shipped)

One task per process, spawned by the server and stopped by the same notification as the
HTTP drain. Shutdown joins it for 2 s and then aborts; a row that was in flight stays
leased and `recover_stale_events` returns it to `pending` at the next boot or poll.

Each tick claims due rows in a transaction — `pending` with `next_attempt_at <= now`,
ordered by `(next_attempt_at, created_at)`, flipped to `delivering` with `attempts += 1`
and a 120 s lease stored in `next_attempt_at`:

```sql
SELECT … FROM events_outbox
WHERE status = 'pending' AND next_attempt_at <= :now
ORDER BY next_attempt_at, created_at LIMIT :n;
```

For one event, every enabled endpoint subscribed to its type (exact match or `*`) is tried;
the body is the stored payload **bytes**, signed at attempt time with §3's header set. The
first non-2xx or transport failure aborts the rest and the row is retried as a unit —
receivers dedupe on `x-rrouter-delivery`.

| Result | Transition |
| --- | --- |
| every matching endpoint answered 2xx | `status='delivered'`, `delivered_at`, `last_error=NULL` |
| no enabled subscriber | `status='unmatched'` (terminal), `last_error` names the reason, `delivered_at` stays NULL — **not** a delivery |
| failure, attempts below the cap | `status='pending'`, `next_attempt_at` per §4, `last_error` |
| failure at the cap | `status='dead'` (terminal), `last_error` |

`delivered` and `unmatched` are separate counters: an event nobody subscribes to must not
inflate delivery stats. Neither terminal state is retried. Delivery is bounded (4 per tick,
1 s poll, 10 s request timeout). Terminal rows (`delivered`, `unmatched`, `dead`) are pruned
hourly by `created_at` after 7 days; pending work is never pruned. `last_error` records a
status code or `transport: <error without the url>` — never a response body or the URL's
query string.

### 6.3 Request safety

- Enqueue never blocks or fails the client request: a failed insert is logged
  (`tracing::warn`), never returned to the caller.
- Delivery never happens on the request path, and a receiver's 500/timeout never becomes
  the client's status.
- No provider credentials or `Authorization` values may reach `data`; the redaction helper
  is the net, not the licence (§5 limits).
- A delivery failure must not poison the outbox: the row is rescheduled, and terminal
  states (`dead`, `unmatched`) are recorded, not retried forever.

### 6.4 Schema (shipped in v25)

The outbox and the receiver registry are **v25** (`crates/rrouter-store/src/migrations.rs`).
v25 took the number; v26 is tenant-scoped connection ownership and v27 the eval harness, so
the next free number is v28. The shipped shape refines the earlier guess:

```sql
CREATE TABLE events_outbox (
  id              TEXT PRIMARY KEY,   -- Event::id, the delivery idempotency key
  type            TEXT NOT NULL,      -- EventType::as_str()
  payload         TEXT NOT NULL,      -- Event::to_json(), already redacted; signed per attempt
  created_at      INTEGER NOT NULL,   -- unix seconds, when it happened
  next_attempt_at INTEGER NOT NULL,   -- when the dispatcher may try next
  attempts        INTEGER NOT NULL DEFAULT 0,
  status          TEXT NOT NULL DEFAULT 'pending',  -- pending|delivering|delivered|unmatched|dead
  last_error      TEXT,
  delivered_at    INTEGER
);
CREATE INDEX idx_events_outbox_due     ON events_outbox(status, next_attempt_at);
CREATE INDEX idx_events_outbox_created ON events_outbox(created_at);

CREATE TABLE webhook_endpoints (
  id          TEXT PRIMARY KEY,
  url         TEXT NOT NULL,
  event_types TEXT NOT NULL,   -- subscription document, parsed by the API
  secret_env  TEXT,            -- env-var name; required for new endpoints, never the secret
  enabled     INTEGER NOT NULL DEFAULT 1,
  created_at  INTEGER NOT NULL,
  updated_at  INTEGER          -- NULL until the first edit
);
```

There is no `dropped_at`/`dropped_reason`: a terminal outcome is a `status` value plus
`last_error`, so the queue has one state field and one reason field instead of two pairs.
`unmatched` is terminal but not delivered: `delivered_at` stays NULL so the status screen
cannot confuse "nobody subscribed" with "the receiver accepted it". Uniqueness and
URL/format rules are the API's, not CHECKs, so a validation change is not a migration.

### 6.5 Out of scope for `events.rs`

No HTTP client, no queue, no worker, no subscription/filter model, no retry of the
signature window, no `baggage`. The module is the wire contract plus the pure decisions;
delivery mechanics live in §6.2.

### 6.6 Admin API — `crates/rrouter-gateway/src/routes/webhooks.rs` (shipped)

Admin-only (the `api` router, `require_admin`); the dashboard's Webhooks page consumes this API.

| Method and path | Behaviour |
| --- | --- |
| `GET /api/webhooks` | endpoints, `max` (32), and the subscription vocabulary |
| `POST /api/webhooks` | `url`, `event_types`, `secret_env`; https (`http` only for loopback receivers) |
| `PATCH /api/webhooks/{id}` | partial edit; `secret_env` cannot be cleared, so no endpoint is ever unsigned |
| `DELETE /api/webhooks/{id}` | `404` when absent |
| `GET /api/webhooks/status` | outbox counters (`pending`, `delivering`, `delivered`, `unmatched`, `dead`) and the 50 most recent rows without payloads |

Every row exposes `secret_env` (the variable name), `secret_configured` (the variable
resolves right now; the value is read and discarded) and `ready` (`secret_configured &&
enabled`), so the UI can warn before the first delivery. Secret values are never
serialized. The vocabulary is `event_types` as an **array**; `event_types_csv` keeps the
earlier comma-joined string for older dashboards and is deprecated. Per-row `event_types`
is an array too.

## 7. Tests

`cargo test -p rrouter-gateway events` (17 unit tests): exact JSON shape and field order;
UUID event ids; the type list is unique and round-trips; redaction of key/token/password/
credential/header/PEM shapes, including nested objects, arrays and credential blobs, and
redaction applied at event construction; signature shape and stability; delivery headers;
body and signature tampering both fail; malformed signature headers; stale/future
timestamps with and without a window; secret references from env and file (including a
multi-line refusal and a missing variable); backoff doubling and cap; terminal drop at
`max_attempts`; bounded, deterministic jitter that varies with time; and a zero-base policy
clamped to a safe minimum.

Store and worker coverage (shipped):

- `cargo test -p rrouter-store webhooks` (7 tests): URL/secret/type validation, duplicate
  URL, subscription filter (disabled and `*`), idempotent enqueue, claim/lease/reclaim/
  complete-once, `unmatched` terminal state and counter, backoff-to-`dead` at the cap, and
  pruning that keeps pending work.
- `cargo test -p rrouter-gateway webhooks` (10 tests against a real HTTP receiver): admin
  CRUD, secret redaction and `secret_configured` flipping false→true, HMAC over the exact
  bytes, subscription filtering, retry-then-success, terminal drop at the cap, disabled
  endpoint skip, duplicate-delivery idempotency, `unmatched` versus `delivered` counters, a
  key mutation succeeding while delivery is impossible, and the spawned worker delivering
  then stopping with the server.
