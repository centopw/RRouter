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
| `POST /v1/search` | provider-native web search, routed through the same planner |
| `GET /v1/models/search` | connected providers with a native search surface |
| `/v1/files*` | provider file API forwarded; 501 when unsupported |
| `/v1/batches*` | provider batch API forwarded; 501 when unsupported |

`/v1/messages/count_tokens` and `/v1/responses/compact` are provider-native shapes the
canonical request cannot represent. A provider without the endpoint answers `501` in its
dialect's error envelope. Codex's subscription backend does not serve `/responses/compact`;
an OpenAI API-key connection does.

`GET /v1/models` lists only enabled models from connected providers. A disabled model is
absent; re-enabling restores it. `/v1/models/{id}` returns 404 for a disabled or unroutable
model.

The response remains an OpenAI list. It adds grouping metadata from the instance's default
model preset: every entry carries `group` (a group id) and the top level carries an ordered
`groups` array (`id`, `name`, `provider`, `position`). Models without an explicit group are
in an implicit provider bucket (`provider:<provider>`). The default preset is also the
discovery policy: an item hidden in it, or a group toggled off, is omitted from `data` and
`groups`; `auto_include_new: false` drops models the preset never names. Routing is
unaffected, so hidden models still work when a client names them explicitly. A client that
ignores `groups` still reads a normal OpenAI `data` list.

## Web search

`POST /v1/search` runs a provider's own hosted search tool and returns one answer with
ordered citations. It uses the same model string, planner, credentials, account failover,
key limits and usage accounting as chat: a provider id expands to its default search model,
`provider/model` pins one, and a combo or alias chains providers.

| Field | Required | Notes |
| --- | --- | --- |
| `model` or `provider` | yes | Provider id (`claude`), `provider/model`, alias or combo. Both names are accepted; they must agree when both are sent. |
| `query` | yes | Search prompt, at most 8 KiB. Trimmed. |
| `max_results` | no | 1–20, default 5. Caps returned citations; the model may have read more pages. |
| `search_type` | no | Only `web`, the default. Provider-native search is not a news/X-firehose API. |
| `domain_filter` | no | At most 10 domains. Anthropic and Responses providers get their native domain filter; Gemini receives the same restriction in the prompt. |

```sh
curl -s localhost:20128/v1/search \
  -H "Authorization: Bearer sk-rr-..." \
  -H "Content-Type: application/json" \
  -d '{"model":"claude","query":"What changed in Rust 1.90?","max_results":3}'
```

```json
{
  "provider": "claude",
  "model": "claude-sonnet-4-5",
  "query": "What changed in Rust 1.90?",
  "results": [
    {
      "title": "Announcing Rust 1.90.0",
      "url": "https://blog.rust-lang.org/...",
      "snippet": "Rust 1.90 was released.",
      "position": 1,
      "score": null,
      "published_at": "2026-01-01T00:00:00Z",
      "favicon_url": null,
      "content": null,
      "metadata": {},
      "citation": {"provider": "claude", "retrieved_at": 1794312000, "rank": 1}
    }
  ],
  "answer": {"source": "claude", "model": "claude-sonnet-4-5", "text": "Rust 1.90 was released."},
  "usage": {"queries_used": 1, "search_cost_usd": 0, "input_tokens": 120, "output_tokens": 40, "llm_tokens": 160},
  "metrics": {"total_results_available": null, "returned_results": 1},
  "errors": []
}
```

Native search surfaces:

| Provider ids | Wire surface | Default model |
| --- | --- | --- |
| `claude`, `anthropic` | Messages API with `web_search_20250305` | `claude-sonnet-4-5` |
| `openai`, `codex`, `xai` | Responses API with the `web_search` tool | `gpt-5-mini`, `gpt-5.6-luna`, `grok-4` |
| `gemini` | `generateContent` with `google_search` grounding | `gemini-2.5-flash` |

A provider with no native search is removed from the plan. If no candidate remains, the
route answers `501` without calling upstream. Search tokens are written to the normal usage
row and priced from the model registry; the search tool's own per-call fee is not priced by
the gateway.

`GET /v1/models/search` lists connected search providers for clients that discover
capabilities at runtime (`kind` is `webSearch`, and `default_model` is the provider-only
default).


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
