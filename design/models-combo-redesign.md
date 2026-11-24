# `/models` combo redesign — models, aliases, combos as first-class rows

Assembled from `design/models-combo-redesign/01-models-ux.md`, `01b-group-binding.md`,
`02-metadata-engine.md`. It states the shipped design; every status claim in
**Implementation status** was re-run against the current tree, and every `file:line` anchor
below was re-checked against it. The section files keep the pre-change analysis.

## Executive summary

- One table, one preset board. Alias and combo rows join the registry rows at
  `rrouter/<name>`; a **Kind** filter narrows them. No second section, no view switcher.
- The reported defects were independent faults: no rendered row for names (group assignment
  was stored but invisible), `by_id` first-wins resolution with the provider segment discarded
  (the 256k), and `left.cost = None` in the only fold (missing price).
- Combos are **derived, never sampled**: per member resolve every `(provider, id)` a request
  can land on, then fold with commutative operators into `members` + `aggregate` (additive
  wire). Shipped in `crates/rrouter-gateway/src/combo_meta.rs`.
- `/api/models` stays registry-only. A combo's UI identity is `rrouter/<name>`; a group is
  storage metadata for a combo, never a routable prefix. **Accepted limitation:** a combo's
  callable id stays its bare name (or `combo:<name>`); no `group/<name>` form exists.
- A name is **one identity namespace** with **alias > combo** precedence, enforced at both
  create paths (§ Decision: one name, one owner).

## Defect traceability

Every reported symptom, the root cause it came from, and the change that removed it. RC ids
follow `01b` §1 and `02` §2; **State** says whether the fix is in the tree. A symptom that is
now a *limitation* is marked so, never as fixed.

| # | Symptom | Root cause (citation) | Change that shipped | State |
|---|---|---|---|---|
| D1 | Combo cannot be assigned to a group | **RC1**: the row set was `modelRows + staleRows`; stored `rrouter/<name>` refs counted as "known" and `/api/models` never lists combos, so no combo row existed and `AliasRow`/`ComboRow` were never instantiated. | One row union `rowsFor(...)` (`ui/src/pages/models/unified-rows.ts:97-175`), consumed by `models.tsx:248-266`; filters, board groups, counts, group select all key on the row ref. | Fixed |
| D2 | Same | **RC2**: only two assignment surfaces (Group Manager, per-row select); the combo editor had no preset/group reference. | Add→Combo drawer on `/models` with a **preset group** select (`ui/src/pages/models/preset-header.tsx:280-370`, mounted `ui/src/pages/models.tsx:958-976`); `layoutDraft.assignGroup("rrouter/<name>", group)` after create (`models.tsx:456-465`). | Fixed |
| D3 | Same, non-default preset only | **RC3**: edits go to the active preset; `/v1/models` reads the default one (`crates/rrouter-gateway/src/routes/models.rs:51-57`), as does the group resolver (`crates/rrouter-router/src/planner.rs:759-763`). | Not a defect of the row set. Accepted limitation: `/v1` group parity is guaranteed for the **default** preset only; the Group Manager's `/v1` toggle is the **default-preset** control (`groups.tsx` `GroupHeading` badge, `groups.tsx:33-37`). | Limitation, documented |
| D4 | Same | **RC5**: alias and combo shared one `rrouter/` namespace with no identity rule. | One owner per name, **alias > combo**: both create paths refuse the other kind's name (400); discovery lists the alias once and the combo stays reachable as `combo:<name>`; `rowsFor` renders one row per ref. See *Decision*. | Fixed |
| D5 | 1M model shows 256k | **RC3-meta**: `by_id` was first-wins `or_insert_with`; a `code/id` shorthand only *validated* the provider segment and then looked the id up globally, while a bare id fans out to every provider. | `RegistryIndex::resolve_member` (`combo_meta.rs:386-453`): a slashed entry pins its provider (id or shorthand), a bare id yields the whole target set, a dated id resolves by version core. | Fixed |
| D6 | 1M model shows 256k (ceilings) | The old fold took `min` only and returned `None` if any member was unknown or unlisted, and `model_json` returned early with no limits at all. | Two bounds: `guaranteed = min`, `up to = max` (`combo_meta.rs:590-609`); `null` only when a target or member is genuinely unknown. `min` still feeds the legacy flat fields. | Fixed |
| D7 | Combo price missing | `left.cost = None` in the old fold; `/v1/models` emitted no `cost`; `/api/combos` had no price field. | `aggregate.pricing` = `{min,max}` per cost dimension over known target costs + `known`/`total` (`combo_meta.rs:493-517`); emitted on `/api/combos` and `/v1/models/{id}`, per-target `members[].targets[].cost`. Range, not average, not member 1. | Fixed, wire; no UI read (§ Status) |
| D8 | Capabilities wrong for a mixed combo | `Capabilities::intersect` was a plain AND applied pairwise, so a vision primary with a text backup advertised `vision:false`; a retired last-resort retired the chain. | `guaranteed_capabilities` (AND) and `supported_capabilities` (OR), both nullable (`combo_meta.rs:519-529`); `deprecated_members` count + `all_deprecated` instead of retiring. Legacy flat `capabilities.*` keep the AND. | Fixed, wire; no UI read (§ Status) |
| D9 | Combo named like a model is missing from `/v1/models` | Combo/alias entries were skipped when the name was a known model id, but routing prefers the alias, then the combo (`planner.rs:702-710`). | `/v1/models` advertises the combo under that id (`routes/models.rs:91-113`); registry rows stay reachable as `code/id`. An alias outranks a combo there too. | Fixed |
| D10 | Grouped combo is not `group/<name>` callable | `provider == "rrouter"` takes the bare-name branch (`routes/models.rs:306-307`); the group resolver needs `group/<provider>/<id>` with a real provider (`planner.rs:754-768`); `rrouter` is not a provider (`crates/rrouter-gateway/src/provider_catalog.rs:283`). | **Accepted limitation, decided (Q3 closed).** No group/provider prefix for combos: the callable id stays the bare combo name; `combo:<name>` stays the explicit form (`planner.rs:132`, `:709`, `:735`). Grouping a combo scopes discovery and grants only. | Limitation, decided |
| D11 | One provider with many accounts unreadable | The providers page was flat: no per-provider counts, tiers, spend or health, and the page could not say which accounts were in backoff. | Providers S0 provider header row (see `design/providers-redesign.md` § Target design, § Status). Not a `/models` change. | Fixed (S0) |

## Decision: one name, one owner (alias > combo)

A name is **one identity namespace**, shared by aliases and combos, and an **alias outranks a
combo** because routing resolves the alias first: a key's model mapping, then one alias hop,
then a combo by bare name or `combo:` prefix, then `provider/model`
(`crates/rrouter-router/src/planner.rs:700-740`).

Two enforcement points, one per create path:

| Path | Rule | Citation |
|---|---|---|
| `POST /api/combos` | 400 when the name is an alias; re-posting a combo's own name stays an upsert | `crates/rrouter-gateway/src/routes/combos.rs:61-69` |
| `PUT/POST /api/aliases` | 400 when a *fresh* alias would take a combo's name; updating an existing alias is unaffected | `crates/rrouter-gateway/src/routes/models.rs:709-726` |

Pre-existing collisions (rows written before the validation, or straight into the store)
degrade predictably: `/v1/models` advertises the **alias** once and the combo stays reachable
as `combo:<name>` (`routes/models.rs:97-105`), and the board renders one row per `rrouter/`
ref, alias first (`unified-rows.ts:122-149`).

Why not "the combo wins": routing would have to change order, and every existing alias would
silently start pointing at a combo — a routing contract change for no user-visible gain. Why
not a separate namespace: the callable name space is flat, so `rrouter/<name>` would still be
ambiguous on the board and in `/v1/models`.

Remaining edge: the per-row group select lists free-form groups only, so a name whose stored
group is a provider bucket shows *Ungrouped* while its section is that bucket. Cosmetic; the
assignment is intact (`layout-draft.ts:163-196`).

## Target design (shipped)

### 1. Row set

Rows are `rowsFor({models, aliases, combos, entries, presetAware, autoInclude})`
(`ui/src/pages/models/unified-rows.ts:97-175`):

1. registry `ModelRow`s from `/api/models`;
2. `AliasRow`s at `rrouter/<alias>`, `status = statusOfName(...)`;
3. `ComboRow`s at `rrouter/<name>`, same status rule, one per ref (alias first);
4. preset entries no live row claimed → generic unavailable/stale rows (no inferred kind).

A name waits for both name lists before being treated as stale, so it is never misclassified
mid-load. Downstream consumes the union unchanged: provider/custom/search filters, board vs
flat, pagination, status counts, group sections, `/v1` toggles.

**Filters.** **Kind: all | models | aliases | combos** beside Provider and Status
(`ui/src/pages/models/filters.tsx:8-15`). Provider is **ownership only**: a combo is owned by
the synthetic `rrouter` bucket (`models.tsx:221-239`); every real provider stays its own
option (no preferred-provider picker, no collapsing).

**Grouping.** `rrouter/<name>` is the layout identity everywhere: layout item
`(provider, id)` (`crates/rrouter-store/src/presets.rs:85-113`), UI ref
`ui/src/lib/layout-draft.ts:98-107`, server key `format!("{provider}\0{id}")`
(`routes/models.rs:167-175`), store lookup `group_id_for(provider, id)`
(`presets.rs:218-237`). `combo:<name>` is only a **request** prefix, never a layout ref.

#### Accepted limitation: no group/provider prefix for combos (Q3, closed)

| Form | Meaning | Evidence |
|---|---|---|
| `<name>` (bare) | routes to the combo when a combo has that name — combos are matched **before** `provider/model`, so a combo named `openrouter/x` stays a combo | `planner.rs:707-709` |
| `combo:<name>` | forces combo interpretation when a model id collides | `planner.rs:132`, `:709`, `:735-736` |
| `group/<code>/<id>` | **models only**; the resolver requires a real provider segment and `rrouter` is not a provider | `planner.rs:754-768`, `provider_catalog.rs:283` |

Decision: keep it. A combo is not provider-scoped, and a group prefix would create a second
callable alias per combo (and a second collision surface) without changing routing. The
consequence, stated plainly: for combos a group assignment scopes **discovery and grants
only** (`routes/models.rs:153-213`), whereas for a model it also namespaces the advertised
ref. `/v1/models` keeps advertising the bare combo id.

### 2. Combo row anatomy

Collapsed: RRouter badge, name, **combo** badge, explicit **fallback / round robin / smart**
badge; `Show N members` disclosure. **Shipped:** the second line is `N candidates · metadata —`
(`unified-rows.ts:82-86`); the gateway computes the numbers, the row does not render them yet
(T1), so it never borrows a member's numbers.

| Value | Wire rule (shipped) | Row rendering (T1, not shipped) |
|---|---|---|
| context / max output | `{min, max}`, `null` when unknown | `guaranteed 256k · up to 1M`; equal bounds → `all candidates: 256k context`; one bound → `guaranteed 256k` / `up to 1M` |
| price | `{min,max}` per dimension + `known`/`total` | `$1–$5 in / $5–$25 out per 1M · candidate range`; equal → `all candidates $3 / $15 per 1M`; `—`, never `$0.00` |
| capabilities | guaranteed = AND, supported = OR, both nullable | `tools: all candidates` / `vision: some candidates`; `capabilities —`, never `no tools` |

Expanded (`ComboMemberList`, `table-unified.tsx:379-423`): members in stored order, 1-based,
each with ref, context, max output, per-1M price, and an `unavailable` badge; the order line
names the strategy (smart names the router model). The detail region belongs to the combo
row, not the dnd-kit list: members are not draggable and do not count toward group counts.
The member list joins the registry by ref, so a fan-out or version-core member shows its
first target's numbers until T1 renders `members[].targets[]`.

### 3. Create into a group in one flow

`/models` → **Add → Combo** opens the editor in a dialog (fields and ordered `ModelChain`
unchanged) plus **preset group** (`Ungrouped (RRouter)` + free-form groups). Sequence:

```
POST /api/combos {name, models, strategy, router_model?}   -> 200 (upsert, no group field)
invalidate qk.combos                                       -> row appears
draft.assignGroup(`rrouter/${name}`, groupId)              -> one debounced 250 ms PUT
```

No second writer, no second save protocol. A conflicted preset or no preset disables the
group select with the reason shown (`models.tsx:467-471`); the combo is still created. If the
layout PUT fails, the draft rolls back to the last accepted document
(`layout-draft.ts:296-309`), the combo exists and its row is unassigned; retry saves the
layout only.

**Assigning a group implies visible.** `assignGroup` sets `visible: true`, clears
`last_visible_position` and `hidden_at` on the same entry (`layout-draft.ts:410-432`): a
placement must render in its section, while the group-level `/v1` toggle stays
`setGroupVisible`.

The standalone `/routing/combos` editor binds the active preset or says *Open Models to
assign a preset group*; a second revisioned writer would race the open draft.

### 4. Metadata: resolution then fold

`crates/rrouter-gateway/src/combo_meta.rs` is the single fold (`RegistryIndex`,
`resolve_member`, `aggregate`, `combo_metadata`, `RegistrySnapshot`), shared by
`/v1/models` (`routes/models.rs:106-111`), `/v1/models/{id}`, `/api/combos` (list and create
echo, `routes/combos.rs:14-45`, `:102-106`).

**Resolution policy, per member, in order** (`combo_meta.rs:386-453`) — mirrors
`expand_entry_resolved`/`expand_entry_with` (`planner.rs:747-811`):

1. alias hop, exactly once, recorded as `via_alias`;
2. `code/id` or `provider/id`: first segment names a provider (shorthand or id; id wins on a
   collision) → exactly `(provider, id)`, never re-resolved elsewhere;
3. `group/code/id`: head is a layout group name (`presets.rs:198-200`) → `(provider, id)`;
4. bare id: every row with that id; else version-core matches (`crates/rrouter-router/src/catalog.rs:271-297`);
5. empty set → `state: "unknown"`, guarantees `null`. Disabled rows are kept and marked, and
   never contribute numbers.

A multi-provider bare id is a fan-out, not ambiguity: the runtime really may serve from any
of them, so advertising all rows replaces the guess. The index is built from the **unfiltered**
registry so `disabled` stays distinguishable from `unknown`.

**Aggregate over the union `T` of enabled targets** (`combo_meta.rs:472-588`):

| Field | Rule |
|---|---|
| `context`, `max_output` | `{min, max}`; `null` if any enabled target lacks it or any member is unknown |
| `pricing.{input,output,cache_read,cache_write}` | `{min, max}` over **known** costs + `known`/`total`; `null` only when no target has a price |
| `guaranteed_capabilities` | AND over enabled targets; `null` if any member is unknown or there are no targets |
| `supported_capabilities` | OR; display only ("some candidates"), never a promise |
| `reasoning`, `effort`, `api_capabilities`, `capabilities_extra` | value only when identical across all targets, else absent/null |
| `deprecated_members`, `all_deprecated` | counts of deprecated enabled targets; a retired row stays routable, so this is a warning |
| `availability` | `ok` when ≥1 enabled target; `smart` also needs an enabled, resolved router; `no-targets`, `router-unavailable` |

Per strategy: every strategy can reach every member, so **the numbers are identical** for
fallback, round robin and smart; only `availability` and the `router` block differ. A
per-strategy numeric difference would be precision the runtime does not have.

Invariants the fold keeps, each pinned by a unit test: order-independent, idempotent under
duplicate targets, monotone under member addition (never raises a guaranteed floor), unknown
propagates into guarantees, legacy `/v1` fields are projected from `Aggregate` by one function
(`legacy_meta`, `combo_meta.rs:279-302`), and no path picks a "best" provider row.

## Wire contract

Additive on `/api/combos` items and `POST` echo, `/v1/models` combo entries, and
`/v1/models/{id}` for a combo.

```ts
type Range<T> = { min: T | null; max: T | null };    // null members when unknown
type Member = {
  ref: string;                                       // exactly models[i]
  state: "exact" | "fanout" | "core" | "disabled" | "unknown";
  via_alias: string | null;
  model: Model | null;                               // compat: targets[0]
  targets: Model[];                                  // every row, sorted (provider, id)
};
type Aggregate = {
  context: Range<number> | null;
  max_output: Range<number> | null;
  pricing: { input: Range<number>; output: Range<number>;
             cache_read: Range<number>; cache_write: Range<number>;
             known: number; total: number } | null;
  guaranteed_capabilities: Capabilities | null;
  supported_capabilities: Capabilities | null;
  deprecated_members: number;
  all_deprecated: boolean;
  availability: "ok" | "no-targets" | "router-unavailable";
  router: { ref: string; model: Model | null } | null;   // smart only
};
```

The wire keeps `snake_case` keys exactly as written; absent means `null` in the JSON above
(`combo_meta.rs:192-257`). `01-models-ux.md` §3.1's `MetadataRange`/`ComboMemberMetadata`
sketches are superseded by this shape.

Legacy `/v1` fields for a combo are correct by construction (`legacy_meta`):
`context_length`/`context_window` = `aggregate.context.min`, omitted when unknown;
`max_completion_tokens`/`max_output_tokens` likewise; the `capabilities` block = AND over
enabled targets, omitted when the guarantee is `null` (`routes/models.rs:384-403`, pinned by
`tests/models.rs:2408`); `deprecated` = any; no top-level `cost` (a combo has no single
price). `/api/combos` keeps `name`, `models`, `strategy`, `router_model`, `cursor`,
`created_at` and adds `members`/`aggregate`; `/api/models` is unchanged.

Compatibility: a client that ignores `members`/`aggregate` sees the same OpenAI shape with
correct limits. A UI without the fields reads `metadata —` and still lists stored candidates
in order.

## Verification

**Verification approach (decided, Q5 closed).** No test runner in `ui/` in this slice. UI
proof is the full UI build plus the runtime assignment round-trip; `vitest` is a follow-up.

| Area | Proof | Command / steps |
|---|---|---|
| UI types + build | **passes** | `cd ui && bun run build` — `✓ built in 497ms` |
| Fold + resolution | **11 unit tests pass**; they pin order-independence, duplicate idempotence, the monotone floor, shorthand pinning, bare-id fan-out, core match, `group/code/id`, one alias hop, non-nested combo, disabled exclusion, price coverage | `cargo test -p rrouter-gateway --lib combo_meta` → `test result: ok. 11 passed; 0 failed` |
| Wire parity | **6 integration tests pass**, including `combo_with_a_1m_member_advertises_1m_when_every_target_supports_it`, `fanout_combo_advertises_the_guaranteed_floor_and_the_best_case`, `alias_and_combo_cannot_claim_one_name`, `colliding_alias_and_combo_list_once_and_the_combo_still_routes` | `cargo test -p rrouter-gateway --test models combo_` → `test result: ok. 6 passed; 0 failed` |
| Group parity in `/v1/models` | Combo + alias advertised, hidden alias omitted, combo in a hidden group omitted | `cargo test -p rrouter-gateway --test models v1_models_groups_follow_the_default_preset` (combo coverage at `tests/models.rs:980-1054`) |
| Runtime assignment round-trip | Models → `groups` → add "Prod" → tick a combo → a "Prod" section with one `rrouter` combo row, count 1, `/v1` on; reload keeps it | manual, browser; **not run in this slice** (`[INFERENCE]` from code) |

What a proof must show: the **row exists and resolves to the assigned group**. A test of
`entriesOf`/`layoutFor` alone passes without any row and is rejected as proof (`01b` §5).

## Open questions

Only unresolved items are listed; everything else is a decision above or a status below.

| # | Question | State |
|---|---|---|
| Q1 | Resolution coverage (`code/id`, `provider/id`, `group/code/id`, one alias hop, dated-core match, nested combo, disabled and disconnected members). | **Resolved** by `combo_meta.rs` + unit tests; one residual defect, S1. |
| Q4 | `/v1` parity is only guaranteed for the **default** preset, and the assignment controls do not say so. | **Decided:** the group manager's column is labelled the default-preset control; no code change. Accepted limitation (D3). |

Closed questions, kept for the record: **Q2** (a combo named like a model is advertised as the
combo, D9), **Q3** (no group/provider prefix, D10), **Q5** (no `vitest` in this slice), **Q6**
(anchors re-checked for this revision — the section files' pre-change anchors are superseded
by the ones above).

## Implementation status

Verified 2026-11-18 against the working tree; nothing outside the table is implied.

| Area | State | Evidence |
|---|---|---|
| `combo_meta` engine + additive wire (`members`, `aggregate`, correct legacy fields) | **Shipped, tests green** | `combo_meta.rs`; `cargo test -p rrouter-gateway --lib combo_meta` → 11 passed; `--test models combo_` → 6 passed |
| Name collision rule (alias > combo, both create paths) | **Shipped, tests green** | `routes/combos.rs:61-69`, `routes/models.rs:709-726`; `alias_and_combo_cannot_claim_one_name`, `colliding_alias_and_combo_list_once_and_the_combo_still_routes` |
| Phase 1 row union, dedupe, Kind filter, board groups, group counts, `/v1` toggles | **Shipped, build green** | `rowsFor` (`unified-rows.ts:97-175`) called at `models.tsx:248-266`; `KINDS` (`filters.tsx:8-15`); counts count rows per group (`models.tsx:403-410`), heading says `N entries` (`groups.tsx:39`); `bun run build` passes |
| Add→Combo with preset group; assigning a group implies `visible: true` | **Shipped, build green** | `models.tsx:448-471`, `:958-976`; `layout-draft.ts:410-432` |
| **T1** — combo row renders `aggregate`/`members` (context range, price range, capability split) | **Shipped (2026-11-23)** | `unified-rows.ts` (`aggregateOf`, `membersOf`, `comboMetaSummary`, `comboCapabilitySplit`), `table-unified.tsx` (`NameIdentity`, `ComboMemberList`, `FlatTable`). Wire fields read additively with fallback when absent. |
| **S1** — a multi-provider name gives two different answers | **Bug, open** | An alias is advertised from `resolve_one` = first enabled target, sorted by provider (`routes/models.rs:85`, `:547`), while the same members as a combo fold over the whole set. Observed through the real router with a seeded registry (rows `openai/gpt-4.1` 256k + `deepseek/gpt-4.1` 1M): `/v1/models` `fast` (alias) → `owned_by:"rrouter"`, `context_window:1000000`, no `aggregate`; `duo` (combo) → `256000`, `aggregate.context {min:256000,max:1000000}`. `GET /v1/models/fast` → 200 with the alias view. |
| **T2** — `/api/combos` is not on the admin router | **Defect, open** | Registered at `crates/rrouter-gateway/src/lib.rs:206-211` while `/api/aliases` sits on the admin router (`:161-167`), so a non-admin reader's `GET /api/combos` is refused (the route accepts no credential a reader holds) and the page's `useCombos(isAdmin)` never loads for a reader: alias rows appear, combo rows do not. Move it next to `/aliases`. |
| **T3** — the `preset group` select offers provider buckets as custom groups | **Fixed** | `models.tsx` filters `!group.provider` before mapping `{id, name}` so only custom groups appear in `ComboCreateDialog` and `AliasCreateDialog`. |
| T4 — delete-from-board for a combo | **Not shipped** | The board's hide/remove path only writes layout entries; deleting the combo is the `/routing/combos` page or the API. |
| Group-manager candidate dedupe ("shadowed by …") | **Not needed after D4** | Store-level collisions can still be written directly; `rowsFor` renders them once (`unified-rows.ts`). |
| Phase 3 lifecycle (`renameRef`, delete-entry cleanup) | **Design only** | No `renameRef` in `layout-draft.ts`. |
| UI test runner | **Shipped (`node --test`)** | `ui/package.json` runs `node --test "tests/**/*.test.ts"`; Models pure logic pinned in `ui/src/pages/models/models.test.ts`. |

Known limitations, stated plainly:

- The provider header on the Providers page (see `design/providers-redesign.md`) is expanded by
  default, with no collapse; `ui/src/components/table.tsx:195-216` has no group-header/colSpan
  support, so its summary is spread across columns.
- Live breaker state has no API (only the persisted probe `backoff_level`).
- Non-default presets have no `/v1/models` group parity (D3).
- Grouped combos get no group/provider prefix (D10).
- Non-admin readers still do not see combos until gateway T2 moves `/api/combos` onto the admin router.

---

## 2026-11-23 — Anti-Vibecode pass: the Models screen

Extends the shipped combo unification above without contradicting any closed decision (one table,
one preset board, `rrouter/<name>` identity, alias > combo, derived combo ranges, no `group/<combo>`
prefix). Decisions, not pixels.

### Job

"Which model should my request use, what does it cost, is it available right now, and how do I
make a group, alias, or combo of them."

### The four answers

| # | Question | Answer |
|---|---|---|
| 1 | **Why open it?** | To pick and copy the exact model reference for a client or agent, check what it costs per 1M tokens (including whether it is **Free**), verify whether it can serve traffic right now, and group/alias/combine models into a preset. |
| 2 | **How is it used?** | Open beside an editor, coding agent config, or Providers / Free mode. Scanned in seconds, copied, toggled, or tested inline, then closed. |
| 3 | **What do they want next?** | Confirm *why* a row is unavailable (disabled vs disconnected provider vs broken combo chain), test a model and see the result on that row, jump straight to `/providers` to fix a disconnected provider or `/free-mode` to set up free models, or create a group/alias/combo without leaving the preset. |
| 4 | **What are we willing to leave out?** | The `ConnectionCard` (base URLs / auth headers belong on Quick start and API Keys), duplicate "shown in `<group>`" text under every group select, the duplicate standalone custom-delete trash button beside the row menu, tracked-caps eyebrow styling, virtualisation (pagination already bounds DOM rows), and free-mode discovery/fallback logic (lives on `/free-mode`). |

### Audit (before → tell/tier → decision)

| Finding | Tell / tier | Decision |
|---|---|---|
| `<ConnectionCard />` occupied ~220 px above the toolbar on `/models`, pushing the model table below the fold. | T1 (borrowed component) | Deleted from `/models`. Base URLs and auth headers live on `/quickstart` and `/access/keys`. |
| `model.available === false` was completely hidden on registry rows, while alias targets and combo members marked the same model `unavailable`. Two surfaces disagreed on availability. | Floor (honesty, disagreeing states) | Render an explicit `unavailable` warn badge (`no healthy connection`) whenever `model.enabled && !model.available`, and report `M available` in the header summary from one shared `summarizeRows` function. |
| Non-board desktop views (readers, `Status: All/Hidden/Disabled/Not in preset`) used `DataTable` without `wrap: true` and without combo sub-rows: multi-line identity cells clipped and clicking `Show N members` did nothing. | Floor (broken control, clipped text) | Replaced the flat desktop `DataTable` branch with `FlatTable` sharing `StaticRow` and `ComboMemberList`; combo expansion and multi-line identity rows work identically across every filter and for readers. |
| Combo rows still showed `N candidates · metadata —` even though `combo_meta.rs` ships `aggregate` + `members`; combo member list fell back to `modelByRef` only and broke when `priceLabel` was removed. | T1 (unshipped wire), Floor (broken import) | Wired `aggregate` + `members` additively: qualified bounds (`guaranteed 256K · up to 1M`, `all candidates 256K`), price range + coverage (`2/3 priced`), guaranteed vs supported capability split, `no-targets` / `router-unavailable` warn badges, deprecation count, and per-member `targets[]` expansion. |
| Price units differed between desktop (`$2.50 / $10.00`, unit missing), phone card (`in / out per 1M`), and combo (`—`); zero-priced models had no visible `Free` marker or filter. | T1, T2, product goal | One price module (`price.ts`): `Free` emerald badge strictly for zero catalog price (never unknown), `$X in · $Y out /1M` (+ quiet cache line), `price —` for unknown. Added `?free=1` filter chip with live count, linking to `/free-mode` (admin only) and marking free models in `ModelSelect` / `ModelPicker`. |
| Empty-state branches ignored `kind` and `free` filters (showing "This preset has no models" when `kind=combos` or `free=1` matched 0 rows) and rendered `EmptyRegistry` when `/api/models` errored. | Floor (false empty states) | Pure `emptyStateKind(...)` classifier: error state never renders `EmptyRegistry`; filtered-free empty state offers Clear filters + link to `/free-mode`; every preset status branch checks actual counts. |
| `Test` button reported pass/fail only in a disappearing toast; nothing on the row recorded the result. | T1, T3 | Store last test outcome per ref in `useModelActions` and render it inline on the row (`✓ 412ms` / `✗ 429 …`) with a polite live region. |
| Group `<Select>` repeated `"shown in <group>"` directly beneath the select inside that group's section; custom rows had both a standalone Trash icon and `RowMenu → Delete custom model`; `RowMenu → Move to group` duplicated the inline Group select on desktop and cards. | T1 (same-question) | Deleted `"shown in <group>"` on desktop and phone cards; deleted duplicate Trash icon button; hide `Move to group` inside `RowMenu` whenever the inline Group select is visible (kept only at `sm`–`md` where the group column hides). |
| Tracked-caps eyebrow styling (`uppercase tracking-[0.06em/0.08em]`) on provider chips, `preset` label, `GroupAccessDialog` headings, `ModelPicker`, and `CapabilityChips`. | Floor (tracked-caps) | Replaced with plain small/monospace labels across all owned files. |
| `GroupManager` disabled the Access button on non-default presets with only a hover `title` explanation. | Floor (hover-only) | Added an inline visible note in `GroupManager` whenever `!isDefault`. |
| `Add → Alias` navigated away to `/routing/aliases` while `Add → Combo` stayed in-page. | T3 | Added in-page `AliasCreateDialog` (`POST /api/aliases` + optional preset group assignment) inside `preset-header.tsx`; `/routing/aliases` stays for full edit/delete. |
| Provider pill was static text; no task-scoped hover/focus preview on rows. | T3 | Provider pill links to `/providers`; hovering (~350 ms mouse) or focusing a model/alias/combo name opens a lightweight, non-modal, control-free preview scoped to ref, availability, limits, full pricing, capabilities, and test/member state. |
| **Performance**: `models.tsx` passed fresh inline closures (`onAssignGroup`, `onVisible`, `onHide`, `hideFor`, `rowActions`, `controls`) into `useUnifiedColumns` and `BoardSection` every render, defeating every `useMemo` on Shell's 5 s `useHealth` poll, 15 s background polls, and every search keystroke. `UnifiedRowContext` put `expanded` with `modelByRef`, re-rendering every row on one combo toggle. `ComboMemberList` mounted even while collapsed. `GroupManager` ran O(N²) `entries.find` over all candidates. | Performance | Stabilized draft callbacks via `draftRef` + `useCallback`, memoized `rowActions`/`controls`/`summary`, wrapped `BoardSection`/`StaticRow`/`SortableRow`/`UnifiedCard` in `React.memo`, split `UnifiedRowContext` (data vs expanded), lazy-mounted collapsed `ComboMemberList`, applied `content-visibility: auto` + `contain-intrinsic-size` on below-the-fold `BoardSection`s (`index > 0`, off while dragging), and indexed `GroupManager` entries with a `Map` + 100-row initial cap ("Show all"). |

### Deliberate trade-offs

- **Pagination over virtualisation (`DEFAULT_SIZE = 50`, max `200`).** Operators scrolling through
  500 models must click Next or raise page size to 200 rather than infinitely scroll. We accept
  this because pagination preserves semantic `<table>` structure, native sticky headers, keyboard
  `dnd-kit` board reordering, and browser Find-in-page, while `content-visibility: auto` on
  below-the-fold sections keeps 100–200 row pages responsive.
- **Zero-price only for `Free`; unknown price is never `Free`.** A free endpoint whose provider
  catalog omits pricing shows `price —`, not `Free`. We accept this because marking an unpriced
  model as free is a billing lie; Free mode discovery populates zero pricing for known free models.
- **No `Free` link for non-admin readers.** `/free-mode` is an admin-only route (`main.tsx:235`),
  so readers see a plain emerald `Free` badge instead of a dead link.
- **`Add → Alias` creates new aliases in-page; editing/deleting existing aliases and combos still
  links to `/routing/aliases` and `/routing/combos`.** Some operators will dislike leaving `/models`
  to edit a combo chain; we accept it to avoid duplicating full CRUD editors in two places while
  making *creation into a group* zero-navigation.
- **Inline test results are session-local.** Reloading clears the row's `✓ 412ms` badge. Accepted:
  probing every model on load would burn quota and add latency; the badge answers the test the
  operator just ran.

### Exclusions

No free-mode discovery, probing, or fallback settings on `/models` (`/free-mode` owns them). No
`ConnectionCard`. No virtual scroll library. No per-model quota/spend columns (`/usage` owns spend;
`/providers` owns account quota). No edits to `api.ts`, `queries.ts`, or shared `DataTable`.

### Preview contract

Hovering (~350 ms delay, mouse only) or keyboard-focusing a model, alias, or combo name opens a
non-modal, non-focus-stealing popover (`role="tooltip"` / descriptive panel) with **zero
interactive controls inside** (click/touch never traps; Escape or blur closes):

- **Model**: full callable ref (`group/code/id`), availability (`available` / `unavailable — no
  healthy connection` / `disabled`), context window & max output, input/output/cache pricing (or
  `Free · $0.00 /1M` / `price —`), all enabled capabilities, reasoning effort levels when present,
  and the last inline test result if run.
- **Alias**: `rrouter/<alias> → <target>`, target availability, target context & pricing.
- **Combo**: strategy (+ router model for smart), availability status, qualified context & max
  output ranges, candidate price range & coverage, guaranteed vs supported capabilities, and the
  ordered candidate refs.

### Click counts (top questions)

| Question | Clicks |
|---|---|
| Which models are available and what do they cost? | 0 (header summary + inline availability badge + unified `/1M` price line) |
| Which models are free right now? | 1 (`Free` filter chip, or 0 via `Free` badge + count in chip) |
| Copy the exact model ref for my client | 1 (`Copy` icon next to the model/alias/combo name) |
| Does this model actually answer right now? | 1 (`Play` test icon → inline `✓ latency` / `✗ status` on the row) |
| Create a combo or alias inside a preset group | 2 (`Add → Combo/Alias` → fill & Create) |
| Set up free models or paid-quota fallback | 1 (click any `Free` badge or the Free mode link → `/free-mode`) |

### Deep links kept

`?q=`, `?provider=`, `?kind=all|models|aliases|combos`, `?status=routed|hidden|disabled|unrouted|all`,
`?custom=1`, `?free=1`, `?preset=<id>`, `?page=`, `?size=`, plus legacy rewrites (`?view=`,
`?names=`, `?ipage=`, `?dpage=`).

Footer: combo redesign verified 2026-11-18; Anti-Vibecode Models redesign added 2026-11-23.
