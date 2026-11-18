/**
 * `/usage` — the consolidated consumption surface.
 *
 * Range totals and the provider/model rows are exactly what `/api/usage/summary`
 * aggregates. Account rows come from the same endpoint grouped by connection, so they
 * cover the whole range. Only when a server reports no such grouping do they fall back
 * to the capped request log, and then the table says the numbers came from it.
 * Quota columns are current-window values from the account APIs, never range totals.
 */

import { useEffect, useMemo, useState } from "react";
import { Button, Card, ErrorNote, Segmented, PageHeader } from "../components/ui";
import { RANGES, rangeOf, type RangeId } from "../lib/activity";
import { useUrlFilterState } from "../lib/url-state";
import {
  useAccountQuotas,
  useAccountSubscriptions,
  useConnections,
  useIsAdmin,
  useUsage,
  useUsageSummary,
  useUsageSummaryByConnection,
} from "../lib/queries";
import { providerLabel } from "./providers/types";
import {
  addTotals,
  buildAccountNodes,
  buildObservedAccounts,
  buildProviderNodes,
  emptyTotals,
  isConnectionGrouped,
  mergeKnownAccounts,
  type ProviderNode,
} from "./usage/aggregate";
import { UsageTable, type AccountInfo } from "./usage/table";

const DEFAULT_RANGE: RangeId = "30d";
const SAMPLE_LIMIT = 1000;

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

  const summary = useUsageSummary(range.hours);
  // Exact per-account totals. Off only when the provider summary already failed, which
  // leaves the page in its error state anyway.
  const connectionSummary = useUsageSummaryByConnection(range.hours, !summary.isError);
  const connections = useConnections();
  const quotas = useAccountQuotas(isAdmin);
  const subscriptions = useAccountSubscriptions(isAdmin);

  // The connection aggregate is the source of truth for account rows. A server that
  // answers without `connection_id` has ignored `group_by`, so its rows are not
  // per-account and the log fallback below has to carry the table (with its caveat).
  const aggregateRows = connectionSummary.data;
  const aggregateUsable =
    aggregateRows !== undefined &&
    (aggregateRows.length > 0
      ? isConnectionGrouped(aggregateRows)
      : (summary.data?.length ?? 0) === 0);
  const usage = useUsage(SAMPLE_LIMIT, range.hours, undefined, !aggregateUsable);

  const [expandedProviders, setExpandedProviders] = useState<Set<string>>(() => new Set());
  const [expandedAccounts, setExpandedAccounts] = useState<Set<string>>(() => new Set());

  // A `/providers` deep link opens that provider instead of leaving the reader to hunt.
  useEffect(() => {
    if (!providerFilter) return;
    setExpandedProviders((prev) =>
      prev.has(providerFilter) ? prev : new Set(prev).add(providerFilter),
    );
  }, [providerFilter]);

  const allProviders = useMemo(() => buildProviderNodes(summary.data ?? []), [summary.data]);

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
      aggregateUsable
        ? buildAccountNodes(connectionSummary.data ?? [])
        : buildObservedAccounts(usage.data ?? []),
      [...known].map(([id, provider]) => ({ id, provider })),
    );
  }, [
    connections.data,
    quotas.data,
    subscriptions.data,
    usage.data,
    aggregateUsable,
    connectionSummary.data,
  ]);

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

  const fallback = !aggregateUsable;
  const sampleCapped = fallback && (usage.data?.length ?? 0) >= SAMPLE_LIMIT;
  const emptyText = providerFilter
    ? `No usage for ${providerLabel(providerFilter)} in the last ${range.id}.`
    : `No requests in the last ${range.id}.`;

  return (
    <>
      <PageHeader
        title="Usage"
        subtitle="Consumption and quota by provider, account and model. Totals and quota use different windows and say so."
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
            ariaLabel={`Clear the ${providerLabel(providerFilter)} filter`}
          >
            {providerLabel(providerFilter)} <span aria-hidden>×</span>
          </Button>
        </div>
      ) : null}

      <Card>
        <div className="flex flex-wrap items-center justify-between gap-x-4 gap-y-1 border-b border-line px-4 py-2.5 text-caption text-faint">
          <span>
            Requests, tokens and spend cover the last {range.id}; quota is each account's
            current window.
          </span>
          {fallback ? (
            <span>
              account and model rows from the request log
              {sampleCapped
                ? `, capped at the newest ${SAMPLE_LIMIT.toLocaleString()} requests`
                : ""}
            </span>
          ) : null}
        </div>

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
          loading={summary.isLoading}
          emptyText={emptyText}
          expandedProviders={expandedProviders}
          expandedAccounts={expandedAccounts}
          onToggleProvider={(provider) =>
            setExpandedProviders((prev) => {
              const next = new Set(prev);
              if (next.has(provider)) next.delete(provider);
              else next.add(provider);
              return next;
            })
          }
          onToggleAccount={(key) =>
            setExpandedAccounts((prev) => {
              const next = new Set(prev);
              if (next.has(key)) next.delete(key);
              else next.add(key);
              return next;
            })
          }
        />

        <div className="flex flex-wrap items-center gap-x-4 gap-y-1 border-t border-line px-4 py-2.5 text-caption text-faint">
          <span>Only provider-reported or operator limits get a bar; unconfigured means no limit.</span>
          {isAdmin && (quotas.isError || subscriptions.isError) ? (
            <span className="text-warn">quota data unavailable; usage totals are unaffected</span>
          ) : null}
        </div>
      </Card>
    </>
  );
}
