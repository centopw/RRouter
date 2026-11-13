import { useDeferredValue, useEffect, useMemo, useState } from "react";
import {
  Button,
  Card,
  CardHeader,
  ConfirmDialog,
  EmptyState,
  ErrorNote,
  Input,
  Select,
} from "../../components/ui";
import { DataTable, PaginationBar } from "../../components/table";
import { FilterChip } from "../../components/atoms/FilterChip";
import { usePhone } from "../../lib/media";
import { useModelProviders, useModels } from "../../lib/queries";
import type { Model } from "../../lib/api";
import { compareModels } from "../../lib/models";
import { useModelActions } from "./actions";
import { useModelColumns } from "./columns";
import { CustomModelModal } from "./custom-model";
import { EmptyRegistry, ModelCards, rowKey } from "./rows";
import { ALL, pageOf, sizeOf } from "./params";

/**
 * REGISTRY — every model this gateway can route, including the ones that are disabled.
 * The filters are the view's own: they live in the URL so a palette hit (`?q=`) and a
 * reload land on the same rows.
 */
export function RegistryView({
  isAdmin,
  params,
  setFilters,
}: {
  isAdmin: boolean;
  params: URLSearchParams;
  setFilters: (patch: Record<string, string | null>, replace?: boolean) => void;
}) {
  const models = useModels();
  const providers = useModelProviders();
  const actions = useModelActions();
  const phone = usePhone();
  const size = sizeOf(params);

  // The URL is the filter. The palette's `/models?q=<ref>`, a reload and a back/forward
  // step all land on the same rows; the input is only a local draft while typing.
  const urlQuery = params.get("q") ?? "";
  const provider = params.get("provider") ?? ALL;
  const sharedOnly = params.get("shared") === "1";
  const [draft, setDraft] = useState(urlQuery);
  useEffect(() => setDraft(urlQuery), [urlQuery]);
  // One history entry per pause, not per keystroke: typing replaces, navigation pushes.
  useEffect(() => {
    if (draft === urlQuery) return;
    const timer = window.setTimeout(() => setFilters({ q: draft, page: "1" }, true), 200);
    return () => window.clearTimeout(timer);
  }, [draft, urlQuery, setFilters]);

  const [customOpen, setCustomOpen] = useState(false);
  const [editing, setEditing] = useState<Model | null>(null);
  const [deleting, setDeleting] = useState<Model | null>(null);

  // The catalog lists every provider this binary can route; only a connected one can have
  // models, so only those belong in a filter.
  const connected = useMemo(
    () => (providers.data ?? []).filter((p) => p.has_connection),
    [providers.data],
  );
  // A stale `?provider=azure` (disconnected since the link was copied) falls back to the
  // whole list instead of an empty table.
  const effectiveProvider =
    provider === ALL || connected.some((p) => p.id === provider) ? provider : ALL;

  const all = useMemo(
    () =>
      [...(models.data ?? [])].sort(
        (a, b) => a.provider.localeCompare(b.provider) || compareModels(a, b),
      ),
    [models.data],
  );
  const coverage = useMemo(() => {
    const map = new Map<string, Model[]>();
    for (const model of all) {
      const peers = map.get(model.id);
      if (peers) peers.push(model);
      else map.set(model.id, [model]);
    }
    return map;
  }, [all]);
  const uniqueIds = coverage.size;
  const sharedCount = useMemo(
    () => [...coverage.values()].filter((peers) => peers.length > 1).length,
    [coverage],
  );
  const providerDisplay = useMemo(
    () => new Map((providers.data ?? []).map((p) => [p.id, p.display] as const)),
    [providers.data],
  );

  const appliedQuery = useDeferredValue(urlQuery);
  const matchesRegistry = (m: Model) => {
    const q = appliedQuery.trim().toLowerCase();
    return (
      (effectiveProvider === ALL || m.provider === effectiveProvider) &&
      (!sharedOnly || (coverage.get(m.id)?.length ?? 0) > 1) &&
      (!q || m.ref.toLowerCase().includes(q) || (m.display ?? "").toLowerCase().includes(q))
    );
  };
  const filtered = useMemo(
    () => all.filter((m) => m.enabled && matchesRegistry(m)),
    [all, effectiveProvider, appliedQuery, sharedOnly, coverage],
  );
  const disabledModels = useMemo(
    () => all.filter((m) => !m.enabled && matchesRegistry(m)),
    [all, effectiveProvider, appliedQuery, sharedOnly, coverage],
  );

  const page = pageOf(params, "page", filtered.length, size);
  const pageRows = useMemo(
    () => filtered.slice((page - 1) * size, page * size),
    [filtered, page, size],
  );
  const disabledPage = pageOf(params, "dpage", disabledModels.length, size);
  const disabledSlice = disabledModels.slice((disabledPage - 1) * size, disabledPage * size);

  const pagination = {
    page,
    size,
    total: filtered.length,
    label: "models",
    onPage: (next: number) => setFilters({ page: String(next) }),
    onSize: (next: number) => setFilters({ size: String(next), page: "1" }),
  };
  const disabledPagination = {
    page: disabledPage,
    size,
    total: disabledModels.length,
    label: "disabled",
    onPage: (next: number) => setFilters({ dpage: String(next) }),
    onSize: (next: number) => setFilters({ size: String(next), dpage: "1" }),
  };

  const columns = useModelColumns({
    isAdmin,
    coverage,
    providerDisplay,
    testTarget: actions.testTarget,
    toggleTarget: actions.toggleTarget,
    removeTarget: actions.removeTarget,
    onTest: actions.onTest,
    onToggle: actions.onToggle,
    onEdit: setEditing,
    onDelete: setDeleting,
    onPrefer: actions.onPrefer,
    onPickProvider: (id) => setFilters({ provider: id, page: "1" }),
  });

  const filteredOut = appliedQuery.trim() !== "" || effectiveProvider !== ALL;
  const providerName =
    connected.find((p) => p.id === effectiveProvider)?.display ?? effectiveProvider;
  const clearFilters = () => {
    setDraft("");
    setFilters({ q: null, provider: null, shared: null, page: null });
  };

  const coverageBar = (
    <div className="flex flex-wrap items-center justify-between gap-2 border-b border-line px-5 py-2">
      <span className="text-caption text-faint tnum">
        {all.length.toLocaleString()} models · {uniqueIds.toLocaleString()} unique ids ·{" "}
        {sharedCount.toLocaleString()} shared by 2+ providers
      </span>
      <FilterChip
        on={sharedOnly}
        onClick={() => setFilters({ shared: sharedOnly ? null : "1", page: "1", dpage: "1" })}
        ariaLabel="Show only models served by more than one provider"
      >
        shared only{sharedOnly ? "" : sharedCount > 0 ? ` · ${sharedCount}` : ""}
      </FilterChip>
    </div>
  );

  const emptyState = sharedOnly ? (
    <div className="flex flex-col items-center gap-2">
      <p>
        No model is served by more than one provider
        {filteredOut ? " with these filters" : ""}.
      </p>
      <div className="flex flex-wrap items-center justify-center gap-2">
        <Button variant="ghost" onClick={() => setFilters({ shared: null, page: "1" })}>
          Show all models
        </Button>
        {filteredOut ? (
          <Button variant="quiet" onClick={clearFilters}>
            Clear filters
          </Button>
        ) : null}
      </div>
    </div>
  ) : filteredOut ? (
    <div className="flex flex-col items-center gap-2">
      <p>
        No models match{" "}
        {appliedQuery.trim() !== "" ? <span className="font-mono text-fg">{appliedQuery}</span> : null}
        {appliedQuery.trim() !== "" && effectiveProvider !== ALL ? " in " : null}
        {effectiveProvider !== ALL ? <span className="text-fg">{providerName}</span> : null}.
      </p>
      <Button variant="ghost" onClick={clearFilters}>
        Clear filters
      </Button>
    </div>
  ) : (
    "no models for this provider"
  );

  const disabledCard =
    disabledModels.length === 0 ? null : (
      <Card className="mt-4">
        <CardHeader
          title="Disabled models"
          action={
            <span className="text-caption text-faint tnum">
              {disabledModels.length.toLocaleString()} disabled · not routable until enabled
            </span>
          }
        />
        {phone ? (
          <>
            <ModelCards
              rows={disabledSlice}
              isAdmin={isAdmin}
              testTarget={actions.testTarget}
              toggleTarget={actions.toggleTarget}
              removeTarget={actions.removeTarget}
              onTest={actions.onTest}
              onToggle={actions.onToggle}
              onEdit={setEditing}
              onDelete={setDeleting}
              coverageFor={(id) => coverage.get(id) ?? []}
              displayOfProvider={(id) => providerDisplay.get(id) ?? id}
              onPickProvider={(id) => setFilters({ provider: id, page: "1" })}
              onPreferPreference={(model, provider) => actions.onPrefer(model.id, provider)}
            />
            <PaginationBar pagination={disabledPagination} />
          </>
        ) : (
          <DataTable
            columns={columns}
            rows={disabledSlice}
            rowKey={rowKey}
            tableLayout="fixed"
            stickyRight="actions"
            scrollLabel="Disabled models"
            empty="No disabled models match these filters."
            pagination={disabledPagination}
          />
        )}
      </Card>
    );

  return (
    <>
      <ConfirmDialog
        open={deleting !== null}
        title="Delete custom model"
        message={
          <>
            Remove <span className="font-mono">{deleting?.ref}</span> from this gateway's registry?
            Requests naming it fail until it is listed again.
          </>
        }
        confirmLabel="Delete model"
        busy={actions.remove.isPending}
        error={actions.remove.error}
        onConfirm={() => {
          if (deleting) {
            actions.remove.mutate(deleting, { onSuccess: () => setDeleting(null) });
          }
        }}
        onClose={() => {
          if (!actions.remove.isPending) {
            actions.remove.reset();
            setDeleting(null);
          }
        }}
      />

      {models.isError ? (
        <div className="mb-3">
          <ErrorNote error={models.error} onRetry={() => void models.refetch()} />
        </div>
      ) : null}
      {providers.isError ? (
        <div className="mb-3">
          <ErrorNote error={providers.error} onRetry={() => void providers.refetch()} />
        </div>
      ) : null}
      {actions.sync.isError || actions.toggle.isError || actions.remove.isError ? (
        <div className="mb-3">
          <ErrorNote error={actions.sync.error ?? actions.toggle.error ?? actions.remove.error} />
        </div>
      ) : null}

      {!models.isLoading && all.length === 0 ? (
        <EmptyRegistry onSync={actions.onSync} syncing={actions.sync.isPending} canAdmin={isAdmin} />
      ) : (
        <>
          <Card>
            <CardHeader
              title="Registry"
              action={
                <span className="text-caption text-faint tnum">
                  {filteredOut
                    ? `${filtered.length.toLocaleString()} of ${all.length.toLocaleString()} models`
                    : `${all.length.toLocaleString()} models`}
                </span>
              }
            />
            <div className="flex flex-wrap items-center gap-2 border-b border-line px-5 py-2">
              <div className="w-[190px]">
                <Select
                  ariaLabel="provider filter"
                  value={effectiveProvider}
                  onChange={(next) => setFilters({ provider: next, page: "1" })}
                >
                  <option value={ALL}>all providers</option>
                  {connected.map((p) => (
                    <option key={p.id} value={p.id}>
                      {p.display}
                    </option>
                  ))}
                </Select>
              </div>
              <div className="w-[200px]">
                <Input
                  value={draft}
                  onChange={setDraft}
                  placeholder="filter models…"
                  ariaLabel="filter models"
                />
              </div>
              {isAdmin ? (
                <div className="ml-auto">
                  <Button variant="primary" onClick={() => setCustomOpen(true)}>
                    add custom model
                  </Button>
                </div>
              ) : null}
            </div>
            {coverageBar}
            {phone && pageRows.length > 0 ? (
              <>
                <ModelCards
                  rows={pageRows}
                  isAdmin={isAdmin}
                  testTarget={actions.testTarget}
                  toggleTarget={actions.toggleTarget}
                  removeTarget={actions.removeTarget}
                  onTest={actions.onTest}
                  onToggle={actions.onToggle}
                  onEdit={setEditing}
                  onDelete={setDeleting}
                  coverageFor={(id) => coverage.get(id) ?? []}
                  displayOfProvider={(id) => providerDisplay.get(id) ?? id}
                  onPickProvider={(id) => setFilters({ provider: id, page: "1" })}
                  onPreferPreference={(model, provider) => actions.onPrefer(model.id, provider)}
                />
                <PaginationBar pagination={pagination} />
              </>
            ) : phone ? (
              <EmptyState>{emptyState}</EmptyState>
            ) : (
              <DataTable
                columns={columns}
                rows={pageRows}
                rowKey={rowKey}
                loading={models.isLoading}
                rowHeaderKey="model"
                stickyRight="actions"
                tableLayout="fixed"
                scrollLabel="Models registry"
                caption="Models registry: every model this gateway can route"
                empty={emptyState}
                pagination={pagination}
              />
            )}
          </Card>

          {disabledCard}
        </>
      )}

      {customOpen ? (
        <CustomModelModal
          providers={connected}
          defaultProvider={effectiveProvider !== ALL ? effectiveProvider : ""}
          onClose={() => setCustomOpen(false)}
          onSaved={actions.invalidateRegistry}
        />
      ) : null}

      {editing ? (
        <CustomModelModal
          providers={connected}
          defaultProvider={editing.provider}
          initial={editing}
          onClose={() => setEditing(null)}
          onSaved={actions.invalidateRegistry}
        />
      ) : null}
    </>
  );
}
