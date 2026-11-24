/**
 * Pure data model for the `/usage` Limits Board: merges connections, provider plan
 * windows, operator caps, range usage totals, and recent timestamps into one risk-sorted
 * list of account cards, plus summary strip counts. No React, no side effects, so every
 * classification and sorting edge is pinned by unit tests (`ui/tests/usage-board.test.ts`).
 */

import type {
  AccountQuotaRow,
  AccountSubscription,
  Connection,
  SubscriptionWindow,
  UsageEvent,
  UsageSummaryByConnection,
} from "../../lib/api";
import {
  addTotals,
  buildAccountNodes,
  emptyTotals,
  errorRateOf,
  type AccountNode,
  type ModelNode,
  type Totals,
} from "./aggregate.ts";
import { compareRisk, pace, windowRisk, type Pace, type WindowRisk } from "./pace.ts";

const DAY_MS = 86_400_000;
const MIN_CHART_WINDOW_S = 2 * 86_400;
const FAILING_ERROR_RATE = 50;
const DEGRADED_ERROR_RATE = 10;

const KNOWN_PROVIDER_LABELS: Record<string, string> = {
  claude: "Claude",
  codex: "Codex",
  github: "GitHub Copilot",
  "opencode-go": "OpenCode Go",
  opencode: "OpenCode",
  openrouter: "OpenRouter",
  anthropic: "Anthropic",
  openai: "OpenAI",
  gemini: "Gemini",
  "google-antigravity": "Google Antigravity",
  antigravity: "Google Antigravity",
  deepseek: "DeepSeek",
  groq: "Groq",
  xai: "xAI",
  mistral: "Mistral",
  ollama: "Ollama",
  together: "Together",
  cerebras: "Cerebras",
};

export type BoardFilter = "all" | "at_risk" | "failing" | "healthy" | "no_limit";

export type BoardStatusKind =
  | "exhausted"
  | "at_risk"
  | "failing"
  | "degraded"
  | "healthy"
  | "no_limit"
  | "unavailable"
  | "loading"
  | "admin_only"
  | "inactive";

export type OperatorBar = {
  id: string;
  label: string;
  percent: number;
  detail: string;
  resets_at: number | null;
  exhausted: boolean;
};

export type BoardAccount = {
  /** Stable React key: connection id, or `unattributed:<provider>` for unattributed buckets. */
  key: string;
  connectionId: string | null;
  /** Raw provider id from the gateway (may be empty `""` for unrouted failures). */
  provider: string;
  /** Human-readable provider label, never blank. */
  providerLabel: string;
  /** True when `provider` is empty (requests failed before dispatch). */
  isUnrouted: boolean;
  accountName: string;
  email: string | null;
  plan: string | null;
  isActive: boolean;
  refusal: string | null;
  backoffLevel: number;
  lastError: string | null;
  subscription?: AccountSubscription;
  quota?: AccountQuotaRow;
  /** Plan windows in provider order, each paired with its computed pace. */
  windows: WindowRisk[];
  /** Riskiest plan window, or `null` when the provider reported none. */
  worstWindow: WindowRisk | null;
  /** Operator-configured caps (`max_requests` / `max_tokens` / `max_cost_micros`). */
  operatorBars: OperatorBar[];
  hasLimits: boolean;
  /** Highest used % across all plan windows and operator caps, or `null` when none exist. */
  worstPercent: number | null;
  /** Earliest projected limit hit across plan windows (ms), or `now` when exhausted. */
  earliestHitAt: number | null;
  /** Provider usage fetch error, or page-level quota API error. */
  limitError: string | null;
  /** Unix seconds when provider plan limits were last sampled, or `null`. */
  fetchedAt: number | null;
  totals: Totals;
  errorRate: number;
  /** Range models served by this account, ranked by tokens desc. */
  models: ModelNode[];
  /** Unix seconds of the most recent observed request in recent events, or `null`. */
  lastUsedAt: number | null;
  isAtRisk: boolean;
  isFailing: boolean;
  status: BoardStatusKind;
};

export type BoardSummary = {
  total: number;
  atRisk: number;
  failing: number;
  healthy: number;
  noLimit: number;
  totalSpendMicros: number;
  totalRequests: number;
  totalErrors: number;
};

/**
 * Resolve a raw provider id to a non-empty display label. Empty strings come from requests
 * rejected before dispatch (`TraceLabels.provider` defaults to `""` in proxy.rs:3902).
 */
export function resolveProviderDisplay(
  provider: string,
  providerDisplayMap?: ReadonlyMap<string, string>,
): { label: string; isUnrouted: boolean } {
  const trimmed = provider.trim();
  if (trimmed === "") {
    return { label: "Unrouted", isUnrouted: true };
  }
  const fromCatalog = providerDisplayMap?.get(trimmed)?.trim();
  if (fromCatalog) {
    return { label: fromCatalog, isUnrouted: false };
  }
  const known = KNOWN_PROVIDER_LABELS[trimmed];
  if (known) {
    return { label: known, isUnrouted: false };
  }
  return { label: trimmed, isUnrouted: false };
}

/** Stable key matching both `BoardAccount.key` and `lastUsedByAccount`. */
export function boardKey(provider: string, connectionId: string | null): string {
  return connectionId ?? `unattributed:${provider.trim()}`;
}

/** Most recent request timestamp (unix seconds) per connection or unattributed provider bucket. */
export function lastUsedByAccount(
  events: ReadonlyArray<Pick<UsageEvent, "connection_id" | "provider" | "ts">>,
): Map<string, number> {
  const map = new Map<string, number>();
  for (const event of events) {
    const key = boardKey(event.provider, event.connection_id);
    const prev = map.get(key);
    if (prev === undefined || event.ts > prev) {
      map.set(key, event.ts);
    }
  }
  return map;
}

function compactCount(n: number): string {
  const abs = Math.abs(n);
  const scale = (v: number) =>
    (v >= 100 ? v.toFixed(0) : v >= 10 ? v.toFixed(1) : v.toFixed(2))
      .replace(/\.0+$/, "")
      .replace(/(\.\d)0$/, "$1");
  if (abs >= 1_000_000_000) return `${scale(n / 1_000_000_000)}B`;
  if (abs >= 1_000_000) return `${scale(n / 1_000_000)}M`;
  if (abs >= 1_000) return `${scale(n / 1_000)}K`;
  return String(n);
}

function formatUsd(micros: number): string {
  const dollars = micros / 1_000_000;
  if (Math.abs(dollars) < 0.01) {
    return `$${dollars.toFixed(6).replace(/(\.\d{2}\d*?)0+$/, "$1")}`;
  }
  return `$${dollars.toFixed(4)}`;
}

function capWindowLabel(window: AccountQuotaRow["window"]): string {
  return window.charAt(0).toUpperCase() + window.slice(1);
}

function buildOperatorBars(quota?: AccountQuotaRow): OperatorBar[] {
  if (!quota) return [];
  const prefix = capWindowLabel(quota.window);
  const bars: OperatorBar[] = [];
  if (quota.max_requests > 0) {
    const percent = (quota.requests / quota.max_requests) * 100;
    bars.push({
      id: "op-requests",
      label: `${prefix} requests`,
      percent,
      detail: `${compactCount(quota.requests)} / ${compactCount(quota.max_requests)}`,
      resets_at: quota.resets_at,
      exhausted: quota.exhausted || percent >= 100,
    });
  }
  if (quota.max_tokens > 0) {
    const percent = (quota.tokens / quota.max_tokens) * 100;
    bars.push({
      id: "op-tokens",
      label: `${prefix} tokens`,
      percent,
      detail: `${compactCount(quota.tokens)} / ${compactCount(quota.max_tokens)}`,
      resets_at: quota.resets_at,
      exhausted: quota.exhausted || percent >= 100,
    });
  }
  if (quota.max_cost_micros > 0) {
    const percent = (quota.cost_micros / quota.max_cost_micros) * 100;
    bars.push({
      id: "op-cost",
      label: `${prefix} spend`,
      percent,
      detail: `${formatUsd(quota.cost_micros)} / ${formatUsd(quota.max_cost_micros)}`,
      resets_at: quota.resets_at,
      exhausted: quota.exhausted || percent >= 100,
    });
  }
  return bars;
}

function resolveAccountName(
  connection: Connection | undefined,
  subscription: AccountSubscription | undefined,
  quota: AccountQuotaRow | undefined,
  providerLabel: string,
  isUnrouted: boolean,
  connectionId: string | null,
): string {
  if (connection) {
    if (connection.name?.trim()) return connection.name.trim();
    if (connection.email?.trim()) return connection.email.trim();
    if (connection.project_id?.trim()) return `project ${connection.project_id.trim()}`;
    if (connection.auth_type === "oauth") return `${providerLabel} (OAuth)`;
    return providerLabel;
  }
  if (subscription?.name?.trim()) return subscription.name.trim();
  if (quota?.name?.trim()) return quota.name.trim();
  if (isUnrouted) return "Failed before dispatch";
  if (connectionId === null) return "Unattributed traffic";
  return "Unnamed account";
}

function statusPriority(status: BoardStatusKind): number {
  switch (status) {
    case "exhausted":
      return 0;
    case "at_risk":
      return 1;
    case "failing":
      return 2;
    case "degraded":
      return 3;
    case "healthy":
      return 4;
    case "unavailable":
    case "loading":
      return 5;
    case "no_limit":
      return 6;
    case "admin_only":
      return 6;
    case "inactive":
      return 7;
  }
}

export function compareBoardAccounts(a: BoardAccount, b: BoardAccount): number {
  // Inactive accounts always stay below active ones so they never masquerade as live risk.
  if (a.isActive !== b.isActive) return a.isActive ? -1 : 1;

  // Earliest projected limit hit (exhausted counts as `now`) always leads.
  const hitA = a.earliestHitAt ?? Number.POSITIVE_INFINITY;
  const hitB = b.earliestHitAt ?? Number.POSITIVE_INFINITY;
  if (hitA !== hitB) return hitA < hitB ? -1 : 1;

  // Next, status tier: exhausted -> at_risk -> failing -> degraded -> healthy -> unavailable
  // / loading -> no_limit / admin_only -> inactive.
  const tierDiff = statusPriority(a.status) - statusPriority(b.status);
  if (tierDiff !== 0) return tierDiff;

  // Within the same tier: highest limit % first.
  const pctA = a.worstPercent ?? -1;
  const pctB = b.worstPercent ?? -1;
  if (pctA !== pctB) return pctB - pctA;

  // For failing/no-limit buckets: higher error rate, then higher spend, then more requests.
  if (a.errorRate !== b.errorRate) return b.errorRate - a.errorRate;
  if (a.totals.costMicros !== b.totals.costMicros) return b.totals.costMicros - a.totals.costMicros;
  if (a.totals.requests !== b.totals.requests) return b.totals.requests - a.totals.requests;

  return (
    a.providerLabel.localeCompare(b.providerLabel) ||
    a.accountName.localeCompare(b.accountName) ||
    a.key.localeCompare(b.key)
  );
}

export type BuildBoardInput = {
  connections: ReadonlyArray<Connection>;
  subscriptions: ReadonlyArray<AccountSubscription>;
  quotas: ReadonlyArray<AccountQuotaRow>;
  summaryRows: ReadonlyArray<UsageSummaryByConnection>;
  recentEvents?: ReadonlyArray<Pick<UsageEvent, "connection_id" | "provider" | "ts">>;
  providerDisplayMap?: ReadonlyMap<string, string>;
  providerFilter?: string;
  isAdmin: boolean;
  quotaLoaded: boolean;
  quotaError?: boolean;
  now: number;
};

/**
 * Build the complete risk-sorted list of account cards for the Limits Board.
 * Every known connection plus any unattributed/unrouted range bucket is included so
 * nothing is silently hidden behind a "1 other account" collapse.
 */
export function buildBoardAccounts(input: BuildBoardInput): BoardAccount[] {
  const {
    connections,
    subscriptions,
    quotas,
    summaryRows,
    recentEvents = [],
    providerDisplayMap,
    providerFilter = "",
    isAdmin,
    quotaLoaded,
    quotaError = false,
    now,
  } = input;

  const connectionById = new Map(connections.map((c) => [c.id, c]));
  const subscriptionById = new Map(subscriptions.map((s) => [s.connection_id, s]));
  const quotaById = new Map(quotas.map((q) => [q.connection_id, q]));
  const lastUsed = lastUsedByAccount(recentEvents);

  // Fold summary rows per (provider, connectionId).
  const accountNodesByProvider = buildAccountNodes([...summaryRows]);
  const observedByKey = new Map<string, { provider: string; node: AccountNode }>();
  for (const [provider, nodes] of accountNodesByProvider) {
    for (const node of nodes) {
      const key = boardKey(provider, node.connectionId);
      const existing = observedByKey.get(key);
      if (existing) {
        addTotals(existing.node.totals, node.totals);
        existing.node.models = [...existing.node.models, ...node.models];
      } else {
        observedByKey.set(key, {
          provider,
          node: {
            connectionId: node.connectionId,
            totals: { ...node.totals },
            models: [...node.models],
            observed: node.observed,
          },
        });
      }
    }
  }

  // Collect every unique account key: connections, subscriptions, quotas, and observed traffic.
  const entries = new Map<string, { provider: string; connectionId: string | null }>();
  for (const conn of connections) {
    entries.set(conn.id, { provider: conn.provider, connectionId: conn.id });
  }
  for (const sub of subscriptions) {
    if (!entries.has(sub.connection_id)) {
      entries.set(sub.connection_id, { provider: sub.provider, connectionId: sub.connection_id });
    }
  }
  for (const q of quotas) {
    if (!entries.has(q.connection_id)) {
      entries.set(q.connection_id, { provider: q.provider, connectionId: q.connection_id });
    }
  }
  for (const [key, { provider, node }] of observedByKey) {
    if (!entries.has(key)) {
      entries.set(key, { provider, connectionId: node.connectionId });
    }
  }

  const items: BoardAccount[] = [];
  for (const [key, { provider, connectionId }] of entries) {
    if (providerFilter && provider !== providerFilter) continue;

    const connection = connectionId ? connectionById.get(connectionId) : undefined;
    const subscription = isAdmin && connectionId ? subscriptionById.get(connectionId) : undefined;
    const quota = isAdmin && connectionId ? quotaById.get(connectionId) : undefined;
    const observed = observedByKey.get(key)?.node;

    const { label: providerLabel, isUnrouted } = resolveProviderDisplay(
      provider,
      providerDisplayMap,
    );
    const isActive = connection?.is_active ?? quota?.is_active ?? true;
    const accountName = resolveAccountName(
      connection,
      subscription,
      quota,
      providerLabel,
      isUnrouted,
      connectionId,
    );
    const email =
      connection?.email && connection.email.trim() !== accountName ? connection.email.trim() : null;

    // Preserve provider window order for rendering, while computing risk across every window.
    const windows = (subscription?.windows ?? []).map((w) => windowRisk(w, now));
    const rankedWindows = [...windows].sort(compareRisk);
    const worstWindow = rankedWindows[0] ?? null;
    const operatorBars = buildOperatorBars(quota);
    const hasLimits = isAdmin && (windows.length > 0 || operatorBars.length > 0);

    const allPercents = [
      ...windows.map((w) => w.window.used_percent),
      ...operatorBars.map((b) => b.percent),
    ];
    const worstPercent = allPercents.length > 0 ? Math.max(...allPercents) : null;

    // Limit-derived claims (exhausted, at risk, healthy, no-limit) require BOTH limit
    // sources to have answered. While either is still loading the card says "Checking
    // limits…" instead of inventing an answer from half the data.
    const limitFacts = isAdmin && quotaLoaded;

    const isExhausted =
      limitFacts &&
      (windows.some((w) => w.pace.state === "exhausted" || w.window.used_percent >= 100) ||
        Boolean(quota?.exhausted) ||
        operatorBars.some((b) => b.exhausted));

    // Earliest projected limit hit across plan windows (exhausted counts as `now`).
    let earliestHitAt: number | null = isExhausted ? now : null;
    if (limitFacts && earliestHitAt === null) {
      for (const w of windows) {
        if (w.pace.limitHitAt !== null) {
          if (earliestHitAt === null || w.pace.limitHitAt < earliestHitAt) {
            earliestHitAt = w.pace.limitHitAt;
          }
        }
      }
    }

    const totals = observed ? observed.totals : emptyTotals();
    const errorRate = errorRateOf(totals);
    const models = observed ? observed.models : [];
    const refusal = connection?.refusal?.trim() || null;
    const backoffLevel = connection?.backoff_level ?? 0;
    const lastError = connection?.last_error?.trim() || null;

    const isAtRisk =
      isActive &&
      limitFacts &&
      (isExhausted ||
        windows.some((w) => w.pace.state === "on_pace_to_hit") ||
        (worstPercent !== null && worstPercent >= 90));

    const isFailing =
      (totals.requests > 0 && errorRate >= FAILING_ERROR_RATE) || Boolean(refusal);

    const isDegraded =
      !isFailing &&
      ((totals.requests > 0 && errorRate >= DEGRADED_ERROR_RATE) || backoffLevel > 0);

    const limitError = isAdmin
      ? (subscription?.error?.trim() || (quotaError && !hasLimits ? "Quota API unavailable" : null))
      : null;

    let status: BoardStatusKind;
    if (!isActive) {
      status = "inactive";
    } else if (isExhausted) {
      status = "exhausted";
    } else if (isAtRisk) {
      status = "at_risk";
    } else if (isFailing) {
      status = "failing";
    } else if (isDegraded) {
      status = "degraded";
    } else if (!isAdmin) {
      status = "admin_only";
    } else if (!quotaLoaded) {
      status = "loading";
    } else if (hasLimits) {
      status = "healthy";
    } else if (limitError) {
      status = "unavailable";
    } else {
      status = "no_limit";
    }

    items.push({
      key,
      connectionId,
      provider,
      providerLabel,
      isUnrouted,
      accountName,
      email,
      plan: subscription?.plan ?? null,
      isActive,
      refusal,
      backoffLevel,
      lastError,
      subscription,
      quota,
      windows,
      worstWindow,
      operatorBars,
      hasLimits,
      worstPercent,
      earliestHitAt,
      limitError,
      fetchedAt:
        subscription && subscription.windows.length > 0 && subscription.fetched_at > 0
          ? subscription.fetched_at
          : null,
      totals,
      errorRate,
      models,
      lastUsedAt: lastUsed.get(key) ?? null,
      isAtRisk,
      isFailing,
      status,
    });
  }

  items.sort(compareBoardAccounts);
  return items;
}

/**
 * Compute summary strip counts over the board accounts and total range spend.
 * `atRisk` and `failing` use their independent boolean flags so an account that is both
 * exhausted and failing is never hidden when filtering either bucket.
 */
export function summarizeBoard(
  items: ReadonlyArray<BoardAccount>,
  rangeTotals: Totals,
): BoardSummary {
  let atRisk = 0;
  let failing = 0;
  let healthy = 0;
  let noLimit = 0;

  for (const item of items) {
    if (!item.isActive) continue;
    if (item.isAtRisk) atRisk += 1;
    if (item.isFailing) failing += 1;
    if (item.status === "healthy") healthy += 1;
    if (item.status === "no_limit" || item.status === "unavailable") noLimit += 1;
  }

  return {
    total: items.length,
    atRisk,
    failing,
    healthy,
    noLimit,
    totalSpendMicros: rangeTotals.costMicros,
    totalRequests: rangeTotals.requests,
    totalErrors: rangeTotals.errors,
  };
}

export function filterBoardAccounts(
  items: ReadonlyArray<BoardAccount>,
  filter: BoardFilter,
): BoardAccount[] {
  switch (filter) {
    case "all":
      return [...items];
    case "at_risk":
      return items.filter((item) => item.isAtRisk);
    case "failing":
      return items.filter((item) => item.isFailing);
    case "healthy":
      return items.filter((item) => item.status === "healthy");
    case "no_limit":
      return items.filter(
        (item) => item.isActive && (item.status === "no_limit" || item.status === "unavailable"),
      );
  }
}

export function isWindowChartable(window: Pick<SubscriptionWindow, "window_seconds">): boolean {
  return window.window_seconds === null || window.window_seconds >= MIN_CHART_WINDOW_S;
}

/** Pick the riskiest chartable window (>= 2 days), falling back to the riskiest window overall. */
export function defaultChartWindowId(windows: ReadonlyArray<WindowRisk>): string | null {
  if (windows.length === 0) return null;
  const ranked = [...windows].sort(compareRisk);
  const chartableRisk = ranked.find((w) => isWindowChartable(w.window));
  return (chartableRisk ?? ranked[0])!.window.id;
}

function clockTime(ms: number): string {
  return new Date(ms).toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" });
}

function shortDateLabel(ms: number): string {
  return new Date(ms).toLocaleDateString(undefined, { month: "short", day: "numeric" });
}

/**
 * Per-window pace/status label shown under each bar on the Limits Board.
 * Always returns an honest status rather than leaving the user guessing.
 */
export function windowPaceStatus(
  risk: WindowRisk,
  now: number,
): { text: string; tone: "neg" | "warn" | "muted" | "faint" } {
  const { window, pace: p } = risk;
  if (p.state === "exhausted") {
    return { text: "Exhausted — limit reached", tone: "neg" };
  }
  if (p.state === "on_pace_to_hit" && p.limitHitAt !== null) {
    const at =
      p.limitHitAt - now < DAY_MS ? `at ${clockTime(p.limitHitAt)}` : shortDateLabel(p.limitHitAt);
    return {
      text: `On pace to hit ${p.approximate ? "~" : ""}${at}`,
      tone: "warn",
    };
  }
  if (p.state === "ok") {
    if (window.used_percent <= 0) {
      return { text: "OK · 0% used", tone: "muted" };
    }
    if (p.projectedAtReset !== null) {
      return {
        text: `OK · on pace for ${p.approximate ? "~" : ""}${Math.round(p.projectedAtReset)}% by reset`,
        tone: "muted",
      };
    }
    return { text: "OK", tone: "muted" };
  }
  // `unknown` state: distinguish reset-in-past, too-young window, and missing reset metadata.
  if (window.resets_at !== null && now >= window.resets_at * 1000) {
    return { text: "Window reset pending refresh", tone: "faint" };
  }
  if (p.startMs !== null) {
    return { text: "Too early in window to forecast", tone: "faint" };
  }
  return { text: "No reset window reported", tone: "faint" };
}

/** Re-export `pace` for convenience in board callers without changing `pace.ts`. */
export { pace, type Pace };
