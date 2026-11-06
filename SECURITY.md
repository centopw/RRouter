# Security

## Reporting

Email the maintainer at the address on the GitHub profile, or open a
[private security advisory](https://github.com/centopw/RRouter/security/advisories/new).
Please do not file a public issue for a vulnerability. Include the version, a minimal
reproduction, and the impact you observed. Acknowledgement within 72 hours.

## Supported versions

The latest `v0.*` release and `main`. Older tags receive fixes only for critical issues.

## What RRouter stores

| Data | Where | Protection |
| --- | --- | --- |
| Provider credentials (OAuth tokens, API keys) | SQLite `connections.data` | AES-256-GCM, unique nonce per write |
| Master key | `<data dir>/master.key` (0600) or `RROUTER_MASTER_KEY` | file permissions / secret store |
| Gateway keys | SQLite `api_keys.key_hash` | SHA-256, plaintext shown once |
| Usage | SQLite `usage`, `attempts` | model, tokens, latency, status — never prompts or completions |
| Prompts / responses | not stored | streamed through and dropped |

`GET /api/*` never returns token material; only `has_*` booleans. `/health` is unauthenticated
and exposes version and uptime only.

## Threat model

- **The client API (`/v1/*`) is the exposed surface.** With `RROUTER_REQUIRE_API_KEY=true` every
  request needs a gateway key; per-key limits (rpm, tpm, budget, allowlist, expiry) bound what a
  leaked key can cost.
- **The admin API has no user login.** It is gated by the socket address: loopback callers are
  trusted. A reverse proxy on the same host therefore makes it reachable — authenticate `/api/*`
  and `/metrics` there (see `deploy/Caddyfile.example`). Never publish `20128` directly.
- **A lost `master.key` is worse than a lost database**: it decrypts every provider credential.
  Back it up separately and keep it out of world-readable storage.
- **Upstream payloads are never persisted**, so a database leak does not leak conversations.
- **No telemetry, no outbound calls except the providers you configured** (plus the optional
  models.dev metadata sync you trigger).

## Hardening checklist

See `docs/DEPLOY.md` §8. The short version: TLS at the proxy, loopback bind, authenticated
admin paths, `RROUTER_REQUIRE_API_KEY=true`, per-key limits, scheduled backups, and a rehearsed
restore.
