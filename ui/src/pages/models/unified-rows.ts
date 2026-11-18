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

export const STRATEGY_LABEL: Record<ComboStrategy, string> = {
  fallback: "fallback",
  round_robin: "round robin",
  smart: "smart",
};

/** The routable ref every list and preset entry uses for an alias/combo name. */
export function nameRef(name: string): string {
  return `rrouter/${name}`;
}

/**
 * The second line of a name row: what it resolves to, which its page owns. A combo carries
 * no aggregate metadata in Phase 1, so it says so rather than borrowing a member's numbers.
 */
export function nameDetail(row: AliasRow | ComboRow): string {
  if (row.kind === "alias") return `alias → ${row.alias.target}`;
  const count = `${row.combo.models.length.toLocaleString()} candidate${row.combo.models.length === 1 ? "" : "s"}`;
  return `${count} · metadata —`;
}

/**
 * The one row set of the Models page: registry models, then live aliases and combos at
 * `rrouter/<name>`, then preset entries no live row claimed.
 *
 * `aliases`/`combos` are `undefined` until their list answers. A stored `rrouter/…` ref
 * waits for both, so a name is never mistaken for a stale model mid-load; a registry ref
 * needs only `/api/models`. Names are claimed even when the preset never stored them, so an
 * unseen combo is a live row (and auto-include can adopt it) rather than a stale one.
 */
export function rowsFor({
  models,
  aliases,
  combos,
  entries,
  presetAware,
  autoInclude,
}: {
  models: Model[];
  aliases: Alias[] | undefined;
  combos: Combo[] | undefined;
  entries: ReadonlyMap<string, DraftEntry>;
  presetAware: boolean;
  autoInclude: boolean;
}): UnifiedRow[] {
  const rows: UnifiedRow[] = [];
  const claimed = new Set<string>();
  for (const model of models) {
    claimed.add(model.ref);
    const entry = entries.get(model.ref);
    rows.push({
      kind: "model",
      ref: model.ref,
      model,
      entry,
      status: statusOfModel(model, entry, presetAware),
    });
  }
  for (const alias of aliases ?? []) {
    const ref = nameRef(alias.alias);
    // One ref renders once. Routing resolves an alias before a combo of the same name
    // (alias → explicit provider/model → combo), so when a name exists as both, the
    // alias row owns the ref and the combo row is dropped. Registry refs are
    // provider-qualified and `rrouter` is reserved as the name namespace, so this guard
    // is a guarantee rather than the expected path.
    if (claimed.has(ref)) continue;
    claimed.add(ref);
    const entry = entries.get(ref);
    rows.push({
      kind: "alias",
      ref,
      name: alias.alias,
      alias,
      entry,
      status: statusOfName(entry, presetAware, autoInclude),
    });
  }
  for (const combo of combos ?? []) {
    const ref = nameRef(combo.name);
    // Already claimed by an alias (or a duplicate combo): same rule as above — the ref
    // is the row identity for React keys, the group select and the /v1 toggle.
    if (claimed.has(ref)) continue;
    claimed.add(ref);
    const entry = entries.get(ref);
    rows.push({
      kind: "combo",
      ref,
      name: combo.name,
      combo,
      entry,
      status: statusOfName(entry, presetAware, autoInclude),
    });
  }
  const namesLoaded = aliases !== undefined && combos !== undefined;
  for (const entry of entries.values()) {
    if (claimed.has(entry.ref)) continue;
    if (!namesLoaded && entry.ref.startsWith("rrouter/")) continue;
    // No kind is inferred for a ref nothing live answers: it keeps its stored group and
    // visibility, and the row copy never claims to know what it was.
    rows.push({
      kind: "model",
      ref: entry.ref,
      model: null,
      entry,
      status: entry.visible ? "routed" : "hidden",
    });
  }
  return rows;
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
