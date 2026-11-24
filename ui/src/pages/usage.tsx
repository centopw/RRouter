/**
 * `/usage` — multi-provider limits board and consumption breakdown.
 *
 * Top band: summary strip (at-risk / failing / healthy / no-limit counts + total range
 * spend) over a risk-sorted Limits Board showing EVERY account of EVERY provider and ALL
 * of its quota windows at once, alongside per-account range activity. Clicking "Forecast
 * chart" on any card lazily fetches that window's daily history without N-per-card polling.
 *
 * Below: the detailed provider → account → model drill-down table fed by the same
 * `/api/usage/summary?group_by=connection` query, so totals never disagree.
 */

import { useCallback, useEffect, useMemo, useState } from "react";
import { Button, Card, ErrorNote, PageHeader, Segmented } from "../components/ui";
import { RANGES, rangeOf, type RangeId } from "../lib/activity";
import { useUrlFilterState } from "../lib/url-state";
import {
  useAccountQuotas,
  useAccountSubscriptions,
  useConnections,
  useIsAdmin,
  useModelProviders,
  useUsage,
  useUsageSummaryByConnection,
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
import {
  buildBoardAccounts,
  filterBoardAccounts,
  resolveProviderDisplay,
  summarizeBoard,
  type BoardFilter,
} from "./usage/board-model";
import { LimitsBoard, SummaryStrip } from "./usage/limits";
import { worstQuotaPercent } from "./usage/quota";
import { UsageTable, type AccountInfo } from "./usage/table";

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

  const summary = useUsageSummaryByConnection(range.hours);
  const recentEvents = useUsage(RECENT_EVENTS_LIMIT, range.hours);
  const connections = useConnections();
  const modelProviders = useModelProviders();
  const quotas = useAccountQuotas(isAdmin);
  const subscriptions = useAccountSubscriptions(isAdmin);

  // Both sources answered: only then may a bar-less account read "Not reported".
  const quotaLoaded = isAdmin && quotas.isSuccess && subscriptions.isSuccess;
  const quotaError = isAdmin && (quotas.isError || subscriptions.isError);

  const [statusFilter, setStatusFilter] = useState<BoardFilter>("all");
  const [expandedCharts, setExpandedCharts] = useState<Set<string>>(() => new Set());
  const [expandedProviders, setExpandedProviders] = useState<Set<string>>(() => new Set());
  const [expandedAccounts, setExpandedAccounts] = useState<Set<string>>(() => new Set());

  // A `/providers` deep link opens that provider in the breakdown table automatically.
  useEffect(() => {
    if (!providerFilter) return;
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

  /** Providers with at least one active account that reports a limit right now. */
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
    () => withQuotaShells(buildProviderNodes(summary.data ?? []), providersWithQuota),
    [summary.data, providersWithQuota],
  );

  const providers = useMemo<ProviderNode[]>(() => {
    if (!providerFilter) return allProviders;
    const match = allProviders.filter((node) => node.provider === providerFilter);
    // Keep an empty provider shell so its accounts and quota still render.
    return match.length > 0
      ? match
      : [{ provider: providerFilter, totals: emptyTotals(), models: [] }];
  }, [allProviders, providerFilter]);

  const totals = useMemo(
    () => providers.reduce((acc, node) => addTotals(acc, node.totals), emptyTotals()),
    [providers],
  );

  // Compute one shared clock snapshot whenever data refreshes so pace comparisons stay coherent.
  const snapshotNow = useMemo(
    () => Date.now(),
    [summary.dataUpdatedAt, subscriptions.dataUpdatedAt, quotas.dataUpdatedAt, connections.dataUpdatedAt],
  );

  const boardAccounts = useMemo(
    () =>
      buildBoardAccounts({
        connections: connections.data ?? [],
        subscriptions: subscriptions.data ?? [],
        quotas: quotas.data ?? [],
        summaryRows: summary.data ?? [],
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
      summary.data,
      recentEvents.data,
      providerDisplayMap,
      providerFilter,
      isAdmin,
      quotaLoaded,
      quotaError,
      snapshotNow,
    ],
  );

  const boardSummary = useMemo(
    () => summarizeBoard(boardAccounts, totals),
    [boardAccounts, totals],
  );

  const visibleBoardAccounts = useMemo(
    () => filterBoardAccounts(boardAccounts, statusFilter),
    [boardAccounts, statusFilter],
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
      buildAccountNodes(summary.data ?? []),
      [...known].map(([id, provider]) => ({ id, provider })),
    );
  }, [connections.data, quotas.data, subscriptions.data, summary.data]);

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

  const handleClearFilter = useCallback(() => setStatusFilter("all"), []);

  const filteredProviderLabel = providerFilter
    ? resolveProviderDisplay(providerFilter, providerDisplayMap).label
    : "";

  const emptyText = providerFilter
    ? `No usage for ${filteredProviderLabel} in the last ${range.id}.`
    : `No requests in the last ${range.id}.`;

  const boardLoading =
    connections.isLoading && summary.isLoading && (!isAdmin || subscriptions.isLoading);

  return (
    <>
      <PageHeader
        title="Usage"
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

      <SummaryStrip
        summary={boardSummary}
        rangeLabel={range.id}
        filter={statusFilter}
        isAdmin={isAdmin}
        onFilterChange={setStatusFilter}
      />

      <LimitsBoard
        items={visibleBoardAccounts}
        rangeLabel={range.id}
        isAdmin={isAdmin}
        quotaLoaded={quotaLoaded}
        quotaError={quotaError}
        loading={boardLoading}
        activeFilter={statusFilter}
        onClearFilter={handleClearFilter}
        expandedCharts={expandedCharts}
        onToggleChart={handleToggleChart}
        now={snapshotNow}
      />

      <section aria-labelledby="usage-breakdown-heading">
        <div className="mb-2.5 flex flex-wrap items-baseline justify-between gap-2">
          <h2 id="usage-breakdown-heading" className="text-small font-semibold text-fg">
            Breakdown by provider, account and model
          </h2>
          <span className="text-caption text-faint">Last {range.id}</span>
        </div>

        <Card className="overflow-hidden">
          {summary.isError ? (
            <div className="px-4 pt-3">
              <ErrorNote error={summary.error} onRetry={() => void summary.refetch()} />
            </div>
          ) : null}

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

          {quotaError ? (
            <div className="border-t border-line px-4 py-2.5 text-caption text-warn">
              Quota data unavailable; usage totals are unaffected.
            </div>
          ) : null}
        </Card>
      </section>
    </>
  );
}
