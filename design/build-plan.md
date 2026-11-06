# Workers — dispatch plan (coordinator: [CL])

Each worker receives: this plan's contract path + its own spec path. Specs are
self-contained: target, exact API, files, verification. Workers report back; the
coordinator integrates and does all cross-crate verification.

| Task | Crate | Spec | Depends on |
|---|---|---|---|
| T2 | `rrouter-translate` | `.orca-specs/T2-translate.md` | core (done) |
| T3 | `rrouter-auth` | `.orca-specs/T3-auth.md` | core (done) |
| T4 | `rrouter-store` | `.orca-specs/T4-store.md` | core (done), auth API (T3, header) |
| T5 | `rrouter-router` | `.orca-specs/T5-router.md` | core (done), auth/store APIs (headers) |
| T6 | `rrouter-gateway` | `.orca-specs/T6-gateway.md` | translate/auth/store/router (headers) |

Deadlock rule for T4/T5/T6: if a dependency crate does not yet have the API in its
spec, write against the spec's signature — the coordinator reconciles. A missing symbol
is a compile error to *report*, not to work around by duplicating the type.

## Integration gates (coordinator only)
1. `cargo build` workspace green.
2. `cargo test` workspace green.
3. `cargo clippy --workspace --all-targets -- -D warnings` green.
4. `cargo run --release` → `/health`, `/v1/models`, `/v1/chat/completions` smoke against a
   `mock` provider served by the gateway itself.
