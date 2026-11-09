# Declarative configuration (v1)

RRouter reads the environment. It can also read one JSON file named by
`RROUTER_CONFIG`, which lets a deployment keep its non-secret settings in version
control while the environment still supplies secrets and per-host overrides.

```
environment variable  >  config file  >  built-in default
```

The file is optional. Without `RROUTER_CONFIG` nothing changes: the binary is
configured exactly as before.

## Why JSON

There is no YAML dependency and no second schema. Every file key maps to one environment
variable, and both go through the same defaults, ranges, clamping and error messages —
so a value behaves identically whichever layer set it.

## Keys

Every key is optional and matches the environment variable in lowercase:

| File key | Environment | Notes |
| --- | --- | --- |
| `data_dir` | `RROUTER_DATA_DIR` | |
| `master_key` | `RROUTER_MASTER_KEY` | **reference only**, see below |
| `require_api_key` | `RROUTER_REQUIRE_API_KEY` | |
| `request_timeout_secs` | `RROUTER_REQUEST_TIMEOUT_SECS` | |
| `connect_timeout_secs` | `RROUTER_CONNECT_TIMEOUT_SECS` | |
| `log_filter` | `RUST_LOG` | |
| `static_dir` | `RROUTER_STATIC_DIR` | |
| `cors_origins` | `RROUTER_CORS_ORIGINS` | array of origins; `*` is refused |
| `allow_remote_admin` | `RROUTER_ALLOW_REMOTE_ADMIN` | |
| `admin_token` | `RROUTER_ADMIN_TOKEN` | **reference only** |
| `max_concurrency` | `RROUTER_MAX_CONCURRENCY` | |
| `max_attempts` | `RROUTER_MAX_ATTEMPTS` | clamped to 1–8 |
| `retry_attempts` | `RROUTER_RETRY_ATTEMPTS` | clamped to 0–5 |
| `retry_base_ms` | `RROUTER_RETRY_BASE_MS` | clamped to 0–5000 |
| `retry_max_ms` | `RROUTER_RETRY_MAX_MS` | clamped to 0–60000 |
| `hedge_ms` | `RROUTER_HEDGE_MS` | clamped to 0–60000; `0` disables hedging; restart-only |
| `canary` | `RROUTER_CANARY` | `model=provider:percent`; empty/`off` disables; reloadable |
| `subscription_reserve_percent` | `RROUTER_SUBSCRIPTION_RESERVE_PERCENT` | 0–100, default 90; `0` disables subscription-aware routing; reloadable |
| `semantic_cache` | `RROUTER_SEMANTIC_CACHE` | `off` (default), `sqlite`; reloadable |
| `semantic_cache_threshold` | `RROUTER_SEMANTIC_CACHE_THRESHOLD` | cosine 0–1, default 0.92 |
| `semantic_embedding_model` | `RROUTER_SEMANTIC_EMBEDDING_MODEL` | `provider/model`; required when the cache is on |
| `semantic_cache_ttl_secs` | `RROUTER_SEMANTIC_CACHE_TTL_SECS` | clamped to 1–86400, default 3600 |
| `semantic_cache_max_entries` | `RROUTER_SEMANTIC_CACHE_MAX_ENTRIES` | clamped to 1–100000, default 1000 |
| `shutdown_grace_secs` | `RROUTER_SHUTDOWN_GRACE_SECS` | `0` waits indefinitely |
| `strategy` | `RROUTER_STRATEGY` | `priority`, `weighted`, `least_latency`, `cheapest` |
| `cache` | `RROUTER_CACHE` | `off`, `sqlite` |
| `cache_ttl_secs` | `RROUTER_CACHE_TTL_SECS` | clamped to 1–86400 |
| `cache_max_entries` | `RROUTER_CACHE_MAX_ENTRIES` | clamped to 1–100000 |
| `usage_retention_days` | `RROUTER_USAGE_RETENTION_DAYS` | clamped to 0–3650 |
| `log_format` | `RROUTER_LOG_FORMAT` | `text`, `json` |
| `access_log` | `RROUTER_ACCESS_LOG` | |

`host` and `port` are **not** file keys: they are recognised only to produce a precise
error. The listen address is fixed when the process starts, and accepting it in a file
would promise a runtime change the gateway cannot apply. Set `RROUTER_HOST` /
`RROUTER_PORT` instead.

OpenTelemetry export settings (`RROUTER_OTEL_ENDPOINT`, `RROUTER_OTEL_SERVICE_NAME`,
`RROUTER_OTEL_SAMPLE_RATIO`) are **environment-only**: they are read only by builds with
the `otel` cargo feature, and the config file loader rejects them as unknown keys rather
than silently ignoring them. See [TELEMETRY.md](TELEMETRY.md) §6.

## Secrets are references

```json
{
  "admin_token": { "key_env": "RROUTER_ADMIN_TOKEN" },
  "master_key":  { "key_file": "/run/secrets/master.key" }
}
```

- `key_env` reads the named environment variable (non-empty);
- `key_file` reads one line of secret material and trims surrounding whitespace, so a
  trailing newline from `echo … > key` is fine; an **embedded** newline is refused with a
  precise error (a multi-line file is a JSON/YAML blob or a key dump, never one secret);
- an inline string such as `"admin_token": "abc"` is **rejected**, and the value is not
  echoed in the error;
- `key_env` and `key_file` are mutually exclusive.

`--print-config` never prints the value: it reports `admin_token: "set"` or `"unset"`
and `master_key` as `"env"` or `"file"` (where the key material comes from).

## Validation

```sh
RROUTER_CONFIG=/etc/rrouter/config.json rrouter --check-config
# config file: /etc/rrouter/config.json
# config ok: 1 from the environment, 2 from the file, 22 default
```

Exit status is `0` for a valid merge and `1` otherwise; the error names the source of the
bad value — `config file /etc/rrouter/config.json: field "port" …` for file problems,
`RROUTER_PORT=…` for environment ones. Refused:

- an unknown key, naming the key and its line;
- a wrong type or out-of-range value, naming the line and column;
- a duplicate key, naming the second occurrence's line (JSON parsers silently keep the
  last one, which hides typos);
- `"host"` / `"port"` in the file;
- `cors_origins` containing `*`;
- a secret reference that is missing, empty, or ambiguous.

```sh
RROUTER_CONFIG=/etc/rrouter/config.json rrouter --print-config
```

prints the effective configuration as JSON plus a `sources` map
(`{"port":"default","cache":"file","strategy":"env"}`) and the resolved `config_file`
(`null` when unset). No secret value is ever printed.

## What v1 does not do

- **No provider/model/key declarations.** Only process settings are covered; connections,
  keys and aliases remain database state managed through the dashboard or API.
- **No reload of startup-bound subsystems.** The listen address, data directory, master
  key, static root, dispatcher retry policy and HTTP client are fixed at startup (table
  below).
- **No migration.** Reload adds no schema version and no stored state.

## Hot reload (implemented)

`AppState.config` is a `ConfigSnapshot` (`RwLock<Arc<Config>>`; `arc-swap` is not in this
dependency set). Every handler takes one snapshot per request with `state.config.load()` —
one read lock plus an `Arc` clone — so a reload can never leave a request reading half of
the old configuration and half of the new one.

Two triggers, both without new dependencies:

- **`SIGHUP`** — always installed; `kill -HUP <pid>` re-reads the file immediately.
- **mtime poll** — when `RROUTER_CONFIG` is set, the file's mtime is checked every 5 s, so
  a mounted ConfigMap or `edit` + wait also applies.

A reload re-reads `RROUTER_CONFIG` and the environment with the same precedence as
startup (environment wins), computes a `ReloadPlan`, and copies **only** the `changed`
fields onto the live snapshot under one write lock. On any error the running configuration
is kept and the error is logged; `restart_required` and `rejected` fields are reported and
never applied.

Subsystems that are not read from the snapshot are swapped by the reload task:

- `max_concurrency` → a new semaphore; in-flight requests keep the permits they hold.
- `log_filter` / `log_format` → the tracing subscriber is rebuilt behind
  `tracing_subscriber::reload` (filter and format in one replaceable layer).

Test hooks (used by `tests/reload.rs`): `AppState::apply_reload(&Config)` (deterministic,
no file or environment), `AppState::reload_from(path, env)`, `AppState::reload_from_file`,
and the `ConfigSnapshot::{load, store, apply}` primitives. `ReloadPlan::between`,
`Config::load_reloadable*` and `Loaded::plan_against` stay as they are.

Structured logs: `info!(changed = […], restart_required = […], rejected = […],
"configuration reloaded")` on success; `warn!` when differences exist but nothing applies;
`error!(path, error, "configuration reload failed; keeping the running config")` for an
invalid file. Secrets are always `set` / `unset` / `rotated` — never a value.

One deliberate nuance: the decision settings (planning strategy, cache, timeouts, auth
flags, CORS) are read from the request's snapshot, but the **access-log toggle** is read
when the usage row is written. A request that finishes just after a reload therefore
follows the new logging setting, while its routing was decided under the old one — logging
is bookkeeping, not behaviour.

### Reloadable, restart-only, rejected

| Class | Fields |
| --- | --- |
| Reloadable | `strategy`, `cache`, `cache_ttl_secs`, `cache_max_entries`, `max_concurrency`, `request_timeout_secs`, `log_filter`, `log_format`, `access_log`, `usage_retention_days`, `cors_origins`, `require_api_key`, `allow_remote_admin`, `admin_token` |
| Restart-only | `host`, `port`, `data_dir`, `static_dir`, `master_key`, `max_attempts`, `retry_attempts`, `retry_base_ms`, `retry_max_ms`, `connect_timeout_secs` |
| Rejected at reload | `allow_remote_admin: false` on a non-loopback bind — the same combination startup refuses, so adopting it would silently disconnect the admin API |

Why the split:

- `master_key` is bound into the store's cipher when the store opens; rotating it means
  decrypting and re-encrypting every stored credential — a migration, not a reload.
- `admin_token` is compared per admin request and has no stored state (dashboard sessions
  are JWTs), so it rotates live as `set → rotated`.
- Retry knobs and `connect_timeout_secs` are baked into the dispatcher (which also owns the
  circuit-breaker state) and the HTTP client; swapping them would discard state or silently
  keep using the old value, so they are restart-only rather than "applied but ineffective".
- `host` / `port` cannot even appear in the file — the loader refuses them with a targeted
  error — and are restart-only if they ever differ.

Schema versions: **v24** was reserved for a credits slice that has been removed from the
roadmap ([ADR 0001](decisions/0001-no-prepaid-credits.md)); that reservation is withdrawn.
**v25–v27 are taken**: v25 is telemetry and webhook state (`usage.trace_id`, `events_outbox`,
`webhook_endpoints`), v26 is tenant-scoped connection ownership, v27 is the eval harness —
see `MIGRATIONS` in `crates/rrouter-store/src/migrations.rs`. **The next free number is
v28.** Config state is still not persisted, so this version adds no migration.
