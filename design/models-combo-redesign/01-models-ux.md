# `/models` UX: models, aliases, and combos as first-class rows

## Decision

Keep **one table and one preset board**. Add alias and combo rows to that row set; do not add a separate combo section or a view switcher. A discreet **kind** filter narrows the same rows. Combo details expand inline beneath the combo row.

This preserves the existing filter/preset model and makes group assignment, `/v1` visibility, ordering, search, and stale-row handling operate on the same ref regardless of row kind.

## 1. Current layout anatomy

### 1.1 Row construction and where names disappear

| Concern | Current behavior | Evidence |
|---|---|---|
| Table contract | The page explicitly describes a registry-model-only table; aliases and combos have separate screens. | `ui/src/pages/models.tsx:62-69` |
| Registry rows | `all` sorts `/api/models` by provider and model recency; `modelRows` maps every registry model to a `ModelRow` with its preset entry and status. | `ui/src/pages/models.tsx:163-170`, `ui/src/pages/models.tsx:204-215` |
| Alias/combo loading | Both lists are fetched for admins, but converted only to arrays used outside the main row construction. | `ui/src/pages/models.tsx:138-140`, `ui/src/pages/models.tsx:200-202` |
| Stale rows | The stale set is `modelRows + staleRows`. A stored ref absent from models, aliases, and combos becomes `kind: "model", model: null`; alias/combo refs are only used to avoid misclassifying a name while its list loads. | `ui/src/pages/models.tsx:218-245`, `ui/src/pages/models.tsx:271-274` |
| Actual filtering | Only `modelRows` and `staleRows` enter provider/custom/search filtering; consequently no live combo or alias enters the table. | `ui/src/pages/models.tsx:270-287` |
| Existing name types | `AliasRow` and `ComboRow` already exist, with ref, name, payload, preset entry, and status; `statusOfName` already handles auto-include. They are not instantiated by `Models`. | `ui/src/pages/models/unified-rows.ts:22-40`, `ui/src/pages/models/unified-rows.ts:54-65` |
| Existing name renderers | `NameIdentity` can show an RRouter name, kind, copy action, strategy/target detail, and hidden badge. Desktop unified columns nevertheless return `null` for every non-model row, and actions do likewise. | `ui/src/pages/models/table-unified.tsx:464-478`, `ui/src/pages/models/columns-unified.tsx:56-60`, `ui/src/pages/models/columns-unified.tsx:157-168` |
| Combo strategy | The existing detail formatter renders `combo · fallback|round robin|smart · N models`; the same three strategies drive the combo editor. | `ui/src/pages/models/unified-rows.ts:67-79`, `ui/src/pages/combos.tsx:24-54` |

### 1.2 Columns and row actions

The effective desktop columns are **provider / name**, preset-scoped **group**, preset-scoped **`/v1`**, **enabled**, and admin **actions**. Group is a select containing only custom groups; `/v1` reflects preset visibility and is disabled for disabled models. Enabled is model-only and currently renders an em dash for other kinds. Non-model actions are empty. (`ui/src/pages/models/columns-unified.tsx:54-168`)

Model metadata is embedded in the identity cell rather than separate columns: provider badge, id/ref/copy, custom/deprecated/disabled badges, display name, context, max output, input/output price, and up to two capability chips. (`ui/src/pages/models/columns.tsx:50-114`) Phone cards repeat those fields with spelled-out pricing units. (`ui/src/pages/models/rows.tsx:222-264`) Missing price is omitted, and there is inherently no `0` fallback because `Model.cost` is nullable. (`ui/src/pages/models/rows.tsx:367-382`, `ui/src/lib/api.ts:793-811`)

| Row surface | Current actions | Evidence |
|---|---|---|
| Registry model | Test completion; global enable; edit limits/price; move group; remove from preset; delete custom model where applicable. | `ui/src/pages/models/columns.tsx:116-184`, `ui/src/pages/models/rows.tsx:134-184` |
| Stale model | Hide the stored entry while editable; unavailable and optional hidden badges. | `ui/src/pages/models/columns-unified.tsx:151-160`, `ui/src/pages/models/table-unified.tsx:451-462` |
| Alias/combo | Phone card links to the kind's editor if such a row is supplied; desktop has no rendered name/actions because its columns return `null`. | `ui/src/pages/models/table-unified.tsx:346-353`, `ui/src/pages/models/columns-unified.tsx:58`, `ui/src/pages/models/columns-unified.tsx:158` |
| Combo editor | Edit/delete buttons; create or update name, ordered candidates, strategy, and smart router. No group or preset field. | `ui/src/pages/combos.tsx:221-246`, `ui/src/pages/combos.tsx:269-392` |

### 1.3 Grouping, filters, and draft behavior

- **Grouping.** A draft entry may carry a custom `group`; absent group means its provider bucket. (`ui/src/lib/layout-draft.ts:31-49`) `effectiveGroups` emits explicit groups followed by implicit provider buckets derived from visible entries, and `groupForEntry` resolves custom group before provider bucket. (`ui/src/lib/layout-draft.ts:158-209`) The page retains the synthetic `rrouter` provider bucket when aliases or combos exist. (`ui/src/pages/models.tsx:247-260`) Group headers currently say `N models`, even when candidates can include names. (`ui/src/pages/models/groups.tsx:17-41`)
- **The grouping loophole.** `groupCandidates` includes every alias and combo as `rrouter/<name>`, so the Group Manager can assign a combo that has no table row. (`ui/src/pages/models.tsx:418-440`) The manager already labels candidates model/alias/combo and searches their refs and labels. (`ui/src/pages/models/groups.tsx:181-232`)
- **Auto-include.** Only registry refs are adopted into the draft; alias/combo refs are omitted from `newRefKey`. (`ui/src/pages/models.tsx:383-402`) `assignGroup` already creates a visible entry when the ref is new. (`ui/src/lib/layout-draft.ts:400-414`)
- **URL/search.** Filters live in URL parameters; search writes after 200 ms and resets page one. (`ui/src/pages/models.tsx:87-113`) Filtering then narrows provider, custom, text, and preset status without swapping surfaces. (`ui/src/pages/models.tsx:270-307`, `ui/src/pages/models/filters.tsx:9-14`)
- **Provider filter.** It lists connected providers, one option per provider. (`ui/src/pages/models.tsx:180-183`, `ui/src/pages/models/filters.tsx:67-79`) Name search code already includes member refs for combos and target for aliases. (`ui/src/pages/models/unified-rows.ts:81-102`)
- **Board vs flat.** Routed-only preset view is the grouped board; other status selections produce a flat, paginated table/cards. Drag is enabled only for editable, unfiltered routed board rows. (`ui/src/pages/models.tsx:313-365`) Counts and group sections derive from the current row set. (`ui/src/pages/models.tsx:354-362`, `ui/src/pages/models.tsx:370-381`)
- **Draft saves.** Every group/order/visibility mutation updates local state first and debounces the revisioned layout PUT by 250 ms. A 409 retains the local draft and blocks further automatic saves; other errors restore the last accepted snapshot. (`ui/src/lib/layout-draft.ts:11-23`, `ui/src/lib/layout-draft.ts:267-312`, `ui/src/lib/layout-draft.ts:314-323`) The preset header exposes saved/dirty/saving/conflict, explicit save, discard, and save-as-new. (`ui/src/pages/models/preset-header.tsx:10-15`, `ui/src/pages/models/preset-header.tsx:66-177`)
- **Hidden/stale rows.** Hide records previous visible position and timestamp; restore reuses that position. (`ui/src/lib/layout-draft.ts:427-462`) Missing registry entries remain visible as unavailable rather than disappearing. (`ui/src/pages/models/unified-rows.ts:4-11`, `ui/src/pages/models.tsx:218-245`)
- **Keyboard/a11y.** The row menu provides arrows/Home/End, Escape/focus restore via Radix, and labelled triggers. (`ui/src/pages/models/rows.tsx:43-49`, `ui/src/pages/models/rows.tsx:66-119`) Drag supports pointer, delayed touch, and Space/arrows/Space/Escape, with announcements and reduced-motion handling. (`ui/src/components/sortable-list.tsx:31-37`, `ui/src/components/sortable-list.tsx:97-150`) Combo chain editing uses named selects and up/down/remove buttons. (`ui/src/components/model-select.tsx:112-158`)

## 2. Proposed layout

### 2.1 Same table, all kinds

Rows are constructed as the concatenation of:

1. registry `ModelRow`s;
2. live `AliasRow`s at `rrouter/<alias>`;
3. live `ComboRow`s at `rrouter/<name>`;
4. preset refs not resolved by any of those three, rendered as generic unavailable/stale rows.

Add **Kind: all | models | aliases | combos** beside Provider and Status. It filters the same rows; it does not change columns or route away. Keep one option per real provider and add a separate **RRouter** option for aliases/combos. Provider matching is by row ownership, not by combo membership: selecting Anthropic must not pull in every combo containing an Anthropic member. Never merge real providers behind a preferred-provider picker or into one combined entry.

Board order, flat order, pagination, status counts, search, and group counts all consume this union. Auto-include adopts unseen refs from all three live kinds. A combo with no preset entry otherwise behaves exactly like a new registry model: `unrouted` without auto-include, `routed` with it.

### 2.2 Wireframe

```text
Models     [Preset: Production v] [saved] [Save] [...]              [Add v]

[search models, aliases, combos...] [provider: all v] [kind: all v]
[Status: Routed v] [custom] [groups]                              [Clear]

Production apps · custom group                           3 rows
+----------------------------------------------------------------------------------+
| reorder | provider / name                                      | group | /v1 |...|
+---------+------------------------------------------------------+-------+-----+---+
|   :::   | [acme] claude-sonnet [copy]                         | Prod v| on  |...|
|         | ctx 200k · max 64k · $3 / $15 · tools               |       |     |   |
+---------+------------------------------------------------------+-------+-----+---+
|   :::   | [rrouter] research-stack [copy] [combo] [fallback]  | Prod v| on  |...|
|         | 3 candidates · guaranteed 256k · up to 1M           |       |     |   |
|         | $1-$5 in / $5-$25 out per 1M · tools: all/some      |       |     |   |
|         | [^ Hide members]                                    |       |     |   |
|         | +----------------------------------------------------------------------+
|         | | # | member                         | context | price/1M | capabilities|
|         | | 1 | acme/claude-sonnet             | 256k    | $3 / $15 | tools       |
|         | | 2 | ocg/gemini-pro                 | 1M      | $1 / $5  | tools       |
|         | | 3 | old/vendor-model [unavailable] | —       | —        | —           |
|         | +----------------------------------------------------------------------+
|         | Fallback tries 1 -> 2 -> 3. Combo ranges are not one model's spec.     |
+---------+------------------------------------------------------+-------+-----+---+
|   :::   | [rrouter] fast [copy] [alias]                       | Prod v| off |...|
|         | alias -> acme/claude-haiku · target available       |       |     |   |
+---------+------------------------------------------------------+-------+-----+---+

RRouter · provider                                          1 row
...

Phone card (same information, no horizontal table):
+------------------------------------------------------+
| [rrouter] research-stack [copy] [combo]       [/v1]  |
| fallback · 3 candidates · [all unavailable if so]    |
| guaranteed 256k · up to 1M                          |
| $1-$5 in / $5-$25 out per 1M                        |
| Group [Production apps v]  [Show 3 members]  [...]   |
|   1 acme/claude-sonnet · 256k · $3/$15 · tools      |
|   2 old/model [unavailable] · metadata —             |
+------------------------------------------------------+
```

### 2.3 Expansion and aggregate presentation

- The combo identity begins with an RRouter badge, combo name, **combo** badge, and explicit **fallback / round robin / smart** badge. Smart rows also name the router model and the extra router call.
- A labelled disclosure button, **`Show N members` / `Hide members`**, expands inline. The whole row is not a click target, avoiding conflicts with selects, menus, copy, and drag handles. The detail region belongs to the combo row, not the sortable list; members are not separately draggable and do not increment group counts.
- Members appear in stored candidate order with stable 1-based numbers. Fallback is explicitly `1 → 2 → 3`; round-robin says the starting candidate rotates and remaining candidates retain this fallback order; smart says the router chooses the starting candidate and the rest retain fallback order. (`ui/src/lib/api.ts:625-631`)
- Every member shows stored ref, context, max output where space permits, candidate input/output price per 1M tokens, and capabilities. `model: null` keeps the ref and adds **unavailable**; all unknown cells are `—`.
- The collapsed combo **must not use `ctx N` or `$low / $high` without a range qualifier**. Use:
  - two bounds: `guaranteed 256k · up to 1M`;
  - equal bounds: `all candidates: 256k context`;
  - only one known bound: `guaranteed 256k` or `up to 1M`;
  - no known bound: `context —`.
  Apply the same pattern to max output.
- Pricing is **`$min–$max in / $min–$max out per 1M · candidate range`**. Equal bounds read `all candidates $3 / $15 per 1M`. Unknown fields are `—`; no zero, cheapest-member-only, or first-member-only substitution. Smart-combo candidate pricing excludes the router's extra call and says so in the detail.
- Capability chips are split into **`all candidates`** (guaranteed intersection) and **`some candidates`** (union). No unqualified tools chip on a combo. If capabilities cannot be established completely, show `capabilities —` rather than treating absence as unsupported.
- Availability is a separate badge from routed/hidden/disabled. A combo can be routed but currently have **no eligible candidates**; show that warning on the parent row and the reason beside each unavailable member.

## 3. Column and row contract

### 3.1 API fields

Reuse `/api/models` `Model` unchanged: `ref`, `provider`, `code`, `id`, `display`, `context`, `max_output`, `capabilities`, optional `reasoning`, `cost`, `source`, `enabled`, `available`, and `deprecated`. (`ui/src/lib/api.ts:793-811`)

Reuse `/api/aliases` `Alias` as `{ alias, target }`; resolve target metadata through the existing `/api/models` ref index. (`ui/src/lib/api.ts:623`) No alias metadata duplication is needed.

Extend each `/api/combos` item additively:

```ts
type MetadataRange = { min: number | null; max: number | null } | null;

type ComboMemberMetadata = {
  ref: string;                 // identical to the corresponding models[] item
  model: Model | null;         // null: ref no longer resolves; never drop the member
};

type ComboAggregate = {
  context: MetadataRange;
  max_output: MetadataRange;
  pricing: {
    input: MetadataRange;
    output: MetadataRange;
    cache_read: MetadataRange;
    cache_write: MetadataRange;
  };
  guaranteed_capabilities: Capabilities | null;
  supported_capabilities: Capabilities | null;
};

type Combo = {
  name: string;
  models: string[];                   // candidate order remains authoritative
  strategy?: "fallback" | "round_robin" | "smart";
  router_model?: string | null;
  members: ComboMemberMetadata[];     // same length/order as models
  aggregate: ComboAggregate | null;   // null: aggregate not yet computed/available
};
```

The old combo contract is `{name, models, strategy?, router_model?}`. (`ui/src/lib/api.ts:633-641`) UI must continue to accept missing `members`/`aggregate` during rollout and derive member availability by joining `models[]` to `/api/models` until those fields arrive.

Aggregate bounds are across candidates, not first-member metadata. Any unknown member value makes the affected bound `null`; the API must not silently take a min/max over only known values and present it as a chain guarantee. Pricing covers the selected candidate only. Router metadata remains available through `/api/models`; smart-router cost is not folded into candidate ranges.

### 3.2 Common columns and null rendering

| Column | Model | Alias | Combo | Stale ref |
|---|---|---|---|---|
| Provider / name | Existing provider/id/ref and single-model metadata. | RRouter/alias, target ref, target availability; target metadata may be shown as `target: …`. | RRouter/name, combo + strategy badges, qualified ranges/capabilities, member disclosure. | Stored ref, **unavailable**; no inferred kind or metadata. |
| Group | Custom group name/select; ungrouped provider bucket remains implicit. | Same. | Same; new combo is immediately assignable. | Existing stored group remains visible while editable. |
| `/v1` | Existing preset visibility toggle; disabled model cannot be listed. | Preset visibility, using auto-include status when no entry. | Same as alias. Member health does not silently rewrite preset visibility. | Existing visibility; removal/hide remains available. |
| Availability | `enabled && available`; separate from preset status. | Resolved target has `enabled && available`. | At least one eligible candidate; smart also requires an enabled, available router. | Unavailable. |
| Enabled | Existing global model switch/read-only badge. | `—` (no independent global alias switch). | `—` (no independent global combo switch). | `—`. |
| Actions | Existing test/edit/toggle/group/hide/custom delete. | Edit alias; group; hide. | Show members; edit combo; group; hide; delete combo in editor/action menu. | Group/hide as currently permitted. |

| Value state | Rendering rule |
|---|---|
| Numeric value `null`/missing | `—` with an accessible `unknown` label/title. |
| Range with two unequal bounds | Explicit `guaranteed` / `up to` or `min–max` plus `candidate range`. |
| Range with equal bounds | `all candidates`; still never a bare model-style combo spec. |
| Partially known range | Show only the known labelled bound; no invented other endpoint. |
| Price missing | `—`, never `$0.00`; include unit whenever a price is present. |
| Capabilities missing/incomplete | `capabilities —`, never `no tools`. |
| Member unresolved | Keep ordered ref and `unavailable`; all metadata `—`. |
| Aggregate missing | Member disclosure still works; summary says `metadata —`, not a fallback to member 1. |

### 3.3 Meaning of status vs availability

| Kind | Preset status (`routed`, `hidden`, `unrouted`, `catalog`) | Runtime availability |
|---|---|---|
| Model | Existing `statusOfModel`: global disabled first, then preset entry/visibility. (`ui/src/pages/models/unified-rows.ts:42-52`) | Registry model exists, is enabled, and `available` is true. |
| Alias | Existing `statusOfName`, including auto-include for an unseen ref. (`ui/src/pages/models/unified-rows.ts:54-65`) | Stored target resolves to an enabled, available model. Unknown target means unavailable, not hidden. |
| Combo | Same as alias. | Fallback/round-robin: at least one candidate resolves to an enabled, available model. Smart: that condition plus enabled/available router metadata; if router metadata cannot be found, availability is unavailable and its reason is shown. |
| Stale | Derived from the stored entry's visibility, as today. | False. |

Do not make the existing Status filter into a health filter. Health is visible on the row and in details; preset status retains its current meaning and counts.

## 4. Group integration

### 4.1 Create in one flow

Opening **Add → Combo** from `/models` opens the combo editor in a modal/drawer on `/models`, reusing the current fields and ordered `ModelChain` instead of navigating away. (`ui/src/pages/models/preset-header.tsx:182-223`, `ui/src/components/model-select.tsx:93-166`) Add:

- **Preset group** select: `Ungrouped (RRouter)` plus each custom group in the active preset.
- Default `Ungrouped`; when launched from a group's inline **Add combo** action, default to that group.
- The select is disabled without a loaded editable preset, with the reason shown; combo creation itself remains possible.
- A conflicted preset disables assignment until conflict resolution, consistent with current layout editing restrictions. (`ui/src/pages/models.tsx:193-197`)

On successful combo creation:

1. Build `ref = rrouter/<name>` and call the existing draft `assignGroup(ref, group)`; this already creates a visible entry if needed. (`ui/src/lib/layout-draft.ts:400-414`)
2. Invalidate combos as the current mutation does, then render the new row in its selected group. (`ui/src/pages/combos.tsx:92-114`)
3. Preserve the current 250 ms draft autosave and saved/saving/conflict indicators; do not add a second save protocol. (`ui/src/lib/layout-draft.ts:11-17`, `ui/src/lib/layout-draft.ts:314-323`)
4. If layout persistence fails after combo creation, keep the created combo, state **Combo created; group assignment not saved**, and retain the existing save error/conflict recovery. Retry saves the layout only; it must not create a duplicate combo.

The same editor remains usable on `/routing/combos`; there its group selector binds the currently active preset or is explicitly disabled with **Open Models to assign a preset group**. The new combo row also retains an inline group select for later correction.

### 4.2 Group display and lifecycle

- Group headers change from `N models` to **`N rows`** or **`N items`**. Count models + aliases + combos once each; expanded member rows do not count. (`ui/src/pages/models/groups.tsx:39-41`)
- The combo row displays its current custom group in the same Group select as models. Ungrouped aliases/combos appear under the existing RRouter provider bucket, not under a member's provider. (`ui/src/pages/models.tsx:247-260`)
- **Rename:** capture the old ref when editing begins. After a successful rename, replace `rrouter/<old>` with `rrouter/<new>` in the same draft entry, preserving group, visibility, order, and hidden-restore metadata. Do not append a duplicate or silently drop membership. If rename fails, do not touch the draft.
- **Delete from this UI:** after successful combo deletion, remove its preset entry rather than creating a deliberately unavailable row; decrement the group's count. Other presets and out-of-band deletions still use stale-row behavior so missing configuration remains visible. (`ui/src/pages/models.tsx:218-245`)
- **Delete a custom group:** preserve current semantics—clear that group id from every model/alias/combo entry. Ungrouped combos return to RRouter, and the combo is not deleted or hidden. (`ui/src/lib/layout-draft.ts:388-399`, `ui/src/lib/layout-draft.ts:199-209`)
- **Rename a group:** no combo/member rewrite is needed; the entry stores the stable group id. (`ui/src/lib/layout-draft.ts:368-379`)

## 5. Interaction requirements to retain

| Interaction | Required behavior with first-class names |
|---|---|
| Search | Keep URL persistence and 200 ms debouncing. Match model provider/id/display, alias/target, and combo/name/member refs through existing `matchesSearch`. (`ui/src/pages/models.tsx:103-113`, `ui/src/pages/models/unified-rows.ts:81-102`) Searching a member may reveal its parent combo, but expansion remains operator-controlled. |
| Provider and kind filters | Compose with custom/status/search and reset page one. Provider is ownership only; RRouter owns aliases/combos. Keep every real provider as its own option and every model/provider row separate. |
| Status and “outside” count | Calculate counts across all kinds from the same filtered union; search matches outside the active status still offer Show all. (`ui/src/pages/models.tsx:299-307`) |
| Preset draft | All group, visibility, hide, restore, and reorder operations use the same revisioned debounced draft. Filtering never unmounts the header or drops a pending save. (`ui/src/pages/models.tsx:62-69`) Save-conflict UI remains global and actionable. |
| Board/flat parity | Board and flat views share columns and row data. Combo expansion works on desktop and phone cards. Phone retains up/down move buttons when board drag is unavailable. (`ui/src/pages/models/table-unified.tsx:274-365`) |
| Sort/keyboard drag | Only the current unfiltered routed board is draggable, as now. Expanded content moves with the combo and is excluded from dnd-kit's ref list. Existing Space/arrow/Escape commands and announcements remain unchanged except saying `row` rather than `model`. (`ui/src/pages/models.tsx:313-320`, `ui/src/components/sortable-list.tsx:50-53`) |
| Disclosure a11y | Real button with `aria-expanded`, `aria-controls`, and accessible name `Show/Hide members for <combo>`; detail region has an accessible heading. Escape is not co-opted; Tab moves into member links/actions only if present. Expanded state is local and keyed by combo ref; removing the combo removes that state. |
| Menus/forms | Preserve one-tab-stop menus with arrow/Home/End navigation, focus return, labelled icon buttons, visible focus, and named combo member controls. Disabled group selects expose the reason. (`ui/src/pages/models/rows.tsx:66-119`, `ui/src/components/model-select.tsx:124-145`) |
| Stale rows | Keep unresolved preset refs visible with unavailable badges and group/hide controls; do not fabricate `kind: model` in displayed copy. Live combos missing some members are not stale rows—the parent stays live and members show their own unavailable state. |
| Hidden rows | Preserve restore position and undo. Hiding a combo affects preset discovery, not direct routing, matching the current undo explanation. (`ui/src/lib/layout-draft.ts:427-462`, `ui/src/pages/models.tsx:680-706`) |
| Metadata loading/errors | A failed aggregate/member metadata response leaves name, strategy, order, groups, and editing usable. Show unknown metadata, a retry, and never a plausible-looking substitute. |
| Empty states | An empty registry no longer implies no rows if aliases/combos exist. Keep registry-specific sync guidance inside an empty model-kind result instead of replacing the whole union table. (`ui/src/pages/models.tsx:719-727`) |

## 6. Phasing

| Phase | Additive work | Backend dependency | Result |
|---|---|---|---|
| **1 — first-class identity and grouping** | Union live model/alias/combo/stale rows; render existing `NameIdentity`; add Kind and RRouter filters; adopt all unseen refs; shared group/`/v1`/hide/reorder; `N rows`; combo disclosure with ordered stored refs; Add Combo editor with preset-group select. Join existing `/api/models` for best-effort member health. | None beyond existing `/api/models`, `/api/aliases`, `/api/combos`, and preset layout. | New combos appear immediately and can be assigned during creation. Strategy and member order are visible even without metadata. No provider collapsing. |
| **2 — honest combo metadata** | Add nullable `members` and `aggregate` consumers; qualified context/output/price/capabilities; unavailable-member states; aggregate retry/error states. | Add per-member `Model | null` and complete-or-null aggregate bounds/capabilities to `/api/combos`. | No first-member 256k/price/tools masquerading as whole-combo metadata. Missing data stays unknown. Phase 1 remains functional against old responses. |
| **3 — identity lifecycle completion** | Preserve draft entry on rename; intentional-delete entry cleanup and failure recovery copy. | Atomic combo rename or an explicitly coordinated API operation if current create/update cannot rename in place. | Grouped combos survive renames without duplicates/stale ghosts; existing presets remain consistent. |

Phase 1 must not block on Phase 2: absent aggregate fields render `metadata —`, while the disclosure still lists ordered candidates and whatever current registry join can prove. Phase 2 must not change grouping, routing refs, or provider ownership semantics.
