# Task L1 — dashboard layout: persistent sidebar shell

You own `crates/rrouter-gateway/assets/index.html`, `assets/app.js`, `assets/style.css`.
Do not modify any Rust file.

## Target
Replace the top tab bar with a **persistent left sidebar** plus a content shell.

```
┌──────────────┬──────────────────────────────────────────┐
│  brand/logo  │  panel header (title + actions)          │
│              ├──────────────────────────────────────────┤
│ ● Connections│                                          │
│   API Keys   │   scrollable panel content               │
│   Aliases    │                                          │
│   Combos     │                                          │
│   Usage      │                                          │
│   Health     │                                          │
│              │                                          │
│ ── extension │                                          │
│  panels…     │                                          │
│              │                                          │
│  status dot  │                                          │
└──────────────┴──────────────────────────────────────────┘
```

## Requirements
1. **Shell layout**: `display: grid` with a fixed-width sidebar column (~220px) and a
   `1fr` content column; the whole shell is `height: 100vh` with `overflow: hidden`; only the
   content column scrolls. Header and sidebar stay put. No page-level scrollbar.
2. **Sidebar contents**: brand at top (honours the white-label `brand_name`/`brand_color` /
   `logo_url` already applied at boot), the built-in panels in their existing order, a
   separator and then any `extension` panels from `/api/ui/panels`, and the live `/health`
   indicator pinned at the bottom.
3. **Active state**: the current panel is visually distinct (left accent bar + brighter text)
   and carries `aria-current="page"`. Keep the existing hash routing (`#/connections`) so
   reloads and deep links land on the right panel; if none is set, default to the first panel.
4. **Keyboard**: the sidebar is a `<nav>` with real `<button>`s; `Tab` reaches each item,
   `Enter`/`Space` activates. No focus traps.
5. **Collapse on narrow screens**: below 900px the sidebar collapses to an icon-only rail
   (~56px) and expands on hover or when it has focus within. Below 600px it becomes a
   slide-over opened by a menu button in the content header, closable with `Escape` and by
   clicking the scrim.
6. **Content header**: panel title on the left, the panel's own primary action(s) on the
   right (e.g. Connections: the provider form already lives inside the panel — leave that
   as is; do not move panel logic into the shell).
7. **No behaviour regressions**: every existing panel (Connections incl. OAuth sign-in,
   API Keys, Aliases, Combos, Usage, Health) must still work exactly as today. Panel
   functions keep their current signature (`async (panel) => {}`).
8. No build step, no CDN, no dependencies. Plain ES modules. Keep `app.js` under ~420 lines.

## Verification (paste summarized output)
- `node --check crates/rrouter-gateway/assets/app.js`
- `cargo test -p rrouter-gateway` green (assets are `include_str!`-embedded, so a
  non-compiling asset breaks the build).
- `cargo clippy -p rrouter-gateway --all-targets -- -D warnings` clean.
- Start the binary on a spare port and paste the `ariaSnapshot`-equivalent structure you
  verified: sidebar nav with 6 items, active item marked, content region present. If you
  cannot run a browser, say so explicitly and instead print the rendered HTML skeleton by
  `curl`-ing `/` and confirming the nav element order.
