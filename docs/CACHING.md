# Caching

## Exact response cache

`RROUTER_CACHE=sqlite` enables a byte-exact cache for **non-streaming** chat, messages and
responses. The key is a SHA-256 of the canonical request, so equivalent spellings share an
entry. A hit returns the stored body with `x-rrouter-cache: hit` and a fresh `x-request-id`,
and records a usage row with zero tokens/cost. `x-rrouter-cache: bypass` forces a miss.
Streams and errors are never cached. `RROUTER_CACHE_TTL_SECS` and
`RROUTER_CACHE_MAX_ENTRIES` bound it.

## Semantic cache

`RROUTER_SEMANTIC_CACHE=sqlite` adds an embedding-backed layer. The canonical prompt is
embedded by `RROUTER_SEMANTIC_EMBEDDING_MODEL`; a stored response is served when cosine
similarity reaches `RROUTER_SEMANTIC_CACHE_THRESHOLD` (default 0.92) within the same scope
(embedding model, provider, model, team). It is non-streaming chat/messages/responses only
and marks a hit `x-rrouter-cache: semantic-hit`. `RROUTER_SEMANTIC_CACHE_TTL_SECS` and
`RROUTER_SEMANTIC_CACHE_MAX_ENTRIES` bound it.

## Prompt-cache preservation

A gateway can silently destroy provider prompt caching. RRouter avoids two common causes:

- Same-dialect requests are forwarded as the client wrote them. Only routing fields are
  patched; Anthropic `cache_control` breakpoints and OpenAI Responses cache fields survive.
  Cross-dialect requests still go through the canonical translator.
- Accounts are pinned per conversation: equal-priority connections are ordered by a hash of
  the conversation (model, system, first turn, `prompt_cache_key`), not round-robin. KV
  caches live on one upstream account.

Usage records cache reads/writes; Activity shows the hit rate.
