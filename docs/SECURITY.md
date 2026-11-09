# Security

- Provider credentials are AES-256-GCM encrypted with a unique nonce per write. The key is
  `<data>/master.key` created `0600`, or `RROUTER_MASTER_KEY`.
- Gateway keys are stored as SHA-256 hashes; plaintext is shown once.
- The admin API never returns token material, only `has_*` booleans.
- `RROUTER_ALLOW_REMOTE_ADMIN=false` by default. Admin routes are loopback-only unless the
  operator enables remote admin and protects them with `RROUTER_ADMIN_TOKEN` or a proxy.
  `RROUTER_CORS_ORIGINS=*` is rejected at startup.
- Dashboard accounts use argon2id. Session cookies, recovery codes and invitation tokens are
  stored only as hashes. The owner cannot be demoted, disabled or deleted. Cookie writes
  require `x-rrouter-csrf`; sign-in is rate-limited; the setup code is printed once.
- Sessions record resolved IP, bounded User-Agent and a derived device label. Forwarded IPs
  are only as trustworthy as the proxy that sets them.
- Upstream payloads are never persisted. Usage rows record tokens, latency, status and model,
  not prompts or completions.

## Checklist

- [ ] TLS terminates at the proxy; `RROUTER_HOST=127.0.0.1`.
- [ ] `/api/*` and `/metrics` require authentication at the proxy or an admin token.
- [ ] `RROUTER_REQUIRE_API_KEY=true` for shared deployments; one key per team/client.
- [ ] Per-key rpm/tpm/budget/allowed-models/expiry set for keys handed to others.
- [ ] `RROUTER_CORS_ORIGINS` empty unless a browser must call cross-origin.
- [ ] Backups run and a restore has been rehearsed.
- [ ] `master.key` is `0600` and excluded from world-readable backups.
- [ ] In Kubernetes/Helm, the Ingress authenticates `/api/*` and `/metrics`.
