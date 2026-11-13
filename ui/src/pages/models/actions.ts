import { useCallback } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { api, type Model, type ModelTest, type SyncResult } from "../../lib/api";
import { qk } from "../../lib/queries";
import { useToast } from "../../components/toast";

/**
 * The registry mutations every model row can reach: enable, test, prefer a provider,
 * delete a custom row, and sync metadata. One hook so the board and the registry table
 * behave identically, and so a view never mounts a mutation it cannot reach.
 */
export function useModelActions() {
  const client = useQueryClient();
  const toast = useToast();

  const invalidateRegistry = useCallback(() => {
    void client.invalidateQueries({ queryKey: qk.models });
    void client.invalidateQueries({ queryKey: qk.modelProviders });
    void client.invalidateQueries({ queryKey: qk.system });
  }, [client]);

  const sync = useMutation({
    mutationFn: () => api.post<SyncResult>("/api/models/sync"),
    onSuccess: invalidateRegistry,
  });

  // The gateway's disabled list is keyed by bare model id (planner and registry both
  // compare against the id), so a ref with its provider prefix would store a key that
  // nothing ever reads back. One id disables that model on every provider.
  const toggle = useMutation({
    mutationFn: (model: Model) =>
      api.put<{ model: string; disabled: boolean }>(
        `/api/disabled/${encodeURIComponent(model.id)}${model.enabled ? "" : "/enable"}`,
      ),
    onMutate: async (model) => {
      await client.cancelQueries({ queryKey: qk.models });
      const previous = client.getQueryData<Model[]>(qk.models);
      client.setQueryData<Model[]>(qk.models, (old) =>
        old?.map((m) => (m.id === model.id ? { ...m, enabled: !model.enabled } : m)),
      );
      return { previous };
    },
    onError: (_error, _model, context) => {
      if (context?.previous) client.setQueryData(qk.models, context.previous);
    },
    onSettled: invalidateRegistry,
  });

  const test = useMutation({
    mutationFn: (model: Model) => api.post<ModelTest>("/api/models/test", { model: model.ref }),
    onSuccess: (result, model) => {
      if (result.ok) {
        toast.success(`Test passed · ${model.ref}`, `${result.status} · ${result.latency_ms}ms`);
      } else {
        toast.error(`Test failed · ${model.ref}`, result.error ?? `status ${result.status}`);
      }
    },
    onError: (error, model) =>
      toast.error(`Test failed · ${model.ref}`, error instanceof Error ? error.message : String(error)),
  });

  // One preference per canonical id, shared by every provider row of it; `null` clears.
  const prefer = useMutation({
    mutationFn: ({ model, provider }: { model: string; provider: string | null }) =>
      api.put<{ model: string; preferred_provider: string | null }>("/api/models/preference", {
        model,
        provider,
      }),
    onSuccess: () => void client.invalidateQueries({ queryKey: qk.models }),
  });

  const remove = useMutation({
    mutationFn: (model: Model) =>
      api.del<{ deleted: boolean }>(
        `/api/models/custom?provider=${encodeURIComponent(model.provider)}&id=${encodeURIComponent(model.id)}`,
      ),
    onSuccess: invalidateRegistry,
  });

  const onTest = useCallback((model: Model) => test.mutate(model), [test.mutate]);
  const onToggle = useCallback((model: Model) => toggle.mutate(model), [toggle.mutate]);
  const onPrefer = useCallback(
    (model: string, provider: string | null) => prefer.mutate({ model, provider }),
    [prefer.mutate],
  );
  const onSync = useCallback(() => sync.mutate(), [sync.mutate]);

  // Pending ids rather than whole mutation objects: the column list must keep the same
  // identity while the toolbar is being typed into.
  return {
    sync,
    toggle,
    test,
    prefer,
    remove,
    invalidateRegistry,
    testTarget: test.isPending ? (test.variables?.ref ?? null) : null,
    toggleTarget: toggle.isPending ? (toggle.variables?.id ?? null) : null,
    removeTarget: remove.isPending ? (remove.variables?.ref ?? null) : null,
    onTest,
    onToggle,
    onPrefer,
    onSync,
  };
}
