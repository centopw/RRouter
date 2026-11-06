# M2 — Models panel + model selects

Implement the UI half of `.orca-specs3/CONTRACT-models.md`. No Rust logic beyond
the panel registry entry.

## Files you own (do not touch anything else)

- `crates/rrouter-gateway/assets/index.html`, `app.js`, `style.css`
- `crates/rrouter-gateway/src/ui.rs` — ONLY the `builtin_panels()` list

NEVER touch `crates/rrouter-gateway/src/routes/**`, `proxy.rs`, `state.rs`,
`crates/rrouter-store/**`, `crates/rrouter-translate/**`, or any `tests/` file.

## 1. Panel

Add `models` to `builtin_panels()` with `order: 25`, title `Models`, an icon, so
the sidebar order becomes Connections, API Keys, Models, Aliases, Combos, Usage,
Health.

The panel (`models(panel)` in `app.js`) renders:

- toolbar: provider `<select>` filter (from `GET /api/models/providers`), a text
  filter over ids, `sync models.dev` button (POST `/api/models/sync`, then show
  `synced_at` and counts), and `fetch` (POST `/api/models/fetch` with the
  connection id chosen from a second select populated by `GET /api/connections`;
  disabled with a tooltip when the selected provider has no connection);
- table: provider, model (`ref`), context, source, capability chips, enabled
  toggle, `test` button. Enabled toggles call the existing
  `PUT /api/disabled/{model}` and `PUT /api/disabled/{model}/enable` routes;
- `test` posts `/api/models/test` and renders `{ok, latency_ms, error|reply}` in
  the row (green/red, never an alert);
- `add custom model` form: provider `<select>`, model id (free text — this is the
  ONE legitimate model text field, it *defines* a model the provider did not
  list), display name, context, max output, capability checkboxes for all nine
  keys. Submits `POST /api/models/custom`. Custom rows get a `delete` button
  (`DELETE /api/models/custom?provider=&id=`).

Capability chips: show only the true ones, with `title` attributes; render
`—` when none.

## 2. Model select everywhere (STRICT)

Anywhere the dashboard accepts a model the user picks for routing, it MUST be a
`<select>` built from `GET /api/models`, grouped with `<optgroup label="provider">`
and labelled with `ref` (`provider/model`). Free text is allowed only in the
"add custom model" definition field above.

Replace, at minimum:

- Aliases: `target` (currently `input('provider/model')`) → select;
- Combos: `models` (currently `input('m1, m2, m3')`) → ordered multi-select: a
  row of selects with `add` / `remove` / `↑` `↓` buttons, submitted in order;
- the Models panel test target, provider filter, and any other model field you
  find (`grep -n "model" app.js`).

Implementation: one exported helper, e.g.

```js
async function modelSelect({ multi, value, onChange }) // returns an Element
```

Cache `GET /api/models` per page load, invalidate after `sync`, `fetch`, or a
custom add/delete. A select whose stored value is no longer in the list keeps
that value as a selected option marked `(missing)` rather than silently snapping
to another model — an alias pointing at a removed model must stay visible.

Accessibility: every select has an `aria-label`, the table's toggles are real
buttons with text, and the panel keeps the existing `role="tabpanel"` shell
untouched.

## 3. Style

Extend `style.css` with the chips, the multi-select row, and the test result
colors. Reuse the existing tokens and spacing scale; no new palette.

## 4. Verification (required, must be observable)

The backend routes do not exist in your branch yet. Verify the UI against a
throwaway fixture server in `/tmp` (do NOT commit it):

- serve `assets/` plus fake JSON for `/api/models`, `/api/models/providers`,
  `/api/connections`, `/api/ui/panels`, `/health`;
- drive it with a real browser: the panel lists rows with chips, the provider
  filter narrows, the custom form posts and the row appears, aliases and combos
  now use selects, and the missing-value case shows `(missing)`.

Then run `node --check crates/rrouter-gateway/assets/app.js` and
`cargo test -p rrouter-gateway` (the panel-registry change must not break tests;
if a test asserts the exact panel list, update that test — it is yours to touch
ONLY for the added entry).

Report: screenshots or measured DOM assertions for each check, files changed, and
any contract mismatch you hit.
