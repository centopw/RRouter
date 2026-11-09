# 0001 — No prepaid credits: budgets, not balances

- **Status:** Accepted
- **Date:** 2026-11-08
- **Deciders:** maintainers
- **Supersedes:** the planned "credits" slice (never released)

## Context

RRouter is a self-managed gateway: the operator runs it, and the operator owns the provider
accounts it routes to. The gateway's job is to decide *which account* serves a request and
*how much* a caller may spend — it is not a party to the money.

A prepaid-credit model (a balance per key or team, topped up by someone, drawn down per
request) was planned as a slice. It is a different business shape:

- **It makes the gateway a reseller.** A balance that someone pays for is money held on
  behalf of a third party: refunds, expiry, rounding, tax, invoices, disputes and a
  payment-processor integration all become the gateway's problem. None of that exists in a
  self-managed deployment where the operator already pays the providers directly.
- **It invites a marketplace.** Credits only make sense if accounts can be resold, which
  needs metering a buyer did not configure, settlement between parties, and a ledger that
  survives audit. That is a product, not a routing feature, and it contradicts the
  single-binary, one-process, SQLite-bounded deployment this project optimises for.
- **The correct cap already exists.** Key and team budgets (tokens *and* USD, with daily /
  weekly / monthly windows) express "this caller may spend up to X of my provider budget".
  That is the operator's own money being bounded, not a balance being purchased.

The slice was never shipped: no release contains a credit balance, no supported API or UI
exposes one, and the roadmap no longer carries it. The decision below records why, so it is
not re-litigated by adding a `balance` column.

## Decision

1. **Prepaid credits are not part of RRouter.** No credit balance, top-up, invoice,
   settlement or reseller surface will be added to the core.
2. **Cost control stays budget-based**: per-key and per-team token and USD budgets over a
   configurable window, plus per-key request/token minute caps. Budgets are enforced before
   dispatch (`429 budget_exhausted` / `rate_limited`).
3. **Accounting stays observational**: every request records tokens, USD cost (from the
   catalog price), provider, model, key and team; the usage API, summary, series and CSV
   export are the reporting surface. Nothing invents a price it does not know.
4. **Removed, not deprecated.** The roadmap no longer lists a credits phase, and any
   in-flight, unreleased code for it is not a supported capability. Documentation must not
   claim it. (At the time of this decision the worktree still contained unreleased credit
   code — store, routes and tests — that no release ships; it is expected to be removed and
   is not part of the documented surface.)
5. **Migration numbering**: the schema version reserved for the credits slice is withdrawn;
   see the per-document notes in `docs/CONFIG.md`, `docs/EVENTS.md`, `docs/GUARDRAILS.md` and
   `docs/TELEMETRY.md`.

## Consequences

- **Positive:** no payment/ledger liability in a self-hosted binary; one less schema surface
  to migrate; the accounting model is auditable from the request log the gateway already
  writes; an operator can answer "what did team X cost me?" without a settlement layer.
- **Negative:** an operator who *wants* to resell access must do the billing themselves
  today (they can still hand out budgeted keys per customer, and read usage per key/team).
  A future marketplace use case would start from scratch.
- **Neutral:** budget enforcement and spend reporting are unchanged; team budgets and the
  usage/spend endpoints remain the documented controls (see `README.md` "Keys and limits"
  and the roadmap's cost-accounting phase).

## If resale is ever pursued

Credits return as an **optional module**, not a core column:

- behind an explicit feature or separate binary/crate, with its own storage (a ledger table
  or an external payment provider's objects), never in the request hot path;
- with a funding model chosen up front (who invoices whom, refunds, expiry, currency,
  tax), because that is the decision credits actually encode;
- the core keeps budgets and usage reporting as the enforcement mechanism; the module only
  answers "how much has been funded", not "how routing works".

Until then: no balance, no top-up, no credits in the roadmap.
