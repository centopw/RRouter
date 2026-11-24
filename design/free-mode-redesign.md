# Free mode redesign: find free models, use them, fall back to them

2026-11-23 · phase 1 (opinion ledger + wireframe). The UI is coded only against the API
contract fm-core posts on the swarm board; the [contract needs](#what-the-ui-needs-from-the-api)
below are what this page asks of it, not a contract.

Extends `design/providers-redesign.md` and `design/models-combo-redesign.md`, and does not
contradict them: accounts stay on Providers, model pricing and the Free marker stay on Models.
This page owns one thing: **is the free pool on, what is in it, and when does it get used.**

## Audit of the current page (`ui/src/pages/free-mode.tsx`, 1016 lines)

What it is: an editor for a JSON document of free-tier accounts and egress proxies
(`docs/FREE_MODE.md`). Seven cards: empty-state banner, Document (textarea, drag-and-drop,
Validate, Choose .json, Revert, Import), Routing, Accounts, Discovered models, Call it like
this, Tear down.

Findings, by tell:

- **T2, complexity moved onto the user.** Nothing works until the operator writes a JSON
  document of providers, accounts, keys and proxies by hand. The system can already find
  free models without any of that: the OpenCode public catalog (`/zen/v1/models`, no key)
  lists 11 models priced 0/0 (`evidence.md`). The page asks for what the system can derive.
- **T2, controls that are not controls.** The Routing card's *Enabled* toggle, the RPM and
  max-attempts inputs, *Prefer*, and picking a model in Discovered models do not change
  anything: each one rewrites the draft JSON (`applyRouting` → `edit(documentText(...))`,
  page L168-180) and only takes effect after *Import*. A switch that needs a second, distant
  button to mean anything is a Floor failure (honesty).
- **T1, two components answering one question.** "Is free mode on?" is answered by the
  summary strip (`free routing enabled`), the Routing toggle (draft state, may disagree with
  the strip), and the empty-state banner. "Which models?" is answered by the Discovered
  models table and the picked-chips row above it.
- **T1, components without their reasoning.** *Source* badge (`document`/`provider-list`/
  `registry`) answers a question only the implementer asks. *Call it like this* lists four
  ref grammars (`free`, `free:P`, `free/M`, `free:P/M`) for a user who wants one.
  *Price* column on a list that is by definition free.
- **T3.** No model links to its Models row; no account links to its Providers row; nothing
  is deep-linkable; finding out whether a model actually answers means leaving for the
  Playground.
- **The product promise is missing.** Nothing on the page says what happens when paid
  tokens run out, because nothing does: there is no fallback.

## Job

Someone opens Free mode for one of two reasons:

1. "My paid tokens might run out; I want requests to keep working on something free."
2. "I just want a free model to point a client at."

Both are answered by the same three facts, which the page leads with.

## The four answers

1. **Why open it?** To turn free models on and know they work. Not to author a document.
2. **What is open beside it?** A coding client or agent pointed at RRouter, usually mid-task,
   often because it just failed with a quota error. The user wants to fix that in seconds and
   get back, so the page must be actionable on first paint, without reading.
3. **What do they want to know next?** In order: *Is it on? Which free models answer right
   now? What will happen to my requests when the paid route runs out? What model name do I
   type?* The status line answers all four in one sentence group; nothing else competes.
4. **What do we leave out?** The document editor as the front door, the ref grammar, the
   source/price columns, RPM/attempt/prefer knobs, and teardown as a card. They move into
   *Advanced* or are deleted (see [Deleted](#deleted)).

## Quick setup, counted in clicks

Target: **enabled and working in 2 clicks, with a visible test result.**

| Step | Click | What the user sees |
|---|---|---|
| Open `/free-mode` | 0 | Status: "Free models are off. 10 free models found on OpenCode." The list is already populated by discovery (catalog fetched by the server, no key). |
| Turn on free models | 1 | Switch flips; the server enables the pool and probes every enabled model; each row shows a live result (`answered in 820 ms` / `rate limited by OpenCode`) as it arrives. |
| Turn on fallback | 2 | "When a paid route runs out of quota or returns 429/402, the request is retried on a free model; the log marks it *fallback: quota*." |

The user is done after click 1 if they only want a free model; click 2 is the
"tokens ran out" promise. Copying the model name (`free`) is a third, optional click.

If the contract says an OpenCode key is needed to lift the anonymous rate limit
(`evidence.md`: anonymous calls answer 429 FreeUsageLimitError, a real key is unverified),
the test result *says so on the row* ("OpenCode needs an account key for free use: add one")
with a link to `/providers?add=opencode`. We do not put a key field on this page: keys live
on Providers.

## Wireframe

```
Free mode
──────────────────────────────────────────────────────────────────────────────
 [●  ON ]  Free models are on. 7 of 10 answer right now.          Refresh
           Requests to `free` use them. When paid quota runs out:  [● ON ]
           retry on a free model.  Last check 2 min ago.
           Model name  free  [copy]
──────────────────────────────────────────────────────────────────────────────
 When your paid quota runs out                                   (preview)
   A request for claude-sonnet-5-5 that fails with 429/402 or an exhausted
   quota is retried once on the free pool (big-pickle first, by last
   latency). The answer and the request log say "fallback: quota".
   Requests that name a model explicitly with `free/` never fall back.
──────────────────────────────────────────────────────────────────────────────
 Free models                                     [All 10] [Working 7] [Off 1]
   on   model                       provider   status                  test
  [✓]  big-pickle                  OpenCode   ● answered · 820 ms 2m   Try
  [✓]  deepseek-v4.1-flash-free    OpenCode   ● answered · 1.4 s 2m    Try
  [✓]  nemotron-3.5-super-vl-free  OpenCode   ▲ rate limited · 2m      Try
         "Free usage limit reached." Add an OpenCode key → Providers
  [ ]  tidepool-alpha-free         OpenCode   ○ off                     Try
  ...
  Not usable here (1)  jev-1.13-free · speaks a dialect RRouter does not
──────────────────────────────────────────────────────────────────────────────
 ▸ Advanced: import free-tier accounts and proxies (JSON)
     document editor · per-account proxies · imported accounts · clear import
```

Row rules:

- Model id links to `/models?q=<id>`; provider links to `/providers?inspect=<provider>`.
- *Try* sends one short probe and puts the result inline on that row (status text, latency,
  the provider's own error message), never in a toast. Keyboard reachable, no hover-only.
- Status is relative time + outcome, from the server's last probe; never a dot alone.
- *Not usable here* is a collapsed footnote, not rows with a disabled switch: listing a model
  the user can never turn on is noise, hiding it is dishonest.

Mobile: the header block stacks; the table drops to a list of cards (model, status line,
switch, Try).

## Deliberate trade-offs

- **We discover and probe without being asked.** Some operators dislike a gateway that calls
  outside on page load. We accept it because the catalog is public and keyless, and an empty
  page with a "Discover" button is the exact relocation of work this redesign removes. The
  probe runs only when the user turns the pool on or clicks Try.
- **New free models start on.** Some will want an allow-list. We accept it because the pool
  is the point, a row switch turns any one off, and "nothing works until you pick" is the
  current page's failure.
- **Fallback is opt-in, one switch.** Some want it per key or per team. We accept one global
  switch because the plain-language preview has to stay one paragraph; per-key scope is a
  later decision, not a hidden knob.
- **The document import survives, but behind Advanced.** Power users with their own free-tier
  accounts and proxies keep it intact; nobody else sees JSON.
- **One model name.** We show `free` only. `free/<id>` appears as the copy target of a row,
  not as a grammar lesson.

## Exclusions

- No key or account entry on this page (Providers owns accounts).
- No pricing column (every row is free; the Models page shows prices).
- No RPM / max-attempts / prefer knobs on the main surface (Advanced document only, if the
  contract keeps them).
- No "source" column, no discovery timestamps per row beyond the last probe.
- No tour or explainer card; the status sentence and the fallback preview are the
  explanation.

## Preview contract

Every switch shows what it will do before and after it is flipped, in the page's own words:

- Pool switch, off: "Turn on to route `free` to N free models." On: "N of M answer right now."
- Fallback switch: the paragraph in *When your paid quota runs out* is rendered from the real
  settings (which errors trigger it, the first model it would pick, how the log marks it). It
  changes when the pool changes. When the pool is off, it says fallback has nothing to use.
- Row switch: off rows say "off" in the status column, not a greyed row.
- Try: result replaces the status cell; a failure shows the provider's message verbatim
  (already scrubbed by the server).

## Deleted

From the main surface (code removed, not hidden):

- Empty-state banner "Nothing imported yet" (discovery makes it unreachable).
- Summary strip (replaced by the status headline).
- Routing card: Enabled toggle-that-edits-JSON, Requests per account, Max attempts, Prefer.
- Discovered models table columns: Source, Price, discovered-at; the picked-chips row.
- *Call it like this* card (four ref grammars) → one copyable `free`.
- *Tear down* card and its confirm dialog → "Clear import" inside Advanced, only when a
  document exists.

Moved under *Advanced* unchanged in behaviour: document editor with validate/import, file
drop, accounts table with masked proxies. `free-mode-document.ts` keeps validation; its
routing-knob helpers (`withRouting`, `rpmProblem`, `attemptsProblem`, `FREE_PREFERS`,
`freeRefHints`, `sourceLabel`, `priceLabel`) are deleted if the contract no longer needs
them, with their tests.

## What the UI needs from the API

To be matched against fm-core's posted contract before phase 2; the UI does not invent
fields.

1. One read: pool on/off, fallback on/off, last check time, and per model: provider, model
   id, endpoint, enabled, usable (with reason when not), last probe `{ok, status, latency_ms,
   message, at}`.
2. Writes: set pool on/off; set fallback on/off; enable/disable one model.
3. Probe one model now → the same probe shape. Turning the pool on probes enabled models.
4. Discovery that needs no document (public catalog), run by the server on read or on a
   schedule; the page never has a "Discover" button on the main surface.
5. Existing import/teardown/accounts endpoints unchanged for Advanced.

## Ownership notes

- `ui/src/lib/queries.ts` is not this worker's file. New hooks live in
  `ui/src/pages/free-mode/hooks.ts` (sharing `qk.freeMode` with `lib/queries.ts`).
- Route `/free-mode` and the sidebar entry stay as they are.

## Phase 2 implementation (against frozen contract `#20`)

Implemented against `fm-core`'s frozen contract (`GET/PATCH /api/free-mode`, `PATCH /api/free-mode/models`, `POST /api/free-mode/probe`, and preserved Advanced endpoints):

- `ui/src/pages/free-mode/model.ts`: pure contract types, defensive parsers, cache reducers, single-source `modelStatus()`, `statusHeadline()`, `fallbackPreview()`, and sort/filter partitioning (unit-tested directly by Node's runner).
- `ui/src/lib/free-mode.ts`: thin API layer keeping existing `lib/queries.ts` exports (`getFreeMode`, `importFreeMode`, `discoverFreeMode`, `teardownFreeMode`) and adding `refreshFreeMode`, `patchFreeMode`, `patchFreeModeModel`, `probeFreeModel`, `probeAllFreeModels`.
- `ui/src/lib/free-mode-document.ts`: kept validation + example for Advanced; deleted unused routing-knob / ref-grammar / price / source helpers (`withRouting`, `routingOf`, `FREE_PREFERS`, `rpmProblem`, `attemptsProblem`, `priceLabel`, `sourceLabel`, `freeRefHints`).
- `ui/src/pages/free-mode/{hooks,status-hero,fallback-card,models-section,advanced-section}.tsx` and `ui/src/pages/free-mode.tsx`: 2-click surface (pool switch with inline probe results, fallback switch with live plain-language preview), provider + status filter chips, visible endpoint + auth mode (`anonymous` / `account connected` / `key required`), inline `Add a <Provider> key →` links to `/providers?add=<provider>`, model links to `/models?q=<id>&status=all`, collapsed `Not usable here` footnote, mobile card layout, and JSON document import / Clear import demoted under Advanced.
