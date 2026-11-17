# Routing

RRouter resolves a model string to an ordered candidate list, then dispatches with retries,
fallback and a per-account breaker.

## Model strings

- **`provider/model`** — `openai/gpt-4.1`, `claude/sonnet-4-5`, `codex/gpt-5.6-luna`.
- **`code/model`** — the provider's shorthand: `oai/gpt-4.1`, `ocg/deepseek-v4.1-flash`.
  Every provider has a short code (`oai`, `gh`, `ocg`, `ant`, …), editable in
  **Providers → Provider codes**; it is unique across compiled and operator-defined
  providers. The id and the code both resolve, so existing refs keep working.
- **`group/code/model`** — a preset group namespaces the ref and still pins the
  provider: `main/oai/gpt-4.1`. The provider segment is required, so a model id
  served by several providers is never ambiguous.
- **Aliases** — a stable name (`prod-default`) mapped to one target.
- **Combos** — `combo:<name>` or a bare combo name; strategies are `fallback`,
  `round_robin` and `smart`. Combining providers is a combo, not a model row.

A bare model id expands to every connected provider that serves it. Explicit
`provider/model`, `code/model` and `group/code/model` never widen to another provider.

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

## Headers and traces

Every `/v1` response carries `x-request-id` and `x-trace-id`. The request id links the usage
row and each attempt; see [Observability](OBSERVABILITY.md).
