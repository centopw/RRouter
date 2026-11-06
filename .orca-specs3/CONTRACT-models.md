# Model registry — frozen contract (M1 backend / M2 UI)

Both workers implement against THIS file. Do not renegotiate shapes in code; if
something here is wrong, say so in the worker report instead of inventing a
second shape.

## Capability object (every place capabilities appear)

```json
{
  "vision": false,
  "pdf": false,
  "audio_input": false,
  "video_input": false,
  "image_output": false,
  "audio_output": false,
  "tools": true,
  "reasoning": false,
  "search": false
}
```

All nine keys are always present. `search` is never derived from models.dev (it
has no such field); it is only ever user-set.

## `GET /api/models`

```json
{ "data": [
  { "ref": "codex/gpt-5.6-luna",
    "provider": "codex",
    "id": "gpt-5.6-luna",
    "display": "GPT 5.6 Luna",
    "context": 128000,
    "max_output": null,
    "capabilities": { "...": "nine keys" },
    "source": "catalog",
    "enabled": true,
    "available": true }
] }
```

- `source` is one of `catalog`, `upstream`, `custom`, `synced`.
- `available` = at least one active connection exists for `provider`.
- Sorted by `provider`, then `id`.
- `enabled` comes from the existing `disabled_models` table (a model disabled
  through the current `/api/disabled/{model}` routes reads `false` here too).

## `GET /api/models/providers`

```json
{ "data": [ { "id": "codex", "display": "Codex (ChatGPT OAuth)",
              "has_connection": true, "listed": true } ] }
```

`listed` = the provider can enumerate its models over HTTP (false for codex).

## `POST /api/models/sync`

Downloads `https://models.dev/api.json`, replaces every `source = "synced"` row.

```json
{ "ok": true, "models": 4123, "providers": 145, "synced_at": 1762500000 }
```

A failed download is HTTP 502 with the admin error shape; it never deletes rows.

## `POST /api/models/fetch`

Body `{ "connection_id": "uuid" }`. Calls the provider's own list endpoint with
that connection's credentials and stores the ids as `source = "upstream"` for the
provider.

```json
{ "provider": "openai", "status": 200, "count": 42, "added": 40,
  "data": ["gpt-4.1", "o4-mini"] }
```

## `POST /api/models/custom`

Upserts one `source = "custom"` row. `capabilities` and limits are optional.

```json
{ "provider": "openai", "id": "my-model", "display": "My Model",
  "context": 128000, "max_output": 8000,
  "capabilities": { "tools": true, "vision": true } }
```

Returns the stored item in the `GET /api/models` item shape.

## `DELETE /api/models/custom?provider=openai&id=my-model`

`{ "deleted": true }`, 404 when absent. Only `custom` rows may be deleted here.

## `POST /api/models/test`

Body `{ "model": "codex/gpt-5.6-luna" }` — a real 16-token completion through the
normal planner/dispatcher path, so it proves exactly what clients hit.

```json
{ "ok": true, "status": 200, "latency_ms": 812,
  "model": "codex/gpt-5.6-luna", "reply": "ok" }
```

Failures keep HTTP 200 and report `{ "ok": false, "status": 400,
"latency_ms": 300, "error": "..." }`, because the UI renders both outcomes.

## Precedence

For a given `(provider, id)`: `custom` > `upstream` > `synced` > `catalog`.
`/v1/models` (public, OpenAI-shaped) also includes `custom` and `upstream` rows.

## Provider id → models.dev provider id

```
codex → openai      claude → anthropic   gemini → google
github → github-copilot                 glm → zai
kimi → moonshotai   qwen → alibaba      hunyuan → tencent
```

Providers not listed use their own id.
