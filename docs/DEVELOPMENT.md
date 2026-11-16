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

## Reclaim disk

`target/` accumulates every profile and target combination; a long-lived checkout can
reach tens of gigabytes. Check it first, then clean Rust output when you do not need an
incremental build:

```sh
./scripts/clean.sh --dry-run          # show target/, ui/node_modules and ui/dist sizes
./scripts/clean.sh                    # cargo clean; keeps source and tracked ui/dist
./scripts/clean.sh --ui               # also remove ui/node_modules
./scripts/verify.sh --clean           # full gate with a from-scratch Rust build
```

`ui/dist` is committed and embedded by `cargo build`; leave it alone unless you intend
to rebuild the dashboard before committing. A clean is a rebuild cost, not data loss:
application state lives in `RROUTER_DATA_DIR`, not `target/`.

Coverage includes fallback on 401/429 without losing the client request, first SSE frame
before upstream completion, Anthropic clients receiving Anthropic shapes from an
OpenAI-speaking provider, tool calls/results crossing dialects, strict schemas and native
tools surviving translation, credential material absent from the database, and the admin API
never returning tokens.

## Acceptance harnesses

`scripts/antigravity_smoke.py`, `scripts/combo_smoke.py`, `scripts/model_presets_smoke.py`,
`scripts/backup_restore_smoke.py` and `scripts/e2e_smoke.py` run offline mocks; they exit 3
with `BLOCKED-BY-*` only when a route is absent.
