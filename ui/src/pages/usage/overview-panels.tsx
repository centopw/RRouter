/**
 * Primary panels for the `/usage` Overview layout:
 * - `OverviewKpiRow`: 4 KPI tiles (Peak Usage, Active Providers, Estimated Days Left, Total Spend).
 * - `TrackedProvidersTable`: ONLY providers/accounts whose quota is tracked, with columns
 *   Provider | 5h Limit | Daily Limit | Weekly Limit | Monthly Limit | Status + lazy inline chart.
 * - `QuotaAlertsCard`: real quota alerts derived from pace/limits (at-risk, exhausted, resetting).
 * - `RecentActivityCard`: latest served requests across providers.
 */

import { Fragment, type ReactNode } from "react";
import { Link } from "react-router-dom";
import {
  Bell,
  ChartLineUp,
  CurrencyDollar,
  Info,
  Lightning,
  Pulse,
  ShieldCheck,
  Stack,
  WarningCircle,
} from "@phosphor-icons/react";
import { Badge, Card, Dot, cx, type BadgeTone, type DotTone } from "../../components/ui";
import type { UsageEvent } from "../../lib/api";
import { ago, compact, pct, splitRef, usd } from "../../lib/format";
import { barFill, barText } from "../providers/types";
import { resolveProviderDisplay, type BoardAccount, type BoardStatusKind } from "./board-model";
import { AccountForecastPanel, AccountPreviewLink, LimitMeter } from "./limits";
import {
  WINDOW_COLUMNS,
  bucketAccountWindows,
  type OverviewKpis,
  type QuotaAlert,
  type WindowCellData,
} from "./overview-model";
import { MiniSparkline } from "./overview-charts";
import { UsageProviderIcon } from "./provider-badge";
import { capitalized } from "./quota";

function KpiIcon({ tone, children }: { tone: "info" | "accent" | "pos" | "warn" | "neg"; children: ReactNode }) {
  const cls =
    tone === "pos"
      ? "bg-[var(--color-pos-tint)] text-[var(--color-pos)]"
      : tone === "warn"
        ? "bg-[var(--color-warn-tint)] text-[var(--color-warn)]"
        : tone === "neg"
          ? "bg-[var(--color-neg-tint)] text-[var(--color-neg)]"
          : "bg-[var(--color-info-tint)] text-[var(--color-info)]";
  return (
    <span
      aria-hidden
      className={cx("inline-flex h-9 w-9 shrink-0 items-center justify-center rounded-[10px]", cls)}
    >
      {children}
    </span>
  );
}

/** 4 KPI tiles across the top of Usage Overview. Every number comes directly from real API data. */
export function OverviewKpiRow({
  kpis,
  rangeLabel,
  spendSeries,
  isAdmin,
}: {
  kpis: OverviewKpis;
  rangeLabel: string;
  spendSeries: ReadonlyArray<number>;
  isAdmin: boolean;
}) {
  const { peakUsage, providers, estimatedDaysLeft, spend } = kpis;

  const daysTone: "pos" | "warn" | "neg" | "info" =
    estimatedDaysLeft.kind === "exhausted"
      ? "neg"
      : estimatedDaysLeft.kind === "countdown"
        ? "warn"
        : estimatedDaysLeft.kind === "on_track"
          ? "pos"
          : "info";

  return (
    <div className="mb-4 grid grid-cols-1 gap-3 sm:grid-cols-2 xl:grid-cols-4">
      {/* Tile 1: Peak Quota Usage (or range tokens for non-admin / no tracked limits) */}
      <Card as="article" className="flex items-start gap-3 p-4">
        <KpiIcon
          tone={
            peakUsage === null
              ? "info"
              : peakUsage.percent >= 90
                ? "neg"
                : peakUsage.percent >= 70
                  ? "warn"
                  : "info"
          }
        >
          <Lightning size={18} weight="fill" />
        </KpiIcon>
        <div className="flex min-w-0 flex-1 flex-col gap-1">
          <span className="text-caption font-medium text-muted">
            {isAdmin && peakUsage !== null ? "Peak Quota Usage" : `Total Usage (${rangeLabel})`}
          </span>
          {isAdmin && peakUsage !== null ? (
            <>
              <span className="text-2xl font-semibold leading-tight text-fg tnum">
                {Math.round(peakUsage.percent)}%
              </span>
              <span className="mt-0.5 block h-1.5 w-full overflow-hidden rounded-full bg-rule">
                <span
                  className={cx("block h-full rounded-full", barFill(peakUsage.percent))}
                  style={{ width: `${Math.min(100, Math.max(0, peakUsage.percent))}%` }}
                />
              </span>
              <span className="truncate text-caption text-faint">
                {peakUsage.providerLabel} · {peakUsage.windowLabel}
              </span>
            </>
          ) : (
            <>
              <span className="text-2xl font-semibold leading-tight text-fg tnum">
                {compact(spend.tokens)}
              </span>
              <span className="truncate text-caption text-faint tnum">
                tokens · {compact(spend.requests)} req
              </span>
            </>
          )}
        </div>
      </Card>

      {/* Tile 2: Active Providers */}
      <Card as="article" className="flex items-start gap-3 p-4">
        <KpiIcon tone="accent">
          <Stack size={18} weight="fill" />
        </KpiIcon>
        <div className="flex min-w-0 flex-1 flex-col gap-1">
          <span className="text-caption font-medium text-muted">Active Providers</span>
          <span className="text-2xl font-semibold leading-tight text-fg tnum">
            {providers.active}
            <span className="text-lead font-normal text-faint"> / {providers.total}</span>
          </span>
          <span className="truncate text-caption text-faint">
            {isAdmin
              ? `${providers.tracked} tracked quota${providers.inactive > 0 ? ` · ${providers.inactive} inactive` : ""}`
              : providers.inactive > 0
                ? `${providers.inactive} inactive`
                : "All connected active"}
          </span>
        </div>
      </Card>

      {/* Tile 3: Estimated Days/Time Left */}
      <Card as="article" className="flex items-start gap-3 p-4">
        <KpiIcon tone={daysTone}>
          <ShieldCheck size={18} weight="fill" />
        </KpiIcon>
        <div className="flex min-w-0 flex-1 flex-col gap-1">
          <span className="text-caption font-medium text-muted">{estimatedDaysLeft.title}</span>
          <span className="text-2xl font-semibold leading-tight text-fg tnum">
            {estimatedDaysLeft.label}
          </span>
          <span className="truncate text-caption text-faint">{estimatedDaysLeft.detail}</span>
        </div>
      </Card>

      {/* Tile 4: Total Spend */}
      <Card as="article" className="flex items-start justify-between gap-2 p-4">
        <div className="flex min-w-0 flex-1 items-start gap-3">
          <KpiIcon tone="info">
            <CurrencyDollar size={18} weight="bold" />
          </KpiIcon>
          <div className="flex min-w-0 flex-1 flex-col gap-1">
            <span className="text-caption font-medium text-muted">Total Spend ({rangeLabel})</span>
            <span className="truncate text-2xl font-semibold leading-tight text-fg tnum">
              {usd(spend.costMicros)}
            </span>
            <div className="flex items-end justify-between gap-2">
              <span className="truncate text-caption text-faint tnum">
                {compact(spend.requests)} req
                {spend.errors > 0 ? ` · ${pct(spend.errorRate)} err` : ` · ${compact(spend.tokens)} tok`}
              </span>
              <div className="hidden shrink-0 sm:block">
                <MiniSparkline values={spendSeries} color="var(--color-series-2)" width={64} height={20} />
              </div>
            </div>
          </div>
        </div>
      </Card>
    </div>
  );
}

const ROW_STATUS: Record<BoardStatusKind, { label: string; tone: BadgeTone; dot: DotTone }> = {
  exhausted: { label: "Exhausted", tone: "neg", dot: "neg" },
  at_risk: { label: "Warning", tone: "warn", dot: "warn" },
  failing: { label: "Failing", tone: "neg", dot: "neg" },
  degraded: { label: "Warning", tone: "warn", dot: "warn" },
  healthy: { label: "Healthy", tone: "pos", dot: "pos" },
  no_limit: { label: "Healthy", tone: "neutral", dot: "muted" },
  unavailable: { label: "Unavailable", tone: "warn", dot: "warn" },
  loading: { label: "Checking…", tone: "neutral", dot: "muted" },
  admin_only: { label: "Active", tone: "neutral", dot: "muted" },
  inactive: { label: "Inactive", tone: "neutral", dot: "muted" },
};

function WindowCell({ cell }: { cell: WindowCellData | null }) {
  if (!cell) {
    return (
      <td className="px-2.5 py-3 align-middle">
        <div className="flex flex-col gap-1">
          <span className="text-caption text-faint">—</span>
          <span className="block h-1.5 w-full rounded-full bg-rule/50" aria-hidden />
          <span className="text-caption text-faint">N/A</span>
        </div>
      </td>
    );
  }

  return (
    <td className="px-2.5 py-3 align-middle">
      <div className="flex min-w-[84px] flex-col gap-1">
        <div className="flex items-baseline justify-between gap-1 text-caption tnum">
          <span className={cx("font-medium", barText(cell.percent))}>{cell.primary}</span>
          {cell.meta ? <span className="truncate text-faint">{cell.meta}</span> : null}
        </div>
        <LimitMeter percent={cell.percent} tick={cell.tick} label={cell.ariaLabel} />
        {cell.notable ? (
          <span
            className={cx(
              "truncate text-caption font-medium tnum",
              cell.notable.tone === "neg"
                ? "text-[var(--color-neg)]"
                : "text-[var(--color-warn)]",
            )}
          >
            {cell.notable.text}
          </span>
        ) : null}
      </div>
    </td>
  );
}

/**
 * Providers quota table: strictly shows ONLY accounts with tracked quota (`hasLimits === true`).
 * Accounts without tracked quota are omitted here (their traffic still counts in KPI totals,
 * Usage Breakdown, Usage Trend, and the secondary full breakdown disclosure).
 */
export function TrackedProvidersTable({
  accounts,
  rangeLabel,
  isAdmin,
  quotaLoaded,
  quotaError,
  expandedCharts,
  onToggleChart,
  now,
}: {
  accounts: BoardAccount[];
  rangeLabel: string;
  isAdmin: boolean;
  quotaLoaded: boolean;
  quotaError: boolean;
  expandedCharts: Set<string>;
  onToggleChart: (key: string) => void;
  now: number;
}) {
  return (
    <Card className="flex min-w-0 flex-col overflow-hidden">
      <div className="flex flex-wrap items-center justify-between gap-2 border-b border-line px-4 py-3">
        <div className="flex items-center gap-2">
          <Stack size={16} aria-hidden className="text-accent-text" />
          <h2 className="text-body font-semibold text-fg">Providers</h2>
          {isAdmin && quotaLoaded ? (
            <span className="text-caption text-faint tnum">
              {accounts.length} tracked
            </span>
          ) : null}
        </div>
        <div className="flex items-center gap-3 text-caption">
          <span className="hidden items-center gap-1.5 text-muted sm:inline-flex">
            <span aria-hidden className="h-2 w-2 rounded-full bg-[var(--color-ink-soft)]" />
            Usage
          </span>
          <span className="hidden items-center gap-1.5 text-faint sm:inline-flex">
            <span aria-hidden className="h-2 w-2 rounded-full bg-rule" />
            Limit
          </span>
          <Link
            to="/providers"
            className="text-accent-text underline-offset-2 hover:underline focus-visible:underline"
          >
            View all →
          </Link>
        </div>
      </div>

      {!isAdmin ? (
        <div className="px-4 py-10 text-center text-small text-faint">
          Provider quota windows are visible to admins only.
        </div>
      ) : !quotaLoaded && accounts.length === 0 ? (
        <div className="px-4 py-10 text-center text-small text-faint">
          Checking tracked provider quotas…
        </div>
      ) : accounts.length === 0 ? (
        <div className="flex flex-col items-center gap-1.5 px-4 py-10 text-center text-small text-faint">
          <span>No providers with tracked quota yet.</span>
          <span className="text-caption">
            Connect a subscription provider (Claude, Codex, OpenCode Go) or add an operator cap on{" "}
            <Link to="/providers" className="text-accent-text underline-offset-2 hover:underline">
              Providers
            </Link>
            .
          </span>
        </div>
      ) : (
        <div className="overflow-x-auto">
          <table className="w-full min-w-[640px] border-collapse text-small">
            <caption className="sr-only">
              Tracked provider quota windows across 5-hour, daily, weekly, and monthly limits
            </caption>
            <thead>
              <tr className="border-b border-line bg-surface-2/60">
                <th scope="col" className="px-3.5 py-2.5 text-left text-caption font-medium text-faint">
                  Provider
                </th>
                {WINDOW_COLUMNS.map((col) => (
                  <th
                    key={col.id}
                    scope="col"
                    className="w-[104px] px-2.5 py-2.5 text-left text-caption font-medium text-faint"
                  >
                    {col.label}
                  </th>
                ))}
                <th scope="col" className="w-[116px] px-3 py-2.5 text-right text-caption font-medium text-faint">
                  Status
                </th>
              </tr>
            </thead>
            <tbody className="divide-y divide-rule">
              {accounts.map((item) => {
                const buckets = bucketAccountWindows(item, now);
                const statusMeta = ROW_STATUS[item.status];
                const chartOpen = expandedCharts.has(item.key);
                const canOpenChart = item.connectionId !== null && item.windows.length > 0;
                const titleText = item.plan
                  ? `${item.providerLabel} ${capitalized(item.plan)}`
                  : item.providerLabel;
                const topModelNames = item.models
                  .slice(0, 1)
                  .map((m) => splitRef(m.model).id || m.model)
                  .join(", ");

                return (
                  <Fragment key={item.key}>
                    <tr
                      className={cx(
                        "transition-colors hover:bg-[var(--color-row-hover)]",
                        !item.isActive && "opacity-70",
                      )}
                    >
                      <th scope="row" className="px-3.5 py-3 text-left align-middle font-normal">
                        <div className="flex min-w-[136px] max-w-[190px] items-center gap-2">
                          <span className="shrink-0">
                            <UsageProviderIcon provider={item.provider} size={20} />
                          </span>
                          <div className="flex min-w-0 flex-col">
                            <span className="truncate text-body font-medium text-fg">
                              {titleText}
                            </span>
                            <div className="flex min-w-0 items-baseline gap-1 text-caption">
                              <AccountPreviewLink item={item} rangeLabel={rangeLabel} />
                              {topModelNames ? (
                                <span className="truncate text-faint">· {topModelNames}</span>
                              ) : null}
                            </div>
                          </div>
                        </div>
                      </th>

                      {WINDOW_COLUMNS.map((col) => (
                        <WindowCell key={col.id} cell={buckets[col.id]} />
                      ))}

                      <td className="px-3 py-3 text-right align-middle">
                        <div className="inline-flex items-center justify-end gap-1.5">
                          <Badge tone={statusMeta.tone}>
                            <span className="inline-flex items-center gap-1.5">
                              <Dot tone={statusMeta.dot} />
                              {statusMeta.label}
                            </span>
                          </Badge>
                          {canOpenChart ? (
                            <button
                              type="button"
                              aria-expanded={chartOpen}
                              aria-label={`Forecast chart for ${titleText} ${item.accountName}`}
                              onClick={() => onToggleChart(item.key)}
                              className={cx(
                                "inline-flex h-7 w-7 items-center justify-center rounded-[var(--radius-control)] transition-colors",
                                "hover:bg-surface-2 hover:text-fg focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--color-focus-ring)]",
                                chartOpen ? "bg-surface-2 text-fg" : "text-faint",
                              )}
                            >
                              <ChartLineUp size={15} aria-hidden />
                            </button>
                          ) : null}
                        </div>
                      </td>
                    </tr>

                    {canOpenChart && chartOpen && item.connectionId ? (
                      <tr className="bg-surface-2/40">
                        <td colSpan={6} className="p-0">
                          <AccountForecastPanel
                            connectionId={item.connectionId}
                            windows={item.windows}
                            now={now}
                          />
                        </td>
                      </tr>
                    ) : null}
                  </Fragment>
                );
              })}
            </tbody>
          </table>
        </div>
      )}

      {quotaError ? (
        <div className="border-t border-line px-4 py-2 text-caption text-[var(--color-warn)]">
          Quota data partially unavailable; range totals are unaffected.
        </div>
      ) : null}
    </Card>
  );
}

/** Right-rail Quota Alerts card derived strictly from real tracked pace/limits. */
export function QuotaAlertsCard({
  alerts,
  isAdmin,
  quotaLoaded,
  hasTracked,
}: {
  alerts: QuotaAlert[];
  isAdmin: boolean;
  quotaLoaded: boolean;
  hasTracked: boolean;
}) {
  return (
    <Card className="flex flex-col gap-3 p-4">
      <div className="flex items-center justify-between gap-2">
        <div className="flex items-center gap-2">
          <Bell size={16} aria-hidden className="text-accent-text" />
          <h2 className="text-body font-semibold text-fg">Quota Alerts</h2>
        </div>
        <Link
          to="/providers"
          className="text-caption text-accent-text underline-offset-2 hover:underline focus-visible:underline"
        >
          View all →
        </Link>
      </div>

      {!isAdmin ? (
        <p className="py-3 text-caption text-faint">Quota alerts are visible to admins only.</p>
      ) : !quotaLoaded ? (
        <p className="py-3 text-caption text-faint">Checking quota headroom…</p>
      ) : !hasTracked ? (
        <p className="py-3 text-caption text-faint">No tracked quotas to monitor.</p>
      ) : alerts.length === 0 ? (
        <p className="py-3 text-caption text-muted">
          All tracked quotas are within safe headroom.
        </p>
      ) : (
        <ul role="list" className="flex flex-col divide-y divide-rule">
          {alerts.map((alert) => {
            const iconWrap =
              alert.severity === "neg"
                ? "bg-[var(--color-neg-tint)] text-[var(--color-neg)]"
                : alert.severity === "warn"
                  ? "bg-[var(--color-warn-tint)] text-[var(--color-warn)]"
                  : "bg-[var(--color-info-tint)] text-[var(--color-info)]";
            const Icon = alert.severity === "info" ? Info : WarningCircle;
            const inner = (
              <div className="flex items-start justify-between gap-2.5 py-2.5 first:pt-0.5 last:pb-0.5">
                <div className="flex min-w-0 items-start gap-2.5">
                  <span
                    aria-hidden
                    className={cx(
                      "mt-0.5 inline-flex h-6 w-6 shrink-0 items-center justify-center rounded-full",
                      iconWrap,
                    )}
                  >
                    <Icon size={14} weight="fill" />
                  </span>
                  <div className="flex min-w-0 flex-col gap-0.5">
                    <span className="text-small font-medium leading-snug text-fg">
                      {alert.title}
                    </span>
                    <span className="line-clamp-2 text-caption text-faint">{alert.detail}</span>
                  </div>
                </div>
                <span className="shrink-0 pt-0.5 text-caption text-faint tnum">{alert.cue}</span>
              </div>
            );

            return (
              <li key={alert.id}>
                {alert.connectionId ? (
                  <Link
                    to={`/providers?inspect=${encodeURIComponent(alert.connectionId)}`}
                    className="block rounded-[var(--radius-control)] transition-colors hover:bg-[var(--color-row-hover)] focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--color-focus-ring)]"
                  >
                    {inner}
                  </Link>
                ) : (
                  inner
                )}
              </li>
            );
          })}
        </ul>
      )}
    </Card>
  );
}

/** Right-rail bottom card: newest served requests across all providers. */
export function RecentActivityCard({
  events,
  providerDisplayMap,
  colorByProvider,
  loading,
}: {
  events: ReadonlyArray<UsageEvent>;
  providerDisplayMap: ReadonlyMap<string, string>;
  colorByProvider: ReadonlyMap<string, string>;
  loading: boolean;
}) {
  // Show the 6 newest requests that reached a provider (unrouted has its own disclosure).
  const items = events.filter((ev) => ev.provider.trim() !== "").slice(0, 6);

  return (
    <Card className="flex flex-col gap-3 p-4">
      <div className="flex items-center justify-between gap-2">
        <div className="flex items-center gap-2">
          <Pulse size={16} aria-hidden className="text-accent-text" />
          <h2 className="text-body font-semibold text-fg">Recent Activity</h2>
        </div>
        <Link
          to="/activity"
          className="text-caption text-accent-text underline-offset-2 hover:underline focus-visible:underline"
        >
          View all →
        </Link>
      </div>

      {loading && items.length === 0 ? (
        <p className="py-4 text-center text-small text-faint">Loading recent activity…</p>
      ) : items.length === 0 ? (
        <p className="py-4 text-center text-small text-faint">No recent provider requests.</p>
      ) : (
        <ul role="list" className="flex flex-col divide-y divide-rule text-small">
          {items.map((ev, idx) => {
            const totalTok = ev.input_tokens + ev.output_tokens;
            const failed = ev.status >= 400;
            const { id: modelShort } = splitRef(ev.model);
            const providerName = resolveProviderDisplay(ev.provider, providerDisplayMap).label;
            const dotColor = failed
              ? "var(--color-neg)"
              : (colorByProvider.get(ev.provider) ?? "var(--color-series-6)");

            return (
              <li
                key={ev.request_id ?? `${ev.ts}-${idx}`}
                className="flex items-center justify-between gap-2 py-2 first:pt-0.5 last:pb-0.5"
              >
                <div className="flex min-w-0 items-center gap-2">
                  <span
                    aria-hidden
                    className="h-2 w-2 shrink-0 rounded-full"
                    style={{ backgroundColor: dotColor }}
                  />
                  <span className="truncate font-medium text-fg" title={`${providerName} · ${ev.model}`}>
                    {modelShort || ev.model}
                  </span>
                  <span className={cx("shrink-0 text-caption tnum", failed ? "text-[var(--color-neg)]" : "text-muted")}>
                    · {failed ? `HTTP ${ev.status}` : `${compact(totalTok)} tok`}
                  </span>
                </div>
                <span className="shrink-0 text-caption text-faint tnum">{ago(ev.ts)}</span>
              </li>
            );
          })}
        </ul>
      )}
    </Card>
  );
}
