import { useCallback, useDeferredValue, useEffect, useMemo, useRef, useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useSearchParams } from "react-router-dom";
import {
  AlertBanner,
  Button,
  Card,
  ConfirmDialog,
  EmptyState,
  ErrorNote,
  PageHeader,
} from "../components/ui";
import { DataTable, PaginationBar } from "../components/table";
import { ConnectionCard } from "../components/connection-card";
import { effectiveGroups, groupForEntry, layoutFor, useLayoutDraft } from "../lib/layout-draft";
import { usePhone, useReducedMotion } from "../lib/media";
import { compareModels } from "../lib/models";
import {
  qk,
  useActivatePreset,
  useAliases,
  useCombos,
  useCreatePreset,
  useIsAdmin,
  useModelProviders,
  useModels,
  usePresetIndex,
  usePresetLayout,
} from "../lib/queries";
import { api, type Combo, type Model } from "../lib/api";
import { useModelActions } from "./models/actions";
import { useUnifiedColumns } from "./models/columns-unified";
import { CustomModelModal } from "./models/custom-model";
import { KINDS, ModelFilters, type KindKey } from "./models/filters";
import { GroupManager, type GroupCandidate } from "./models/groups";
import {
  ALL,
  DEFAULT_SIZE,
  STATUSES,
  STATUS_LABELS,
  mapLegacyView,
  pageOf,
  sizeOf,
  statusParam,
  statusesOf,
  type StatusKey,
} from "./models/params";
import {
  AddMenu,
  ComboCreateDialog,
  PresetControls,
  SaveAsNewDialog,
  type ComboCreatePayload,
} from "./models/preset-header";
import { EmptyRegistry } from "./models/rows";
import {
  BoardSection,
  UnifiedCards,
  UnifiedRowProvider,
  type LayoutControls,
  type ModelActions,
} from "./models/table-unified";
import { matchesSearch, rowsFor, type UnifiedRow } from "./models/unified-rows";

/**
 * Models: one page, one table.
 *
 * The table is the union of registry models, live aliases, live combos, and preset entries
 * nothing live answers (stale rows). `Status`, `Kind`, `Custom`, `Provider` and `q` narrow
 * that one row set; none of them swaps the surface. The preset header edits the draft that
 * group and `/v1` controls write to, and it is shared by every filter, so a save that is
 * still debouncing survives a filter change.
 */
const KIND_OF_ROW: Record<Exclude<KindKey, "all">, UnifiedRow["kind"]> = {
  models: "model",
  aliases: "alias",
  combos: "combo",
};

/** Filter ownership, not route membership: a combo containing a model is RRouter's row. */
function providerOfRow(row: UnifiedRow): string {
  if (row.kind !== "model") return "rrouter";
  if (row.model) return row.model.provider;
  const cut = row.ref.indexOf("/");
  return cut < 0 ? row.ref : row.ref.slice(0, cut);
}

export default function Models() {
  const isAdmin = useIsAdmin();
  const [params, setParams] = useSearchParams();
  const client = useQueryClient();
  const phone = usePhone();
  const reduceMotion = useReducedMotion();

  // ---- URL -------------------------------------------------------------------------
  // A pre-unification bookmark (`?view=`, `?ipage=`, `?dpage=`) is rewritten once into the
  // filter vocabulary instead of keeping two view states that must be reconciled.
  useEffect(() => {
    const next = new URLSearchParams(params);
    if (mapLegacyView(next)) setParams(next, { replace: true });
  }, [params, setParams]);

  const setFilters = useCallback(
    (patch: Record<string, string | null>, replace = false) => {
      const next = new URLSearchParams(params);
      for (const [key, value] of Object.entries(patch)) {
        const defaulted =
          value === null ||
          value === "" ||
          (key === "provider" && value === ALL) ||
          (key === "status" && value === "routed") ||
          (key === "kind" && value === "all") ||
          (key === "size" && Number(value) === DEFAULT_SIZE) ||
          (key === "page" && Number(value) === 1) ||
          (key === "custom" && value === "0");
        if (defaulted) next.delete(key);
        else next.set(key, value);
      }
      setParams(next, { replace });
    },
    [params, setParams],
  );

  const urlQuery = params.get("q") ?? "";
  const [draft, setDraft] = useState(urlQuery);
  useEffect(() => setDraft(urlQuery), [urlQuery]);
  // One history entry per pause, not per keystroke: typing replaces, navigation pushes.
  useEffect(() => {
    if (draft === urlQuery) return;
    const timer = window.setTimeout(() => setFilters({ q: draft, page: "1" }, true), 200);
    return () => window.clearTimeout(timer);
  }, [draft, urlQuery, setFilters]);
  const appliedQuery = useDeferredValue(urlQuery).trim().toLowerCase();

  const requestedProvider = params.get("provider") ?? ALL;
  const rawKind = params.get("kind");
  const requestedKind: KindKey = KINDS.includes(rawKind as KindKey)
    ? (rawKind as KindKey)
    : "all";
  const customOnly = params.get("custom") === "1";
  const requestedStatus = useMemo(() => statusesOf(params), [params]);
  const size = sizeOf(params);

  // ---- data ------------------------------------------------------------------------
  const models = useModels();
  const providers = useModelProviders();
  const presets = usePresetIndex(isAdmin);
  const presetId = isAdmin
    ? (params.get("preset") ||
      presets.data?.active_id ||
      presets.data?.default_id ||
      presets.data?.data[0]?.id ||
      null)
    : null;
  const layout = usePresetLayout(presetId, isAdmin);
  const invalidatePresetList = useCallback(
    // Only the list: refreshing the open document would reset the saved-at indicator.
    () => void client.invalidateQueries({ queryKey: qk.modelPresets, exact: true }),
    [client],
  );
  const layoutDraft = useLayoutDraft(layout.data, invalidatePresetList);
  const aliases = useAliases(isAdmin);
  const combos = useCombos(isAdmin);
  const actions = useModelActions();
  const activatePreset = useActivatePreset();
  const createPreset = useCreatePreset();

  const selectPreset = useCallback(
    (id: string) => {
      setFilters({ preset: id, page: "1" });
      activatePreset.mutate(id);
    },
    [setFilters, activatePreset],
  );

  const [copyOpen, setCopyOpen] = useState(false);
  const [customOpen, setCustomOpen] = useState(false);
  const [comboOpen, setComboOpen] = useState(false);
  const [editing, setEditing] = useState<Model | null>(null);
  const [deleting, setDeleting] = useState<Model | null>(null);
  const [groupsOpen, setGroupsOpen] = useState(false);
  const [undo, setUndo] = useState<{ ref: string } | null>(null);

  // ---- derived rows ----------------------------------------------------------------
  const all = useMemo(
    () =>
      [...(models.data ?? [])].sort(
        (a, b) => a.provider.localeCompare(b.provider) || compareModels(a, b),
      ),
    [models.data],
  );
  const providerDisplay = useMemo(
    () => new Map((providers.data ?? []).map((p) => [p.id, p.display] as const)),
    [providers.data],
  );
  const displayOf = useCallback(
    (id: string) => providerDisplay.get(id) ?? id,
    [providerDisplay],
  );
  const connected = useMemo(
    () => (providers.data ?? []).filter((p) => p.has_connection),
    [providers.data],
  );

  const hasPreset = isAdmin && presetId !== null && layout.data !== undefined;
  const presetAware = hasPreset;
  const editable = hasPreset && layoutDraft.state !== "conflict";
  // The draft seeds from the document in an effect, one render after the query resolves.
  // Nothing may read or write it before that render, or it would overwrite the stored order.
  const draftReady =
    !hasPreset ||
    layoutDraft.entries.length > 0 ||
    (layout.data?.layout.items.length ?? 0) === 0;

  const entryByRef = useMemo(
    () => new Map(layoutDraft.entries.map((entry) => [entry.ref, entry] as const)),
    [layoutDraft.entries],
  );
  const aliasList = useMemo(() => aliases.data ?? [], [aliases.data]);
  const comboList = useMemo(() => combos.data ?? [], [combos.data]);
  const hasNames =
    aliasList.length > 0 ||
    comboList.length > 0 ||
    layoutDraft.entries.some((entry) => entry.ref.startsWith("rrouter/"));
  // A stale `?provider=` (disconnected since the link was copied) falls back to the list.
  // RRouter is a filter option only while an alias, combo or stored name ref exists.
  const effectiveProvider =
    requestedProvider === ALL ||
    providers.data?.some((provider) => provider.id === requestedProvider) ||
    (requestedProvider === "rrouter" && hasNames)
      ? requestedProvider
      : ALL;
  const providerOptions = useMemo(
    () => [
      ...connected.map((provider) => ({ id: provider.id, display: provider.display })),
      ...(hasNames ? [{ id: "rrouter", display: "RRouter" }] : []),
    ],
    [connected, hasNames],
  );
  const modelByRef = useMemo(
    () => new Map(all.map((model) => [model.ref, model] as const)),
    [all],
  );

  // One union: registry rows, live names at `rrouter/<name>`, then preset entries that no
  // live row claimed. Everything downstream (filters, groups, `/v1`, hide, reorder) keys on
  // the row ref, so a name needs no second code path.
  const allRows = useMemo<UnifiedRow[]>(
    () =>
      rowsFor({
        models: all,
        aliases: aliases.data,
        combos: combos.data,
        entries: entryByRef,
        presetAware,
        autoInclude: layoutDraft.autoInclude,
      }),
    [
      all,
      aliases.data,
      combos.data,
      entryByRef,
      presetAware,
      layoutDraft.autoInclude,
    ],
  );

  const groupChoices = useMemo(() => {
    const knownProviders = new Set(all.map((model) => model.provider));
    if (hasNames) knownProviders.add("rrouter");
    // Every stored entry contributes its bucket: an entry whose provider stopped listing
    // the model still needs a section to render in, badged `unavailable`.
    return effectiveGroups(
      layoutDraft.entries,
      layoutDraft.groups.filter((group) => !group.provider || knownProviders.has(group.provider)),
      displayOf,
    );
  }, [layoutDraft.entries, layoutDraft.groups, all, hasNames, displayOf]);
  const groupIndex = useMemo(
    () => new Map(groupChoices.map((group, index) => [group.id, index] as const)),
    [groupChoices],
  );
  const groupOf = useCallback(
    (ref: string) => groupForEntry(entryByRef.get(ref), groupChoices),
    [entryByRef, groupChoices],
  );

  const filteredRows = useMemo(() => {
    return allRows.filter((row) => {
      if (requestedKind !== "all" && row.kind !== KIND_OF_ROW[requestedKind]) return false;
      // A combo containing an Anthropic model is still an RRouter row: provider filters on
      // ownership, while search is the place a member can reveal its parent.
      if (effectiveProvider !== ALL && providerOfRow(row) !== effectiveProvider) return false;
      if (customOnly && !(row.kind === "model" && row.model?.source === "custom")) {
        return false;
      }
      return matchesSearch(row, appliedQuery, displayOf);
    });
  }, [
    allRows,
    requestedKind,
    effectiveProvider,
    customOnly,
    appliedQuery,
    displayOf,
  ]);

  const statusAware = isAdmin && hasPreset;
  const statusRows = useMemo(
    () =>
      statusAware
        ? filteredRows.filter((row) => requestedStatus.has(row.status as StatusKey))
        : filteredRows,
    [filteredRows, statusAware, requestedStatus],
  );

  const counts = useMemo(() => {
    const result: Record<StatusKey, number> = { routed: 0, hidden: 0, disabled: 0, unrouted: 0 };
    for (const row of filteredRows) {
      const status = row.status;
      if (status !== "catalog") result[status] += 1;
    }
    return result;
  }, [filteredRows]);

  const routedTotal = allRows.filter((row) => row.status === "routed").length;

  const outside =
    statusAware && appliedQuery !== ""
      ? filteredRows.filter(
          (row) => !requestedStatus.has(row.status as StatusKey),
        ).length
      : 0;

  const boardMode =
    statusAware && requestedStatus.size === 1 && requestedStatus.has("routed");
  const dragEnabled =
    boardMode &&
    editable &&
    !customOnly &&
    requestedKind === "all" &&
    appliedQuery === "" &&
    effectiveProvider === ALL;

  const entryPosition = useMemo(
    () => new Map(layoutDraft.entries.map((entry, index) => [entry.ref, index] as const)),
    [layoutDraft.entries],
  );

  const boardRows = useMemo(() => {
    if (!boardMode) return [];
    const rank = (ref: string) => groupIndex.get(groupOf(ref)) ?? Number.MAX_SAFE_INTEGER;
    return [...statusRows].sort(
      (a, b) =>
        rank(a.ref) - rank(b.ref) ||
        (entryPosition.get(a.ref) ?? Number.MAX_SAFE_INTEGER) -
          (entryPosition.get(b.ref) ?? Number.MAX_SAFE_INTEGER),
    );
  }, [boardMode, statusRows, groupIndex, groupOf, entryPosition]);

  const flatRows = useMemo(() => {
    if (boardMode) return [];
    return [...statusRows].sort((a, b) => {
      const ap = entryPosition.get(a.ref);
      const bp = entryPosition.get(b.ref);
      if (ap !== undefined && bp !== undefined) return ap - bp;
      if (ap !== undefined) return -1;
      if (bp !== undefined) return 1;
      const am = a.kind === "model" ? a.model : null;
      const bm = b.kind === "model" ? b.model : null;
      if (am && bm) {
        return (
          displayOf(am.provider).localeCompare(displayOf(bm.provider)) ||
          compareModels(am, bm)
        );
      }
      if (am) return -1;
      if (bm) return 1;
      return (
        providerOfRow(a).localeCompare(providerOfRow(b)) || a.ref.localeCompare(b.ref)
      );
    });
  }, [boardMode, statusRows, entryPosition, displayOf]);

  const total = boardMode ? boardRows.length : flatRows.length;
  const page = pageOf(params, "page", total, size);
  const pageSlice = useMemo(
    () => (boardMode ? boardRows : flatRows).slice((page - 1) * size, page * size),
    [boardMode, boardRows, flatRows, page, size],
  );
  const boardGroups = useMemo(() => {
    const byGroup = new Map<string, UnifiedRow[]>();
    for (const row of pageSlice) {
      const id = groupOf(row.ref);
      const list = byGroup.get(id);
      if (list) list.push(row);
      else byGroup.set(id, [row]);
    }
    return groupChoices
      .filter((group) => byGroup.has(group.id))
      .map((group) => ({ group, rows: byGroup.get(group.id) ?? [] }));
  }, [pageSlice, groupChoices, groupOf]);
  const groupCounts = useMemo(() => {
    const result = new Map<string, number>();
    for (const row of boardRows) {
      const id = groupOf(row.ref);
      result.set(id, (result.get(id) ?? 0) + 1);
    }
    return result;
  }, [boardRows, groupOf]);

  // ---- draft behaviour -------------------------------------------------------------
  const newRefKey = useMemo(
    () =>
      allRows
        .filter((row) => !entryByRef.has(row.ref))
        .map((row) => row.ref)
        .join("|"),
    [allRows, entryByRef],
  );
  // Auto-include is a policy, not a button: an opted-in preset adopts every live row it
  // never stored — registry, alias or combo — and the debounced save persists them.
  useEffect(() => {
    if (!draftReady || !isAdmin || !hasPreset || !layoutDraft.autoInclude || newRefKey === "") {
      return;
    }
    layoutDraft.adopt(newRefKey.split("|"));
    // `adopt` is stable in spirit; rerunning it for the same refs is a no-op.
  }, [draftReady, isAdmin, hasPreset, layoutDraft.autoInclude, newRefKey]);

  // Undo is local: hiding is a layout change, so after the debounced save there is nothing
  // on the server to undo, and a toast surviving a reload would lie.
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

  // Add → Combo creates the combo and writes its group in the same draft the row select
  // writes, so the new row lands in that section without a second step. On a conflicted
  // draft the group control is disabled and only the combo is created.
  const createCombo = useMutation({
    mutationFn: (payload: ComboCreatePayload) =>
      api.post<Combo>("/api/combos", {
        name: payload.name,
        models: payload.models,
        strategy: payload.strategy,
        ...(payload.strategy === "smart" ? { router_model: payload.router_model } : {}),
      }),
    onSuccess: (combo, payload) => {
      void client.invalidateQueries({ queryKey: qk.combos });
      void client.invalidateQueries({ queryKey: qk.models });
      void client.invalidateQueries({ queryKey: qk.system });
      if (editable) {
        layoutDraft.assignGroup(`rrouter/${combo.name}`, payload.group);
      }
      setComboOpen(false);
    },
  });

  const groupDisabledReason = !hasPreset
    ? "No preset is loaded to assign a group."
    : layoutDraft.state === "conflict"
      ? "Resolve the layout conflict below before assigning a group."
      : null;

  // Built from the same row set the board renders, so a name that is both an alias and a
  // combo appears once (alias wins, as in rowsFor) and no candidate is unassignable.
  const groupCandidates = useMemo<GroupCandidate[]>(
    () =>
      allRows.flatMap((row): GroupCandidate[] => {
        if (row.kind === "model") {
          if (!row.model) return [];
          return [
            {
              ref: row.ref,
              label: row.model.id,
              provider: row.model.provider,
              kind: "model" as const,
            },
          ];
        }
        if (row.kind === "alias") {
          return [
            {
              ref: row.ref,
              label: `${row.name} → ${row.alias.target}`,
              provider: "rrouter",
              kind: "alias" as const,
            },
          ];
        }
        const count = row.combo.models.length;
        return [
          {
            ref: row.ref,
            label: `${row.name} · ${count} candidate${count === 1 ? "" : "s"}`,
            provider: "rrouter",
            kind: "combo" as const,
          },
        ];
      }),
    [allRows],
  );

  const openGroups = () => {
    layoutDraft.ensureProviderGroups(
      groupChoices
        .filter((group) => group.provider)
        .map((group) => ({ provider: group.provider as string, name: group.name })),
    );
    setGroupsOpen(true);
  };

  // ---- columns and row controls ----------------------------------------------------
  const columns = useUnifiedColumns({
    model: {
      isAdmin,
      testTarget: actions.testTarget,
      toggleTarget: actions.toggleTarget,
      removeTarget: actions.removeTarget,
      onTest: actions.onTest,
      onToggle: actions.onToggle,
      onEdit: setEditing,
      onDelete: setDeleting,
      hide: editable ? hideRow.current : undefined,
      hideFor: (ref) => entryByRef.has(ref),
      groups: isAdmin && hasPreset ? groupChoices : undefined,
      groupOf,
      onMoveGroup: (ref, group) => layoutDraft.assignGroup(ref, group),
    },
    isAdmin,
    editable,
    presetScoped: hasPreset,
    groups: groupChoices,
    groupOf,
    onAssignGroup: (ref, group) => layoutDraft.assignGroup(ref, group),
    onVisible: (ref, visible) => layoutDraft.setVisible(ref, visible),
    onHide: (ref) => hideRow.current(ref),
  });

  const rowActions: ModelActions = {
    isAdmin,
    testTarget: actions.testTarget,
    toggleTarget: actions.toggleTarget,
    removeTarget: actions.removeTarget,
    onTest: actions.onTest,
    onToggle: actions.onToggle,
    onEdit: setEditing,
    onDelete: setDeleting,
    onHide: (ref) => hideRow.current(ref),
  };

  const controls: LayoutControls | undefined = hasPreset
    ? {
        editable,
        groups: groupChoices,
        groupOf,
        onAssignGroup: (ref, group) => layoutDraft.assignGroup(ref, group),
        onVisible: (ref, visible) => layoutDraft.setVisible(ref, visible),
      }
    : undefined;

  // ---- states ----------------------------------------------------------------------
  const loading =
    !draftReady ||
    models.isLoading ||
    (isAdmin &&
      (presets.isLoading ||
        aliases.isLoading ||
        combos.isLoading ||
        (presetId !== null && layout.isLoading)));
  const presetName =
    presets.data?.data.find((preset) => preset.id === presetId)?.name ?? "this preset";
  const statusCaption = STATUSES.filter((status) => requestedStatus.has(status))
    .map((status) => STATUS_LABELS[status])
    .join(", ");
  const caption = statusAware
    ? `${statusCaption} rows in ${presetName}`
    : "Every model this gateway can serve";
  const subtitle = hasPreset
    ? `Every model this gateway can serve. ${routedTotal.toLocaleString()} routed · ${allRows.length.toLocaleString()} rows`
    : "Every model this gateway can serve.";

  const rowsNothing = total === 0;
  const hasRowFilters =
    appliedQuery !== "" ||
    effectiveProvider !== ALL ||
    requestedKind !== "all" ||
    customOnly ||
    (statusAware && statusParam(requestedStatus) !== null);
  const clearFilters = () => {
    setDraft("");
    setFilters({
      q: null,
      provider: null,
      status: null,
      kind: null,
      custom: null,
      page: null,
    });
  };
  const showStatus = (status: StatusKey) =>
    setFilters({ status: statusParam(new Set<StatusKey>([status])), page: "1" });
  const canClear =
    urlQuery !== "" ||
    params.has("provider") ||
    params.has("status") ||
    params.has("kind") ||
    params.has("custom");

  const emptyState =
    appliedQuery !== "" || effectiveProvider !== ALL || customOnly ? (
      <div className="flex flex-col items-center gap-2">
        <p>No rows match these filters.</p>
        <Button variant="ghost" onClick={clearFilters}>
          Clear filters
        </Button>
      </div>
    ) : statusAware && requestedStatus.size === 1 && requestedStatus.has("routed") ? (
      layoutDraft.entries.length === 0 ? (
        <div className="flex flex-col items-center gap-2">
          <p>This preset has no models.</p>
          <Button variant="ghost" onClick={() => showStatus("unrouted")}>
            Show not-in-preset models
          </Button>
        </div>
      ) : counts.disabled > 0 ? (
        <div className="flex flex-col items-center gap-2">
          <p>Every model in this preset is disabled.</p>
          <Button variant="ghost" onClick={() => showStatus("disabled")}>
            Show disabled models
          </Button>
        </div>
      ) : (
        <div className="flex flex-col items-center gap-2">
          <p>No model is visible in this preset.</p>
          <Button variant="ghost" onClick={() => showStatus("hidden")}>
            Show hidden models
          </Button>
        </div>
      )
    ) : statusAware && requestedStatus.size === 1 && requestedStatus.has("hidden") ? (
      <p>Nothing is hidden from this preset. Rows you hide appear here.</p>
    ) : statusAware && requestedStatus.size === 1 && requestedStatus.has("disabled") ? (
      <p>No model is disabled. The gateway refuses a disabled id on every endpoint.</p>
    ) : statusAware && requestedStatus.size === 1 && requestedStatus.has("unrouted") ? (
      <p>Every model this gateway knows is already in this preset.</p>
    ) : rowsNothing && hasRowFilters ? (
      <div className="flex flex-col items-center gap-2">
        <p>No rows match these filters.</p>
        <Button variant="ghost" onClick={clearFilters}>
          Clear filters
        </Button>
      </div>
    ) : (
      "no rows here yet"
    );

  const pagination = {
    page,
    size,
    total,
    label: "rows",
    onPage: (next: number) => setFilters({ page: String(next) }),
    onSize: (next: number) => setFilters({ size: String(next), page: "1" }),
  };

  return (
    <>
      <PageHeader
        title="Models"
        subtitle={subtitle}
        actions={
          isAdmin ? (
            <>
              {presets.data && presetId && !layout.isError ? (
                <PresetControls
                  presets={presets.data.data}
                  presetId={presetId}
                  defaultId={presets.data.default_id ?? ""}
                  draft={layoutDraft}
                  busy={activatePreset.isPending || createPreset.isPending}
                  onSelect={selectPreset}
                  onSaveAsNew={() => setCopyOpen(true)}
                />
              ) : null}
              <AddMenu
                onAddCustom={() => setCustomOpen(true)}
                onAddCombo={() => {
                  createCombo.reset();
                  setComboOpen(true);
                }}
              />
            </>
          ) : undefined
        }
      />

      <ConnectionCard />

      {presets.isError ? (
        <AlertBanner
          title="Presets could not be loaded"
          actions={
            <Button variant="ghost" onClick={() => void presets.refetch()}>
              retry
            </Button>
          }
        >
          The table still lists every model; preset order, groups and `/v1` visibility are
          unavailable until the library answers.
        </AlertBanner>
      ) : null}
      {layout.isError && presetId ? (
        <div className="mb-4">
          <ErrorNote error={layout.error} onRetry={() => void layout.refetch()} />
        </div>
      ) : null}
      {models.isError ? (
        <div className="mb-4">
          <ErrorNote error={models.error} onRetry={() => void models.refetch()} />
        </div>
      ) : null}
      {providers.isError ? (
        <div className="mb-4">
          <ErrorNote error={providers.error} onRetry={() => void providers.refetch()} />
        </div>
      ) : null}
      {actions.sync.isError || actions.toggle.isError || actions.remove.isError ? (
        <div className="mb-4">
          <ErrorNote error={actions.sync.error ?? actions.toggle.error ?? actions.remove.error} />
        </div>
      ) : null}
      {layoutDraft.error ? (
        <AlertBanner
          title="Could not save this layout"
          actions={
            <Button variant="ghost" onClick={layoutDraft.discard}>
              Discard changes
            </Button>
          }
        >
          {layoutDraft.error}. The previous order was restored; the rows below show what the
          gateway last accepted.
        </AlertBanner>
      ) : null}
      {layoutDraft.conflict ? (
        <div
          role="alert"
          className="mb-4 rounded-[var(--radius-card)] border border-[var(--color-warn-line)] bg-[var(--color-warn-tint)] px-5 py-3 text-small text-fg"
        >
          <span className="font-medium">Another tab saved a newer revision.</span>{" "}
          <span className="text-muted">
            Your draft is kept here. Load the stored layout, or save your draft as a new preset.
          </span>
          <span className="mt-2 flex flex-wrap gap-1.5">
            <Button variant="ghost" onClick={layoutDraft.loadLatest}>
              Load latest
            </Button>
            <Button variant="primary" onClick={() => setCopyOpen(true)}>
              Save as new
            </Button>
          </span>
        </div>
      ) : null}
      {undo ? (
        <div
          role="status"
          className="mb-4 flex flex-wrap items-center justify-between gap-2 rounded-[8px] border border-line bg-surface px-4 py-2 text-small text-fg"
        >
          <span>
            <span className="font-mono">{undo.ref}</span> removed from this preset and
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

      {!models.isLoading && allRows.length === 0 ? (
        <EmptyRegistry
          onSync={actions.onSync}
          syncing={actions.sync.isPending}
          canAdmin={isAdmin}
          onAddCustom={isAdmin ? () => setCustomOpen(true) : undefined}
        />
      ) : (
        <UnifiedRowProvider modelByRef={modelByRef}>
          <Card>
          <ModelFilters
            query={draft}
            onQuery={setDraft}
            providers={providerOptions}
            provider={effectiveProvider}
            onProvider={(next) => setFilters({ provider: next, page: "1" })}
            kind={requestedKind}
            onKind={(next) => setFilters({ kind: next, page: "1" })}
            statuses={requestedStatus}
            counts={counts}
            showStatus={statusAware}
            onStatus={(next) => setFilters({ status: statusParam(next), page: "1" })}
            custom={customOnly}
            onCustom={() => setFilters({ custom: customOnly ? null : "1", page: "1" })}
            extra={
              isAdmin ? (
                <span className="flex items-center gap-2">
                  {hasPreset ? (
                    <Button variant="quiet" onClick={openGroups}>
                      groups
                    </Button>
                  ) : null}
                </span>
              ) : undefined
            }
            outside={outside}
            onShowOutside={() =>
              setFilters({ status: STATUSES.join(","), page: "1" })
            }
            canClear={canClear}
            onClear={clearFilters}
          />

          {loading ? (
            <div
              aria-busy="true"
              className="min-h-[240px] px-5 py-12 text-center text-body text-faint"
            >
              loading models…
            </div>
          ) : boardMode ? (
            boardGroups.length === 0 ? (
              <EmptyState>{emptyState}</EmptyState>
            ) : (
              boardGroups.map(({ group, rows }, index) => (
                <BoardSection
                  key={group.id}
                  group={group}
                  count={groupCounts.get(group.id) ?? rows.length}
                  rows={rows}
                  columns={columns}
                  actions={rowActions}
                  controls={controls}
                  phone={phone}
                  drag={!phone && dragEnabled}
                  reducedMotion={reduceMotion}
                  onReorder={layoutDraft.reorder}
                  onHide={(ref) => hideRow.current(ref)}
                  showHeader={index === 0}
                  onMove={(ref, delta) => {
                    const index = rows.findIndex((row) => row.ref === ref);
                    const target = rows[index + delta];
                    if (target) layoutDraft.reorder(ref, target.ref);
                  }}
                />
              ))
            )
          ) : phone ? (
            pageSlice.length === 0 ? (
              <EmptyState>{emptyState}</EmptyState>
            ) : (
              <UnifiedCards
                rows={pageSlice}
                actions={rowActions}
                controls={controls}
                onHide={(ref) => hideRow.current(ref)}
              />
            )
          ) : (
            <DataTable
              columns={columns}
              rows={loading ? undefined : pageSlice}
              rowKey={(row) => row.ref}
              loading={loading}
              stickyRight="actions"
              rowHeaderKey="model"
              tableLayout="fixed"
              scrollLabel="Models"
              caption={caption}
              empty={emptyState}
              pagination={pagination}
            />
          )}

          {boardMode && !loading && boardRows.length > 0 ? (
            <PaginationBar pagination={pagination} />
          ) : null}
          </Card>
        </UnifiedRowProvider>
      )}

      {copyOpen ? (
        <SaveAsNewDialog
          onClose={() => {
            createPreset.reset();
            setCopyOpen(false);
          }}
          onSubmit={(name) =>
            createPreset.mutate(
              {
                name,
                layout: layoutFor(
                  layoutDraft.entries,
                  layoutDraft.groups,
                  layoutDraft.autoInclude,
                ),
              },
              {
                onSuccess: (created) => {
                  setCopyOpen(false);
                  selectPreset(created.id);
                },
              },
            )
          }
          busy={createPreset.isPending}
          error={createPreset.error}
        />
      ) : null}

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

      {comboOpen ? (
        <ComboCreateDialog
          models={all}
          groups={groupChoices
            .filter((group) => !group.provider)
            .map((group) => ({ id: group.id, name: group.name }))}
          editable={editable}
          groupDisabledReason={groupDisabledReason}
          busy={createCombo.isPending}
          error={createCombo.error}
          onClose={() => {
            if (!createCombo.isPending) {
              createCombo.reset();
              setComboOpen(false);
            }
          }}
          onSubmit={(payload) => createCombo.mutate(payload)}
        />
      ) : null}

      <ConfirmDialog
        open={deleting !== null}
        title="Delete custom model"
        message={
          <>
            Remove <span className="font-mono">{deleting?.ref}</span> from this gateway's
            registry? Requests naming it fail until it is listed again.
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
