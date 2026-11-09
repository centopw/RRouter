# Combos and strategies

A **combo** is a named list of model refs that a client can request as one model
(`combo:<name>`). The list is operator-authored; the strategy decides how a request moves
through it. Combos are instance-wide admin configuration.

Run `scripts/combo_smoke.py` against a build to verify the contract: it exits `3`
(`BLOCKED-BY-BACKEND`) when the CRUD surface stores strategies but routing does not apply
them yet, so a green script (exit 0) is the actual claim of support.

| Strategy | Request order | Best for |
| --- | --- | --- |
| `fallback` (default) | The stored order, top to bottom; the next model is tried only when the previous one fails. | A reliable primary with cheaper/secondary backups. |
| `round_robin` | Starts one model later on every request (durable per-combo cursor) and wraps; a failure still falls through the rotated order. | Spreading load across equivalent models/accounts. |
| `smart` | A **decision model** (`router_model`) picks one entry for the request; the list order is the fallback chain if the decision fails. | Cost/latency-aware routing with a cheap classifier. |

## API shape

```json
POST /api/combos
{
  "name": "chat-default",
  "models": ["openai/gpt-4.1-mini", "anthropic/claude-haiku-4-5", "openai/gpt-4.1"],
  "strategy": "fallback",
  "router_model": null
}
```

The response echoes the stored row, including `strategy`, `router_model` and the durable
`cursor`:

```json
{
  "id": "…", "name": "chat-default", "models": ["openai/gpt-4.1-mini", "..."],
  "strategy": "fallback", "router_model": null, "cursor": 0, "created_at": 1760000000
}
```

- `POST /api/combos` is an upsert by `name`: re-posting edits the list and strategy and
  **resets `cursor` to 0**.
- `GET /api/combos` lists every combo; `DELETE /api/combos?name=<name>` removes one.
- `models` must be a non-empty list; entry order is the fallback order.
- `strategy` is one of `fallback`, `round_robin`, `smart`; anything else is `400`.
- `smart` requires `router_model`; a missing/unknown strategy or a smart combo without a
  decision model is `400` (it is never silently downgraded to fallback).
- `router_model` names a normal model ref (not this combo, to avoid recursion); it must be
  routable through an active connection.

Clients call the combo by name:

```json
POST /v1/chat/completions
{"model": "combo:chat-default", "messages": [{"role": "user", "content": "hi"}]}
```

`combo:` forces combo interpretation even when the name looks like a provider path.

## Round-robin semantics

- The cursor is stored on the combo and survives restarts.
- Request *n* starts at `cursor % len(models)` and advances the cursor once per routed
  request; order wraps (`a, b, c, a, …`).
- Within a request, failures still fall through the rotated order, so a dead first entry
  costs a fallback, not the request.
- `POST /api/combos` resets the cursor; duplicate concurrent requests may occasionally
  start at the same position (at-most-once advancement is not a load balancer).
- Round-robin distributes **requests**, not tokens: uneven response sizes still produce an
  even request split.

## Smart decision-model contract

For a `smart` combo the gateway sends the decision model one chat request containing the
user turn and a system instruction listing the combo's candidate refs. The decision model
must answer a JSON object on the assistant message:

```json
{"choice": "openai/gpt-4.1-mini"}
```

- `choice` must be exactly one of the combo's `models` entries (a bare `provider/model`).
- Anything else — invalid JSON, a value outside the list, a timeout, a 5xx — falls back to
  the **first** entry in `models`; the request is never failed because the classifier
  misbehaved.
- The decision call happens before the answer call, so it adds one model round-trip to
  p99 and its tokens are an operational cost of routing (visible in logs/attempt
  accounting as its own upstream attempt, not billed as the client's completion).
- The decision model should be small and cheap; `router_model` may be a combo-free model
  ref on any connected provider.
- Never put credentials or user PII in the candidate list; entries are model refs only.

## Failure behaviour

- `fallback` and `round_robin` treat a provider error/429/timeout as “try the next entry”;
  the breaker, retry budget and attempt cap still apply per candidate.
- If every entry fails, the client receives the last error (dialect-native), and the usage
  row records the terminal status.
- A combo with no routable entries answers `404` naming the combo/model; an empty combo
  cannot be saved.
- Disabling the only connection behind a combo does not disable the combo row; requests
  fail until an entry is routable again.

## Acceptance harness

```sh
cargo build --release
RR=target/release/rrouter python3 scripts/combo_smoke.py
```

The script starts a mock OpenAI upstream plus a temporary gateway, creates a fake
connection and custom model rows, then:

1. creates `fallback`, `round_robin` and `smart` combos and asserts `strategy`/
   `router_model` echo and model order;
2. asserts smart-without-`router_model` and unknown strategies are `400`;
3. forces the first fallback entry to fail and checks the second serves the answer;
4. sends six sequential requests through the round-robin combo and checks both entries are
   used with an even split (within one);
5. sends one request through the smart combo and checks the decision model is called first
   and the chosen entry serves the answer;
6. checks the upstream API key never appears in the combo listing or gateway logs.

Exit codes: `0` all checks pass, `1` a real failure, `3` `BLOCKED-BY-BACKEND` when the
build stores/validates strategies but routing does not apply them yet (or the smart
decision is not invoked), `4` the gateway could not start. Against an already running
gateway use `--base-url` and `--token`; the script still creates its own mock upstream and
temporary rows.

## Manual browser checklist

1. **Create** a combo with three models; save and reload — order and name persist.
2. **Edit** the list (reorder) and save; the row shows the new order.
3. **Strategy switch** fallback → round_robin → smart; the strategy badge and (for smart)
   the router-model picker appear; saving echoes both fields.
4. **Smart picker**: choose a decision model from the searchable model list; saving without
   one is blocked with an inline `router_model` error.
5. **Failure copy**: an unknown strategy or a smart combo without a decision model shows the
   `400` message, not a silent fallback.
6. **390 px**: the combo editor/list stack without horizontal scrolling; strategy picker,
   model rows and save/cancel remain reachable and focus-visible.
7. **No secrets**: the combo list and editor never show connection keys, tokens or headers.
