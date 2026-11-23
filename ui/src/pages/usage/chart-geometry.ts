/**
 * Geometry for the /usage limit chart, kept pure so its edges are unit-tested: where the
 * sampled points sit in time, where the line breaks, the y ticks, and which x labels fit.
 * No React, no clock: the caller passes `now`.
 */

import type { AccountUsageHistory, SubscriptionWindow, UsageHistoryPoint } from "../../lib/api";

const DAY_S = 86_400;
const DAY_MS = DAY_S * 1000;

/**
 * Whether the history can be drawn as one window's curve. It needs two samples, and for a
 * rolling window (5-hour, weekly) it needs `window_start`: without it the server could not
 * bound the query to the current window, so after a restart the samples may mix earlier
 * windows' readings into this one.
 */
export function historyDrawable(
  history: AccountUsageHistory,
  window: Pick<SubscriptionWindow, "id" | "label" | "window_seconds">,
): boolean {
  if (history.data.length < 2) return false;
  if (history.window_start !== null) return true;
  const rolling =
    window.window_seconds !== null
      ? window.window_seconds <= 7 * DAY_S
      : /hour|week/i.test(`${window.id} ${window.label}`);
  return !rolling;
}

/** A plotted point: time in ms, value in percent. */
export type Pt = { t: number; v: number };

/** `[d0, d1] -> [r0, r1]`, linear. A zero-width domain maps everything to `r0`. */
export function scale(d0: number, d1: number, r0: number, r1: number): (value: number) => number {
  const span = d1 - d0;
  return (value) => (span === 0 ? r0 : r0 + ((value - d0) / span) * (r1 - r0));
}

/**
 * The sampled curve as runs of consecutive days. A day's value is that day's highest
 * reading, which usage reaches by the day's end, so it sits at the end of its day (clamped
 * to the window start and to `now`). `live`, the current reading, replaces today's sample
 * so the line ends where the forecast starts. A missing day breaks the line: no
 * interpolation across a gap in sampling.
 */
export function actualSegments(
  data: UsageHistoryPoint[],
  startMs: number,
  now: number,
  live?: number,
): Pt[][] {
  const today = Math.floor(now / DAY_MS) * DAY_S;
  const days = data
    .filter((point) => live === undefined || point.day !== today)
    .map((point) => ({ day: point.day, v: point.used_percent }));
  if (live !== undefined) days.push({ day: today, v: live });
  days.sort((a, b) => a.day - b.day);

  const segments: Pt[][] = [];
  let previous: number | null = null;
  for (const { day, v } of days) {
    const t = Math.max(startMs, Math.min(now, (day + DAY_S) * 1000));
    if (previous === null || day - previous > DAY_S) segments.push([]);
    segments[segments.length - 1]!.push({ t, v });
    previous = day;
  }
  return segments;
}

/** SVG path data through one run of points. */
export function linePath(points: Pt[], x: (t: number) => number, y: (v: number) => number): string {
  return points
    .map((point, index) => `${index === 0 ? "M" : "L"}${x(point.t).toFixed(1)},${y(point.v).toFixed(1)}`)
    .join("");
}

const STEPS = [25, 50, 100, 250, 500, 1000, 2500];

/** Y axis from 0 to at least `max` (never below 100%), in at most 4 even steps. */
export function yTicks(max: number): { top: number; ticks: number[] } {
  const target = Math.max(100, Number.isFinite(max) ? max : 100);
  const step = STEPS.find((candidate) => Math.ceil(target / candidate) <= 4) ?? STEPS[STEPS.length - 1]!;
  const top = Math.ceil(target / step) * step;
  const ticks: number[] = [];
  for (let tick = 0; tick <= top; tick += step) ticks.push(tick);
  return { top, ticks };
}

export type XLabel = {
  key: string;
  /** Position in px. */
  x: number;
  text: string;
  /** Lower wins when two labels would overlap. */
  priority: number;
};

/** Keep the labels that fit `minGap` px apart, most important first; returned left to right. */
export function fitLabels(labels: XLabel[], minGap: number): XLabel[] {
  const kept: XLabel[] = [];
  for (const label of [...labels].sort((a, b) => a.priority - b.priority)) {
    if (kept.every((other) => Math.abs(other.x - label.x) >= minGap)) kept.push(label);
  }
  return kept.sort((a, b) => a.x - b.x);
}
