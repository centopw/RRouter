import { useState } from "react";
import { Card, ErrorNote, PageHeader } from "../components/ui";
import { useFreeMode } from "../lib/queries";
import { AdvancedSection } from "./free-mode/advanced-section";
import { FallbackCard } from "./free-mode/fallback-card";
import {
  usePatchFreeMode,
  useProbeFreeModel,
  useRefreshFreeMode,
  useToggleFreeModel,
} from "./free-mode/hooks";
import type { FreeModel } from "./free-mode/model";
import { ModelsSection } from "./free-mode/models-section";
import { StatusHero } from "./free-mode/status-hero";

/**
 * Free mode: find free models automatically, turn them on in one click with live probe
 * results, and opt in to automatic fallback when paid quota runs out.
 *
 * The JSON document importer and per-account proxies stay intact under Advanced.
 */
export default function FreeMode() {
  const free = useFreeMode();
  const patchPool = usePatchFreeMode();
  const toggleModel = useToggleFreeModel();
  const probeModel = useProbeFreeModel();
  const refresh = useRefreshFreeMode();

  const [actionError, setActionError] = useState<unknown>(null);
  const state = free.data;

  const handleTogglePool = (enabled: boolean) => {
    setActionError(null);
    patchPool.mutate(
      { enabled, ...(enabled ? { probe_on_enable: true } : {}) },
      { onError: (error) => setActionError(error) },
    );
  };

  const handleToggleFallback = (fallback_on_quota: boolean) => {
    setActionError(null);
    patchPool.mutate(
      { fallback_on_quota },
      { onError: (error) => setActionError(error) },
    );
  };

  const handleToggleModel = (row: FreeModel, enabled: boolean) => {
    setActionError(null);
    toggleModel.mutate(
      { provider: row.provider, model: row.model, enabled },
      { onError: (error) => setActionError(error) },
    );
  };

  const handleRefresh = () => {
    setActionError(null);
    refresh.mutate(state?.enabled === true, {
      onError: (error) => setActionError(error),
    });
  };

  const isTogglingPool =
    patchPool.isPending && patchPool.variables?.enabled !== undefined;
  const isProbingPool =
    patchPool.isPending && patchPool.variables?.enabled === true;
  const isTogglingFallback =
    patchPool.isPending && patchPool.variables?.fallback_on_quota !== undefined;

  return (
    <div className="flex flex-col gap-5">
      <PageHeader title="Free mode" />

      {free.isError ? (
        <ErrorNote error={free.error} onRetry={() => void free.refetch()} />
      ) : null}

      {actionError ? (
        <ErrorNote error={actionError} title="Could not update free mode" />
      ) : null}

      {state ? (
        <>
          <StatusHero
            state={state}
            togglingPool={isTogglingPool}
            probingPool={isProbingPool}
            refreshing={refresh.isPending}
            onTogglePool={handleTogglePool}
            onRefresh={handleRefresh}
          />

          <FallbackCard
            state={state}
            busy={isTogglingFallback}
            onToggleFallback={handleToggleFallback}
          />

          <ModelsSection
            models={state.models}
            loading={false}
            refreshing={refresh.isPending}
            onToggleModel={handleToggleModel}
            isRowTogglePending={toggleModel.isRowPending}
            onProbeModel={(row) => void probeModel.probe({ provider: row.provider, model: row.model })}
            isRowProbing={probeModel.isRowProbing}
            errorForRow={probeModel.errorForRow}
            onRefresh={handleRefresh}
          />

          <AdvancedSection
            stored={state}
            onRefetch={async () => (await free.refetch()).data}
          />
        </>
      ) : free.isLoading ? (
        <Card className="px-5 py-6 text-small text-muted">
          Checking discovered free models…
        </Card>
      ) : null}
    </div>
  );
}
