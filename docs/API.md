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

Provider-native **server compaction** works on both native surfaces:

- `POST /v1/responses/compact` forwards OpenAI's compaction body and response.
- `POST /v1/responses` with a trailing `compaction_trigger` input item and
  `x-codex-beta-features: remote_compaction_v2` (Remote Compaction V2) forwards the opaque
  `compaction` output item byte-for-byte. The stream is still decoded for usage and
  guardrails, but the encrypted item is re-emitted verbatim rather than rebuilt.
- `POST /v1/messages` forwards Anthropic's compaction beta fields unchanged:
  `context_management.edits[].type = "compact_20260112"` (threshold) or the top-level
  `compaction` parameter with beta `compact-2026-09-04` (on-demand). The client's
  `anthropic-beta` header is merged with the gateway's own flags; the request body is not
  rebuilt, so the signed compaction block survives round trips. These are not local
  summarization: the provider writes the summary and bills its extra sampling step.

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

Every entry is identified by the route a client should use: a free-form group's name plus
the provider code and model id (`main/ocg/deepseek-v4.1-flash`), the provider code for a
provider bucket (`ocg/...`), or the bare alias/combo name. The provider code is its
editable shorthand (defaults: `oai`, `gh`, `ocg`, `ant`, …), unique across compiled and
operator-defined providers; the long id also resolves in a request, so older refs keep
working. The provider segment pins
the row; combining providers is a combo's job. A shared id lists once per provider. Each
provider entry also carries `model` with the bare id, so a client never has to split the id.

Group access is opt-in. A group with no `access` object is open to everyone; otherwise
`/v1/models` omits it (and its models) unless the caller's key, key team or key owner is
granted, and a `<group>/...` ref answers `403` for an ungranted caller.

```sh
# Open the group to everyone, or restrict it to identities:
curl -X PUT localhost:20128/api/model-groups/g-main/access \
  -H "Content-Type: application/json" \
  -d '{"keys":["<key-uuid>"],"teams":[],"users":[]}'
curl -X PUT localhost:20128/api/model-groups/g-main/access \
  -H "Content-Type: application/json" -d '{"all":true}'
```

A gateway key can carry `owner_user_id` (a user uuid), so user-targeted grants match the
key's owner. Existing keys are ownerless until set.

Every entry also carries the registry's model metadata, so a client can choose a model
without a second lookup. Unknown fields are omitted, never invented:

| Field | Meaning |
| --- | --- |
| `id` | `<group>/<code>/<model>` for a free-form group, `<code>/<model>` for a provider bucket, or the alias/combo name. |
| `model` | Bare model id for provider entries (absent on aliases and combos). |
| `name` | Operator/provider display name. |
| `context_window`, `context_length` | Context window in tokens (both spellings). |
| `max_output_tokens`, `max_completion_tokens` | Output cap in tokens (both spellings). |
| `input_modalities` / `output_modalities` | Derived from the capability flags: `text`, `image`, `pdf`, `audio`, `video`. |
| `capabilities` | The nine flags in camelCase (`vision`, `pdf`, `audioInput`, `videoInput`, `imageOutput`, `audioOutput`, `search`, `tools`, `reasoning`), plus `contextWindow`/`maxOutput` and provider-native reasoning keys such as `thinkingFormat`. |
| `effort` | Provider-published reasoning levels (`supported_levels`, `default_level`). |
| `reasoning` | Normalized thinking options: `supported_efforts`, `default_effort`, `default_enabled`, `supports_max_tokens`, `mandatory`. See [Thinking effort](REASONING.md). |
| `api_capabilities` | Provider-published API quirks (for example Anthropic message rules). |
| `deprecated` | Present and `true` when the catalog marks the family retired. |

Limits, modalities, `effort` and `api_capabilities` are stored from the provider's own
`/models` response when it publishes them (DeepSeek and 9router-style gateways do), and are
overlaid by the models.dev sync and operator-defined rows. An id-only provider list (OpenAI)
gets enrichment only from those sources. Aliases borrow their target's metadata; a combo
advertises the conservative intersection of its members (smallest context/output, capability
flags every member has) and omits metadata when a member cannot be resolved.

`GET /v1/models/{id}` returns the same object for one routable id (with no `group`).

## Provider codes and custom providers (admin)

`GET /api/models/providers` lists every provider with its `code`, `custom` flag, dialect and
(when operator-defined) `base_url`.

| Endpoint | Body | Effect |
| --- | --- | --- |
| `PATCH /api/models/providers/{id}` | `{"code":"ocg"}` | Rename the shorthand. Codes are unique across compiled and custom providers and 1–16 characters of `a-z 0-9 - _ .`; `400` when taken. |
| `POST /api/models/providers` | `{"name":"Acme","code":"acme","base_url":"https://api.acme/v1","dialect":"openai"}` | Create an operator-defined provider. `code` is optional (derived from the name); `dialect` is `openai` or `anthropic`. The id is a slug of `name`, de-duplicated. |
| `DELETE /api/models/providers/{id}` | — | Delete an operator-defined provider. Refused while an account still uses it; its model rows are removed. |

A new custom provider has no account yet: add one with `POST /api/connections` using its id.
The account inherits the provider's base URL (the connection's own `base_url_override` still
wins), and its models come from Fetch or `POST /api/models/custom`.

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

## Capability defaults

Search, rerank and embeddings each accept one stored default — a `provider` or
`provider/model` ref, edited on the dashboard's Search, Rerank and Embeddings pages or with
`PUT /api/settings`. It is used only when the client omits the field, and it never
overrides a client value. Resolution order:

```
request model/provider  >  stored default  >  the endpoint's own 400
```

- `POST /v1/search` — the default applies only when the body has neither `model` nor
  `provider`. Partial bodies are untouched, so `{"provider":"jina"}` keeps its own answer
  and `{"model":123}` still fails validation.
- `POST /v1/rerank` and `POST /v1/embeddings` — the default applies only when `model` is
  absent (not `null`, not a non-string). The model it supplies is routed exactly like a
  client-sent one: key mapping, allowlist, admission and planning all apply.
- With no default stored, the endpoint answers as before: `400` for a missing model, and
  `501` when the routed provider does not serve the endpoint (`/v1/search` with a provider
  that has no native search surface, `/v1/rerank` with no plan candidate, `/v1/embeddings`
  on a provider whose dialect is neither OpenAI Chat nor OpenAI Embeddings).

A stored value must be a non-empty, already-trimmed string of at most 200 characters;
`null` clears the slot. `PUT /api/settings` deep-merges the block, so naming one capability
keeps the other two, and a number, empty, padded or over-long value answers `400`.

```sh
curl -s localhost:20128/api/capabilities -H "x-api-key: <admin token>"
```

```json
{
  "defaults": { "search": "claude", "rerank": null, "embeddings": "openai/text-embedding-3-small" },
  "providers": {
    "search": [
      { "provider": "claude", "display": "Claude (Anthropic OAuth)", "default_model": "claude-sonnet-4-5", "connected": true, "accounts": 2 }
    ],
    "rerank": [
      { "provider": "jina", "display": "Jina AI", "default_model": null, "connected": false, "accounts": 0 }
    ],
    "embeddings": [
      { "provider": "openai", "display": "OpenAI", "default_model": null, "connected": true, "accounts": 1 }
    ]
  }
}
```

`GET /api/capabilities` is admin-only. `providers.<capability>` mirrors the gates the
request path applies, so a ref the page suggests can actually be served (see
[Routing](ROUTING.md#capability-endpoints)): search by the compiled native-search list,
rerank by `rerank_url` in a community manifest, embeddings by an OpenAI Chat or OpenAI
Embeddings provider. `accounts` counts active connections (`connected` is `accounts > 0`);
a disabled account keeps its row in the store but serves nothing, so it does not count.
Rows are sorted by display name, then provider id.

## Admin

`/api/*` is the admin surface: providers, keys, models, presets, teams, webhooks, evals,
usage, settings and quota pings. It is loopback-only unless `RROUTER_ALLOW_REMOTE_ADMIN=true`
and is additionally protected by `RROUTER_ADMIN_TOKEN` when set. See [Security](SECURITY.md).

Usage reads are aggregated in SQL:

| Query | Answers |
| --- | --- |
| `GET /api/usage?hours=&limit=` | request log: id, endpoint, method/protocol, tokens, cache, latency, status, requested model, finish reason, reasoning effort/tokens, client session |
| `GET /api/usage/series?hours=&bucket=5m\|hour\|day` | per-bucket requests, errors, tokens, cache, latency |
| `GET /api/usage/series?by=model` | the same buckets split per model; `connection_id=<uuid>` narrows either series to one account |
| `GET /api/accounts/usage` | each active account's provider-reported subscription windows (`id`, `used_percent`, `window_seconds`, `resets_at`) |
| `GET /api/accounts/usage/{id}/history?window=<window_id>` | the sampled curve of one window: `{day, used_percent}` per UTC day, the day's peak, oldest first, at most 31 points; `window` is required and an unknown account is `404` |
| `GET /api/usage/summary?hours=` | per-model totals; omit `hours` for all time |
| `GET /api/usage/export.csv?…` | the same filters as the log, streamed CSV |
| `GET /api/usage/{id}` | one request plus its candidate attempts |
| `DELETE /api/usage/{id}` | delete one request-log row by row id or request id; `204` on success, `404` when absent, audited as `log_deleted`. Its attempts and captured bodies are removed in the same transaction |
| `GET /api/usage/{id}/capture` | the stored request headers/body and response body for one request, or `captured: false` when capture was off |

Request/response capture is opt-in (`capture_bodies`, default off) and edited on
`/settings/runtime`. While it is on, the proxy stores up to 64 KiB of each body — request
as the client sent it, response as the client received it, including SSE frames — with
the values of `authorization`, `x-api-key`, `api-key` and `cookie` redacted. Captured rows
are pruned with usage retention and deleted with their usage row.

### Compression stats

`GET /api/compression/stats?hours=&team_id=` aggregates the `compression_events` rows the
proxy writes for each request whose body compression actually changed. The counters are
tool-result text bytes, never whole-request bytes; the token figure is an estimate. See
[Compression → Effectiveness](COMPRESSION.md#effectiveness).

| Parameter | Default | Notes |
| --- | --- | --- |
| `hours` | `168` | window in hours, clamped to `1`–`8760`; a non-numeric value answers `400` |
| `team_id` | none | a uuid, scoped exactly like `GET /api/usage/summary`: an admin may name any team, a reader only one they belong to (`403` otherwise) |

```json
{
  "hours": 168,
  "requests": 1204,
  "rtk_requests": 1103,
  "rtk_before": 18400000,
  "rtk_after": 5100000,
  "rtk_saved": 13300000,
  "rtk_saved_tokens_estimate": 3325000,
  "caveman_requests": 640
}
```

| Field | Meaning |
| --- | --- |
| `hours` | the window applied, after the clamp |
| `requests` | rows recorded — RTK removed something, Caveman injected, or both |
| `rtk_requests` | rows with `rtk_before > 0`, where the token saver dropped something |
| `rtk_before`, `rtk_after` | tool-result text bytes the token saver saw and produced |
| `rtk_saved` | `rtk_before - rtk_after`, saturating at zero |
| `rtk_saved_tokens_estimate` | `rtk_saved / 4` — an estimate, not a tokenizer run |
| `caveman_requests` | rows where the style instruction was appended |

`rtk_requests` and `caveman_requests` are subsets of `requests` and do not sum to it. The
rows have no retention setting of their own: they follow the usage cutoff.

## Health

| Path | Meaning |
| --- | --- |
| `GET /health` | process liveness and version |
| `GET /ready` | additionally reads the store; use for readiness |
| `GET /metrics` | Prometheus text behind the admin gate |
