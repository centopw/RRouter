# RRouter

A Rust rewrite of [9router](https://github.com/decolua/9router) — one local endpoint that
every coding agent ([CC], Codex CLI, Cline, Cursor, OpenCode, aider) points at, routing each
request to the provider/account most likely to serve it right now, translating between wire
protocols on the fly.

Fast and boring on purpose: no Next.js, no JS runtime, no dashboard build step, one binary,
one SQLite file.

## Why a rewrite

| 9router (Node/Next.js) | RRouter (Rust) |
|---|---|
| Next.js app + ~140 route handlers + 85 dashboard files (~19k LoC) to proxy bytes | ~8k LoC across 6 crates, axum + reqwest |
| JS object-heavy SSE parsing; desyncs on chunk splits | byte-level SSE codec, tested at 7-byte chunk boundaries |
| Token refresh races: reactive 401 paths bypass the refresh lock | one refresh path, per-credential mutex + double-check, proven by a 20-task concurrency test |
| Binary model lock on failure (no half-open) | closed / open / half-open breaker per connection, 429 backoff honors `Retry-After` |
| Plaintext OAuth tokens in SQLite | AES-256-GCM at rest, SHA-256 gateway keys, GCM nonce per write |
| `npm i -g`, ~200 MB runtime | static-ish single binary, cold start in milliseconds |

## Quick start

```sh
cargo build --release
./target/release/rrouter            # listens on 127.0.0.1:20128 (same port as 9router)
```

Point a client at it:

```sh
export ANTHROPIC_BASE_URL=http://127.0.0.1:20128
export ANTHROPIC_AUTH_TOKEN=sk-rr-...      # only if RROUTER_REQUIRE_API_KEY=true
cc -m "claude/sonnet-4-5"                  # or any alias/combo you defined
```

```sh
curl -s localhost:20128/v1/chat/completions \
  -H 'content-type: application/json' \
  -d '{"model":"github/gpt-5","messages":[{"role":"user","content":"hi"}]}'
```

## Endpoints

Client-facing (OpenAI- and Anthropic-compatible):

| path | notes |
|---|---|
| `POST /v1/chat/completions` | OpenAI Chat Completions in, any provider out |
| `POST /v1/messages` | Anthropic Messages in, any provider out |
| `POST /v1/responses` | OpenAI Responses in |
| `POST /v1/embeddings` | passthrough for OpenAI-compatible providers |
| `GET /v1/models` | union of catalog, aliases, combos, connected providers |
| `GET /health` | liveness + version |

Admin API (loopback-bound): `/api/connections`, `/api/keys`, `/api/aliases`,
`/api/combos`, `/api/usage`. The UI at `/` drives the same API.

## Model strings

- `provider/model` — explicit, e.g. `claude/sonnet-4-5`, `github/gpt-5`, `deepseek/deepseek-chat`
- `alias` — an alias you registered, pointing at another model string
- `combo:<name>` — ordered fallback chain; the router walks it until one succeeds

## Configuration

| env | default | |
|---|---|---|
| `RROUTER_PORT` | `20128` | same port as 9router, so existing client configs keep working |
| `RROUTER_HOST` | `127.0.0.1` | bind address |
| `RROUTER_DATA_DIR` | `./data` | SQLite DB + master key file |
| `RROUTER_MASTER_KEY` | unset | 64-hex-char AES key; otherwise `data/.key` (0600) is generated |
| `RROUTER_REQUIRE_API_KEY` | `false` | require a stored `sk-rr-…` key on `/v1/*` |
| `RROUTER_REQUEST_TIMEOUT_SECS` | `600` | upstream request timeout |
| `RROUTER_CONNECT_TIMEOUT_SECS` | `15` | connect timeout |
| `RUST_LOG` | `rrouter=info` | tracing filter |

## Providers

OAuth/subscription transports: `claude`, `codex`, `github` (Copilot).
API-key transports: `openai`, `anthropic`, `deepseek`, `gemini`, `groq`, `openrouter`,
`xai`, `mistral`, `together`, `cerebras`.
Local: `ollama`. Any provider accepts a `base_url_override` per connection.

## How a request flows

```
client ──/v1/messages──► gateway
                          │ auth (optional API key)
                          │ translate → canonical request
                          │ plan: model → candidates (alias, provider/model, combo, priority)
                          ▼
                     dispatcher (circuit breaker, fallback)
                          │ refresher: guarantee ≥5 min of token life (one refresh per credential)
                          │ translate canonical → provider dialect
                          ▼
                     provider HTTP ──SSE bytes──► decoder → canonical events → encoder ──► client
                          │
                          └── usage recorded (tokens, latency, status; never bodies)
```

## Workspace

```
crates/rrouter-core       canonical model, dialects, errors, SSE codec
crates/rrouter-translate  OpenAI Chat · Anthropic Messages · OpenAI Responses  ↔ canonical
crates/rrouter-auth       provider registry, credentials, mutex-locked refresh
crates/rrouter-store      SQLite persistence, AES-256-GCM credentials, keys, usage
crates/rrouter-router     model resolution, fallback, circuit breaker
crates/rrouter-gateway    axum server, /v1 proxy, admin API, embedded UI, `rrouter` binary
```

```sh
cargo test                                          # all crates
cargo clippy --workspace --all-targets -- -D warnings
```

## License

MIT. Upstream 9router is MIT as well; no code is copied — the wire contracts were
re-derived from its published behaviour and the providers' own APIs.
