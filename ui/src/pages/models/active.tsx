import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { UseQueryResult } from "@tanstack/react-query";
import { Button, Card, CardHeader, ConfirmDialog, EmptyState, ErrorNote } from "../../components/ui";
import { PaginationBar } from "../../components/table";
import { PresetBar } from "../../components/model-presets";
import { InactiveList, SortableCards, SortableTable, type LayoutRow } from "../../components/sortable-list";
import {
  effectiveGroups,
  groupForEntry,
  layoutFor,
  type LayoutDraft,
} from "../../lib/layout-draft";
import { usePhone, useReducedMotion } from "../../lib/media";
import {
  useActivatePreset,
  useCreatePreset,
  useDeletePreset,
  useDuplicatePreset,
  useMakeDefaultPreset,
  useModelProviders,
  useModels,
  usePatchPreset,
} from "../../lib/queries";
import { ApiError, type Alias, type Combo, type Model, type ModelPreset, type PresetIndex } from "../../lib/api";
import { compareModels } from "../../lib/models";
import { sizeOf } from "./params";
import { useModelActions } from "./actions";
import { useModelColumns } from "./columns";
import { CustomModelModal } from "./custom-model";
import { GroupHeading, GroupManager, type GroupCandidate } from "./groups";
import { ModelCardBody } from "./rows";

/**
 * ACTIVE — the preset this caller routes through. The board is the only place a layout can
 * be edited, so the preset bar, its save state and the group manager live here.
 */
export function ActiveView({
  isAdmin,
  params,
  setFilters,
  presets,
  presetId,
  layout,
  layoutDraft,
  aliases,
  combos,
}: {
  isAdmin: boolean;
  params: URLSearchParams;
  setFilters: (patch: Record<string, string | null>, replace?: boolean) => void;
  presets: UseQueryResult<PresetIndex>;
  presetId: string | null;
  layout: UseQueryResult<ModelPreset>;
  layoutDraft: LayoutDraft;
  aliases: UseQueryResult<Alias[]>;
  combos: UseQueryResult<Combo[]>;
}) {
  const models = useModels();
  const providers = useModelProviders();
  const actions = useModelActions();
  const phone = usePhone();
  const reduceMotion = useReducedMotion();
  const size = sizeOf(params);

  const presetsReady = Boolean(presets.data && layout.data);
  const createPreset = useCreatePreset();
  const patchPreset = usePatchPreset();
  const duplicatePreset = useDuplicatePreset();
  const deletePreset = useDeletePreset();
  // One activation endpoint: it stores this account's choice, not a global default.
  const activatePreset = useActivatePreset();
  const makeDefaultPreset = useMakeDefaultPreset();
  // The default endpoint is pending in this build: probe once per session and stop
  // offering the control when the gateway says the route is not there.
  const [defaultBlocked, setDefaultBlocked] = useState(false);
  const [undo, setUndo] = useState<{ ref: string } | null>(null);

  const selectPreset = (id: string) => {
    if (id === presetId) return;
    setFilters({ preset: id, page: "1", ipage: null });
    activatePreset.mutate(id);
  };

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
  const providerDisplay = useMemo(
    () => new Map((providers.data ?? []).map((p) => [p.id, p.display] as const)),
    [providers.data],
  );

  // A preset row is a registry model plus its place in the layout; a ref the registry no
  // longer lists is still shown, as unavailable, so ordering never silently drops.
  const byRef = useMemo(() => new Map(all.map((model) => [model.ref, model] as const)), [all]);
  const entryByRef = useMemo(
    () => new Map(layoutDraft.entries.map((entry) => [entry.ref, entry] as const)),
    [layoutDraft.entries],
  );
  const aliasList = useMemo(() => aliases.data ?? [], [aliases.data]);
  const comboList = useMemo(() => combos.data ?? [], [combos.data]);
  const groupChoices = useMemo(() => {
    const knownEntries = layoutDraft.entries.filter((entry) => byRef.has(entry.ref));
    const knownProviders = new Set(Array.from(byRef.values(), (model) => model.provider));
    if (aliasList.length > 0 || comboList.length > 0) {
      knownProviders.add("rrouter");
    }
    return effectiveGroups(
      knownEntries,
      layoutDraft.groups.filter((group) => !group.provider || knownProviders.has(group.provider)),
      (provider) => providerDisplay.get(provider) ?? provider,
    );
  }, [layoutDraft.entries, layoutDraft.groups, providerDisplay, byRef, aliasList, comboList]);
  const groupIndex = useMemo(
    () => new Map(groupChoices.map((group, index) => [group.id, index] as const)),
    [groupChoices],
  );
  const groupOf = useCallback(
    (ref: string) => groupForEntry(entryByRef.get(ref), groupChoices),
    [entryByRef, groupChoices],
  );
  const [groupsOpen, setGroupsOpen] = useState(false);
  const openGroups = () => {
    layoutDraft.ensureProviderGroups(
      groupChoices
        .filter((group) => group.provider)
        .map((group) => ({ provider: group.provider as string, name: group.name })),
    );
    setGroupsOpen(true);
  };
  const groupCandidates = useMemo<GroupCandidate[]>(
    () => [
      ...all.map((model) => ({
        ref: model.ref,
        label: model.id,
        provider: model.provider,
        kind: "model" as const,
      })),
      ...aliasList.map((alias) => ({
        ref: `rrouter/${alias.alias}`,
        label: `${alias.alias} → ${alias.target}`,
        provider: "rrouter",
        kind: "alias" as const,
      })),
      ...comboList.map((combo) => ({
        ref: `rrouter/${combo.name}`,
        label: `${combo.name} · ${combo.models.length} models`,
        provider: "rrouter",
        kind: "combo" as const,
      })),
    ],
    [all, aliasList, comboList],
  );
  const entryRefs = useMemo(
    () => new Set(layoutDraft.entries.map((entry) => entry.ref)),
    [layoutDraft.entries],
  );
  // Only an enabled model belongs in the active list; a disabled one is not part of the
  // board's order. Rows hidden from the preset stay hidden-only even when they are
  // disabled too, so no model appears twice.
  const visibleRows = layoutDraft.entries
    .filter((entry) => entry.visible)
    .map<LayoutRow>((entry) => ({
      ref: entry.ref,
      model: byRef.get(entry.ref) ?? null,
      visible: true,
      isNew: false,
    }))
    .filter((row) => row.model !== null);
  const rowOrder = new Map(visibleRows.map((row, index) => [row.ref, index] as const));
  const activeRows = visibleRows
    .filter((row) => row.model?.enabled !== false)
    .sort((a, b) => {
      const ai = groupIndex.get(groupOf(a.ref)) ?? Number.MAX_SAFE_INTEGER;
      const bi = groupIndex.get(groupOf(b.ref)) ?? Number.MAX_SAFE_INTEGER;
      return ai - bi || (rowOrder.get(a.ref) ?? 0) - (rowOrder.get(b.ref) ?? 0);
    });
  const disabledRows = visibleRows.filter((row) => row.model?.enabled === false);
  const newRows = all
    .filter((model) => !entryRefs.has(model.ref))
    .map<LayoutRow>((model) => ({ ref: model.ref, model, visible: false, isNew: true }));
  const hiddenRows = layoutDraft.entries
    .filter((entry) => !entry.visible)
    .map<LayoutRow>((entry) => ({
      ref: entry.ref,
      model: byRef.get(entry.ref) ?? null,
      visible: false,
      isNew: false,
    }))
    .filter((row) => row.model !== null);
  // Entries whose provider was removed stay in the document (a reconnect restores
  // them) but are not part of the visible board or its counts.
  const hiddenCount = hiddenRows.length;
  const activePage = Math.min(
    Math.max(1, Number(params.get("page") ?? "1") || 1),
    Math.max(1, Math.ceil(activeRows.length / size)),
  );
  const inactivePage = Math.min(
    Math.max(1, Number(params.get("ipage") ?? "1") || 1),
    Math.max(1, Math.ceil((hiddenRows.length + newRows.length) / size)),
  );
  const activeSlice = activeRows.slice((activePage - 1) * size, activePage * size);
  const sliceGroups = useMemo(() => {
    const byGroup = new Map<string, LayoutRow[]>();
    for (const row of activeSlice) {
      const id = groupOf(row.ref);
      const list = byGroup.get(id);
      if (list) list.push(row);
      else byGroup.set(id, [row]);
    }
    return groupChoices
      .filter((group) => byGroup.has(group.id))
      .map((group) => ({ ...group, rows: byGroup.get(group.id) ?? [] }));
  }, [activeSlice, groupChoices, groupOf]);
  const activeCounts = useMemo(() => {
    const counts = new Map<string, number>();
    for (const row of activeRows) {
      const id = groupOf(row.ref);
      counts.set(id, (counts.get(id) ?? 0) + 1);
    }
    return counts;
  }, [activeRows, groupOf]);
  const inactiveSlice = [...hiddenRows, ...newRows].slice(
    (inactivePage - 1) * size,
    inactivePage * size,
  );

  // Auto-include is a policy, not a button: a preset that opts in adopts every registry
  // model it has never stored, and the debounced save persists them.
  const newRefKey = newRows.map((row) => row.ref).join("|");
  useEffect(() => {
    if (!presetsReady || !layoutDraft.autoInclude || newRefKey === "") return;
    layoutDraft.adopt(newRefKey.split("|"));
  }, [presetsReady, layoutDraft.autoInclude, newRefKey]);

  // Undo is a local affordance: hiding is a layout change, so there is nothing to undo on
  // the server once the debounced save has run, and the toast would lie after a reload.
  useEffect(() => {
    if (!undo) return;
    const timer = window.setTimeout(() => setUndo(null), 8000);
    return () => window.clearTimeout(timer);
  }, [undo]);
  const hideRow = useRef<(ref: string) => void>(() => {});
  hideRow.current = (ref) => {
    layoutDraft.hide(ref);
    setUndo({ ref });
  };

  const [deleting, setDeleting] = useState<Model | null>(null);
  const [editing, setEditing] = useState<Model | null>(null);
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
    hide: hideRow.current,
    groups: isAdmin && presetId ? groupChoices : undefined,
    groupOf,
    onMoveGroup: (ref, group) => layoutDraft.assignGroup(ref, group),
  });

  const activeEmpty =
    disabledRows.length > 0 ? (
      <div className="flex flex-col items-center gap-2">
        <p>Every model in this preset is disabled.</p>
        <p className="text-caption text-faint">
          The Registry view lists them under Disabled models; enable one there to route through
          it again.
        </p>
      </div>
    ) : layoutDraft.entries.length === 0 ? (
      <p>This preset has no models yet. Reorder or hide rows once the registry has some.</p>
    ) : (
      "no model is visible in this preset"
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
      {actions.toggle.isError || actions.remove.isError || actions.sync.isError ? (
        <div className="mb-3">
          <ErrorNote error={actions.sync.error ?? actions.toggle.error ?? actions.remove.error} />
        </div>
      ) : null}
      {makeDefaultPreset.isError && !defaultBlocked ? (
        <div className="mb-3">
          <ErrorNote error={makeDefaultPreset.error} />
        </div>
      ) : null}

      {presets.isError ? (
        <Card className="mb-4">
          {/* The library is shipped with this binary; an error here is a real failure, not a
              missing feature, so name it and keep the registry reachable. */}
          <div className="flex flex-col gap-2 px-5 py-3">
            <p className="text-caption text-faint">
              Presets could not be loaded. The Registry view is the plain list of every model.
            </p>
            <ErrorNote error={presets.error} onRetry={() => void presets.refetch()} />
          </div>
        </Card>
      ) : presetsReady && presetId ? (
        <>
          <PresetBar
            presets={presets.data?.data ?? []}
            activeId={presetId}
            defaultId={presets.data?.default_id ?? ""}
            draftState={layoutDraft.state}
            savedAt={layoutDraft.savedAt}
            conflict={Boolean(layoutDraft.conflict)}
            error={layoutDraft.error}
            autoInclude={layoutDraft.autoInclude}
            counts={`${activeRows.length.toLocaleString()} active · ${disabledRows.length.toLocaleString()} disabled · ${hiddenCount.toLocaleString()} hidden${
              newRows.length > 0 && !layoutDraft.autoInclude
                ? ` · ${newRows.length.toLocaleString()} new`
                : ""
            }`}
            updatedAt={layout.data?.updated_at ?? 0}
            busy={
              createPreset.isPending ||
              patchPreset.isPending ||
              duplicatePreset.isPending ||
              deletePreset.isPending ||
              activatePreset.isPending ||
              makeDefaultPreset.isPending
            }
            onSelect={selectPreset}
            onSave={() => void layoutDraft.saveNow()}
            onDiscard={layoutDraft.discard}
            onSaveAndSwitch={async (id) => {
              if (await layoutDraft.saveNow()) selectPreset(id);
            }}
            onSaveCopy={(name) =>
              createPreset.mutate(
                { name, layout: layoutFor(layoutDraft.entries, layoutDraft.groups, layoutDraft.autoInclude) },
                { onSuccess: (created) => selectPreset(created.id) },
              )
            }
            onAutoInclude={layoutDraft.setAutoInclude}
            onMakeDefault={(id) =>
              makeDefaultPreset.mutate(id, {
                onError: (failure) => {
                  if (
                    failure instanceof ApiError &&
                    (failure.status === 404 || failure.status === 405)
                  ) {
                    setDefaultBlocked(true);
                  }
                },
              })
            }
            defaultBlocked={defaultBlocked}
            onLoadLatest={layoutDraft.loadLatest}
            manage={{
              create: (name) =>
                createPreset.mutate(
                  { name, layout: layoutFor(layoutDraft.entries, layoutDraft.groups, layoutDraft.autoInclude) },
                  { onSuccess: (created) => selectPreset(created.id) },
                ),
              rename: (id, name) => patchPreset.mutate({ id, patch: { name } }),
              duplicate: (id, name) => duplicatePreset.mutate({ id, name }),
              remove: (id) =>
                deletePreset.mutate(id, {
                  onSuccess: () => {
                    if (id === presetId) {
                      setFilters({ preset: presets.data?.default_id ?? null, page: "1" });
                    }
                  },
                }),
            }}
          />

          {undo ? (
            <div
              role="status"
              className="mb-3 flex flex-wrap items-center justify-between gap-2 rounded-[8px] border border-line bg-surface px-4 py-2 text-small text-fg"
            >
              <span>
                <span className="font-mono">{undo.ref}</span> hidden from this preset and
                /v1/models. It still routes when named explicitly.
              </span>
              <span className="flex gap-1.5">
                <Button
                  variant="ghost"
                  onClick={() => {
                    layoutDraft.restore(undo.ref);
                    setUndo(null);
                  }}
                >
                  Undo
                </Button>
                <Button variant="quiet" onClick={() => setUndo(null)}>
                  dismiss
                </Button>
              </span>
            </div>
          ) : null}

          <Card>
            <CardHeader
              title="Active models"
              action={
                <span className="flex flex-wrap items-center justify-end gap-2">
                  <span className="text-caption text-faint">
                    drag a row, or focus its handle and press space
                  </span>
                  {isAdmin && presetsReady ? (
                    <Button variant="ghost" onClick={openGroups}>
                      manage groups
                    </Button>
                  ) : null}
                </span>
              }
            />
            {sliceGroups.length === 0 ? (
              <EmptyState>{activeEmpty}</EmptyState>
            ) : (
              sliceGroups.map((group) => (
                <section key={group.id} aria-label={group.name}>
                  <GroupHeading
                    group={group}
                    count={activeCounts.get(group.id) ?? group.rows.length}
                  />
                  {phone ? (
                    <SortableCards
                      rows={group.rows}
                      onReorder={layoutDraft.reorder}
                      reducedMotion={reduceMotion}
                      canHide={!layoutDraft.conflict}
                      onHide={(ref) => hideRow.current(ref)}
                      empty={activeEmpty}
                      render={(model) => (
                        <ModelCardBody
                          model={model}
                          isAdmin={isAdmin}
                          testTarget={actions.testTarget}
                          toggleTarget={actions.toggleTarget}
                          removeTarget={actions.removeTarget}
                          onTest={actions.onTest}
                          onToggle={actions.onToggle}
                          onEdit={setEditing}
                          onDelete={setDeleting}
                          onHide={(ref) => hideRow.current(ref)}
                          groups={isAdmin && presetId ? groupChoices : undefined}
                          currentGroup={groupOf(model.ref)}
                          onMoveGroup={(next) => layoutDraft.assignGroup(model.ref, next)}
                          coverageFor={(id) => coverage.get(id) ?? []}
                          displayOfProvider={(id) => providerDisplay.get(id) ?? id}
                          onPickProvider={(id) => setFilters({ provider: id, page: "1" })}
                          onPreferPreference={(model, provider) => actions.onPrefer(model.id, provider)}
                        />
                      )}
                    />
                  ) : (
                    <SortableTable
                      rows={group.rows}
                      columns={columns}
                      onReorder={layoutDraft.reorder}
                      onHide={(ref) => hideRow.current(ref)}
                      reducedMotion={reduceMotion}
                      canHide={!layoutDraft.conflict}
                      scrollLabel={`${group.name} models`}
                      empty={activeEmpty}
                    />
                  )}
                </section>
              ))
            )}
            <PaginationBar
              pagination={{
                page: activePage,
                size,
                total: activeRows.length,
                label: "models",
                onPage: (next) => setFilters({ page: String(next) }),
                onSize: (next) => setFilters({ size: String(next), page: "1" }),
              }}
            />
          </Card>

          {hiddenCount + newRows.length > 0 ? (
            <Card className="mt-4">
              <CardHeader
                title="Hidden from this preset"
                action={
                  <span className="text-caption text-faint tnum">
                    {hiddenCount.toLocaleString()} hidden
                    {newRows.length > 0 && !layoutDraft.autoInclude
                      ? ` · ${newRows.length.toLocaleString()} not in this preset`
                      : ""}
                  </span>
                }
              />
              <InactiveList
                rows={inactiveSlice}
                onRestore={(ref) => layoutDraft.restore(ref)}
                onRestoreAll={() =>
                  layoutDraft.restoreAll(
                    layoutDraft.autoInclude ? [] : newRows.map((row) => row.ref),
                  )
                }
                note="Hide a model to keep it out of this preset's order. It stays routable."
              />
              {hiddenRows.length + newRows.length > 0 ? (
                <div className="border-t border-line">
                  <PaginationBar
                    pagination={{
                      page: inactivePage,
                      size,
                      total: hiddenRows.length + newRows.length,
                      label: "hidden",
                      onPage: (next) => setFilters({ ipage: String(next) }),
                      onSize: (next) => setFilters({ size: String(next), ipage: "1" }),
                    }}
                  />
                </div>
              ) : null}
            </Card>
          ) : null}
        </>
      ) : presets.isLoading || layout.isLoading ? null : (
        <Card className="px-6 py-10 text-center">
          <p className="text-body text-muted">
            No preset is available yet. The Registry view lists every model this gateway can
            route; the default preset is created with the instance.
          </p>
        </Card>
      )}

      {editing ? (
        <CustomModelModal
          providers={providers.data}
          defaultProvider={editing.provider}
          initial={editing}
          onClose={() => setEditing(null)}
          onSaved={actions.invalidateRegistry}
        />
      ) : null}

      <GroupManager
        open={groupsOpen}
        groups={groupChoices}
        onClose={() => setGroupsOpen(false)}
        onCreate={(name) => {
          layoutDraft.createGroup(name);
        }}
        onRename={(id, name) => layoutDraft.renameGroup(id, name)}
        onReorder={(from, to) => layoutDraft.reorderGroup(from, to)}
        onDelete={(id) => layoutDraft.deleteGroup(id)}
        entries={layoutDraft.entries}
        candidates={groupCandidates}
        onAssign={(ref, group) => layoutDraft.assignGroup(ref, group)}
        onSetVisible={(id, visible) => layoutDraft.setGroupVisible(id, visible)}
        isDefault={presetId === presets.data?.default_id}
      />
    </>
  );
}
