import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { Link } from "react-router-dom";
import {
  AlertBanner,
  Button,
  Card,
  ConfirmDialog,
  EmptyState,
  ErrorNote,
  PageHeader,
} from "../components/ui";
import { PaginationBar } from "../components/table";
import { useFeatureFlag } from "../lib/flags";
import { effectiveGroups, groupForEntry, layoutFor, useLayoutDraft } from "../lib/layout-draft";
import { usePhone, useReducedMotion } from "../lib/media";
import { compareModels } from "../lib/models";
import { useUrlFilterState } from "../lib/url-state";
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
import { api, type Alias, type Combo, type Model } from "../lib/api";
import { useModelActions } from "./models/actions";
import { type ModelColumnsOptions } from "./models/columns";
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
  AliasCreateDialog,
  ComboCreateDialog,
  PresetControls,
  PresetManagerDrawer,
  SaveAsNewDialog,
  type AliasCreatePayload,
  type ComboCreatePayload,
} from "./models/preset-header";
import { EmptyRegistry } from "./models/rows";
import {
  BoardSection,
  FlatTable,
  UnifiedCards,
  UnifiedRowProvider,
  type LayoutControls,
  type ModelActions,
} from "./models/table-unified";
import {
  emptyStateKind,
  matchesSearch,
  rowIsFree,
  rowsFor,
  summarizeRows,
  type UnifiedRow,
} from "./models/unified-rows";

/**
 * Models: one page, one table.
 *
 * The table is the union of registry models, live aliases, live combos, and preset entries
 * nothing live answers (stale rows). `Status`, `Kind`, `Free`, `Custom`, `Provider` and `q`
 * narrow that one row set; none of them swaps the surface. The preset header edits the draft
 * that group and `/v1` controls write to, and it is shared by every filter, so a save that
 * is still debouncing survives a filter change.
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
  const presetDrawerEnabled = useFeatureFlag("presetManagerDrawer");
  const client = useQueryClient();
  const phone = usePhone();
  const reduceMotion = useReducedMotion();

  // ---- URL -------------------------------------------------------------------------
  const {
    params,
    setParams,
    values: urlFilters,
    searchDraft: draft,
    setSearchDraft: setDraft,
    appliedSearch,
    setFilters,
    resetFilters,
  } = useUrlFilterState({
    defaults: {
      q: "",
      provider: ALL,
      kind: "all",
      status: "routed",
      custom: "0",
      free: "0",
      size: String(DEFAULT_SIZE),
      page: "1",
      preset: "",
    },
    searchKey: "q",
    debounceMs: 200,
    pageKey: "page",
    isDefault: (key, value) => {
      if (key === "size") return Number(value) === DEFAULT_SIZE;
      if (key === "page") return Number(value) === 1;
      return false;
    },
  });

  // A pre-unification bookmark (`?view=`, `?ipage=`, `?dpage=`) is rewritten once into the
  // filter vocabulary instead of keeping two view states that must be reconciled.
  useEffect(() => {
    const next = new URLSearchParams(params);
    if (mapLegacyView(next)) setParams(next, { replace: true });
  }, [params, setParams]);

  const urlQuery = urlFilters.q;
  const appliedQuery = appliedSearch.trim().toLowerCase();

  const requestedProvider = urlFilters.provider;
  const rawKind = urlFilters.kind;
  const requestedKind: KindKey = KINDS.includes(rawKind as KindKey)
    ? (rawKind as KindKey)
    : "all";
  const customOnly = urlFilters.custom === "1";
  const freeOnly = urlFilters.free === "1";
  const requestedStatus = useMemo(() => statusesOf(params), [params]);
  const size = sizeOf(params);

  // ---- data ------------------------------------------------------------------------
  const models = useModels();
  const providers = useModelProviders();
  const presets = usePresetIndex(isAdmin);
  const requestedPresetId = urlFilters.preset || null;
  const validRequestedPreset =
    requestedPresetId &&
    (!presets.data || presets.data.data.some((preset) => preset.id === requestedPresetId))
      ? requestedPresetId
      : null;
  const presetId = isAdmin
    ? (validRequestedPreset ||
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
  const draftRef = useRef(layoutDraft);
  draftRef.current = layoutDraft;

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
  const [manageOpen, setManageOpen] = useState(false);
  const [customOpen, setCustomOpen] = useState(false);
  const [aliasOpen, setAliasOpen] = useState(false);
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

  const summary = useMemo(
    () => summarizeRows(allRows, modelByRef),
    [allRows, modelByRef],
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

  const baseFilteredRows = useMemo(() => {
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

  const freeCount = useMemo(() => {
    let count = 0;
    for (const row of baseFilteredRows) {
      if (statusAware && !requestedStatus.has(row.status as StatusKey)) continue;
      if (rowIsFree(row, modelByRef)) count += 1;
    }
    return count;
  }, [baseFilteredRows, statusAware, requestedStatus, modelByRef]);

  const filteredRows = useMemo(
    () =>
      freeOnly
        ? baseFilteredRows.filter((row) => rowIsFree(row, modelByRef))
        : baseFilteredRows,
    [baseFilteredRows, freeOnly, modelByRef],
  );

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

  const outside = useMemo(
    () =>
      statusAware && (appliedQuery !== "" || freeOnly)
        ? filteredRows.filter(
            (row) => !requestedStatus.has(row.status as StatusKey),
          ).length
        : 0,
    [statusAware, appliedQuery, freeOnly, filteredRows, requestedStatus],
  );

  const boardMode =
    statusAware && requestedStatus.size === 1 && requestedStatus.has("routed");
  const dragEnabled =
    boardMode &&
    editable &&
    !customOnly &&
    !freeOnly &&
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
    draftRef.current.adopt(newRefKey.split("|"));
  }, [draftReady, isAdmin, hasPreset, layoutDraft.autoInclude, newRefKey]);

  // Undo is local: hiding is a layout change, so after the debounced save there is nothing
  // on the server to undo, and a toast surviving a reload would lie.
  useEffect(() => {
    if (!undo) return;
    const timer = window.setTimeout(() => setUndo(null), 8000);
    return () => window.clearTimeout(timer);
  }, [undo]);

  // Stabilized draft callbacks so Shell's 5s health poll, 15s background polls, and search
  // keystrokes never invalidate column definitions or memoized table rows.
  const assignGroup = useCallback(
    (ref: string, group: string | null) => draftRef.current.assignGroup(ref, group),
    [],
  );
  const setVisible = useCallback(
    (ref: string, visible: boolean) => draftRef.current.setVisible(ref, visible),
    [],
  );
  const reorder = useCallback(
    (from: string, to: string) => draftRef.current.reorder(from, to),
    [],
  );
  const hideRow = useCallback((ref: string) => {
    draftRef.current.hide(ref);
    setUndo({ ref });
  }, []);
  const hideFor = useCallback((ref: string) => entryByRef.has(ref), [entryByRef]);

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
        draftRef.current.assignGroup(`rrouter/${combo.name}`, payload.group);
      }
      setComboOpen(false);
    },
  });

  // Add → Alias mirrors Combo: creates the alias and assigns it to the chosen group.
  const createAlias = useMutation({
    mutationFn: (payload: AliasCreatePayload) =>
      api.post<Alias>("/api/aliases", {
        alias: payload.alias,
        target: payload.target,
      }),
    onSuccess: (created, payload) => {
      void client.invalidateQueries({ queryKey: qk.aliases });
      void client.invalidateQueries({ queryKey: qk.models });
      void client.invalidateQueries({ queryKey: qk.system });
      if (editable) {
        draftRef.current.assignGroup(`rrouter/${created.alias}`, payload.group);
      }
      setAliasOpen(false);
    },
  });

  const groupDisabledReason = !hasPreset
    ? "No preset is loaded to assign a group."
    : layoutDraft.state === "conflict"
      ? "Resolve the layout conflict below before assigning a group."
      : null;

  const customGroupOptions = useMemo(
    () =>
      groupChoices
        .filter((group) => !group.provider)
        .map((group) => ({ id: group.id, name: group.name })),
    [groupChoices],
  );

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

  const openGroups = useCallback(() => {
    draftRef.current.ensureProviderGroups(
      groupChoices
        .filter((group) => group.provider)
        .map((group) => ({ provider: group.provider as string, name: group.name })),
    );
    setGroupsOpen(true);
  }, [groupChoices]);

  const groupRowsById = useMemo(() => {
    const map = new Map<string, UnifiedRow[]>();
    if (!boardMode) return map;
    for (const row of boardRows) {
      const id = groupOf(row.ref);
      const bucket = map.get(id);
      if (bucket) bucket.push(row);
      else map.set(id, [row]);
    }
    return map;
  }, [boardMode, boardRows, groupOf]);

  const canMoveTop = useCallback(
    (ref: string) => {
      if (!dragEnabled) return false;
      const list = groupRowsById.get(groupOf(ref));
      return Boolean(list && list.length > 1 && list[0]?.ref !== ref);
    },
    [dragEnabled, groupRowsById, groupOf],
  );

  const canMoveBottom = useCallback(
    (ref: string) => {
      if (!dragEnabled) return false;
      const list = groupRowsById.get(groupOf(ref));
      return Boolean(list && list.length > 1 && list[list.length - 1]?.ref !== ref);
    },
    [dragEnabled, groupRowsById, groupOf],
  );

  const onMoveTop = useCallback(
    (ref: string) => {
      if (!dragEnabled) return;
      const list = groupRowsById.get(groupOf(ref));
      const first = list?.[0];
      if (first && first.ref !== ref) draftRef.current.reorder(ref, first.ref);
    },
    [dragEnabled, groupRowsById, groupOf],
  );

  const onMoveBottom = useCallback(
    (ref: string) => {
      if (!dragEnabled) return;
      const list = groupRowsById.get(groupOf(ref));
      const last = list?.[list.length - 1];
      if (last && last.ref !== ref) draftRef.current.reorder(ref, last.ref);
    },
    [dragEnabled, groupRowsById, groupOf],
  );

  const onMoveDelta = useCallback(
    (ref: string, delta: number) => {
      const list = groupRowsById.get(groupOf(ref));
      if (!list) return;
      const index = list.findIndex((row) => row.ref === ref);
      const target = list[index + delta];
      if (target) draftRef.current.reorder(ref, target.ref);
    },
    [groupRowsById, groupOf],
  );

  // ---- columns and row controls ----------------------------------------------------
  const modelColumnOptions = useMemo<ModelColumnsOptions>(
    () => ({
      isAdmin,
      testTarget: actions.testTarget,
      testResults: actions.testResults,
      toggleTarget: actions.toggleTarget,
      removeTarget: actions.removeTarget,
      onTest: actions.onTest,
      onToggle: actions.onToggle,
      onEdit: setEditing,
      onDelete: setDeleting,
      hide: editable ? hideRow : undefined,
      hideFor,
      onMoveTop: dragEnabled ? onMoveTop : undefined,
      onMoveBottom: dragEnabled ? onMoveBottom : undefined,
      canMoveTop: dragEnabled ? canMoveTop : undefined,
      canMoveBottom: dragEnabled ? canMoveBottom : undefined,
      groups: isAdmin && hasPreset ? groupChoices : undefined,
      groupOf,
      onMoveGroup: assignGroup,
    }),
    [
      isAdmin,
      actions.testTarget,
      actions.testResults,
      actions.toggleTarget,
      actions.removeTarget,
      actions.onTest,
      actions.onToggle,
      editable,
      hideRow,
      hideFor,
      dragEnabled,
      onMoveTop,
      onMoveBottom,
      canMoveTop,
      canMoveBottom,
      hasPreset,
      groupChoices,
      groupOf,
      assignGroup,
    ],
  );

  const columns = useUnifiedColumns({
    model: modelColumnOptions,
    isAdmin,
    editable,
    presetScoped: hasPreset,
    groups: groupChoices,
    groupOf,
    onAssignGroup: assignGroup,
    onVisible: setVisible,
    onHide: hideRow,
  });

  const rowActions = useMemo<ModelActions>(
    () => ({
      isAdmin,
      testTarget: actions.testTarget,
      testResults: actions.testResults,
      toggleTarget: actions.toggleTarget,
      removeTarget: actions.removeTarget,
      onTest: actions.onTest,
      onToggle: actions.onToggle,
      onEdit: setEditing,
      onDelete: setDeleting,
      onHide: hideRow,
      onMoveTop: dragEnabled ? onMoveTop : undefined,
      onMoveBottom: dragEnabled ? onMoveBottom : undefined,
      canMoveTop: dragEnabled ? canMoveTop : undefined,
      canMoveBottom: dragEnabled ? canMoveBottom : undefined,
    }),
    [
      isAdmin,
      actions.testTarget,
      actions.testResults,
      actions.toggleTarget,
      actions.removeTarget,
      actions.onTest,
      actions.onToggle,
      hideRow,
      dragEnabled,
      onMoveTop,
      onMoveBottom,
      canMoveTop,
      canMoveBottom,
    ],
  );

  const controls = useMemo<LayoutControls | undefined>(
    () =>
      hasPreset
        ? {
            editable,
            groups: groupChoices,
            groupOf,
            onAssignGroup: assignGroup,
            onVisible: setVisible,
          }
        : undefined,
    [hasPreset, editable, groupChoices, groupOf, assignGroup, setVisible],
  );

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

  const subtitle = useMemo(() => {
    if (summary.total === 0) return "Models, aliases and combos this gateway can serve.";
    const freePart = summary.free > 0 ? ` · ${summary.free.toLocaleString()} free` : "";
    if (hasPreset) {
      return `${summary.routed.toLocaleString()} routed (${summary.routedAvailable.toLocaleString()} available)${freePart} · ${summary.total.toLocaleString()} total`;
    }
    return `${summary.available.toLocaleString()} of ${summary.total.toLocaleString()} available${freePart}`;
  }, [hasPreset, summary]);

  const clearFilters = useCallback(() => {
    resetFilters(["q", "provider", "status", "kind", "custom", "free", "page"]);
  }, [resetFilters]);

  const showStatus = useCallback(
    (status: StatusKey) =>
      setFilters({ status: statusParam(new Set<StatusKey>([status])), page: "1" }),
    [setFilters],
  );

  const canClear =
    urlQuery !== "" ||
    params.has("provider") ||
    params.has("status") ||
    params.has("kind") ||
    params.has("custom") ||
    params.has("free");

  const emptyKind = emptyStateKind({
    hasError: models.isError,
    query: appliedQuery,
    providerIsAll: effectiveProvider === ALL,
    kindIsAll: requestedKind === "all",
    customOnly,
    freeOnly,
    statusAware,
    statuses: requestedStatus,
    draftEntryCount: layoutDraft.entries.length,
    counts,
  });

  const emptyState = (() => {
    switch (emptyKind) {
      case "error":
        return (
          <div className="flex flex-col items-center gap-2">
            <p>Could not load the model registry.</p>
            <Button variant="ghost" onClick={() => void models.refetch()}>
              Retry
            </Button>
          </div>
        );
      case "filtered_free":
        return (
          <div className="flex flex-col items-center gap-2">
            <p>No free ($0) models match these filters.</p>
            <div className="flex flex-wrap items-center justify-center gap-2">
              <Button variant="ghost" onClick={clearFilters}>
                Clear filters
              </Button>
              {isAdmin ? (
                <Link
                  to="/free-mode"
                  className="inline-flex h-8 items-center rounded-full bg-accent px-3.5 text-small font-semibold text-[var(--color-accent-ink)] transition-colors hover:bg-fg"
                >
                  Set up Free mode →
                </Link>
              ) : null}
            </div>
          </div>
        );
      case "filtered":
        return (
          <div className="flex flex-col items-center gap-2">
            <p>No rows match these filters.</p>
            <Button variant="ghost" onClick={clearFilters}>
              Clear filters
            </Button>
          </div>
        );
      case "preset_empty":
        return (
          <div className="flex flex-col items-center gap-2">
            <p>This preset has no routed models yet.</p>
            <Button variant="ghost" onClick={() => showStatus("unrouted")}>
              Show not-in-preset models
            </Button>
          </div>
        );
      case "all_disabled":
        return (
          <div className="flex flex-col items-center gap-2">
            <p>Every model in this preset is disabled.</p>
            <Button variant="ghost" onClick={() => showStatus("disabled")}>
              Show disabled models
            </Button>
          </div>
        );
      case "all_hidden":
        return (
          <div className="flex flex-col items-center gap-2">
            <p>No model is visible in this preset.</p>
            <Button variant="ghost" onClick={() => showStatus("hidden")}>
              Show hidden models
            </Button>
          </div>
        );
      case "none_hidden":
        return <p>Nothing is hidden from this preset. Rows you hide appear here.</p>;
      case "none_disabled":
        return <p>No model is disabled. The gateway refuses a disabled id on every endpoint.</p>;
      case "none_unrouted":
        return <p>Every model this gateway knows is already in this preset.</p>;
      default:
        return <p>No rows here yet.</p>;
    }
  })();

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
                  onManagePresets={presetDrawerEnabled ? () => setManageOpen(true) : undefined}
                />
              ) : null}
              <AddMenu
                onAddCustom={() => setCustomOpen(true)}
                onAddAlias={() => {
                  createAlias.reset();
                  setAliasOpen(true);
                }}
                onAddCombo={() => {
                  createCombo.reset();
                  setComboOpen(true);
                }}
              />
            </>
          ) : undefined
        }
      />

      {presets.isError ? (
        <AlertBanner
          title="Presets could not be loaded"
          actions={
            <Button variant="ghost" onClick={() => void presets.refetch()}>
              Retry
            </Button>
          }
        >
          The table still lists every model; preset order, groups and /v1 visibility are
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

      {!models.isLoading && !models.isError && allRows.length === 0 ? (
        <EmptyRegistry
          onSync={actions.onSync}
          syncing={actions.sync.isPending}
          canAdmin={isAdmin}
          onAddCustom={isAdmin ? () => setCustomOpen(true) : undefined}
        />
      ) : (
        <UnifiedRowProvider modelByRef={modelByRef} isAdmin={isAdmin}>
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
              free={freeOnly}
              onFree={() => setFilters({ free: freeOnly ? null : "1", page: "1" })}
              freeCount={freeCount}
              showFreeModeLink={isAdmin}
              custom={customOnly}
              onCustom={() => setFilters({ custom: customOnly ? null : "1", page: "1" })}
              extra={
                isAdmin && hasPreset ? (
                  <Button variant="quiet" onClick={openGroups}>
                    groups
                  </Button>
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
                    onReorder={reorder}
                    onHide={hideRow}
                    showHeader={index === 0}
                    deferLayout={index > 0}
                    onMove={onMoveDelta}
                  />
                ))
              )
            ) : pageSlice.length === 0 ? (
              <EmptyState>{emptyState}</EmptyState>
            ) : phone ? (
              <UnifiedCards
                rows={pageSlice}
                actions={rowActions}
                controls={controls}
                onHide={hideRow}
              />
            ) : (
              <FlatTable caption={caption} rows={pageSlice} columns={columns} />
            )}

            {!loading && total > 0 ? <PaginationBar pagination={pagination} /> : null}
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

      {aliasOpen ? (
        <AliasCreateDialog
          groups={customGroupOptions}
          editable={editable}
          groupDisabledReason={groupDisabledReason}
          busy={createAlias.isPending}
          error={createAlias.error}
          onClose={() => {
            if (!createAlias.isPending) {
              createAlias.reset();
              setAliasOpen(false);
            }
          }}
          onSubmit={(payload) => createAlias.mutate(payload)}
        />
      ) : null}

      {comboOpen ? (
        <ComboCreateDialog
          models={all}
          groups={customGroupOptions}
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

      {isAdmin && presets.data && presetDrawerEnabled ? (
        <PresetManagerDrawer
          open={manageOpen}
          onClose={() => setManageOpen(false)}
          presets={presets.data.data}
          activePresetId={presetId}
          defaultId={presets.data.default_id ?? ""}
          draft={hasPreset ? layoutDraft : undefined}
          busy={activatePreset.isPending || createPreset.isPending}
          onSelectPreset={(id) => {
            selectPreset(id);
            setManageOpen(false);
          }}
          onDeletedPreset={(id) => {
            if (urlFilters.preset === id) {
              setFilters({ preset: null, page: "1" }, true);
            }
          }}
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
        onAssign={assignGroup}
        onSetVisible={(id, visible) => layoutDraft.setGroupVisible(id, visible)}
        isDefault={presetId === presets.data?.default_id}
      />
    </>
  );
}
