import type { Alias, Combo, ComboStrategy, Model } from "../../lib/api";
import type { DraftEntry } from "../../lib/layout-draft";

/**
 * One row of the unified Models table. A model is a registry row; an alias or combo is a
 * routable name. Both carry their place in the preset draft, because grouping and
 * `/v1/models` visibility are stored per ref in the same layout document.
 *
 * A model row with `model: null` is an entry whose provider stopped listing it: it stays
 * on the board, dimmed and badged `unavailable`, until the operator removes it.
 */
export type RowStatus = "routed" | "hidden" | "disabled" | "unrouted" | "catalog";

export type ModelRow = {
  kind: "model";
  ref: string;
  model: Model | null;
  entry?: DraftEntry;
  status: RowStatus;
};

export type AliasRow = {
  kind: "alias";
  ref: string;
  name: string;
  alias: Alias;
  entry?: DraftEntry;
  status: RowStatus;
};

export type ComboRow = {
  kind: "combo";
  ref: string;
  name: string;
  combo: Combo;
  entry?: DraftEntry;
  status: RowStatus;
};

export type UnifiedRow = ModelRow | AliasRow | ComboRow;

/** `catalog` is the member view: no preset, so only the global enabled flag is known. */
export function statusOfModel(
  model: Model,
  entry: DraftEntry | undefined,
  presetAware: boolean,
): RowStatus {
  if (!model.enabled) return "disabled";
  if (!presetAware) return "catalog";
  if (!entry) return "unrouted";
  return entry.visible ? "routed" : "hidden";
}

export function statusOfName(
  entry: DraftEntry | undefined,
  presetAware: boolean,
  autoInclude: boolean,
): RowStatus {
  if (!presetAware) return "catalog";
  // An alias/combo with no stored entry is still advertised when the preset merges its
  // live catalog, exactly like a model; calling it unrouted would make the Routed filter
  // lie about what /v1/models serves.
  if (!entry) return autoInclude ? "routed" : "unrouted";
  return entry.visible ? "routed" : "hidden";
}

const STRATEGY_LABEL: Record<ComboStrategy, string> = {
  fallback: "fallback",
  round_robin: "round robin",
  smart: "smart",
};

/** The second line of a name row: what it resolves to, which its page owns. */
export function nameDetail(row: AliasRow | ComboRow): string {
  if (row.kind === "alias") return `alias → ${row.alias.target}`;
  const strategy = STRATEGY_LABEL[row.combo.strategy ?? "fallback"];
  const count = `${row.combo.models.length.toLocaleString()} model${row.combo.models.length === 1 ? "" : "s"}`;
  return `combo · ${strategy} · ${count}`;
}

/** Search matches the ref, the display name, the owning provider name, or the target. */
export function matchesSearch(
  row: UnifiedRow,
  query: string,
  displayOfProvider: (provider: string) => string,
): boolean {
  if (query === "") return true;
  const q = query.toLowerCase();
  if (row.kind === "model") {
    if (row.model === null) return row.ref.toLowerCase().includes(q);
    return (
      row.model.ref.toLowerCase().includes(q) ||
      row.model.id.toLowerCase().includes(q) ||
      (row.model.display ?? "").toLowerCase().includes(q) ||
      displayOfProvider(row.model.provider).toLowerCase().includes(q)
    );
  }
  if (row.ref.toLowerCase().includes(q) || row.name.toLowerCase().includes(q)) return true;
  return row.kind === "alias"
    ? row.alias.target.toLowerCase().includes(q)
    : row.combo.models.some((model) => model.toLowerCase().includes(q));
}
