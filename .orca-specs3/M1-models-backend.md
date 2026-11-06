# M1 — model registry backend

Implement `.orca-specs3/CONTRACT-models.md` exactly. Rust only.

## Files you own (do not touch anything else)

- `crates/rrouter-store/src/migrations.rs`, `crates/rrouter-store/src/store.rs`
- `crates/rrouter-gateway/src/routes/models.rs`
- `crates/rrouter-gateway/src/routes/mod.rs` (only to export a new module if you add one)
- `crates/rrouter-gateway/src/lib.rs` (only to register the new routes)
- `crates/rrouter-gateway/src/state.rs` (only to add fields you need)
- `crates/rrouter-gateway/src/models_registry.rs` (new module; preferred home for merge logic)
- `crates/rrouter-gateway/tests/models.rs` (new test file)
- `crates/rrouter-router/src/catalog.rs` (only if you need the models.dev alias map there; a helper `models_dev_provider(id)` is welcome)

NEVER touch `crates/rrouter-gateway/src/proxy.rs`, `crates/rrouter-translate/**`,
`crates/rrouter-gateway/assets/**`, or `crates/rrouter-gateway/src/routes/connections.rs`.

## 1. Storage

One new table, additive migration, `IF NOT EXISTS`:

```sql
CREATE TABLE IF NOT EXISTS models (
  provider   TEXT NOT NULL,
  model      TEXT NOT NULL,
  display    TEXT,
  context    INTEGER,
  max_output INTEGER,
  caps       TEXT NOT NULL DEFAULT '{}',
  source     TEXT NOT NULL,
  updated_at INTEGER NOT NULL,
  PRIMARY KEY (provider, model)
);
```

Store methods: `upsert_model`, `delete_model(provider, model) -> bool`,
`list_models() -> Vec<ModelRow>`, `clear_models_by_source(source)`.
`delete_model` must refuse non-`custom` rows: return whether something was
deleted, and let the route answer 404 for absent or non-custom rows.

## 2. Merge

`models_registry::list(state)` builds the `GET /api/models` payload:

- start from `state.catalog.all()` (`source = "catalog"`, context from catalog);
- overlay `synced` rows (models.dev), then `upstream`, then `custom` — later wins
  on a `(provider, id)` collision, but a `custom` row must never be dropped just
  because the catalog also lists it;
- capabilities merge per field: a row's non-default value wins, else the value
  from the row it overlays, else `catalog` defaults (`tools: true`, rest false);
- `enabled` from `state.store`'s `disabled_models` set;
- `available` from active connections.

## 3. Routes

Register under the admin-guarded router next to the existing `/api/*` routes:

| Method | Path | Handler |
|---|---|---|
| GET | `/api/models` | `models::registry_list` |
| GET | `/api/models/providers` | `models::providers` |
| POST | `/api/models/sync` | `models::sync` |
| POST | `/api/models/fetch` | `models::fetch` |
| POST | `/api/models/custom` | `models::custom` |
| DELETE | `/api/models/custom` | `models::delete_custom` |
| POST | `/api/models/test` | `models::test` |

`sync`: `state.http.get("https://models.dev/api.json")`, 60 s timeout, parse
`{ "<provider>": { "models": { "<id>": { "name", "reasoning", "tool_call",
"modalities": {"input": [...], "output": [...]}, "limit": {"context","output"} } } } }`,
map ids through the alias table, `clear_models_by_source("synced")`, insert, then
also spawn a background task at startup: first run after 60 s, then every 24 h,
failures logged via `tracing::warn` and never fatal.

`fetch`: load the connection, `fresh()` its credentials, GET the provider's model
list. Base rule: take the provider's Chat base (or the connection's
`base_url_override`), strip a trailing `/chat/completions`, and append `/v1/models`;
for `claude` use `https://api.anthropic.com/v1/models`; for `codex` answer
`{"provider":"codex","status":0,"count":0,"added":0,"data":[]}` with a 400 admin
error explaining codex has no list endpoint — the UI then offers the custom form.
Cap ids at 5000; ignore anything empty.

`test`: resolve the model through the normal planner/dispatcher (same code the
proxy uses) with a 16-token `ping` message and answer the contract shape. Never
stream client-side; the gateway already reassembles stream-only providers.

## 4. `/v1/models`

Extend the existing handler so it also lists `custom` and `upstream` rows
(`owned_by = provider`). Keep the OpenAI envelope.

## 5. Tests (`crates/rrouter-gateway/tests/models.rs`)

Reuse the harness style from `tests/proxy.rs` (build `AppState`, `oneshot`).
Cover, at minimum:

- `GET /api/models` lists catalog models with the nine capability keys and sorted order;
- precedence: a `custom` row wins over a catalog id of the same provider/id;
- `POST /api/models/custom` round trip, then `DELETE` removes it, then `DELETE`
  again 404s;
- `disabled_models` flips `enabled` to false in `GET /api/models`;
- sync mapping: feed a small models.dev fixture through the parser (extract a
  pure function taking `Value` and returning rows, so no network in tests) and
  assert `context`, `tools`, `reasoning`, `vision` come out right, and that an
  unlisted provider falls back to its own id.

## Acceptance

`cargo test --workspace` green, `cargo clippy --workspace --all-targets -- -D warnings`
clean, and this live check on a spare port (`PORT=20299 ./target/debug/rrouter`):

```
GET /api/models | jq '.data[0]'
POST /api/models/test {"model":"codex/gpt-6-luna"}   # may fail without credentials; must return the contract shape
```

Report: files changed, the exact commands you ran, and anything in the contract
you believe is wrong.
