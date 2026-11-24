/**
 * React Query hooks for the redesigned Free mode surface.
 *
 * Lives under `ui/src/pages/free-mode/` while sharing `qk.freeMode` with `lib/queries.ts`
 * so Advanced import/discover/teardown mutations keep the cache in sync automatically.
 */
import { useCallback, useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import {
  patchFreeMode,
  patchFreeModeModel,
  probeAllFreeModels,
  probeFreeModel,
  refreshFreeMode,
  type FreeModePatch,
  type FreeModeState,
} from "../../lib/free-mode";
import { qk } from "../../lib/queries";
import {
  applyProbeAllToState,
  applyProbeSingleToState,
  applyToggleToState,
  modelKey,
} from "./model";

/** Toggle pool (`enabled`) or automatic fallback (`fallback_on_quota`). */
export function usePatchFreeMode() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (patch: FreeModePatch) => patchFreeMode(patch),
    onSuccess: (next) => {
      client.setQueryData<FreeModeState>(qk.freeMode, next);
    },
  });
}

/** Enable/disable one free model with per-row busy tracking and immediate cache update. */
export function useToggleFreeModel() {
  const client = useQueryClient();
  const [pendingKeys, setPendingKeys] = useState<Set<string>>(() => new Set());

  const mutation = useMutation({
    mutationFn: (input: { provider: string; model: string; enabled: boolean }) =>
      patchFreeModeModel(input),
    onMutate: (input) => {
      const key = modelKey(input);
      setPendingKeys((prev) => new Set(prev).add(key));
      return { key };
    },
    onSuccess: (result) => {
      client.setQueryData<FreeModeState>(qk.freeMode, (prev) =>
        prev ? applyToggleToState(prev, result) : prev,
      );
    },
    onSettled: (_data, _error, _vars, ctx) => {
      if (ctx) {
        setPendingKeys((prev) => {
          const next = new Set(prev);
          next.delete(ctx.key);
          return next;
        });
      }
      void client.invalidateQueries({ queryKey: qk.freeMode });
    },
  });

  const isRowPending = useCallback(
    (provider: string, model: string) => pendingKeys.has(modelKey({ provider, model })),
    [pendingKeys],
  );

  return { ...mutation, isRowPending };
}

/**
 * Probe one free model inline ("Try"). Supports testing several rows independently
 * without blocking unrelated rows.
 */
export function useProbeFreeModel() {
  const client = useQueryClient();
  const [probingKeys, setProbingKeys] = useState<Set<string>>(() => new Set());
  const [rowErrors, setRowErrors] = useState<Record<string, string>>({});

  const probe = useCallback(
    async (input: { provider: string; model: string }) => {
      const key = modelKey(input);
      setProbingKeys((prev) => new Set(prev).add(key));
      setRowErrors((prev) => {
        if (!(key in prev)) return prev;
        const next = { ...prev };
        delete next[key];
        return next;
      });
      try {
        const result = await probeFreeModel(input);
        client.setQueryData<FreeModeState>(qk.freeMode, (prev) =>
          prev ? applyProbeSingleToState(prev, result) : prev,
        );
      } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        setRowErrors((prev) => ({ ...prev, [key]: message }));
      } finally {
        setProbingKeys((prev) => {
          const next = new Set(prev);
          next.delete(key);
          return next;
        });
      }
    },
    [client],
  );

  const isRowProbing = useCallback(
    (provider: string, model: string) => probingKeys.has(modelKey({ provider, model })),
    [probingKeys],
  );

  const errorForRow = useCallback(
    (provider: string, model: string): string | null =>
      rowErrors[modelKey({ provider, model })] ?? null,
    [rowErrors],
  );

  return { probe, isRowProbing, errorForRow };
}

/**
 * Refresh button behavior:
 *   - When the pool is off: re-fetches public catalogs (`GET /api/free-mode?refresh=true`)
 *     without running live probes.
 *   - When the pool is on: refreshes catalogs and re-probes every enabled usable model
 *     (`POST /api/free-mode/probe { all: true, refresh_catalogs: true }`), then reloads state.
 */
export function useRefreshFreeMode() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: async (poolEnabled: boolean) => {
      if (!poolEnabled) {
        const state = await refreshFreeMode();
        client.setQueryData<FreeModeState>(qk.freeMode, state);
        return;
      }
      const result = await probeAllFreeModels({ refreshCatalogs: true });
      client.setQueryData<FreeModeState>(qk.freeMode, (prev) =>
        prev ? applyProbeAllToState(prev, result) : prev,
      );
      await client.invalidateQueries({ queryKey: qk.freeMode });
    },
  });
}
