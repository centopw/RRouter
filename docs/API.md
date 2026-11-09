# API

All inference endpoints accept the same model string. The protocol decides the wire shape;
the model decides where it routes.

## Inference

| Path | Dialect |
| --- | --- |
| `POST /v1/chat/completions` | OpenAI Chat Completions |
| `POST /v1/responses` | OpenAI Responses (Codex-style) |
| `POST /v1/messages` | Anthropic Messages |
| `POST /v1/embeddings` | OpenAI Embeddings |
| `POST /v1/messages/count_tokens` | Anthropic token counting (forwarded) |
| `POST /v1/responses/compact` | OpenAI/Codex compaction (forwarded) |
| `GET /v1/models` | catalog + upstream + custom models |
| `GET /v1/models/{id}` | one model object, or OpenAI-shaped 404 |
| `POST /v1/images/generations` | forwarded when the provider declares images |
| `POST /v1/audio/speech` | forwarded; audio bytes relayed with upstream content-type |
| `POST /v1/audio/transcriptions` | multipart forwarded verbatim |
| `GET /v1/audio/voices` | forwarded when declared |
| `POST /v1/rerank` | canonical rerank, forwarded when declared |
| `/v1/files*` | provider file API forwarded; 501 when unsupported |
| `/v1/batches*` | provider batch API forwarded; 501 when unsupported |

`/v1/messages/count_tokens` and `/v1/responses/compact` are provider-native shapes the
canonical request cannot represent. A provider without the endpoint answers `501` in its
dialect's error envelope. Codex's subscription backend does not serve `/responses/compact`;
an OpenAI API-key connection does.

`GET /v1/models` lists only enabled models from connected providers. A disabled model is
absent; re-enabling restores it. `/v1/models/{id}` returns 404 for a disabled or unroutable
model.

## Admin

`/api/*` is the admin surface: providers, keys, models, presets, teams, webhooks, evals,
usage, settings and quota pings. It is loopback-only unless `RROUTER_ALLOW_REMOTE_ADMIN=true`
and is additionally protected by `RROUTER_ADMIN_TOKEN` when set. See [Security](SECURITY.md).

Usage reads are aggregated in SQL:

| Query | Answers |
| --- | --- |
| `GET /api/usage?hours=&limit=` | request log: id, endpoint, tokens, cache, latency, status |
| `GET /api/usage/series?hours=&bucket=5m\|hour\|day` | per-bucket requests, errors, tokens, cache, latency |
| `GET /api/usage/series?by=model` | the same buckets split per model |
| `GET /api/usage/summary?hours=` | per-model totals; omit `hours` for all time |
| `GET /api/usage/export.csv?…` | the same filters as the log, streamed CSV |
| `GET /api/usage/{id}` | one request plus its candidate attempts |

## Health

| Path | Meaning |
| --- | --- |
| `GET /health` | process liveness and version |
| `GET /ready` | additionally reads the store; use for readiness |
| `GET /metrics` | Prometheus text behind the admin gate |
