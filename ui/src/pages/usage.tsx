/**
 * `/usage` — Usage Overview.
 *
 * Top row: 4 KPI tiles (Peak Quota Usage, Active Providers, Estimated Days Left, Total Spend).
 * Middle band: Tracked Providers quota table (ONLY providers/accounts whose quota is tracked,
 * with 5h / Daily / Weekly / Monthly progress bars) alongside Quota Alerts & Usage Breakdown.
 * Bottom band: Usage Trend multi-line chart alongside Recent Activity.
 * Secondary disclosures below: "Requests that never reached a provider" (per-reason breakdown)
 * and the full provider → account → model drill-down table across all providers.
 */

import { useCallback, useEffect, useMemo, useState } from "react";
import { CaretRight } from "@phosphor-icons/react";
import { Button, Card, ErrorNote, PageHeader, Segmented, cx } from "../components/ui";
import { RANGES, rangeOf, type RangeId } from "../lib/activity";
import { useUrlFilterState } from "../lib/url-state";
import {
  useAccountQuotas,
  useAccountSubscriptions,
  useConnections,
  useIsAdmin,
  useModelProviders,
  useUsage,
  useUsageByModel,
} from "../lib/queries";
import {
  addTotals,
  buildAccountNodes,
  buildProviderNodes,
  emptyTotals,
  mergeKnownAccounts,
  withQuotaShells,
  type ProviderNode,
} from "./usage/aggregate";
import { buildBoardAccounts, resolveProviderDisplay } from "./usage/board-model";
import { UsageBreakdownCard, UsageTrendCard } from "./usage/overview-charts";
import {
  buildQuotaAlerts,
  buildUsageBreakdown,
  buildUsageTrend,
  computeOverviewKpis,
  trackedQuotaAccounts,
} from "./usage/overview-model";
import {
  OverviewKpiRow,
  QuotaAlertsCard,
  RecentActivityCard,
  TrackedProvidersTable,
} from "./usage/overview-panels";
import { worstQuotaPercent } from "./usage/quota";
import { useUsageSummaryWithUnrouted } from "./usage/summary-query";
import { UsageTable, type AccountInfo } from "./usage/table";
import { UnroutedDisclosure } from "./usage/unrouted";
import { buildUnroutedSummary } from "./usage/unrouted-model";

const DEFAULT_RANGE: RangeId = "30d";
const RECENT_EVENTS_LIMIT = 200;

export default function UsagePage() {
  const isAdmin = useIsAdmin();
  const { values, setFilter } = useUrlFilterState({
    defaults: { range: DEFAULT_RANGE, provider: "" },
    sanitize: {
      range: (raw) => (RANGES.some((item) => item.id === raw) ? (raw as RangeId) : DEFAULT_RANGE),
    },
  });
  const range = rangeOf(values.range as RangeId);
  const providerFilter = values.provider;

  const summary = useUsageSummaryWithUnrouted(range.hours);
  const summaryRows = summary.data?.rows;
  const recentEvents = useUsage(RECENT_EVENTS_LIMIT, range.hours);
  const trendQuery = useUsageByModel(range.hours, range.bucket);
  const connections = useConnections();
  const modelProviders = useModelProviders();
  const quotas = useAccountQuotas(isAdmin);
  const subscriptions = useAccountSubscriptions(isAdmin);

  const quotaLoaded = isAdmin && quotas.isSuccess && subscriptions.isSuccess;
  const quotaError = isAdmin && (quotas.isError || subscriptions.isError);

  const [expandedCharts, setExpandedCharts] = useState<Set<string>>(() => new Set());
  const [expandedProviders, setExpandedProviders] = useState<Set<string>>(() => new Set());
  const [expandedAccounts, setExpandedAccounts] = useState<Set<string>>(() => new Set());
  const [breakdownOpen, setBreakdownOpen] = useState(false);

  // A `/providers` deep link opens that provider in the full breakdown disclosure automatically.
  useEffect(() => {
    if (!providerFilter) return;
    setBreakdownOpen(true);
    setExpandedProviders((prev) =>
      prev.has(providerFilter) ? prev : new Set(prev).add(providerFilter),
    );
  }, [providerFilter]);

  const providerDisplayMap = useMemo(() => {
    const map = new Map<string, string>();
    for (const provider of modelProviders.data ?? []) {
      if (provider.display.trim()) map.set(provider.id, provider.display.trim());
    }
    return map;
  }, [modelProviders.data]);

  const providersWithQuota = useMemo(() => {
    const inactive = new Set(
      (connections.data ?? []).filter((c) => !c.is_active).map((c) => c.id),
    );
    const subscriptionById = new Map(
      (subscriptions.data ?? []).map((row) => [row.connection_id, row]),
    );
    const out = new Set<string>();
    for (const quota of quotas.data ?? []) {
      if (!quota.is_active || inactive.has(quota.connection_id)) continue;
      if (worstQuotaPercent(quota, subscriptionById.get(quota.connection_id)) !== null) {
        out.add(quota.provider);
      }
    }
    for (const subscription of subscriptions.data ?? []) {
      if (inactive.has(subscription.connection_id)) continue;
      if (worstQuotaPercent(undefined, subscription) !== null) out.add(subscription.provider);
    }
    return out;
  }, [connections.data, quotas.data, subscriptions.data]);

  const allProviders = useMemo(
    () => withQuotaShells(buildProviderNodes(summaryRows ?? []), providersWithQuota),
    [summaryRows, providersWithQuota],
  );

  const providers = useMemo<ProviderNode[]>(() => {
    if (!providerFilter) return allProviders;
    const match = allProviders.filter((node) => node.provider === providerFilter);
    return match.length > 0
      ? match
      : [{ provider: providerFilter, totals: emptyTotals(), models: [] }];
  }, [allProviders, providerFilter]);

  const totals = useMemo(
    () => providers.reduce((acc, node) => addTotals(acc, node.totals), emptyTotals()),
    [providers],
  );

  const snapshotNow = useMemo(
    () => Date.now(),
    [
      summary.dataUpdatedAt,
      subscriptions.dataUpdatedAt,
      quotas.dataUpdatedAt,
      connections.dataUpdatedAt,
      trendQuery.dataUpdatedAt,
    ],
  );

  const boardAccounts = useMemo(
    () =>
      buildBoardAccounts({
        connections: connections.data ?? [],
        subscriptions: subscriptions.data ?? [],
        quotas: quotas.data ?? [],
        summaryRows: summaryRows ?? [],
        recentEvents: recentEvents.data ?? [],
        providerDisplayMap,
        providerFilter,
        isAdmin,
        quotaLoaded,
        quotaError,
        now: snapshotNow,
      }),
    [
      connections.data,
      subscriptions.data,
      quotas.data,
      summaryRows,
      recentEvents.data,
      providerDisplayMap,
      providerFilter,
      isAdmin,
      quotaLoaded,
      quotaError,
      snapshotNow,
    ],
  );

  // Strictly ONLY accounts with tracked quota appear in the Providers quota table & alerts.
  const trackedAccounts = useMemo(() => trackedQuotaAccounts(boardAccounts), [boardAccounts]);

  const kpis = useMemo(
    () =>
      computeOverviewKpis({
        boardAccounts,
        trackedAccounts,
        totals,
        isAdmin,
        quotaLoaded,
        now: snapshotNow,
      }),
    [boardAccounts, trackedAccounts, totals, isAdmin, quotaLoaded, snapshotNow],
  );

  const quotaAlerts = useMemo(
    () => (isAdmin && quotaLoaded ? buildQuotaAlerts(trackedAccounts, snapshotNow) : []),
    [isAdmin, quotaLoaded, trackedAccounts, snapshotNow],
  );

  const usageBreakdown = useMemo(
    () => buildUsageBreakdown(providers, providerDisplayMap),
    [providers, providerDisplayMap],
  );

  const colorByProvider = useMemo(() => {
    const map = new Map<string, string>();
    for (const slice of usageBreakdown.slices) {
      if (slice.id !== "__others__") map.set(slice.id, slice.color);
    }
    return map;
  }, [usageBreakdown.slices]);

  const filteredTrendBuckets = useMemo(() => {
    const rows = trendQuery.data ?? [];
    if (!providerFilter) return rows;
    return rows.filter((row) => row.provider === providerFilter);
  }, [trendQuery.data, providerFilter]);

  const usageTrend = useMemo(
    () => buildUsageTrend(filteredTrendBuckets, range, snapshotNow, providerDisplayMap),
    [filteredTrendBuckets, range, snapshotNow, providerDisplayMap],
  );

  const filteredRecentEvents = useMemo(() => {
    const rows = recentEvents.data ?? [];
    if (!providerFilter) return rows;
    return rows.filter((ev) => ev.provider === providerFilter);
  }, [recentEvents.data, providerFilter]);

  const unroutedSummary = useMemo(
    () =>
      buildUnroutedSummary({
        summaryRows: summaryRows ?? [],
        recentEvents: recentEvents.data ?? [],
        breakdown: summary.data?.unrouted ?? null,
      }),
    [summaryRows, recentEvents.data, summary.data?.unrouted],
  );

  const accountsByProvider = useMemo(() => {
    const known = new Map<string, string>();
    for (const connection of connections.data ?? []) known.set(connection.id, connection.provider);
    for (const quota of quotas.data ?? []) {
      if (!known.has(quota.connection_id)) known.set(quota.connection_id, quota.provider);
    }
    for (const subscription of subscriptions.data ?? []) {
      if (!known.has(subscription.connection_id)) {
        known.set(subscription.connection_id, subscription.provider);
      }
    }
    return mergeKnownAccounts(
      buildAccountNodes(summaryRows ?? []),
      [...known].map(([id, provider]) => ({ id, provider })),
    );
  }, [connections.data, quotas.data, subscriptions.data, summaryRows]);

  const accountInfo = useMemo(() => {
    const info = new Map<string, AccountInfo>();
    for (const quota of quotas.data ?? []) {
      info.set(quota.connection_id, {
        name: quota.name,
        email: null,
        isActive: quota.is_active,
      });
    }
    for (const subscription of subscriptions.data ?? []) {
      const prior = info.get(subscription.connection_id);
      info.set(subscription.connection_id, {
        name: subscription.name ?? prior?.name ?? null,
        email: prior?.email ?? null,
        isActive: prior?.isActive ?? null,
      });
    }
    for (const connection of connections.data ?? []) {
      info.set(connection.id, {
        name: connection.name,
        email: connection.email,
        isActive: connection.is_active,
      });
    }
    return info;
  }, [connections.data, quotas.data, subscriptions.data]);

  const quotaByConnection = useMemo(
    () => new Map((quotas.data ?? []).map((row) => [row.connection_id, row])),
    [quotas.data],
  );
  const subscriptionByConnection = useMemo(
    () => new Map((subscriptions.data ?? []).map((row) => [row.connection_id, row])),
    [subscriptions.data],
  );

  const handleToggleChart = useCallback((key: string) => {
    setExpandedCharts((prev) => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  }, []);

  const handleToggleProvider = useCallback((provider: string) => {
    setExpandedProviders((prev) => {
      const next = new Set(prev);
      if (next.has(provider)) next.delete(provider);
      else next.add(provider);
      return next;
    });
  }, []);

  const handleToggleAccount = useCallback((key: string) => {
    setExpandedAccounts((prev) => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  }, []);

  const filteredProviderLabel = providerFilter
    ? resolveProviderDisplay(providerFilter, providerDisplayMap).label
    : "";

  const emptyText = providerFilter
    ? `No usage for ${filteredProviderLabel} in the last ${range.id}.`
    : `No requests in the last ${range.id}.`;

  return (
    <>
      <PageHeader
        title="Usage Overview"
        subtitle="Monitor your quota across tracked providers"
        actions={
          <Segmented
            options={RANGES.map((item) => ({ id: item.id, label: item.id }))}
            value={range.id}
            onChange={(id) => setFilter("range", id, { replace: true })}
            ariaLabel="Usage range"
          />
        }
      />

      {providerFilter ? (
        <div className="mb-3 flex flex-wrap items-center gap-2 text-small">
          <span className="text-faint">Filtered to</span>
          <Button
            variant="quiet"
            size="sm"
            onClick={() => setFilter("provider", null)}
            ariaLabel={`Clear the ${filteredProviderLabel} filter`}
          >
            {filteredProviderLabel} <span aria-hidden>×</span>
          </Button>
        </div>
      ) : null}

      {summary.isError ? (
        <div className="mb-4">
          <ErrorNote error={summary.error} onRetry={() => void summary.refetch()} />
        </div>
      ) : null}

      <OverviewKpiRow
        kpis={kpis}
        rangeLabel={range.id}
        spendSeries={usageTrend.spendMicros}
        isAdmin={isAdmin}
      />

      <div className="mb-4 grid grid-cols-1 gap-4 xl:grid-cols-[minmax(0,1fr)_316px]">
        <TrackedProvidersTable
          accounts={trackedAccounts}
          rangeLabel={range.id}
          isAdmin={isAdmin}
          quotaLoaded={quotaLoaded}
          quotaError={quotaError}
          expandedCharts={expandedCharts}
          onToggleChart={handleToggleChart}
          now={snapshotNow}
        />

        <div className="flex min-w-0 flex-col gap-4">
          <QuotaAlertsCard
            alerts={quotaAlerts}
            isAdmin={isAdmin}
            quotaLoaded={quotaLoaded}
            hasTracked={trackedAccounts.length > 0}
          />
          <UsageBreakdownCard breakdown={usageBreakdown} rangeLabel={range.id} />
        </div>
      </div>

      <div className="mb-4 grid grid-cols-1 gap-4 xl:grid-cols-[minmax(0,1fr)_316px]">
        <UsageTrendCard
          trend={usageTrend}
          range={range}
          now={snapshotNow}
          loading={trendQuery.isLoading}
        />
        <RecentActivityCard
          events={filteredRecentEvents}
          providerDisplayMap={providerDisplayMap}
          colorByProvider={colorByProvider}
          loading={recentEvents.isLoading}
        />
      </div>

      <div className="flex flex-col gap-3">
        {!providerFilter ? (
          <UnroutedDisclosure summary={unroutedSummary} rangeLabel={range.id} />
        ) : null}

        <Card className="overflow-hidden">
          <button
            type="button"
            aria-expanded={breakdownOpen}
            onClick={() => setBreakdownOpen((prev) => !prev)}
            className="flex w-full flex-wrap items-center justify-between gap-2 px-4 py-2.5 text-left transition-colors hover:bg-[var(--color-row-hover)] focus-visible:outline focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-[var(--color-focus-ring)]"
          >
            <span className="flex items-center gap-2 text-small">
              <CaretRight
                size={12}
                aria-hidden
                className={cx("shrink-0 text-faint transition-transform", breakdownOpen && "rotate-90")}
              />
              <span className="font-medium text-fg">
                All providers, accounts &amp; models
              </span>
              <span className="text-caption text-faint tnum">
                · {providers.length} {providers.length === 1 ? "provider" : "providers"}
              </span>
            </span>
            <span className="text-caption text-faint">Last {range.id}</span>
          </button>

          {breakdownOpen ? (
            <div className="border-t border-rule">
              <UsageTable
                providers={providers}
                totals={totals}
                accountsByProvider={accountsByProvider}
                accountInfo={accountInfo}
                quotaByConnection={quotaByConnection}
                subscriptionByConnection={subscriptionByConnection}
                providerDisplayMap={providerDisplayMap}
                showQuota={isAdmin}
                quotaLoaded={quotaLoaded}
                loading={summary.isLoading}
                emptyText={emptyText}
                expandedProviders={expandedProviders}
                expandedAccounts={expandedAccounts}
                onToggleProvider={handleToggleProvider}
                onToggleAccount={handleToggleAccount}
              />
            </div>
          ) : null}
        </Card>
      </div>
    </>
  );
}
