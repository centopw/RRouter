# RRouter · Activity page redesign

Spec for implementing the redesigned **Activity** page (`ui/src/pages/activity.tsx`).
Written for engineers and coding agents. Read it top to bottom once, then use the
checklist at the end.

- Visual reference: [`reference/`](./reference) (Design canvas source, see "Reference files")
- Live mockup: https://claude.ai/artifact/1aKvVZsTfKbdNRv17DvGh4 (private, owner only)
- Stack: React 19, Tailwind v4 (`@theme` tokens in `ui/src/index.css`), TanStack Query, no chart library

> **Theme update:** the look is now light, not dark. All colour values come from [THEME.md](./THEME.md); the dark hexes named in section 4 are historical. Accounts and sign-in are specified in [AUTH.md](./AUTH.md).
>
> **Status:** the Activity page was implemented in commit `5877d33`, and the backend gaps listed in section 6 were closed in that commit
> (per-model series, 5m buckets, `hours=` scoping, request ids). Treat section 6 as history. First-run setup is specified separately in [ONBOARDING.md](./ONBOARDING.md).

## 1. Rules for anyone implementing this

1. **Use the repo's existing brand tokens and components.** The mockup was drawn with
   IBM Plex and a teal accent; **do not copy those**. Section 4 maps every mockup colour/font to
   an existing token. Never add a raw hex to a component.
2. **Reuse before you build.** `Card`, `CardHeader`, `Badge`, `Dot`, `Button`, `Select`, `Tabs`,
   `CopyChip`, `EmptyState`, `ErrorNote`, `PageHeader` (`components/ui.tsx`), `Sparkline`, `Bars`,
   `Metric` (`components/charts.tsx`) and `DataTable` (`components/table.tsx`) already exist. Extend
   them when the need is shared; create a new component only for the pieces listed in section 5.
3. **Do not invent data.** Anything the API cannot supply today is listed in section 6 with the
   fallback to ship. Do not fake numbers client-side.
4. **Scope:** the Activity page, plus the small shell change in 5.10. Other pages (Providers, Models,
   Keys, Aliases, Combos, Health) are out of scope; they should inherit the new tokens/components
   only where those are shared.
5. No new dependencies. Charts stay hand-rolled SVG/CSS like `charts.tsx`.

## 2. Why the page changes (UX intent)

The current page shows six equal-weight tiles, two charts that show the same data, and a sparse
chart that is a single bar when traffic is young. It tells you *numbers*, not *what is happening*.

The redesign is one top-to-bottom story:

| Question the user has | Where it is answered |
|---|---|
| Is everything OK right now? | Alert banner (only when something fails) + the four readouts |
| How much, how fast, how often does it fail? | Readouts: Requests, Tokens, Avg latency, Error rate |
| When did it happen? | Traffic chart with error lane and tooltip |
| Which model is responsible? | Models table (share bars, latency, errors) |
| What exactly happened in one request? | Requests log, then the detail panel |

Principles to preserve when you make trade-offs:

- **Everything cross-links.** Click a model, the log filters. Click a request, the detail opens.
  Click "View failing requests" on the banner, **Logs** opens filtered to that model's errors.
- **Nothing is shown twice.** "Top models" and "Traffic by model" merge into one table.
- **Problems get louder than health.** Green is quiet (a small dot). Red is the only thing that
  earns a banner, a coloured stat and an error lane.
- **Empty is a guide, not a blank.** A zero-traffic gateway shows how to send the first request,
  not four dashes. A missing cache reading is one explanatory line, not an empty tile.
- **Honest sparse data.** Zero buckets render as a 2px stub, never as nothing. When all traffic
  sits in one bucket the page offers "Zoom to 1h".
- **Numbers are monospace and tabular** so columns line up and values do not jitter on refresh.

## 3. Page anatomy

Content column: `max-width: 1160px`, centred, padding `32px 40px 56px` (`16px` side padding below 820px).
Vertical rhythm: header 24px, then panels separated by 20px.

```
Header            title + subtitle | live status + range control
[Alert banner]    only when a model is failing
Overview panel    4 readouts, Traffic chart, footer strip
Models panel      table, row = filter
Requests panel    filter chips, log table | detail panel, footer
```

### 3.1 Header
- `h1` "Activity" 30px / 600 / -0.02em. Subtitle one line: "What this gateway is serving, how fast, and whether anything is failing."
- Right side: live indicator (pulsing 8px dot + "Live · updated just now" / "Updated 4s ago"), then a
  **segmented control** for range: `1h  24h  7d  30d` (replaces the `<Select>`). Default `7d`.
- Pulse animation must be disabled under `prefers-reduced-motion`.

### 3.2 Alert banner (conditional)
Shown when any model in the window has an error rate above 20% **and** at least 3 failures (tune later).
- Red-tinted surface: border `neg` at ~30% alpha, background `neg` at ~8% alpha over `bg`. `role="alert"`.
- Title: `<provider>/<model> is failing`, or `<model> is failing` when no provider was
  recorded. Body: one sentence with counts and the dominant status (for example "5 of its 9
  requests returned 429"), plus one actionable hint.
- Actions: primary **View failing requests** (opens **Logs** with the error status, failing
  model and current range preselected), plus **Open providers** when the incident names a
  provider. A close control dismisses the banner for the tab; the dismissal clears once the
  condition resolves, so a later recurrence alerts again.
- Only one banner: the worst model.

### 3.3 Overview panel
**Readouts**: CSS grid `repeat(auto-fit, minmax(150px, 1fr))`, 1px gaps over `line`, each cell padding `20px 24px`.

| Readout | Value | Second line |
|---|---|---|
| Requests | count | `N succeeded · M failed`, or "All succeeded" |
| Tokens | input + output | `~X per request` |
| Avg latency | `1.89s` (2 decimals, seconds) | `Slowest: <model> <latency>` |
| Error rate | `0.0%` | dot + "No failures" / "M of N failed" |

- Value: mono, 34px, 500, -0.02em. Label 13px `muted`. Second line 12.5px `faint`.
- Error rate value turns `neg` and its dot turns `neg` when errors exist; otherwise the dot is `pos`.
- Each readout may show a delta vs the previous window (the current page already computes `prev`).
  Render it in the second line as `▲ 12%` / `▼ 8%` in `muted`; colour it only for error rate
  (up = `neg`) and latency (up = `warn`). Skip the delta when the previous window has no data.
- Cache hit (existing metric) is **not** a tile. Show it as a fifth readout only once
  `cacheRead > 0`; until then use the footer strip note (below).

**Traffic chart** (header row: `Traffic` + segmented `Requests | Tokens`, legend on the right):
- Plot height 200px, three gridlines (0, 50%, 100%), y labels in mono 11px `faint`, "nice" max
  (1/2/5 x 10^k) so the axis is stable.
- One column per bucket; bars `max-width: 56px`, centred in the column, top corners 3px, 1px
  `surface`-coloured divider between stacked segments. Minimum visible segment 3px.
- Stacked **by model** using the series palette (section 4.2). Legend lists only models present in the window.
- **Error lane**: 6px-high strip under the plot, one cell per bucket, `neg` when that bucket has
  errors, a faint `rule` colour otherwise. Row label "Errors" aligned with the y-axis column.
- **X labels** every Nth bucket (1h: every 3rd, 24h: every 6th, 7d: all, 30d: every 5th); last label
  reads "Today" on day ranges. Bucket sizes: 1h = 5 min x 12, 24h = 1 h x 24, 7d = 1 day x 7, 30d = 1 day x 30.
- **Tooltip** (CSS-only, no JS state): shows on hover and keyboard focus of a column; when the chart
  is not hovered, the **peak** bucket's tooltip is shown by default. Tooltip sits beside the bar
  (right of it in the left half, left of it in the right half), never over it, `pointer-events: none`,
  208px wide. Content: bucket range, total + unit, one row per model (dot, name, value), red
  "N failed" line when relevant. Columns are `tabindex="0"` with an `aria-label` summarising the bucket.

**Footer strip** (border-top, 13px `muted`): left, the cache note ("Cache hits: none recorded in this
window. They appear once a request reuses an earlier prompt prefix."; omit once hits exist).
Right, the zoom hint ("All N requests arrived in the last hour." + ghost button **Zoom to 1h**) shown when
the range is above 1h and every request falls in the last hour.

### 3.4 Models panel
Header: `Models` + "N in use · ranked by tokens", right hint "Select a model to filter the requests below".
Columns: **Model** (colour square + `provider/` in `faint` + name; a button), **Requests** (right), **Tokens**
(right-aligned number, share bar, percent), **Avg latency** (right), **Errors** (right; `neg` when > 0, `faint` when 0).
- Sort by tokens descending. Row hover `row-hover`; selected row gets `surface-2` background and a 2px
  inset `fg` bar on the left. Clicking the selected model again clears the filter.
- Share bar: 6px, track `rule`, fill = the model's series colour, width = share of total tokens.
- Layout uses CSS grid rows with ARIA roles (`role="table|row|columnheader|cell"`), not `<table>`,
  so rows can be arbitrary components. Wrap in `overflow-x: auto` with a 680px min width.

### 3.5 Requests panel
Header: `Requests` + "Most recent first". Right: filter chips:
- **All N** (selected when no filter), **Errors N** (red outline when N > 0), and, when a model filter is
  active, a removable chip with the model name and an X.
- Chips are pill buttons, 32px high (40px under 820px), selected = `fg` fill with dark text.

**Update (2026-11-07):** the detail is an overlay dialog opened by clicking a row, not an
inline column; the log uses the full panel width and nothing is selected on load.

Historical layout: body is a flex row that wraps: **log** (`flex: 999 1 620px`) and **detail** (`flex: 1 1 320px; max-width: 400px`).

Log columns: **Time** (mono, a button that selects the row), **Model** (dot + name), **Tokens in → out**
(mono, right), **Latency** (mono, right), **Cache** (`hit` / `miss` / `write`, `faint`), **Status** (pill).
- Status pill: `pos` tint for 2xx, `neg` tint for errors, mono 12px, 6px radius.
- Failed rows show `—` for tokens and cache.
- Shows 8 rows by default; footer "Showing 8 of 18" + ghost **Show all 18** / **Show fewer**.
- Filtered to nothing: "No requests match these filters." + **Clear filters**.

Detail panel (`aside`, background slightly darker than the panel, 1px left border):
- Header: request id (mono 12px `faint`), model with colour square (16px/600), close icon button.
- Status pill with text (`200 OK`, `429 Too Many Requests`).
- Errors: tinted box with the error message from the event.
- Success: **Latency** line with value (see 6 for the first-token split, which is optional).
- Key/value list: When, Provider, Endpoint, Tokens (in/out), Cache. Rows 13px with a bottom hairline.
- Actions: ghost buttons **Copy request ID**, **Copy as cURL**.
- Default selection: first visible row. Closing shows "Select a request to inspect its tokens, latency and routing."
  If the selected row is filtered out, fall back to the first visible row.

### 3.6 States

| State | Behaviour |
|---|---|
| Loading | Skeletons for the readouts and the chart area (keep layout height; no spinners). Table rows as 3 skeleton rows. |
| Empty (no requests ever) | Readouts show `0 / 0 / — / —` in `faint`. Chart, Models and Requests are replaced by the onboarding block: "No requests yet", three numbered steps (Add a provider -> Providers, Create an API key -> API Keys, Send a test request), and a cURL block with a **Copy** button that uses the real base URL. Live dot reads "Waiting for the first request". |
| Empty window (traffic exists, not in this range) | Keep the layout, render zero stubs, and show "No requests in this window. Try 30d." with a button that widens the range. |
| Error loading | Existing `ErrorNote` with retry, in place of the failing panel only. |
| Incident | Banner + red readout + error lane + Errors chip highlighted (see 3.2). |
| Filtered to nothing | See 3.5. |

## 4. Tokens

### 4.1 Reuse the existing theme
Do **not** introduce the mockup's hexes. Map them like this:

| Mockup | Use this token (`ui/src/index.css`) |
|---|---|
| page background | `bg` |
| panels | `surface` |
| selected row / input bg | `surface-2` |
| borders | `line` (panels), `rule` (row dividers) |
| row hover | `row-hover` |
| primary text | `fg` |
| secondary text | `muted` |
| tertiary text (>= AA at 11px) | `faint` |
| UI accent / selected control | `fg` fill on dark ink for segmented/chips (as mocked); `accent` for focus rings, links, the live dot and the first series |
| success | `pos` |
| error | `neg` (text on tint: lighten with `color-mix` so contrast stays >= 4.5:1) |
| warning | `warn` |

Fonts stay `font-sans` (Inter) and `font-mono` (JetBrains Mono). Use `.tnum` on every number that
is not already mono.

### 4.2 New tokens (add to `@theme`)
Categorical series colours for models. Assigned by model in a stable order (see below).

```css
--color-series-1: #00d4ff; /* = accent */
--color-series-2: #b58cff;
--color-series-3: #f59e0b;
--color-series-4: #f472b6;
--color-series-5: #3b82f6;
--color-series-6: #94a3b8; /* "other" */
```
- Do not use `pos`/`neg` for series; those are reserved for status.
- Assignment: sort models by all-time tokens (or alphabetically if unavailable) and assign 1..5; everything
  past the fifth is grouped as "Other" (6). The mapping must be **stable across the chart, legend, table and log**,
  so compute it once per page in a `useModelColors(models)` helper.
- Colour is never the only carrier: the legend, table and tooltip always show the model name next to its swatch.

### 4.3 Type scale (px)
Title 30 / 600 · panel title 15 / 600 · readout value 34 mono 500 · body 14 · table body 13.5 · helper text 12.5-13 ·
axis, pills, kbd 11-12. Nothing below 11.

### 4.4 Shape and spacing
Panel radius 14 (existing `--radius-card` is 12; keep 12 unless you also change the other pages), controls 9-10,
pills 6, chips 999. Panel padding 24 horizontally. Row height 48 (52 under 820px). Gaps on an 4px grid.

## 5. Components

Build in this order. All live in `ui/src/components/` unless noted.

| # | Component | Notes |
|---|---|---|
| 5.1 | `Segmented` (`ui.tsx`) | `{ options, value, onChange, ariaLabel }`. `role="group"`, buttons with `aria-pressed`. 30px high (40px < 820px), selected = `fg` bg / `bg` text. Replaces the window `<Select>` here; `Tabs` stays for navigation. |
| 5.2 | `Readout` (`charts.tsx`) | Replaces `Metric` usage on this page: label, mono value, second line with optional dot/delta. Keep `Metric` for other pages if used. |
| 5.3 | `TrafficChart` (`charts.tsx`) | Props: `buckets: { label, full, parts: {id,value}[], errors }[]`, `metric`, `colors`. Implements stacked bars, gridlines, error lane, x labels, CSS tooltip. Generalises `Bars`; migrate `Bars` callers or keep it for them. |
| 5.4 | `ModelTable` (`pages/activity/` or `components/`) | Grid rows + ARIA roles; row button toggles filter. |
| 5.5 | `RequestLog` + `RequestDetail` | Master/detail as in 3.5. Selection state is local to the page. |
| 5.6 | `FilterChip` (`ui.tsx`) | Pill button, `on` / `bad` variants. |
| 5.7 | `AlertBanner` | `role="alert"`, tone `neg`, actions slot. |
| 5.8 | `Onboarding` | Empty block with steps and `CopyChip`-style cURL. Reuse `CopyChip`/`EmptyState` internals. |
| 5.9 | `StatusPill` | Wrap the existing `Badge` (tone `pos`/`neg`, mono 12px) rather than adding a second one. |
| 5.10 | Shell (`app.tsx`) | Move the gateway base URL out of the top bar into a sidebar footer card ("Endpoint", mono text, copy icon button) above "Running · up Xm / vX · MIT". Keep the search box as a "Jump to… ⌘K" button in the sidebar. This is optional for v1; do it after the page lands. Under 820px the sidebar collapses to a top bar with a horizontally scrolling tab row. |

Page state (all in `activity.tsx`, no global store): `range`, `metric`, `modelFilter`, `errorsOnly`,
`selectedId`, `showAll`.

## 6. Data and API mapping

Existing hooks (`ui/src/lib/queries.ts`): `useUsageSeries(hours)`, `useUsageSummary()`, `useUsage(limit)`.
Poll intervals stay as they are (15s events/summary, 30s series).

| UI need | Source today | Gap and v1 fallback |
|---|---|---|
| Readouts, deltas | `useUsageSeries(hours * 2)`, same maths as the current page | none |
| Cache hit | series `cache_read_tokens` etc. (already computed) | none |
| Chart totals per bucket | `UsageBucket` | none |
| **Chart stacked by model** | series is **not** per model | v1: single-colour (series-1) bars with the error lane. v2 (backend): `GET /api/usage/series?hours=&by=model` returning `model` on each bucket. The component API already accepts `parts[]`. |
| **1h range / 5-min buckets** | series bucket size is fixed server-side | v1: ship `24h / 7d / 30d` only and hide `1h`. v2: add `bucket_secs` param. |
| **Models table scoped to the range** | `useUsageSummary()` is **all time** | v1: label the table "all time" and keep it. v2: `GET /api/usage/summary?hours=`. Error rate, latency and requests per model all come from this row type already. |
| Request log | `useUsage(50)` (`UsageEvent`) | Log is the latest N events, not range-scoped. Footer must say "latest 50", not "of {total}". |
| Request id | not in `UsageEvent` | v1: derive a short display id from `ts` + index; hide **Copy request ID** until the backend returns one. |
| Cache hit/miss per request | `cache_read_tokens`, `cache_write_tokens` | `hit` if read > 0, `write` if write > 0, else `miss`. |
| First-token vs generation latency | not recorded | v1: omit the split bar, show the single total. Do not estimate it. |
| Endpoint / provider / connection | `provider`, `connection_id` | Show "Endpoint" only if the backend records it; otherwise drop the row. |
| Error message | `UsageEvent.error` | none |
| Incident detection | derive from summary rows (or events if summary is all-time) | Banner logic lives in a pure function with a unit test. |

Backend changes in section 6 are separate tasks. The frontend must degrade exactly as the fallback
column says and must not wait on them.

## 7. Interaction and accessibility

- Every interactive element is a real `<button>`/`<a>`. No `onClick` on `div`/`span`. Icon-only buttons have `aria-label`.
- Visible focus: 2px `accent` (or `fg`) outline with 2px offset. Never remove outlines.
- Segmented/filter buttons use `aria-pressed`. The selected log row's time button has `aria-pressed="true"`.
- Chart columns are focusable and expose their summary via `aria-label`; the tooltip opens on focus.
- Colour contrast: text >= 4.5:1 (use `faint` for the lowest tier, never darker). Do not rely on colour alone for errors:
  also show the status code text and the "failed" count.
- Touch targets >= 44px under 820px (segmented 40px + padding, chips 40px, rows 52px, icon buttons 44px).
- `prefers-reduced-motion`: no pulse, no transitions on bars.
- Live updates must not steal focus or reset the selected row; when a refetch reorders rows, keep selection by id.
- Keyboard: Tab order is header, banner, readouts (no stops), chart columns, model buttons, chips, log time
  buttons, detail actions. No custom key handlers required.

## 8. Responsive

- >= 1100px: as drawn. The log and detail sit side by side.
- 820-1100px: detail wraps under the log, full width.
- < 820px: sidebar becomes a top tab row; readouts are 2 columns; chart plot stays 200px with fewer x labels;
  tables scroll horizontally inside their panel; header controls wrap. Page padding 16px.
- Never hide data to fit, only reflow or scroll.

## 9. Acceptance checklist

- [ ] Page matches the "live traffic" board for structure, spacing and type (using repo tokens, not mockup hexes).
- [ ] Incident, empty-gateway and phone layouts match their boards.
- [ ] Range control works for the ranges the API supports; unsupported ones are hidden, not disabled.
- [ ] Clicking a model filters the log; clicking it again clears; the chip shows and clears the filter.
- [ ] Errors chip and "View failing requests" both narrow to failures; the banner opens Logs
  with the failing model preselected, and its dismiss control hides it until the incident
  resolves.
- [ ] Selecting a row shows detail; close works; selection survives refetch.
- [ ] Tooltip works with mouse and keyboard; peak tooltip shows by default.
- [ ] Zero buckets show a stub; an all-zero window shows the "No requests in this window" message.
- [ ] No raw hex in `activity.tsx` or new components; new series tokens are in `@theme`.
- [ ] `npm run typecheck` and `npm run build` pass in `ui/`; keyboard-only pass done; contrast spot-checked.
- [ ] No data is faked client-side; each gap in section 6 uses its documented fallback.
- [ ] Pure helpers (bucketing, nice axis max, incident detection, series colour assignment) have unit tests if the repo has a test runner; otherwise they are small exported functions with an inline self-check.

## 10. Suggested implementation order

1. Tokens (`series-*`) and `Segmented`, `FilterChip`, `Readout`.
2. Readouts + header + range control on the existing data (no visual regressions elsewhere).
3. `TrafficChart` with totals-only bars, error lane, tooltip.
4. `ModelTable` and `RequestLog` with the filter links.
5. `RequestDetail`.
6. Banner, empty/onboarding, loading skeletons.
7. Responsive pass, accessibility pass.
8. Shell change (5.10).
9. Backend follow-ups from section 6, then turn on stacked-by-model and the 1h range.

## 11. Reference files

`reference/` holds the Design canvas source exported from the mockup. They are **inspiration for layout, copy
and logic, not code to port**: they use a custom `.dc.html` format that needs the Design runtime to render.

| File | What it shows |
|---|---|
| `Main.dc.html` | The full page: markup, CSS, and the data logic (bucketing, nice max, filtering, selection). The `scenario` tweak switches `live` / `incident` / `empty`. |
| `Incident.dc.html` | Same component, `scenario="incident"` |
| `Empty.dc.html` | Same component, `scenario="empty"` |
| `Mobile.dc.html` | Same component in a 390px frame |
| `canvas.json` | Board sizes and positions |

Sample data in the mockup is invented (18 sample requests that sum to the 18 requests / 613 tokens / 1.89s
avg of the original screenshot; incident numbers; request detail fields). Treat it as fixtures, not facts.

## 12. Non-goals

Per-key or per-user breakdowns, cost estimates, CSV export, saved views, dark/light theme switch, and
any change to Providers/Models/Keys/Aliases/Combos/Health layouts.
