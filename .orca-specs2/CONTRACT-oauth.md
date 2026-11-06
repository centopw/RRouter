# RRouter — OAuth phase: shared contract (owner: coordinator)

State: `main` at `68ab619`, workspace builds, **143 tests green**, clippy clean.

## What already exists (do NOT rewrite; extend)

`crates/rrouter-auth`:
- `registry.rs` — `ProviderSpec { id, display, kind, base_url, …, refresh, login: Option<LoginFlow> }`.
  `LoginFlow::Pkce(PkceFlow)` / `LoginFlow::DeviceCode(DeviceFlow)`.
  Constants `CLAUDE_LOGIN`, `CODEX_LOGIN`, `GITHUB_LOGIN`. Lookup: `rrouter_auth::get(id)`.
- `pkce.rs` — `generate_verifier()`, `challenge_for(v)`, `generate_state()`,
  `authorize_url(...)` (reserved params cannot be overridden).
- `loopback.rs` — `LoopbackServer::bind(port, path)`, `.redirect_uri()`,
  `.wait(expected_state, max_wait) -> Result<Callback>`. Loopback-only, one shot,
  rejects state mismatch, times out.
- `flows.rs` — `OAuthFlows::{start, complete, is_pending, cancel, pending_ids}`;
  `Challenge::{Pkce, DeviceCode}`; `SignedIn { provider, credentials, account_label }`.

`crates/rrouter-gateway`:
- `routes/oauth.rs` — `POST /api/oauth/{provider}/start`,
  `GET /api/oauth/{provider}/status/{id}`, `POST /api/oauth/{provider}/complete`, 
  `DELETE /api/oauth/{provider}/{id}`.
- `assets/app.js` — `startLogin()` + Connect buttons in the Connections panel.

## Hard rules (all workers)
- **Path-scoped.** Only create/edit files inside the crate/asset paths your task names.
  Never touch root `Cargo.toml`, `README.md`, other crates, or `.orca-specs/`.
- Need a dependency or a change to `rrouter-core`/`rrouter-auth` public API? Report the
  exact symbol and stop that thread — the coordinator will add it.
- Verify with the exact commands in your task. Paste summarized command output in your report.
- No `unwrap()`/`expect()`/`panic!` on request, upstream, or user data in non-test code.
- No `TODO`, stubs, placeholder functions, or "left as an exercise". If something cannot be
  completed, say exactly what is missing and why.
- Do not run `cargo fmt`/`clippy --fix` across other crates; do not revert or reformat code
  you did not write.
- Report: files changed, the public API you added, test/clippy output summary, blockers.

## Verification baseline you must keep green
```
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```
