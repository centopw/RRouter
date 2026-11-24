# FreeLLMAPI parity: what upstream does, what we have, what to take

2026-11-24 · research only, no code changed. Owner: `freellmapi-research`.

## Upstream reviewed

| | |
|---|---|
| Repo | `https://github.com/tashfeenahmed/freellmapi` |
| Commit | `6daa81b8def6a7e1d99fd67a611779913c287076` |
| Commit date | 2026-11-24T09:24:44Z (`feat(agents): add Zero to Agents page & CLI …`, #2081) |
| Licence | MIT, `Copyright (c) 2026 Tashfeen Ahmed` (root `LICENSE`; `cli/LICENSE` is also MIT) |
| Size | 1,458 tracked files; `server/` is about 73k lines of TypeScript across 764 files |
| How it was reviewed | Cloned into its own empty directory (`/private/tmp/freellmapi-upstream/repo`) and only read. No install, script or binary from it was run. |

Upstream is a self-hosted Express + SQLite gateway. It stacks the free tiers of about 45
built-in platforms behind one bearer token, plus more than 40 "relay" adapters. A Thompson-sampling
bandit picks one `(platform, model, key)` per attempt and fails over up to 20 times inside a
45 s budget. It ships a desktop app, a CLI, a playground, MCP, media, search and compression.
Most of that is outside the scope of free-tier aggregation. This doc covers only the free-tier
aggregation core.

### The upstream source of truth has moved off the repo

This is the most important finding for keeping our design trackable against upstream.

- Model rows and their limits (`rpm/rpd/tpm/tpd`, context, ranks) are seeded by
  `server/src/db/migrations/20260101_000000_legacy_baseline.ts`. That file is a 2,364-line
  chain of `migrateModelsVnn()` INSERTs and UPDATEs, and **its last change was 2026-10-24,
  with zero commits in the last 30 days**. Every built-in platform's rows live only in that
  file.
- New models and limit corrections now ship through a **hosted, Ed25519-signed catalog** at
  `api.freellmapi.co/v1/latest` (`server/src/services/catalog-sync.ts`,
  `MIN_CATALOG_VERSION = '2026.06.07'`). Premium licence holders get a "live" tier every
  2-3 days. Free installs get a "monthly" snapshot that trails by 30 days. The provider doc
  marks several rows as "hosted-catalog-only".
- What still changes in the repo, and is MIT, falls in four groups:
  - **provider identity and transport**: `server/src/providers/index.ts`, 21 commits in 30 days.
  - **quota header maps and pool keys**: `server/src/services/provider-quota.ts`, 14 commits in 30 days.
  - **provider-wide caps and the cooldown ladder**: `server/src/services/ratelimit.ts`.
  - **error classification**: `server/src/lib/error-classify.ts`, plus the prose in
    `docs/en/providers/01-supported-platforms.md` and `02-quotas-and-cooldowns.md`.

So "pull upstream updates" can mean diffing the **repo**: providers, transport, caps,
headers and error patterns. It cannot mean pulling **live model limits**, because those are a
paid feed and not MIT (see [Do not copy](#deliberately-not-copied)).

## Our free mode today (read first)

Sources: `docs/FREE_MODE.md`, `design/free-mode-redesign.md`,
`crates/rrouter-router/src/free_mode.rs`, `crates/rrouter-gateway/src/free_mode.rs`,
`crates/rrouter-gateway/src/routes/free_mode.rs`, `ui/src/pages/free-mode*`,
`crates/rrouter-router/src/breaker.rs`.

- **Discovery** works from two public, keyless catalogs: OpenCode Zen, where the price is
  0/0 and the model has a `chat-completions` API, and OpenRouter, where the id ends in
  `:free` or the price is 0/0. Both refresh every 30 minutes and on demand. Free-mode accounts
  also run per-account `/v1/models` discovery, which `classify_free` judges by a
  `:free`/`-free` id or a registry price of exactly 0. An unknown price is never treated as
  free.
- **Pool refs**: `free`, `free:<provider>`, `free/<model>` and `free:<provider>/<model>`.
  A `free` ref never widens to a paid model.
- **Anonymous send** is OpenCode only (`Bearer public`, nil-UUID connection marker).
- **Rotation** (`FreeRotation`) keeps a per-account 60 s request window from `free_rpm`. It
  is a budget, never a gate: `prefer: spread|cheapest` with least-recent use.
- **Cooldowns** come from `breaker.rs`. 401/403/5xx open the connection breaker (30 s for
  5xx, 300 s for auth) and reopen with a half-open single probe. A 429 starts a
  `(connection, model)` cooldown that grows exponentially from 30 s up to a 1 h cap. A
  provider-wide free-usage cooldown covers OpenCode `FreeUsageLimitError`; it honours
  `Retry-After` and is capped at 1 h. **All of this state is in memory.**
- **In flight (@free-fix, uncommitted diff in `free_mode.rs`/`proxy.rs`, not edited here)**:
  `is_free_usage_limit_error` now also treats OpenRouter's own `:free` cap (a
  `metadata.error_type` of `rate_limit_exceeded`/`free_models_per_*` with no `provider_name`
  or `raw`) as provider-wide. An upstream-provider 429 behind OpenRouter stays per-model.
  `extract_provider_error_message` unwraps `error.metadata.raw`.
  `is_public_catalog_free_model` extends the provider cooldown to keyed OpenRouter `:free`
  models.
- **Fallback**: opt-in `fallback_on_quota` reroutes a paid request to `free` on a
  quota/429/402 failure. If the free pool also fails, the original error is returned.
- **Probes** run only when the pool is enabled or Try is clicked (concurrency 5, 6 s each).
  There is no background polling.
- **Advanced**: a JSON document of named accounts, per-account proxies, `rpm`, priority and
  weight, with seed models.

## Parity table

Status: **have**, **partial** or **missing**. "n/a" means a deliberate non-goal (see below).

| # | Upstream capability | Upstream file(s) | Ours | Status |
|---|---|---|---|---|
| 1 | Provider registry: about 45 built-in platforms, base URL, adapter, timeout, keyless flag | `server/src/providers/index.ts`, `providers/*.ts`, `docs/en/providers/01-supported-platforms.md` | `rrouter-auth/src/registry.rs` compiles claude, codex, github, openai, opencode, anthropic, opencode-go, deepseek, gemini, groq, openrouter, xai, mistral, together, cerebras, ollama. Custom providers come from the document `custom` block. | partial: the transports exist, but free mode only *discovers* opencode and openrouter |
| 2 | Free-model catalog with per-model limits (rpm/rpd/tpm/tpd, context, size tier, rank) | `db/migrations/20260101_000000_legacy_baseline.ts` (frozen 2026-10-24) plus the hosted catalog | `free_models` rows from discovery or document seeds. They carry no limits. | missing (limits) |
| 3 | Free classification for providers without a `:free` marker (Groq, Cerebras, Gemini, Mistral free tiers: every model on a free key is free) | Catalog rows; "free" is implicit in membership | `classify_free` accepts only an id marker or a registry price of 0. Groq, Cerebras and Gemini free keys therefore classify as **not free**. | missing |
| 4 | Keyless providers: kilo (`:free`, 200 req/h/IP), ovh (2 req/min/IP/model), llm7, aihorde | `providers/index.ts` (`keyless: true`), `providers/aihorde.ts` | `supports_anonymous_free` = opencode only | partial |
| 5 | Per-(platform, model, key) RPM/RPD/TPM/TPD pre-check gates, UTC-midnight day windows | `services/ratelimit.ts` (`canMakeRequest`, `canUseTokens`) | `FreeRotation` per-account RPM only, soft, 60 s | partial (RPM soft) / missing (RPD/TPM/TPD) |
| 6 | In-flight leases so concurrent requests see each other's usage (check-then-act) | `ratelimit.ts` `acquireLease/releaseLease` | `FreeRotation::note` at send time counts in-flight requests for RPM | have (RPM scope only) |
| 7 | Provider-wide pools: OpenRouter 1000/day (50 below 10 credits), NVIDIA 40 RPM account-wide, Requesty 180/day, NavyAI 150k tokens/day | `ratelimit.ts` `DEFAULT_PROVIDER_DAILY_REQUEST_CAPS`/`…MINUTE…`; `provider-quota.ts` `inferPoolForPlatform` | Reactive only: the provider-wide cooldown fires on OpenCode `FreeUsageLimitError` and (after @free-fix) on the OpenRouter platform 429. No proactive count. | partial |
| 8 | Cooldown provenance (heuristic/authoritative/credit/tier) and the ladder 2 min → 10 min → 1 h → 24 h, with a transient 90 s for RPM/TPM | `ratelimit.ts` `getCooldownDecisionForLimit`; docs `02-quota-and-cooldown-engine.md` | `breaker.rs` 429 backoff from 30 s, doubling, capped at **1 h** | partial: no daily-exhaustion bench to UTC midnight, no provenance |
| 9 | Retry delay stated in the error body: Gemini `RetryInfo.retryDelay`, `retry_after`, prose like "try again in 30s", clamped to 24 h | `providers/base.ts` `providerHttpError`, `lib/retry-hint.ts` | `parse_retry_after_secs` reads the header only (and ignores the HTTP-date form) | partial |
| 10 | Learn real limits from error bodies (Groq 413 "Limit 30000 … TPM"). It only tightens, never raises. | `ratelimit.ts` `parseProviderLimit`, `learnLimitFromError` | none | missing |
| 11 | Error classification: daily-exhausted vs transient 429, 402 credit, 403 tier, account suspended, model retired (404/410 ×2 in 1 h), context too large, provider-level 5xx skips the platform | `lib/error-classify.ts`, `services/model-retirement.ts` | `breaker.rs`: 429 is model-scoped; 401/403/5xx/transport are connection-scoped. Fallback triggers on quota/429/402. | partial |
| 12 | Persisted cooldowns and usage ledger that survive a restart | `rate_limit_cooldowns`, `rate_limit_usage` tables | Breaker and rotation are in memory. The `usage` table already holds per-request rows (provider, model, connection, tokens). | partial |
| 13 | Probe-based early recovery for heuristic cooldowns (ripe after ≥50% served and ≥60 s left; backoff 2 → 15 min; at most 3 per pass) | `services/cooldown-probe.ts` | Connection breaker half-open single probe. Model 429 cooldowns just expire. | partial |
| 14 | Provider-reported quota from response headers (`x-ratelimit-*` maps per provider) and key-info polling (OpenRouter `GET /api/v1/key`) | `services/provider-quota.ts` (header map at about L216-245), `services/health.ts` `QUOTA_POLL_INTERVAL_MS` = 15 min | `provider_usage.rs` covers subscriptions only (Claude/Codex/OpenCode Go). Free providers get nothing. | missing |
| 15 | Health checks that never burn metered quota (5 min ±20% jitter; free endpoints only; `supportsKeyPolling=false` where validation costs quota) | `services/health.ts` | Probe only on enable or Try, which is deliberate (see the redesign). The connection health route exists. | have (by design, lighter) |
| 16 | Multi-key per platform, per-key ordering (bandit or `least-remaining`) | `services/router.ts` `orderKeysByScore`, `orderKeysByRemainingQuota` | Multiple accounts per provider, `priority`/`weight`, spread by RPM pressure plus least-recent use | have (simpler) |
| 17 | Model selection across the pool: weighted reliability, speed and intelligence, guardrail multipliers for headroom and penalty | `services/scoring.ts`, `services/router.ts` | `pick_next_fallback_model` prefers working models first by last probe; the planner does rotation | partial (deliberately simpler) |
| 18 | Fallback loop: max 20 attempts, 45 s wall-clock budget, hedge-abort, `X-Fallback-Trail` | `lib/fallback-loop.ts` | `dispatch.rs` with `max_attempts` (default 8), backoff ceiling, `all_candidates_cooling_down` with `Retry-After`, `x-rrouter-fallback*` headers | have |
| 19 | Paid → free fallback on quota | (upstream is free-only) | `fallback_on_quota` | have (we go further) |
| 20 | Catalog sync: signed, versioned, transactional; never re-enables a model the user disabled; tombstones; never overwrites user rows | `services/catalog-sync.ts` | Public catalogs are refreshed every 30 min. Document seeds are never downgraded. A disabled model stays disabled. | partial (no versioning or provenance across a vendored dataset) |
| 21 | Free-tier budget dashboard, deduped per pool, with observed used/remaining/reset | `routes/free-tier.ts`, `client/src/components/keys/quota-*` | Usage redesign is concurrent (this swarm). Free mode shows per-model probe state only. | partial |
| 22 | Bulk key import from `.env` (`PREFIX_MAP` env var → platform) | `lib/key-parser.ts` | Document import (JSON) | partial |
| 23 | ToS verdict per provider (Gemini caution, NVIDIA evaluation-only, Cohere avoid, …) | `docs/en/architecture/00-high-level-index.md` §Terms of Service review | none | missing |
| 24 | Degraded mode (healthy-provider ratio, hysteresis) | `services/degradation.ts` | none | n/a for now |
| 25 | Thompson bandit, community prior, decision-model routing, peak hours, sticky sessions | `services/scoring.ts`, `services/decision.ts` | none | n/a (see below) |
| 26 | Admin API: `/api/keys`, `/api/models`, `/api/fallback`, `/api/free-tier`, `/api/health`, `/api/analytics`; OpenAI, Anthropic, Responses and Gemini surfaces | `app.ts`, `routes/*` | `/api/free-mode*`, `/api/providers`, `/api/models`, `/api/usage`, `/api/health`; OpenAI, Anthropic and Responses surfaces | have (equivalent shape) |

## Catalog sync: vendor the repo data, not the hosted feed

### What to vendor

Put one file at `crates/rrouter-gateway/data/free-tiers.json`, embedded with `include_str!` the
same way `ui.rs` embeds its assets. It is read by `crates/rrouter-gateway/src/free_mode.rs`
alongside `default_catalog_sources()`. It holds **only providers RRouter can dispatch**
(compiled transports plus the keyless OpenAI-compat ones added in gap 4). Each row records
the upstream path it came from.

```json
{
  "upstream": {
    "repo": "https://github.com/tashfeenahmed/freellmapi",
    "sha": "6daa81b8def6a7e1d99fd67a611779913c287076",
    "reviewed_at": "2026-11-24",
    "license": "MIT",
    "copyright": "Copyright (c) 2026 Tashfeen Ahmed",
    "paths": [
      "server/src/providers/index.ts",
      "server/src/services/ratelimit.ts",
      "server/src/services/provider-quota.ts",
      "server/src/lib/error-classify.ts",
      "server/src/db/migrations/20260101_000000_legacy_baseline.ts",
      "docs/en/providers/01-supported-platforms.md",
      "docs/en/providers/02-quotas-and-cooldowns.md",
      "docs/en/architecture/00-high-level-index.md"
    ]
  },
  "providers": [
    {
      "id": "groq",
      "base_url": "https://api.groq.com/openai/v1",
      "auth": "key",
      "free_rule": "all-models-on-free-key",
      "pool": "per-model",
      "provider_caps": null,
      "quota_headers": { "requests": ["x-ratelimit-limit-requests", "x-ratelimit-remaining-requests", "x-ratelimit-reset-requests"],
                          "tokens":   ["x-ratelimit-limit-tokens",   "x-ratelimit-remaining-tokens"] },
      "tos": "likely-ok",
      "src": "server/src/providers/index.ts"
    },
    {
      "id": "openrouter",
      "base_url": "https://openrouter.ai/api/v1",
      "auth": "key",
      "free_rule": "id-suffix:free",
      "pool": "account",
      "provider_caps": { "rpd": 1000, "rpd_low_credit": 50 },
      "tos": "likely-ok",
      "src": "server/src/services/ratelimit.ts"
    },
    {
      "id": "kilo",
      "base_url": "https://api.kilo.ai/api/gateway/v1",
      "auth": "anonymous",
      "free_rule": "id-suffix:free",
      "provider_caps": { "rph_per_ip": 200 },
      "notes": "prompts/outputs logged for training",
      "src": "docs/en/providers/01-supported-platforms.md"
    }
  ],
  "models": [
    { "provider": "groq", "model": "llama-3.3-70b-versatile",
      "rpm": 30, "rpd": 1000, "tpm": 12000, "tpd": 100000, "context": 131072,
      "as_of": "2026-10-24", "src": "server/src/db/migrations/20260101_000000_legacy_baseline.ts" }
  ]
}
```

Rules for the file:

- **Hand-curated, never generated by running upstream code.** `export-catalog.ts` needs
  their DB and runtime. The baseline is an INSERT/UPDATE chain, so a row's final value is
  read by following every UPDATE for that `(platform, model_id)`. For example, Groq
  `llama-3.3-70b-versatile` is seeded at L364 with TPM 6000 and TPD 500k, then raised to TPM
  12000 at L656 and cut to TPD 100k at L1357. Record `as_of` per row
  because the repo rows are frozen while the hosted feed moves on.
- **A row is a seed, never an authority over live evidence.** Live public catalogs
  (OpenCode, OpenRouter) and the account's own `/v1/models` still decide whether a model
  exists. The vendored row only supplies limits and the free rule. Limits learned from
  provider errors (gap 6) only ever *tighten* a vendored value.
- **Licence.** Upstream is MIT, so copying is allowed if the copyright and permission
  notice go with the copied material. Keep `upstream.license` and `upstream.copyright` in
  the file, and add the full MIT text as `crates/rrouter-gateway/data/LICENSE.freellmapi`.
  Most of the content is facts (URLs, numeric limits), but carrying the notice costs nothing
  and removes any doubt. **None of it may come from `api.freellmapi.co`.** That feed is
  served by their service under its own terms, the live tier is licence-gated, and scraping
  the free monthly tier to rebuild it would be taking their paid product.

### The diff command (one command)

Run it from the RRouter root with an upstream clone at `$FREELLMAPI`. It fetches upstream
and lists what changed in every tracked path since the SHA the data file records:

```sh
git -C "$FREELLMAPI" fetch -q origin && git -C "$FREELLMAPI" diff --stat "$(jq -r .upstream.sha crates/rrouter-gateway/data/free-tiers.json)" origin/HEAD -- $(jq -r '.upstream.paths[]' crates/rrouter-gateway/data/free-tiers.json)
```

Drop `--stat` to read the hunks. After you reconcile, bump `upstream.sha` and `reviewed_at`
in the same change as the data edits, so the next diff starts from there. Until the data
file exists, this is the same check with the SHA written out:

```sh
git -C "$FREELLMAPI" fetch -q origin && git -C "$FREELLMAPI" diff --stat 6daa81b8def6a7e1d99fd67a611779913c287076 origin/HEAD -- server/src/providers/index.ts server/src/services/ratelimit.ts server/src/services/provider-quota.ts server/src/lib/error-classify.ts server/src/db/migrations/20260101_000000_legacy_baseline.ts docs/en/providers/01-supported-platforms.md docs/en/providers/02-quotas-and-cooldowns.md docs/en/architecture/00-high-level-index.md
```

`$FREELLMAPI` is a clone kept outside the RRouter tree and used only for reading. Never run
`npm install` or any script from it.

## Prioritized gaps

Sizes: S is under a day and one module; M is a few days and crosses router plus gateway; L is a
new subsystem.

1. **Free rule for whole-key free tiers (Groq, Cerebras, Gemini, Mistral).** *(#3, #1)*
   Today a free-mode Groq or Cerebras account discovers its models and classifies every one
   as `free = false`, so the pool cannot use the four best free tiers. Add the
   `free_rule` field from the vendored file: `all-models-on-free-key` means every model a
   free-mode account lists is free, but **only on a free-mode account**. That keeps the
   "never widen to paid" invariant, because the operator asserted the account is a free
   tier. Lives in `rrouter-gateway/src/free_mode.rs` (`classify_free`, `discover`) plus the
   new `data/free-tiers.json`. **M** (most of the size is curating the data file).

2. **Per-model RPD/TPM/TPD windows with daily exhaustion parked until UTC midnight.** *(#5,
   #8, #12)* Our only gate is a soft per-account RPM. A Gemini key at 20 RPD keeps getting
   picked, eats a 429, and ladders back after at most 1 h, all day long. Add day/token windows to
   `FreeRotation`, keyed `(connection, model)`, using UTC calendar days to match providers.
   Limits come from the vendored row or a learned value. Rebuild the day counts on boot from
   the existing `usage` rows (`created_at >= today 00:00 UTC`), so no new ledger table is
   needed. When a 429 is classified as daily (body says per-day/`RPD`/`quota exceeded for
   … per day`, or the day count ≥ RPD), cool until the next UTC midnight instead of 1 h.
   Lives in `rrouter-router/src/free_mode.rs` (windows), `breaker.rs` (a daily-exhausted
   cooldown variant) and `rrouter-store` (`free_models` columns `rpm, rpd, tpm, tpd,
   context` plus a read of today's usage). **M**.

3. **Provider-wide proactive caps.** *(#7)* The @free-fix work makes the OpenRouter platform
   429 cool the whole provider *after* it happens. With the gap-2 windows, also count
   per `(connection, provider)` against `provider_caps`: OpenRouter 1000/day (50 below 10
   credits), NVIDIA 40 RPM, Kilo/OVH per-IP. A drained account is then skipped without
   spending an attempt. Reuses the provider-limit map that already exists in `FreeRotation`.
   **S** after gap 2.

4. **Retry delay from the error body, plus limit learning.** *(#9, #10)* Extend
   `parse_retry_after_secs` into one `retry_hint(headers, body)`: the header (including
   the HTTP-date form) wins; otherwise a depth-capped walk for `retryDelay`/`retry_after`/
   `retryAfterSeconds`, then anchored prose ("try again in N s/m"), clamped to 24 h. Feed
   the result to the breaker as the *authoritative* cooldown. Separately, parse
   "Limit N … (TPM|RPM|TPD|RPD)" from 413/429 bodies and only ever lower the stored limit.
   Lives in `rrouter-gateway/src/free_mode.rs` next to `is_free_usage_limit_error` and
   `rrouter-router/src/dispatch.rs` (the existing upstream `Retry-After` read). **S**.

5. **More keyless providers: Kilo, OVH, LLM7.** *(#4)* All three are OpenAI-compat with a
   public, keyless route. Generalise `supports_anonymous_free` and `anonymous_endpoint_for`
   from the hard-coded `opencode` to the vendored `auth: "anonymous"` rows, and register
   each as a public catalog in `default_catalog_sources()` with its own free rule. The UI must say
   when a provider logs prompts for training (Kilo does). AI Horde needs a dedicated queue
   adapter, so it is not in this slice. Lives in `rrouter-router/src/free_mode.rs` and
   `rrouter-gateway/src/free_mode.rs`. **S** per provider once gap 1's data file exists.

6. **Persist cooldowns across restarts.** *(#12)* A restart today forgets every 429 bench,
   so the first minutes after a deploy re-hit exhausted free tiers. Add a small
   `free_cooldowns(connection_id, provider, model, until, source)` table, written when a
   cooldown is set and loaded into `Breaker`/`FreeRotation` at boot. Lives in
   `rrouter-store` (migration plus queries) and `rrouter-router` (load hook). **S**.

7. **Provider-reported quota from response headers.** *(#14, #21)* Parse per-provider
   `x-ratelimit-*` (header map in the vendored `quota_headers`) on every free response and
   keep the latest `{limit, remaining, reset}` per `(connection, provider|model)`. That
   gives the Usage/free-mode pages real "remaining today" numbers and lets gap 2 correct
   its counts. OpenRouter's `GET /api/v1/key` (free, unmetered) can be polled every 15 min
   for the credit and low-credit state that picks 50 vs 1000 RPD. Lives in a new
   `rrouter-gateway/src/free_quota.rs` (the same role `provider_usage.rs` plays for
   subscriptions; extend that module instead if its cache shape fits). **M**.

8. **Error classes beyond 429.** *(#11)* Add three classes:
   - 402 credit-exhausted: bench the connection 24 h and do not probe.
   - 403 model-not-on-tier: bench `(connection, model)` 24 h.
   - model retired: a 404/410 seen twice in 1 h marks the `free_models` row unusable,
     with the reason shown in the UI, the same way "Not usable here" works today.

   Lives in `breaker.rs` (new cooldown kinds) and `free_mode.rs`. **S**.

9. **ToS verdict per provider in the data file and UI.** *(#23)* Carry upstream's verdicts
   as `tos`: Gemini caution, NVIDIA evaluation-only, Cohere avoid (personal use forbidden),
   Z.ai caution. Show one line on the free-mode provider row. **S**.

10. **`.env` import.** *(#22)* Map `GROQ_API_KEY`, `CEREBRAS_API_KEY`, `GEMINI_API_KEY`,
    `OPENROUTER_API_KEY` and similar names to providers on the Providers page. Take
    upstream's prefix *idea*, not its 100+ relay prefixes. Lives in `ui/` (Providers) plus a
    small parser. **S**. Low priority.

The router's model choice (#17) stays "working first, then rotate". The gap-2 headroom
signal is enough to steer away from a nearly-exhausted model without a scoring engine.

## Deliberately not copied

- **The hosted signed catalog and its premium licence check** (`catalog-sync.ts`,
  `routes/premium.ts`). The feed is not MIT, it is a paid product, and it would make our
  routing depend on their uptime. We vendor repo data with a recorded SHA instead.
- **The Thompson-sampling bandit, community prior and decision-model routing**
  (`scoring.ts`, `decision.ts`). The community prior sends usage aggregates to their
  service, and decision routing spends an LLM call per user turn to choose weights. Our
  planner's job is "find a free model that answers". Probabilistic ordering would make the
  free-mode page's "next fallback model" line untrue, and it breaks the redesign's
  preview-honesty rule.
- **Most of the relay roster** (about 40 adapters such as sail, puter, aclide, nara,
  electronhub and similar). Many are resold or "promo" gateways with no stable terms. Some
  need Telegram verification or Chinese real-name auth, or take an *account* token (Puter)
  instead of an API key. Each is a maintenance and trust liability. Add a provider only
  when it is first-party or officially keyless, and has a ToS row.
- **Multi-account stacking framed as a quota multiplier.** Upstream's own rule of thumb is
  "one account per provider, no reselling, no sharing". Cerebras explicitly forbids
  transferring keys. Our Advanced document already supports several accounts and egress
  proxies per provider. We should not add more features whose only purpose is evading
  per-account or per-IP limits, and the gap-9 ToS line should sit next to that feature.
- **Peak-hours reweighting, degraded-mode state machine and hedging** (`degradation.ts`,
  `fallback-loop.ts` hedge). They add complexity for a multi-provider scale we do not have.
  `dispatch.rs` already bounds attempts and waits. Revisit degraded mode only if the pool
  grows past about 10 providers.
- **Background key-health polling every 5 minutes** (`health.ts`). The redesign deliberately
  probes only on enable or Try. Upstream's own history (SerpSearch revoking an account over
  empty probe searches, ModelScope paid probes) shows the cost of the alternative. Header
  observation (gap 7) gives freshness without spending quota.
- **Tool-call rescue, response cache, compression, media, search, playground, desktop, MCP.**
  These are out of scope for free-tier aggregation, and we already have our own caching and
  compression.
- **Sticky sessions keyed by SHA-1 of the first user message.** They collide across users
  who send the same opener, and our session handling already exists elsewhere.
