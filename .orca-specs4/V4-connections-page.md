# V4 — Connections page (Vite app)

Implement `ui/src/pages/connections.tsx` for the new Vite dashboard. It must make
signing in a provider a first-class flow, matching the OpenRouter density.

## Files you own

- `ui/src/pages/connections.tsx` (create)
- `ui/src/components/oauth-dialog.tsx` (create)
- `ui/src/lib/queries.ts` — only to add a hook that is missing.

Do NOT touch `ui/src/main.tsx` (the coordinator wires the route), `app.tsx`,
`index.css`, other pages, or anything in `crates/`.

## API

- `GET /api/connections` → `Connection[]` (see `ui/src/lib/api.ts`).
- `POST /api/connections {provider, auth_type:"api_key", name?, credentials:{api_key, base_url_override?}}`
- `DELETE /api/connections/{id}`
- `POST /api/connections/{id}/test` → `{ok, status, latency_ms}` or an error
  envelope.
- `GET /api/oauth/providers` → `[{id, display, flow}]`
- `POST /api/oauth/{provider}/start` → `{id, provider, flow, challenge}` where
  challenge is `{kind:"pkce", authorize_url, redirect_uri}` or
  `{kind:"device_code", verification_uri, user_code, expires_in, interval}`.
- `GET /api/oauth/{provider}/{id}` → `{id, provider, pending}`
- `POST /api/oauth/{provider}/complete {id}` → creates the connection
- `DELETE /api/oauth/{provider}/{id}` cancels.

## The page

### Layout

Header `Connections`, subtitle `Accounts this gateway can route through.` Action:
`add connection` (modal).

Card 1 — `Providers`: one tile per provider from `/api/models/providers`, showing
display name, whether a connection exists, and a primary action: `sign in` for
OAuth providers (`/api/oauth/providers`), `add key` otherwise. This is the
fastest path for a first install, so it sits above the table.

Card 2 — `Accounts`: table with provider, name/email, auth type, tokens present
(`access`/`refresh`/`key` badges), priority, state (active / off / backoff N),
actions: `test` (inline result: `ok · 200 · 812ms` green, or the upstream error
in red), `disable/enable` (priority edit not required), `delete` (confirm).

### OAuth dialog (`components/oauth-dialog.tsx`)

- Open with `window.open(authorize_url)` for PKCE, show the redirect URI so the
  user can complete a manual paste flow if the loopback is blocked; a `complete`
  button calls `POST /api/oauth/{provider}/complete`.
- Device code: show `user_code` large, a copy button, a link to
  `verification_uri`, and poll `GET /api/oauth/{provider}/{id}` every
  `max(interval,2)`s until `pending` is false, then call `complete`.
- While open, poll status; on success invalidate `qk.connections` and close with
  a toast line in the page.
- Errors render inside the dialog, never as an alert.

## Rules

- Every field with a model is a select (this page has none; do not add one).
- Never display secret material: the API only returns `has_*` booleans.
- No mock data, no placeholder rows.

## Verification (required)

- `npm run build` clean.
- Live browser run against `http://127.0.0.1:20128/app/connections` (coordinator
  wires the route) or local `npm run dev` with a temporary route edit you revert.
  There is a real codex OAuth connection: `test` must return a real latency, and
  the providers grid must show codex as connected.
- Report: what you clicked, what the server logged, and the measured values.

Commit in your worktree.
