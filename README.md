# RRouter

A Rust-native local AI gateway: one endpoint that every coding agent ([CC], Codex CLI, Cline,
Cursor, OpenCode, aider) points at. Each request goes to the provider/account most likely to
serve it right now, with wire-protocol translation on the fly.

Fast and boring on purpose: no Next.js, no JS runtime, no dashboard build step, one binary,
one SQLite file.

## Design bar

Inspired by [9router](https://github.com/decolua/9router)'s feature set — OAuth subscription
accounts, round-robin across accounts, automatic fallback. Not a reimplementation of it: every
layer is rebuilt against a stricter bar.

1. **Correctness first** — exactly one token-refresh path, proven by concurrent tests;
   byte-level SSE framing that cannot desync.
2. **Security as a default, not an option** — credentials AES-256-GCM at rest (per-write nonce,
   authenticated), gateway keys stored only as SHA-256, secrets never echoed through the admin
   API, loopback bind by default.
3. **Latency is the product** — zero-copy streaming; fallback decided in microseconds (no
   sleeping on 429/5xx while another candidate is eligible); half-open breakers so a recovered
   account is used again immediately instead of waiting out a timer.
4. **Lightweight deployment** — a single `rrouter` binary with embedded UI and embedded
   SQLite: no npm, no Node, no build pipeline at deploy time, millisecond cold start.

What that means concretely versus the Node implementation:

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
./target/release/rrouter            # 127.0.0.1:20128, data/ created on first run
open http://127.0.0.1:20128         # dashboard: add a provider connection, copy an API key
```

Recurring account setup lives in the dashboard. From the CLI it is two calls:

```sh
# store an [OI]-compatible provider key (encrypted at rest immediately)
curl -s localhost:20128/api/connections -H 'content-type: application/json' -d '{
  "provider":"openai","auth_type":"api_key","name":"personal",
  "credentials":{"api_key":"sk-..."}}'

# optional gateway key: clients then need "Authorization: Bearer sk-rr-..."
curl -s -X POST localhost:20128/api/keys -d '{"name":"laptop"}'
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

## Dashboard

`GET /` serves a dashboard embedded in the binary — no build step, no CDN, no internet access
required. It is an *extension surface*, not a fixed page: the client composes itself from
`GET /api/ui/panels`, which lists the built-in panels (Connections, API keys, Aliases, Combos,
Usage, Health) plus any `extension` panels you register. An extension panel is a URL the
dashboard loads in an isolated frame and hands a scoped token, so an organisation can add its
own panels without forking RRouter.

White-label it from `PUT /api/settings`: brand name, accent colour, logo URL, footer text.

## Running it as a self-hosted platform

Everything runs on your own hardware; there is no vendor service in the path and no telemetry.

```sh
RROUTER_HOST=0.0.0.0 \
RROUTER_DATA_DIR=/var/lib/rrouter \
RROUTER_MASTER_KEY=$(openssl rand -hex 32) \
RROUTER_REQUIRE_API_KEY=true \
RROUTER_CORS_ORIGINS=https://ide.internal.example \
RROUTER_ALLOW_REMOTE_ADMIN=true \
rrouter
```

| env | default | |
|---|---|---|
| `RROUTER_HOST` | `127.0.0.1` | bind address |
| `RROUTER_DATA_DIR` | `./data` | SQLite DB + master key file |
| `RROUTER_MASTER_KEY` | unset | 64-hex-char AES key; otherwise `data/master.key` (0600) is generated |
| `RROUTER_REQUIRE_API_KEY` | `false` | require a stored `sk-rr-…` key on `/v1/*` |
| `RROUTER_ALLOW_REMOTE_ADMIN` | `false` | required to bind beyond loopback; the admin API would otherwise be unreachable, so startup fails instead of half-opening |
| `RROUTER_CORS_ORIGINS` | unset | comma-separated allowlist; `*` is rejected at startup |
| `RROUTER_STATIC_DIR` | unset | serve operator-supplied files under `/ext/*` (traversal-safe) |
| `RROUTER_REQUEST_TIMEOUT_SECS` | `600` | upstream request timeout |
| `RROUTER_CONNECT_TIMEOUT_SECS` | `15` | connect timeout |
| `RUST_LOG` | `rrouter=info` | tracing filter |

Admin API: `/api/connections`, `/api/keys`, `/api/aliases`, `/api/combos`, `/api/usage`,
`/api/settings`, `/api/system`, `/api/ui/panels`. No endpoint ever returns token material;
`/api/system` exposes booleans and counters only.

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
| `RROUTER_MASTER_KEY` | unset | 64-hex-char AES key; otherwise `data/master.key` (0600) is generated |
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

## Tests

```sh
cargo test --workspace                             # 110 tests
cargo clippy --workspace --all-targets -- -D warnings
python3 scripts/e2e_smoke.py                       # live binary + mock upstream, 12 checks
```

`scripts/e2e_smoke.py` starts a mock provider and the release binary, then proves: a 401 on
the first account falls through to the next in milliseconds, the first SSE frame reaches the
client before the upstream finishes its stream, an Anthropic client gets Anthropic-shaped
output from an [OI]-speaking provider, credentials are absent from the DB file in plaintext,
the key file is `0600`, and the admin API never returns token material.

## License

MIT. Behavioural inspiration from 9router (also MIT); no code is copied — wire contracts were
re-derived from published behaviour and the providers' own APIs.
