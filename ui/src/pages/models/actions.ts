import { useCallback, useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { api, type Model, type ModelTest, type SyncResult } from "../../lib/api";
import { qk } from "../../lib/queries";

/** Inline result recorded on a model row after the operator runs a test completion. */
export type ModelTestOutcome = {
  ok: boolean;
  status: number;
  latency_ms: number;
  error?: string;
  at: number;
};

/**
 * The registry mutations every model row can reach: enable, test, delete a custom row,
 * and sync metadata. One hook so the board and flat table behave identically, and test
 * outcomes stay visible inline on the tested row instead of vanishing in a toast.
 */
export function useModelActions() {
  const client = useQueryClient();
  const [testResults, setTestResults] = useState<ReadonlyMap<string, ModelTestOutcome>>(
    () => new Map(),
  );

  const recordTest = useCallback((ref: string, outcome: ModelTestOutcome) => {
    setTestResults((previous) => {
      const next = new Map(previous);
      next.set(ref, outcome);
      return next;
    });
  }, []);

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
      recordTest(model.ref, {
        ok: result.ok,
        status: result.status,
        latency_ms: result.latency_ms,
        error: result.error ?? undefined,
        at: Date.now(),
      });
    },
    onError: (error, model) => {
      recordTest(model.ref, {
        ok: false,
        status: 0,
        latency_ms: 0,
        error: error instanceof Error ? error.message : String(error),
        at: Date.now(),
      });
    },
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
  const onSync = useCallback(() => sync.mutate(), [sync.mutate]);

  // Pending ids rather than whole mutation objects: the column list must keep the same
  // identity while the toolbar is being typed into.
  return {
    sync,
    toggle,
    test,
    remove,
    invalidateRegistry,
    testResults,
    testTarget: test.isPending ? (test.variables?.ref ?? null) : null,
    toggleTarget: toggle.isPending ? (toggle.variables?.id ?? null) : null,
    removeTarget: remove.isPending ? (remove.variables?.ref ?? null) : null,
    onTest,
    onToggle,
    onSync,
  };
}
