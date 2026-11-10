# Models

**Models** in the dashboard is the registry behind `/v1/models`: exactly what the gateway can
call right now, from connected providers.

- **Provider lists** — adding a provider fetches its own `/models` endpoint; Fetch re-runs it.
  Codex cannot enumerate, so its verified ids come from the built-in catalog.
- **models.dev sync** — metadata only: context, output cap, capabilities and price. It never
  adds a model.
- **Custom models** — any id with limits/capabilities/price for providers without a list.
  Custom rows win and survive syncs.
- **Disabled models** — disabled rows leave the active table and `/v1/models`; enabling
  restores them. Disabling is by canonical id, so a shared model is disabled on every provider.
- **Provider coverage** — a model id served by several providers shows a provider count and
  peer popover with price/state; `shared only` filters to those ids.
- **Groups** — the board is grouped by provider by default. A preset can create free-form
  groups, rename providers' buckets, reorder groups, and move a model to another group;
  grouping is presentation only. `/v1/models` carries the same groups additively.
- **Preferred provider** — pin one provider for a bare model id. Bare requests use it first;
  explicit `provider/model` ignores it. `/v1/models` reports it as `owned_by`.
- **Test** — a real 16-token completion through the planner, dispatcher and reassembly path.

Every model picker is grouped by provider by default; a preset can define custom groups and
move models between them. No free-text model fields.

Saved layouts are covered in [Model presets](MODEL_PRESETS.md).
