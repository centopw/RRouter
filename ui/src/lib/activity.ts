/**
 * Pure helpers behind the Activity page: window arithmetic, axis scaling, series
 * colours and incident detection. Kept out of the components so each rule is one
 * function with one reason to change (and one place to test).
 */

export type RangeId = "1h" | "24h" | "7d" | "30d";

export type RangeSpec = {
  id: RangeId;
  /** Window length in hours. */
  hours: number;
  /** Server bucket width token (`?bucket=`). */
  bucket: "5m" | "hour" | "day";
  /** Bucket width in seconds, mirrored client-side to align empty cells. */
  seconds: number;
  /** How many cells the window holds when the API returns nothing. */
  cells: number;
  /** Show an x label every N cells (the last cell always gets one). */
  labelEvery: number;
};

export const RANGES: readonly RangeSpec[] = [
  { id: "1h", hours: 1, bucket: "5m", seconds: 300, cells: 12, labelEvery: 3 },
  { id: "24h", hours: 24, bucket: "hour", seconds: 3600, cells: 24, labelEvery: 6 },
  { id: "7d", hours: 168, bucket: "day", seconds: 86_400, cells: 7, labelEvery: 1 },
  { id: "30d", hours: 720, bucket: "day", seconds: 86_400, cells: 30, labelEvery: 5 },
];

export function rangeOf(id: RangeId): RangeSpec {
  return RANGES.find((r) => r.id === id) ?? RANGES[1]!;
}

/**
 * Aligned bucket start times: the last cell contains `now`, so traffic from the
 * current bucket is never dropped (a plain floor of `now - hours` can end the window
 * half a bucket early).
 */
export function windowStarts(spec: RangeSpec, now: number): number[] {
  const last = Math.floor(now / spec.seconds) * spec.seconds;
  const first = last - (spec.cells - 1) * spec.seconds;
  const out: number[] = [];
  for (let i = 0; i < spec.cells; i += 1) out.push(first + i * spec.seconds);
  return out;
}

/** A "nice" axis maximum: 1, 2 or 5 times a power of ten. */
export function niceMax(value: number): number {
  if (!Number.isFinite(value) || value <= 0) return 1;
  const exp = Math.floor(Math.log10(value));
  const base = 10 ** exp;
  const fraction = value / base;
  const step = fraction <= 1 ? 1 : fraction <= 2 ? 2 : fraction <= 5 ? 5 : 10;
  return step * base;
}

/** Indices that earn an x label: every `every` cells plus the final cell. */
export function xLabelIndices(count: number, every: number): number[] {
  if (count <= 0) return [];
  const step = Math.max(1, every);
  const out: number[] = [];
  for (let i = 0; i < count; i += step) out.push(i);
  if (out[out.length - 1] !== count - 1) out.push(count - 1);
  return out;
}

/** Index of the busiest cell; -1 for an all-zero window. */
export function peakIndex(values: number[]): number {
  let best = -1;
  let max = 0;
  values.forEach((v, i) => {
    if (v > max) {
      max = v;
      best = i;
    }
  });
  return best;
}

export function isToday(ts: number, now: number): boolean {
  const a = new Date(ts * 1000);
  const b = new Date(now * 1000);
  return (
    a.getFullYear() === b.getFullYear() &&
    a.getMonth() === b.getMonth() &&
    a.getDate() === b.getDate()
  );
}

export function bucketLabel(start: number, seconds: number, now: number): string {
  const d = new Date(start * 1000);
  if (seconds >= 86_400) {
    if (isToday(start, now)) return "Today";
    return d.toLocaleDateString(undefined, { month: "short", day: "numeric" });
  }
  return d.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" });
}

/** "Aug 3, 14:00 – 15:00" style range for the tooltip. */
export function bucketRangeLabel(start: number, seconds: number): string {
  const from = new Date(start * 1000);
  const to = new Date((start + seconds) * 1000);
  const date = from.toLocaleDateString(undefined, { month: "short", day: "numeric" });
  const t = (d: Date) => d.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" });
  return seconds >= 86_400 ? date : `${date}, ${t(from)}–${t(to)}`;
}

/**
 * Colour per model. Sorted input keeps the mapping stable across the chart, the
 * table and the log; everything past the fifth model is grouped as "other".
 */
export function seriesColors(idsInRankOrder: string[]): {
  ids: string[];
  colorOf: (id: string) => string;
  other: boolean;
} {
  const ids = [...new Set(idsInRankOrder)];
  const mapped = ids.slice(0, 5);
  const table = new Map(mapped.map((id, i) => [id, `var(--color-series-${i + 1})`]));
  return {
    ids: mapped,
    other: ids.length > 5,
    colorOf: (id) => table.get(id) ?? "var(--color-series-6)",
  };
}

export type Incident = {
  provider: string;
  model: string;
  requests: number;
  errors: number;
  rate: number;
};

/** The worst failing model in the window, if any clears the noise floor. */
export function incidentOf(
  rows: Array<{ provider: string; model: string; requests: number; errors: number }>,
  minFailures = 3,
  minRate = 0.2,
): Incident | null {
  let worst: Incident | null = null;
  for (const r of rows) {
    if (r.requests <= 0 || r.errors < minFailures) continue;
    const rate = r.errors / r.requests;
    if (rate <= minRate) continue;
    if (!worst || rate > worst.rate || (rate === worst.rate && r.errors > worst.errors)) {
      worst = { provider: r.provider, model: r.model, requests: r.requests, errors: r.errors, rate };
    }
  }
  return worst;
}

/** `▲ 12%` / `▼ 8%`, or null when the previous window has nothing to compare. */
export function deltaLabel(current: number, previous: number): { up: boolean; text: string } | null {
  if (previous <= 0) return null;
  const change = ((current - previous) / previous) * 100;
  if (!Number.isFinite(change) || Math.abs(change) < 0.05) return { up: true, text: "no change" };
  return { up: change > 0, text: `${change > 0 ? "▲" : "▼"} ${Math.abs(change).toFixed(0)}%` };
}

/**
 * Inline self-check, run in development only. The repo has no UI test runner, and
 * these rules are exactly the kind that break silently (an off-by-one axis, a lost
 * zero cell), so they assert themselves where a developer will see it.
 */
export function selfCheck(): string[] {
  const failures: string[] = [];
  const eq = (name: string, got: unknown, want: unknown) => {
    if (JSON.stringify(got) !== JSON.stringify(want)) {
      failures.push(`${name}: got ${JSON.stringify(got)}, want ${JSON.stringify(want)}`);
    }
  };
  eq("niceMax(0)", niceMax(0), 1);
  eq("niceMax(3)", niceMax(3), 5);
  eq("niceMax(7)", niceMax(7), 10);
  eq("niceMax(12)", niceMax(12), 20);
  eq("niceMax(400)", niceMax(400), 500);
  eq("xLabelIndices", xLabelIndices(24, 6), [0, 6, 12, 18, 23]);
  eq("peakIndex", peakIndex([0, 3, 9, 2]), 2);
  eq("peakIndex empty", peakIndex([0, 0]), -1);
  eq("windowStarts length", windowStarts(rangeOf("7d"), 1_700_000_000).length, 7);
  eq(
    "windowStarts ends at now's bucket",
    windowStarts(rangeOf("1h"), 1_700_000_123)[11],
    Math.floor(1_700_000_123 / 300) * 300,
  );
  eq("incident floor", incidentOf([{ provider: "p", model: "m", requests: 2, errors: 2 }]), null);
  eq(
    "incident worst",
    incidentOf([
      { provider: "a", model: "x", requests: 10, errors: 3 },
      { provider: "b", model: "y", requests: 10, errors: 6 },
    ])?.model,
    "y",
  );
  eq("series colors", seriesColors(["a", "b"]).colorOf("b"), "var(--color-series-2)");
  eq("series overflow", seriesColors(["a", "b", "c", "d", "e", "f"]).other, true);
  eq("delta null", deltaLabel(5, 0), null);
  eq("delta up", deltaLabel(112, 100), { up: true, text: "▲ 12%" });
  return failures;
}

if (import.meta.env.DEV) {
  const failures = selfCheck();
  if (failures.length > 0) {
    console.error("activity helper self-check failed", failures);
  }
}
