# Task: Account quota tracking — dashboard UI

Repo: /Users/cento/git/personal/RRouter, dashboard in `ui/` (React + Vite + TS, design tokens in
`ui/src/index.css`, shared components in `ui/src/components/`, api types in
`ui/src/lib/api.ts`, react-query hooks in `ui/src/lib/queries.ts`). Read
`ui/src/pages/providers.tsx`, `ui/src/pages/keys.tsx` (limits modal is the closest pattern)
and `ui/src/components/` before writing anything. The backend lands in parallel against the
contract below, so it will not be running while you work: typecheck only.

## Backend contract (being built in parallel; do not edit Rust)

- `GET /api/accounts/quotas` -> `{ quotas: AccountQuotaRow[] }`, one row per connection:

```ts
export interface AccountQuotaRow {
  connection_id: string;
  name: string | null;
  provider: string;
  is_active: boolean;
  window: "daily" | "weekly" | "monthly";
  max_requests: number;      // 0 = unlimited
  max_tokens: number;        // 0 = unlimited
  max_cost_micros: number;   // 0 = unlimited
  requests: number;
  tokens: number;            // input + output
  cost_micros: number;
  exhausted: boolean;
  resets_at: number | null;  // epoch seconds, null for an all-time quota
}
```

- `PUT /api/accounts/quotas/{connection_id}` body
  `{ window?: "daily"|"weekly"|"monthly"; max_requests?: number; max_tokens?: number; max_cost_micros?: number }`
  -> `{ quota: AccountQuotaRow }`. Missing fields keep their current values.
- `DELETE /api/accounts/quotas/{connection_id}` -> 204 (idempotent).

## Work

1. `ui/src/lib/api.ts`: the types above plus `getAccountQuotas()`, `updateAccountQuota(id,
   patch)`, `clearAccountQuota(id)` next to the existing key/connection helpers, reusing
   whatever request helper that file already uses.
2. `ui/src/lib/queries.ts`: `useAccountQuotas()` (`queryKey: ["accounts", "quotas"]`) and
   mutations for update/clear that invalidate it, matching the existing mutation style.
3. `ui/src/pages/providers.tsx`: under the connection list add an **Account quotas** card
   (same Card/Table components as the neighbouring sections). One row per connection:
   - name (fall back to the connection id's first 8 chars) and provider, `active`/
     `inactive` badge, and an `exhausted` badge with "resets in Xm" built from `resets_at`
     when exhausted (a tiny helper is fine; no dependency).
   - usage vs limit for each non-zero limit only, in one muted sentence per dimension using
     `formatTokens`-style helpers already in the repo (search for them) and `formatUsd` for
     cost: `requests 120/1,000 · tokens 1.2M/5M · $0.40/$2.00`. When every limit is 0 show
     `unlimited` and still show what was spent in the window (`$0.00 · 1.2M tokens · 12
     requests`).
   - `edit` opens a modal (copy the modal + Segmented patterns from `keys.tsx` / existing
     modals): WINDOW segmented control (`daily`/`weekly`/`monthly`) with a "UTC calendar
     window" hint, three numeric inputs (`max_requests`, `max_tokens`, `max_cost_micros` entered as
     dollars with step 0.01, converted with the same rounding helper used for key budgets —
     search the file), each with `empty = unlimited` hint. Save PUTs only the changed fields;
     `clear` (visible only when a quota row exists, i.e. any limit non-zero or window not
     `day`) DELETEs after a confirm like other destructive actions.
   - Keep it disabled/hidden when there are no connections: show the existing empty state
     wording instead of an empty table.
4. Empty and error states follow the page's existing conventions. No new dependencies.

## Verification

`cd ui && npx tsc -b --noEmit` must exit 0. Do **not** run `npm run build` (the coordinator
rebuilds and commits `ui/dist` after the backend lands), do not start servers, no commits.
Files: `ui/src/lib/api.ts`, `ui/src/lib/queries.ts`, `ui/src/pages/providers.tsx` (plus at
most one new component file if the page genuinely needs it). Report the row layout and the
PUT/DELETE shapes you send.
