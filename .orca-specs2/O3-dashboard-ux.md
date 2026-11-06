# Task O3 — dashboard sign-in UX (crates/rrouter-gateway/assets/)

Read `.orca-specs2/CONTRACT-oauth.md` first; it is binding.

You own `crates/rrouter-gateway/assets/index.html`, `assets/app.js`, `assets/style.css`
only. Do not edit Rust.

## Why
`startLogin()` exists but the UX is thin: no status polling, no progress while the human is
in the browser, no way to see which providers can be signed in, and errors are a bare line of
text. This is the surface an enterprise operator actually uses.

## Requirements

1. **Provider list from the server, not the client.** Replace the hardcoded
   `OAUTH_PROVIDERS` array with `GET /api/oauth/providers`, which must return
   `{"data":[{"id","display","flow":"pkce"|"device_code"}]}` for every registered provider
   that has a login flow. **You must add this route** — that is the one Rust exception
   granted to you: create `crates/rrouter-gateway/src/routes/oauth.rs` **only for this one
   handler**, and register it in `lib.rs` next to the other `/oauth` routes. Nothing else.
   **Likely conflict:** a parallel task may add the same route. Register it only if absent:
   grep `lib.rs` for `oauth/providers` first. If it already exists, reuse it and do not add a
   second registration. Report exactly what you added.
2. **PKCE flow UX**: after `start`, show the authorize URL as a button, a live "waiting for
   you to finish in the browser…" state with elapsed seconds, and Cancel. Pressing the
   continue button calls `complete` and shows a spinner while it waits.
3. **Device flow UX**: big monospace user code with a copy button, the verification URL as a
   link, the remaining time from `expires_in` counting down, and automatic completion (the
   server polls; the client should call `complete` once and display progress).
4. **Errors**: every failure path shows the server's `error.message` verbatim in a
   `.error` block, plus a Retry button. No `alert()` calls anywhere.
5. **After success**: the new connection appears in the table without a full page reload, and
   a success line shows the account label when one was resolved.
6. **Accessibility**: every interactive control is a real `<button>`/`<a>`, the user code has
   an `aria-label`, status changes go into an element with `role="status"` and
   `aria-live="polite"`, and focus moves to the status region when a flow starts.
7. **No build step, no CDN, no dependency.** Plain ES modules. Keep `app.js` under ~350 lines.

## Verification (paste summarized output)
- `python3 -c "import html,re;open('crates/rrouter-gateway/assets/app.js')" ` is not enough:
  instead run `node --check crates/rrouter-gateway/assets/app.js` (or
  `bun build --target browser --outfile /dev/null crates/rrouter-gateway/assets/app.js`) and
  show it passes.
- `cargo test -p rrouter-gateway` and `cargo clippy -p rrouter-gateway --all-targets -- -D warnings` clean.
- Add a gateway test asserting `GET /api/oauth/providers` returns `claude`, `codex`, `github`
  with the right `flow` values, and that it never includes a provider without a login flow
  (`openai`). Put it in `crates/rrouter-gateway/tests/oauth_providers.rs` (a new file you own).
