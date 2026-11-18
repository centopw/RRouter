# Combo metadata: resolution and capability engine

## Decision

A combo's advertised metadata is **derived, never sampled**: resolve each member to the
full set of `(provider, id)` rows a request can actually land on, then fold that set with
commutative operators. Keep one pure aggregator (Rust) and one wire shape (`members` +
`aggregate`, additive) shared by `/v1/models`, `/v1/models/{id}` and `/api/combos`.
The legacy flat `/v1` fields keep their current conservative meaning, computed over the
corrected target set.

Refs: row UX lives in `01-models-ux.md`; this file owns resolution and numbers.

## 2. Data logic and metadata mapping

### 2.1 Every place combo metadata is produced or dropped

| Surface | Produces | Evidence | Status |
|---|---|---|---|
| `GET /api/models` (admin) | nothing — registry items only; `source` union is `catalog\|synced\|upstream\|custom` | `crates/rrouter-gateway/src/routes/models.rs:804-807`, `crates/rrouter-gateway/src/models_registry.rs:568-593` | combos absent by construction |
| `GET /v1/models` (client) | `combo_meta` per combo, or `None` | `routes/models.rs:83-94` | dropped on any unresolved member |
| `GET /v1/models/{id}` | same `combo_meta` | `routes/models.rs:655-668` | same |
| name collision | combo entry skipped entirely when its name equals a known model id | `routes/models.rs:85-87` (combo), `:73-75` (alias) | dropped, and the comment at `:69-70` contradicts routing |
| `GET/POST /api/combos` | name, models, strategy, router_model, cursor, created_at | `routes/combos.rs:13-23` | no metadata |
| `GET /api/system` | combo **count** only | `routes/system.rs:33-36`, `:57-63` | none |
| Models page row | none: `all` is `/api/models`; `modelRows` maps it; `staleRows` excludes every ref known to the alias/combo lists | `ui/src/pages/models.tsx:162-170`, `:204-215`, `:221-245` | combo has no row |
| Combo editor | strategy sentence + `ModelSelect` (id, `deprecated`, `unavailable`) | `ui/src/pages/combos.tsx:24-54`, `ui/src/components/model-select.tsx:43`, `:56-62` | no limits, no price |
| GroupManager candidates | `rrouter/<name> · N models` | `ui/src/pages/models.tsx:419-440` | no metadata |
| Group header | name, `custom group`/provider, visibility, `N models` | `ui/src/pages/models/groups.tsx:18-41` | none (correct — a group is not routable) |
| Registry row identity | context/max/price/chips, `—` for `null` | `ui/src/pages/models/columns.tsx:112-118`, `ui/src/lib/format.ts:3-4`, `ui/src/pages/models/rows.tsx:368-372` | the only renderer today |
| Request accounting | real per-request price from stored rows | `models_registry.rs:523-564` | proves the price data exists; it is dropped only at display time |

`/api/models` stays registry-only. Duplicating combos into it would widen the UI `source`
union (`ui/src/lib/api.ts:793-811`) and the provider filters for no gain: the page already
fetches `/api/combos` (`ui/src/pages/models.tsx:140-141`, `:200-202`).

### 2.2 Why a 1M-context member advertises 256k

Three independent defects, each sufficient:

1. **First-wins by bare id.** `by_id` is built with `.or_insert_with(...)` over a
   registry-ordered stream, so the first provider row for an id wins for every member
   using that id (`routes/models.rs:51-57`). It is also rebuilt identically inside
   `routable_model` (`:633-639`).
2. **Provider segment discarded on shorthand refs.** `resolve_meta` tries the exact ref —
   `ModelItem.reference` is always the provider id (`models_registry.rs:181`) — then, for
   `provider/id`, resolves the prefix only to *validate* it and then looks up the bare id:
   `and_then(|(_, id)| by_id.get(id))` (`routes/models.rs:111-116`). A member written with
   a shorthand (`oai/gpt-4.1`, the form the existing test uses at
   `crates/rrouter-gateway/tests/models.rs:483-490`) therefore resolves to **some other
   provider's row**. Any other provider serving that id with 262 144 tokens produces
   exactly the reported 256k. The specific provider pair is `[INFERENCE]`; the mechanism
   is `routes/models.rs:111-116` + `:55`.
3. **One row per member.** Routing does not pick one row: a bare id fans out to *every*
   catalog provider serving it (`crates/rrouter-router/src/planner.rs:796-802`), and a
   suffix-dated id resolves by version core (`crates/rrouter-router/src/catalog.rs:275-297`,
   `:389-406`). Advertising a single row's number is a guess, whichever row is chosen.

On top of that, `intersect_meta` takes `min` (`routes/models.rs:148-149`) and
`min_limit` returns `None` when *either* side is `None` (`:178-183`), while any unknown
member makes the whole combo `None` via `?` (`:135`). A `None` meta makes `model_json`
return early: no limits, no capabilities, no modality block at all
(`routes/models.rs:421-423`). The UI's only renderer for `null` is an em dash
(`ui/src/lib/format.ts:4`), and price is omitted, not zeroed
(`ui/src/pages/models/rows.tsx:368-372`). A derived model therefore looks like a
low-capability one.

Baseline the redesign must preserve: the existing "smallest member window" assertion
(`tests/models.rs:540`) is a *correct* conservative answer for a genuinely mixed chain
(1 048 576 + 200 000 → 200 000). The bug is the unresolved/sampled cases, not `min`
itself — which is why the fix is a resolution policy plus a range, not a different fold.

### 2.3 Why cost is always absent for combos

`intersect_meta` hardcodes `left.cost = None` (`routes/models.rs:174`); nothing else
recomputes it. `/v1/models` never emits `cost` even for plain models
(`model_json`, `routes/models.rs:401-515` — no `cost` key). `/api/combos` has no price
field (`routes/combos.rs:13-23`). `/api/models` has `cost` on `ModelItem`
(`models_registry.rs:169`, populated by `overlay` at `:319-321`) but does not list combos.
Conclusion: the price is known per member and dropped at three separate display seams.

A combo has **no single price** — the served member decides it — so the truthful surface
is a range plus the per-target numbers, never an average and never member 1's price.

### 2.4 Why capability intersection mislabels a fallback chain

`Capabilities::intersect` is a plain AND over nine flags (`models_registry.rs:99-111`) and
is applied pairwise (`routes/models.rs:150`). It answers "can *every* request to this
combo use vision", which is the right question for a guarantee and the wrong question for
describing the model. Consequences:

- A chain `[vision model, text-only backup]` advertises `vision:false`. A client that
  would use the combo with images never tries it, even though the primary handles them.
- `reasoning`, `effort`, `capabilities_extra` and `api_capabilities` already use
  equal-or-`None` (`routes/models.rs:154-171`); two of those flags are also inside the
  AND, so the same fact is expressed twice with different truthiness.
- `tools` defaults to `true` in `Capabilities::default` (`models_registry.rs:55-69`), so
  "all members support tools" is partly an assumption, not evidence.
- `deprecated |= right.deprecated` (`routes/models.rs:172`) marks a whole chain retired
  when its last-resort member is retired, even though a retired row stays routable
  (`models_registry.rs:173-175`) and the healthy members are unaffected.

Correct split: **guaranteed** (AND) and **supported** (OR), both nullable, with the
deprecation count carried separately.

### 2.5 Resolution and aggregate rules

Resolution happens first and is strategy-independent. `targets` is the deduped set of
`(provider, id)` rows a request may land on for one member, sorted by `(provider, id)`.

Aggregate, over the union `T` of all members' targets (disabled targets excluded from all
numbers, unresolved members poison the guarantees):

| Field | Rule | Why a client can rely on it |
|---|---|---|
| `context` | `{min: min(context), max: max(context)}` | `min` is the guaranteed floor for any request; `max` is the best case. Both `null` if any enabled target's `context` is `None`, or if any member is unresolved. |
| `max_output` | same as `context` | same |
| `pricing.input/output/cache_read/cache_write` | `{min, max}` over **known** costs of enabled targets, plus `known`/`total` counts | A price is accounting data, not a safety guarantee: a range over known values plus an explicit coverage count is better than hiding everything when one row lacks a models.dev price. Null only when no target has a price. |
| `guaranteed_capabilities` | AND over enabled targets; `null` if any target is unresolved | A capability a caller may send unconditionally |
| `supported_capabilities` | OR over enabled targets; `null` if any target is unresolved | Display only: "some candidates"; never a promise |
| `reasoning`, `effort`, `api_capabilities`, `capabilities_extra` | value only when identical across every enabled target, else `null` | A parameter is safe to send only if every possible target accepts it (`routes/models.rs:154-171`, extended from pairwise to the set) |
| `deprecated_members` | count of deprecated enabled targets (with `all_deprecated`) | Deprecation is a per-member warning, not a combo property |
| `availability` | at least one enabled target resolved | A combo with none is routed but unservable |

No average, no first-member substitution, no `0` for unknown.

Per strategy:

| Strategy | Reachable set | Numeric aggregate | Availability | Extra |
|---|---|---|---|---|
| `fallback` | every member may serve (the next is tried on failure) | table above | ≥1 enabled target | `guaranteed` floor is the honest answer: an operator cannot stop fallback from reaching the backup |
| `round_robin` | every member may serve; the start rotates (`proxy.rs:249`, `docs/COMBOS.md:10-15`) | identical | ≥1 enabled target | identical numbers are correct, not laziness — every request can hit every target |
| `smart` | the decision model picks one; on any failure it starts at index 0 (`proxy.rs:254-300`, `docs/COMBOS.md:80-82`) | identical over members | ≥1 enabled target **and** the router resolves to an enabled model | `router: {ref, model}` — the router is a separate billable call and is not folded into candidate pricing |

Strategy therefore changes *availability* and *reading*, never the numbers. Any
per-strategy numeric difference would be a made-up precision the runtime does not have.

### 2.6 Wire shape

Additive on `/api/combos` items (and its `POST` echo), on `/v1/models` combo entries, and
on `/v1/models/{id}` for a combo. Amends the type sketch in `01-models-ux.md` §3.1 in two
places: a member resolves to `targets[]` (not one `model`), and pricing carries a coverage
count.

```ts
type Range<T> = { min: T; max: T };            // null members when unknown
type Member = {
  ref: string;                                  // exactly models[i]
  state: "exact" | "fanout" | "core" | "disabled" | "unknown";
  via_alias?: string;                           // one hop, as routing does
  model: Model | null;                          // compat: targets[0]
  targets: Model[];                              // every routable row, sorted
};
type Aggregate = {
  context: Range<number | null> | null;          // null when any guarantee is unknown
  max_output: Range<number | null> | null;
  pricing: { input: Range<number|null>; output: Range<number|null>;
             cache_read: Range<number|null>; cache_write: Range<number|null>;
             known: number; total: number } | null;
  guaranteed_capabilities: Capabilities | null;
  supported_capabilities: Capabilities | null;
  deprecated_members: number;
  availability: "ok" | "no-targets" | "router-unavailable";
  router?: { ref: string; model: Model | null } | null;   // smart only
};
```

Existing fields change meaning only by becoming **correct**, and only for combos:

| Field | Before | After | Compat |
|---|---|---|---|
| `context_length`, `context_window` | min over one sampled row per member | `aggregate.context.min` over the honest target set; omitted when any guarantee is unknown | same rule (min/omit), same key, corrected set |
| `max_completion_tokens`, `max_output_tokens` | as above | `aggregate.max_output.min` | same |
| `capabilities.*` | AND over sampled rows; whole block omitted when meta is `None` | AND over honest targets; whole block omitted when `guaranteed_capabilities` is `null` | a combo with an unresolved member still emits no block, exactly as today (`tests/models.rs:548-554`) |
| `deprecated` | true if any member deprecated | unchanged (`any`) — the corrected "all" view lives in `deprecated_members`/`all_deprecated` | no value changes for a client |
| `name` | absent for combos | absent | unchanged |
| `members`, `aggregate` | absent | new | additive; ignored by OpenAI clients |

Explicitly **not** added: a top-level `cost` on `/v1/models` items. A combo has no single
price, and emitting one would create a new lie; plain-model pricing already exists on
`/api/models` (`models_registry.rs:169`).

### 2.7 Edge cases

| Case | Resolution | Numbers | UI |
|---|---|---|---|
| Exact `provider/id` or `code/id` | one target; the provider segment is never re-interpreted | that row's values | as a single model |
| Bare id served by N providers | `state:"fanout"`, N targets, planner-identical (`planner.rs:796-802`) | range over all N | `N targets`, expandable |
| Suffix-dated id | `state:"core"`, targets from version-core matches (`catalog.rs:275-297`) | range over matches | shows the matched ids |
| Group ref `group/code/id` | one target after the group-name check (`planner.rs:754-768`); group must exist | that row | ref copied as-is |
| Alias | one hop (`planner.rs:722-727`); `via_alias` records it | target's values | `alias → target` |
| Member naming another combo | **not** expanded — routing treats it as a bare id (`planner.rs:707-734` only expands the requested name) | `state:"unknown"` → guarantees `null` | editor warns "a member cannot be a combo" |
| Disabled member | `state:"disabled"`, `model` present, excluded from all numbers (`models_registry.rs:281` disables an id on every provider) | excluded | `disabled` badge with reason |
| Disconnected provider | item absent from the registry (`models_registry.rs:275-280`) → `state:"unknown"` | guarantees `null` | `unavailable` |
| Deprecated member | routable (`models_registry.rs:173-175`) | included | `deprecated` badge + count |
| Members spanning providers/shorthands | each member resolves independently; no cross-member provider assumption | set union | provider chip per target |
| Combo name equals a model id | routing prefers the combo (`planner.rs:707-710`), so `/v1/models` must advertise the **combo** for that id and stop dropping it; the registry row stays reachable as `provider/id` | combo aggregate | one row, `shadowed` registry rows listed in the detail |

## 3. Capability calculation engine

### 3.1 Module and signature

New `crates/rrouter-gateway/src/combo_meta.rs` (replaces `combo_meta`/`intersect_meta`/
`min_limit`/`resolve_meta` in `routes/models.rs:102-183`).

```rust
pub struct RegistryIndex<'a> { /* by_ref, by_id, by_core, providers, groups, aliases */ }
impl<'a> RegistryIndex<'a> {
    pub fn new(items: &'a [ModelItem], providers: &[(String, String)],
               groups: &'a [LayoutGroup], aliases: &'a [ModelAlias]) -> Self;
    pub fn resolve_member(&self, entry: &str) -> Member;      // I/O-free
}
/// Pure: no store, no clock, no state, no provider order.
/// `strategy` and `router` only affect `availability` and the `router` block;
/// every numeric field is a function of `members` alone.
pub fn aggregate(strategy: ComboStrategy, members: &[Member],
                 router: Option<&Member>) -> Option<Aggregate>;
pub fn combo_metadata(index: &RegistryIndex, combo: &Combo) -> ComboMetadata; // resolve + fold
```

`RegistryIndex` is built once per request from the **unfiltered** registry (so `disabled`
is distinguishable from `unknown`), unlike today's enabled-only maps
(`routes/models.rs:46-57`). Version-core matching needs `core_of` (`catalog.rs:275`) made
`pub`; it is already the routing rule.

`aggregate` is one fold over a sorted, deduped `Vec<&ModelItem>`, so every operator is
commutative and associative.

### 3.2 Resolution policy (replaces `by_id` first-wins)

Applied in order, per member:

1. **Alias hop.** If the entry names an alias, follow it exactly once (`planner.rs:722-727`).
2. **Slashed entry.** If the first segment names a provider by id or shorthand
   (`provider_catalog.rs:283-285`), the target is `(provider, rest)` — always, even when the
   registry has no such row. Never re-resolve the id on another provider.
3. **Group ref.** If the entry is `head/code/id`, `head` is a layout group name
   (`presets.rs:198-201`) and `code` names a provider, the target is `(provider, id)`.
4. **Bare id.** All enabled items with that id, deduped and sorted. If none, version-core
   matches, same ordering. If still none → `state:"unknown"`, no targets.
5. A target that exists but is disabled is kept and marked; it never contributes numbers.

This mirrors `expand_entry_resolved`/`expand_entry_with` (`planner.rs:747-811`) instead of
approximating it. It deliberately diverges from the coordinator's "ambiguous → unresolved"
instruction: a multi-provider bare id is *not* ambiguous at runtime, it is a fan-out; the
fix for guessing is to advertise all reachable rows, not to hide them. Empty is the only
unresolved state.

### 3.3 Invariants

1. **Determinism.** Output depends only on the sorted `(provider, id)` multiset of targets
   and the strategy field, not on registry or provider iteration order.
2. **Idempotence under duplicates.** Fan-out and core-match can yield the same target
   twice; dedup makes the aggregate identical.
3. **Monotonicity under member addition.** Adding a member can only lower `context.min` /
   `max_output.min`, raise the `max` bounds, widen price ranges, shrink
   `guaranteed_capabilities`, grow `supported_capabilities`, or introduce `null`. It can
   never raise a guaranteed floor.
4. **None-propagation.** A guarantee (context, max output, capabilities) is `null` if any
   member is unresolved or any enabled target lacks that field. Pricing degrades to a
   range plus a coverage count, never to a fabricated value.
5. **No projection drift.** The legacy `/v1` view is derived from `Aggregate` by one
   function, so the flat fields and `aggregate` cannot disagree.
6. **No provider preference.** No code path chooses "the best" provider row; the only
   ordering is lexicographic for stable output.

### 3.4 UI-side contract

Deferring to `01-models-ux.md` §2.3/§3.2 for rendering; the data obligations are:

| Surface | Must show | Unknown |
|---|---|---|
| Combo row (`/models`) | `aggregate.context` as `guaranteed {min} · up to {max}` (or one bound, or `all candidates`), `aggregate.pricing` as `$min–$max in / out per 1M · candidate range`, guaranteed/supported chips, deprecated count | `—` with an accessible "unknown" label; never a member's number standing in |
| Member disclosure | `targets[]` per member: ref, context, max, price, chips, state badge | `—`; unresolved members keep their stored ref and order |
| Combo editor (`ui/src/pages/combos.tsx`) | per member the resolved target line; for `fanout`, all targets; footer shows the aggregate `/v1` will advertise plus the strategy caveat | `—`; a member that resolves to nothing shows the reason (unknown / disabled / combo) |
| GroupManager candidate | `N models` plus availability when not all resolve | no metadata rather than an invented one |
| Group header | unchanged: count only (`groups.tsx:18-41`) — a group is a presentation bucket, not a routing unit | n/a |

`ModelSelect` keeps listing only `/api/models` refs (`model-select.tsx:43`), so the editor
creates exact refs only; fan-out members can only arrive through the API, and the editor
must show them rather than silently rewriting them.

Group assignment of a combo (defect 1) is a row-set problem, not a metadata one:
`assignGroup` already creates a visible entry for an unseen ref
(`ui/src/lib/layout-draft.ts:410-419`) and the layout validator accepts `rrouter/<name>`
(`presets.rs:274-455`); the row is simply never rendered (`models.tsx:221-245`). Once
combo rows exist (`01-models-ux.md` §2.1) the group cell and group counts include them with
no further API change; the metadata fields above are what that row needs.

### 3.5 Tests that must exist

Unit, in `combo_meta.rs`:

| Test | Asserts |
|---|---|
| `aggregate_is_independent_of_target_order` | shuffled input → identical output |
| `aggregate_is_idempotent_under_duplicate_targets` | same target twice → same result |
| `adding_a_member_never_raises_the_guaranteed_floor` | monotonicity |
| `shorthand_ref_resolves_only_to_its_provider` | `oai/x` ≠ another provider's `x` (the 256k fix) |
| `bare_id_fans_out_to_every_enabled_provider` | `targets.len() == N`, sorted |
| `core_match_resolves_a_dated_id` | suffix-dated member finds the curated row |
| `group_ref_member_resolves_through_the_middle_segment` | `group/code/id` |
| `alias_member_resolves_one_hop` | `via_alias` set, target numbers used |
| `combo_member_is_unknown_not_nested` | no recursion, guarantees `null` |
| `disabled_member_is_excluded_from_guarantees` | numbers from enabled targets only |
| `pricing_is_a_range_with_coverage_counts` | min/max + `known`/`total`, never an average |

Integration, in `crates/rrouter-gateway/tests/models.rs`:

| Test | Asserts |
|---|---|
| `combo_with_a_1m_member_advertises_1m_when_every_target_supports_it` | reported defect: no arbitrary 256k |
| `fanout_combo_advertises_the_guaranteed_floor_and_the_best_case` | `/v1` flat fields = `min`; `aggregate` carries both bounds |
| `combo_members_carry_per_target_price_and_capabilities` | `members[].targets[].cost` present |
| `unresolved_member_keeps_legacy_fields_omitted_but_lists_the_member` | extends `tests/models.rs:548-554` |
| `combo_named_like_a_model_is_advertised_as_the_combo` | collision follows routing (`planner.rs:707-710`) |

Run: `cargo test -p rrouter-gateway combo_meta && cargo test -p rrouter-gateway --test models`.
Existing `tests/models.rs:538-546` must keep passing unchanged (mixed chain → 200 000).

### 3.6 Migration and compatibility

- `/v1/models` and `/v1/models/{id}`: only additive keys (`members`, `aggregate`) and
  corrected values for combos. Every omitted-when-unknown rule is preserved, so a client
  that reads `context_length` sees the same key present/absent in the same cases.
- `/api/combos`: additive `members`/`aggregate`; `name`, `models`, `strategy`,
  `router_model`, `cursor`, `created_at` untouched (`routes/combos.rs:13-23`).
- `/api/models`: unchanged; combos stay out of the registry payload.
- The one behavioral change: a combo whose name equals a model id is advertised as the
  combo instead of being dropped. This matches routing and removes a name that today
  resolves differently from how it is listed.
- Rollout is safe in either order for the UI (missing `members`/`aggregate` must render as
  "metadata unavailable", per `01-models-ux.md` §3.1), and for clients (unknown keys are
  ignored by OpenAI-compatible SDKs).
