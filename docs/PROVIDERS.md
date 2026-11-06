# Providers

One row per provider this build can route to. "Native" is the endpoint the provider speaks
without translation; anything else is translated through the canonical request (Anthropic
clients can call a Codex account, OpenAI clients get OpenAI-shaped streams back).

| Id | Display | Auth | Native | Model list | Notes |
| --- | --- | --- | --- | --- | --- |
| `claude` | Claude (Anthropic OAuth) | subscription (PKCE) | Messages | yes | Request must claim a current Claude Code version; a stale one is refused with "version 2.1.280 or newer is required". Callback host is `localhost`. |
| `codex` | Codex (ChatGPT OAuth) | subscription (device code) | Responses | **no** | Stream-only: non-streaming clients are served by reassembling the stream. Its five verified ids come from the built-in catalog. The subscription backend does not serve `/responses/compact`. |
| `github` | GitHub Copilot | subscription (device code) | Chat | yes | Also exposes Responses and Messages URLs; a Copilot-specific token refresh is handled per request. |
| `openai` | OpenAI | API key | Chat | yes | Responses and compaction available; `store:false` is enforced for Codex traffic. |
| `anthropic` | Anthropic | API key | Messages | yes | Exact token counting via `/v1/messages/count_tokens`. |
| `deepseek` | DeepSeek | API key | Chat | yes | |
| `gemini` | Google Gemini | API key | Chat (OpenAI compatibility layer) | yes | |
| `groq` | Groq | API key | Chat | yes | Very fast small models; no Responses endpoint. |
| `openrouter` | OpenRouter | API key | Chat | yes | Aggregator: the widest model catalog, priced per model. |
| `xai` | xAI | API key | Chat | yes | |
| `mistral` | Mistral | API key | Chat | yes | |
| `together` | Together AI | API key | Chat | yes | |
| `cerebras` | Cerebras | API key | Chat | yes | |
| `ollama` | Ollama (local) | none | Chat | yes | `base_url_override` pointing at a remote Ollama works the same way. |

## Dialect matrix

Non-chat endpoints follow the same rule: a provider that declares one gets the client body
forwarded (`/v1/images/generations`, `/v1/audio/speech`, `/v1/audio/transcriptions`,
`/v1/audio/voices`); every other provider answers `501` for that path without calling upstream.

| Client → provider | Chat | Responses | Messages | Embeddings |
| --- | --- | --- | --- | --- |
| Chat provider | pass-through | translated | translated | forwarded |
| Responses provider (Codex/OpenAI) | translated + reassembled | pass-through | translated | 501 |
| Messages provider (Claude/Anthropic) | translated | translated | pass-through | 501 |

"Pass-through" means the body is forwarded as the client wrote it, with only routing fields
patched, so provider-native cache markers and tool definitions survive.

## Adding a provider

1. Add a `ProviderSpec` in `crates/rrouter-auth/src/registry.rs` (id, display, kind, base URL,
   optional Responses/Messages URLs, headers, sign-in flow or API-key auth).
2. Model listing derives from the base URL (`models_url` in `crates/rrouter-gateway/src/models_registry.rs`),
   so a standard OpenAI-compatible server works with no extra code.
3. Run `cargo test -p rrouter-auth -p rrouter-gateway`; the registry tests assert that every
   provider has a valid spec.

OAuth providers additionally need a `LoginFlow` (PKCE with a fixed or dynamic loopback port, or
a device-code flow) and, if the provider reports an account label, a `profile_url`.
