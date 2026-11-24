import type { Alias, Capabilities, Combo, ComboStrategy, Model } from "../../lib/api";
import type { DraftEntry } from "../../lib/layout-draft";
import { compact } from "../../lib/format.ts";
import { isFree, rangeText, type NumberRange } from "./price.ts";

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
 * Per-member resolution from `combo_meta.rs` (additive wire on `/api/combos`).
 * `Combo` in `lib/api.ts` predates it, so it is read additively with fallback to `models[]`.
 */
export type ComboMemberState = "exact" | "fanout" | "core" | "disabled" | "unknown";

export type ComboMember = {
  ref: string;
  state: ComboMemberState;
  via_alias?: string | null;
  model: Model | null;
  targets: Model[];
};

export type ComboPricing = {
  input: NumberRange;
  output: NumberRange;
  cache_read?: NumberRange;
  cache_write?: NumberRange;
  known: number;
  total: number;
};

/**
 * The fold `/api/combos` sends (`combo_meta.rs`, additive wire). Older gateways omit it,
 * in which case the row shows the candidate count alone rather than borrowing member 1's numbers.
 */
export type ComboAggregate = {
  context: NumberRange | null;
  max_output: NumberRange | null;
  pricing: ComboPricing | null;
  guaranteed_capabilities?: Capabilities | null;
  supported_capabilities?: Capabilities | null;
  deprecated_members?: number;
  all_deprecated?: boolean;
  availability: "ok" | "no-targets" | "router-unavailable";
  router?: { ref: string; model: Model | null } | null;
};

type ComboWithMeta = Combo & {
  members?: ComboMember[] | null;
  aggregate?: ComboAggregate | null;
};

export function aggregateOf(combo: Combo): ComboAggregate | null {
  return (combo as ComboWithMeta).aggregate ?? null;
}

export function membersOf(combo: Combo): ComboMember[] | null {
  const members = (combo as ComboWithMeta).members;
  return Array.isArray(members) ? members : null;
}

/**
 * Qualified token ceiling range for a combo. Never mimics a single model's bare `ctx 256K`:
 * equal bounds say `all candidates`, unequal bounds say `guaranteed … · up to …`, and
 * missing bounds stay `null` so nothing is invented.
 */
export function qualifiedTokenRange(
  range: NumberRange | null | undefined,
  unit: string,
): string | null {
  if (!range) return null;
  if (range.min !== null && range.max !== null) {
    if (range.min === range.max) return `all candidates ${compact(range.min)} ${unit}`;
    return `guaranteed ${compact(range.min)} · up to ${compact(range.max)} ${unit}`;
  }
  if (range.min !== null) return `guaranteed ${compact(range.min)} ${unit}`;
  if (range.max !== null) return `up to ${compact(range.max)} ${unit}`;
  return null;
}

/**
 * Free only when every candidate has a known price and the most expensive one is zero on
 * every published dimension: one unpriced member may route to a paid model, so not free.
 */
export function comboIsFree(
  aggregate: ComboAggregate | null,
  combo?: Combo,
  modelByRef?: ReadonlyMap<string, Model>,
): boolean {
  const pricing = aggregate?.pricing;
  if (pricing) {
    const cacheReadMax = pricing.cache_read?.max ?? 0;
    const cacheWriteMax = pricing.cache_write?.max ?? 0;
    return Boolean(
      pricing.total > 0 &&
        pricing.known === pricing.total &&
        pricing.input.max === 0 &&
        pricing.output.max === 0 &&
        cacheReadMax === 0 &&
        cacheWriteMax === 0,
    );
  }
  // Fallback only when an older gateway omits `aggregate`: require every stored candidate
  // to resolve in the registry with a proven zero price.
  if (!aggregate && combo && combo.models.length > 0 && modelByRef) {
    return combo.models.every((ref) => {
      const m = modelByRef.get(ref);
      return m !== undefined && isFree(m.cost);
    });
  }
  return false;
}

/** Qualified candidate price text for a combo; null when free (the badge carries it). */
export function comboPriceText(aggregate: ComboAggregate | null): string | null {
  if (!aggregate) return null;
  if (comboIsFree(aggregate)) return "Free";
  const pricing = aggregate.pricing;
  if (!pricing) return "price —";
  const input = rangeText(pricing.input);
  const output = rangeText(pricing.output);
  if (!input || !output) return "price —";
  const equal =
    pricing.input.min === pricing.input.max && pricing.output.min === pricing.output.max;
  const base = equal
    ? `all candidates ${input} in · ${output} out /1M`
    : `${input} in · ${output} out /1M · candidate range`;
  return pricing.known < pricing.total ? `${base} (${pricing.known}/${pricing.total} priced)` : base;
}

/** Why a combo cannot route right now, in the row's words; null when it can. */
export function comboBlocked(aggregate: ComboAggregate | null): string | null {
  if (!aggregate) return null;
  if (aggregate.availability === "no-targets") return "no candidate is connected";
  if (aggregate.availability === "router-unavailable") return "router model unavailable";
  return null;
}

/**
 * Capability split for a combo: `guaranteed` (every candidate supports) and `someOnly`
 * (supported by at least one candidate, not all). Both are `null` when unknown.
 */
export function comboCapabilitySplit(aggregate: ComboAggregate | null): {
  guaranteed: Array<keyof Capabilities>;
  someOnly: Array<keyof Capabilities>;
  known: boolean;
} {
  const g = aggregate?.guaranteed_capabilities;
  const s = aggregate?.supported_capabilities;
  if (!g && !s) return { guaranteed: [], someOnly: [], known: false };
  const keys = [
    "vision",
    "pdf",
    "audio_input",
    "video_input",
    "image_output",
    "audio_output",
    "tools",
    "reasoning",
    "search",
  ] as const satisfies ReadonlyArray<keyof Capabilities>;
  const guaranteed = g ? keys.filter((key) => g[key] === true) : [];
  const guaranteedSet = new Set(guaranteed);
  const someOnly = s ? keys.filter((key) => s[key] === true && !guaranteedSet.has(key)) : [];
  return { guaranteed, someOnly, known: true };
}

/**
 * The second line of a name row. An alias names its target. A combo states the candidate
 * range the gateway folded (guaranteed floor to best case), never one member's numbers;
 * without the fold it shows the candidate count alone.
 */
export function nameDetail(row: AliasRow | ComboRow): string {
  if (row.kind === "alias") return `alias → ${row.alias.target}`;
  const n = row.combo.models.length;
  const count = `${n.toLocaleString()} candidate${n === 1 ? "" : "s"}`;
  const aggregate = aggregateOf(row.combo);
  if (!aggregate) return count;
  const parts = [count];
  const ctx = qualifiedTokenRange(aggregate.context, "ctx");
  parts.push(ctx ?? "ctx —");
  const price = comboPriceText(aggregate);
  if (price && price !== "Free") parts.push(price);
  return parts.join(" · ");
}

/**
 * Normalize the synthetic `rrouter` provider bucket to "RRouter names" unless the operator
 * explicitly renamed the group to something else.
 */
export function groupDisplayName(group: { name: string; provider?: string }): string {
  if (group.provider === "rrouter" && group.name.toLowerCase() === "rrouter") {
    return "RRouter names";
  }
  return group.name;
}

export type RowStatusKind =
  | "routed"
  | "available"
  | "unavailable"
  | "hidden"
  | "unrouted"
  | "disabled";

export type RowStatusMeta = {
  kind: RowStatusKind;
  label: string;
  tone: "pos" | "warn" | "info" | "neutral";
  dot: "pos" | "warn" | "info" | "muted";
  title: string;
};

/**
 * Single status vocabulary for every Models row, aligned with Usage's HealthBadge tones:
 * Routed / Available (pos) · Unavailable / Hidden (warn) · Not in preset (info) · Disabled (neutral).
 */
export function rowStatusMeta(
  row: UnifiedRow,
  modelByRef: ReadonlyMap<string, Model>,
): RowStatusMeta {
  if (row.status === "disabled") {
    return {
      kind: "disabled",
      label: "Disabled",
      tone: "neutral",
      dot: "muted",
      title: "Disabled globally — the gateway refuses this id on every endpoint",
    };
  }
  if (row.status === "hidden") {
    return {
      kind: "hidden",
      label: "Hidden",
      tone: "warn",
      dot: "warn",
      title: "Hidden from /v1/models in this preset",
    };
  }
  if (row.status === "unrouted") {
    return {
      kind: "unrouted",
      label: "Not in preset",
      tone: "info",
      dot: "info",
      title: "In the registry, not included in this preset",
    };
  }
  if (!rowIsAvailable(row, modelByRef)) {
    const reason =
      row.kind === "model"
        ? row.model === null
          ? "No connection lists this model right now"
          : "No healthy connection for this provider right now"
        : row.kind === "alias"
          ? "Target model is disabled or not connected"
          : (comboBlocked(aggregateOf(row.combo)) ?? "No candidate is connected");
    return {
      kind: "unavailable",
      label: "Unavailable",
      tone: "warn",
      dot: "warn",
      title: reason,
    };
  }
  if (row.status === "routed") {
    return {
      kind: "routed",
      label: "Routed",
      tone: "pos",
      dot: "pos",
      title: "Listed on /v1/models and ready to serve",
    };
  }
  return {
    kind: "available",
    label: "Available",
    tone: "pos",
    dot: "pos",
    title: "Ready to serve",
  };
}

/**
 * Whether a row is proven free: a model priced at zero, an alias whose target is, or a
 * combo whose every candidate is. A row whose price is unknown is never free.
 */
export function rowIsFree(
  row: UnifiedRow,
  modelByRef: ReadonlyMap<string, Model>,
): boolean {
  if (row.kind === "model") return isFree(row.model?.cost);
  if (row.kind === "alias") return isFree(modelByRef.get(row.alias.target)?.cost);
  return comboIsFree(aggregateOf(row.combo), row.combo, modelByRef);
}

/**
 * Single source of truth for whether a row can actually serve a request right now.
 * Separate from preset visibility (`routed`/`hidden`) so header summaries and row badges
 * can never disagree.
 */
export function rowIsAvailable(
  row: UnifiedRow,
  modelByRef: ReadonlyMap<string, Model>,
): boolean {
  if (row.kind === "model") {
    return row.model !== null && row.model.enabled && row.model.available;
  }
  if (row.kind === "alias") {
    const target = modelByRef.get(row.alias.target);
    return target !== undefined && target.enabled && target.available;
  }
  const aggregate = aggregateOf(row.combo);
  if (aggregate) return aggregate.availability === "ok";
  return row.combo.models.some((ref) => {
    const candidate = modelByRef.get(ref);
    return candidate !== undefined && candidate.enabled && candidate.available;
  });
}

export type RowsSummary = {
  total: number;
  routed: number;
  available: number;
  routedAvailable: number;
  free: number;
  routedFree: number;
};

/** One summary pass over the unified row set so subtitle and badges always agree. */
export function summarizeRows(
  rows: readonly UnifiedRow[],
  modelByRef: ReadonlyMap<string, Model>,
): RowsSummary {
  let routed = 0;
  let available = 0;
  let routedAvailable = 0;
  let free = 0;
  let routedFree = 0;
  for (const row of rows) {
    const isRouted = row.status === "routed";
    const isAvail = rowIsAvailable(row, modelByRef);
    const freeRow = rowIsFree(row, modelByRef);
    if (isRouted) routed += 1;
    if (isAvail) available += 1;
    if (isRouted && isAvail) routedAvailable += 1;
    if (freeRow) free += 1;
    if (isRouted && freeRow) routedFree += 1;
  }
  return {
    total: rows.length,
    routed,
    available,
    routedAvailable,
    free,
    routedFree,
  };
}

export type EmptyStateKind =
  | "error"
  | "filtered_free"
  | "filtered"
  | "preset_empty"
  | "all_disabled"
  | "all_hidden"
  | "no_routed"
  | "none_hidden"
  | "none_disabled"
  | "none_unrouted"
  | "empty";

/**
 * Pure empty-state classifier so filters (`kind`, `free`, `provider`, `custom`, `q`) and
 * API errors never produce a false "This preset has no models" message.
 */
export function emptyStateKind({
  hasError,
  query,
  providerIsAll,
  kindIsAll,
  customOnly,
  freeOnly,
  statusAware,
  statuses,
  draftEntryCount,
  counts,
}: {
  hasError: boolean;
  query: string;
  providerIsAll: boolean;
  kindIsAll: boolean;
  customOnly: boolean;
  freeOnly: boolean;
  statusAware: boolean;
  statuses: ReadonlySet<string>;
  draftEntryCount: number;
  counts: { routed: number; hidden: number; disabled: number; unrouted: number };
}): EmptyStateKind {
  if (hasError) return "error";
  if (freeOnly) return "filtered_free";
  if (query !== "" || !providerIsAll || !kindIsAll || customOnly) return "filtered";
  if (statusAware && statuses.size === 1) {
    if (statuses.has("routed")) {
      if (draftEntryCount === 0 && counts.unrouted > 0) return "preset_empty";
      if (counts.disabled > 0 && counts.hidden === 0) return "all_disabled";
      if (counts.hidden > 0) return "all_hidden";
      if (counts.unrouted > 0) return "preset_empty";
      return "no_routed";
    }
    if (statuses.has("hidden")) return "none_hidden";
    if (statuses.has("disabled")) return "none_disabled";
    if (statuses.has("unrouted")) return "none_unrouted";
  }
  return "empty";
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
    // alias row owns the ref and the combo row is dropped.
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
