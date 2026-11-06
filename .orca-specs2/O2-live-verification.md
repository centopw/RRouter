# Task O2 — live OAuth verification harness (scripts/ + crates/rrouter-gateway/tests/)

Read `.orca-specs2/CONTRACT-oauth.md` first; it is binding.

You own `scripts/oauth_smoke.py` and `crates/rrouter-gateway/tests/oauth_flow.rs` only.
Do not modify `crates/*/src/**`.

## Why
The OAuth endpoints are unit-tested for shape, but nothing proves the **whole path on the
real binary**: network exchange → credential persistence → encryption at rest → the
connection appearing in `/api/connections`. Also nothing proves a PKCE login through the
gateway including the loopback redirect.

## 1. `scripts/oauth_smoke.py` (live, real binary)

Python 3 stdlib only. It must:
- start a mock OAuth provider (`ThreadingHTTPServer`) implementing
  `POST /device`, `POST /token`, `GET /verify` and `GET /user`;
- start `target/release/rrouter` on a free port with
  `RROUTER_OAUTH_OVERRIDE_GITHUB_DEVICE/TOKEN`, `RROUTER_OAUTH_OVERRIDE_CLAUDE_TOKEN`, and
  `RROUTER_ALLOW_INSECURE_OAUTH=1` (the override feature is being added in parallel; if the
  env var has no effect when you test, say so in your report and still ship the script);
- **device flow**: `POST /api/oauth/github/start` → assert `challenge.user_code`, assert no
  `access_token` anywhere in the body → `GET /api/oauth/github/status/{id}` → `pending: true`
  → `POST /api/oauth/github/complete` → assert a connection id comes back → assert the mock
  was polled ≥2 times (one `authorization_pending`, then success) → assert
  `GET /api/connections` shows one `auth_type == "oauth"` row with `has_access_token: true`
  and never the token text → second `complete` is idempotent (poll count unchanged);
- **PKCE flow**: `POST /api/oauth/claude/start` → read `challenge.authorize_url`, extract
  `redirect_uri` and `state` → `POST /api/oauth/claude/complete` in a thread → `GET` the
  loopback redirect with the *correct* code+state → assert the connection is created;
- **negative**: a redirect with a wrong `state` must make `complete` fail (and the loopback
  response is 400); the connection count must not increase;
- **at rest**: read `data/rrouter.db` bytes and assert no access/refresh token appears;
- print `PASS`/`FAIL` per check and end with `RESULT: ALL PASS` / `RESULT: FAILURES PRESENT`;
- exit non-zero on failure; always terminate the gateway and mock (try/finally).

## 2. `crates/rrouter-gateway/tests/oauth_flow.rs` (in-process)

Use the existing harness style from `tests/proxy.rs` (`app(Arc<AppState>)` + `tower::ServiceExt::oneshot`),
but point the OAuth endpoints at an `axum` mock you spawn in the test. Cover:
1. device flow end to end through the gateway, ending in a stored connection;
2. PKCE flow end to end, including the loopback redirect (spawn `complete` as a task,
   connect to the redirect URL, then assert the connection);
3. state mismatch → `complete` returns an error and no connection is stored;
4. the connection row from `/api/connections` contains no token material.

## Verification (paste summarized output)
- `python3 scripts/oauth_smoke.py` → `RESULT: ALL PASS`.
- `cargo test -p rrouter-gateway --test oauth_flow` → 4 tests pass.
- `cargo clippy -p rrouter-gateway --all-targets -- -D warnings` clean.
