# Free mode

Free mode finds free endpoints and models automatically and uses them, so a user keeps a
working model when their paid tokens run out — or when they just want a free one. Nothing
has to be imported or configured: on startup RRouter fetches the public OpenCode and
OpenRouter catalogs, keeps every zero-priced chat model that speaks a dialect it can route,
and exposes the reserved ref `free` across that pool.

The default is **off** and **opt-in**: flip the pool on, flip automatic fallback on, done.
An optional JSON document (the *Advanced* path) remains for operators who need named
free-tier accounts, per-account rate limits and egress proxies.

## Quick setup

1. `GET /api/free-mode` — the discovered pool is already populated (OpenCode zero-price chat
   models; OpenRouter `:free` models).
2. `PATCH /api/free-mode {"enabled": true}` — turns the pool on and probes the working
   models so the UI shows a live result.
3. `PATCH /api/free-mode {"fallback_on_quota": true}` (optional) — routes a paid request to
   `free` when the paid route fails with quota exhaustion, upstream `429` or `402`.

Then request `free` (the whole working pool) or `free/<model>` (one pinned model, e.g.
`free/big-pickle`):

```json
POST /v1/chat/completions
{ "model": "free", "messages": [{ "role": "user", "content": "hi" }] }
```

`free` never widens to a non-free account, and an anonymous keyless `Bearer public` send is
used only for verified public-catalog free models on OpenCode — never for a paid model.

## Discovery

Two public catalogs are fetched (keyless, no account) on startup and every 30 minutes, and
on demand via `?refresh=true`:

- **OpenCode Zen** — `https://opencode.ai/inference/v1/models`. Free rule:
  `cost.text.input.price == 0 && cost.text.output.price == 0`. A model is usable only when
  it declares a `chat-completions` API; a model that speaks another dialect (for example
  `jev-1.13-free`, `systemone`) is recorded but marked `usable: false` and excluded from
  routing, with the reason surfaced in the UI.
- **OpenRouter** — `https://openrouter.ai/api/v1/models`. Free rule: `id` ends in `:free`,
  or `pricing.prompt == "0" && pricing.completion == "0"`. These require an account
  (`auth_mode: key_required`), so they stay visible with an "add a key" link until one is
  connected on Providers.

For a model to be served *free*, it must be verified free by one of those rules. A normal
account on the provider is eligible for a verified-free model, but never for a paid one; a
paid model can never ride the `free` pool.

## Automatic fallback

Opt-in (`fallback_on_quota`, default `false`). Fires only when **all** hold:

1. the free pool is on *and* fallback is on,
2. the request did not already name a `free` ref and did not set
   `provider.allow_fallbacks: false`,
3. the paid route failed with a budget/quota limit (`quota`), an upstream `429` or
   `all_candidates_cooling_down` (`rate_limit`), or an upstream `402` (`payment_required`).

On fallback the response carries `x-rrouter-fallback: free`,
`x-rrouter-fallback-reason: <quota|rate_limit|payment_required>` and
`x-rrouter-fallback-model: <provider>/<model>`; the usage row records the same reason with
`cost_micros = 0`. If the free pool also fails, the **original paid error** is returned —
fallback never hides the real failure behind a secondary one.

OpenCode's account/IP-wide `FreeUsageLimitError` (`429`) cools the whole provider (honoring
the HTTP `Retry-After`, capped at one hour) instead of burning one attempt per free model,
so a single exhausted bucket fails over to another provider rather than fanning out.
OpenRouter's account-wide `:free` platform `429` is handled the same way, and its real
reason is unwrapped from `metadata.raw` instead of the generic "Provider returned error".

## Advanced: the JSON document

The document path — named free-tier providers, accounts and per-account egress proxies — is
unchanged, and lives behind *Advanced* in the UI. Everything below describes it in full.

```json
{
  "version": 1,
  "prune": false,
  "routing": {
    "enabled": true,
    "per_account_rpm": 10,
    "max_attempts": 8,
    "prefer": "spread",
    "models": ["deepseek-v4.1-flash", "qwen3-coder:free"]
  },
  "proxies": {
    "p1": "http://user:pass@10.0.0.1:8080",
    "p2": "socks5h://10.0.0.2:1080"
  },
  "providers": [
    {
      "provider": "openrouter",
      "models": ["deepseek-r1:free"],
      "accounts": [
        {
          "label": "acc-1",
          "api_key": "sk-or-...",
          "proxy": "p1",
          "rpm": 5,
          "priority": 0,
          "weight": 100
        },
        {
          "label": "acc-2",
          "access_token": "ya29...",
          "refresh_token": "...",
          "proxy": "https://127.0.0.1:1080",
          "rpm": 5
        }
      ]
    },
    {
      "provider": "my-free-host",
      "custom": {
        "id": "my-free-host",
        "display": "My Free Host",
        "base_url": "https://api.example.com/v1",
        "dialect": "chat"
      },
      "models": ["deepseek-v4.1-flash"],
      "accounts": [
        { "label": "host-1", "api_key": "sk-...", "proxy": "p2" }
      ]
    }
  ]
}
```

| Field | Required | Meaning |
| --- | --- | --- |
| `version` | no (default `1`) | Document schema version. This build accepts `1` only. |
| `prune` | no (default `false`) | Disable free-mode accounts of a listed provider that the document no longer names. Never deletes a connection. |
| `routing.enabled` | no (default `true`) | Stored for the router; `false` parks the free rotation without forgetting the document. |
| `routing.per_account_rpm` | no | Default proactive per-account cap (requests/minute). An account's own `rpm` wins; both absent means no local cap. |
| `routing.max_attempts` | no (default `8`) | Cap on free-mode candidates in one plan. |
| `routing.prefer` | no (default `spread`) | `spread` or `cheapest`. |
| `routing.models` | no | Narrows the bare `free` ref to these model ids. Absent or empty means every discovered free model. |
| `proxies` | no | Named proxy URLs, so several accounts can share one without repeating it. |
| `providers[].provider` | yes | Provider ref: an id (`openrouter`) or a shorthand code (`or`), resolved exactly like a model ref. |
| `providers[].custom` | when the provider is not compiled | `{id, display, base_url, dialect}`. Created or updated through the same path as `POST /api/models/providers`. |
| `providers[].models` | no | Models to seed as free rows for this provider (see [seed models](#seed-models)). |
| `providers[].accounts[]` | yes | At least one account. |
| `accounts[].label` | yes | Account identity. Written to `connections.name` and `credentials.account_label`. |
| `accounts[].api_key` | one of the two | Static key. |
| `accounts[].access_token` / `refresh_token` | one of the two | OAuth credentials; `refresh_token` is optional when the access token does not expire. |
| `accounts[].proxy` | no | A key of `proxies`, or a full `scheme://[user:pass@]host[:port]` URL. |
| `accounts[].rpm` | no | Per-account cap; falls back to `routing.per_account_rpm`. |
| `accounts[].priority` / `weight` | no | The usual routing columns (`priority` orders candidates, `weight` shares traffic inside a priority group). |

Unknown fields are rejected, at every level: `deny_unknown_fields` means a typo such as
`"proxys"` is a validation error rather than a silently ignored key.

### Provider resolution

- Without `custom`, `provider` must resolve to a provider this build knows (compiled, or a
  custom provider imported earlier). A typo is a validation error, never a new provider.
- With `custom`, `custom.id` is the id the accounts and model refs use. It must not collide
  with a compiled provider. `provider` may be the same id (the normal first import of a new
  host) or a compiled provider whose protocol family the operator used as a reference
  (`"provider": "openrouter"` with `"custom": {"id": "my-free-host", …}` creates
  `my-free-host` and records that it speaks the OpenAI Chat shape).
- `custom.dialect` is `chat`/`openai` or `anthropic`/`messages`; it selects the transport a
  custom provider borrows. `custom.base_url` is an absolute `http(s)` URL with no query and
  no fragment (a trailing slash is dropped).

## Validation

Validation is total and pre-flight. Every problem is collected into one answer, and
**nothing is written** unless the whole document validates — a half-applied import would
leave accounts that route through a proxy the operator never meant to use.

```
400
{
  "error": {
    "message": "the free-mode document has 3 problem(s)",
    "details": [
      "providers[0].provider: unknown provider openroutr; add a custom block to declare it",
      "providers[1].accounts[1].proxy: unknown proxy key p9",
      "providers[1].accounts[2].label: a label is required"
    ]
  }
}
```

Every detail names the JSON path of the problem. The rules, in full:

| Rule | Detail |
| --- | --- |
| `version` must be `1` | `version: this build understands version 1, not 2` |
| At least one provider, at most 64 | `providers: at least one provider is required` |
| A provider id appears once per document | `providers[1].provider: openrouter appears twice` |
| A provider resolves, or declares `custom` | `providers[0].provider: unknown provider …` |
| `custom.id` must not collide with a compiled provider | `custom.id openai collides with a compiled provider; pick another id` |
| `custom.id` is 1–64 chars of `a-z0-9-_.` | `custom.id "my host" must be 1-64 characters …` |
| `custom.base_url` is absolute `http(s)`, no query/fragment | `custom.base_url: must be an absolute http(s) URL` |
| `custom.dialect` is chat/openai or anthropic/messages | `custom.dialect: "grpc" is not "chat", "openai" or "anthropic"` |
| Every account needs a label, unique on its provider, ≤128 chars | `…label: acc-1 appears twice on this provider` |
| Every account needs `api_key` or `access_token` | `…: one of api_key or access_token is required` |
| `rpm` is 1–100000 when present | `…rpm: must be between 1 and 100000` |
| `weight` is 1–1000 when present | `…weight: must be between 1 and 1000` |
| `proxy` is a known key or a valid URL | `…proxy: unknown proxy key p9` / `…proxy: http://… is not a scheme://[user:pass@]host[:port] URL …` |
| Proxy scheme is `http`, `https`, `socks5` or `socks5h` | same detail |
| `routing.prefer` is `spread` or `cheapest` | `routing.prefer: "fastest" is not "spread" or "cheapest"` |
| `routing.max_attempts` is 1–64 | `routing.max_attempts: must be between 1 and 64` |
| `routing.per_account_rpm` is 1–100000 | `routing.per_account_rpm: must be between 1 and 100000` |
| `routing.models` has at most 200 entries | `routing.models: at most 200 entries` |
| Each `routing.models` entry is a non-blank id, ≤200 chars, no whitespace | `routing.models[1]: a model id is required` |
| Each `routing.models` entry appears once | `routing.models[1]: deepseek-v4.1-flash appears twice; each model id may appear once` |
| At most 64 accounts and 500 seed models per provider | `providers[0].accounts: at most 64 accounts per provider` |
| A malformed document (wrong type, unknown field) | one detail naming the serde error |

A proxy URL is never echoed unmasked, even inside an error: the detail carries
`http://***@host:port`.

## Applying an import

Order of operations, all of it after validation:

1. A `custom` provider is created (or its display, dialect and base URL updated) first, so
   the accounts that dispatch to it exist after it does.
2. Each account is matched by `(provider, label)` — `credentials.account_label` or
   `connections.name`. A match updates the key, proxy, rpm and the priority/weight the
   document states; no match creates a connection with `free_mode = true` and
   `is_active = true`. Re-importing the same document therefore updates instead of
   duplicating, and a changed proxy replaces the old one (dropping `proxy` and `rpm` from
   the document clears them).
3. `prune: true` disables free-mode accounts of each listed provider that the document no
   longer names. It never deletes a connection or a credential.
4. Declared `providers[].models` are upserted into `free_models` with `free = true` and
   `source = "document"` (see [seed models](#seed-models)).
5. The redacted document and the resolved routing knobs are stored last, so the router
   keeps reading the last document that fully applied.

Each step is idempotent: a retry after a store failure converges on the same state.

### Seed models

`providers[].models` is a seed list, not a discovery result: every entry is written as a
free row at import time with `source = "document"`, so a model the provider does not list in
its own `/v1/models` — or one no price registry knows — is still routable through the pool
immediately. A later discovery upgrades the row when the provider's list advertises it
(`provider-list`) or the registry prices it at zero (`registry`); a list that merely omits
the marker never downgrades a seeded row.

## Endpoints

All of these sit on the admin API and are gated exactly like the rest of `/api/*`. The
zero-config surface is `GET`/`PATCH /api/free-mode`, `PATCH /api/free-mode/models` and
`POST /api/free-mode/probe`; the document path (`import`/`discover`/`DELETE`) is unchanged.

### `GET /api/free-mode[?refresh=true]`

Returns the complete state in one call. Fetches the public catalogs first when they have not
been fetched yet (or `?refresh=true`), degrading to cached rows plus `catalogs[].error` on a
network failure.

```json
{
  "enabled": false,
  "fallback_on_quota": false,
  "fallback_triggers": ["quota", "429", "402"],
  "ref": "free",
  "last_checked_at": 1763890000,
  "discovered_at": 1763890000,
  "summary": { "total": 26, "usable": 25, "enabled": 25, "working": 7, "unusable": 1 },
  "next_fallback_model": "big-pickle",
  "models": [
    {
      "provider": "opencode",
      "provider_display": "OpenCode",
      "model": "big-pickle",
      "display": "Big Pickle",
      "ref": "free/big-pickle",
      "endpoint": "https://opencode.ai/inference/openai/v1/chat/completions",
      "dialect": "openai_chat",
      "free": true,
      "enabled": true,
      "usable": true,
      "unusable_reason": null,
      "auth_mode": "key_or_anonymous",
      "has_account": false,
      "needs_key": false,
      "price": 0.0,
      "source": "public-catalog",
      "discovered_at": 1763890000,
      "probe": { "ok": true, "status": 200, "latency_ms": 820, "message": null, "needs_key": false, "at": 1763890000 }
    }
  ],
  "catalogs": [
    { "provider": "opencode", "url": "https://opencode.ai/inference/v1/models", "discovered_at": 1763890000, "models": 11, "usable": 10, "error": null }
  ],
  "document": null,
  "routing": { "enabled": false, "fallback_on_quota": false, "per_account_rpm": null, "max_attempts": 8, "prefer": "spread", "disabled_models": [] },
  "updated_at": null,
  "accounts": []
}
```

- `probe` is `null` until the model is first probed; otherwise
  `{ ok, status, latency_ms, message, needs_key, at }`.
- `auth_mode` is `key_or_anonymous` (OpenCode — an account if connected, else anonymous
  `Bearer public`), `key_required` (OpenRouter — needs a connected account), or `anonymous`.
- `needs_key` is `true` when the model needs an account the operator has not connected
  (`key_required` with no account, or the last probe hit `FreeUsageLimitError`/`401`).
- `next_fallback_model` is the model the free pool would pick right now (working-first),
  or `null`.
- `enabled` defaults to `false` on a fresh instance; importing an Advanced document with
  `routing.enabled: true` turns it on.

### `PATCH /api/free-mode`

Toggles the pool and/or automatic fallback. Works with **zero** document imported. All
fields optional; unknown fields are rejected.

```json
{ "enabled": true, "fallback_on_quota": true, "probe_on_enable": true }
```

On `enabled` turning `false -> true` (with `probe_on_enable`, the default) the enabled
usable models are probed concurrently (concurrency 5, 6s each); a `key_required` model with
no account returns an instant synthetic `{ ok: false, status: 401, message: "Add an
<Provider> API key on Providers to use its free models", needs_key: true }`. Responds `200`
with the same shape as `GET /api/free-mode` (fresh probes and summary).

### `PATCH /api/free-mode/models`

Enable or disable one discovered model. The model id travels in the JSON body so ids with a
`/` (e.g. `inclusionai/ling-3.2-flash:free`) never break URL routing.

```json
{ "provider": "opencode", "model": "tidepool-alpha-free", "enabled": false }
```

`400` when `provider`/`model` are missing or enabling a `usable: false` model; `404` when
`(provider, model)` was not discovered. Responds `200` with
`{ ok, provider, model, enabled, summary, next_fallback_model }`.

### `POST /api/free-mode/probe`

Probe one model now (the "Try" button) or all enabled usable models (`{}` / `{"all": true}`,
optionally `{"refresh_catalogs": true}`). Always `200` for a probe outcome; `400`/`404` only
for invalid input or an unknown model.

```json
{ "provider": "opencode", "model": "big-pickle" }
```

Responds `200` with `{ provider, model, endpoint, probe }` (single) or
`{ ok, checked_at, results: [{ provider, model, endpoint, probe }] }` (all).

### `POST /api/free-mode/import`

Body: the document, or `{"document": {…}}`. Idempotent.

```json
{
  "ok": true,
  "providers": 2,
  "accounts": { "created": 3, "updated": 0 },
  "disabled": 0,
  "routing": {
    "enabled": true,
    "per_account_rpm": 10,
    "max_attempts": 8,
    "prefer": "spread",
    "models": ["deepseek-v4.1-flash", "qwen3-coder:free"]
  }
}
```

`routing` is echoed as stored (defaults filled in, `models` only when the document gave one)
so a dashboard can render the selection it just imported. `providers` is the number of
provider entries, `accounts` counts connections created and updated, `disabled` counts
accounts `prune` parked.

Failures are `400` with the `details` list above, or a store error as `{"error": {...}}`.

### `POST /api/free-mode/discover`

Body optional: `{"providers": ["openrouter"]}` limits the run to those providers (ids or
codes); an empty array or no body covers every free-mode account.

```json
{
  "ok": true,
  "discovered_at": 1763760100,
  "providers": [
    { "provider": "openrouter", "accounts": 2, "models": 12, "free": 4 }
  ],
  "accounts": [
    {
      "connection_id": "0b4c…",
      "provider": "openrouter",
      "label": "acc-1",
      "models": 12,
      "free": 4,
      "error": null
    },
    {
      "connection_id": "71de…",
      "provider": "openrouter",
      "label": "acc-2",
      "models": 0,
      "free": 0,
      "error": "upstream answered 401: {\"error\":{\"message\":\"Incorrect API key provided: ***\"}}"
    }
  ]
}
```

`providers[].accounts` counts every account the run covered, including the ones that
failed; `models` and `free` count the distinct ids the working accounts listed. One broken
account costs its own row and nothing else — the run never fails as a whole for a country of
one bad credential.

## How discovery decides "free"

For each free-mode account, the gateway fetches the provider's own model list through that
account's credentials (and, once the per-account proxy lands, through that account's proxy)
using `models_registry::models_url` / `parse_models`. Each id is classified by
`classify_free`:

1. **The id advertises it** — a `:free` suffix, or `-free` / `free-` / `free:` anywhere,
   case-insensitive (`deepseek-r1:free`, `llama-3-70b-free`, `free-gpt-oss`). Source
   `provider-list`.
2. **The registry prices it at exactly zero** for input *and* output. Source `registry`.
3. **A vendored whole-key free tier lists it** — on a Groq, Cerebras, Gemini or Mistral
   free-mode account, a model with a row in `crates/rrouter-gateway/data/free-tiers.json`
   (vendored from freellmapi; see `design/freellmapi-parity.md`). Only those rows: other
   models on the same key stay paid. Source `provider-list`.
4. Otherwise it is not free. An unknown price is not a free price: guessing would spend paid
   quota by accident. Such rows are still stored with `free = false`, so the dashboard can
   show what an account serves without pretending it belongs in the pool.

Every classified id is upserted into `free_models` with the account that proved it
(`connection_id`) and the run's timestamp. A seeded model (source `document`) that the list
does not advertise keeps its free status and its source; a list that does advertise it
upgrades the row to `provider-list`.

## Choosing models

Four refs are reserved, and none of them ever widens to a non-free account:

| Ref | What it spends |
| --- | --- |
| `free` | Every enabled, usable free model — the public-catalog pool plus free-mode accounts, narrowed by `routing.models` when that list is non-empty. |
| `free:PROVIDER` | The same pool limited to one provider, e.g. `free:openrouter`. |
| `free/MODEL` | One model id across every free-mode account that serves it. |
| `free:PROVIDER/MODEL` | One model on one provider's free-mode accounts. |

A normal (non-`free_mode`) account on a provider is eligible for a **verified-free** model
but never for a paid one. A model is verified free only by a public catalog's zero price, a
`:free`/`-free` id, or (on a free-mode account only) a vendored whole-key free-tier row; an
unknown price is not a free price.

A concrete call:

```json
POST /v1/chat/completions
{ "model": "free/deepseek-v4.1-flash", "messages": [{ "role": "user", "content": "hi" }] }
```

`free/deepseek-v4.1-flash` rotates across accounts (and therefore proxies) on the free tier
of every provider that serves that model. `free` is the pool-wide form: it picks a model as
well as an account, and `routing.models` is how an operator says which models that bare ref
may pick from — `["deepseek-v4.1-flash"]` restricts `free` to that one model while
`free/qwen3-coder:free` keeps working, because a `free/MODEL` ref names its model
explicitly. An empty or absent `routing.models` leaves the whole discovered pool available.

`free:unknown` answers `404` naming the provider, and a pool with nothing discovered answers
`404` telling the operator to run discovery.

## Discovery and teardown from the CLI

Both commands reuse the same `validate` and `apply` code as the endpoint, so a document the
CLI accepts is one the HTTP API accepts. They open the store directly and never speak HTTP,
so they work on a stopped instance.

```console
$ rrouter admin free-mode import ./free-mode.json
free-mode: 2 provider(s), 3 account(s) created, 0 updated, 0 disabled, 3 model(s) seeded
$ rrouter admin free-mode show
free-mode: imported at 1763760000
routing: {"enabled":true,"per_account_rpm":10,"max_attempts":8,"prefer":"spread","models":["deepseek-v4.1-flash"]}
accounts: 3
  openrouter / acc-1  proxy=http://***@10.0.0.1:8080  rpm=5  active
  openrouter / acc-2  proxy=direct  rpm=5  active
  my-free-host / host-1  proxy=socks5h://10.0.0.2:1080  rpm=none  active
models: 4 (3 free)
  openrouter / deepseek-r1:free  free  source=provider-list
  …
```

`import` exits `0` when the document applied, `1` when it was rejected — printing one line
per problem on stderr — or when the file or store could not be read.

## Teardown

```
DELETE /api/free-mode
{"ok": true, "disabled": 0}
```

Clears the stored document and every `free_models` row. `?disable=true` also flips the
free-mode accounts to inactive. Credentials are never deleted: a teardown that dropped them
could not be undone by a re-import, and disabling is the operator's decision to make
separately.

## Security notes

- **Credentials are written once.** An imported `api_key` / `access_token` / `refresh_token`
  goes into that connection's AES-256-GCM blob. The `free_mode` table's `document` column is
  plain text, so the stored copy is redacted before it is written: secrets are replaced by
  `has_api_key`-style booleans. `GET /api/free-mode` re-runs the same redaction, so a
  document written by an older build cannot leak through the read path.
- **Proxies are masked everywhere a read path touches them.** `scheme://user:pass@host:port`
  is shown as `scheme://***@host:port`; the raw URL stays in the encrypted credential blob.
  A proxy URL inside a validation error is masked too. The point of a proxy is the address
  the account leaves from, and its userinfo is a password.
- **Discovery errors are scrubbed.** A provider that echoes the rejected key in its 401 body
  would otherwise hand it back in the per-account error row; the account's own secrets are
  replaced with `***` before the text is returned.
- **An import is pre-flight and idempotent.** Validation runs entirely before the first
  write, so a rejected document changes nothing; each apply step is idempotent, so a failed
  import is fixed by importing again rather than by repairing rows.
- **Teardown is reversible for credentials, not for accounts.** The document and the
  discovery cache are dropped; accounts and their secrets are kept, and `?disable=true` only
  flips `is_active`.
