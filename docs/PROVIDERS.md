# Providers

One row per provider this build can route to. "Native" is the endpoint the provider speaks
without translation; anything else is translated through the canonical request (Anthropic
clients can call a Codex account, OpenAI clients get OpenAI-shaped streams back).

| Id | Display | Auth | Native | Model list | Notes |
| --- | --- | --- | --- | --- | --- |
| `claude` | Claude (Anthropic OAuth) | subscription (PKCE) | Messages | yes | Request must claim a current Claude Code version; a stale one is refused with "version 2.1.280 or newer is required". Callback host is `localhost`. Native web search through the Messages `web_search` tool. |
| `codex` | Codex (ChatGPT OAuth) | subscription (PKCE, fixed port 1455) | Responses | **no** | Stream-only: non-streaming clients are served by reassembling the stream. Its five verified ids come from the built-in catalog. The subscription backend does not serve `/responses/compact`. Responses also exposes native web search. |
| `github` | GitHub Copilot | subscription (device code) | Chat | yes | Also exposes Responses and Messages URLs; a Copilot-specific token refresh is handled per request. |
| `google-antigravity` | Google Antigravity | subscription (PKCE, dynamic loopback port) | Google Code Assist (`/v1internal:*`) | yes (project-scoped `POST`) | Sign-in returns a Google token; the account's Cloud project is discovered via `loadCodeAssist` (onboarding when absent) and stored as `credentials.extra.project_id`, which every CCA envelope carries. `POST /api/models/fetch` posts `v1internal:fetchAvailableModels` with that project and stores the catalogue's ids/`displayName` as upstream rows. |
| `qoder` | Qoder | operator-supplied device token | Qoder agent (`/algo/api/v2/service/pro/sse/agent_chat_generation`) | **no** (`/algo/api/v2/model/list` is COSY-signed and per account) | Stream-only, COSY-signed and body-obfuscated. The connection supplies `credentials.api_key` (`dt-…`), `credentials.extra.user_id` and optionally `extra.machine_id`; there is no OAuth flow. The account's catalog is fetched and cached by the gateway, and a model it does not list is refused rather than silently served as another model. See [Qoder](#qoder-api3qodersh). |
| `openai` | OpenAI | API key | Chat | yes | Responses and compaction available; `store:false` is enforced for Codex traffic. Native web search through Responses. |
| `anthropic` | Anthropic | API key | Messages | yes | Exact token counting via `/v1/messages/count_tokens`. Native web search through the Messages `web_search` tool. |
| `opencode` | OpenCode | API key | Chat | yes | Paid Console base `https://opencode.ai/inference/openai/v1`; Go plans use the separate `opencode-go` provider. |
| `opencode-go` | OpenCode Go | API key | Chat; Responses | yes | Go subscription base `https://opencode.ai/inference/go/openai/v1`; use `opencode-go/<model-id>`. Separate base URL from paid OpenCode; every request carries a stable `x-opencode-session` derived from the conversation. The Anthropic lane is intentionally not declared because it authenticates with `x-api-key`, so Messages clients translate through Chat. |
| `deepseek` | DeepSeek | API key | Chat | yes | |
| `gemini` | Google Gemini | API key | Chat (OpenAI compatibility layer) | yes | Native search uses `generateContent` + `google_search`; auth is `x-goog-api-key` there. |
| `groq` | Groq | API key | Chat | yes | Very fast small models; no Responses endpoint. |
| `openrouter` | OpenRouter | API key | Chat | yes | Aggregator: the widest model catalog, priced per model. |
| `xai` | xAI | API key | Chat; Responses for search | yes | Native search uses `/v1/responses` + `web_search`. |
| `mistral` | Mistral | API key | Chat | yes | |
| `together` | Together AI | API key | Chat | yes | |
| `cerebras` | Cerebras | API key | Chat | yes | |
| `ollama` | Ollama (local) | none | Chat | yes | `base_url_override` pointing at a remote Ollama works the same way. |
| `azure` | Azure OpenAI | API key (`api-key` header) | Chat (v1 surface) | **no** | Requires a per-connection base URL (`https://<resource>.openai.azure.com`); the deployment name is the model id, registered as a custom model. Uses `max_completion_tokens`. |
| `cohere` | Cohere | API key | Chat (OpenAI-compatibility surface) | derived | Chat goes through Cohere's compatibility API; rerank is `https://api.cohere.com/v1/rerank`. Chat and rerank model ids differ, so each surface needs its own custom rows. |
| `fireworks` | Fireworks AI | API key | Chat | derived | Ids look like `accounts/fireworks/models/<name>` and are stored verbatim; the API documents `max_tokens`. |
| `jina` | Jina AI | API key | — (rerank only) | **no** | No chat API: the entry exists for `https://api.jina.ai/v1/rerank`, and a chat request aimed at it is rejected by Jina. |

**Remote OAuth.** A gateway in a cluster or on a remote VPS cannot receive a browser
callback on its own loopback. The sign-in dialog never opens a browser automatically:
it shows the authorize URL, an **open** button and a **copy** button, and the operator
pastes the final browser URL back. Every PKCE provider advertises this mode: Codex uses
its fixed port 1455, Google Antigravity uses a dynamic loopback port, and Claude uses the stable manual
callback `http://localhost:54545/callback`. Device-code sign-in (GitHub Copilot) needs no
callback and works unchanged.

The last four are reviewed community manifests (`providers/community/*.json`) compiled into
the binary at build time, so they appear here exactly as the built-in rows do. `derived` means
the gateway derives the list URL from the chat endpoint because the provider declares none; a
failed fetch leaves the catalog rows untouched and is not an error the operator sees.

Non-chat endpoints follow the same rule: a provider that declares one gets the client body
forwarded (`/v1/rerank`, `/v1/files`, `/v1/batches`, `/v1/images/generations`,
`/v1/audio/speech`, `/v1/audio/transcriptions`, `/v1/audio/voices`); every other provider
answers `501` for that path without calling upstream.

### Native web search

`POST /v1/search` uses the provider's own search tool; the gateway does not fetch or rank
pages itself. Search is routed like chat (priority, strategy, key allowlist, team scope,
breaker, retries), and a provider that cannot search is removed from the plan rather than
being sent a guessed tool.

| Provider | Request shape | Default when the client sends the provider id |
| --- | --- | --- |
| `claude`, `anthropic` | Messages `tools: [{"type":"web_search_20250305","name":"web_search"}]`; `allowed_domains` supported | `claude-sonnet-4-5` |
| `openai`, `codex` | Responses `tools: [{"type":"web_search"}]`; `filters.allowed_domains` supported | `gpt-5-mini`, `gpt-5.6-luna` |
| `xai` | Responses `tools: [{"type":"web_search"}]`; domain filters supported | `grok-4` |
| `gemini` | `generateContent` with `tools: [{"google_search":{}}]`; domain filters are stated in the prompt | `gemini-2.5-flash` |

The client's `max_results` caps the citations returned, not the number of searches the
model may run. Tokens land in the normal usage row; the search tool's own per-call fee is
not priced by the gateway. `GET /v1/models/search` lists providers that are connected and
search-capable.

## Google Antigravity (Cloud Code Assist)

Antigravity is the Google desktop client's Cloud Code Assist backend, not the Gemini API:
it authenticates a **Google account** over OAuth and speaks a Google-internal envelope
(`/v1internal:generateContent`, `/v1internal:streamGenerateContent?alt=sse`). A Gemini API
key cannot replace it, and the CCA backend is only available to accounts with Antigravity /
Code Assist access.

| Item | Behaviour |
| --- | --- |
| Sign-in | Authorization code + PKCE with the **built-in Antigravity public desktop client** (the same client 9router and the IDE ship). No OAuth client registration is needed. `GOOGLE_ANTIGRAVITY_CLIENT_ID` + `GOOGLE_ANTIGRAVITY_CLIENT_SECRET` optionally replace it as a pair. The callback is a dynamic loopback port with path `/callback`; the browser must reach this host, or use manual/remote callback mode. |
| Project | Every envelope needs a Cloud project id. After the token exchange the gateway calls `POST /v1internal:loadCodeAssist`; if that names no project it onboards (`onboardUser`, up to five polls, two seconds apart). The result is stored once as `credentials.extra.project_id`. A missing project fails sign-in with a message naming Antigravity/Code Assist — it never connects an unusable account. |
| Auth storage | Access and refresh tokens are encrypted at rest like every OAuth account. The public client secret is compiled in (installed-app secrets are not confidential); an environment override is read at each sign-in and refresh. Secrets are never logged or returned to a client. |
| Model ids | There is no `GET /models`. `POST /api/models/fetch` (and the background fetch after sign-in) posts `POST /v1internal:fetchAvailableModels` to the production Cloud Code Assist host with `{project}` and the account token, then stores every `models[id].displayName` as an `upstream` row under `google-antigravity`. `quotaInfo` is read past, not persisted: quota belongs to the account usage surface. Models may also be added manually with `POST /api/models/custom`; the request `model` may be the bare id or `google-antigravity/<id>`. |
| Envelope | `{model, userAgent: "antigravity/hub/2.24.0 darwin/arm64", requestType: "agent", project, requestId: "agent-<uuid>", request: {contents, systemInstruction, generationConfig, tools?}}`; headers include that User-Agent, `X-Goog-Api-Client` and `x-request-source: local` on project discovery. The nested `request` carries no model and no safety settings. |
| Responses | Both buffered and SSE bodies are `{"response": {candidates, usageMetadata}, "traceId"}`; the gateway unwraps `response` (and also accepts a bare Gemini body). Text, reasoning (`thought: true`), function calls and usage (`usageMetadata`) are translated to the canonical stream. |
| Limits | Chat and streaming only. No embeddings, rerank, files, batches, images or audio endpoints; those paths answer `501` for this provider. The daily host is used first with the production host as a fallback (`RROUTER_ANTIGRAVITY_PROD_HOST` / `RROUTER_ANTIGRAVITY_DAILY_HOST` override both). Non-streaming clients are served normally; there is no stream-only restriction. |

### Offline acceptance

CI has no Google account, so the transport and sign-in contract are exercised against a
local mock (no live network):

```sh
cargo build --release
RR=target/release/rrouter python3 scripts/antigravity_smoke.py
python3 scripts/antigravity_smoke.py --check-backend   # route presence only
```

The harness mocks the token endpoint, `loadCodeAssist` (three project shapes), the
onboarding poll, and a CCA SSE/buffered server; it asserts the token-exchange form, envelope
fields, `response.candidates` unwrapping, usage mapping, refresh single-flight, missing-project
refusal, and that neither tokens nor secrets appear in logs or the database. It exits `3`
with `BLOCKED-BY-BACKEND` if the provider routes are absent, and `4` if the fixed callback
callback port cannot be bound.

### Live manual checklist (browser)

1. **Add provider → Google Antigravity**; the dialog opens Google's consent page. Approve
   the Cloud Platform, userinfo, cclog and experiments/config scopes.
2. Let the browser return to the dynamic `localhost:<port>/callback` shown in the dialog; the dashboard connection
   appears with the Google **email account label** and no token.
3. Confirm the account is active and (if you need several accounts) that only one is
   active at a time while testing.
4. Add a custom model with the CCA id you intend to use, then send one non-streaming and
   one streaming test request from the Playground; expect text and token usage, and a
   `401`-free result.
5. Negative check: sign in with an account that has no Antigravity/Code Assist access; the
   connection must fail with the "no Cloud project" message rather than connecting.
6. Inspect the connection view: no access token, refresh token or client secret is shown.
   Do not paste tokens into issues; the repo has no fixture containing real credentials.

## Qoder (api3.qoder.sh)

Qoder is a subscription coding agent whose inference call is signed per request, not
bearer-authenticated. RRouter implements the transport but **not** a sign-in flow: the
operator supplies the credential material, which qodercli already holds on the machine
where it is signed in.

| | |
| --- | --- |
| Credential | `credentials.api_key` is the device access token (`dt-…`); `credentials.extra.user_id` is the account's stable Qoder user id. COSY signing needs both, and a wrong user id signs every request wrongly. `credentials.extra.machine_id` is optional. |
| Where qodercli keeps it | `~/.qoder/.auth` (Windows: `%USERPROFILE%\.qoder\.auth`) holds the signed-in account record — token and user id; `~/.qoder/.qoder/machineid` holds the per-install machine UUID. RRouter never writes either file. |
| Signing | COSY: a fresh AES-128 key per request sealed with RSA PKCS#1 v1.5, an AES-CBC blob holding the user id and token, and an MD5 signature over `payload …, cosyKey …, timestamp …, body, sigPath`, plus ~17 `Cosy-*` fingerprint headers. The signature covers the body **bytes**, so every attempt (retry, fallback) is re-encoded and re-signed. `Cosy-Date` is unix seconds. |
| Body | Base64 → thirds reorder → custom-alphabet substitution, sent with `&Encode=1`. Obfuscation for the edge WAF, not encryption. |
| `pt-` tokens | A personal access token cannot sign COSY requests; it has to be exchanged for a job token on a different host first. That exchange is not implemented, and a `pt-` token fails loudly at signing time. |
| Models | `GET /algo/api/v2/model/list` is per account. The gateway fetches it once per account (1 h), uses the account's own `model_config` block in every envelope and refuses a model the catalog does not list: the chat endpoint validates nothing and would silently answer as a different model. |
| Limits | The endpoint is SSE-only, so the gateway always streams upstream and reassembles for non-streaming clients. Responses are never cached (per-request ids plus a timestamp-bound signature). |
| Errors | A `200` answer can still be a failure: the first envelope frame carries `statusCodeValue`. Code `10605` (`RESOURCE_LIMIT`) is a model queue — retryable, honoring `retryAfterSeconds`; codes `110`/`112` (or a `pricingUrl`) are quota blocks and take the gateway's existing quota-exhausted path (`429`, candidate cooled down). |

Models are routed as `qoder/<key>` (or the bare key when the connection is unambiguous);
`<key>` is the catalog's own id (`auto`, `qmodel`, `smodel`, …).

## Quota auto-ping (Claude / Codex)

Subscription plans expose rolling windows (Claude's 5-hour session, Codex's primary
window). A tiny request starts a new window, so an account can be warmed just after its
current window resets. The Providers page exposes four modes per Claude/Codex OAuth account:

| Mode | Behaviour |
| --- | --- |
| `off` | Never ping. |
| `reset` | Ping five seconds after the current window's `resets_at`. |
| `schedule` | Ping at fixed local wall-clock slots: anchor, IANA timezone, window hours, slots/day. |
| `smart` | Record usage samples and stagger accounts by recent traffic. The busiest account pings at its reset; the others follow phase offsets across one window. Idle accounts (no requests in 24h and no current usage) are skipped. |

The scheduler runs every 60 seconds. It refreshes credentials and subscription usage,
sends at most one tiny request per due reset/slot, and records `last_ping_at`,
`last_ping_reset_at`, `last_ping_slot` and `last_error`. A failed ping backs off for 15
minutes and never fails a client request. Pings are operational: they create no usage row
and no API-key traffic.

Claude uses a 1-token Haiku Messages request; Codex uses a streamed Responses request with
the CLI account header and `store:false`. API-key accounts have no subscription window and
are not listed. Usage samples are kept for 30 days.

## Dialect matrix

| Client → provider | Chat | Responses | Messages | Embeddings |
| --- | --- | --- | --- | --- |
| Chat provider | pass-through | translated | translated | forwarded |
| Responses provider (Codex/OpenAI) | translated + reassembled | pass-through | translated | 501 |
| Messages provider (Claude/Anthropic) | translated | translated | pass-through | 501 |

"Pass-through" means the body is forwarded as the client wrote it, with only routing fields
patched, so provider-native cache markers and tool definitions survive.

## Adding a provider

1. For an API-key provider, add a reviewed manifest in `providers/community/<id>.json`
   (schema: `providers/schema.json`); `crates/rrouter-auth/build.rs` compiles it into the
   binary. Built-in providers — and anything needing an OAuth flow — are a `ProviderSpec` in
   `crates/rrouter-auth/src/registry.rs` (id, display, kind, base URL, optional
   Responses/Messages/rerank/files/batches URLs, headers, sign-in flow or API-key auth).
2. Model listing derives from the chat base URL unless the spec declares `models_url`, so a
   standard OpenAI-compatible server works with no extra code; the derivation itself is
   `models_url` in `crates/rrouter-gateway/src/models_registry.rs`.
3. If the provider documents a hosted web-search tool, add it to `search_spec` in
   `crates/rrouter-auth/src/registry.rs` and a request/response adapter in
   `crates/rrouter-translate/src/search.rs`; until then `/v1/search` answers `501` for it.
4. Run `cargo test -p rrouter-auth -p rrouter-translate -p rrouter-gateway`; the registry
   tests assert that every provider has a valid spec and every search provider has an
   endpoint for its mode.

OAuth providers additionally need a `LoginFlow` (PKCE with a fixed or dynamic loopback port, or
a device-code flow) and, if the provider reports an account label, a `profile_url`.

### Remote sign-in (manual callback)

A loopback listener only works when the browser and the gateway share a host. On a remote
VPS the provider redirects to the *browser's* localhost, so the gateway never receives the
code. Start the sign-in with `{"manual":true}`: the gateway builds the registered redirect
URI from the provider spec without binding anything, and `complete` accepts the final URL
the browser landed on:

```text
POST /api/oauth/{provider}/start       {"manual":true}
  -> {"id":"…","flow":"pkce","challenge":{"kind":"pkce","authorize_url":"…",
      "redirect_uri":"http://localhost:54545/callback","manual":true}}

POST /api/oauth/{provider}/complete    {"id":"…","callback_url":"http://localhost:54545/callback?code=…&state=…"}
  -> {"id":"<connection>","provider":"…","name":"…","warning":null}
```

Every PKCE provider supports manual mode. A provider with a fixed registered port uses it;
Claude accepts a stable manual callback at `http://localhost:54545/callback`. The pasted
URL's `state` is checked against the pending login, and the code is exchanged at most once
even if both a loopback waiter and a pasted callback race. The dashboard always starts
PKCE sign-ins in manual mode and requires an explicit **open** or **copy** action.

Callback pages use a built-in light/dark card with the RRouter wordmark. When
`RROUTER_PUBLIC_URL` is a safe absolute URL (`https://…`, or loopback `http://`), the success
and failure pages link back to `<url>/app`; credentials, query strings and fragments in that
value are rejected. No external assets are loaded and no token material is rendered.

Client credentials and endpoints are overridable without a rebuild:

| Variable | Meaning |
| --- | --- |
| `GOOGLE_ANTIGRAVITY_CLIENT_ID` / `GOOGLE_ANTIGRAVITY_CLIENT_SECRET` | **Required** for Google Antigravity: register your own Google OAuth desktop client; no credential is shipped in this repository. Read at sign-in/exchange/refresh, never logged or returned. |
| `RROUTER_OAUTH_CLIENT_ID_<PROVIDER>` / `RROUTER_OAUTH_CLIENT_SECRET_<PROVIDER>` | Generic fallback, provider id upper-cased with `-` → `_`. |
| `RROUTER_OAUTH_OVERRIDE_<PROVIDER>_AUTHORIZE` / `_TOKEN` / `_DEVICE` | Sign-in endpoints (HTTPS only; loopback HTTP needs `RROUTER_ALLOW_INSECURE_OAUTH=1`). |
| `RROUTER_ANTIGRAVITY_DAILY_HOST` / `RROUTER_ANTIGRAVITY_PROD_HOST` | CCA daily and fallback origins; also used for project discovery. |
| `RROUTER_PUBLIC_URL` | Public dashboard origin used by the OAuth callback pages' "Return to dashboard" link; unset omits the link. |
