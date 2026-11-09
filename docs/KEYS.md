# Keys, limits and teams

A gateway key is a credential plus the limits an operator needs.

| Limit | Effect |
| --- | --- |
| `rpm` | requests per minute, sliding window; over limit `429 rate_limited` + `Retry-After` |
| `tpm` | tokens per minute from the last 60 s; `429 rate_limited` |
| `budget_tokens` | token budget per `budget_window`; `429 budget_exhausted` |
| `budget_micros` | spend budget in micro-USD, priced from the model catalog |
| `allowed_models` | exact refs or `provider/*` globs; otherwise `403 model_not_allowed` |
| `expires_at` | after that moment `403 api_key_expired` |
| `guardrails` | prompt cap, blocked phrases, output scan, PII redaction |

Spend is read from the usage table, not a drifting counter. The API Keys page edits limits,
renames keys, maps models and shows spend. Keys are stored as SHA-256 hashes; plaintext is
shown once.

## Teams

A team carries token and USD budgets over a lifetime or daily/weekly/monthly window. A key
belonging to a team spends the team's allowance; an exhausted team answers
`429 team_budget_exhausted` before dispatch. Usage rows record the team, so usage reads
attribute spend per team without a join. There is deliberately no prepaid credit balance
([ADR 0001](decisions/0001-no-prepaid-credits.md)).

## Guardrails

Input checks run before planning (system prompt plus message text); output checks inspect the
response, and a streamed hit is cut with one error frame. Blocks answer `403` with
`prompt_too_long`, `guardrail_blocked` or `output_blocked`.

PII redaction is opt-in per key. Emails, phone numbers, cards and credential-shaped strings
become `[email]`/`[phone]`/`[card]`/`[secret]` in prompts, non-streaming responses and
streamed text. Streams hold at most 512 bytes so a value split across deltas is caught; a
longer value can straddle an emission cut. Tool-call arguments and opaque payloads are not
rewritten.

See [Guardrails](GUARDRAILS.md) for the full policy semantics.
