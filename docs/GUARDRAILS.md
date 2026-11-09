# Guardrails and PII redaction

Two layers, deliberately separate:

| Layer | Status | Where |
| --- | --- | --- |
| Key guardrails: prompt-size cap, blocked phrases, response scan with stream cut | shipped | `rrouter-store` policy + `proxy.rs` enforcement |
| Text redaction: email / phone / card / secret → stable placeholders | **wired for chat, streams included**: input before planning, non-streaming output before return, streamed text deltas rewritten with a bounded carry; tool-call arguments are deliberately not rewritten | `crates/rrouter-gateway/src/redaction.rs` + `proxy.rs` |

Guardrails decide whether a request or response is allowed. Redaction decides what text may
be logged, stored or forwarded. They are orthogonal: running one never substitutes for the
other, and this page is the contract for wiring the second.

## 1. What redaction does

`redaction::redact(&str) -> (Cow<str>, RedactionReport)` — pure, no I/O, no `AppState`, no
global state.

| Class | Placeholder | Recognised as | Left alone |
| --- | --- | --- | --- |
| email | `[email]` | `local@domain.tld`, alphabetic TLD, RFC-ish local/domain constraints | `a@b`, `user@host-01`, `@example.com` |
| phone | `[phone]` | `+`-prefixed numbers (8–15 digits) or formatted numbers with ≥3 groups (10–15 digits) | bare digit runs (timestamps, ids), year-prefixed ids, card-shaped 4-4-… runs |
| card | `[card]` | 13–19 digits (space/dash separated) passing Luhn, not all one digit | `1234 5678 9012 3456`, all-zero runs |
| secret | `[secret]` | `Bearer`/`Basic` values (≥8 chars), `sk-`/`sk-rr-`/`gh*_`/`xox*`/`AKIA`/`AIza` key shapes, JWT triples | short words (`Bearer of …`, `sk-learn`), bare prefixes |

Properties:

- **Deterministic overlap policy.** Candidates sort by earliest start, then longest match,
  then class priority (`secret` > `card` > `email` > `phone`); overlapping candidates are
  dropped. An email containing a card number becomes one `[email]`; no two placeholders ever
  overlap.
- **Byte-accurate.** Patterns are ASCII, so matches land on character boundaries; UTF-8
  around and between matches is copied verbatim (`café: [email] ☎ [phone] 🔐`).
- **Cheap.** One pass per class over the bytes, no regex, no allocations when nothing
  matches (`Cow::Borrowed`), no locks. Cost is linear in the text and independent of the
  process's other state.
- **Idempotent.** Placeholders cannot match again, so redacting twice changes nothing.
- **Auditable without leaking.** `RedactionReport { redacted, counts }` stores counts only;
  its `Debug` and `Display` cannot print an input value because none is kept. Counts are
  per class (`report.count(PiiClass::Email)`), and reports accumulate across chunks
  (`+=`) for streams.

## 2. What it is not

No NER, no address/name detection, no PEM-block or prose-password parsing, no locale-aware
phone validation (no libphonenumber), no guarantee that a determined encoding escapes it.
It is a conservative net over four high-signal classes. Treat redacted text as
lower-risk, not as certified. A caller must still not put credentials into text
it forwards; the layer is a second line, not a licence.

## 3. Report shape

```rust
pub struct RedactionReport { pub redacted: bool, pub counts: [u64; 4] } // indexed by PiiClass
```

Use `count(class)` / `total()`; feed `PiiClass::as_str()` into a metric label. `Display`
renders `redacted 3 email=1 phone=1 secret=1` and never a value.

## 4. Wiring contract (A)

### 4.1 Input (request)

Order matters:

1. existing key guardrails run **first**, on the original text (prompt size, blocked
   phrases) — a blocked phrase inside an email must still block;
2. canonicalize the body as today;
3. **then** redact the text that will leave the gateway or be recorded:
   `system` + message text parts (and tool-result content, which routinely carries pasted
   keys), not tool schemas or ids;
4. run Token Saver and dispatch on the redacted text.

Suggested insertion point: after `state::check_prompt`/canonical request construction and
before `Attempt` dispatch in `proxy.rs`, using `ChatRequest` (or `prompt_text`) rather than
the raw JSON so the placeholder never breaks JSON escaping.

Redacting input changes what the model sees; it is a **per-key opt-in** (§4.4).

### 4.2 Response

- **Never alter the bytes sent to the client by default.** A key without
  `guardrails.redact_pii` is byte-identical to an unredacted gateway. An opted-in key
  accepts that text is rewritten (see §4.3); structured payloads stay untouched regardless.
- Redact for **storage and logs**: before writing the usage row / attempt error / access
  line, run `redact` on the response text (or on the error string). A response guardrail
  refusal message should be redacted the same way.
- When the guardrail output scan already walks the response, reuse that pass instead of a
  second full walk.

### 4.3 Streams — carry-window policy (shipped)

Streaming text is transformed when the key opted in. The gateway never buffers the whole
stream; it holds a bounded carry (`STREAM_CARRY_BYTES = 512` bytes) in
`redaction::StreamRedactor`:

1. append the delta to the carry and scan the carry with the same detectors as the
   non-streaming path;
2. emit everything before the last 512 bytes, **except** when a match ends at the buffer
   end (it may still grow) or a match crosses the emission cut — in those cases emission
   stops at that match's start, so a placeholder is never split and a value is either
   removed whole or not yet emitted;
3. on `Done`, flush the carry (redacted) as one final text delta before the terminal frame;
4. on an upstream/decode/encoder error, flush the carry the same way but drop a trailing
   token that looks like the start of a PII value (unterminated `…@…`, ≥8 digits, a known
   secret prefix) rather than emitting its prefix.

Consequences:

- **Latency.** Visible text can lag the upstream by up to 512 bytes: a short answer may
  arrive only when the stream ends, longer answers emit once roughly that much text
  accumulates. `TTFT` in the usage row still measures the upstream's first output, so a
  large gap between it and the client's first visible token is expected on opted-in keys.
- **Bound.** A value longer than 512 bytes that straddles an emission cut can be emitted
  in part; the supported classes (emails, cards, phones, provider keys/JWTs) fit the
  window. Do not raise the window past what a live chat tolerates without measuring.
- **Idempotence.** Already emitted text is never rescanned; placeholders such as
  `[email]` do not match any detector, so a second pass (cache re-redaction, carry
  overlaps) cannot double-redact.
- **Tool calls.** `ToolCallStart` and `ToolCallArgsDelta` are protocol payloads: rewriting
  them would change what a tool receives, so they are skipped with this limitation — a
  credential inside tool arguments reaches the client unredacted. Same rule for opaque
  parts.
- **Framing.** Only decoded `TextDelta`/`ReasoningDelta` events are rewritten; SSE framing,
  event order, the terminal frame and its usage are the encoder's output and are unchanged.
- **Abort.** A client disconnect sends nothing further (the carry is dropped, not leaked);
  a policy block discards the carry so the blocked term cannot leak.

### 4.4 Opt-in and schema (shipped as a policy field)

- The flag now exists: `guardrails.redact_pii` (boolean, default `false`) on a key's
  existing guardrails JSON — no schema change was needed, and a key without it behaves
  exactly as before.
- Set it with `PATCH /api/keys/{id}` `{"guardrails": {"redact_pii": true}}` or the Keys
  editor's redaction toggle (shipped).
- The reference implementation: input redaction runs after the guardrail decision and
  before planning; output redaction covers the non-streaming body before it is returned and
  cached; provider error text is redacted before it is stored or returned; streamed text
  deltas are rewritten with the carry policy in §4.3, and the streamed tail is flushed
  before `Done` or an error frame.
- Changing a key's redaction policy must invalidate nothing else: it is read-only policy.

### 4.5 Metrics and logs

- `rrouter_pii_redactions_total{stage="input|output|stream",class="email|phone|card|secret"}`
  from `report.counts`; every pair is always exposed like the guardrail stages, so a
  dashboard series exists before it first fires. No label ever carries a value.
- Never log `text` after redaction unless the caller already needs it; never log the report
  as `{text, report}`. `report` is safe on its own.
- The existing `events::redact` (key/value based, for JSON webhook payloads) and this
  module (text based) are complementary: run the text layer before building an event
  payload so a secret in a message body cannot reach the outbox.

## 5. Tests

`cargo test -p rrouter-gateway redaction` (25 unit tests, including the streaming carry):
a short stream is held until finish; an email, card and secret split across deltas inside a
long buffer never leak; split placeholders are not redacted twice; an interrupted stream
drops an unterminated PII tail. The rest cover each class; placeholders and
labels are stable; emails with display text; non-emails; formatted phones and the
bare-digit policy; Luhn-valid cards (Visa/Mastercard/Amex/discover-style) and invalid long
numbers; random order ids untouched; bearer/basic/provider key/JWT shapes and short-word
non-matches; mixed text with per-class counts; overlapping matches resolving to the email;
UTF-8 preservation around matches; borrowed output for empty/clean text; idempotence;
report accumulation; and the guarantee that neither `Debug` nor `Display` carries a removed
value.

`crates/rrouter-gateway/tests/proxy.rs` adds end-to-end stream tests against a real SSE
mock: redaction across event boundaries with usage/finish preserved, split card and secret,
tool arguments kept byte-identical while text is redacted, a disabled key passing raw
bytes, and a stream with no terminal frame still flushing the redacted tail.
