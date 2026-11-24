# Routing

RRouter resolves a model string to an ordered candidate list, then dispatches with retries,
fallback and a per-account breaker.

## Model strings

- **`provider/model`** — `openai/gpt-4.1`, `claude/sonnet-4-5`, `codex/gpt-5.6-luna`.
- **`code/model`** — the provider's shorthand: `oai/gpt-4.1`, `ocg/deepseek-v4.1-flash`.
  Every provider has a short code (`oai`, `gh`, `ocg`, `ant`, …), editable in
  **Providers → Provider codes**; it is unique across compiled and operator-defined
  providers. The id and the code both resolve, so existing refs keep working.
- **`slug/code/model`** — a free-form preset group namespaces the ref with its **stable
  slug** (`main/oai/gpt-4.1`, `main/ocg/deepseek-v4.1-flash`) and still pins the
  provider. The slug is assigned when the group is created (or backfilled on migration)
  and is **immutable**, so renaming a group's display name never breaks existing client
  configs. Legacy display-name prefixes remain accepted as a fallback (resolved via
  name → slug lookup and logged once at `debug` level). Inbound group refs are
  normalized to their canonical `<slug>/<code>/<model>` form for routing while usage
  logs keep the client's literal `requested_model`.
- **`GET /api/models`** emits each row's canonical `reference` (`<slug>/<code>/<id>` for
  grouped models, `<provider>/<id>` for ungrouped models) alongside `also_matches`
  listing the other accepted spellings (`<code>/<id>`, `<provider>/<id>`, legacy group
  name prefixes, and bare `<id>`).
- **Per-key `default_group`** — an inference key may set a nullable `default_group`
  (stored as a free-form group's stable slug). When a request on that key has no explicit
  group prefix (and is not `free*`, an alias, or a combo):
  - a bare model id (`deepseek-v4.1-flash`) scopes to that group's members in layout
    order and never widens to unrelated providers (falling back to normal catalog
    resolution if the group has no member for that id);
  - an explicit `provider/model` or `code/model` keeps its pinned provider (never widens
    to another provider);
  - the group's access grants (`GroupAccess`) are enforced at admission.
- **Aliases** — a stable name (`prod-default`) mapped to one target.
- **Combos** — `combo:<name>` or a bare combo name; strategies are `fallback`,
  `round_robin` and `smart`. Combining providers is a combo, not a model row.

A bare model id expands to every connected provider that serves it (unless scoped by the
key's `default_group`). Explicit `provider/model`, `code/model` and `slug/code/model`
never widen to another provider.

## Custom providers

A compiled provider is fixed; an operator-defined one adds the same three things:
an id derived from its name, a code, and a base URL speaking OpenAI Chat or Anthropic
Messages. Add it under **Providers → Provider codes → new provider**, then add an API-key
account for it. Its models are the provider's own `/v1/models` (Fetch) or operator-defined
rows, and requests route exactly like a compiled provider's. The provider's base URL is
copied onto new accounts; connections cannot clear it.

Deleting a custom provider is refused while an account still routes through it.

## Account order

Candidates are ordered by connection priority, then by `RROUTER_STRATEGY` inside each
priority group:

| Strategy | Order |
| --- | --- |
| `priority` | configured priority, stable rotation |
| `weighted` | smooth weighted round-robin over connection weights |
| `least_latency` | ascending EWMA of successful attempts |
| `cheapest` | lowest catalog price for the model; unknown prices last |

Priority always dominates across groups. A key's model mapping runs before alias/combo
resolution, so a client can keep a stable name while the operator repoints it.

## Breakers and fallback

- A retryable failure moves to the next candidate after bounded retries.
- 401/403, 5xx and transport failures are connection-wide breaker failures.
- A 429 cooldown is scoped to `(connection, model)`: a free-tier model limit does not block
  another model on the same account.
- Exhausted plans return `429 all_candidates_cooling_down` with `Retry-After`, not 404.
- Half-open probes bring recovered accounts back without a thundering herd.

Team-scoped keys are planned only from their own team's connections; shared instance-wide
accounts are never eligible for a team key.

## Free mode

An operator can import free-tier accounts, each with its own egress proxy, and then ask for
the reserved refs `free`, `free:<provider>`, `free/<model>` or `free:<provider>/<model>`.
They resolve before aliases and combos, expand only to free-mode accounts and only to models
discovery marked free, and rotate models and accounts — hence proxies — per request so a
pool of accounts reaches the providers' per-account limits later. Priority, allowlists,
request policy, quotas and the breaker/429 cooldown keep their usual precedence; the
per-account `free_rpm` window only reorders accounts and never refuses a request. See
[Free mode](FREE_MODE.md).

## Capability endpoints

`/v1/search`, `/v1/rerank` and `/v1/embeddings` route through the same planner — key
mapping, allowlist, admission, priority, strategy and account failover — but only some
providers can answer each one, so candidates that cannot are dropped.

| Endpoint | A provider qualifies when | Refused otherwise |
| --- | --- | --- |
| `/v1/search` | the compiled native-search list names it: `claude`/`anthropic`, `openai`, `codex`, `xai`, `gemini`. A provider id alone expands to its default search model | `501` before any plan |
| `/v1/rerank` | a community manifest declares `rerank_url` (`cohere`, `jina` today) | no plan candidate |
| `/v1/embeddings` | its default dialect is OpenAI Chat or OpenAI Embeddings | `501` |

Each endpoint falls back in the same order: a client's `model` (or, for search, `model` or
`provider`) wins; otherwise the operator's stored default for that capability is used,
routed exactly like a client ref; otherwise the endpoint keeps its own missing-model answer.
See [API → Capability defaults](API.md#capability-defaults) and
[Compression](COMPRESSION.md) for the input/output trimming that runs before the plan.

## Headers and traces

Every `/v1` response carries `x-request-id` and `x-trace-id`. The request id links the usage
row and each attempt; see [Observability](OBSERVABILITY.md).
