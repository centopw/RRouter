# Routing

RRouter resolves a model string to an ordered candidate list, then dispatches with retries,
fallback and a per-account breaker.

## Model strings

- **`provider/model`** — `openai/gpt-4.1`, `claude/sonnet-4-5`, `codex/gpt-5.6-luna`.
- **`group/provider/model`** — a preset group namespaces the ref and still pins the
  provider: `main/openai/gpt-4.1`. The provider segment is required, so a model id
  served by several providers is never ambiguous.
- **Aliases** — a stable name (`prod-default`) mapped to one target.
- **Combos** — `combo:<name>` or a bare combo name; strategies are `fallback`,
  `round_robin` and `smart`. Combining providers is a combo, not a model row.

A bare model id expands to every connected provider that serves it. Explicit
`provider/model` and `group/provider/model` never widen to another provider.

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
