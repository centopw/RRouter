# Model presets

Layout presets for the **Models** page: a named, ordered board of registry models with
groups and a per-row visibility flag. A preset changes what an operator sees and how it is
ordered; it does **not** change routing. Hiding a row leaves the model routable, and nothing
here writes `disabled_models`.

Status: shipped. The routes below are served by this build, the dashboard is the reference
client (`ui/src/lib/api.ts`, `ui/src/lib/layout-draft.ts`, `ui/src/components/model-presets.tsx`),
and `scripts/model_presets_smoke.py` is the API half of acceptance.

## State model

| State | Where it lives | Effect |
| --- | --- | --- |
| **Preset** | server-side library, `scope: "global"` | Named layout document, instance-wide, admin-managed |
| **Group** | preset `layout.groups[]` | `{ id, name, provider? }`; array order is display order. Provider buckets use `provider:<provider>` |
| **Item** | preset `layout.items[]` | `{ provider, id, group?, position, visible }`; `group` is an explicit group id, omitted means the provider bucket |
| **visible: false** | item flag | The row is hidden from this preset's active list; it **stays routable** |
| **`disabled_models`** | routing registry | Separate `PUT /api/disabled/{model}` state. Hiding a preset row never touches it |
| **active** | per account | The preset a signed-in person is looking at; `active_id` in the list is resolved for the caller |
| **default** | one row, `is_default: true` | What a caller with no preference resolves to (admin token, gateway key, loopback). Exactly one; cannot be deleted |
| **auto_include_new** | layout document | Registry models the document does not name are merged in rather than left unlisted |
| **revision** | preset | Bumped only by a layout write; every save must send the revision it read |

A preset item whose `provider`/`id` is no longer in the registry stays in the document so a
reconnect restores it, but it is omitted from the visible board and counts while unavailable.
That is not a substitute for `disabled_models`: a connected, disabled model remains visible in
the Disabled table until enabled.

## Preset JSON

`GET /api/model-presets/{id}` (and every create/update response) returns the full preset:

```json
{
  "id": "bf4c1740-5e3f-402f-8129-2385787824c9",
  "name": "production board",
  "description": null,
  "scope": "global",
  "owner_user_id": null,
  "team_id": null,
  "is_default": false,
  "revision": 3,
  "layout": {
    "schema_version": 1,
    "auto_include_new": true,
    "groups": [
      {
        "id": "provider:anthropic",
        "name": "Anthropic",
        "provider": "anthropic"
      },
      {
        "id": "g-fast",
        "name": "Fast models"
      }
    ],
    "items": [
      { "provider": "anthropic", "id": "claude-sonnet-4-5", "position": 0, "visible": true },
      {
        "provider": "openai",
        "id": "gpt-5.1",
        "group": "g-fast",
        "position": 1,
        "visible": true
      },
      {
        "provider": "openai",
        "id": "gpt-4.1",
        "position": 0,
        "visible": false,
        "last_visible_position": 2,
        "hidden_at": 1760000000
      }
    ]
  },
  "created_by": "admin@example.com",
  "created_at": 1760000000,
  "updated_at": 1760000100
}
```

`GET /api/model-presets` returns list rows: the same object **without `layout`**, plus
`entry_count` (all items) and `visible_count` (items with `visible: true`). Both counts are
`0` for a document with no explicit items — the seeded default merges the live catalog and
stores nothing of its own.

```json
{
  "data": [ { "id": "…", "name": "production board", "revision": 3, "is_default": false,
              "entry_count": 12, "visible_count": 11, "…": "…" } ],
  "active_id": "bf4c1740-5e3f-402f-8129-2385787824c9",
  "default_id": "00000000-0000-0000-0000-000000000001"
}
```

## Layout document

| Field | Rule |
| --- | --- |
| `schema_version` | exactly `1`; anything else is `400 layout_invalid` |
| `auto_include_new` | default `true`; when true the registry merges models the document does not name |
| `groups` | at most 500; `id` ≤ 64 chars and unique, `name` ≤ 80 chars and non-empty, `provider` ≤ 64 chars |
| provider group | `provider` set; the id must be exactly `provider:<provider>` and each provider appears once |
| free-form group | no `provider`; its id must not start with `provider:` |
| `items` | at most 20,000; `provider` ≤ 64 chars, `id` ≤ 200, both non-empty after trim |
| `item.group` | optional; must name a group. A provider group only accepts items of that provider |
| `(provider, id)` | unique across the whole document, in either visibility group |
| `position` | dense and unique from 0 **inside each visibility group** (visible rows `0..n`, hidden rows `0..m`) |
| `visible` | omitted means `true` |
| `last_visible_position` | optional restore memory: the slot the item had in the visible group before it was hidden; below 20,000 |
| `hidden_at` | optional Unix seconds when the item was hidden; omitted while it is visible |

`last_visible_position` and `hidden_at` are **client-owned restore memory**: the store
validates their bounds and otherwise does not interpret them. The dashboard writes both when
a row is hidden and clears them when it comes back, so a reload still restores a hidden row to
its old slot. A row with no remembered position is appended.

## Endpoints

| Method and path | Body | Response |
| --- | --- | --- |
| `GET /api/model-presets` | — | `{ data, active_id, default_id }` |
| `GET /api/model-presets/active` | — | the preset this caller resolves to (layout included) |
| `GET /api/model-presets/{id}` | — | full preset |
| `POST /api/model-presets` | `{ name, description?, layout? }` | `201` full preset |
| `PATCH /api/model-presets/{id}` | `{ name?, description? }` (`null` clears the description) | full preset; metadata never bumps `revision` |
| `PUT /api/model-presets/{id}/layout` | `{ revision, layout }` | full preset with the new revision |
| `POST /api/model-presets/{id}/duplicate` | `{ name }` | `201` full preset with a new id |
| `POST /api/model-presets/{id}/default` | — | full preset; demotes the previous default in one transaction, revision untouched |
| `DELETE /api/model-presets/{id}` | — | `{ deleted: true, id }` |
| `POST /api/model-presets/{id}/activate` | — | `{ active_id, persisted }` |

Details that matter:

- **Admin gate** — the library routes live on the admin router. `activate` is a per-account
  preference on the session router: any signed-in account may call it (`x-rrouter-csrf: 1`
  required), and a caller with no dashboard session (admin token, gateway key, loopback) gets
  the default back with `persisted: false`.
- **Compare-and-swap** — a layout save whose `revision` is stale fails with `409`, code
  `preset_conflict`, and the body names the current revision:

  ```json
  { "error": { "message": "layout revision 7 is current; re-read the preset and retry",
               "type": "conflict_error", "code": "preset_conflict", "current_revision": 7 } }
  ```

  Nothing is written on that path.
- **Delete** — the default preset is refused before the delete with `409 preset_is_default`.
  Make another preset the default first.
- **Create** — an absent `layout` is the compatibility default
  `{ "schema_version": 1, "auto_include_new": true, "items": [] }`.
- **Validation errors** — `400 invalid_request` (name missing, `revision` missing, `layout`
  missing), `400 layout_invalid` (bad schema version or a document the store would not
  persist), `404 preset_not_found`.
- **Limits** — name ≤ 80 characters, description ≤ 500, 20 layout snapshots kept per preset
  (older revisions are pruned on write); `created_by` names the account that created the
  preset, and each snapshot records who wrote that revision.

## UI workflow

1. **Models → preset selector** — picks the preset for this account (`?preset=` mirrors it in
   the URL). Switching rewrites no routing; a genuinely unsaved draft prompts
   *Save and switch* / *Discard and switch* / *Cancel*.
2. **Edit** — reorder (drag handle, or focus it and press space, arrows, space), hide a row,
   restore it from the hidden section, restore all, or toggle *include new models
   automatically*.
3. **Groups** — **manage groups** opens the group board: create a free-form group, rename
   provider or free-form groups, move a group up/down, or delete a free-form group. The row
   menu's **Move to group** assignment moves one model; a deleted group's models fall back to
   their provider bucket. Opening the board materializes the implicit provider buckets so
   they can be renamed and ordered.
4. **Save** — every change lands in a local draft and persists as a full-layout `PUT` 250 ms
   after the last edit, carrying the revision the screen read. The header shows *saving…*,
   *saved · <time>* or *unsaved changes*.
5. **Conflict** — `409` keeps the draft and pauses saving; the operator chooses *Load latest*
   (drop the draft) or *Save as copy…* (create a preset from the draft).
6. **Failure** — any other error rolls the draft back to the last accepted layout and says so
   inline; the table never keeps an order the gateway rejected.
7. **Lifecycle** — create, rename, duplicate, make default, activate, delete from *Manage
   presets*. Delete is disabled for the default preset.

## Accessibility

Dragging is an enhancement, never the only way to act:

- each row has a dedicated sortable handle (`aria-roledescription="sortable"`, named
  *Reorder \<ref\>*); the drag sensors are pointer, touch (delayed, so a scroll swipe is not a
  drag) and keyboard (space to lift, arrows to move, space to drop, escape to cancel);
- a live region announces the picked-up row, the position it is over, and where it landed;
  focus stays on the handle after a drop;
- hide, restore, add and the auto-include toggle are buttons, reachable without a pointer;
- reduced motion removes the drag overlay animation and row transitions;
- the conflict banner and the unsaved-changes prompt are dialogs, not toast-only.

## Acceptance

- `scripts/model_presets_smoke.py` — API half: creates a preset, exercises layout save
  (including a stale-revision `409`), duplicate, activate, default promotion and its delete
  protection, asserts the routing registry is unchanged, and restores the original active and
  default presets. An older build that does not serve the routes exits `3` with
  `BLOCKED-BY-API`.
- Browser half:

| # | Check | Expected |
| --- | --- | --- |
| 1 | Drag a row to a new position, reload | New order persists; revision advanced; indicator returns to *saved* |
| 2 | Keyboard-only reorder (focus the handle, space, arrows, space) | Same order change; focus stays on the handle; the live region names the new position |
| 3 | Hide a model, then `GET /api/models` | Model leaves the active list; the registry still lists it (not disabled); Restore returns it to its remembered slot, including after a reload |
| 4 | Switch presets | Active list, groups, hidden section and counts follow the selection; the choice survives reload (`?preset=` + per-account preference) |
| 4b | Create a group, move two models in, reorder it above a provider group, reload | The group name/order and assignments persist; `/v1/models` reports the same group id, name and order |
| 4c | Rename a provider group and move it below a custom group | The provider bucket keeps its models; `/v1/models` shows the new name and position |
| 5 | Edit, then switch preset | Unsaved-changes prompt; Cancel keeps the draft, Discard drops it deliberately |
| 6 | Two tabs: save in A, then edit in B | B shows the conflict banner, keeps its draft, offers *Load latest* / *Save as copy*; the stored layout is A's |
| 7 | Make default | The chosen preset becomes the instance default, the previous one can then be deleted, and the control disables on the current default |
| 8 | 390 px viewport | Preset bar and actions wrap without horizontal scrolling; cards keep the handle, hide and restore reachable |
| 9 | Screen reader pass | Rows are named, handles are labelled buttons, focus is visible, no drag-only action |

## Operational notes

- Preset state is server-side with the rest of the admin configuration; the dashboard keeps
  only a draft and cannot become a second source of truth.
- Revisions are per preset. Concurrent saves never overwrite each other: the loser gets
  `409 preset_conflict` and resolves it with *Load latest* or *Save as copy*.
- The library is global in this release (`scope: "global"`); the team/owner columns exist so
  sharing needs no migration.
- Presets are a view. Nothing in this document changes provider connections, keys, aliases or
  the disabled-model list.
