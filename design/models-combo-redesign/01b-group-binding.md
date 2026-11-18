# 01b — Binding combos (and aliases) to preset groups

**Verdict: the assignment succeeds and is stored; the row that would show it is never built.**
"No combo can be assigned to a custom group" is, in the current code, "an assigned combo has
no row anywhere, so nothing visibly changes". The fixes are therefore in row construction
(shared with `01-models-ux.md` §2.1), not in the draft, the PUT, or the store.

Evidence level: **code path, executed at the draft layer.** No RRouter dev server was
running (`:4200` is an unrelated Angular app — `ps` shows `ng serve`; `/admin/...` returns
its HTML). I executed `entriesOf → effectiveGroups → groupForEntry → layoutFor` from
`ui/src/lib/layout-draft.ts` under node on a combo item; output in §1.4. Nothing was written
to a running instance.

## 0. Which hypothesis holds

| Hypothesis | Verdict | Proof |
|---|---|---|
| Entry rejected on save (400) and the error swallowed | **No** | `layoutFor` emits `{provider:"rrouter", id:<combo>, group:<g>}` (`ui/src/lib/layout-draft.ts:119-156`); server validation has no provider-existence rule, only schema, bounds, `(provider,id)` uniqueness, dense positions and group references (`crates/rrouter-store/src/presets.rs:270-470`); the route only parses and calls the store (`crates/rrouter-gateway/src/routes/model_presets.rs:471-505`). |
| Candidate list lacks a fresh combo | **No** for the normal flow | Create invalidates the combo cache (`ui/src/pages/combos.tsx:92-96`); the Models page reads it with default `staleTime: 0` (`ui/src/lib/queries.ts:404-410`) and builds `groupCandidates` from it (`ui/src/pages/models.tsx:419-440`). Only an out-of-band create (direct API, other tab without focus) is stale, and refetch-on-focus fixes that. |
| Saved but never rendered | **Yes — primary** | Combo refs are deliberately excluded from the stale-row union (`ui/src/pages/models.tsx:225-237`) and `/api/models` never contains combos (`crates/rrouter-gateway/src/routes/models.rs:803-806`), so no row exists (§1.1). |
| Group header count / filter hides it | **Consequence, not independent** | `boardGroups` drops a group with zero rows (`ui/src/pages/models.tsx:366-376`); `groupCounts` therefore reports 0 (`ui/src/pages/models.tsx:377-383`, header at `:772`). |
| `/v1/models` groups disagree with the UI | **No inside the default preset**, **yes outside it** (§3) | `/v1/models` reads only the default preset (`crates/rrouter-gateway/src/routes/models.rs:198-205`); the layout PUT targets whichever preset is active (`ui/src/pages/models.tsx:122-128,909`). |

## 1. Root causes

### RC1 — The Models board has no row kind that can carry `rrouter/<name>`

The board's row set is `modelRows + staleRows` (`ui/src/pages/models.tsx:272`), and neither
half can contain a live combo:

| Step | Where | Result |
|---|---|---|
| Registry rows | `all` = `/api/models` (`ui/src/pages/models.tsx:162`), which is `models_registry::list` only (`crates/rrouter-gateway/src/routes/models.rs:803-806`) | no aliases, no combos |
| Page contract | "The table holds registry models only; aliases and combos live on their own screens." (`ui/src/pages/models.tsx:62-69`) | intentional, and the cause |
| Stale rows | filter is `!known.has(entry.ref)`, where `known` is registry refs **plus** `rrouter/<alias>` and `rrouter/<combo>` (`ui/src/pages/models.tsx:225-237`) | an assigned combo is "known", so it is excluded |
| Row types | `AliasRow` / `ComboRow` exist with `entry` and `status` (`ui/src/pages/models/unified-rows.ts:22-38`) | never instantiated by the page |

Everything downstream then has nothing to work with: `filteredRows/statusRows` (`:272-307`),
`boardRows` (`:328-341`), `boardGroups` (`:366-376`), and the per-row group select
(`ui/src/pages/models/columns-unified.tsx:66-92`).

**Observable symptom:** in the Group Manager the "models (N)" count increments
(`ui/src/pages/models/groups.tsx:159`), the checkbox stays checked on reopen because it
reads the draft entry (`:207-221`), and the board shows no row and no section. The
assignment *is* in `model_presets.layout_json` (`crates/rrouter-store/src/presets.rs:866-870`).

### RC2 — There is no second place to assign from, and the combo editor knows nothing about presets

Only two assignment surfaces exist: the Group Manager (custom groups only — its "models (N)" entry
is hidden when `group.provider` is set, `ui/src/pages/models/groups.tsx:159`) and the per-row
group select (`ui/src/pages/models/columns-unified.tsx:66-92`). The combo editor has no
`group`, `preset` or `layout` reference at all (`rg 'group|preset|layout'
ui/src/pages/combos.tsx` → 0 matches). A user who expects to assign at creation time has no
control, which reads as "cannot be assigned".

### RC3 — Editing a non-default preset binds nothing that `/v1/models` reports

The page's active preset is a URL/selection value (`ui/src/pages/models.tsx:122-128`), and
the debounced PUT targets that preset (`ui/src/lib/layout-draft.ts:271-318`;
`crates/rrouter-gateway/src/routes/model_presets.rs:471-505`). Discovery reads the *default*
preset only (`crates/rrouter-gateway/src/routes/models.rs:192-205`), as do the group-alias
resolver (`crates/rrouter-gateway/src/routes/models.rs:597`) and the planner
(`crates/rrouter-router/src/planner.rs:760`). The Group Manager's own description already names the default preset as the
discovery target (`ui/src/pages/models/groups.tsx:113-116`), but the assignment controls do not.

### RC4 — A grouped combo gets no group-qualified id, so the group is metadata only for names

For a registry model in a free-form group, `/v1/models` advertises
`<group name>/<provider code>/<id>` (`crates/rrouter-gateway/src/routes/models.rs:373-386`).
For `provider == "rrouter"` the canonical id is the bare name, unconditionally
(`:367-372`). The store's own doc says a free-form group's name *is* a routing alias whose
grant gates use (`crates/rrouter-store/src/presets.rs:143-149,120-124`); the group resolver
requires a real provider segment (`crates/rrouter-gateway/src/routes/models.rs:588-614`,
`crates/rrouter-router/src/planner.rs:741-767`), and `rrouter` is not a provider
(`crates/rrouter-gateway/src/provider_catalog.rs:283`). Net: `main/<combo>` cannot be formed
or called, and a grant on a combo-only group scopes discovery only — unlike the model case.
This is a design decision for the coordinator, not a prerequisite for RC1/RC2.

### RC5 (small) — Combo names and alias names share one namespace, and `/v1/models` can emit duplicates

Both are `rrouter/<name>` (`ui/src/pages/models.tsx:427-438`). If an alias and a combo share
a name, `groupCandidates` produces two entries with the same `ref` and the same React key
(`ui/src/pages/models/groups.tsx:213`), one checkbox toggles both, and `/v1/models` pushes
both rows (`crates/rrouter-gateway/src/routes/models.rs:69-95`) with the same id and group.

### 1.4 Executed draft-layer check

```
entries        [{"ref":"rrouter/research","visible":true,"group":"g-main"}]
effectiveGroupIds g-main
groupForEntry  g-main
roundTrip      [{"provider":"rrouter","id":"research","group":"g-main","position":0,"visible":true}]
ungrouped      provider:rrouter
```

The draft keeps, saves and resolves the combo's group correctly. The break is strictly the
missing row in `models.tsx`.

## 2. Fix design

Keep the fix boring: one row union, one identity rule, no server change.

### 2.1 Identity rule — keep `rrouter/<name>`

| Layer | Contract | Citation |
|---|---|---|
| Layout item | a group assignment is stored per `(provider, id)` | `crates/rrouter-store/src/presets.rs:87-99` |
| UI ref | `ref = \`${provider}/${id}\`` | `ui/src/lib/layout-draft.ts:98-107` |
| Server grouping key | `format!("{provider}\0{id}")` | `crates/rrouter-gateway/src/routes/models.rs:236-243` |
| Store group resolution | `group_id_for(provider, id)` | `crates/rrouter-store/src/presets.rs:218-236` |
| Candidate ref | `rrouter/${combo.name}` | `ui/src/pages/models.tsx:433-438` |

Do **not** introduce `combo:<name>` or `bare name`: `grouped_models` looks the item up by
`(provider, id)`, so a prefixed or bare ref would simply miss, and the combo would silently
fall to the implicit `provider:rrouter` bucket (`crates/rrouter-gateway/src/routes/models.rs:249-262`).
Aliases keep the same shape; the two share the namespace, which is why RC5 needs a UI guard.

### 2.2 Draft/state layer — no change

`assignGroup` already does the right thing for a ref the preset has never stored: it appends
`{ ref, visible: true, group }` (`ui/src/lib/layout-draft.ts:410-420`). `deleteGroup` already
clears `group` on its entries before removing the group (`:387-396`), which matters because
the server rejects an item referencing an unknown group (`crates/rrouter-store/src/presets.rs:431-446`).
`layoutFor` also strips an unknown group id defensively (`ui/src/lib/layout-draft.ts:131`).

One addition is worth it, and only for rename (§2.5): a `renameRef(from, to)` that moves
`ref`, `visible`, `group`, `last_visible_position`, `hidden_at` in one `change`.

### 2.3 Save/validation layer — no change

Explicitly **do not** add a "provider must exist" check. Combos are not registry rows, so
such a check would reject exactly the case that is broken. The existing document validation
(shape, `(provider,id)` uniqueness, dense positions, group references, provider-group
membership) is sufficient and already accepts `provider: "rrouter"` items.

### 2.4 Rendering — build the union once, then everything else is free

1. Extract a pure `rowsFor({models, aliases, combos, entries, presetAware, autoInclude})`
   in `ui/src/pages/models/unified-rows.ts` returning `UnifiedRow[]`:
   registry `ModelRow`s, then `AliasRow`s at `rrouter/<alias>`, then `ComboRow`s at
   `rrouter/<name>` with `entry = entryByRef.get(ref)` and `status = statusOfName(entry,
   presetAware, autoInclude)`, then stale rows for entries no live row claimed.
   `AliasRow`/`ComboRow`/`statusOfName` already exist (`:22-65`).
2. `models.tsx` replaces `modelRows`/`staleRows` with that call; `filteredRows` keeps its
   provider/custom/search predicates (`:272-287`). `ComboRow` is provider `rrouter`, so the
   existing "RRouter" bucket already exists for them (`:251-260`).
3. Nothing else changes: the group select is kind-agnostic (`columns-unified.tsx:66-92`), and
   `groupForEntry` already returns the explicit group for any entry
   (`ui/src/lib/layout-draft.ts:199-209`).
4. Wording: `GroupHeading`/`boardGroups` counts currently say "models" and count rows
   (`groups.tsx:17-41`, `models.tsx:377-383,772`). Count entries, not kinds; a combo member
   is not a row.

### 2.5 Creating a combo *into* a group in one step

Preferred, and the only one that avoids a second layout writer: **create from the Models
board, inside the preset draft.** The draft is already loaded for the active preset, so the
assignment is the same debounced, revisioned PUT as every other group change.

```
POST /api/combos {name, models, strategy, router_model?}     -> 200 (name)
invalidate qk.combos                                          -> row appears (2.4)
draft.assignGroup(`rrouter/${name}`, groupId)                 -> dirty, one 250 ms PUT
```

If the PUT fails, `persist` rolls the draft back to the last accepted document and surfaces
`draft.error` (`ui/src/lib/layout-draft.ts:296-309`); the combo still exists and its row is
now unassigned, so the operator can retry from the row. No data is lost.

The standalone `/combos` page has no preset context and no draft. Adding a group select
there means a second revisioned write: `GET /api/model-presets/<id>` → append the item →
`PUT .../layout` with the fetched revision. It will 409 whenever the Models page holds
unsaved edits, which is the exact conflict the draft exists to prevent. If the operator
wants it anyway, limit it to *new* combos (no group field when editing) and on 409 say
"open Models to assign" rather than retrying. Recommendation: don't.

### 2.6 Rename and delete reconciliation

| Event | Current behavior | Rule to adopt |
|---|---|---|
| Combo renamed in the editor | `POST /api/combos` is an upsert keyed by name (`crates/rrouter-gateway/src/routes/combos.rs:38-79`; `crates/rrouter-store/src/store.rs:2832-2900`), and there is **no rename endpoint**. A new name inserts a new row; the old combo stays until deleted. | Treat refs as keys: create new name → delete old name → `draft.renameRef("rrouter/<old>", "rrouter/<new>")` in the open preset. Other presets keep `rrouter/<old>` and surface it as a stale row (below). A server-side transactional rename that rewrites every `layout_json` is the only alternative and is not worth it now. |
| Combo deleted | Layout entries survive. Today that ghost is invisible (RC1). | Once rows exist, it renders as an unavailable stale row **in the group it was assigned to** (`ui/src/pages/models.tsx:221-241`; hide/remove at `columns-unified.tsx:151-160`). Keep that semantic — same as a deleted custom model. If it was the group's only row, the section disappears (`:366-376`), so hiding the ghost is also how the operator removes an empty group from the board. |
| Group deleted | `deleteGroup` clears `entry.group` first (`layout-draft.ts:387-396`). | Unchanged; required by `presets.rs:431-446`. |
| Provider bucket normalized | `ensureProviderGroups` materializes `provider:rrouter` when the manager opens (`models.tsx:443-451`). | With combo rows, assign them explicitly or leave them in that bucket; both resolve identically (`effectiveGroups`, `layout-draft.ts:163-196`). |

Rename is the only case that needs new draft code; everything else is already correct.

## 3. Server-side truth

| Question | Answer | Citation |
|---|---|---|
| Does `/v1/models` place a grouped combo in the UI's group? | **Yes, when the assignment is in the default preset.** The item is found by `(provider,id) = ("rrouter", combo name)`, its `group` is accepted only if the group id exists, and the entry carries `"group": <id>`. | `crates/rrouter-gateway/src/routes/models.rs:236-262,393-420` |
| Does it when the UI edited a non-default preset? | **No.** Discovery reads `default_model_preset()` only. | `crates/rrouter-gateway/src/routes/models.rs:192-205` |
| Is a grouped combo namespaced like a grouped model? | **No.** The canonical id stays the bare combo name; only `group` records the binding. | `crates/rrouter-gateway/src/routes/models.rs:367-372` vs `:373-386` |
| Can a client call `main/<combo>`? | **No.** The group resolver needs `group/<provider>/<id>` and `rrouter` is not a provider. | `crates/rrouter-gateway/src/routes/models.rs:588-614`; `crates/rrouter-router/src/planner.rs:741-767`; `crates/rrouter-gateway/src/provider_catalog.rs:283` |
| Do hidden groups / `auto_include_new` / grants apply to combos? | **Yes, identically**: hidden group drops the entry, `auto_include_new: false` drops an entry-less combo, a denied group hides its combos. | `crates/rrouter-gateway/src/routes/models.rs:253-283` |

So the UI grouping and `/v1/models` grouping agree for the default preset; the two places
that do **not** agree are (a) any non-default preset (RC3) and (b) the "group is a routable
prefix" property (RC4). State them plainly; neither is the reported defect.

## 4. Edge cases

| Case | Behavior after the fix | Notes |
|---|---|---|
| Default vs non-default preset | Row and group render in both; `/v1/models` reflects only the default | Label the group manager's `/v1` toggle "default preset" (RC3) |
| `auto_include_new: true`, combo never assigned | `statusOfName` → `routed`; `/v1/models` lists it in `provider:rrouter` | `unified-rows.ts:54-65`; `models.rs:255` |
| `auto_include_new: false`, combo never assigned | `unrouted`; assigning appends `visible:true`, which routes it | `layout-draft.ts:410-420` |
| Hidden group | Board still shows the group; `/v1/models` omits it and its combos | `presets.rs:158-160`; `models.rs:275-280` |
| Deleted group | Items lose `group` in the same document, else the PUT is a 400 | `presets.rs:431-446` |
| Combo name collides with a real model id | Combo is not listed at all (`known_ids` skip) while the model keeps the name; the assignment is inert | `models.rs:69-95`; disable the candidate and say "shadowed by `<provider>/<id>`" |
| Alias and combo share a name | Duplicate candidate ref, duplicate checkbox, duplicate `/v1/models` row | RC5; dedupe in `groupCandidates` and prefer one kind |
| Duplicate assignment | Idempotent; at most one item per `(provider,id)` because the document rejects duplicates | `layout-draft.ts:410-420`; `presets.rs:431-436` |
| Assigning an alias | Same ref shape, same path; the alias target is metadata only | `models.rs:71-82` |
| Combo deleted while assigned | Unavailable stale row inside its group, removable | `models.tsx:221-241` |
| Combo renamed | Requires `renameRef` in the open draft; other presets keep a stale ref | §2.6 |

## 5. Verification plan

**What would make a test meaningful:** it must assert the *row* exists and resolves to the
assigned group. A test of `layoutFor`/`entriesOf` alone passes today (§1.4) and would certify
the status quo — reject that shape.

1. **UI unit test (this is the one that fails today).** New `vitest` runner in `ui/`
   (`vitest` is not a dependency: `ui/package.json` has only `tsc`/`vite`), file
   `ui/src/pages/models/rows.test.ts`, exercising the extracted `rowsFor` from §2.4 with:
   - a fresh combo (`models/[rrouter/research]`, no entry) → one `ComboRow`;
   - the same combo with `entry {ref:"rrouter/research", group:"g-main", visible:true}` →
     one `ComboRow`, `status: "routed"`, and `groupForEntry(entry, groups) === "g-main"`;
   - the same entry with `autoInclude:false` and no entry → `unrouted`;
   - unassigning (`group: undefined`) → `provider:rrouter`;
   - a combo ref absent from all three live lists → exactly one stale row (no duplication).
   Command: `npx vitest run ui/src/pages/models/rows.test.ts`.
2. **Gateway regression (passes today; locks `/v1/models` parity).** Extend
   `crates/rrouter-gateway/tests/models.rs` next to `free_form_group_name_is_a_routable_alias`
   (`:557-600`) with `combo_group_is_advertised_in_the_default_preset`: `store.upsert_combo`,
   write a default-preset layout whose item is `("rrouter", "research", Some("g-main"))`,
   `GET /v1/models`, assert exactly one entry with `id == "research"`, `owned_by == "rrouter"`,
   `group == "g-main"`; then rewrite the layout with `auto_include_new: false` and no item and
   assert the combo is omitted. Command:
   `cargo test -p rrouter-gateway --test models combo_group_is_advertised_in_the_default_preset`.
3. **Manual acceptance (5 steps).** As admin: Models → `groups` → add "Prod" → `models (1)`
   → tick a combo → close. Expect: a "Prod" section with one `rrouter` combo row, count 1,
   `/v1` on; reload the page and the row is still there; `curl /v1/models` shows
   `{"id":"<combo>","owned_by":"rrouter","group":"<g-id>"}` when the preset is the default
   one. Today step 3 ends with no row and no section — that is the reproduction.
