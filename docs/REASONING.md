# Thinking effort

Modern models expose **thinking effort** instead of a sampling temperature. RRouter
normalizes the three client shapes to one canonical `Reasoning` value and maps it to each
provider's native field. Temperature remains accepted for compatibility; it is never
required and is not invented when a client omits it.

| Client shape | Where | Canonical |
| --- | --- | --- |
| `reasoning_effort: "high"` | Chat Completions | `effort = high` |
| `reasoning: {effort, max_tokens, enabled, exclude}` | Chat / Responses | the same four controls |
| `output_config: {effort}` | Anthropic Messages | `effort` |
| `thinking: {type, budget_tokens}` | Anthropic Messages | `enabled`, `max_tokens` |

Effort values are `max`, `xhigh`, `high`, `medium`, `low`, `minimal`, `none` (strongest
first). Unknown values are refused with a translation error instead of being forwarded.

## Outbound mapping

| Provider | Field |
| --- | --- |
| OpenAI Chat / OpenAI-compatible | `reasoning_effort` |
| OpenAI Responses | `reasoning: {effort, summary: "auto"}` |
| Anthropic | `thinking: {type:"enabled", budget_tokens}`; effort becomes a share of `max_tokens`, and `max_tokens` is raised so it stays above the budget |
| Google Gemini / Code Assist | `generationConfig.thinkingConfig`; Gemini 3 gets a `thinkingLevel`, older models a `thinkingBudget` |

Anthropic budgets are clamped to at least 1,024 tokens and always below `max_tokens`.
`none` or `enabled: false` sends Anthropic `thinking: {"type":"disabled"}` and Gemini a
zero budget.

## Discovery

`GET /v1/models` adds a `reasoning` object to every reasoning model, in the shape
OpenRouter clients already read:

```json
"reasoning": {
  "supported_efforts": ["high", "medium", "low"],
  "default_effort": "medium",
  "default_enabled": true,
  "supports_max_tokens": true,
  "mandatory": false
}
```

- Native provider metadata (`effort.supported_levels`) wins.
- Otherwise RRouter uses the provider's documented levels (OpenAI, Anthropic, Gemini,
  DeepSeek, xAI, Qwen).
- `capabilities.thinkingCanDisable: false` becomes `mandatory: true`.
- A model without reasoning omits the object, so clients hide the selector.

## OpenCode / OpenAI-compatible harness

A harness that does not consume the discovery metadata can declare the capability in its
own provider config. The model id is the one `/v1/models` advertises:

```json
{
  "provider": {
    "rrouter": {
      "npm": "@ai-sdk/openai-compatible",
      "options": {
        "baseURL": "http://127.0.0.1:20128/v1",
        "apiKey": "sk-rr-…"
      },
      "models": {
        "main/deepseek-v4.1-flash": {
          "name": "DeepSeek V4.1 Flash (main)",
          "reasoning": true,
          "options": { "reasoningEffort": "high" }
        }
      }
    }
  }
}
```

The request then carries `reasoning_effort: "high"` (or a `reasoning` object); RRouter
forwards it to an OpenAI-compatible provider or translates it to the provider's native
field.

## Temperature

Send `temperature` only when the target model accepts it. Some providers reject it
outright (ChatGPT-backed Codex accounts do; RRouter already strips temperature/top_p for
that provider). A missing temperature is sent as absent, never as `0`.
