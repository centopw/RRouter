# Usage redesign: every provider's limits at a glance

2026-11-23 · supersedes `DESIGN.md` section 7's single-account headline layout. Extends
`design/providers-redesign.md` and `design/models-combo-redesign.md`, and does not contradict
them: account credentials and routing order stay on Providers, model pricing stays on Models,
and live request traces stay on Activity. This page owns one thing: **which providers and
accounts are close to running out, which ones are failing, and what the current range cost.**

## Audit of the current page (`ui/src/pages/usage.tsx`, `usage/*`)

What it is today (verified against `usage-now.png` and `ui/src/pages/usage.tsx`): a two-card
top band (`Headline` left, `LimitsCard` right) for **one** headline account only, followed by a
collapsed provider → account → model table.

What each current region answers — and what it hides:

| Region | Question it answers today | What it hides |
|---|---|---|
| `Headline` card (top left) | "Which single window is riskiest right now?" (plus daily chart when window >= 2d) | Every other provider and account. For a 5-hour window (`< 2d`), the chart is omitted, leaving a mostly blank card that repeats the card beside it. |
| `LimitsCard` (top right) | "What are all windows of that one headline account?" | Every other account is collapsed behind `▸ 1 other account, by risk` — and expanding that disclosure still shows only **one** worst window per account, hiding OpenCode Go's 5-hour and weekly bars. Operator caps, failing accounts, and no-limit accounts are absent. |
| `UsageTable` (below) | "Range requests, tokens, spend, and errors by provider, plus worst quota %." | OpenCode Go's 3 windows require expanding the provider row. Failing providers (`openrouter` 100% errors, unnamed row 100% errors) look like quiet red numbers. Range spend and failures are detached from the limits band above. |

Findings, by tell and tier:

- **Core product defect (single-provider bias).** RRouter routes across multiple providers and
  accounts simultaneously. Selecting `rankAccounts(accounts)[0]` as a "headline account" hid
  OpenCode Go's 54% monthly / 9% weekly / 0% 5-hour windows while spotlighting Claude's 27%
  5-hour window alone.
- **Floor — broken identity (unnamed provider row, blank icon).** Requests that fail before
  upstream dispatch record `provider = ""` (`crates/rrouter-gateway/src/proxy.rs:3902`).
  `providerLabel("")` returned `""` and `<ProviderIcon provider="" />` rendered an empty dark
  rounded square next to `2 models | 21 | 0 | $0.00 | 100.0%`. Non-subscription providers like
  `openrouter` also printed raw lowercase ids instead of their catalog display name.
- **Floor — dishonest "no limit".** `QuotaCell` printed `"no limit"` whenever no bars came back
  and queries succeeded. Pay-as-you-go providers (e.g. OpenRouter, API-key accounts) do not
  report plan windows — that means *"not reported by provider"*, never unlimited capacity.
- **Floor — tracked-caps table headers.** `usage/table.tsx` used `uppercase tracking-[0.06em]`
  on `<th>` headers.
- **T1 — two components answering one question.** `Headline` and `LimitsCard` both answered
  "what is Claude's 5-hour window?" with the exact same `%`, `resets in`, and `On pace to hit`
  sentence side by side.
- **T2 — complexity relocated onto the user.** Answering "are any of my providers failing or
  close to a limit?" required opening `1 other account, by risk` AND expanding each provider
  row in the table below AND mentally cross-referencing limits with error rates.
- **T3 — no task-scoped preview for account activity.** Account links jumped straight to
  `/providers?inspect=<id>` without showing which models on that account drove the current
  range's usage.

## Job

Someone opens `/usage` to answer four questions in one scan:

1. **Will any provider or account run out of quota soon, and which one hits first?**
2. **Are any providers or accounts failing right now?**
3. **What are all my providers' limits (every window, every account) right now?**
4. **How much have I spent and routed in the selected range, and on which models?**

## The four answers

| # | Question | Answer |
|---|---|---|
| 1 | **Why open it?** | To check quota headroom across *all* connected providers at once, spot failing accounts immediately, and see range spend alongside limits. |
| 2 | **How is it used?** | Open beside a coding agent, terminal, or `/providers`. Glanced at in seconds during a session or on-call (mobile width happens); deep-linked from `/providers` (`?provider=`). |
| 3 | **What do they want next?** | Inspect the burn-rate forecast chart for one specific window (lazy inline expansion, no navigation), see which models on an account caused the burn (hover/focus preview or drill-down table below), or click straight into `/providers?inspect=<id>` to fix/cap an account. |
| 4 | **What are we willing to leave out?** | The single-account `Headline` hero card, the collapsed "N other accounts" drawer, per-model quota cells, latency columns (Activity owns latency), summing percent-only plan windows across accounts, and eager N-card history fetches on mount. |

## Wireframe

```
Usage                                                              [ 1h | 24h | 7d | 30d ]
──────────────────────────────────────────────────────────────────────────────────────────────
  Summary strip (interactive filter pills + range spend)
  [ All 6 ]  [ At risk 2 ]  [ Failing 2 ]  [ Healthy 1 ]  [ No limit reported 1 ]   30d spend $13.2294
──────────────────────────────────────────────────────────────────────────────────────────────
  Provider limits & accounts (every account of every provider, sorted by risk)

  ┌──────────────────────────────────────────┐  ┌──────────────────────────────────────────┐
  │ [AI] Claude Max · claude-oauth  [At risk]│  │ [▲] Codex · codex-work       [Exhausted] │
  │ 5-hour window · resets in 4h 26m     27% │  │ 5-hour window · resets in 1h 12m    100% │
  │ ████░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░ │  │ ████████████████████████████████████████ │
  │ On pace to hit the limit at 03:36 PM     │  │ Limit reached                            │
  │ Weekly · resets in 4d 16h            18% │  │ Weekly · resets in 5d 02h            64% │
  │ ███░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░ │  │ █████████████████████████░░░░░░░░░░░░░░░ │
  │ On pace for 41% by reset                 │  │ On pace for 82% by reset                 │
  │ 30d: 0 req · 0 tok · $0.00 · 0.0% err    │  │ 30d: 142 req · 18M tok · $4.12 · 0.0%    │
  │      idle in 30d · checked 24s ago       │  │      last used 3m ago · checked 24s ago  │
  │ ▸ Forecast chart                         │  │ ▾ Forecast chart    [5-hour] [Weekly]    │
  └──────────────────────────────────────────┘  │   (lazy LimitChart SVG for Weekly)       │
                                                └──────────────────────────────────────────┘
  ┌──────────────────────────────────────────┐  ┌──────────────────────────────────────────┐
  │ [OR] OpenRouter · prod-key     [Failing] │  │ [!] Unrouted                   [Failing] │
  │ Not reported by OpenRouter.              │  │ Requests failed before dispatch.         │
  │ Set an operator cap on Providers.        │  │                                          │
  │ 30d: 6 req · 0 tok · $0.00 · 100.0% err  │  │ 30d: 21 req · 0 tok · $0.00 · 100.0% err │
  │      6 of 6 failed · last used 12m ago   │  │      21 of 21 failed · last used 8m ago  │
  └──────────────────────────────────────────┘  └──────────────────────────────────────────┘
  ┌──────────────────────────────────────────┐  ┌──────────────────────────────────────────┐
  │ [OG] OpenCode Go · opencode    [Healthy] │  │ [OA] OpenAI · team-key       [No limit reported]  │
  │ 5-hour window · resets in 4h 50m      0% │  │ Not reported by OpenAI.                  │
  │ ░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░ │  │ Set an operator cap on Providers.        │
  │ OK · 0% used                             │  │ 30d: 84 req · 4.2M tok · $0.84 · 0.0%    │
  │ Weekly · resets in 6d 17h             9% │  │      last used 1h ago                    │
  │ ██░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░ │  └──────────────────────────────────────────┘
  │ On pace for 48% by reset                 │
  │ Monthly · resets in 24d 18h          54% │
  │ █████████████████████░░░░░░░░░░░░░░░░░░░ │
  │ On pace for ~88% by reset                │
  │ 30d: 3.83K req · 316M tok · $13.23 · 0.1%│
  │      last used 1m ago · checked 24s ago  │
  │ ▸ Forecast chart                         │
  └──────────────────────────────────────────┘

  Breakdown by provider, account and model · Last 30d
  ┌────────────────────────────────────────────────────────────────────────────────────────┐
  │ Provider / account / model        Requests   Tokens       Spend   Errors  Quota (now)  │
  │ All providers                        3.85K     316M    $13.2294     0.8%               │
  │ ▸ [OG] OpenCode Go  3 models         3.83K     316M    $13.2294     0.1%  54% Monthly  │
  │ ▸ [!]  Unrouted  2 models [failing]     21        0       $0.00   100.0%  —            │
  │ ▸ [OR] OpenRouter  1 model [failing]     6        0       $0.00   100.0%  —            │
  │ ▸ [AI] Claude  0 models                  0        0       $0.00     0.0%  27% 5-hour   │
  └────────────────────────────────────────────────────────────────────────────────────────┘
```

Responsive & role rules:

- **Mobile (`< lg`):** Summary strip wraps cleanly; the limits board drops to a single column
  (`grid-cols-1`) with full-width cards; the breakdown table scrolls horizontally inside its
  bordered card without breaking page width.
- **Non-admin readers:** Limit APIs (`/api/accounts/usage`, `/api/accounts/quotas`, history)
  are admin-only. The summary strip omits admin-only quota buckets and shows active/failing
  accounts + range spend; each card renders range activity and honestly states *"Plan limits
  and operator caps are visible to admins only."* Never prints *"no limit"*.
- **URL state:** Keeps `?range=<1h|24h|7d|30d>` and `?provider=<id>` (deep-linked from
  `/providers`), filtering both the Limits Board and the breakdown table consistently.

## Sorting and classification rules

Every account card receives a deterministic status and rank (`ui/src/pages/usage/board-model.ts`).
Limit-derived claims (exhausted, at risk, healthy, unavailable, no limit reported) are only
made once **both** limit sources have answered; while either is still loading an admin sees
**"Checking limits…"** rather than a guess, and those cards are excluded from the at-risk,
healthy and no-limit counts and chips. Non-admins always see **"Admin only"**.

1. **Exhausted** (any plan window >= 100% or `quota.exhausted === true`) — ranked first (`hitAt = now`).
2. **At risk** (any plan window `on_pace_to_hit` before reset, or any bar >= 90%) — ranked by earliest projected `limitHitAt` ascending, then highest used `%` descending.
3. **Failing** (range requests > 0 with error rate >= 50%, or provider refusal on connection) — ranked right after at-risk accounts so outages (e.g. 100% errors on OpenRouter or Unrouted traffic) are never buried beneath idle accounts.
4. **Degraded / Healthy with limits** — active accounts reporting at least one window or operator cap, ranked by highest used `%` descending.
5. **Checking limits… / Limits unavailable / No limit reported** — active accounts without a limit answer, ranked by range spend then requests descending.
6. **Inactive** — disabled accounts at the bottom, badged `inactive`, so disabled accounts never masquerade as live quota risk.

Window pace text per window:
- `exhausted` → `Limit reached` (crimson)
- `on_pace_to_hit` → `On pace to hit the limit at 03:36 PM` / `~Nov 27` (amber)
- `ok` → `On pace for 41% by reset` (or `OK · 0% used` when 0% used)
- `unknown` → `Too early to forecast` when elapsed < 5% of a known window; `No reset window reported` when the provider omitted `window_seconds` or `resets_at`.

## Deliberate trade-offs

- **All accounts visible on first paint in a 2-column grid, no "headline hero" card.** On a
  deployment with 1–2 accounts, the page no longer leads with a large chart already open. We
  accept this because RRouter is a multi-provider gateway: showing 1 account at a time hid
  every other provider's limits, and opening any card's forecast chart is one click away.
- **Lazy history fetch per opened card only.** Expanding a card fetches
  `GET /api/accounts/usage/{id}/history?window=` on demand for the selected window, defaulting
  to the riskiest chartable window (`>= 2 days`). Users comparing 3 charts click 3 cards rather
  than seeing all charts immediately. We accept this to prevent N simultaneous history requests
  on mount and keep the 15s polling budget flat.
- **Non-exclusive chart disclosures.** Opening one card's forecast chart does not collapse
  another card's chart, so operators can compare burn rates of two providers side by side.
- **`last used` derived from recent 200 usage events in range (`useUsage(200, range.hours)`).**
  `/api/usage/summary?group_by=connection` does not return `max(ts)` per connection. Rather
  than invent a timestamp or add a backend field outside this slice's scope, we read the
  existing `/api/usage?limit=200&hours=` endpoint once per page. When an account has range
  requests older than the newest 200 events, the card honestly says `used earlier in <range>`
  instead of faking a time or saying idle.
- **Empty `provider === ""` is rendered as `Unrouted` with a warning icon.** Those rows come
  from requests rejected before dispatch (`proxy.rs:3902`). Naming them `Unrouted` with a
  clear explanation is honest without needing a backend schema migration.
- **Linear pace forecast stays unchanged.** Kept from `DESIGN.md` section 7: straight-line
  extrapolation with `~` for hard-coded 30-day monthly windows, no interpolation across
  missing days in `LimitChart`.

## Exclusions

- No chart library (reuses inline SVG `limit-chart.tsx`).
- No N-per-card polling (single page-level queries; history fetched lazily only while expanded).
- No summing of percent-only plan windows across accounts.
- No per-model quota cells (quotas are per connection, never per model).
- No latency column on `/usage` (Activity owns latency).
- No edits to shared components (`ui/src/components/*`), `main.tsx`, or `providers/*`.

## Preview contract

Hovering (~350 ms, mouse only) or keyboard-focusing (`:focus-visible`) any account name link
on the Limits Board opens a non-modal, non-focus-stealing preview (`Popover`, zero interactive
controls inside) scoped to **"what is burning this account and where does clicking go?"**:

1. Account title, provider + plan, email (when set), and standing state (`active` / `inactive`,
   plus `last_error` or `refusal` if present).
2. Top models served by this account in the active range (up to 3, with requests, tokens,
   spend, and error rate), or *"No requests in the last <range>."*
3. Limit freshness (`Limits checked 24s ago`) when subscription windows exist.
4. Footer hint: `Click to open connection details on Providers`.

Escape or blur closes it; touch taps skip the preview and navigate directly to
`/providers?inspect=<id>`.

## Click counts (top 5 questions)

| Question | Clicks |
|---|---|
| Which provider/account will run out of quota first? | 0 (top-left card on the risk-sorted board) |
| What are all windows of every provider right now? | 0 (every window bar of every account visible on first paint) |
| Which providers or accounts are failing? | 0 (`Failing` count in summary strip + `Failing` cards near top) |
| What is the projected burn-rate curve for an account? | 1 (click `Forecast chart` on that card) |
| Which models on an account drove spend or errors? | 0 (hover/focus account preview) or 1–2 (expand row in breakdown table) |

## Deleted

Removed from the `/usage` surface (code replaced or removed, not hidden):

- Single-account `<Headline>` hero card on `usage.tsx` (replaced by `<SummaryStrip>` +
  `<LimitsBoard>`; shared pure formatters `paceSentence`, `hitWhen`, `renewalText`,
  `planTitle`, `accountName`, `AccountLink` remain exported in `usage/headline.tsx` for
  `providers/account-preview.tsx` stability).
- Single-account `<LimitsCard>` and its collapsed `▸ N other accounts, by risk` disclosure
  (replaced by `<LimitsBoard>` showing every account and every window at once).
- Blank-icon / unnamed provider rendering for empty `provider === ""` rows.
- False `"no limit"` text for providers that simply do not report plan windows (replaced by
  `"Not reported by <provider>"` / `"Not reported"`).
- Tracked-caps (`uppercase tracking-[0.06em]`) styling on `UsageTable` column headers.
- Duplicate hand-rolled progress bar in `ui/src/pages/activity/quota-total.tsx` (now reuses
  shared `QuotaMeter` and `resetsIn` from `usage/quota.tsx`).

## Ownership & API stability notes

- `prov-ui` imports `QuotaMeter`, `windowLabel`, `resetsIn`, `worstQuotaBar`, `quotaBars`,
  `QuotaCell` from `ui/src/pages/usage/quota.tsx`, `pace`, `accountRisk`, `isAlarming`,
  `rankAccounts` from `ui/src/pages/usage/pace.ts`, and `paceSentence` from
  `ui/src/pages/usage/headline.tsx`. All of these export names and signatures remain strictly
  unchanged.
- `ui/src/pages/activity/index.tsx` imports `QuotaTotalLine` and `hasLimit` from
  `ui/src/pages/activity/quota-total.tsx`. Exports and behaviour remain unchanged.
