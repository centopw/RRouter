# Platform design audit: Providers, Models and the shell around them

2026-11-24 · read-only audit (no code changed) · reference direction: Usage Overview.

This audit is about how the pages look and read, not whether they are correct. The two
2026-11-23 passes in `design/providers-redesign.md` and `design/models-combo-redesign.md` fixed
correctness: wrong numbers, false empty states, broken controls. Their closed decisions still
hold: one Models table, one preset board, `rrouter/<name>` identity, pagination over
virtualisation, the drawer at `?inspect=`, and every deep link they list. Nothing here reopens
them.

## How the screenshots were made

- **Setup.** A mock gateway with fake data only, so no real keys or user data were touched.
  The Vite dev server ran from the current `ui/src`, with `RROUTER_URL` pointed at the mock. Pages
  were captured in headless Chrome over CDP, using real device emulation for the 390 px shots.
  Script: `$SHOTS/audit-capture.mjs`. It tears down Vite, Chrome and the mock when it exits.
- **Mock data.**
  - 10 providers, 6 of them connected.
  - 9 accounts: 2 Claude, one of which is in backoff 2 with a 429 error. OpenRouter has a
    free-mode account with a stale `last_error` and a paid account with a spend cap. OpenAI has
    one account the operator disabled and one suspended by the provider. Gemini has one
    team-owned account.
  - Operator quotas and provider plan windows.
  - 25 models, including disabled, deprecated/unavailable, custom, free and unpriced ones.
  - 2 aliases, 2 combos and 1 preset. The preset has a custom group, two hidden rows and three
    rows that are not in the preset.
- `$SHOTS` = `/Users/cento/.local/state/herdr-swarm/rooms/Users-cento-git-personal-RRouter/shots`

| Shot | What |
|---|---|
| `$SHOTS/audit-providers-1440-light.png`, `…-dark.png` | Providers, admin, desktop |
| `$SHOTS/audit-providers-390-light.png`, `…-dark.png` | Providers, 390 px device emulation |
| `$SHOTS/audit-providers-drawer-1440-light.png`, `audit-providers-drawer-390-light.png` | Account drawer |
| `$SHOTS/audit-providers-add-1440-light.png` | Add provider dialog |
| `$SHOTS/audit-providers-reader-1440-light.png` | Providers as a non-admin |
| `$SHOTS/audit-models-1440-light.png`, `…-dark.png` | Models, default view (board, Status = Routed) |
| `$SHOTS/audit-models-390-light.png`, `…-dark.png` | Models, 390 px |
| `$SHOTS/audit-models-all-1440-light.png` | Models, all statuses (flat table) |
| `$SHOTS/audit-models-combos-1440-light.png`, `audit-models-add-1440-light.png` | Combos filter; Add menu |
| `$SHOTS/usage-overview-light.png`, `usage-overview-dark.png`, `audit-usage-1440-light.png` | Reference |

## What Usage Overview gets right

Compare `$SHOTS/usage-overview-light.png` with `$SHOTS/audit-providers-1440-light.png`:

1. **A summary first.** Four KPI tiles answer "am I OK?" before any table.
2. **Cards with the same header.** Each card has an icon, a `text-body` semibold title, quiet
   meta text, and actions on the right ("View all →", a legend).
3. **Status pills with a meaning.** Tinted `Badge` pills: green Healthy, amber Warning, red
   Exhausted. "Fine" and "off" never look alike.
4. **Every column holds data.** Each column is a number with a meter, so there is no wide empty
   column.
5. **One home per job.** Alerts, breakdown and activity each get their own card instead of
   being mixed into the rows.

Providers and Models do none of these five things. That gap is most of why they feel worse.

## Ranked critique

Severity: **P1** misleads, or is a visible bug. **P2** costs scan time on every visit.
**P3** polish.

### P1 — wrong or misleading on screen

1. **Countdowns print "2h 60m" and "11d 24h".**
   - **Seen in.** `audit-providers-1440-light.png` (claude-max "resets in 2h 60m", codex-work
     "8h 60m", opencode "11d 24h") and `audit-providers-drawer-1440-light.png` ("1d 24h").
   - **Cause.** `pages/providers/types.ts:419-425` `until()` uses `Math.round` for the
     remainder. The same idea is written again, correctly with `Math.floor`, as `uptime()` in
     `lib/format.ts:92-99`. `until` is also imported by `pages/usage/quota.tsx` and
     `usage/limits.tsx`, so Usage can show the bug too.
   - **Fix.** Make `until` call one floor-based duration function in `lib/format.ts`.
2. **Money shows four decimals: "$24.1000 / $25.0000".** Usage's spend KPI has the same
   problem ("$42.1694").
   - **Seen in.** `audit-providers-1440-light.png` (openrouter-paid).
   - **Cause.** `lib/format.ts:45`: `usd()` prints `toFixed(4)` for any amount from $0.01 up.
     Four decimals help in a per-request log. They are noise on a cap, a KPI or a total.
   - **Fix.** Add a display variant: 2 decimals from $1, the current rule below $1. Use it for
     caps, KPIs and totals, and keep `usd()` for per-request costs.
3. **"Ready" and "disabled" look the same, and an operator's choice reads as an outage.**
   - **What happens.** Both are a grey dot with grey text (`account-row.tsx:44-49`
     `DOT_TONE.muted`, `types.ts:490-498, 545-547`). Readiness counts accounts the operator
     disabled on purpose as not ready, so:
     - OpenAI shows a red "0 of 2 ready".
     - The card header shows an amber "6 of 9 ready".
   - **Effect.** The page always looks degraded once anyone parks a backup key.
   - **Contrast.** Usage separates `healthy` (green pill) from `inactive` (neutral)
     (`usage/overview-panels.tsx:193-204`).
   - **Seen in.** `audit-providers-1440-light.png` (OpenAI group, header readout).
4. **Hidden rows and "not in preset" rows look the same on Models.**
   - **What happens.** In the all-statuses table, both kinds of row show `/v1` off, a grey
     row and no label.
   - **Why it matters.** Status is the filter the page is built around, yet once you are in a
     mixed view nothing shows which status a row has.
   - **Seen in.** `audit-models-all-1440-light.png` (the last six rows).
   - **Cause.** `pages/models/columns-unified.tsx:100-139`: the `/v1` and enabled toggles stand
     in for status, and there is no status cell.
5. **A stale upstream error is invisible on the row.**
   - **What happens.** openrouter-free has a `last_error` but `backoff_level` is 0, so it reads
     plain "ready". The error only shows after you hover or open the drawer
     (`types.ts:499-507` only shows it under backoff).
   - **Why it matters.** The 11-23 pass set the job of the row as answering "why is this
     account not ready" with zero clicks. This is the one place it doesn't.
   - **Seen in.** `audit-providers-1440-light.png`.

### P2 — hierarchy, density, repeated text

6. **No summary before the inventory.**
   - **Providers.** Above the table there are three lines of prose (`providers.tsx:264-275`;
     a reader gets two paragraphs stacked, `audit-providers-reader-1440-light.png`). The only
     summary is an 11 px amber line in the card corner (`account-row.tsx:610-619`).
   - **Models.** The summary is the subtitle "23 routed (23 available) · 4 free · 29 total"
     (`models.tsx:796-803`). "routed" and "available" are internal words, and the brackets repeat
     the number.
   - **Contrast.** Usage puts the same kind of answer in KPI tiles.
7. **Wide empty space in the Providers table.**
   - **What happens.** At 1440 the Account column is ~530 px wide and holds a 16 px name. The
     priority column holds "p0" in a slot 1 inch from the name, and Limit gets ~290 px. Provider
     header rows leave the Priority and Limit cells blank.
   - **Seen in.** `audit-providers-1440-light.png`.
   - **Cause.** `DESKTOP_GRID` in `account-row.tsx`.
   - **Data left out.** Owner (team or Shared) is computed for every row (`account-row.tsx:372`
     `ownerLabel`) but only appears inside the ⋯ menu and the hover preview. So the team-owned
     gemini-team row looks exactly like a shared one.
8. **The same information appears many times on Models.**
   - **Provider name in every row.** A board section titled "Claude" carries the subtitle
     "provider · claude" (`groups.tsx:35`), then every row inside it repeats a `claude` chip.
     That is 23 chips that add nothing in the default board view.
   - **The group select.** Every row has a full-size select reading "Ungrouped", even under a
     section header that already names the bucket (`columns-unified.tsx:84-91`). That is 20
     identical controls on the default screen. It also reads wrong: rows under "Claude" say
     "Ungrouped".
   - **Two toggles.** `/v1` and `enabled` render as two identical black toggles side by side.
     On alias and combo rows `enabled` is "—".
   - **Seen in.** `audit-models-1440-light.png`, `audit-models-1440-dark.png`.
9. **Models rows are too tall for what they hold.**
   - **Desktop.** Rows are 65–90 px. 23 rows make a 2,700 px page
     (`audit-models-1440-light.png`).
   - **Phone.** Each card is ~230 px, with a full-width group select and 44 px ↑/↓ buttons
     (`table-unified.tsx:799-808`). 23 rows make a 5,984 px page
     (`audit-models-390-light.png`). Reordering is the rarest action on the page but takes
     the most space on a phone card.
10. **Row actions in three visual styles on one table.**
    - **What happens.** Alias and combo rows get an underlined "Test" link, a "hide" pill
      button and an underlined "edit" link (`columns-unified.tsx:150-199`). Model rows get a ▷
      icon and a ⋯ icon (`columns.tsx` actions).
    - **Why it matters.** The column jumps from text to icons halfway down the board.
    - **Seen in.** `audit-models-1440-light.png` (Daily drivers vs Claude sections).
11. **Preset controls take over the Models page header.**
    - **What it holds.** "Preset", a select labelled "Default · default"
      (`preset-header.tsx:558` adds "· default" to a preset that is already named Default), a
      "saved" dot, a Save button that is active when there is nothing to save
      (`preset-header.tsx:587`), "⋯" and "Add".
    - **Phone.** At 390 the controls wrap, and "⋯" and "Add" sit alone on a third line
      (`audit-models-390-light.png`).
12. **The Models filter bar uses five control styles.**
    - **The five.** A text input, two square selects, a pill-dropdown ("Status Routed"), toggle
      chips ("Free 4", "custom"), and "groups", which looks like a filter chip but opens a
      manager.
    - **Phone.** The bar scrolls sideways (`filters.tsx:80` `overflow-x-auto`), and the
      provider select is cut off mid-word at 390 px.
    - **Seen in.** `audit-models-1440-light.png`, `audit-models-390-light.png`.
13. **Two places manage presets, aliases and combos.**
    - **What happens.** Models has a preset switcher, Add → Alias/Combo, and per-row "edit".
      That "edit" leaves for `/routing/aliases` and `/routing/combos`. The sidebar also lists
      "Routing", which holds Presets, Combos and Aliases (`app.tsx:110, 177-179`).
    - **Accepted already.** The 11-23 trade-off kept editing on `/routing/*` on purpose.
    - **The design problem.** The navigation doesn't tell users which place to use. The
      sidebar group is called "ROUTING" and contains an item also called "Routing".
14. **Status uses three vocabularies.**
    - **Providers rows.** Lowercase dot text ("ready", "backoff 2", "plan at limit"), plus a
      tinted Badge only for provider refusals ("suspended").
    - **Usage.** Capitalised pills ("Healthy", "Warning", "Exhausted").
    - **Models.** Badges ("unavailable", "deprecated", "disabled"), with no "available" state
      at all.
    - **Result.** One account can be "ready" on Providers and "Warning" on Usage at the same
      moment (claude-max at 80 %: `audit-providers-1440-light.png` vs
      `usage-overview-light.png`).
15. **The drawer says things twice.**
    - **Header vs body.** The header reads "Claude · cc · c-claude2" and the first body line
      reads "Claude cc oauth · access token · refresh token".
    - **Limits.** A 100 % red bar, then "5 hour: Limit reached" in text right below it.
    - **Two closes.** A circular ✕ (autofocused, so it opens with a heavy blue focus ring) and
      a footer "Close" next to "Save changes".
    - **Seen in.** `audit-providers-drawer-1440-light.png`.
    - **Files.** `pages/providers/connection-drawer.tsx`; the shared `Modal` close focus.

### P3 — shell and polish

16. **The star banner sits above every page title, and the sidebar repeats it.**
    - **What happens.** A 40 px "If RRouter saves you a router…" row (`app.tsx:570, 684-710`)
      pushes every H1 down. The same ask is a sidebar link (`app.tsx:727-739`).
    - **Fix.** Keep only the sidebar link, or show the banner once and auto-dismiss it after the
      first visit.
17. **Codex and OpenAI share one glyph and colour** (`components/provider-icon.tsx:27-28`).
    In the grouped list they look like the same provider twice
    (`audit-providers-1440-light.png`).
18. **The `rrouter` group header is lowercase while the others are title case.** The board
    gets section names from `displayOf`, which has no entry for `rrouter`
    (`models.tsx:219-226`). Combos that aren't in a custom group end up in a lowercase
    "rrouter" section at the bottom of the board (`audit-models-1440-light.png`).
19. **Combo rows read "3 candidates · metadata —".** This happens whenever the gateway sends
    no `aggregate` (`unified-rows.ts:249`). "metadata —" is an implementation word on screen.
20. **Card titles use different sizes.**
    - Providers' "Connections" is `Heading level 2` (`account-row.tsx:610`), larger than Usage's
      `text-body` card titles.
    - Usage hand-rolls the same `<h2 className="text-body font-semibold">` five times
      (`overview-panels.tsx:273/459/553`, `overview-charts.tsx:132/249`).
    - `CardHeader` exists but is only used on older pages.
21. **Two shape styles in one row.** Square 4 px selects and inputs (`--radius-input`) sit next
    to pill buttons and pill chips: "All accounts" next to "Add provider", "Ungrouped" next to
    "hide". Pick one style per control height.

## Shared design system plan: extract only what is already copied

Every item names its **current copies**. If something has only one consumer today, it stays
where it is.

| Primitive | Current copies (proof) | Action | Size |
|---|---|---|---|
| **`Meter`** (progress bar with an optional pace tick) | `QuotaMeter` `usage/quota.tsx:119-144`; `LimitMeter` `usage/limits.tsx:290-320` (the same component plus a tick); an inline bar without ARIA in `overview-panels.tsx:108-113` | One `components/atoms/Meter.tsx`, with `barFill`/`barText` moved next to it from `providers/types.ts:401-417`. Usage already imports them from the Providers folder. Delete the three copies. | S |
| **Duration / money display** | `until` `providers/types.ts:419` (buggy); `uptime` `lib/format.ts:92` (correct) | One `duration(secs)` in `lib/format.ts`, with `until` delegating to it. Add a display-money variant beside `usd` (finding 2). | S |
| **`CardHeader`** (icon, title, meta, right slot) | 5 hand-rolled headers in Usage (finding 20), plus `molecules/CardHeader.tsx` without icon or meta slots | Extend `CardHeader` with `icon?`, `meta?` and the `text-body` title size Usage uses. Convert the Usage five and the Providers "Connections" header. | S |
| **`AlertBanner` adoption** (not a new primitive) | Hand-rolled tinted banners: `providers.tsx:339-357` (notice, with its own dismiss), `providers.tsx:359-382` (team_forbidden), `models.tsx:1004-1022` (conflict), `models.tsx:1023-1047` (undo). `AlertBanner` already has `neg/warn/info/pos` and dismiss. | Replace the four with `AlertBanner`. No new code. | S |
| **`HealthBadge`** (account/row status pill) | `ROW_STATUS` map in `overview-panels.tsx:193-204` (Badge + tone); `DOT_TONE`/`STATE_TEXT` dot-text in `account-row.tsx:44-49, 213-218` and the provider header `:483-486` | Do **not** make a new atom until Providers adopts pills (Phase 2). That slice moves `ROW_STATUS` into a shared `components/molecules/HealthBadge.tsx`, so it has two real consumers. One vocabulary: Healthy (pos) · Warning (warn) · Failing/Exhausted (neg) · Disabled/Inactive (neutral). | S |
| **`KpiTile`** | 4 inline copies inside `OverviewKpiRow` (`overview-panels.tsx:85-188`) plus `KpiIcon` | Extract in the Providers slice, which is the second consumer, not before. Use the existing unused `--text-kpi` token (`index.css:457`) instead of `text-2xl`. | S |

**Tokens.** No new colours or radii are needed. Use what is there:

- the `--sem-*`/`--color-*` tone ramps (`pos`/`warn`/`neg`/`info` with `-tint`/`-line`/`-ink`);
- `--text-kpi` for KPI numbers;
- `--radius-card` for cards.

One rule to write into `index.css` comments rather than new tokens: 32 px controls are pills;
form inputs inside dialogs stay `--radius-input`. That covers finding 21 without touching every
select.

## Proposal — Providers page (overall size **M**)

Data already available, no new endpoint:

- `GET /api/connections`: priority, weight, team_id, last_error, backoff_level, refusal,
  free_mode, is_active.
- `/api/accounts/quotas` and `/api/accounts/usage`: limits.
- `/api/teams`: owner names.
- `useUsageSummaryByConnection(24)` (`lib/queries.ts:702`): per-account requests and errors in
  24 h. It is already shared and already used by Usage. The "in use" marker that the 11-23 pass
  excluded "until S1" can now come from this.

```
Providers                                          [All accounts ▾] [+ Add provider]
Upstream accounts this gateway routes through.
┌ Ready ──────────┐┌ Needs attention ─┐┌ Tightest limit ─────────┐┌ Traffic 24h ──────┐
│ 6 / 7 enabled   ││ 2                ││ 100% claude-pro-backup  ││ 5.2K req · 0.3% err│
│ 2 disabled      ││ backoff · error  ││ 5h · resets in 59m  ███ ││ ~~~~ sparkline     │
└─────────────────┘└──────────────────┘└─────────────────────────┘└────────────────────┘
┌ ⚙ Accounts  9 · 6 providers      [All | Needs attention | Disabled]   Provider codes ┐
│ ACCOUNT                 OWNER    PRIO  LIMIT (worst window)          24H     STATUS    ⋯│
│ Ⓐ Claude  cc                                              1/2 ready          Usage → │
│   claude-max            Shared   p0    5h ████████░░ 80% · 3h 0m     920   ● Warning ⋯│
│   claude-pro-backup     Shared   p1    5h ██████████ 100% · 59m      12    ● Backoff ⋯│
│     ↳ 429 rate_limit_error: This request would exceed your account's rate limit       │
│ ◎ OpenRouter  or                                          2/2 ready          Usage → │
│   openrouter-free  free Shared   p0    no cap set                    36    ● Healthy ⋯│
│     ↳ last error 4m ago: Provider returned error                                     │
│ ◎ OpenAI  oa                                    0/1 enabled ready · 1 off   Usage → │
│   openai-backup         Shared   p0    no cap set                     —    ○ Disabled ⋯│
│   openai-org            Shared   p0    no cap set                     —    ● Suspended⋯│
└────────────────────────────────────────────────────────────────────────────────────┘
```

**Remove**

- The two prose paragraphs (`providers.tsx:264-275`). The team sentence moves to the Owner
  column header tooltip plus the empty state; the reader sentence becomes a neutral `info`
  `AlertBanner` shown only to readers.
- The amber card-corner readout. The KPI strip replaces it.
- The duplicate drawer identity line, the "5 hour: Limit reached" text under a 100 % bar, and
  the footer "Close" (the ✕ stays).

**Merge and change**

- Status becomes a `HealthBadge` pill. Readiness counts **enabled** accounts only (shown as
  "N/M enabled ready · K off"), so an intentionally disabled account never turns a group red
  (fixes 3).
- `last_error` gets a one-line, neg-ink sub-row whenever it is set, not only under backoff
  (fixes 5). The truncated error text links to the drawer.
- Owner and 24 h traffic fill the empty column width (fixes 7). Priority stays.
- A segmented filter (All / Needs attention / Disabled) does the work of the KPI "needs
  attention" count.

**Sizes**

| Piece | Size |
|---|---|
| KPI strip | S, using `KpiTile` |
| Status, readiness and error sub-row | S |
| Owner and 24 h columns | S |
| Filter | S |
| Drawer de-duplication | S |
| **Total** | **M** |

**Phone (390).** KPI tiles become a 2×2 grid; Traffic drops to a single line. Rows keep today's
two-line card (`audit-providers-390-light.png`, which is already fine) and gain the error
sub-row.

## Proposal — Models page (overall size **L**)

Data already available, no new endpoint:

- `/api/models`: context, max_output, cost, capabilities, enabled, available, deprecated,
  source, reasoning.
- `/api/aliases`, `/api/combos` (with aggregate/members when sent), `/api/model-presets{,/id}`.
- `/api/usage/summary` (default `group_by` model): requests per model, for an optional
  "used 24 h" hint. Only add it if the slice has room. It isn't required.

```
Models                                                    [Copy base URL] [+ Add ▾]
Preset  [Default ▾]  ✓ saved                                         Manage presets
┌ Routed ─────┐┌ Free ───────┐┌ Needs attention ──────┐┌ Hidden / not in preset ┐
│ 23 of 29    ││ 4           ││ 1 disabled · 1 unavail ││ 2 · 3                  │
└─────────────┘└─────────────┘└────────────────────────┘└────────────────────────┘
┌ [🔍 search models, aliases, combos     ] [Provider ▾] [Kind ▾] [Status ▾] [Free] [Custom] ┐
│ Daily drivers  · custom group · 3                                          Groups ⚙ │
│ ⠿  coder        combo · fallback   3 candidates   $1.25–$3 in      ● Routed   ▷  ⋯ │
│ ⠿  fast         alias → claude-haiku-4-5   200K   $1 / $5          ● Routed   ▷  ⋯ │
│ Claude · 3                                                                         │
│ ⠿  claude-opus-4-5     Claude Opus 4.5   200K · 64K   $5 / $25   vision pdf +2  ● Routed ▷ ⋯ │
│ ⠿  claude-sonnet-4-5   Claude Sonnet 4.5 200K · 64K   $3 / $15   vision pdf +2  ● Routed ▷ ⋯ │
│ …                                                                                   │
│ 1–23 of 23                                                              Rows 50 ▾ │
└──────────────────────────────────────────────────────────────────────────────────┘
```

**Remove**

- The per-row provider chip **inside provider sections**. Keep it in flat (filtered) views,
  where the section is gone.
- The "provider · claude" section subtitle (`groups.tsx:35`).
- The always-visible per-row group `<select>`. "Move to group…" goes in the ⋯ menu, which
  `RowMenu` already supports and today hides while the select is visible. Drag between sections
  stays on desktop.
- The phone ↑/↓ buttons (`table-unified.tsx:799-808`). They become "Move up / Move down" in the
  card's ⋯ menu.
- "metadata —" (finding 19). Show "3 candidates" alone.

**Merge and change**

- **Status pill.** `/v1` and `enabled` merge into one `HealthBadge`-style pill per row: Routed /
  Hidden / Not in preset / Disabled / Unavailable (fixes 4 and 8). Toggling stays in ⋯
  ("Hide from /v1", "Disable model"). Hiding keeps its existing local undo banner, now an
  `AlertBanner`.
- **Actions.** One action cell for every row kind: ▷ test and ⋯, with Test / Hide / Edit / Move
  inside the menu (fixes 10).
- **Preset header.** Preset select plus save state on its own line under the title. "Save"
  appears only when the draft is dirty. Drop " · default" from the option when the name is
  already "Default". "Manage presets" becomes a quiet text button (fixes 11). The 11-23 click
  counts hold: copy ref 1, test 1, add combo into a group 2.
- **Filter bar.** One control style: pill dropdowns for Provider, Kind and Status; toggle chips
  for Free and Custom; search stays an input. "Groups" moves out of the filter bar into the
  first section header (fixes 12). At 390 the bar becomes search plus a "Filters (n)" button
  that opens a sheet, instead of scrolling sideways.
- **Section names.** `rrouter` renders as "RRouter names" (fixes 18).

**Sizes**

| Piece | Size |
|---|---|
| Row anatomy: status pill, action cell, chip and select removal | M |
| Header and preset line | S |
| Filter bar plus phone sheet | M |
| KPI strip | S |
| Phone card trim | S |
| **Total** | **L** |

## Recommended worker slices

Two workers in the same phase never share a file. Phase 2 starts after Phase 1 has landed. No
slice may commit; stage with `git add` only.

| Phase | Slice | Owns (exclusively) | Delivers | Size |
|---|---|---|---|---|
| 1 | **ds-primitives** | `components/atoms/Meter.tsx` (new), `components/atoms/index.ts`, `components/molecules/CardHeader.tsx`, `components/ui.tsx`, `lib/format.ts`, `pages/usage/quota.tsx`, `pages/usage/limits.tsx`, `pages/usage/table.tsx`, `pages/usage/overview-panels.tsx`, `pages/usage/overview-charts.tsx`, `pages/activity/quota-total.tsx`, plus, **in Phase 1 only**, the import/helper lines of `pages/providers/types.ts` (`barFill`, `barText`, `until`), `pages/providers/account-row.tsx` (the `QuotaMeter` import) and `pages/providers/providers.test.ts`; `tests/usage-board.test.ts` | `Meter` replaces `QuotaMeter`, `LimitMeter` and the inline KPI bar, and `barFill`/`barText` move next to it. Every importer switches in this slice, with no re-export shim. A floor-based `duration` in `lib/format.ts`, which `until` now calls, plus display money (P1 1–2), with a test. The extended `CardHeader`, adopted by the 5 Usage headers. `KpiIcon` and `ROW_STATUS` **stay** in Usage until Phase 2. | M |
| 1 | **shell** | `app.tsx`, `components/provider-icon.tsx` | Star banner shown once (finding 16); distinct Codex mark/colour (17); sidebar copy: rename the "ROUTING" group or its "Routing" item so the two don't match, and label it "Presets, aliases & combos" (13) | S |
| 2 | **providers-page** | `pages/providers.tsx`, `pages/providers/*` (account-row, types, connection-drawer, account-preview, add-provider-flow, providers.test.ts), `components/molecules/KpiTile.tsx` (new), `components/molecules/HealthBadge.tsx` (new) | The Providers proposal. Extracts `KpiTile` + `HealthBadge`, the first point where each has two consumers. It also converts Usage's tiles and `ROW_STATUS` to them, so in Phase 2 it **also owns** `pages/usage/overview-panels.tsx`. No other Phase 2 slice touches that file. | M |
| 2 | **models-rows** | `pages/models/table-unified.tsx`, `columns-unified.tsx`, `columns.tsx`, `rows.tsx`, `unified-rows.ts`, `groups.tsx` | Status pill, one action cell, chip/select removal, phone card trim, "metadata —", `rrouter` label. Must keep the `ModelActions`/`LayoutControls` prop shapes, so `models.tsx` needs no edit. It imports `HealthBadge` only after providers-page lands; until then it uses `Badge` tones with the same vocabulary. | M |
| 2 | **models-chrome** | `pages/models.tsx`, `pages/models/preset-header.tsx`, `pages/models/filters.tsx`, `pages/models/params.ts`, `pages/models/models.test.ts` | Preset line, dirty-only Save, filter bar unification and the phone filter sheet, KPI strip (after `KpiTile` exists), `AlertBanner` for the conflict and undo banners | M |

**Ordering note.** models-rows and models-chrome run in parallel, but both depend on
providers-page's `KpiTile`/`HealthBadge`. Either run providers-page first within Phase 2 and the
two Models slices after it, or have models-chrome add the KPI strip in a short follow-up. The
rows slice has no hard dependency because `Badge` tones are enough.

## Verification each slice must show

- `cd ui && npm run typecheck && npm test`.
- Re-run `$SHOTS/audit-capture.mjs` (or a filtered `node audit-capture.mjs providers`) and attach
  the same light/dark/1440/390 shots.
- Check for no horizontal page overflow at 390. The script's report includes `scrollWidth`.
- Re-check the 11-23 click-count tables in both design docs. They must not get worse.
