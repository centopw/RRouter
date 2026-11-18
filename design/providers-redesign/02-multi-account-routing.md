# One provider, N accounts: grouping and routing semantics

## Decision

Grouping is a **read projection over `connections`**, not a new routing entity:

- A provider row is a header over its accounts; the accounts remain real rows.
- The planner keeps receiving a flat `Vec<Connection>`; it never learns that a group exists.
- Default view is expanded, so each account stays individually visible and actionable
  (required by the merged Connected+limits work, `#5`). Collapse is an optional view
  preference, not a data model.
- No preferred-provider picker, no cross-provider collapsing (decision memory; `#5` D).

No routing algorithm change is required to support N accounts: priority/weight/quota/
breaker logic is already per connection. The missing pieces are visibility (aggregate row,
health that reflects live routing) and duplicate identity for subscriptions.

## 1. Current model

### What an account is

A `connections` row is one credential set for one provider. `id` is the identity; `name`
and `email` are dashboard labels only.

| Field | Meaning | Evidence |
|---|---|---|
| `id` | UUID primary key. Nothing else is unique. | `crates/rrouter-store/src/migrations.rs:744-759` |
| `provider` | Routable provider id (compiled or operator-defined); unconstrained `TEXT`, no FK. | `migrations.rs:746`, `crates/rrouter-store/src/store.rs:35-37` |
| `auth_type` | `oauth` / `api_key` / none. | `store.rs:38` |
| `name`, `email` | Labels. `email` is filled from `credentials.account_label`; API-key accounts usually have neither. | `store.rs:39-40`, `store.rs:1256-1268`, `crates/rrouter-auth/src/credentials.rs:35-37` |
| `priority` | Lower tried first. Default `0`. | `store.rs:41`, `migrations.rs:750` |
| `weight` | Traffic share `1..=1000`, default `100`; primary input of `weighted`, tie-break in `least_latency`. | `store.rs:42-44`, `migrations.rs:678`, `store.rs:4057-4058`, `planner.rs:1023-1036` |
| `team_id` | `NULL` = shared/instance-wide; otherwise owned by one team. | `store.rs:54-57`, `migrations.rs:353-355` |
| `owner_user_id` | Attribution only; never an access or routing input. | `store.rs:58-61` |
| `backoff_level`, `last_error*` | Persisted operator-probe state, not live breaker state (see gap G4). | `store.rs:46-52` |

One provider can already own N accounts: there is **no** unique constraint on
`(provider, team_id)`, `(provider, email)`, or credentials (`migrations.rs:744-760`;
`migrations.rs:1360-1391` asserts only the id is unique). Every create inserts a new UUID
row (`store.rs:1234-1279`; OAuth path `crates/rrouter-gateway/src/routes/oauth.rs:183`).

Listing is already provider-grouped in storage: `ORDER BY provider ASC, priority ASC,
created_at ASC` for the unscoped read, and `priority ASC, created_at ASC` for
`?provider=` (`store.rs:1342-1375`).

### What “several accounts” already means

`Candidate` is `(provider, model, connection)`; a provider/model pair expands to one
candidate per active connection, which is what makes account-level fallback possible
(`crates/rrouter-router/src/candidate.rs:9-23`). So the routing primitive for N accounts
already exists; only the page and the aggregate view are flat.

### Concrete gaps

| # | Gap | Evidence |
|---|---|---|
| G1 | Flat table: one row per account, provider name repeated; no per-provider account count, spend, or health. The card header only counts distinct providers and total accounts. | `ui/src/pages/providers.tsx:555-579` (`endpointCount` / `accounts.length`) |
| G2 | Per-account operator limits are invisible at a glance: quotas are admin-only, fetched lazily, and shown in a separate card while the redesign moves the editor behind the row menu. | `providers.tsx:100-104`, `providers.tsx:703-716`, `providers.tsx:2761` |
| G3 | Subscription windows live in a third card keyed by `connection_id`, not under the provider. | `providers.tsx:585-625`, `crates/rrouter-gateway/src/provider_usage.rs:41-53` |
| G4 | No API exposes live circuit-breaker or model-cooldown state. `backoff_level` is written only by the admin `Test` probe, so a failing live account can still render `active`. | `crates/rrouter-gateway/src/routes/connections.rs:943-963` is the only writer; no breaker read in `crates/rrouter-gateway/src/routes/health.rs:1-28` |
| G5 | No duplicate detection. Re-running OAuth or pasting the same key twice creates a second row; nothing warns. | `oauth.rs:175-183`, `store.rs:1234-1279` |
| G6 | Routing order across accounts is not explainable in the page. `/api/route/explain` exists but is not linked from an account/provider row and omits *why* accounts were dropped. | `crates/rrouter-gateway/src/routes/explain.rs:26-110` |
| G7 | Docs promise priority/strategy and team scope, but nothing documents multiple accounts per provider or provider-level aggregates. | `docs/ROUTING.md:34-53`, `docs/PROVIDERS.md:3` |

## 2. Proposed provider-centric grouping

### Row anatomy

A collapsed/expanded provider header grouped **within** the Connected card:

| Element | Content | Source |
|---|---|---|
| Identity | provider icon + display name + shorthand `code` (e.g. `ocg`) | `/api/models/providers` (`crates/rrouter-gateway/src/routes/models.rs:842-955`); page already loads it (`providers.tsx:74`) |
| Accounts | `3 accounts · 2 enabled · 1 cooling` | `GET /api/connections` rows, grouped by `provider` |
| Priority tiers | `p0 ×2 · p1 ×1` | `ConnectionView.priority` (`routes/connections.rs:33`) |
| Spend | `$4.12 · 1.2M tok · 840 req` for the current quota window | `GET /api/accounts/quotas` (`routes/quotas.rs:58-95`); admin only |
| Subscription | worst window `82%` · resets in `2h` · `3/3 reporting` | `GET /api/accounts/usage` (`routes/quotas.rs:100-105`, `provider_usage.rs:753-765`); admin only |
| Over-limit | `1 over quota`, `0 cooling` badges | quota `exhausted` + (proposed) breaker snapshot |
| State | healthy / degraded / down | derived, definition below |

Expand renders the existing account rows unchanged (provider cell, owner, auth, priority,
weight, state, actions), indented under the header. This satisfies “each provider is its
own entry; accounts stay separate rows” while making the aggregate glanceable.

### Aggregate states

| State | Rule |
|---|---|
| healthy | ≥1 account enabled and eligible, no eligible account over quota/reserve, no breaker open |
| degraded | ≥1 eligible account, but ≥1 account open, over quota, or above the subscription reserve |
| down | every account disabled, over quota, or breaker-open |
| unavailable | provider has zero rows (render only under a “not connected” section, not as a routable row) |

“Eligible” is defined in §4. Aggregate state is display-only; it must never write
`is_active`, open breakers, or alter quotas.

### Queries

All aggregates reuse existing reads; **no new endpoint is required for grouping**:

- rows/counts/priorities: one `GET /api/connections` (already fetched).
- spend: `/api/accounts/quotas` (already fetched when the limits dialog opens; for the
  header fetch it once per page, not per group).
- subscription: `/api/accounts/usage` (already fetched by `subscriptions`).
- pings: `/api/quota-pings` (`routes/quota_pings.rs:78-130`) for “next ping”.
- live breaker: missing (G4) → proposed additive `health` field, §5.

Do not add per-provider aggregate SQL: grouping in the handler/page over the already
ordered list is one pass and cannot drift from the planner's input.

## 3. Routing semantics for N accounts

### What exists today, in execution order

| Stage | Current behavior | Evidence |
|---|---|---|
| 1. Key scope | `key.team_id = Some(T)` → only rows with `team_id = T`. Shared rows are excluded for a team key. `None` → all rows, **including team-owned rows**. | `planner.rs:375-377`, `store.rs:68-85`, `store.rs:1342-1372` |
| 2. Enabled | rows with `is_active = false` are dropped. | `planner.rs:409` |
| 3. Resolve | explicit `provider/model`/`code/model` hits one provider; a bare id fans out to every active provider. | `planner.rs:415-449` |
| 4. Priority | within one `(provider, model)` block: sort by `priority` ascending; equal priorities form a tier. | `planner.rs:511-519` |
| 5. Tier strategy | `priority` → rotate by plan offset; `weighted` → smooth WRR on `weight`; `least_latency` → EWMA asc; `cheapest` → price, then a global price sort. | `planner.rs:520-528`, `planner.rs:949-1010`, `planner.rs:1040-1078` |
| 6. Operator quota | exhausted accounts are removed; if all planned candidates are exhausted → `429 quota_exhausted` + `Retry-After`. | `planner.rs:582-620` |
| 7. Subscription reserve | default `90`; `0` disables. If at least one candidate is below reserve, drop those at/above it; if all are at/above, keep all. | `planner.rs:623-646`, `crates/rrouter-gateway/src/config.rs:70`, `config.rs:1218-1233` |
| 8. Headroom order | each equal-priority run re-sorted by `max(operator quota utilization, subscription used%/100)`, ascending. | `planner.rs:647-650`, `planner.rs:870-900` |
| 9. Canary | provider promoted inside each equal-priority run only. | `planner.rs:655-662` |
| 10. Client policy | `provider.order` stable-reorders within priority; `allow_fallbacks=false` keeps only the first priority tier. | `planner.rs:665-675`, `planner.rs:913-928` |
| 11. Dispatch | skip breaker-open account or `(connection, model)` cooldown; retry the same candidate, then move to the next candidate automatically. | `crates/rrouter-router/src/dispatch.rs:1-31`, `dispatch.rs:275-330` |
| 12. Breaker record | 401/403/5xx/transport → connection-wide open (30 s / 300 s); 429 → `(connection, model)` cooldown 30 s, capped 1 h. Success closes. | `crates/rrouter-router/src/breaker.rs:1-31`, `breaker.rs:166-235` |

Answers to the brief's specific questions:

- **Order is `priority` ascending, not `weight` descending.** Weight is a proportional
  share inside one tier and only under `RROUTER_STRATEGY=weighted`
  (`planner.rs:136-149`, `config.rs:1435-1438`). It never promotes across tiers; the
  planner test pins this (`planner.rs:1454-1466`).
- **Conversation affinity** pins equal-priority account selection for a conversation by
  using a stable offset instead of the planner cursor (`planner.rs:331-340`), so multiple
  Claude OAuth accounts do not cold-start the KV cache on every turn.
- **Fallback to the next account is automatic** after a candidate's retries are spent
  (`dispatch.rs:1-31`), within the request's send budget.
- **Cross-provider priority is not global.** Candidates are emitted provider block by
  provider block, each internally priority-sorted; `priority` dominates only inside one
  `(provider, model)` target. The only global re-sort happens under `cheapest`
  (`planner.rs:1040-1078`). The docs' “Priority always dominates across groups”
  (`docs/ROUTING.md:46-47`) should be read per provider, or corrected.
- **Team filtering is asymmetric, and the brief's premise is reversed.** A team-owned
  account is *not* filtered out for a shared/global key: `ConnectionScope::Global` is
  documented as “every account, team-owned or shared … routing of keys that belong to no
  team” (`store.rs:68-77`). The filter removes shared rows for a *team* key, and other
  teams' rows for that team's key (`store.rs:79-85`, `store.rs:1355-1360`). Preserve this
  unless isolation is explicitly re-decided; treating it as a bug would silently remove
  routing capacity.

### Proposed (additive, no reordering)

1. Keep the order exactly as above; grouping must not change plans.
2. Add an explicit **per-account availability reason** so the group row and Routing dialog
   can say why an account will not be tried:

| Reason | When | Source today |
|---|---|---|
| `disabled` | `is_active = false` | `planner.rs:409` |
| `out_of_scope` | team key, row not in that team | `planner.rs:375-377` |
| `quota_exhausted` | operator quota reached | `planner.rs:592-620` |
| `reserve` | subscription window ≥ reserve while another account is below | `planner.rs:623-646` |
| `breaker_open` | connection breaker open | `breaker.rs` (not exposed) |
| `model_cooldown` | 429 cooldown for this `(connection, model)` | `breaker.rs:1-22` (not exposed) |
| `eligible` | none of the above | — |

3. Reuse `/api/route/explain` as the “Routing…” preview: it already runs the real planner
   and returns ordered `connection_id`/`priority`/subscription fields
   (`explain.rs:54-110`). Add `excluded: [{connection_id, reason}]` (additive) so the
   dialog can explain gaps rather than just list survivors.
4. Do **not** add a provider-level routing knob. Provider-wide changes are batch edits of
   the existing per-account `priority`/`weight`/`is_active`, written account by account.

## 4. Multiple subscriptions of the same provider

### Identity

| Auth kind | Native account identity | Stored today | Duplicate detection |
|---|---|---|---|
| OAuth (`claude`, `codex`, `github`, `google-antigravity`) | provider-side account (email / login) | `email` = `credentials.account_label`; `name` defaults to that label | none; two sign-ins create two rows (`oauth.rs:175-183`) |
| API key (`opencode-go`, `openrouter`, `xai`, …) | the key itself (or a provider-side key/subscription id) | key sealed in `data`; `email` usually `NULL` | none |
| Operator-defined provider | provider id + credential | same as API key | none |

A connection id remains the only identity (`migrations.rs:1360-1391`). Do not make
`(provider, email)` unique: one email can own several workspaces/subscriptions, and
API-key accounts have no email.

### Duplicate detection (proposed, warn first)

- **API key**: compute a non-reversible fingerprint at create/update time —
  `SHA-256(instance_salt || provider || secret)`, store first 16 bytes hex in a new
  nullable `connection_fingerprint` column. A non-unique index + pre-insert lookup gives
  “this key is already connected as …, add anyway?”. Never store the raw key outside the
  encrypted blob, never compare plaintext.
- **OAuth**: `(provider, email)` is a **soft** match only, since email is an unverified
  label (`crates/rrouter-auth/src/flows.rs:960-984`) and may be absent. Show
  “already connected” with the existing row, require an explicit confirm.
- **Fingerprint unavailable** (provider gives no stable id and the secret is not readable
  after sealing): skip detection rather than guess `[INFERENCE]`.

### Per-subscription limits display

Each account keeps its own windows; never average them. The provider header shows the
*worst* used percent and the *earliest* reset among accounts, plus `n reporting`; the
expanded rows show each account's windows (existing `SubscriptionRow`). They must not be
merged into one “provider quota”, because routing reserves per connection
(`provider_usage.rs:126-145`, `planner.rs:623-650`).

### What “active” means

Three separate states, displayed distinctly:

1. **enabled** — operator intent (`is_active`), persisted.
2. **eligible** — enabled + in key scope + not quota-exhausted + breaker closed + (when the
   reserve filter fires) below reserve. This is what the group header counts.
3. **in use** — the account that served a recent request (`usage.connection_id`), optional
   badge; derived, never stored on the connection.

## 5. Wire/API changes

**Grouping needs no new field and no new endpoint**: `provider` is the group key, and the
three aggregate sources are already fetched by the page. Keep `GET /api/connections`
`{ data: [...] }` and every `ConnectionView` field exactly as-is
(`routes/connections.rs:27-84`).

Additive changes, in value order:

| Change | Shape | Why | Compatibility |
|---|---|---|---|
| `health` on `ConnectionView` | `{ "breaker": "closed\|open\|half_open", "retry_after_secs": n?, "model_cooldowns": n }` | G4; makes aggregate health and availability reasons real | New optional object; old clients ignore it. Requires a `Breaker::snapshot()` read and access from `AppState` (already reachable: `proxy.rs:4537` exposes `breaker_arc()`) |
| `/api/route/explain` `excluded` | `[{ "connection_id", "reason" }]` | explains dropped accounts; reasons in §3 | additive |
| `connection_fingerprint` on `ConnectionView` | `string?` (never the key) | duplicate detection UI | additive; admin-only? Keep it visible only to admin-scope readers |
| Optional `groups` sibling on `GET /api/connections` | `[{ provider, accounts, enabled, eligible, spend, subscription }]` | only if a second surface needs the aggregate without the three reads | additive; omit admin-only spend/subscription for non-admin. Not required for the smallest slice |

Do **not** add a `group: <provider>` field: `provider` already is that value, and duplicating
it invites drift. Do **not** change `/api/models/providers`; it already returns
`has_connection`, `scope`, `code`, `display` per provider (`routes/models.rs:842-955`).

`GET /api/connections?provider=` already narrows server-side (`routes/connections.rs:186-206`);
reuse it if grouping becomes paginated.

## 6. UI impact

All inside the existing Connected card (`providers.tsx:553-582`), assuming the merged
limits card from `#5`.

**Provider header row**

- icon, display name, `code` badge, accounts/enabled/eligible counts, priority tiers,
  aggregate state, spend, worst subscription window.
- actions: `Routing…` (preview only, via explain), `Add account` (open the existing add
  flow preselected to this provider), expand/collapse.
- Admin-only aggregates are hidden for readers, matching the existing gating
  (`providers.tsx:76-104`).

**Account rows (expanded, default)**

- unchanged columns and actions (`providers.tsx:252-330`, menu `providers.tsx:2657-2775`).
- `Limits…` stays the per-account operator-quota dialog (`providers.tsx:2761`,
  `providers.tsx:2485-2560`); the group header must not become a second quota editor.
- state cell gains the availability reason (quota / reserve / breaker / cooldown) from
  §3 instead of only the persisted `backoff_level` badge.

**Routing dialog**

- keep priority/weight editing per account (`providers.tsx:890-990`).
- add a “plan preview” for a sample `code/model`: ordered account list from
  `/api/route/explain`, with each excluded account and its reason.
- copy must state: weight is a share inside a priority tier; it only applies when the
  gateway strategy is `weighted`.

**DataTable**

No group-header support exists (`ui/src/components/table.tsx:195-216`). One shared
component change is required: an optional `groupBy`/`renderGroupHeader` prop (default off)
so groups render as rows above their accounts. Do not fork the table or build a second
list. If touching the shared table is not allowed in the current slice, render a provider
header block + a per-provider `DataTable`; that is acceptable but repeats column headers
and is the fallback, not the target.

**Search/filter**

Grouping composes with the existing search and team filter: filter accounts first, then
drop empty groups; a provider header's counts describe the filtered set. `#providers-connected`
must keep working; deep links with `?team_id=` continue to pass through.

## 7. Risks and non-goals

### Must not change

- **Planner input**: still `Vec<Connection>` from `list_connections_scoped`. No group id in
  `Candidate`, `Plan`, quotas, or breaker keys.
- **Per-account credentials**: each `data` blob is sealed with its own connection id
  (`store.rs:1234-1250`). A “provider credential” shared by accounts would break
  revocation and team ownership.
- **Team scoping**: team keys never see/spend shared accounts; global keys keep today's
  all-rows semantics (§3). Any change here is a security decision, not a UI change.
- **Breaker/quota accounting**: per connection and per `(connection, model)`. A provider
  header must not open/close breakers or write quotas.
- **Priority semantics**: no new ordering; weight remains a share, not a sort key.
- **One row per provider, accounts as rows** (`#5` D, decision memory).

### Non-goals

- Cross-provider account failover — that is a combo (`docs/ROUTING.md:6-21`).
- Automatic credential rotation, key pooling, or creating accounts on quota exhaustion.
- Billing consolidation across subscriptions; spend stays a sum of reported usage.
- Provider-level disable as a new stored flag; if wanted, it is an explicit batch write to
  each account's `is_active`.
- A provider-level quota that the planner reads.

### Smallest shippable slice

| Slice | Work | Wire | Proof |
|---|---|---|---|
| S0 | group headers + expanded account rows + counts/state/spend/subscription from existing reads; default expanded; no routing change | none | `cd ui && bun run build`; page check with 3 accounts of one provider |
| S1 | `health` breaker snapshot on `ConnectionView`; availability reason; explain `excluded` | additive | `cargo test -p rrouter-router` (breaker/planner), `cargo test -p rrouter-gateway` (view serialization) |
| S2 | duplicate fingerprint + soft warning on add | additive column | `cargo test -p rrouter-store` |

Named tests for S1/S2 (future): `group_counts_only_visible_accounts`,
`breaker_state_is_per_account_not_per_provider`,
`exhausted_account_is_excluded_with_reason`,
`duplicate_api_key_is_detected_without_storing_plaintext`,
`weight_does_not_reorder_priority_tiers`.

## Evidence notes

- `ui/src/pages/providers.tsx` is being edited concurrently by the merged-card worker;
  line anchors above are from the current working tree and will shift. Symbols
  `ConnectionMenu`, `RoutingModal`, `QuotaModal`, `ProviderCodesCard` are stable anchors.
- Prior vocabulary for “provider vs account” exists in `design/ROUTING.md:476-563` and its
  proposed `provider_endpoints` table (`design/ROUTING.md:565-585`). That table was not
  implemented; the shipped custom-provider row is `provider_profiles`
  (`crates/rrouter-store/src/migrations.rs:36-43`). This design therefore groups on the
  existing `connections.provider` and adds no endpoint table.
- “Three OpenCode Go rows, Claude + XAI247 + OpenRouter + Qoder” is the operator's report,
  not a repository fact; the code supports it (§1, no uniqueness).
