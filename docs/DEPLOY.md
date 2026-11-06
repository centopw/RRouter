# Running RRouter in production

Short version: one process, one data directory, SQLite inside. No Redis, no Postgres, no
sidecars. This page covers the parts an operator actually needs: exposure, secrets, backups,
upgrades, and the failure modes that look like gateway bugs but are not.

## 1. Install

Pick one:

| Method | Command |
| --- | --- |
| Installer | `curl -fsSL https://raw.githubusercontent.com/centopw/RRouter/main/install.sh \| sh` |
| Release tarball | download `rrouter-<os>-<arch>.tar.gz` + `.sha256`, verify, unpack to `/usr/local/bin` |
| Docker | `docker compose up -d` (see `docker-compose.yml`) |
| Kubernetes | `kubectl apply -f deploy/k8s.yaml` (one replica, PVC, probes) |
| From source | `cargo build --release`, copy `target/release/rrouter` |

Keep the binary and the data directory on the same host. Do not run two processes against
one SQLite file.

## 2. Configure

Every setting is an environment variable; `rrouter --print-config` prints the effective
configuration with secrets redacted. The ones that matter in production:

| Variable | Why |
| --- | --- |
| `RROUTER_HOST` | Keep `127.0.0.1` and proxy through Caddy/nginx/Traefik. Binding a public address requires `RROUTER_ALLOW_REMOTE_ADMIN=true` and an authenticating proxy in front. |
| `RROUTER_PORT` | Default `20128`. |
| `RROUTER_DATA_DIR` | SQLite database, `master.key`, `/ext` assets. Back this up. |
| `RROUTER_MASTER_KEY` | 64 hex chars. If unset, a key file is generated in the data dir. Prefer the key file for local installs and the variable when the platform stores secrets. |
| `RROUTER_REQUIRE_API_KEY` | `true` for anything beyond a trusted network. |
| `RROUTER_CORS_ORIGINS` | Only needed if browsers call the gateway from another origin; `*` is rejected at startup. |
| `RROUTER_REQUEST_TIMEOUT_SECS` | Per-attempt upstream timeout. Streaming responses are not cut by this once headers arrive. |

The admin API (`/api/*`, `/metrics`, the dashboard) is gated by the socket address: it
answers only loopback callers unless `RROUTER_ALLOW_REMOTE_ADMIN=true`. A reverse proxy on
the same host connects from loopback, so **the proxy makes the admin API reachable**. Protect
`/api/*` and `/metrics` there (basic auth, OIDC, IP allowlist). `deploy/Caddyfile.example` does
this.

## 3. Exposure

Put the gateway behind TLS and never publish the port directly:

```
internet -> CDN/WAF (optional) -> Caddy/nginx (TLS, auth for /api) -> 127.0.0.1:20128
```

Remote OAuth sign-in: the provider redirects to `http://localhost:<port>/callback` on the
machine running the browser. When the dashboard is remote, forward that port first
(`ssh -L <port>:localhost:<port> gateway-host`) — the wizard prints the exact address — or
connect the account from a browser on the gateway host. A paste-the-URL fallback is not
implemented yet.

For streaming endpoints the proxy must not buffer responses (`flush_interval -1` in Caddy;
`proxy_buffering off` in nginx) and must allow long reads (15 minutes is a safe ceiling).
Compression is fine for JSON; SSE passes best unmodified, but gzip/zstd streams are still
chunked correctly by RRouter.

## 4. Backups

The database is the only state that matters (connections are encrypted with `master.key`;
API keys are hashes; usage rows are accounting).

```sh
RROUTER_DATA_DIR=/var/lib/rrouter ./scripts/backup.sh /var/backups/rrouter/$(date -u +%F)
```

The script uses `sqlite3 "VACUUM INTO"` so the copy is consistent while the gateway runs,
and writes `SHA256SUMS`. Restore: stop the service, replace `rrouter.db` and `master.key` in
the data directory, start it. If the deployment uses `RROUTER_MASTER_KEY` instead of the key
file, the key is not in the snapshot — keep it in the secret store.

Losing `master.key` makes stored provider credentials unrecoverable; it is the one file to
keep safe. Losing the database loses usage history and key hashes only.

### Verify a restore

A backup you have never restored is a guess. Rehearse it on the same host:

```sh
./scripts/backup.sh /tmp/restore-drill
mkdir -p /tmp/restore-data && cp /tmp/restore-drill/{rrouter.db,master.key} /tmp/restore-data/
RROUTER_DATA_DIR=/tmp/restore-data RROUTER_PORT=20131 ./rrouter &
curl -s localhost:20131/api/connections | head -c 300   # has_access_token must be true
```

That `has_access_token: true` is the proof the snapshot and the key match: the credentials
decrypted. Stop the drill instance and remove `/tmp/restore-data` afterwards.

## 5. Upgrades

1. Back up (§4).
2. Replace the binary (or pull the new image) and restart.
3. Migrations run automatically at startup and are forward-only; the schema version is
   `PRAGMA user_version`.

A rollback to an older binary against a newer schema is not supported — restore the backup
instead. Tags are immutable: pin a version and upgrade deliberately.

## 6. What healthy looks like

- `rrouter --healthcheck` exits 0 (the container image runs it every 30 s).
- `GET /health` → `{"ok":true,"version":"…","uptime_secs":…}`.
- The dashboard's **Health** page shows every connection; a degraded one carries
  `last_error` and a backoff level.
- Logs: `RUST_LOG=rrouter=info` is quiet; use `rrouter=debug` for routing decisions.

## 7. Troubleshooting

## Monitoring

`/metrics` is Prometheus text on the admin gate (loopback, same rule as `/api`). Scrape it from
the same host, or authenticate at the proxy:

```yaml
scrape_configs:
  - job_name: rrouter
    static_configs:
      - targets: ["127.0.0.1:20128"]
```

Queries that answer the questions operators actually ask:

```promql
# requests per second, split by the status class the client saw
sum by (status) (rate(rrouter_requests_total[5m]))

# upstream fallbacks: attempts exceed requests when an account failed first
sum(rate(rrouter_attempts_total[5m])) - sum(rate(rrouter_requests_total[5m]))

# p95 latency
histogram_quantile(0.95, sum by (le) (rate(rrouter_request_duration_seconds_bucket[5m])))

# accounts currently fenced by the breaker
sum(rrouter_breaker_open)
```

Alert on a sustained gap between attempts and requests (fallback storm) and on
`rrouter_breaker_open > 0` for more than a minute.

| Symptom | Likely cause |
| --- | --- |
| Dashboard loads, `/api/*` answers 403 | Admin gate: the request did not arrive over loopback. Bind `127.0.0.1` and proxy from the same host, or set `RROUTER_ALLOW_REMOTE_ADMIN=true` deliberately. |
| `/v1/*` answers 401 | `RROUTER_REQUIRE_API_KEY=true` and the client sent no key or a key that does not exist. |
| One provider returns 429 while another works | Account-level rate limit. The breaker backs that account off; a Combo or a second account keeps traffic moving. |
| Requests hang, then time out | Upstream connect timeout (`RROUTER_CONNECT_TIMEOUT_SECS`) or a proxy in front buffering/capping the response. |
| `database is locked` under load | Two processes on one database file, or an external `sqlite3` session holding a write lock. |
| Container healthy but `/data` not writable | A host bind mount owned by another user. Use the named volume from `docker-compose.yml` or `chown` the mount to uid 65532. |

## 8. Security checklist

- [ ] TLS terminates at the proxy; `RROUTER_HOST=127.0.0.1`.
- [ ] `/api/*` and `/metrics` require authentication at the proxy.
- [ ] `RROUTER_REQUIRE_API_KEY=true` for shared deployments; give each team its own key.
- [ ] Per-key limits set (rpm, tpm, budget, allowed models, expiry) for keys handed to others.
- [ ] `RROUTER_CORS_ORIGINS` is empty unless a browser must call the gateway cross-origin.
- [ ] Backups run on a schedule and a restore has been rehearsed.
- [ ] `master.key` is `0600` and excluded from world-readable backups.
