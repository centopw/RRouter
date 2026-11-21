# Free mode

Free mode lets one JSON document declare free-tier providers, the accounts on them and the
egress proxy each account must leave through. RRouter then discovers which models each
account actually serves, and exposes the reserved ref `free` — routed across every free
account and proxy pair so per-account rate limits are hit later rather than failing a
request.

The document is the only place free mode is configured. Nothing about it is compiled in:
adding a provider, an account or a proxy is an import, not a restart.

## The document

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

All three sit on the admin API and are gated exactly like the rest of `/api/*`.

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

### `GET /api/free-mode`

```json
{
  "document": {
    "version": 1,
    "prune": false,
    "routing": { "…": "as imported" },
    "proxies": { "p1": "http://***@10.0.0.1:8080" },
    "providers": [
      {
        "provider": "openrouter",
        "models": ["deepseek-r1:free"],
        "accounts": [
          {
            "label": "acc-1",
            "priority": 0,
            "weight": 100,
            "rpm": 5,
            "proxy": "p1",
            "has_api_key": true,
            "has_access_token": false,
            "has_refresh_token": false
          }
        ]
      }
    ]
  },
  "routing": { "enabled": true, "per_account_rpm": 10, "max_attempts": 8, "prefer": "spread", "models": ["…"] },
  "updated_at": 1763760000,
  "accounts": [
    {
      "connection_id": "0b4c…",
      "provider": "openrouter",
      "label": "acc-1",
      "proxy": "http://***@10.0.0.1:8080",
      "free_rpm": 5,
      "is_active": true,
      "last_error": null
    }
  ],
  "models": [
    {
      "provider": "openrouter",
      "model": "deepseek-r1:free",
      "free": true,
      "price": 0.0,
      "source": "provider-list",
      "discovered_at": 1763760100
    }
  ],
  "discovered_at": 1763760100
}
```

`document` is `null` until something was imported; `accounts` and `models` are populated
independently, so an instance after a teardown is still inspectable. `document` is the
stored copy with every secret replaced: `has_api_key` / `has_access_token` /
`has_refresh_token` booleans, and proxy URLs masked. Which ids are in `routing.models` is
read from `routing`, not from a per-model flag.

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
3. Otherwise it is not free. An unknown price is not a free price: guessing would spend paid
   quota by accident. Such rows are still stored with `free = false`, so the dashboard can
   show what an account serves without pretending it belongs in the pool.

Every classified id is upserted into `free_models` with the account that proved it
(`connection_id`) and the run's timestamp. A seeded model (source `document`) that the list
does not advertise keeps its free status and its source; a list that does advertise it
upgrades the row to `provider-list`.

## Choosing models

Four refs are reserved, and none of them ever widens to a non-free-mode account:

| Ref | What it spends |
| --- | --- |
| `free` | Every free model of every active free-mode account, narrowed by `routing.models` when that list is non-empty. |
| `free:PROVIDER` | The same pool limited to one provider, e.g. `free:openrouter`. |
| `free/MODEL` | One model id across every free-mode account that serves it. |
| `free:PROVIDER/MODEL` | One model on one provider's free-mode accounts. |

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
