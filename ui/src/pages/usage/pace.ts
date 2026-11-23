/**
 * Pace and forecast for one provider-reported plan window, computed client-side from the
 * three numbers `/api/accounts/usage` already returns: `used_percent`, `window_seconds`
 * and `resets_at`. Pure (no React, no clock): the caller passes `now`.
 *
 *   start     = resets_at - window_seconds
 *   pace      = used% / elapsed            (straight line from the window start)
 *   projected = pace * window_seconds      (where the line lands at reset)
 *   hit       = now + (100 - used%) / pace (only when it lands before reset)
 *
 * Nothing is invented: a window without `window_seconds` or `resets_at`, a reset already in
 * the past, or a window too young to extrapolate returns `state: "unknown"` with no numbers.
 *
 * Caveat: the gateway hard-codes some monthly windows as 30 days (OpenCode Go "Monthly",
 * provider_usage.rs:339, and the GitHub Copilot monthly window). Calendar months run 28–31
 * days, so for those the derived start can be off by a day; `approximate` flags them and the
 * UI prints the forecast date with a "~".
 */

import type { AccountSubscription, SubscriptionWindow } from "../../lib/api";

export type PaceState = "ok" | "on_pace_to_hit" | "exhausted" | "unknown";

export type PaceInput = {
  used_percent: number;
  /** Window length in seconds; `null` when the provider did not say. */
  window_seconds: number | null;
  /** Unix seconds when the window rolls over; `null` when the provider did not say. */
  resets_at: number | null;
  /** Wall clock in milliseconds (`Date.now()`). */
  now: number;
};

export type Pace = {
  /** Window start in ms, `null` when it cannot be derived. */
  startMs: number | null;
  /** Percentage points consumed per day at the current average rate. */
  paceDaily: number | null;
  /** Where the current rate lands at reset, in percent (may exceed 100). */
  projectedAtReset: number | null;
  /** When the limit is (or was) reached, in ms; `null` when it is not reached this window. */
  limitHitAt: number | null;
  state: PaceState;
  /** The window length is a gateway hard-code (30 days), not the provider's own number. */
  approximate: boolean;
};

const DAY_MS = 86_400_000;
/** The 30-day stand-in the gateway uses when a provider only says "monthly". */
const HARD_CODED_MONTH_SECONDS = 30 * 24 * 3600;
/** Below this share of the window elapsed, a straight-line forecast is noise. */
const MIN_ELAPSED_SHARE = 0.05;

const UNKNOWN: Pace = {
  startMs: null,
  paceDaily: null,
  projectedAtReset: null,
  limitHitAt: null,
  state: "unknown",
  approximate: false,
};

export function pace({ used_percent, window_seconds, resets_at, now }: PaceInput): Pace {
  const used = Number.isFinite(used_percent) ? Math.max(0, used_percent) : 0;
  const hasWindow = window_seconds !== null && window_seconds > 0 && resets_at !== null;
  const approximate = window_seconds === HARD_CODED_MONTH_SECONDS;
  const windowMs = hasWindow ? window_seconds * 1000 : 0;
  const resetMs = hasWindow ? resets_at * 1000 : 0;
  const startMs = hasWindow ? resetMs - windowMs : null;

  // A reset already behind us means the reading is from the previous window (the
  // subscription cache can lag a reset by minutes): nothing it says holds now, not even
  // "exhausted". Checked on `resets_at` alone, so it covers windows of unknown length too.
  if (resets_at !== null && now >= resets_at * 1000) return { ...UNKNOWN, approximate };

  // Exhausted is a fact, not a forecast: it holds even when the window length is unknown.
  if (used >= 100) {
    const elapsed = startMs === null ? 0 : now - startMs;
    return {
      startMs,
      paceDaily: elapsed > 0 ? (used / elapsed) * DAY_MS : null,
      projectedAtReset: null,
      limitHitAt: now,
      state: "exhausted",
      approximate,
    };
  }

  if (startMs === null) return { ...UNKNOWN, approximate };

  const elapsed = now - startMs;
  if (elapsed <= 0 || elapsed < windowMs * MIN_ELAPSED_SHARE) {
    return { ...UNKNOWN, startMs, approximate };
  }

  const ratePerMs = used / elapsed;
  const projectedAtReset = ratePerMs * windowMs;
  let limitHitAt: number | null = null;
  if (ratePerMs > 0) {
    const hit = now + (100 - used) / ratePerMs;
    if (hit < resetMs) limitHitAt = hit;
  }
  return {
    startMs,
    paceDaily: ratePerMs * DAY_MS,
    projectedAtReset,
    limitHitAt,
    state: limitHitAt === null ? "ok" : "on_pace_to_hit",
    approximate,
  };
}

export type WindowRisk = { window: SubscriptionWindow; pace: Pace };

export type AccountRisk = {
  account: AccountSubscription;
  /** Every reported window, riskiest first. */
  windows: WindowRisk[];
  /** `windows[0]`: the window that decides this account's place in the ranking. */
  worst: WindowRisk;
};

export function windowRisk(window: SubscriptionWindow, now: number): WindowRisk {
  return {
    window,
    pace: pace({
      used_percent: window.used_percent,
      window_seconds: window.window_seconds,
      resets_at: window.resets_at,
      now,
    }),
  };
}

/** Earliest limit hit first (exhausted counts as now); otherwise the higher used share. */
export function compareRisk(a: WindowRisk, b: WindowRisk): number {
  const hitA = a.pace.limitHitAt ?? Number.POSITIVE_INFINITY;
  const hitB = b.pace.limitHitAt ?? Number.POSITIVE_INFINITY;
  if (hitA !== hitB) return hitA < hitB ? -1 : 1;
  return b.window.used_percent - a.window.used_percent;
}

export function accountRisk(account: AccountSubscription, now: number): AccountRisk | null {
  if (account.windows.length === 0) return null;
  const windows = account.windows.map((window) => windowRisk(window, now)).sort(compareRisk);
  return { account, windows, worst: windows[0]! };
}

/** Accounts that report at least one window, riskiest first. The head is the headline. */
export function rankAccounts(accounts: AccountSubscription[], now: number): AccountRisk[] {
  return accounts
    .map((account) => accountRisk(account, now))
    .filter((risk): risk is AccountRisk => risk !== null)
    .sort((a, b) => compareRisk(a.worst, b.worst));
}

/** Warn colour belongs to the two states that need action; the rest stay neutral. */
export function isAlarming(state: PaceState): boolean {
  return state === "on_pace_to_hit" || state === "exhausted";
}
