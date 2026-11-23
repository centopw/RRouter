/**
 * The pace forecast drives the /usage headline and its amber warning, so its edges are
 * pinned: no forecast without a window, exhausted is a fact even without one, a young
 * window is not extrapolated, and the headline is the earliest limit hit.
 *
 * Run with `npm test` (Node's built-in runner over the TS sources).
 */
import assert from "node:assert/strict";
import { test } from "node:test";
import type { AccountSubscription } from "../src/lib/api.ts";
import { pace, rankAccounts } from "../src/pages/usage/pace.ts";

const HOUR = 3600;
const DAY = 86_400;
/** A fixed clock (unix seconds) so nothing depends on when the test runs. */
const NOW_S = 1_800_000_000;
const NOW = NOW_S * 1000;

test("missing window_seconds or resets_at gives no forecast", () => {
  for (const input of [
    { used_percent: 40, window_seconds: null, resets_at: NOW_S + DAY },
    { used_percent: 40, window_seconds: 7 * DAY, resets_at: null },
    { used_percent: 40, window_seconds: 0, resets_at: NOW_S + DAY },
  ]) {
    const result = pace({ ...input, now: NOW });
    assert.equal(result.state, "unknown");
    assert.equal(result.paceDaily, null);
    assert.equal(result.projectedAtReset, null);
    assert.equal(result.limitHitAt, null);
  }
});

test("a reset already in the past is stale data, not a forecast", () => {
  const result = pace({ used_percent: 40, window_seconds: 7 * DAY, resets_at: NOW_S - 60, now: NOW });
  assert.equal(result.state, "unknown");
  assert.equal(result.limitHitAt, null);
});

test("a window under 5% elapsed is not extrapolated", () => {
  // 5-hour window, 10 minutes in (3.3%), already 10% used: would project 150%.
  const result = pace({
    used_percent: 10,
    window_seconds: 5 * HOUR,
    resets_at: NOW_S + 5 * HOUR - 600,
    now: NOW,
  });
  assert.equal(result.state, "unknown");
  assert.equal(result.startMs, (NOW_S - 600) * 1000);
  assert.equal(result.limitHitAt, null);
});

test("on pace: straight line from the window start, hit before reset", () => {
  // 30-day window, 10 days in, 50% used => 5%/day, 150% at reset, hits 100% on day 20.
  const result = pace({
    used_percent: 50,
    window_seconds: 30 * DAY,
    resets_at: NOW_S + 20 * DAY,
    now: NOW,
  });
  assert.equal(result.state, "on_pace_to_hit");
  assert.equal(result.startMs, (NOW_S - 10 * DAY) * 1000);
  assert.ok(Math.abs(result.paceDaily! - 5) < 1e-9);
  assert.ok(Math.abs(result.projectedAtReset! - 150) < 1e-9);
  assert.equal(Math.round(result.limitHitAt!), NOW + 10 * DAY * 1000);
  // 30 days is the gateway's monthly stand-in: flagged so the UI prints "~".
  assert.equal(result.approximate, true);
});

test("ok: under the limit at reset, no hit time", () => {
  // Weekly window, 3.5 days in, 21% used => 42% at reset.
  const result = pace({
    used_percent: 21,
    window_seconds: 7 * DAY,
    resets_at: NOW_S + 3.5 * DAY,
    now: NOW,
  });
  assert.equal(result.state, "ok");
  assert.ok(Math.abs(result.projectedAtReset! - 42) < 1e-9);
  assert.equal(result.limitHitAt, null);
  assert.equal(result.approximate, false);
});

test("zero use is ok with zero pace", () => {
  const result = pace({ used_percent: 0, window_seconds: 7 * DAY, resets_at: NOW_S + DAY, now: NOW });
  assert.equal(result.state, "ok");
  assert.equal(result.paceDaily, 0);
  assert.equal(result.limitHitAt, null);
});

test("exhausted holds even when the window is unknown", () => {
  const withWindow = pace({ used_percent: 100, window_seconds: 5 * HOUR, resets_at: NOW_S + HOUR, now: NOW });
  assert.equal(withWindow.state, "exhausted");
  assert.equal(withWindow.limitHitAt, NOW);
  const without = pace({ used_percent: 100, window_seconds: null, resets_at: null, now: NOW });
  assert.equal(without.state, "exhausted");
  assert.equal(without.paceDaily, null);
});

test("a window that reset after hitting 100% is not still exhausted", () => {
  // The cached reading says 100%, but its reset passed a minute ago: a new window is open.
  for (const window_seconds of [5 * HOUR, null]) {
    const result = pace({ used_percent: 100, window_seconds, resets_at: NOW_S - 60, now: NOW });
    assert.equal(result.state, "unknown");
    assert.equal(result.limitHitAt, null);
  }
});

function account(
  id: string,
  windows: Array<[used: number, seconds: number | null, resetsIn: number | null]>,
): AccountSubscription {
  return {
    connection_id: id,
    provider: "claude",
    name: id,
    plan: "max",
    error: null,
    fetched_at: NOW_S,
    windows: windows.map(([used, seconds, resetsIn], index) => ({
      id: `w${index}`,
      label: `w${index}`,
      used_percent: used,
      window_seconds: seconds,
      resets_at: resetsIn === null ? null : NOW_S + resetsIn,
    })),
  };
}

test("headline: earliest limit hit beats a higher used share", () => {
  const ranked = rankAccounts(
    [
      // 80% used but on track (weekly, 6 days in => 93% at reset).
      account("steady", [[80, 7 * DAY, DAY]]),
      // 40% used, 1 day into a weekly window => hits in 1.5 days.
      account("burning", [[40, 7 * DAY, 6 * DAY]]),
      account("silent", []),
    ],
    NOW,
  );
  assert.deepEqual(
    ranked.map((risk) => risk.account.connection_id),
    ["burning", "steady"],
  );
  assert.equal(ranked[0]!.worst.pace.state, "on_pace_to_hit");
});

test("headline: exhausted first; with no forecast anywhere, highest used share", () => {
  const exhausted = rankAccounts(
    [account("burning", [[40, 7 * DAY, 6 * DAY]]), account("done", [[100, null, null]])],
    NOW,
  );
  assert.equal(exhausted[0]!.account.connection_id, "done");

  const noForecast = rankAccounts(
    [account("low", [[30, null, null]]), account("high", [[70, null, null]])],
    NOW,
  );
  assert.deepEqual(
    noForecast.map((risk) => risk.account.connection_id),
    ["high", "low"],
  );
});

test("an account's worst window is its riskiest, not its first", () => {
  const [risk] = rankAccounts(
    [account("a", [[10, 5 * HOUR, 4 * HOUR], [60, 7 * DAY, 4 * DAY]])],
    NOW,
  );
  // Weekly: 3 days in at 60% => 140% at reset, hits before reset; 5-hour stays at 50%.
  assert.equal(risk!.worst.window.id, "w1");
  assert.equal(risk!.windows.length, 2);
});
