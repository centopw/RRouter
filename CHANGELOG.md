# Changelog

All notable changes. Dates are local; versions follow semver once tagged.

## 0.34.1 — 2026-11-24

### Other Changes

- test(e2e): group models are advertised under the stable slug ([`5446cb9`](https://github.com/centopw/RRouter/commit/5446cb9877412fff6f2156effc58c6a2e1e92860))

## 0.34.0 — 2026-11-24

### Features

- feat(ui): redesign free mode, providers, models and usage; show requested vs routed model ([`5d0ab97`](https://github.com/centopw/RRouter/commit/5d0ab97b16a30166561ff0f6852f704d486b7db3))
- feat(routing): zero-config free mode, stable group slugs and per-key default group ([`be82d23`](https://github.com/centopw/RRouter/commit/be82d23f7ced6f8d067591e63d6a51b19ada9d96))

### Other Changes

- chore(ui): rebuild the served bundle ([`d986b3f`](https://github.com/centopw/RRouter/commit/d986b3fadc8a07d4b9e6b792b71a2b043273f3fc))

## 0.33.0 — 2026-11-23

### Features

- feat(ui): usage page leads with the limit, the pace and a forecast chart ([`97a8240`](https://github.com/centopw/RRouter/commit/97a8240139f80e65be2ee343175403654c143e77))
- feat(gateway): quota history endpoint, concurrent quota fetch, 35-day sample retention ([`e315285`](https://github.com/centopw/RRouter/commit/e31528588f1167ca8ab46cd9f94f560f304e6d26))

### Bug Fixes

- fix(deps): bump url to 2.5.9 for RUSTSEC-2026-0592 ([`10c6838`](https://github.com/centopw/RRouter/commit/10c6838ec7f0d20b992f2331bf2f8679393c6103))

### Performance

- perf(store): let usage and compression range filters use the ts index ([`5587c47`](https://github.com/centopw/RRouter/commit/5587c47cf4fea5a8d31d256db9004ed388dea3e0))

### Build & CI

- ci: release builds the dashboard itself instead of trusting the committed bundle ([`005bcf2`](https://github.com/centopw/RRouter/commit/005bcf2bccba1ea954506514bcc13a4a3d545eb1))
- ci: audit stays a visible check but no longer gates image publish ([`5be29c6`](https://github.com/centopw/RRouter/commit/5be29c6a1a33711cfee39bcf2663a48a8feb6aee))
- ci: build and test each commit once; re-tag instead of rebuild on release ([`5f23dc5`](https://github.com/centopw/RRouter/commit/5f23dc51be6424aaa508614814357029db5c6941))

### Other Changes

- chore(ui): rebuild the served bundle ([`daaee8b`](https://github.com/centopw/RRouter/commit/daaee8b8922714d7120721a74c4111297c7261ba))

## 0.32.0 — 2026-11-21

### Features

- feat(proxy): route every account's calls through its own egress proxy ([`bf441d1`](https://github.com/centopw/RRouter/commit/bf441d100efa4613e711938e6700b7974448c69e))
- feat(free-mode): import document, discovery and admin CLI ([`d0d7835`](https://github.com/centopw/RRouter/commit/d0d78357c1af33f94244a3d74506af39266a8ead))
- feat(router): reserved free refs and anti-rate-limit rotation ([`1faf179`](https://github.com/centopw/RRouter/commit/1faf179e6f6f123a77b66b73fd31dfaa19458652))
- feat(ui): free-mode dashboard with document import and model selection ([`1a5be53`](https://github.com/centopw/RRouter/commit/1a5be5333d853279888aafaa9f5fbc9fbb5f83d3))
- feat(free-mode): per-account proxy credential and free-mode storage ([`b4c1b0f`](https://github.com/centopw/RRouter/commit/b4c1b0fb227e684d59b327140f0d7ede953475e9))
- feat(ui): show a provider refusal as its own badge, not as a disable ([`0f61b71`](https://github.com/centopw/RRouter/commit/0f61b71ab6be832e5e280e989bafa714b42b2706))
- feat(ui): map connection refusal tokens to copy and severity ([`206fbde`](https://github.com/centopw/RRouter/commit/206fbde4b148e5661f19a38355c417ef8507566d))
- feat(store,gateway): persist the reason for a recoverable account refusal ([`f171458`](https://github.com/centopw/RRouter/commit/f171458e560c40f3a8291965280e2fa5d09d2a7a))
- feat(gateway): retire claude accounts on an entitlement refusal ([`b904f85`](https://github.com/centopw/RRouter/commit/b904f85bcbf8a464a44d0729e66282ea34de1ed2))
- feat(core,translate): classify Claude subscription refusals ([`807dbaf`](https://github.com/centopw/RRouter/commit/807dbaf6a900ebdf79689c89043438e55f773756))
- feat(gateway): refresh once and retry on a pre-stream 401 ([`79686f5`](https://github.com/centopw/RRouter/commit/79686f595c2f9a8e08b50b9e34fbe6954885e15a))
- feat(auth): refresh a credential the upstream rejected ([`84f940c`](https://github.com/centopw/RRouter/commit/84f940ccef993c896a080d72761ec0a2ad139b1f))
- feat(gateway): retire permanently-refused accounts ([`092c7a8`](https://github.com/centopw/RRouter/commit/092c7a813f10db52de63f292c8fb78ac9fe2c6fd))
- feat(translate): classify upstream account refusals ([`856310c`](https://github.com/centopw/RRouter/commit/856310c5e7742076af3337997ebd4561927c60cd))
- feat(ui): provider cards and live tryout on Search, Rerank, Embeddings ([`22f4248`](https://github.com/centopw/RRouter/commit/22f4248eef13327ebbcbb924fb6500d12ae36ed0))
- feat(compression): record and surface what compression removed ([`57b8fef`](https://github.com/centopw/RRouter/commit/57b8fef8ae35db7b4b3de31715b46bf9554cc1b3))
- feat: compression page, capability defaults, and appearance settings ([`00de2e1`](https://github.com/centopw/RRouter/commit/00de2e101a33b08836e8610a49ddaa6c880b5c86))

### Bug Fixes

- fix(free-mode): discovery leaves through the account's proxy; prove account rotation ([`4674782`](https://github.com/centopw/RRouter/commit/467478257b9c76f2028409d90b50e05ff8f8c5c8))
- fix(auth): refresh the OAuth override cache on demand ([`03a9aeb`](https://github.com/centopw/RRouter/commit/03a9aeb783e1d2eff903a960010c8eab037a5178))
- fix(gateway): bench and retire an account refused inside a stream ([`3ff36d9`](https://github.com/centopw/RRouter/commit/3ff36d90f3070eea26474fde422ad2a53adc4a50))
- fix(antigravity): derive wire marker and platform enum from one identity ([`687d7f1`](https://github.com/centopw/RRouter/commit/687d7f1dcce961824d4edec9b846557c0676983f))
- fix(translate): never repair away the CCA envelope's identity fields ([`673534c`](https://github.com/centopw/RRouter/commit/673534c4f870139a19d6adfa1eb5c821eb8aad64))
- fix(ui): consolidate Quick start into one card with modals ([`44aa35e`](https://github.com/centopw/RRouter/commit/44aa35e93ba8fcb657865f50e6870a1330ac6a64))

### Documentation

- docs(gateway): retire-on-refusal write is retried; record the mid-stream rule ([`17cfc87`](https://github.com/centopw/RRouter/commit/17cfc87d0e9d406406b00eab7a6e8f62f2e2a340))
- docs: describe compression effectiveness and the capability catalog ([`5b56d13`](https://github.com/centopw/RRouter/commit/5b56d13bec8edb0606264b6d4dc40aacb8e42563))

### Build & CI

- build(deps): bump the dashboard group in /ui with 5 updates (#10) ([`0879d2d`](https://github.com/centopw/RRouter/commit/0879d2dfc0bbbc6779ed57780d5a86cc5e739caf))
- build(deps): bump the rust group across 1 directory with 3 updates (#11) ([`3be4e07`](https://github.com/centopw/RRouter/commit/3be4e073ae4579e49a66400fdc3593c4d4a51508))

### Other Changes

- test(free-mode): treat any non-200 state read as blocked, not a failure ([`df37270`](https://github.com/centopw/RRouter/commit/df37270e0788d185bd52ffe91ba34407428e2d37))
- test(free-mode): acceptance harness for import, discovery and free routing ([`e2d7e0d`](https://github.com/centopw/RRouter/commit/e2d7e0d40d0dbbf501ac11b7d9bdfa66e7fd04ce))
- test(store): pin free-mode upsert, ordering and document round-trip ([`bf9859c`](https://github.com/centopw/RRouter/commit/bf9859c01b238bfb299972e6fbd6a082b1ae39aa))
- merge fix/test-temp-dirs ([`d8be8ad`](https://github.com/centopw/RRouter/commit/d8be8ad9436b230c834bf7548cbb27e0fe7a4812))
- test(gateway): give the remaining temp-dir helpers the same guard ([`35ed4df`](https://github.com/centopw/RRouter/commit/35ed4dfb965dacdbf53f3e44d0fd5354cb3f6f5f))
- test(gateway): hold the temp data dir in the test harnesses ([`64606f5`](https://github.com/centopw/RRouter/commit/64606f59b8414e63010b7838f1db8a19f8908035))
- test(gateway): add a shared temp data-dir guard for the integration tests ([`5f69970`](https://github.com/centopw/RRouter/commit/5f699701455dd42b80eeaa7e50c9c6ec5b33aed5))
- merge fix/refusal-badge ([`89fca32`](https://github.com/centopw/RRouter/commit/89fca3259b8ab78ab527dc79a50432da8b541971))
- merge fix/verify-reason-persist ([`a11a269`](https://github.com/centopw/RRouter/commit/a11a26919a6b3be524e37bac0dcc356918e3e162))
- merge fix/cca-stream-retire ([`36206e9`](https://github.com/centopw/RRouter/commit/36206e9110652e941c58c0cd809bd4ea9c6b95f7))
- chore(ui): rebuild the served bundle ([`1612d7c`](https://github.com/centopw/RRouter/commit/1612d7c8ec204eacebd0c25502e01bafe8a5798b))
- test: a recoverable refusal is recorded and still routable ([`f8a9b35`](https://github.com/centopw/RRouter/commit/f8a9b357aeed26c59f3817071bc79406c274e3ef))
- merge fix/cc-401-refresh ([`933f7e7`](https://github.com/centopw/RRouter/commit/933f7e77ed4525dd16c78d4562bfcb38b32c5fec))
- merge fix/cc-refusal-taxonomy ([`c24f645`](https://github.com/centopw/RRouter/commit/c24f64544bd18ac46fdc014585b312aa0fc167ee))
- test(antigravity): keep provider wording out of assertion sinks ([`5dc7192`](https://github.com/centopw/RRouter/commit/5dc71921642579bd281352ed72815a57ae4fb69f))
- merge fix/ban-classification ([`d65d143`](https://github.com/centopw/RRouter/commit/d65d143bb8c7607190871ac6ec155dc43599dae7))
- merge fix/identity-consistency ([`5fed2a8`](https://github.com/centopw/RRouter/commit/5fed2a811eb640df1efb6e121a6b9d32a96d6a97))
- merge fix/envelope-repair ([`0c12ba2`](https://github.com/centopw/RRouter/commit/0c12ba2bca06844b3da881d2f7f72682e84ca1fa))

## Unreleased

### Features

- **Compression effectiveness.** Every request whose body compression changed now records
  one row in `compression_events` (schema V42: request, key, team, RTK bytes in and out,
  Caveman injected) and `GET /api/compression/stats?hours=&team_id=` aggregates them over
  a window of up to a year. Counters are tool-result text bytes only; saved tokens are
  estimated at ~4 bytes per token, never a tokenizer run. The rows follow usage retention.
  The Compression page gains an Effect card (24h/7d/30d) and Activity a one-line readout,
  shown only when the window has events. See [Compression → Effectiveness](docs/COMPRESSION.md#effectiveness).
- **Compression page and Caveman.** Compression settings now live under one `compression`
  key (`rtk`, `caveman`, `caveman_level` of `lite`/`full`/`ultra`) and move to their own
  ROUTING → Compression page. RTK keeps its on-by-default behaviour and a legacy flat `rtk`
  is still read, never written. The new Caveman method appends an idempotent terse-output
  instruction to the system slot of OpenAI Chat, OpenAI Responses and Anthropic Messages
  bodies after RTK, fails open on unexpected shapes, is bypassable with
  `x-rrouter-caveman: off`, and is counted as `rrouter_caveman_injected_total`. See
  [Compression](docs/COMPRESSION.md).
- **Capability catalog and live tryout.** ROUTING → Search, Rerank and Embeddings now list
  every provider that serves the capability as a card with its connected state and active
  account count, a Connect link that opens the add-provider flow on that provider
  (`/providers?add=<id>`) and a Manage link for connected ones. Each card opens a drawer
  holding a session-scoped gateway key, an editable model ref prefilled per capability, and
  a tryout that calls `/v1/search`, `/v1/rerank` or `/v1/embeddings` for real, showing
  status, latency, `x-request-id` and the body, plus a copyable curl with a
  `$RROUTER_API_KEY` placeholder. Search offers `web` only; news and images are disabled
  rather than silently dropped. See
  [API → Capability defaults](docs/API.md#capability-defaults).
- **Capability defaults.** Search, rerank and embeddings accept a stored provider/model
  default (`capability_defaults`, deep-merged by `PUT /api/settings`, null or a trimmed
  reference of at most 200 characters) used only when the request omits that field. New
  admin `GET /api/capabilities` lists the defaults and the providers that serve each
  capability; three dashboard pages (ROUTING → Search, Rerank, Embeddings) edit them with
  advisory suggestions and accept free text. See
  [API → Capability defaults](docs/API.md#capability-defaults).
- **Appearance and runtime settings cleanup.** An Appearance settings tab now owns color
  mode and display density (per browser, applied immediately), replacing the sidebar and
  command-palette toggles. The sidebar footer shows the running app version from `/health`.
  On Runtime, captured-body retention moves into the Request capture card, so Stored
  history no longer edits the same setting; each card saves only its own keys.

### Bug Fixes

- **A refusal inside a `200` stream benches the account.** A Cloud Code Assist (or Claude)
  refusal that arrived as an error frame in an already-open stream was classified and, for
  the permanent classes, retired — but it was never booked against the breaker, and the
  dispatcher's stream wrapper settled it as a *success* on drop. A `VALIDATION_REQUIRED`
  account therefore kept being selected after it had just said it could not serve. The
  terminal-error carrier now settles the upstream body first and records the refusal with
  the provider's own status for every class, retiring only the permanent ones. The
  retirement write is also retried (3 attempts) and logged at `error` when it is lost,
  instead of a single best-effort attempt whose failure only appeared as traffic the
  provider kept refusing.
- **Suspended Antigravity accounts leave rotation.** Cloud Code Assist refusals are now
  classified instead of flattened into a translation error: a `TOS_VIOLATION` answer (or
  the "service has been disabled in this account" wording) retires the connection —
  `is_active = 0` plus the new `connections.refusal` token (schema V43) — so it stops being
  a candidate for every model and the client gets a `403 account_suspended` naming the
  suspension. A `VALIDATION_REQUIRED` / "Verify your account to continue" answer is
  recoverable and never retires the account. Neither class is retried on the same account
  within a request. See [Providers → Google Antigravity](docs/PROVIDERS.md#google-antigravity-cloud-code-assist).
- **Claude subscriptions: entitlement is not a throttle.** The subscription gate's
  `429 {"type":"error","error":{"type":"rate_limit_error","message":"Error"}}` was cooling
  the account down and re-probing it forever, because it wears a rate limit's status. It
  is now classified as an **entitlement** refusal — as are "This credential is only
  authorized for use with Claude Code", "Third-party apps now draw from your extra usage"
  and `403 "Request not allowed"` — and the connection is retired (`is_active = 0`,
  `connections.refusal = account_not_entitled`) with a `403 account_not_entitled` naming
  the entitlement rather than a suspension. Disabled accounts and organizations stay
  `account_suspended`; a rejected client version is `client_version_rejected` (`502`, the
  account is kept — the fault is this build's); a `429` carrying `anthropic-ratelimit-*`
  headers, `overloaded_error`/`529`, any `5xx` and a revoked `401` never classify. An
  `event: error` frame inside a `200` stream is classified the same way and retires the
  account too. See [Providers](docs/PROVIDERS.md#providers).

## 0.31.0 — 2026-11-18

### Features

- feat(ui): expose stored-history retention in Runtime settings ([`10b9355`](https://github.com/centopw/RRouter/commit/10b9355fb9c747e1126f8de17af48f6d4cd48c49))
- feat(settings): give every stored artifact its own switch and lifetime ([`471c126`](https://github.com/centopw/RRouter/commit/471c12699d4116d2efd0f5511db3f72c92d7df1d))

### Other Changes

- chore(ui): rebuild the served bundle ([`b782e30`](https://github.com/centopw/RRouter/commit/b782e30bd3f6463d1f8975b9dcbca241a6a53443))

## 0.30.0 — 2026-11-18

### Features

- feat(ui): ask for a GitHub star in the sidebar and above every page ([`794805d`](https://github.com/centopw/RRouter/commit/794805d825826da46997385d8666f07294013f29))

### Bug Fixes

- fix(usage): attribute a failed request to the account that answered ([`996fccf`](https://github.com/centopw/RRouter/commit/996fccf5fc6b4be78bb856538d2e33e7ee9bfdd2))

### Other Changes

- chore(ui): rebuild the served bundle ([`e6877f4`](https://github.com/centopw/RRouter/commit/e6877f4a67b9f2995b92898047f3ea6a83c4c9aa))

## 0.29.1 — 2026-11-18

### Bug Fixes

- fix(providers): carry the client fingerprints the upstreams gate on ([`98ac8f5`](https://github.com/centopw/RRouter/commit/98ac8f5c77921bce6b1ad2c1c5bd95bbdb584a39))

## 0.29.0 — 2026-11-18

### Features

- feat(ui): page every table, keep rows on one line, stop columns moving ([`060d5d6`](https://github.com/centopw/RRouter/commit/060d5d69693b9d4897d45218e8de8ff86599558b))
- feat(logging): delete log rows and opt-in request/response capture ([`2522d62`](https://github.com/centopw/RRouter/commit/2522d62b11071a52824b6f636f88baa44d8209b6))

### Bug Fixes

- fix(audit): record the real client address for every audited handler ([`fa9378d`](https://github.com/centopw/RRouter/commit/fa9378dcd171ae51f3bf1cf41deae19c35cb3ea3))

### Other Changes

- chore(ui): rebuild the served bundle ([`dde05bf`](https://github.com/centopw/RRouter/commit/dde05bf8a119dbc450fa6a8e4e40420bdd9660f2))

## 0.28.0 — 2026-11-18

### Features

- feat(ui): flatten dashboard navigation and reduce colour to signal ([`4c6f805`](https://github.com/centopw/RRouter/commit/4c6f805f43e924eec556ddefffd319e1ae4a52db))
- feat(usage): exact per-connection aggregates and key last-used ([`86880ad`](https://github.com/centopw/RRouter/commit/86880ad58210680a5617d7874f5f142b38aae9fd))

## 0.27.0 — 2026-11-18

### Features

- feat(ui): overhaul design system, navigation hubs, and modular page drawers ([`21be296`](https://github.com/centopw/RRouter/commit/21be2967171fb19795eed7629d572c21136e8112))
- feat(models): per-member combo metadata and first-class combo rows ([`4c29869`](https://github.com/centopw/RRouter/commit/4c2986967f46174815cb12a7e4cff3e9552a89c7))

### Documentation

- docs(design): models/combo redesign plan and providers plan ([`b5d913a`](https://github.com/centopw/RRouter/commit/b5d913a98f087f943aa246ea042f65645d2f2d27))

## 0.26.0 — 2026-11-17

### Features

- feat: provider shorthand codes and operator-defined providers ([`d7f871a`](https://github.com/centopw/RRouter/commit/d7f871a7e596769f651991e7a641573a38874474))

## 0.25.0 — 2026-11-17

### Features

- feat: request context in activity and provider-pinned model refs ([`e89ea7a`](https://github.com/centopw/RRouter/commit/e89ea7a1ceb42120a8afbbcb6fe8435e24a5cc28))

## 0.24.1 — 2026-11-16

### Bug Fixes

- fix(ui): replace the live-routing status dot and inset the usage bar ([`1bed177`](https://github.com/centopw/RRouter/commit/1bed1778891cc7a9b751f3695f609f4237163c79))

### Other Changes

- chore(ui): rebuild dashboard bundle ([`f8b52a2`](https://github.com/centopw/RRouter/commit/f8b52a20406d060d28b7f6ac0beb1ddae4507081))

## 0.24.0 — 2026-11-16

### Features

- feat(quota): read OpenCode Go plan usage ([`fec496e`](https://github.com/centopw/RRouter/commit/fec496e))

## 0.23.3 — 2026-11-16

### Features

- feat(proxy): preserve Remote Compaction V2 opaque items ([`288cd9e`](https://github.com/centopw/RRouter/commit/288cd9e0ac4960ff328e073d9d384e15a1580fa3))

## 0.23.2 — 2026-11-16

### Bug Fixes

- fix(ui): space quick-start cards and pad the steps ([`e85200a`](https://github.com/centopw/RRouter/commit/e85200a60f034708c310ead9b36791499588934c))

## 0.23.1 — 2026-11-16

### Bug Fixes

- fix(ui): configure API keys in the quick-start drawer ([`fc82bfe`](https://github.com/centopw/RRouter/commit/fc82bfeed1a3186accd92ed4e7e32305c20efb44))

## 0.23.0 — 2026-11-16

### Features

- feat(auth): add OpenRouter OAuth PKCE sign-in ([`9cfd0de`](https://github.com/centopw/RRouter/commit/9cfd0dec743871df61e46a57710287097e05be08))

### Other Changes

- test(gateway): cover OpenRouter OAuth start ([`650c7d7`](https://github.com/centopw/RRouter/commit/650c7d78b6253fdf91c2568d6caed0fac7b55231))

## 0.22.0 — 2026-11-16

### Features

- feat(ui): create and paste API keys on quick start ([`a11592e`](https://github.com/centopw/RRouter/commit/a11592e858d37ab46b0e49cb216caa01f4f35303))

## 0.21.2 — 2026-11-16

### Bug Fixes

- fix(ui): prefer an OpenAI GPT model in OpenAI guides ([`4224162`](https://github.com/centopw/RRouter/commit/422416294ed83c49f4d385933b12081fe740cae1))

## 0.21.1 — 2026-11-16

### Bug Fixes

- fix(ui): choose a model that matches the selected wire format ([`32e2966`](https://github.com/centopw/RRouter/commit/32e29666c605c9f807627d7426edf2ff3c3c773e))

## 0.21.0 — 2026-11-16

### Features

- feat(ui): turn quick start into a platform-driven setup guide ([`8798e7f`](https://github.com/centopw/RRouter/commit/8798e7f7126f789bfae528b6dc762f5bf2e554f4))

### Other Changes

- chore(ui): wire quick start route and rebuild dashboard assets ([`f4a7afb`](https://github.com/centopw/RRouter/commit/f4a7afb91807d02d285788a4e7d13f6022301f04))

## 0.20.0 — 2026-11-16

### Features

- feat(ui): add connection guides and provider compaction metadata ([`f4f5307`](https://github.com/centopw/RRouter/commit/f4f53070cc9d851e4ef93909a88be89e39af86b8))

### Documentation

- docs(providers): note server compaction pass-through ([`84ef22f`](https://github.com/centopw/RRouter/commit/84ef22fe609a53fc9c088fa6b19c61f4d02e0f9b))

## 0.19.0 — 2026-11-16

### Features

- feat(scripts): choose releases by bump type from the latest tag ([`aee8ad2`](https://github.com/centopw/RRouter/commit/aee8ad24c79f60f02ad877a2c4af785f601db4dd))

## 0.18.2 — 2026-11-16

### Bug Fixes

- fix(ui): keep narrow routing cards inside the canvas ([`672e087`](https://github.com/centopw/RRouter/commit/672e0873f4e8783256fbb06248660caeb6ae2c20))

## 0.18.1 — 2026-11-16

### Bug Fixes

- fix(ui): match the keys status filter to the visible badge ([`e7a4c56`](https://github.com/centopw/RRouter/commit/e7a4c560c21e09ea6d988906711bd70adfb3f225))

## 0.18.0 — 2026-11-16

### Features

- feat(ui): make live routing interactive and polish keys ([`d11c1a2`](https://github.com/centopw/RRouter/commit/d11c1a27b22ee25fd205c6831665633b890c548c))

## 0.17.0 — 2026-11-16

### Features

- feat(ui): group models and add thinking controls ([`7970422`](https://github.com/centopw/RRouter/commit/7970422fee088633b3b6a005f8f9b651082fdeaa))

### Bug Fixes

- fix(models): keep explicit reasoning flags and combo levels conservative ([`8ce09b2`](https://github.com/centopw/RRouter/commit/8ce09b2d7d3127fdc09ff0222f7de47943ece2d6))

## 0.16.1 — 2026-11-16

### Bug Fixes

- fix(upstream): refuse a catalog with no routable rows ([`b1cb4ba`](https://github.com/centopw/RRouter/commit/b1cb4ba2163de82a6093af920b596e7d3dc87c9a))
- fix(ui): repair models density, activity trends and first login ([`298c032`](https://github.com/centopw/RRouter/commit/298c03284b1e2ee006178da4f7f109cc474646c8))
- fix(ui): repair models density, activity trends and first login ([`464554b`](https://github.com/centopw/RRouter/commit/464554b8933bf38a7a7e7267ba24cce2efaaf25f))

## 0.16.0 — 2026-11-15

### Features

- feat(ui): unify models page and split routing navigation ([`ca73122`](https://github.com/centopw/RRouter/commit/ca731221fa5ddbdbf13dab3016f8001a0b31e3f4))

## 0.15.1 — 2026-11-13

### Bug Fixes

- fix(cca): adapt JSON Schema type unions in tool parameters for the Gemini proto ([`f8613f4`](https://github.com/centopw/RRouter/commit/f8613f47852f05023a4290d9d00b1a297bda3931))

## 0.15.0 — 2026-11-13

### Features

- feat(qoder): accept a personal access token via job-token exchange ([`18e8464`](https://github.com/centopw/RRouter/commit/18e8464f4e0b254a1a0e77dec5ed040a7d506522))
- feat(qoder): first-class Qoder subscription provider with COSY signing ([`dcf7c0a`](https://github.com/centopw/RRouter/commit/dcf7c0aaf72c02843a10d47f9583f35074cb15ec))
- feat(qoder): first-class Qoder provider over its COSY-signed agent wire ([`6f639d8`](https://github.com/centopw/RRouter/commit/6f639d87ea4ea151b4840709cc22f663b74d2ae1))

## 0.14.0 — 2026-11-13

### Features

- feat(providers): let operators rename provider accounts ([`45505e7`](https://github.com/centopw/RRouter/commit/45505e7cf8208b9b512fbd996fa406eca36dfd99))
- feat(providers): let an operator name a connection account ([`df420af`](https://github.com/centopw/RRouter/commit/df420af56bdea1444ad705c444a9fba22d20aa76))

### Bug Fixes

- fix(cca): repair envelope fields the upstream rejects ([`3edeafd`](https://github.com/centopw/RRouter/commit/3edeafd42b3dff7ad48d3f60144eaf720cfb9095))
- fix(cca): repair an envelope field the upstream rejects instead of always sending it ([`6d31327`](https://github.com/centopw/RRouter/commit/6d3132724b6edb4f5055b3e275fcda43906b0b68))

### Refactoring

- refactor(ui): split Routing > Models into Active, Registry and Names views ([`3819e5d`](https://github.com/centopw/RRouter/commit/3819e5de2819f0d341b05707ee95e40ac1772e4f))
- refactor(ui): split Routing → Models into Active, Registry and Names ([`bdb74a5`](https://github.com/centopw/RRouter/commit/bdb74a57e99033686c01a361a8b53bf94e13f962))

### Build & CI

- build(ui): rebuild dist with the models views and account rename ([`06fbecf`](https://github.com/centopw/RRouter/commit/06fbecff4e69b06ec7281a8e8fd324cb6c2bd921))

## 0.13.1 — 2026-11-12

### Bug Fixes

- fix(deps): drop the RustCrypto RSA stack for the AWS-LC HS256 backend ([`340ea24`](https://github.com/centopw/RRouter/commit/340ea2409ab865003e55772768516a57e2778a31))
- fix(deps): pin jsonwebtoken to the aws_lc_rs backend to drop the rsa stack ([`7f4d1c7`](https://github.com/centopw/RRouter/commit/7f4d1c71359e5effa080c852322d84c01f7aad7d))

## 0.13.0 — 2026-11-12

### Features

- feat(playground): give the playground its own gateway key and prompt enhancer ([`600aa3f`](https://github.com/centopw/RRouter/commit/600aa3fae75c2f1eaf836cd26cea47f8b899e17e))

### Bug Fixes

- fix(playground): let the admin token admit the playground key ([`5f93f01`](https://github.com/centopw/RRouter/commit/5f93f01b24551789e222c665717c0d8b47bc0513))
- fix(playground): admit the admin token as admin authority for /v1 ([`53a1b1f`](https://github.com/centopw/RRouter/commit/53a1b1fb89dbbb8408419f166b0db47a0efe1fe4))
- fix(update): drop update state cached by another build ([`e471161`](https://github.com/centopw/RRouter/commit/e4711613996ee3debbf315fae51cd2355f81693f))

### Build & CI

- build(ui): rebuild dist with the playground key and update card changes ([`f4f1325`](https://github.com/centopw/RRouter/commit/f4f132541be71ad208266e03a3faf7274afebb35))

## 0.12.2 — 2026-11-12

### Bug Fixes

- fix(update): compare release SemVer and surface updates in the sidebar ([`64afdbd`](https://github.com/centopw/RRouter/commit/64afdbd63c89012c8db2074c47946ba14a8b20cc))
- fix(release): refuse untracked files before publish ([`b357c2c`](https://github.com/centopw/RRouter/commit/b357c2c956c7a4857b4c216305094703fb125bbe))

### Other Changes

- chore: track the wiki sync tool and ignore dogfood output ([`8595bf4`](https://github.com/centopw/RRouter/commit/8595bf45de28336711614e6e8372f51498ad16a9))

## 0.12.1 — 2026-11-12

### Bug Fixes

- fix(auth): ship the built-in Antigravity OAuth client ([`191cd86`](https://github.com/centopw/RRouter/commit/191cd86b1e9356db6616a6fc28d08b8a4b2588fc))
- fix(release): track the reasoning translator module ([`5a16692`](https://github.com/centopw/RRouter/commit/5a16692e22131535dcba80bc72f5f6c27faaeb06))

## 0.12.0 — 2026-11-12

### Features

- feat(reasoning): normalize thinking effort across dialects ([`fbdabd8`](https://github.com/centopw/RRouter/commit/fbdabd834619005e81aaa834b66de8a86424a9d4))

## 0.11.0 — 2026-11-11

### Features

- feat(models): group-qualified ids, routing and access grants ([`ffb5f53`](https://github.com/centopw/RRouter/commit/ffb5f5378758d7de5e477dc5702c151751aaf5eb))

### Build & CI

- ci(release): validate version carriers at the tag ([`11811b5`](https://github.com/centopw/RRouter/commit/11811b57f524d1531fb8cc442fe0cc377cbdff20))

## 0.10.0 — 2026-11-11

### Features

- feat(ui): make copy fields selectable and add model reference copy ([`f8b409c`](https://github.com/centopw/RRouter/commit/f8b409c1c50e1130801ac1515179c295d497d56a))
- feat(release): derive version and notes from conventional commits ([`46fa832`](https://github.com/centopw/RRouter/commit/46fa8323d3d476c7f21cac0997db953530a4c23b))

### Bug Fixes

- fix(release): stage tracked carriers with git add -u ([`4c45954`](https://github.com/centopw/RRouter/commit/4c45954ac58199476cc7c6a1bcfff56135b7b943))

### Other Changes

- test(release): cover patch numbers above 100 ([`cee874f`](https://github.com/centopw/RRouter/commit/cee874f94e92662d8a602681a41928c941051606))

## 0.9.0 — 2026-11-11

### Changed
- `/v1/models` identifies provider models as `provider/model` (the same reference routing
  accepts) and keeps the bare id in `model`. Aliases and combos keep their bare names. This
  is a breaking change for clients that expected the bare id in `id`; `GET /v1/models/{id}`
  still accepts either spelling and returns the canonical `provider/model` id.
- Bare ids no longer fuzzy-match across unrelated families. Dotted version segments stay in
  the model core, so `deepseek-v4.1-flash` is treated as an unknown id and tried against
  active providers instead of pinning itself to unconnected DeepSeek catalog entries. This
  removes the `no eligible upstream … resolved to 3 provider/model target(s)` 404 for bare
  requests that a connected aggregator (for example OpenCode Go) actually serves.

## 0.8.0 — 2026-11-11

### Added
- `/v1/models` and `/v1/models/{id}` entries now carry model metadata: `name`,
  `context_window`/`context_length`, `max_output_tokens`/`max_completion_tokens`,
  `input_modalities`, `output_modalities`, the nine capability flags in camelCase,
  provider-published `effort` and `api_capabilities`, and `deprecated` when known.
  The OpenAI list shape stays additive and unchanged for clients that ignore the fields.
- Provider `/models` fetches now keep native metadata instead of only ids: DeepSeek-style
  top-level limits and modalities, and 9router-style `capabilities` with
  `thinkingFormat`/`thinkingRange` are normalized and stored. An id-only list (OpenAI)
  still works and is enriched from models.dev or operator rows.
- Alias entries borrow their target's metadata. Combo entries advertise the conservative
  intersection of their members: smallest context/output and only capabilities every
  member supports; an unresolved member leaves the combo metadata unknown.

## 0.7.3 — 2026-11-11

### Fixed
- The Activity failing-model alert can be dismissed; the dismissal lasts for the tab and
  clears once the incident resolves, so a later recurrence alerts again. Its primary action
  now opens **Logs** with the failing model, error status and current range preselected, and
  the dead-end **Combos** link is gone.
- **Logs** accepts `status`, `provider`, `model` and `range` deep links. A deep-linked
  provider or model the catalogue does not list stays visible in its select instead of
  snapping back to "All".

## 0.7.2 — 2026-11-11

### Fixed
- OpenCode and OpenCode Go requests now send a stable `x-opencode-session` derived from the
  conversation affinity, fixing `400 MissingSessionID` on OpenCode Go.
- Group headings use the same horizontal padding and border treatment as the grouped table.

### Changed
- The default model preset now controls `/v1/models` discovery: hidden items are omitted,
  a group toggled off is omitted with its models, and `auto_include_new: false` drops models
  the preset never names. Routing is unchanged; hidden models still work when named.
- The group manager can add or remove models directly for a custom group, with a filter.
  Aliases and combos are listed there and in a new **Aliases & combos** card, with a group
  picker and a `/v1/models` visibility toggle.

## 0.7.1 — 2026-11-11

### Fixed
- The Models board no longer renders preset entries whose provider is disconnected or
  removed. The entries stay in the preset document so reconnecting restores them, but they
  are omitted from the active/hidden lists, group buckets and counts. `/v1/models` likewise
  no longer advertises a provider bucket that has no live model. This removes the stale
  "unavailable" rows left behind after a provider connection was deleted.

## 0.7.0 — 2026-11-10

### Added
- Model groups on the **Models** page and in model presets. The board is grouped by
  provider by default; operators can create free-form groups, rename provider or free-form
  groups, reorder groups, and move a model to another group. Grouping is presentation only.
- `/v1/models` now carries additive group metadata: each entry has `group`, and the top level
  has an ordered `groups` array from the instance default preset. The flat `data` list still
  contains every enabled model, so OpenAI clients are unaffected.

## 0.6.1 — 2026-11-10

### Fixed
- OpenCode Go is now a distinct provider, `opencode-go`, whose chat and Responses
  calls use `https://opencode.ai/inference/go/...`. Paid OpenCode keeps
  `https://opencode.ai/inference/...`; Go models no longer route through the paid base.
  The Go models endpoint is `https://opencode.ai/inference/go/v1/models`.

## 0.6.0 — 2026-11-10

### Added
- `POST /v1/search`: provider-native web search through existing connections. Claude and
  Anthropic use the Messages `web_search` server tool; OpenAI, Codex and xAI use the
  Responses `web_search` tool; Gemini uses `generateContent` with Google Search grounding.
  The request routes through the normal planner, account failover, key limits, input/output
  guardrails and usage accounting, and returns one cited answer in a 9Router-compatible
  shape.
- `GET /v1/models/search`: connected providers with a native search surface and their
  provider-only default model.
- Request fields: `model` (or `provider`), `query`, `max_results` (1–20, citation cap),
  `search_type` (`web` only), and `domain_filter` (native filters where supported, prompt
  hint for Gemini).
- xAI now declares its Responses endpoint for native `web_search`.

### Notes
- Search tokens are recorded and priced from the model registry; the provider's per-search
  tool fee is not priced by the gateway.
- A provider without native search is filtered from the plan; if none remains the route
  answers `501` without calling upstream.

## 0.5.5 — 2026-11-10

### Changed
- Dashboard navigation now has five surfaces — Activity, Playground, Routing, Access, System — with tabs for their pages. Account moved to the sidebar footer; Settings, Health, Webhooks, System logs and Audit are no longer permanent sidebar entries. Legacy URLs redirect to the new tab paths.

### Fixed
- Upgraded `opentelemetry_sdk` to 0.33.1 and `jsonwebtoken` to 10.4, clearing the two moderate Dependabot alerts on the release branch.

## 0.5.4 — 2026-11-10

### Changed
- OAuth sign-in no longer opens a browser automatically. PKCE dialogs always use the paste-back flow, with explicit **open** and **copy** actions for the authorize URL. This makes remote/cluster sign-in deterministic instead of depending on a loopback listener inside the pod.

### CI
- GitHub Actions now target Node 24 action majors and `ubuntu-24.04`, clearing the Node.js 20 and Ubuntu 26 migration annotations.

## 0.5.3 — 2026-11-10

### Fixed
- Remote OAuth for PKCE providers without a registered fixed port (Claude) now has a manual mode using a stable `localhost:54545` redirect. The dashboard advertises manual completion for every PKCE provider, so a gateway in a cluster or on a remote VPS can be signed in by pasting the callback URL.

## 0.5.2 — 2026-11-10

### Added
- Subscription-aware routing: cached provider plan windows (`claude`, `codex`, `github`)
  now feed the planner. `RROUTER_SUBSCRIPTION_RESERVE_PERCENT` (0–100, default 90) skips
  an account near a window cap while a healthier candidate remains; all-over plans stay
  serviceable and order the healthiest first. A background refresher keeps the cache warm
  at the provider usage module's polite cadence.
- `POST /api/route/explain`: the ordered plan a request would execute — providers,
  connections, priorities, dialects and cached subscription headroom — without dispatching.

### Fixed
- Updated `rusqlite`/`libsqlite3-sys` to 0.41.1/0.39.1, clearing RUSTSEC-2026-0459 in the bundled SQLite library.
- Budgeted keys and teams now serialize admission through usage commit, so concurrent
  requests cannot all pass the preflight check and overspend. Unbudgeted traffic stays
  concurrent.
- OAuth completion is bound to the provider that started the pending login; an id from
  one provider cannot be redeemed through another provider's route.
- Existing `master.key`, `jwt.key` and `setup-code.txt` files are tightened to `0600` on
  load instead of trusting the mode they were created with.
- Connection deletion removes quota, quota-ping and latency rows in one transaction, and
  historical usage/attempt rows remain for audit.
- Streamed output guardrails match blocked terms split across SSE deltas; terminal
  stream failures are recorded as failures instead of `200`; invalid streamed tool JSON
  fails translation instead of becoming `null`.
- Model-scoped 429s no longer clear an unrelated connection-wide failure streak.
- `/api/system` reports storage failures instead of returning zeroed counters.
- Dashboard: horizontal table overflow, destructive-dialog errors, retry buttons inside
  forms, preset-default errors, duplicate export errors, provider toggle method, and
  activity error-lane accessibility.
- Helm and raw Kubernetes defaults keep remote admin disabled; enabling it requires an
  admin token or authenticating Ingress.

## 0.5.1 — 2026-11-09

### Changed
- API Keys is a compact four-column list (Key, Limits, Spend, Actions): identity and status are together, limits wrap, spend is secondary, and edit/delete are horizontal sticky actions instead of a tall clipped column.
- Key settings can rename a key. `PATCH /api/keys/{id}` accepts `name` (trimmed, null/blank clears, 100 characters), and the edit modal exposes it alongside limits and guardrails.

## 0.5.0 — 2026-11-09

### Added
- Smart quota auto-ping for Claude and Codex OAuth accounts. Per-account modes: off, on reset, fixed schedule (anchor/timezone/window/slots), and smart.
- Smart mode records subscription usage samples and staggers reset pings across accounts by recent traffic, so one fresh window remains available while another account is near its cap; idle accounts are skipped.
- Admin API `GET/PUT /api/quota-pings` with next-ping previews and validation.
- Providers UI auto-ping badge, settings drawer, schedule preview and smart fleet explanation.
- Schema v34 adds `quota_pings` and `quota_ping_samples`.

### Fixed
- Codex auto-ping sends the account header and Responses-shaped input used by the CLI.

## 0.4.6 — 2026-11-09

### Fixed
- Docker publishing lowercases the GHCR repository path. GitHub keeps the repository name as `centopw/RRouter`, but registry references must be `centopw/rrouter`; both the image matrix and the manifest merge now derive `IMAGE_NAME` with `${GITHUB_REPOSITORY,,}`.

## 0.4.5 — 2026-11-09

### Added
- Evals context and judging: a suite-level system prompt is prepended to every case, and runs can name an optional judge model that scores `expected` as a semantic criterion. Results expose the criterion, model output and errors; schema v32 adds `eval_suites.system_prompt` and `eval_runs.judge_model`.
- Per-model preferred provider: the Models coverage popover can pin a provider for a bare model id; `/v1/models` advertises it as `owned_by`, bare requests route there first, explicit `provider/model` still wins, and schema v33 adds `model_preferences`.
- Multi-provider model UX: coverage counts and provider tags, a shared-only filter, and peer details with price/state.
- Provider model auto-fetch now waits up to 8s on connection create, OAuth completion and credential/base-URL edits, then detaches; the dashboard performs one follow-up registry read.

### Changed
- Models separates enabled rows from a dedicated Disabled models table; hidden-from-preset rows stay separate.
- `/v1/models` omits disabled models; re-enabling restores them.
- 429 breaker cooldowns are scoped to `(connection, model)`, so a free-tier 429 no longer blocks other models on the same connection. Exhausted plans return 429 `all_candidates_cooling_down` with `Retry-After`, not 404.
- Evals page redesigned around suite context, run settings, cases and expandable results.
- Every `DataTable` paginates itself when no controlled pagination is supplied (25/50/100/200).
- API Keys, Providers and Models tables fit their cards; Logs keeps the four filter selects on one row at desktop widths.
- Provider search in the API-key drawer matches names, ids and aliases.

### Fixed
- `useToast` no longer crashes a route when its provider context is unavailable.
- `GET /v1/models` respects disabled models and preferred-provider ownership.
- 429s from OpenCode free models no longer poison unrelated models.
- API Keys table no longer overflows its card at 1440/1024/390.

## 0.4.4 — 2026-11-09

### Added
- Combo strategies: ordered fallback, durable-cursor round robin, and smart routing through a configured decision model. Smart failures fall back to the first candidate; decision calls are bounded and never recurse into the same combo.
- Schema v31 adds `combos.strategy`, `combos.router_model` and `combos.cursor`; existing rows remain fallback.
- Combos UI strategy cards, smart router picker, validation and strategy-aware table.
- Combo acceptance harness and docs.

## 0.4.3 — 2026-11-09

### Added
- Google Antigravity / Cloud Code Assist provider: Google OAuth + PKCE, project discovery/onboarding, `project_id` credential metadata, email label, CCA envelope and SSE translation, provider model discovery.
- Remote OAuth deployments: manual start without a loopback listener and paste-the-callback completion; branded loopback success/failure pages.
- Webhook/audit, session device metadata and model preset follow-ups from the 0.4.2 line.

### Changed
- Provider connection table no longer overflows its card; names wrap, auth is one badge with a tooltip, and row actions live in a keyboard menu.
- Shell/sidebar/content scroll ownership and provider marks/shell layout fixes; number-input spinners removed; quiet buttons outlined.

## 0.4.2 — 2026-11-09

### Added
- Model presets: named layouts with drag-and-drop ordering, hidden/restore lists, archived restore positions, auto-include policy, revision-guarded saves, 409 conflict recovery, and per-account active preset selection.
- GitHub update checks now use an optional read-only token, persistent conditional metadata, rate-limit reset backoff, and a release-redirect fallback so routine checks do not consume the unauthenticated API budget.

### Changed
- Release pipeline now builds Docker architectures natively instead of QEMU, merges per-arch manifests, and creates one changelog-derived release body instead of four generated compare blocks.
- CI runs actionlint for workflow files.

## 0.4.1 — 2026-11-09

### Documentation and media
- Added the 10-second product demo (MP4/WebM/GIF), dashboard screenshots for desktop and mobile, and a Mermaid architecture diagram with a numbered runtime flow.
- README now leads with the demo and screenshots; install and roadmap references point at the new release.
- This release carries the same gateway features as 0.4.0; no runtime behavior changed.

## 0.4.0 — 2026-11-09

### Added
- Webhooks: an admin-only endpoint registry (`/api/webhooks`) and a leased SQLite outbox
  that delivers HMAC-SHA256-signed events (key/team/connection/audit) from a background
  worker with backoff, dead-lettering and a terminal `unmatched` state for events with no
  subscriber. The dashboard page shows subscription, secret readiness and queue status.
- W3C trace context: `traceparent`/`tracestate` are accepted on every `/v1` entry, `x-trace-id`
  is echoed on every response next to `x-request-id`, the access line carries `trace_id`,
  every attempt logs a child span, and `usage.trace_id` joins a request's rows. Every
  outbound attempt forwards a child `traceparent` (and the accepted `tracestate`) with the
  same span id it logs — retries and hedged legs get distinct children — and rejected
  inbound contexts are counted in `rrouter_trace_context_rejected_total{reason}`.
- Teams own provider accounts (`connections.team_id`): a team-scoped key is routed only to
  its team's accounts, and the upgrade note lives in `docs/DEPLOY.md` §5.1.
- Evals: suites and cases executed through the real `/v1` path, with results and summaries
  (`/api/evals/*` and the Evals page).
- Per-key PII redaction (email/phone/card/credential shapes) for prompts, non-streaming
  responses and streamed text, off by default, counted per stage and documented in
  `docs/GUARDRAILS.md`. Streams rewrite text/reasoning deltas through a 512-byte carry
  (bounded visible lag; a stream ending without its terminal frame still flushes); tool-call
  arguments and opaque payloads are deliberately left byte-identical.
- Semantic response cache: `RROUTER_SEMANTIC_CACHE=sqlite` embeds a canonical prompt with
  an operator-supplied `RROUTER_SEMANTIC_EMBEDDING_MODEL` and serves a stored response when
  cosine similarity reaches `RROUTER_SEMANTIC_CACHE_THRESHOLD`, within the scope, TTL and
  row bounds. Non-streaming chat/messages/responses only.
- Operator canary and non-streaming hedging: `RROUTER_CANARY=model=provider:percent`
  prefers a provider for a share of request ids inside its priority tier (reloadable), and
  `RROUTER_HEDGE_MS` starts the next same-priority candidate in parallel after the delay,
  first success wins, loser cancelled. Streams are never hedged.
- OpenAI-compatible files and batches: `/v1/files` and `/v1/batches` are forwarded
  byte-identically to providers that declare the capability (multipart boundary and batch
  JSON preserved), never executed or stored here; a provider without it answers `501`.
- Optional OTLP/HTTP span export behind the `otel` cargo feature (default off, so default
  builds link no OpenTelemetry crates): `RROUTER_OTEL_ENDPOINT` (env-only) enables
  request/attempt spans with parent-based sampling, and shutdown flushes with a 5 s bound.
  Only ids, provider/model, status and latency are exported.
- Session device metadata (schema v28): every sign-in path records the resolved client IP,
  raw User-Agent (bounded to 512 chars) and a derived `device_label`, returned for the
  signed-in user's own sessions.
- Token Saver (RTK): inbound tool results are compressed before routing — grouped greps,
  capped git status/diff/log, collapsed build logs and repeated lines, head/tail truncation
  with markers. On by default, toggle in Settings, bypass with `x-rrouter-rtk: off`; savings
  are logged and exported as `rrouter_rtk_saved_bytes_total`.
- Production identity tooling: persisted audit trail with an Audit page, self-service
  account page (rename, password change with session rotation, per-device revoke and device
  labels from the session metadata), recovery
  throttling, `/ready`, and `rrouter admin backup <file>` (consistent `VACUUM INTO`). CI runs
  clippy, tests and the dashboard build.
- Model lists are newest-first (version- and date-aware) and retired families (Claude 3.x,
  GPT-4 turbo, gpt-5-codex) sort last, carry a `deprecated` badge, and are never chosen by a
  connectivity probe. Probes prefer the cheapest current model, using the provider's own list
  or a synced price, so testing an account no longer spends `claude-3-7-sonnet`. Claude's
  list fetch asks for one maximum-size page, so newly released models appear without cursor
  paging.
- Subscription usage: `/api/accounts/usage` reads the plan windows the provider already
  reports (Claude's 5-hour and weekly buckets, Codex's primary/secondary windows), cached for
  a minute, and the Providers page shows them as read-only bars next to the optional operator
  caps. GitHub Copilot's monthly request snapshots are read too. The provider stays the
  source of truth; RRouter never invents a percentage.
- Dashboard accounts: first-run owner creation with the one-time setup code printed at startup,
  argon2id passwords, HttpOnly session cookies, ten single-use recovery codes, invite-only
  sign-up, and the admin/member/viewer roles (`/api/auth/*`, `/api/users`, `/api/users/invites`).
  The owner cannot be demoted, disabled or deleted; role and CSRF refusals name their reason.
  Automation keeps working: `RROUTER_ADMIN_TOKEN` and admin-scoped keys are unchanged, and
  `rrouter admin show-setup-code` / `rrouter admin reset-password <email>` run locally.
- A light, flat dashboard system: near-black navigation rail, off-white canvas, white surfaces,
  a single muted navy accent, pill buttons, 4px inputs, 8px cards, hairline borders instead of
  shadows, Phosphor icons, and 75/400/500ms ease-out motion that respects reduced motion.
- Guardrails per key: prompt-size cap, case-insensitive blocked phrases, and response scanning
  (streams are cut with one error frame). Blocks return 403 and are counted per stage.
- `RROUTER_STRATEGY=cheapest` orders equal-priority accounts by the model's catalog price.
- Retries before failover: `RROUTER_RETRY_ATTEMPTS`, `RROUTER_RETRY_BASE_MS`,
  `RROUTER_RETRY_MAX_MS` drive exponential backoff with jitter, capped by `Retry-After`, the
  headers deadline and the send budget. Streaming attempts retry only before the first byte.
- Prompt templates with version history (`/api/templates`), and a Templates page plus Playground
  insert / save-as-template.
- Key scopes: an admin key manages the gateway through `/api` and `/metrics` without loopback or
  a shared admin token, and is refused on `/v1`; inference keys keep limits, budgets and
  guardrails. The key editor sets the scope and the list badges admin keys.
- Account quotas: each upstream account can cap requests, tokens and spend per UTC day, week or
  month (`/api/accounts/quotas`); the router skips an exhausted account, returns
  `429 quota_exhausted` with `Retry-After` when every candidate is spent, and the Providers page
  shows each account's window usage with a quota editor. Usage is aggregated from the request
  log, so nothing counts, resets or repairs a counter.

### Added
- Logs now include failed requests: every terminal failure writes one zero-token usage row
  with its HTTP status and client-facing error, and the request drawer lists each attempt's
  upstream error. A new **System logs** page captures the gateway's own tracing events in
  memory for debugging; it is off by default, never written to disk, and can be enabled or
  cleared at runtime.

### Added
- Activity has a **Live routing** map: the gateway in the centre, one node per provider
  with an active connection, request/error counts for the last 15 minutes, and an animated
  edge while a provider served traffic in the last 90 seconds. It refreshes every 5 seconds.
- Request detail now records and draws the latency phases — connect + upstream headers,
  model → first token, generation — instead of only inferring them from TTFT.

### Changed
- Interactive primitives are fully Radix-backed: Dialog, AlertDialog, Tabs, Switch,
  Checkbox, Popover, Toast, Accordion, Tooltip-group/Toggle, Separator and Label. The
  searchable Select keeps its filter UX on a Radix Popover; no hand-rolled dismiss/focus
  logic remains.

### Security
- Dashboard sessions are HS256 JWTs signed with a key stored beside the data volume.
  The database still validates every session, so logout and password changes revoke a
  mathematically valid token. A new `/api/auth/refresh` rotates the session id and the
  client retries once on 401, invalidating the previous token. Sessions created without
  “keep me signed in” are pruned on process start, so a redeploy ends them; remembered
  sessions survive.

### Added
- Community providers: the Add-provider → API key screen links to API and OAuth issue
  forms. The API form is parsed into `providers/community/<id>.json`, compiled into the
  next binary by `crates/rrouter-auth/build.rs`, and opened as a reviewed pull request by
  the `provider-request` workflow. OAuth providers are reviewed manually with their flow.

### Added
- A semantic design system: one type scale in `index.css`, one component per file under
  `components/atoms` and `components/molecules`, and Radix-backed Dialog, Tabs, Switch,
  Separator and Label primitives. Shared molecules use Text/Heading/Stack/Inline; raw font
  sizes were removed from the dashboard so typography is changed in one place.

### Added
- The Add-provider → API key screen now carries the two custom endpoints, tinted with the
  OpenAI and Anthropic brand colours. Every provider row shows its real mark from
  simple-icons where available, with the provider's favicon as the fallback.
- OTA updates: **Settings → Software updates** checks GitHub daily, shows release notes and
  the detected deployment, optionally downloads and verifies the release archive, and offers
  a confirmed restart. Binary/systemd installs replace and exec in place; Homebrew runs
  `brew upgrade`. Docker, Kubernetes and Helm report their supervisor's exact upgrade
  command because a container or pod cannot replace its own image or release.
- OpenCode is a native provider (`https://opencode.ai/inference/openai/v1`), with its shared
  model catalogue and an add-provider guide for the Free, pay-as-you-go, Go/Go Plus/Go Teams
  plans, including direct API-key links.
- The sidebar has two custom-endpoint shortcuts (OpenAI-compatible and Anthropic) that open
  the key form with the protocol fixed and a required base URL.

### Fixed
- Choosing a provider in the API-key flow now carries that selection into the form instead of
  always opening with the first registered provider.
- Activity model rows select on a click anywhere, not only on the model name.

### Changed
- In the collapsed sidebar the logo itself is the expand control: hovering swaps it for the
  expand icon, with no second button competing for the rail's width. Activity request rows
  are clickable across every cell, the inspector is a right-hand drawer, and the tokens
  column no longer wraps its header.
- Rate-limited accounts fail over immediately: a 429 is no longer retried on the same
  sub-account before trying the next one, and the breaker's 429 cooldown now starts at 30 s
  and escalates to one hour instead of expiring after four minutes. Providers shows an
  explicit **quota exhausted** badge when a subscription window reports 100%.

### Fixed
- Codex rejects `temperature` and `top_p` instead of ignoring them, failing every
  Playground turn; the gateway strips those sampling controls for the codex provider on
  both the chat→Responses translation and same-dialect Responses passthrough.
- Sign-in reads the submitted DOM values instead of React state, so browser autofill and
  password managers (which can fill fields without firing `input`) work on the first submit.
- Dialogs are right-hand drawers; test results and other transient outcomes use a toast
  stack. The Logs row is clickable everywhere with a clock time + relative age. Models can be
  edited per row (display, context, output, price, capabilities), metadata sync/fetch moved to
  Settings with an explanation of each, and the API key is configured in a creation drawer and
  revealed there once. Aliases and combos gained edit flows. Chart tooltips appear only on
  hover, and the supplied brand logo replaces the interim mark.
- The shell is light throughout: white rail, collapsible to icons, light-mode logo, and every
  dialog rebuilt as a flat card with a header, scrollable body and sticky footer. Destructive
  actions (provider, key, user, invitation, template, alias, combo, custom model, quota clear)
  confirm in a dialog instead of `window.confirm`, and every dropdown is the dashboard's own
  searchable, keyboard-navigable list.
- Logs filters sit in one toolbar (range, status, provider, model, key) with a clear action,
  instead of wrapping into a stack, and the Playground reads as a conversation: centered thread,
  suggestion starters, and a composer with the model and send control inline.
- The dashboard now signs you in as a user. The pasted admin token moved behind "Use an admin
  token instead" on the sign-in screen, and loopback/break-glass access is unchanged for the API.
- The request detail opens as a dialog when a row is clicked (Activity and Logs) instead of
  occupying a column of the page; nothing is selected until you click, and Escape or the
  backdrop closes it.

## 0.2.3

### Security
- Every response carries `x-content-type-options: nosniff`, `x-frame-options: DENY`,
  `referrer-policy: no-referrer` and a restrictive `permissions-policy`; HTML documents add a
  same-origin CSP (https images and operator-configured extension-panel frames allowed,
  scripts and fetches same-origin). Insert-only: headers already set upstream win.

## 0.2.2

### Added
- CSV export of the filtered request log (`/api/usage/export.csv`) and an Export CSV action on
  the Logs page; rows stream in 1000-row cursor pages.
- Usage retention: `RROUTER_USAGE_RETENTION_DAYS` deletes old usage/attempts in a daily sweep
  (expired cache rows always go) and runs `PRAGMA optimize`; schema v13 indexes `attempts(ts)`.

## 0.2.1

### Added
- Server-side filters on the request log (`provider`, `model` ref or bare id, `key_id`,
  `status` 2xx/error) that compose with the cursor.
- Logs page: filterable, cursor-paginated request history with the same attempts detail as
  Activity.
- Structured logs: `RROUTER_LOG_FORMAT=json` and one access line per finished request
  (request id, provider, model, status, latency, first-output time, endpoint, key id).
- Cursor pagination for the request log (`/api/usage` `before`/`before_id` and
  `next_before`), so history can be paged without time-window guessing.
- Budget windows: per-key budgets reset daily, weekly or monthly (UTC), enforced alongside the
  lifetime cap; `spent_*` on the key row is scoped to the current window.
- Opt-in exact-match response cache for non-streaming requests (`RROUTER_CACHE=sqlite`), with
  TTL, size bound, `x-rrouter-cache` hit/miss/bypass and hit rows that record zero tokens.
- The dashboard prompts for `RROUTER_ADMIN_TOKEN` and keeps it per tab, so a proxy-fronted
  deployment can authenticate its own admin API.

## 0.2.0 — first public release

### Gateway

- One binary serving OpenAI Chat, OpenAI Responses and Anthropic Messages on `/v1`, with
  incremental translation in both directions (streaming and non-streaming).
- `/v1/embeddings`, `/v1/models` (list and by id), and forwarded
  `/v1/messages/count_tokens`, `/v1/responses/compact`, `/v1/images/generations`,
  `/v1/audio/speech`, `/v1/audio/transcriptions` and `/v1/audio/voices` for providers that
  implement them (501 otherwise, never an estimate).
- Subscription sign-in for Claude and Codex, GitHub Copilot device flow, API keys for OpenAI,
  Anthropic, Gemini, DeepSeek, Groq, OpenRouter, xAI, Mistral, Together, Cerebras, Ollama.
- Credential refresh ahead of expiry with a per-account lock; encrypted at rest (AES-256-GCM).

### Routing

- `provider/model`, aliases (one hop), and combos; priority-ordered fallback with a circuit
  breaker, `Retry-After` handling, and half-open probes.
- Conversation affinity: equal-priority accounts are pinned per conversation so prompt caches
  stay warm; unrelated conversations still spread.
- Same-dialect requests are forwarded as the client wrote them — Anthropic `cache_control` and
  OpenAI cache fields survive; only routing fields are patched.
- Explicit `provider/model` never widens to other providers, even for ids the catalog does not
  know.

### Keys and limits

- Per-key RPM, TPM, token budget, model allowlist (exact refs and `provider/*`), expiry, and
  per-key spend from recorded usage.
- Hashed at rest, plaintext shown once; enforcement returns `403`/`429` with stable codes in
  the caller's dialect.

### Model registry

- Merged catalog + provider `/models` + models.dev metadata + custom rows; capabilities, limits,
  pricing; a real 16-token test through the same planner and dispatcher a client would use.
- Only connected providers are listed; models.dev enriches, never adds.

### Dashboard

- Activity redesign: readouts with window deltas, stacked traffic chart with error lane and
  tooltip, ranked models, request log with attempt timeline, tokens/latency/first-output split.
- Setup wizard for the first run: connect an account, create a key, send and watch the first
  request.
- Playground for streaming Chat/Messages; Settings for branding and runtime facts; white-label
  branding applied live.
- Requests can be filtered by model, errors, and key; every number is server-side.

### Operations

- `usage`/`attempts` accounting, Prometheus `/metrics`, `/health` and a self-probe
  (`rrouter --healthcheck`), graceful shutdown with a bounded drain.
- Request ids (`x-request-id`) and per-attempt records, so a fallback is visible in
  `GET /api/usage/{id}` and in Activity instead of inferred.
- `RROUTER_ADMIN_TOKEN` for the admin API, `RROUTER_MAX_CONCURRENCY` backpressure (503 +
  Retry-After), `RROUTER_MAX_ATTEMPTS`, `RROUTER_SHUTDOWN_GRACE_SECS`.
- Docker (distroless, nonroot, healthcheck), compose, single-replica Kubernetes manifest,
  systemd unit, Caddy example with admin auth.
- `scripts/backup.sh` (consistent SQLite snapshot), `scripts/load_test.py`, `scripts/verify.sh`,
  live e2e and OAuth smokes.

### Fixed

- Claude OAuth: the registered callback host (`localhost`, not `127.0.0.1`), `state` in the
  token exchange, and an immediate loopback answer instead of a hung page.
- OAuth requests now claim a current Claude Code version, which newer models require.
- Cache markers were dropped when a request was rebuilt; same-dialect bodies are now forwarded.
- Activity counted requests one bucket early; windows now end in the bucket containing "now".
- Streamed responses recorded `streamed=false`; the flag and time-to-first-output are recorded.
- The shutdown grace started at process boot instead of on the signal, so every gateway
  exited after 20 s; the timer now starts with SIGTERM/SIGINT and exits in milliseconds when
  nothing is streaming.

### Known limitations

- Codex's subscription backend does not serve `/responses/compact` (OpenAI API keys do).
- OAuth sign-in needs the browser to reach the loopback callback; remote hosts need an SSH
  port-forward (documented) — there is no paste-the-URL fallback yet.
- Limits are in tokens; USD budgets track the catalog price in a later release.
