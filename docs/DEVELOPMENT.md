# Development

## Build

```sh
cargo build --release
./target/release/rrouter
# http://127.0.0.1:20128
```

The dashboard is embedded at build time. A checkout without `ui/dist` still builds and serves
the smaller dependency-free shell.

## Dashboard dev server

```sh
cd ui
npm install
npm run dev     # http://127.0.0.1:5173, proxies /api and /v1 to :20128
npm run build   # writes ui/dist; next cargo build embeds it
```

The gateway must be running on `20128` for the proxy. Use `RROUTER_DATA_DIR` to point the
gateway at the data directory you intend to test.

## Tests

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
./scripts/verify.sh
python3 scripts/e2e_smoke.py
python3 scripts/oauth_smoke.py
python3 scripts/load_test.py --requests 20 --concurrency 5
./scripts/backup.sh /var/backups/rrouter
```

Coverage includes fallback on 401/429 without losing the client request, first SSE frame
before upstream completion, Anthropic clients receiving Anthropic shapes from an
OpenAI-speaking provider, tool calls/results crossing dialects, strict schemas and native
tools surviving translation, credential material absent from the database, and the admin API
never returning tokens.

## Acceptance harnesses

`scripts/antigravity_smoke.py`, `scripts/combo_smoke.py`, `scripts/model_presets_smoke.py`,
`scripts/backup_restore_smoke.py` and `scripts/e2e_smoke.py` run offline mocks; they exit 3
with `BLOCKED-BY-*` only when a route is absent.
