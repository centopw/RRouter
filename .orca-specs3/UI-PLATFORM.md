# Dashboard platform decision

Status: decided 2026-11-06. Supersedes ad-hoc growth of `assets/app.js`.

## Constraints

1. The dashboard is **never** on the `/v1` data path. Its weight cannot slow a
   proxied request, but a broken dashboard must not be able to take the proxy
   down either.
2. One binary today serves `include_str!` assets — no CDN, no build step, no
   network at boot. That property is worth keeping for the default install.
3. The UI is becoming interactive (model registry with filters, inline test,
   capability editing, ordered multi-selects). Hand-rolled DOM in one 18 KB file
   grows superlinearly at this point.
4. Panels are already data (`/api/ui/panels`), and third-party panels already run
   in a sandboxed iframe with a token. That is the plugin boundary; keep it.

## Decision

**Two layers, one build-free default.**

### Layer 1 — embedded shell (default, always present)

- Preact 10 + `htm` + `@preact/signals-core`, vendored as ESM under
  `assets/lib/`. Roughly **+5 KB gzip**, no bundler: native `import()` per panel
  splits code without a build step.
- `assets/` becomes:

  ```
  index.html
  app.js                    shell: routing, sidebar, panel loader, error boundary
  lib/{preact.js,htm.js,hooks.js,signals.js}
  ui/{table.js,model-select.js,chips.js,form.js,dialog.js}
  panels/{connections,keys,models,aliases,combos,usage,health}.js
  style.css
  ```

- Every panel is a module default-exporting `render(root)`. A panel that throws
  renders an error card in its own pane; the shell keeps working.
- One shared `store.js` holds `/api/models`, connections, and health; mutations
  invalidate precisely those keys. No polling: `/api/events` (SSE) pushes health
  and usage updates.

### Layer 2 — optional rich app (separate artifact, same origin)

- `crates/rrouter-ui/` (Vite + Solid or React) built to `dist/`, served at `/app`.
- The gateway serves it only when the dist directory exists (`RROUTER_UI_DIR`);
  otherwise `/` keeps using the embedded shell. Same origin, so auth and cookies
  stay simple; a separate host would need a second CORS/token story for no gain.
- Both layers consume the **same frozen API contract**. The UI is a client; a
  second client must never force an API change.

## Rejected

| Option | Why not |
|---|---|
| Keep hand-rolled vanilla | Already 18 KB and about to grow; every new control re-implements binding, focus, and cleanup. |
| HTMX / server-rendered fragments | Model registry is interaction-dense (filters, inline test, ordering); fragment round trips multiply and the state still lives client-side. |
| Next.js / SSR React | Second runtime and second deployable for a seven-panel admin; no benefit over a static SPA. |
| Full React SPA as the only UI | ~45 KB gz runtime for a dashboard that must load instantly on a LAN box. |

## Performance budget (measured, not asserted)

- Shell: **≤ 50 KB gzip** total JS+CSS on first load; each panel ≤ 30 KB gzip.
- Repeat load: near-zero bytes — content-hashed filenames plus
  `Cache-Control: public, max-age=31536000, immutable`.
- TTI < 200 ms locally, < 1 s on a throttled 3G profile.
- No layout shift on panel switch; no full-page reload for any action.
- Check with `tab.vitals()` before and after each migration step and record both
  numbers in the PR description.

## Migration (incremental; the API contract never changes)

1. M2 ships the Models panel in the current vanilla shell.
2. Vendor Preact and move the shell to it; panels stay as they are but become
   modules. Ship only after the Models panel is green.
3. Migrate one panel per PR, cheapest first: health → keys → aliases → combos →
   connections → models → usage. Each PR must keep the budget above.
4. Only if someone actually needs it: add `rrouter-ui` (Layer 2).

## Customization

- `GET /api/ui/panels` gains `size` and `permissions` per panel; layout order and
  hidden-ness are stored in settings (`ui.layout`), so customization needs no
  rebuild.
- Theme comes from CSS variables in `style.css`, overridable per deployment.
- Third-party panels stay sandboxed iframes. They never receive DOM access to the
  shell, and the shell never imports their code.
