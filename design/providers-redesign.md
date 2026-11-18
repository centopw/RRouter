# Providers redesign — one provider, N accounts

From `02-multi-account-routing.md`, re-checked against the tree (2026-11-18). **S0 is
shipped**; S1/S2 are design. Every `file:line` was verified for this revision.

## Executive summary

- Grouping is a **read projection over `connections`**: a provider header row over its account
  rows, computed client-side. No new routing entity, no planner change, no wire change.
- One provider already owns N accounts: `id` is the only key, and `Candidate = (provider,
  model, connection)` expands one model to one candidate per account
  (`crates/rrouter-router/src/candidate.rs:9-23`). The gap was the page, not routing.
- Order is **`priority` ascending, then the tier strategy**; `weight` is a share *inside* a
  tier and only under `RROUTER_STRATEGY=weighted`. It never promotes across tiers.
- Team scoping is asymmetric and **the team premise is reversed**: a team key excludes shared
  rows; a global key sees every account. Preserve it.
- Default view is expanded (no collapse yet). No preferred-provider picker, no cross-provider
  collapse.

## Current model

A `connections` row is one credential set for one provider; `id` is the only identity. The
fields the rest relies on (`store.rs`, `migrations.rs`):

| Field | Meaning |
|---|---|
| `provider` | Routable provider id, compiled or operator-defined; unconstrained `TEXT`, no FK (`migrations.rs:746`) |
| `name`, `email` | Labels only; `email` is `credentials.account_label` (`migrations.rs:748-749`, `crates/rrouter-auth/src/credentials.rs:36`) |
| `priority` / `weight` | Lower priority tried first, default `0`; weight `1..=1000` default `100`, input of `weighted` only (`store.rs:41-44`, `migrations.rs:678,750`) |
| `team_id` / `owner_user_id` | `NULL` team = shared; a team id means that team's routes only. Owner is attribution, never access (`store.rs:54-61`, `migrations.rs:353-354`) |
| `backoff_level`, `last_error*` | Operator-**probe** state, not live breaker state (`store.rs:47-51`, `routes/connections.rs:943-963`) |

`id` is the only key (`migrations.rs:744-760`); a create inserts a new UUID row
(`store.rs:1234-1276`, OAuth `routes/oauth.rs:183`). Storage lists provider-grouped already
(`ORDER BY provider, priority, created_at`; `store.rs:1364-1371`).

### Asymmetric team scope (correct today — do not "fix")

| Key scope | Sees |
|---|---|
| `Some(T)` (team key) | Only rows with `team_id = T`. **Shared rows are excluded.** (`store.rs:1336-1358`) |
| `None` (`Global`) | **Every account**, team-owned or shared (`store.rs:1326-1334`, `:1359-1371`) |

`ConnectionScope` is a type, not a nullable filter: the instance/tenant difference is a
security boundary (`store.rs:1336-1341`). `/api/models/providers` inherits it: `scope` is
`global` only for a shared account the caller can see, `team` or `none` otherwise
(`models.rs:848-859`, customs `:897-903`). Treating the global-key view as a leak would remove
routing capacity; it is a security decision, not a UI change.

## Target design

### 1. Provider rows inside Connected

One `DataTable`; a `{kind:"provider"}` row precedes the first account of each provider or
custom endpoint with **more than one** account (`providers.tsx:257-275`); single-account ones
stay flat.

| Column | Content · source |
|---|---|
| provider | identity (`code` badge, `N accounts · N enabled · N in backoff`) · provider catalog + `GET /api/connections` |
| owner | priority tiers `p0 ×2 · p1 ×1` · `Connection.priority` |
| account | spend, current quota window summed, admin only · `GET /api/accounts/quotas` (`quotas.rs:36-53`) |
| auth | worst subscription window used %, admin only · `GET /api/accounts/usage` (`quotas.rs:97-105`) |

Folded from queries the page already makes (`providers.tsx:158-255`). Subscription windows are
**never averaged**: the header shows the *worst* used percent and how many accounts report;
accounts keep their own, because routing reserves per connection (`provider_usage.rs:122-149`).

### 2. Three states, displayed distinctly

| State | Definition |
|---|---|
| **enabled** | operator intent (`is_active`), persisted |
| **eligible** | enabled + in key scope + not quota-exhausted + breaker closed + (when the reserve filter fires) below reserve |
| **in use** | the account that served a recent request (`usage.connection_id`); derived, never stored |

Aggregate provider state (`providers.tsx:218-252`) is display-only and never writes
`is_active`, opens breakers or alters quotas:

| healthy | ≥1 enabled account, none of them flagged |
| degraded | ≥1 enabled account, ≥1 enabled account flagged (`backoff_level > 0`, quota exhausted, or worst subscription window ≥ 90 %) |
| down | no enabled account, or every one flagged |
| unavailable | zero rows → never a routable row |

### 3. Routing semantics — unchanged order

`scope` is the only account filter before priority; every later stage is per
`(provider, model)` or per connection.

| Stage | Behavior |
|---|---|
| 1. Key scope | team key → that team's rows; no team → all rows (`planner.rs:375-377`) |
| 2. Enabled | `is_active = false` dropped (`planner.rs:409`) |
| 3. Resolve | alias hop, combo; `provider/model` pins one provider; bare id fans out (`planner.rs:700-811`) |
| 4. Policy | request policy and key `allowed_models` filter expanded pairs (`planner.rs:446-488`) |
| 5. Priority, then tier strategy | equal priorities form a tier: `priority` rotates, `weighted` WRR, `least_latency` EWMA, `cheapest` price (`planner.rs:511-542`) |
| 6. Operator quota | exhausted removed; all exhausted → `429 quota_exhausted` + `Retry-After` (`planner.rs:582-621`) |
| 7. Subscription reserve | default `90`, `0` disables; drop at/above only if someone is below (`planner.rs:625-650`) |
| 8. Headroom | re-sorted by `max(quota utilization, used%/100)` asc (`planner.rs:651-653`) |
| 9. Canary | promoted inside each tier (`planner.rs:655-663`) |
| 10. Client policy | `provider.order` reorders in-tier; `allow_fallbacks=false` keeps tier 1 (`:665-675`) |
| 11. Dispatch | skip open breaker / cooldown; retry same candidate, then next (`dispatch.rs:1-31`) |

Facts to keep: priority dominates weight (`planner.rs:338-340`, `config.rs:1435-1446`);
conversation affinity pins the equal-priority choice (`planner.rs:330-340`); next-account
fallback is automatic (`dispatch.rs:1-31`); breaker windows are 30 s (5xx) / 300 s (auth),
429 cooldowns 30 s → 1 h (`breaker.rs:24-35`).

### 4. Additive: availability reason and explain

No reordering. Each account gets an explicit reason why it will not be tried:

| Reason | When (`planner.rs`) |
|---|---|
| `disabled` | `is_active = false` |
| `out_of_scope` | team key, row outside that team (`:375-377`) |
| `quota_exhausted` | operator quota reached (`:588-621`) |
| `reserve` | window ≥ reserve while another account is below (`:625-650`) |
| `breaker_open` | connection breaker open (`breaker.rs`, not exposed) |
| `model_cooldown` | 429 cooldown for this `(connection, model)` (`breaker.rs:59-70`, not exposed) |
| `eligible` | none of the above |

Reuse `/api/route/explain` as the "Routing…" preview (it runs the real planner,
`explain.rs:55-107`) and add `excluded` so it explains gaps, not just survivors. Do **not** add
a provider-level knob; provider-wide changes are batch edits of per-account fields.

### 5. Duplicate identities

`connection id` stays the only identity; make nothing else unique. Detection warns first:

| Auth kind | Identity | Detection |
|---|---|---|
| OAuth (`claude`, `codex`, `github`, `google-antigravity`) | `email`, unverified (`flows.rs:661-670`) | **soft** `(provider, email)` match; show the row, require confirm |
| API key / operator-defined | key sealed in `data`, usually no email | `SHA-256(salt \|\| provider \| secret)`, 16-byte hex in nullable `connection_fingerprint`, looked up before insert |
| none stable (secret unreadable after sealing) | — | skip detection `[INFERENCE]` |

Never compare plaintext or store the key outside the sealed blob. `(provider, email)` must not
be unique: one email can own several workspaces.

### 6. Wire changes (S1/S2 only)

Grouping needs **no new endpoint**: `provider` is the group key and every source is already
fetched. `GET /api/connections` keeps `{data: [...]}` and `ConnectionView`
(`routes/connections.rs:27-84`).

| Change | Shape | Safety |
|---|---|---|
| `health` on `ConnectionView` | `{"breaker":"closed\|open\|half_open","retry_after_secs":n?,"model_cooldowns":n}` | new optional object; needs a read-only snapshot beside `breaker_arc()` (`proxy.rs:4537`) |
| `/api/route/explain` `excluded` | `[{"connection_id","reason"}]` | additive |
| `connection_fingerprint` on `ConnectionView` | `string?` (never the key) | additive, admin only |
| Optional `groups` sibling on `GET /api/connections` | `[{provider, accounts, enabled, eligible, spend, subscription}]` | additive; only if a second surface needs it, never for S0 |

New keys use `snake_case`. Do **not** add `group: <provider>` to a connection or change
`/api/models/providers` (already returns `has_connection`, `scope`, `code`, `display`);
`GET /api/connections?provider=` already narrows server-side (`routes/connections.rs:186-206`).

### 7. UI

All inside the existing Connected card.

**Provider header (S0, shipped)** — §1 columns, tinted as a divider (`providers.tsx:410-580`,
`:860-864`). **Account rows** — unchanged, indented (`providers.tsx:457-483`); `Limits…` and
`Routing…` stay row-menu entries (`providers.tsx:3241-3248`).

**DataTable** — no colSpan support (`ui/src/components/table.tsx:195-216`), so the summary is
spread across columns; `groupBy` (default off) is the eventual shape. Search filters accounts
first, dropping empty groups.

### 8. Must not change / non-goals

Must not change: the planner input (no group id in `Candidate`, `Plan`, quotas or breaker
keys); per-account sealed credentials (`store.rs:1234-1250`); team scoping; per-connection and
per-`(connection, model)` breaker/quota accounting; priority semantics and weight-as-a-share;
one provider row, accounts as rows.

Non-goals: cross-provider failover (that is a combo); credential rotation, key pooling,
auto-creating accounts on exhaustion; billing consolidation; a stored provider-level disable
flag or a provider-level quota.

## Known limitations

No collapse (S0 is expanded by default); the summary is spread across columns (no colSpan
support); live breaker state has no API (only the probe `backoff_level`); a custom endpoint
groups by `custom:<name>`, a separate header from its protocol provider; a reader sees no spend
or subscription figure; no "Routing…" preview or "Add account" action on the header yet.

## Slices

| Slice | Work | Wire | Proof | State |
|---|---|---|---|---|
| **S0** | Header rows (multi-account only) + expanded account rows + counts/state/tiers/spend/subscription from existing reads; no routing change | none | `cd ui && bun run build`; page check with 3 accounts of one provider | **Shipped** (`providers.tsx:85-95`, `:150-275`, `:410-580`); build green |
| **S1** | `health` breaker snapshot; availability reason; explain `excluded` | additive | `cargo test -p rrouter-router`; `cargo test -p rrouter-gateway` | Design |
| **S2** | duplicate fingerprint + soft warning on add | additive column | `cargo test -p rrouter-store` | Design |

The **S0/S1 boundary is the wire**: S0 adds no field; S1 is the first to change a payload.
Tests to add with S1/S2 (none exist yet `[INFERENCE]`): `group_counts_only_visible_accounts`,
`breaker_state_is_per_account_not_per_provider`, `exhausted_account_is_excluded_with_reason`,
`duplicate_api_key_is_detected_without_storing_plaintext`.

## Decisions (all questions closed)

| # | Question | Decision |
|---|---|---|
| Q1 | Does S0 need a `DataTable` change? | **No** — one full-width cell per column. |
| Q2 | Reader has no spend/subscription (admin-only endpoints). | **Hide for readers** (`providers.tsx:524-526`). |
| Q3 | `connection_fingerprint` audience. | **Admin only**; ownership is the `owner` column. |
| Q4 | Fix `docs/ROUTING.md` "across groups"? | **Yes, wording only** (`planner.rs:503-528` is already per provider); not this slice's file. |
| Q5 | Breaker snapshot for the gateway. | **Read-only snapshot beside `breaker_arc()`** (`proxy.rs:4537`) in S1. |

## Implementation status

Verified 2026-11-18. **Shipped:** the provider header row (counts, tiers, state, spend,
subscription, from existing queries; `cd ui && bun run build` → `✓ built in 497ms`), and the
separately landed provider-codes dialog, limits-as-row-action and `advanced` endpoint + code
(`providers.tsx:763-773`, `:3243`, `:2305-2306`). **Design only:** S1 (`health` absent from
`ConnectionView`, `routes/connections.rs:27-54`; no `excluded` in `explain.rs:81-107`) and S2
(no `connection_fingerprint` column in `migrations.rs`).

Limitations: see § Known limitations. Every anchor above was re-read at this revision of the
working tree; `providers.tsx` anchors move with that file.
