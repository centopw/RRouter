import type { Model } from "../../lib/api.ts";
import { rowIsAvailable, rowIsFree, type UnifiedRow } from "./unified-rows.ts";

/**
 * The URL vocabulary of the unified Models table, and nothing else.
 *
 * One page, one row set, one pagination cursor. The old in-page views (`?view=active`,
 * `registry`, `names`) and their private cursors (`ipage`, `dpage`) are gone; a legacy
 * link is rewritten into this vocabulary once on mount by {@link mapLegacyView}.
 */

/** "every provider", as the provider filter spells it. */
export const ALL = "all";

/** Page sizes the table offers; 50 is the default because it fills a laptop viewport. */
export const SIZES = [25, 50, 100, 200] as const;
export const DEFAULT_SIZE = 50;

/** The states one row can be in, relative to the preset being edited. */
export const STATUSES = ["routed", "hidden", "disabled", "unrouted"] as const;
export type StatusKey = (typeof STATUSES)[number];

export const STATUS_LABELS: Record<StatusKey, string> = {
  routed: "Routed",
  hidden: "Hidden",
  disabled: "Disabled",
  unrouted: "Not in preset",
};

/** Admin default: the preset's board, which is what the page is for. */
export const DEFAULT_STATUSES: readonly StatusKey[] = ["routed"];

/**
 * Parse `?status=`. A missing or unreadable value falls back to the default; `all` is
 * accepted because a hand-written link should not need to spell four names.
 */
export function statusesOf(params: URLSearchParams): Set<StatusKey> {
  const raw = params.get("status");
  if (raw === null) return new Set(DEFAULT_STATUSES);
  if (raw.trim().toLowerCase() === "all") return new Set(STATUSES);
  const chosen = raw
    .split(",")
    .map((value) => value.trim())
    .filter((value): value is StatusKey => (STATUSES as readonly string[]).includes(value));
  return chosen.length > 0 ? new Set(chosen) : new Set(DEFAULT_STATUSES);
}

/** Serialize a status set; `null` means "drop the parameter" (the default). */
export function statusParam(chosen: ReadonlySet<StatusKey>): string | null {
  const ordered = STATUSES.filter((status) => chosen.has(status));
  if (ordered.length === STATUSES.length) return STATUSES.join(",");
  if (ordered.length === 1 && ordered[0] === "routed") return null;
  return ordered.length === 0 ? null : ordered.join(",");
}

/** Old state maps forward: `view=*` becomes filters, and the private cursors disappear. */
export function mapLegacyView(params: URLSearchParams): boolean {
  const view = params.get("view");
  let changed = false;
  if (view !== null) {
    if (view === "active") params.delete("status");
    else if (view === "registry") params.set("status", STATUSES.join(","));
    // Aliases and combos have their own screens now; an old link lands on the models
    // they used to be mixed into, with the closest status filter.
    else if (view === "names") params.set("status", "routed,hidden");
    params.delete("view");
    changed = true;
  }
  // The old `names` toggle is gone; a bookmarked `?names=1` must not survive.
  if (params.has("names")) {
    params.delete("names");
    changed = true;
  }
  for (const key of ["ipage", "dpage"]) {
    if (params.has(key)) {
      params.delete(key);
      changed = true;
    }
  }
  return changed;
}

/** A requested page size that is not offered falls back instead of shrinking the table. */
export function sizeOf(params: URLSearchParams): number {
  const requested = Number(params.get("size") ?? DEFAULT_SIZE);
  return (SIZES as readonly number[]).includes(requested) ? requested : DEFAULT_SIZE;
}

/** A page number clamped to what the current row count can show. */
export function pageOf(params: URLSearchParams, key: string, total: number, size: number): number {
  const last = Math.max(1, Math.ceil(total / size));
  return Math.min(Math.max(1, Number(params.get(key) ?? "1") || 1), last);
}

/**
 * Label for a preset in the selector. Drops the redundant " · default" suffix when
 * the preset is already named "Default" (case-insensitive).
 */
export function presetOptionLabel(name: string, isDefault: boolean): string {
  if (!isDefault) return name;
  return name.trim().toLowerCase() === "default" ? name : `${name} · default`;
}

/** True when the status selection is the default admin board (`Routed` only). */
export function isDefaultStatusSet(statuses: ReadonlySet<StatusKey>): boolean {
  return statuses.size === 1 && statuses.has("routed");
}

/**
 * Count non-default filters controlled inside the filter bar / phone filter sheet.
 * Search (`q`) sits inline beside the phone Filters button, so it is excluded here.
 */
export function countActiveFilters({
  provider,
  kind,
  statuses,
  statusAware,
  free,
  custom,
}: {
  provider: string;
  kind: string;
  statuses: ReadonlySet<StatusKey>;
  statusAware: boolean;
  free: boolean;
  custom: boolean;
}): number {
  let count = 0;
  if (provider !== ALL) count += 1;
  if (kind !== "all") count += 1;
  if (statusAware && !isDefaultStatusSet(statuses)) count += 1;
  if (free) count += 1;
  if (custom) count += 1;
  return count;
}

export type ModelKpiSummary = {
  total: number;
  /** Non-disabled rows (denominator for reader availability so disabled-on-purpose never degrades readiness). */
  enabledTotal: number;
  routed: number;
  routedAvailable: number;
  routedUnavailable: number;
  available: number;
  free: number;
  routedFree: number;
  hidden: number;
  unrouted: number;
  disabled: number;
  /** Rows outside the routed board (`hidden + unrouted + disabled`). */
  notRouted: number;
  /** Non-disabled rows that cannot currently serve (the sole count under "Needs attention"). */
  unavailable: number;
  /** Preset status buckets containing at least one non-disabled unavailable row. */
  unavailableStatuses: StatusKey[];
  /** Non-empty preset status buckets among `hidden`, `unrouted`, `disabled`. */
  notRoutedStatuses: StatusKey[];
};

const NOT_ROUTED_ORDER: readonly StatusKey[] = ["hidden", "unrouted", "disabled"];

/**
 * Real-data counts for the top KPI strip across the unified row set.
 * Disabled rows are counted strictly under `disabled` / `notRouted` and never under
 * `unavailable` ("Needs attention"), so operator-disabled models never read as degraded.
 */
export function summarizeModelKpis(
  rows: readonly UnifiedRow[],
  modelByRef: ReadonlyMap<string, Model>,
): ModelKpiSummary {
  let routed = 0;
  let routedAvailable = 0;
  let available = 0;
  let free = 0;
  let routedFree = 0;
  let hidden = 0;
  let unrouted = 0;
  let disabled = 0;
  let unavailable = 0;
  const unavailableStatusSet = new Set<StatusKey>();

  for (const row of rows) {
    const isAvail = rowIsAvailable(row, modelByRef);
    const isFreeRow = rowIsFree(row, modelByRef);

    if (row.status === "routed") {
      routed += 1;
      if (isAvail) routedAvailable += 1;
      if (isFreeRow) routedFree += 1;
    } else if (row.status === "hidden") {
      hidden += 1;
    } else if (row.status === "unrouted") {
      unrouted += 1;
    } else if (row.status === "disabled") {
      disabled += 1;
    }

    if (isAvail) {
      available += 1;
    } else if (row.status !== "disabled") {
      unavailable += 1;
      if (row.status !== "catalog") {
        unavailableStatusSet.add(row.status);
      }
    }

    if (isFreeRow) free += 1;
  }

  const notRoutedCounts: Record<StatusKey, number> = {
    routed,
    hidden,
    unrouted,
    disabled,
  };

  return {
    total: rows.length,
    enabledTotal: rows.length - disabled,
    routed,
    routedAvailable,
    routedUnavailable: routed - routedAvailable,
    available,
    free,
    routedFree,
    hidden,
    unrouted,
    disabled,
    notRouted: hidden + unrouted + disabled,
    unavailable,
    unavailableStatuses: STATUSES.filter((status) => unavailableStatusSet.has(status)),
    notRoutedStatuses: NOT_ROUTED_ORDER.filter((status) => notRoutedCounts[status] > 0),
  };
}
