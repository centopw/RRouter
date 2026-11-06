# Task O1 — endpoint overrides + durable account identity (crates/rrouter-auth)

Read `.orca-specs2/CONTRACT-oauth.md` first; it is binding.

You own `crates/rrouter-auth/**` only.

## Why
Two gaps block a real OAuth phase:
1. The built-in login flows point at vendor hosts. There is no way to point them at a
   self-hosted/regional OAuth server, and no way to test the flow end to end against a
   local mock. (9router had the same gap: hardcoded URLs, untestable.)
2. A sign-in that returns no `id_token` produces a connection with no human label, so the
   dashboard shows `claude (OAuth)` for every account instead of the email.

## 1. Endpoint overrides (`src/registry.rs` + a new `src/overrides.rs`)

Add an env-driven override layer applied by `get()` and by both flow start functions:

| env var | overrides |
|---|---|
| `RROUTER_OAUTH_OVERRIDE_<PROVIDER>_AUTHORIZE` | `PkceFlow::authorize_url` |
| `RROUTER_OAUTH_OVERRIDE_<PROVIDER>_TOKEN` | `PkceFlow::token_url`, `DeviceFlow::token_url`, and `ProviderSpec::refresh.token_url` |
| `RROUTER_OAUTH_OVERRIDE_<PROVIDER>_DEVICE` | `DeviceFlow::device_code_url` |

`<PROVIDER>` is the registry id uppercased with `-`→`_` (`github` → `GITHUB`).

Requirements:
- Overrides are read **once** into a `LazyLock` map (not per request).
- An override must only replace hosts **RRouter is allowed to call**: it is an operator
  feature, so accept `http://127.0.0.1`/`localhost` only when `RROUTER_ALLOW_INSECURE_OAUTH=1`
  is set; otherwise require `https://`. A rejected override must be reported (return it via a
  `pub fn invalid_overrides() -> Vec<String>` so the gateway can log it) and NOT applied.
- Public API: `pub fn override_for(provider: &str, kind: OverrideKind) -> Option<&'static str>`
  with `pub enum OverrideKind { Authorize, Token, Device }`.
- `get()` stays `&'static ProviderSpec`; instead expose
  `pub fn login_flow_for(provider: &str) -> Option<LoginFlow>` that returns the spec's login
  flow **with overrides applied**. `OAuthFlows::start` must use `login_flow_for`, and
  `Refresher`'s refresh must use the token override from `override_for(id, Token)`.
- Keep the constants as the default; do not mutate the `static PROVIDERS` table.

## 2. Account label fallback (`src/flows.rs`)

After a successful exchange, if `account_label` would be `None`, resolve one:

1. from the `id_token` (already implemented — keep it);
2. else, for providers that have a profile endpoint, call it once with the access token.
   Add optional fields to `PkceFlow`/`DeviceFlow`? **No** — instead add
   `pub profile_url: Option<&'static str>` to `ProviderSpec`, set for
   `claude` (`https://api.anthropic.com/api/oauth/profile` → JSON `account.email_address`
   or `email`), `codex` (`https://auth.openai.com/userinfo` → `email`), and `github`
   (`https://api.github.com/user` → `login`). All three are best-effort: any error or
   unrecognised shape must leave the label `None` and MUST NOT fail the sign-in.
3. `SignedIn.account_label` and `Credentials.account_label` must both be set when resolved.

## Verification (paste summarized output)
- `cargo test -p rrouter-auth` — add tests for:
  - override applied for a provider, and the default used when unset;
  - an `http://` override rejected without `RROUTER_ALLOW_INSECURE_OAUTH`, accepted with it;
  - an `https://` override accepted;
  - `invalid_overrides()` reports the rejected one;
  - label from profile endpoint (mock `wiremock`), label `None` when the endpoint 404s and
    the sign-in still succeeds, and `id_token` label winning over the profile call
    (assert the profile endpoint is hit 0 times in that case);
  - refresh uses the token override (assert the mock URL is hit).
  Set env vars in tests with `std::env::set_var` under a `static MUTEX` (Rust's env is
  process-global) and restore them; keep every test independent.
- `cargo clippy -p rrouter-auth -- -D warnings` clean.
