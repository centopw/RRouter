/**
 * `/usage` — the consolidated consumption surface.
 *
 * One range query feeds the table: `/api/usage/summary?group_by=connection`. Account
 * rows group it by connection; provider and model rows fold the same rows, so the
 * provider row is always the exact sum of its accounts. Quota is each account's
 * current window from the account APIs, never a range total, so a provider with quota
 * stays in the table even when the range has no traffic for it.
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
  useUsageSummaryByConnection,
} from "../lib/queries";
import { providerLabel } from "./providers/types";
import {
  addTotals,
  buildAccountNodes,
  buildProviderNodes,
  emptyTotals,
  mergeKnownAccounts,
  withQuotaShells,
  type ProviderNode,
} from "./usage/aggregate";
import { Headline } from "./usage/headline";
import { LimitsCard } from "./usage/limits";
import { worstQuotaPercent } from "./usage/quota";
import { UsageTable, type AccountInfo } from "./usage/table";

const DEFAULT_RANGE: RangeId = "30d";

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
  const connections = useConnections();
  const quotas = useAccountQuotas(isAdmin);
  const subscriptions = useAccountSubscriptions(isAdmin);
  // Both sources answered: only then may a bar-less account read "no limit".
  const quotaLoaded = isAdmin && quotas.isSuccess && subscriptions.isSuccess;

  const [expandedProviders, setExpandedProviders] = useState<Set<string>>(() => new Set());
  const [expandedAccounts, setExpandedAccounts] = useState<Set<string>>(() => new Set());

  // A `/providers` deep link opens that provider instead of leaving the reader to hunt.
  useEffect(() => {
    if (!providerFilter) return;
    setExpandedProviders((prev) =>
      prev.has(providerFilter) ? prev : new Set(prev).add(providerFilter),
    );
  }, [providerFilter]);

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

  const emptyText = providerFilter
    ? `No usage for ${providerLabel(providerFilter)} in the last ${range.id}.`
    : `No requests in the last ${range.id}.`;

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
            ariaLabel={`Clear the ${providerLabel(providerFilter)} filter`}
          >
            {providerLabel(providerFilter)} <span aria-hidden>×</span>
          </Button>
        </div>
      ) : null}

      {/* Plan windows are current, not range-gated: this band ignores the range control.
          Both render nothing without a reported window (non-admins get none), and the
          band collapses with them. */}
      <section
        className="mb-4 grid items-start gap-4 empty:hidden lg:grid-cols-[minmax(0,1fr)_minmax(320px,400px)]"
      >
        <Headline accounts={subscriptions.data ?? []} connections={connections.data} />
        <LimitsCard accounts={subscriptions.data ?? []} connections={connections.data} />
      </section>

      <Card>
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
          showQuota={isAdmin}
          quotaLoaded={quotaLoaded}
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

        {isAdmin && (quotas.isError || subscriptions.isError) ? (
          <div className="border-t border-line px-4 py-2.5 text-caption text-warn">
            Quota data unavailable; usage totals are unaffected.
          </div>
        ) : null}
      </Card>
    </>
  );
}
