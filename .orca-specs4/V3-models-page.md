# V3 — Models page (Vite app)

Implement `ui/src/pages/models.tsx` for the new Vite dashboard. Backend already
exists (merged): `/api/models`, `/api/models/providers`, `/api/models/sync`,
`/api/models/fetch`, `/api/models/custom`, `/api/models/test`, and the disabled
toggles.

## Files you own

- `ui/src/pages/models.tsx` (create)
- `ui/src/components/` — you may add ONE new file if a component is genuinely
  missing (e.g. `capability-chips.tsx`); do not edit existing components.
- `ui/src/lib/queries.ts` — only to add hooks if one is missing.

Do NOT touch `ui/src/main.tsx` (the coordinator wires the route), `ui/src/app.tsx`,
`ui/src/index.css`, `ui/src/pages/*.tsx` other than yours, or anything in `crates/`.

## Design language

Study the existing pages (`activity.tsx`, `keys.tsx`, `aliases.tsx`) and the kit in
`ui/src/components/ui.tsx`, `table.tsx`, `charts.tsx`. Match OpenRouter's density:
near-black surfaces, hairline borders, one lime accent, tabular numbers, muted
secondary text, no drop shadows beyond modals.

## The page

Header: title `Models`, subtitle `Every model this gateway can route, with the
capabilities it advertises.` Actions: provider filter select, text filter input,
`sync models.dev` button, `fetch` button.

- Provider filter options come from `GET /api/models/providers`.
- `sync`: `POST /api/models/sync`; on success show `synced {models} models over
  {providers} providers` and the relative time of `synced_at`; invalidate
  `qk.models`.
- `fetch`: needs a connection. Show a connection select (from
  `GET /api/connections`, filtered to the chosen provider). Disabled with a
  title explaining why when that provider has no connection or is not listed.
  `POST /api/models/fetch {connection_id}` then invalidate models and show
  `{added} new of {count} listed`.
- Table columns: provider, model (`ref` in mono), context, max out, source badge,
  capabilities (chips), enabled toggle, `test` action.
- Capability chips: show the true capabilities only, each with a `title`; when
  none are true render `—`. Use the nine keys from the type
  `Capabilities` in `ui/src/lib/api.ts`.
- Enabled toggle: `PUT /api/disabled/{encodeURIComponent(ref)}` and
  `.../enable`; optimistic UI is fine but must revert on error.
- Test: `POST /api/models/test {model: ref}`; render `ok · {status} · {ms}ms ·
  {reply|error}` inline in that row (green/red). One test at a time per row is
  enough; disable the button while pending.
- `add custom model` opens `Modal` with: provider select (from providers),
  model id (free text — the only legitimate free-text model field in the app),
  display, context, max out, and a checkbox per capability key. Submit
  `POST /api/models/custom`; invalidate models. Custom rows get a `delete`
  action (`DELETE /api/models/custom?provider=&id=`).

Sort by provider then id. Search filters on ref and display, case-insensitive.
Row count shown in the card header.

## Verification (required)

- `npm run build` in `ui/` must pass with zero TypeScript errors.
- Run the real gateway (`./target/release/rrouter` on port 20128; it is already
  running with a live codex connection) and verify in a browser at
  `http://127.0.0.1:20128/app/models` **after** the coordinator wires the route —
  or, if not wired yet, verify against `npm run dev` (vite proxies /api to 20128)
  by temporarily adding the route locally; revert that edit before committing.
- Prove: rows render with chips, provider filter narrows, search narrows, sync
  updates the count, test returns a real result for `codex/gpt-5.6-luna`, custom
  add appears then deletes.
- Report measured numbers (row counts, filter results, test latency) and any
  contract mismatch.

Commit in your worktree.
