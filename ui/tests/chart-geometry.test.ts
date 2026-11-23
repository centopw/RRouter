/**
 * The limit chart must never draw usage it did not sample: a missing day breaks the line,
 * and the live reading replaces today's sample instead of adding a second point.
 *
 * Run with `npm test` (Node's built-in runner over the TS sources).
 */
import assert from "node:assert/strict";
import { test } from "node:test";
import {
  actualSegments,
  fitLabels,
  historyDrawable,
  linePath,
  scale,
  yTicks,
} from "../src/pages/usage/chart-geometry.ts";

const DAY = 86_400;
/** UTC midnight, so day buckets line up. */
const D0 = 1_799_971_200;
const START = D0 * 1000;

test("a missing day breaks the line into two runs", () => {
  const now = (D0 + 10 * DAY) * 1000;
  const segments = actualSegments(
    [
      { day: D0, used_percent: 5 },
      { day: D0 + DAY, used_percent: 10 },
      // D0 + 2 days missing
      { day: D0 + 3 * DAY, used_percent: 20 },
      { day: D0 + 4 * DAY, used_percent: 25 },
    ],
    START,
    now,
  );
  assert.equal(segments.length, 2);
  assert.deepEqual(
    segments.map((run) => run.map((point) => point.v)),
    [[5, 10], [20, 25]],
  );
  // A day's maximum sits at the end of that day.
  assert.equal(segments[0]![0]!.t, (D0 + DAY) * 1000);
});

test("points clamp to the window start and to now", () => {
  const now = (D0 + 2 * DAY) * 1000 + 3_600_000; // 1h into day 2
  const segments = actualSegments(
    [
      { day: D0 - DAY, used_percent: 1 },
      { day: D0 + 2 * DAY, used_percent: 9 },
    ],
    (D0 - DAY / 2) * 1000,
    now,
  );
  // The two samples are three days apart: separate runs.
  assert.equal(segments.length, 2);
  assert.equal(segments[0]![0]!.t, D0 * 1000);
  assert.equal(segments[1]![0]!.t, now);
});

test("the live reading replaces today's sample and joins yesterday's run", () => {
  const now = (D0 + 2 * DAY) * 1000 + 3_600_000;
  const segments = actualSegments(
    [
      { day: D0 + DAY, used_percent: 30 },
      { day: D0 + 2 * DAY, used_percent: 31 },
    ],
    START,
    now,
    34,
  );
  assert.equal(segments.length, 1);
  assert.deepEqual(segments[0], [
    { t: (D0 + 2 * DAY) * 1000, v: 30 },
    { t: now, v: 34 },
  ]);
});

test("the live reading after a gap starts its own run", () => {
  const now = (D0 + 5 * DAY) * 1000;
  const segments = actualSegments([{ day: D0, used_percent: 10 }], START, now, 40);
  assert.equal(segments.length, 2);
});

test("linePath moves once per run", () => {
  const x = scale(0, 10, 0, 100);
  const y = scale(0, 100, 50, 0);
  assert.equal(
    linePath([{ t: 0, v: 0 }, { t: 5, v: 50 }, { t: 10, v: 100 }], x, y),
    "M0.0,50.0L50.0,25.0L100.0,0.0",
  );
  assert.equal(scale(3, 3, 7, 9)(100), 7);
});

test("y axis reaches 100% at least, in at most four even steps", () => {
  assert.deepEqual(yTicks(40), { top: 100, ticks: [0, 25, 50, 75, 100] });
  assert.deepEqual(yTicks(315), { top: 400, ticks: [0, 100, 200, 300, 400] });
  assert.deepEqual(yTicks(Number.NaN).top, 100);
});

test("overlapping x labels keep the most important", () => {
  const kept = fitLabels(
    [
      { key: "start", x: 0, text: "Nov 1", priority: 3 },
      { key: "today", x: 20, text: "Today", priority: 1 },
      { key: "limit", x: 70, text: "Nov 27", priority: 0 },
      { key: "reset", x: 200, text: "Dec 1", priority: 2 },
    ],
    48,
  );
  assert.deepEqual(
    kept.map((label) => label.key),
    // "Nov 1" sits 20px from "Today" and gives way; the rest are 48px+ apart.
    ["today", "limit", "reset"],
  );
});

test("rolling windows without window_start are not drawn; monthly ones are", () => {
  const data = [
    { day: D0, used_percent: 10 },
    { day: D0 + DAY, used_percent: 20 },
  ];
  const unbounded = { connection_id: "c", window: "w", window_start: null, data };
  const bounded = { ...unbounded, window_start: D0 };
  const weekly = { id: "weekly", label: "Weekly", window_seconds: 7 * DAY };
  const fiveHour = { id: "five_hour", label: "5 hour", window_seconds: 5 * 3600 };
  const monthly = { id: "monthly", label: "Monthly", window_seconds: 30 * DAY };
  const weeklyUnknownLength = { id: "weekly", label: "Weekly", window_seconds: null };

  assert.equal(historyDrawable(unbounded, weekly), false);
  assert.equal(historyDrawable(unbounded, fiveHour), false);
  assert.equal(historyDrawable(unbounded, weeklyUnknownLength), false);
  assert.equal(historyDrawable(unbounded, monthly), true);
  assert.equal(historyDrawable(bounded, weekly), true);
  // One sample is never a curve.
  assert.equal(historyDrawable({ ...bounded, data: data.slice(0, 1) }, monthly), false);
});
