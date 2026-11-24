/**
 * Pure data model for the `/usage` Overview layout:
 * - Filters the Providers table strictly to accounts with tracked quota (`hasLimits === true`).
 * - Buckets each account's plan windows and operator caps into 5h / Daily / Weekly / Monthly.
 * - Derives the 4 top KPI tiles (Peak usage, Active providers, Estimated days left, Total spend)
 *   strictly from real gateway data (`pace.ts`, connections, summary totals).
 * - Derives Quota Alerts (exhausted, on-pace-to-hit, high utilization, resetting soon, limit errors).
 * - Builds the Usage Breakdown donut and Usage Trend time-series across all providers.
 *
 * No React, no Vite-only globals: every rule is unit-tested in `ui/tests/usage-board.test.ts`.
 */

import type { RangeSpec } from "../../lib/activity";
import type { SubscriptionWindow, UsageModelBucket } from "../../lib/api";
import type { ProviderNode, Totals } from "./aggregate.ts";
import {
  projectionTick,
  resolveProviderDisplay,
  windowPaceStatus,
  type BoardAccount,
  type OperatorBar,
} from "./board-model.ts";
import type { WindowRisk } from "./pace.ts";

const HOUR_S = 3600;
const DAY_S = 86_400;
const HOUR_MS = 3_600_000;
const DAY_MS = 86_400_000;

export type WindowColumnId = "5h" | "daily" | "weekly" | "monthly";

export const WINDOW_COLUMNS: ReadonlyArray<{ id: WindowColumnId; label: string }> = [
  { id: "5h", label: "5h" },
  { id: "daily", label: "Daily" },
  { id: "weekly", label: "Weekly" },
  { id: "monthly", label: "Monthly" },
];

export type WindowCellData = {
  kind: "plan" | "operator";
  percent: number;
  /** Primary top-left figure: e.g. "54%" for plan windows, or "1.2K / 5K" for operator caps. */
  primary: string;
  /** Secondary top-right cue: e.g. "10d left" or "54%". */
  meta: string;
  /** Projected % at reset for plan windows (tick on bar), or `null`. */
  tick: number | null;
  /** Notable warning/exhausted line under the bar, or `null` when quiet. */
  notable: { text: string; tone: "neg" | "warn" } | null;
  /** Accessible name for the progressbar. */
  ariaLabel: string;
  windowId: string;
};

export type BucketedWindows = Record<WindowColumnId, WindowCellData | null>;

/**
 * Only accounts whose quota we actually track (at least one provider plan window or
 * operator cap) belong in the Providers quota table. Accounts without tracked limits stay
 * out of the table while still contributing to range spend, breakdown, and trend totals.
 */
export function trackedQuotaAccounts(items: ReadonlyArray<BoardAccount>): BoardAccount[] {
  return items.filter((item) => item.hasLimits);
}

/** Map a provider plan window onto one of the 4 overview table columns. */
export function classifyWindowColumn(
  window: Pick<SubscriptionWindow, "id" | "label" | "window_seconds">,
): WindowColumnId {
  const secs = window.window_seconds;
  if (secs !== null && secs > 0) {
    if (secs <= 12 * HOUR_S) return "5h";
    if (secs <= 36 * HOUR_S) return "daily";
    if (secs <= 10 * DAY_S) return "weekly";
    return "monthly";
  }
  const text = `${window.id} ${window.label}`.toLowerCase();
  if (/5[\s-]?h|\bhour|session|short/.test(text)) return "5h";
  if (/daily|\bday\b|24h/.test(text)) return "daily";
  if (/week/.test(text)) return "weekly";
  return "monthly";
}

function formatDurationShort(ms: number): string {
  const totalMinutes = Math.max(0, Math.floor(ms / 60_000));
  const days = Math.floor(totalMinutes / (60 * 24));
  const hours = Math.floor((totalMinutes % (60 * 24)) / 60);
  const mins = totalMinutes % 60;
  if (days > 0) return hours > 0 ? `${days}d ${hours}h` : `${days}d`;
  if (hours > 0) return mins > 0 ? `${hours}h ${mins}m` : `${hours}h`;
  return `${Math.max(1, mins)}m`;
}

function shortLeft(resetsAt: number | null, now: number): string {
  if (resetsAt === null) return "";
  const diff = resetsAt * 1000 - now;
  if (diff <= 0) return "resetting";
  return formatDurationShort(diff);
}

function shortWindowName(window: Pick<SubscriptionWindow, "id" | "label" | "window_seconds">): string {
  if (window.window_seconds === 5 * HOUR_S || /^5[\s-]?hour$/i.test(window.label.trim())) {
    return "5-hour";
  }
  return window.label;
}

function planWindowToCell(risk: WindowRisk, now: number): WindowCellData {
  const { window } = risk;
  const used = Math.round(window.used_percent);
  const status = windowPaceStatus(risk, now);
  const left = shortLeft(window.resets_at, now);
  let notable: WindowCellData["notable"] = null;
  if (status.notable) {
    if (status.tone === "neg") {
      notable = { text: "Limit reached", tone: "neg" };
    } else {
      const compactHit = status.text.replace(/^On pace to hit the limit\s*/i, "Hits ");
      notable = { text: compactHit, tone: "warn" };
    }
  }
  return {
    kind: "plan",
    percent: window.used_percent,
    primary: `${used}%`,
    meta: left,
    tick: projectionTick(risk),
    notable,
    ariaLabel: [`${shortWindowName(window)}: ${used}% used`, left, status.text]
      .filter(Boolean)
      .join(", "),
    windowId: window.id,
  };
}

function operatorBarToCell(bar: OperatorBar, now: number): WindowCellData {
  const used = Math.round(bar.percent);
  const left = shortLeft(bar.resets_at, now);
  const notable: WindowCellData["notable"] = bar.exhausted
    ? { text: "Cap reached", tone: "neg" }
    : bar.percent >= 90
      ? { text: "Close to cap", tone: "warn" }
      : null;
  return {
    kind: "operator",
    percent: bar.percent,
    primary: bar.detail,
    meta: `${used}%`,
    tick: null,
    notable,
    ariaLabel: [`${bar.label}: ${bar.detail} (${used}%)`, left].filter(Boolean).join(", "),
    windowId: bar.id,
  };
}

function betterCell(current: WindowCellData | null, candidate: WindowCellData): WindowCellData {
  if (!current) return candidate;
  const currentNeg = current.notable?.tone === "neg" ? 1 : 0;
  const candNeg = candidate.notable?.tone === "neg" ? 1 : 0;
  if (candNeg !== currentNeg) return candNeg > currentNeg ? candidate : current;
  return candidate.percent > current.percent ? candidate : current;
}

/** Bucket all plan windows and operator caps of one account into 5h / Daily / Weekly / Monthly. */
export function bucketAccountWindows(account: BoardAccount, now: number): BucketedWindows {
  const buckets: BucketedWindows = {
    "5h": null,
    daily: null,
    weekly: null,
    monthly: null,
  };

  for (const risk of account.windows) {
    const col = classifyWindowColumn(risk.window);
    buckets[col] = betterCell(buckets[col], planWindowToCell(risk, now));
  }

  const quotaCol: WindowColumnId | null = account.quota ? account.quota.window : null;
  if (quotaCol) {
    for (const bar of account.operatorBars) {
      buckets[quotaCol] = betterCell(buckets[quotaCol], operatorBarToCell(bar, now));
    }
  }

  return buckets;
}

export type OverviewKpis = {
  peakUsage: {
    percent: number;
    providerLabel: string;
    windowLabel: string;
    accountName: string;
  } | null;
  providers: {
    active: number;
    total: number;
    tracked: number;
    inactive: number;
  };
  estimatedDaysLeft: {
    kind: "exhausted" | "countdown" | "on_track" | "unavailable";
    title: string;
    label: string;
    detail: string;
  };
  spend: {
    costMicros: number;
    requests: number;
    tokens: number;
    errors: number;
    errorRate: number;
  };
};

/**
 * Compute the 4 top KPI tiles strictly from real data:
 * - Peak usage: highest window % among active tracked accounts (never sums percents).
 * - Active providers: distinct connected providers (active / total) + tracked quota count.
 * - Estimated days left: derived from `pace.ts` earliest projected `limitHitAt` across active
 *   tracked accounts, or "On track" when every window clears reset cleanly.
 * - Total spend: range totals across ALL providers (tracked + untracked).
 */
export function computeOverviewKpis(input: {
  boardAccounts: ReadonlyArray<BoardAccount>;
  trackedAccounts: ReadonlyArray<BoardAccount>;
  totals: Totals;
  isAdmin: boolean;
  quotaLoaded: boolean;
  now: number;
}): OverviewKpis {
  const { boardAccounts, trackedAccounts, totals, isAdmin, quotaLoaded, now } = input;

  // Distinct provider counts across all connected accounts.
  const providerActive = new Map<string, boolean>();
  for (const item of boardAccounts) {
    const prev = providerActive.get(item.provider) ?? false;
    providerActive.set(item.provider, prev || item.isActive);
  }
  const totalProviders = providerActive.size;
  let activeProviders = 0;
  for (const active of providerActive.values()) {
    if (active) activeProviders += 1;
  }

  const activeTracked = trackedAccounts.filter((item) => item.isActive);

  // Peak usage across all active tracked windows & operator caps.
  let peakUsage: OverviewKpis["peakUsage"] = null;
  for (const item of activeTracked) {
    for (const risk of item.windows) {
      if (peakUsage === null || risk.window.used_percent > peakUsage.percent) {
        peakUsage = {
          percent: risk.window.used_percent,
          providerLabel: item.providerLabel,
          windowLabel: shortWindowName(risk.window),
          accountName: item.accountName,
        };
      }
    }
    for (const bar of item.operatorBars) {
      if (peakUsage === null || bar.percent > peakUsage.percent) {
        peakUsage = {
          percent: bar.percent,
          providerLabel: item.providerLabel,
          windowLabel: bar.label,
          accountName: item.accountName,
        };
      }
    }
  }

  // Estimated time/days left from real pace projections.
  let estimatedDaysLeft: OverviewKpis["estimatedDaysLeft"];
  if (!isAdmin) {
    estimatedDaysLeft = {
      kind: "unavailable",
      title: "Estimated Days Left",
      label: "—",
      detail: "Limits are admin-only",
    };
  } else if (!quotaLoaded) {
    estimatedDaysLeft = {
      kind: "unavailable",
      title: "Estimated Days Left",
      label: "…",
      detail: "Checking limits",
    };
  } else if (activeTracked.length === 0) {
    estimatedDaysLeft = {
      kind: "unavailable",
      title: "Estimated Days Left",
      label: "—",
      detail: "No tracked quotas",
    };
  } else {
    let exhaustedMatch: { provider: string; label: string } | null = null;
    let soonestHit: {
      hitAt: number;
      approximate: boolean;
      provider: string;
      label: string;
    } | null = null;
    let soonestResetMs: number | null = null;

    for (const item of activeTracked) {
      for (const bar of item.operatorBars) {
        if (bar.exhausted && !exhaustedMatch) {
          exhaustedMatch = { provider: item.providerLabel, label: bar.label };
        }
        if (bar.resets_at !== null && bar.resets_at * 1000 > now) {
          const resetMs = bar.resets_at * 1000;
          if (soonestResetMs === null || resetMs < soonestResetMs) soonestResetMs = resetMs;
        }
      }
      for (const risk of item.windows) {
        if (risk.window.resets_at !== null && risk.window.resets_at * 1000 > now) {
          const resetMs = risk.window.resets_at * 1000;
          if (soonestResetMs === null || resetMs < soonestResetMs) soonestResetMs = resetMs;
        }
        if (risk.pace.state === "exhausted" || risk.window.used_percent >= 100) {
          if (!exhaustedMatch) {
            exhaustedMatch = {
              provider: item.providerLabel,
              label: shortWindowName(risk.window),
            };
          }
        } else if (risk.pace.state === "on_pace_to_hit" && risk.pace.limitHitAt !== null) {
          if (soonestHit === null || risk.pace.limitHitAt < soonestHit.hitAt) {
            soonestHit = {
              hitAt: risk.pace.limitHitAt,
              approximate: risk.pace.approximate,
              provider: item.providerLabel,
              label: shortWindowName(risk.window),
            };
          }
        }
      }
    }

    if (exhaustedMatch) {
      estimatedDaysLeft = {
        kind: "exhausted",
        title: "Estimated Days Left",
        label: "0 days",
        detail: `${exhaustedMatch.provider} ${exhaustedMatch.label} reached`,
      };
    } else if (soonestHit) {
      const remainingMs = Math.max(0, soonestHit.hitAt - now);
      const prefix = soonestHit.approximate ? "~" : "";
      const isMultiDay = remainingMs >= DAY_MS;
      let label: string;
      if (isMultiDay) {
        const days = Math.round(remainingMs / DAY_MS);
        label = `${prefix}${days} ${days === 1 ? "day" : "days"}`;
      } else {
        label = `${prefix}${formatDurationShort(remainingMs)}`;
      }
      estimatedDaysLeft = {
        kind: "countdown",
        title: isMultiDay ? "Estimated Days Left" : "Estimated Time Left",
        label,
        detail: `${soonestHit.provider} ${soonestHit.label} pace`,
      };
    } else {
      estimatedDaysLeft = {
        kind: "on_track",
        title: "Estimated Days Left",
        label: "On track",
        detail:
          soonestResetMs !== null
            ? `Next reset in ${formatDurationShort(soonestResetMs - now)}`
            : "Within limits through reset",
      };
    }
  }

  return {
    peakUsage,
    providers: {
      active: activeProviders,
      total: totalProviders,
      tracked: activeTracked.length,
      inactive: Math.max(0, totalProviders - activeProviders),
    },
    estimatedDaysLeft,
    spend: {
      costMicros: totals.costMicros,
      requests: totals.requests,
      tokens: totals.tokens,
      errors: totals.errors,
      errorRate: totals.requests > 0 ? (totals.errors / totals.requests) * 100 : 0,
    },
  };
}

export type QuotaAlertSeverity = "neg" | "warn" | "info";

export type QuotaAlert = {
  id: string;
  severity: QuotaAlertSeverity;
  title: string;
  detail: string;
  cue: string;
  connectionId: string | null;
  percent: number;
};

const SEVERITY_RANK: Record<QuotaAlertSeverity, number> = { neg: 0, warn: 1, info: 2 };

/**
 * Derive real quota alerts from tracked active accounts:
 * - Exhausted windows / caps (neg)
 * - Windows on pace to hit limit before reset (warn)
 * - Windows or caps at >= 75% utilization (warn)
 * - Windows >= 25% resetting soon (info)
 * - Limit fetch errors (warn)
 */
export function buildQuotaAlerts(
  trackedAccounts: ReadonlyArray<BoardAccount>,
  now: number,
  maxAlerts = 4,
): QuotaAlert[] {
  const alerts: QuotaAlert[] = [];

  for (const item of trackedAccounts) {
    if (!item.isActive) continue;

    for (const bar of item.operatorBars) {
      const used = Math.round(bar.percent);
      const left = shortLeft(bar.resets_at, now);
      if (bar.exhausted || bar.percent >= 100) {
        alerts.push({
          id: `${item.key}:${bar.id}`,
          severity: "neg",
          title: `${item.providerLabel} ${bar.label.toLowerCase()} reached`,
          detail: `${bar.detail} (${used}%)`,
          cue: left || "capped",
          connectionId: item.connectionId,
          percent: bar.percent,
        });
      } else if (bar.percent >= 75) {
        alerts.push({
          id: `${item.key}:${bar.id}`,
          severity: "warn",
          title: `${item.providerLabel} ${bar.label.toLowerCase()} at ${used}%`,
          detail: bar.detail,
          cue: left || `${used}%`,
          connectionId: item.connectionId,
          percent: bar.percent,
        });
      }
    }

    for (const risk of item.windows) {
      const { window, pace } = risk;
      const used = Math.round(window.used_percent);
      const wName = shortWindowName(window);
      const left = shortLeft(window.resets_at, now);
      const resetMs = window.resets_at !== null ? window.resets_at * 1000 - now : null;

      if (pace.state === "exhausted" || window.used_percent >= 100) {
        alerts.push({
          id: `${item.key}:${window.id}`,
          severity: "neg",
          title: `${item.providerLabel} ${wName} limit reached`,
          detail: `${item.accountName} · ${used}% used`,
          cue: left ? `resets ${left}` : "100%",
          connectionId: item.connectionId,
          percent: window.used_percent,
        });
        continue;
      }

      if (pace.state === "on_pace_to_hit" && pace.limitHitAt !== null) {
        const status = windowPaceStatus(risk, now);
        const hitIn = formatDurationShort(Math.max(0, pace.limitHitAt - now));
        const cuePrefix = pace.approximate ? "~" : "";
        alerts.push({
          id: `${item.key}:${window.id}`,
          severity: "warn",
          title: `${item.providerLabel} ${wName} on pace to hit limit`,
          detail: `${status.text} · ${used}% used`,
          cue: `in ${cuePrefix}${hitIn}`,
          connectionId: item.connectionId,
          percent: window.used_percent,
        });
        continue;
      }

      if (window.used_percent >= 75) {
        alerts.push({
          id: `${item.key}:${window.id}`,
          severity: "warn",
          title: `${item.providerLabel} ${wName} quota is at ${used}%`,
          detail: `${item.accountName}${left ? ` · ${left}` : ""}`,
          cue: `${used}%`,
          connectionId: item.connectionId,
          percent: window.used_percent,
        });
        continue;
      }

      // Reset soon cue for moderately used windows so operators know headroom refreshes soon.
      const col = classifyWindowColumn(window);
      const soonThreshold = col === "5h" ? 2 * HOUR_MS : 48 * HOUR_MS;
      if (window.used_percent >= 25 && resetMs !== null && resetMs > 0 && resetMs <= soonThreshold) {
        alerts.push({
          id: `${item.key}:${window.id}`,
          severity: "info",
          title: `${item.providerLabel} ${wName} resets in ${formatDurationShort(resetMs)}`,
          detail: `${item.accountName} · currently ${used}% used`,
          cue: `${used}%`,
          connectionId: item.connectionId,
          percent: window.used_percent,
        });
      }
    }

    if (item.limitError) {
      alerts.push({
        id: `${item.key}:limit-error`,
        severity: "warn",
        title: `${item.providerLabel} limits unavailable`,
        detail: item.limitError,
        cue: "error",
        connectionId: item.connectionId,
        percent: 0,
      });
    }
  }

  alerts.sort(
    (a, b) =>
      SEVERITY_RANK[a.severity] - SEVERITY_RANK[b.severity] ||
      b.percent - a.percent ||
      a.id.localeCompare(b.id),
  );

  return alerts.slice(0, maxAlerts);
}

export type BreakdownSlice = {
  id: string;
  label: string;
  value: number;
  share: number;
  color: string;
};

export type UsageBreakdown = {
  metric: "tokens" | "requests";
  total: number;
  slices: BreakdownSlice[];
};

const SERIES_VARS = [
  "var(--color-series-1)",
  "var(--color-series-2)",
  "var(--color-series-3)",
  "var(--color-series-4)",
  "var(--color-series-5)",
  "var(--color-series-6)",
] as const;

/**
 * Provider share donut across ALL providers (tracked and untracked). Excludes unrouted
 * `provider === ""` buckets because they never reached a provider (shown in Unrouted).
 * Uses tokens when any tokens were served, falling back honestly to requests when 0 tokens.
 */
export function buildUsageBreakdown(
  providers: ReadonlyArray<ProviderNode>,
  providerDisplayMap?: ReadonlyMap<string, string>,
): UsageBreakdown {
  const realProviders = providers.filter((node) => node.provider.trim() !== "");
  const totalTokens = realProviders.reduce((sum, node) => sum + node.totals.tokens, 0);
  const metric: "tokens" | "requests" = totalTokens > 0 ? "tokens" : "requests";

  const ranked = realProviders
    .map((node) => ({
      id: node.provider,
      label: resolveProviderDisplay(node.provider, providerDisplayMap).label,
      value: metric === "tokens" ? node.totals.tokens : node.totals.requests,
    }))
    .filter((entry) => entry.value > 0)
    .sort((a, b) => b.value - a.value || a.label.localeCompare(b.label));

  const total = ranked.reduce((sum, entry) => sum + entry.value, 0);
  if (total <= 0) {
    return { metric, total: 0, slices: [] };
  }

  const head = ranked.slice(0, 4);
  const tail = ranked.slice(4);
  const combined =
    tail.length > 0
      ? [
          ...head,
          {
            id: "__others__",
            label: "Others",
            value: tail.reduce((sum, entry) => sum + entry.value, 0),
          },
        ]
      : head;

  return {
    metric,
    total,
    slices: combined.map((entry, idx) => ({
      id: entry.id,
      label: entry.label,
      value: entry.value,
      share: (entry.value / total) * 100,
      color: SERIES_VARS[Math.min(idx, SERIES_VARS.length - 1)]!,
    })),
  };
}

export type TrendSeries = {
  id: string;
  label: string;
  color: string;
  values: number[];
};

export type UsageTrend = {
  metric: "tokens" | "requests";
  /** Bucket start times (unix seconds), oldest to newest. */
  starts: number[];
  total: number[];
  /** Spend per bucket in micros (used for the Spend KPI tile sparkline). */
  spendMicros: number[];
  providers: TrendSeries[];
  maxValue: number;
};

/** Aligned bucket start times matching `lib/activity.ts` `windowStarts`. */
function alignedStarts(spec: Pick<RangeSpec, "seconds" | "cells">, nowSeconds: number): number[] {
  const last = Math.floor(nowSeconds / spec.seconds) * spec.seconds;
  const first = last - (spec.cells - 1) * spec.seconds;
  const out: number[] = [];
  for (let i = 0; i < spec.cells; i += 1) out.push(first + i * spec.seconds);
  return out;
}

/**
 * Fold `/api/usage/series?by=model` buckets into a multi-line provider trend: Total + up to
 * 3 busiest providers across the range, plus per-bucket spend for the Spend sparkline.
 */
export function buildUsageTrend(
  buckets: ReadonlyArray<UsageModelBucket>,
  range: Pick<RangeSpec, "seconds" | "cells">,
  nowMs: number,
  providerDisplayMap?: ReadonlyMap<string, string>,
): UsageTrend {
  const starts = alignedStarts(range, Math.floor(nowMs / 1000));
  const indexByStart = new Map<number, number>(starts.map((s, i) => [s, i]));

  let anyTokens = false;
  for (const row of buckets) {
    if (row.input_tokens + row.output_tokens > 0) {
      anyTokens = true;
      break;
    }
  }
  const metric: "tokens" | "requests" = anyTokens ? "tokens" : "requests";

  const total = new Array<number>(starts.length).fill(0);
  const spendMicros = new Array<number>(starts.length).fill(0);
  const byProvider = new Map<string, { sum: number; values: number[] }>();

  for (const row of buckets) {
    const idx = indexByStart.get(row.start);
    if (idx === undefined) continue;
    const val = metric === "tokens" ? row.input_tokens + row.output_tokens : row.requests;
    total[idx] = total[idx]! + val;
    spendMicros[idx] = spendMicros[idx]! + row.cost_micros;

    const provider = row.provider.trim();
    if (!provider || val <= 0) continue;
    let entry = byProvider.get(provider);
    if (!entry) {
      entry = { sum: 0, values: new Array<number>(starts.length).fill(0) };
      byProvider.set(provider, entry);
    }
    entry.sum += val;
    entry.values[idx] = entry.values[idx]! + val;
  }

  const topProviders = [...byProvider.entries()]
    .sort((a, b) => b[1].sum - a[1].sum || a[0].localeCompare(b[0]))
    .slice(0, 3)
    .map(([provider, entry], idx) => ({
      id: provider,
      label: resolveProviderDisplay(provider, providerDisplayMap).label,
      // Reserve series-1 for Total; providers take series-2..4.
      color: SERIES_VARS[idx + 1]!,
      values: entry.values,
    }));

  const maxValue = Math.max(0, ...total);

  return {
    metric,
    starts,
    total,
    spendMicros,
    providers: topProviders,
    maxValue,
  };
}
