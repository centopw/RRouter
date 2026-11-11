import {
  useCallback,
  useDeferredValue,
  useEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
} from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useSearchParams } from "react-router-dom";
import * as Popover from "@radix-ui/react-popover";
import { CircleNotch, DotsThree, Play, Star } from "@phosphor-icons/react";
import {
  Badge,
  Button,
  Card,
  CardHeader,
  ErrorNote,
  Field,
  Input,
  Modal,
  ConfirmDialog,
  EmptyState,
  PageHeader,
  Select,
  cx,
} from "../components/ui";
import { DataTable, PaginationBar, type Column } from "../components/table";
import { PresetBar } from "../components/model-presets";
import { InactiveList, SortableCards, SortableTable, type LayoutRow } from "../components/sortable-list";
import {
  effectiveGroups,
  groupForEntry,
  layoutFor,
  useLayoutDraft,
  type DraftGroup,
} from "../lib/layout-draft";
import { CAPABILITY_KEYS, CAPABILITY_LABELS, CapabilityChips } from "../components/capability-chips";
import { ProviderIcon } from "../components/provider-icon";
import { Checkbox, Heading, IconButton, Toggle } from "../components/atoms";
import { FilterChip } from "../components/atoms/FilterChip";
import { compact } from "../lib/format";
import { usePhone, useReducedMotion } from "../lib/media";
import { compareModels } from "../lib/models";
import { useToast } from "../components/toast";
import {
  qk,
  useCreatePreset,
  useDeletePreset,
  useDuplicatePreset,
  useIsAdmin,
  useModelProviders,
  useModels,
  usePatchPreset,
  usePresetIndex,
  usePresetLayout,
  useActivatePreset,
  useMakeDefaultPreset,
} from "../lib/queries";
import {
  ApiError,
  api,
  type Capabilities,
  type Model,
  type ModelProvider,
  type ModelTest,
  type SyncResult,
} from "../lib/api";

const ALL = "all";
/** Page sizes the registry offers; 50 is the default because it fills a laptop viewport. */
const SIZES = [25, 50, 100, 200] as const;
const DEFAULT_SIZE = 50;

export default function Models() {
  const models = useModels();
  const providers = useModelProviders();
  const isAdmin = useIsAdmin();
  const phone = usePhone();
  const reduceMotion = useReducedMotion();
  const client = useQueryClient();
  const [params, setParams] = useSearchParams();

  // The URL is the filter. The palette's `/models?q=<ref>`, a reload and a back/forward
  // step all land on the same rows; the input is only a local draft while typing.
  const urlQuery = params.get("q") ?? "";
  const provider = params.get("provider") ?? ALL;
  const requestedSize = Number(params.get("size") ?? DEFAULT_SIZE);
  const size: number = (SIZES as readonly number[]).includes(requestedSize)
    ? requestedSize
    : DEFAULT_SIZE;

  const [draft, setDraft] = useState(urlQuery);
  useEffect(() => setDraft(urlQuery), [urlQuery]);

  /** Write filters to the URL, dropping defaults so a clean view has a clean link. */
  const setFilters = useCallback(
    (patch: Record<string, string | null>, replace = false) => {
      const next = new URLSearchParams(params);
      for (const [key, value] of Object.entries(patch)) {
        const defaulted =
          value === null ||
          value === "" ||
          (key === "provider" && value === ALL) ||
          (key === "size" && Number(value) === DEFAULT_SIZE) ||
          (key === "page" && Number(value) === 1) ||
          (key === "ipage" && Number(value) === 1);
        if (defaulted) next.delete(key);
        else next.set(key, value);
      }
      setParams(next, { replace });
    },
    [params, setParams],
  );

  // ---- preset layout ---------------------------------------------------------------
  // `?preset=` is the addressable layout; without it the server's active preset applies.
  const presets = usePresetIndex(isAdmin);
  const presetId =
    params.get("preset") ??
    presets.data?.active_id ??
    presets.data?.default_id ??
    presets.data?.data[0]?.id ??
    null;
  const layout = usePresetLayout(presetId);
  const layoutDraft = useLayoutDraft(layout.data, () =>
    // Only the list: refreshing the open document would reset the saved-at indicator.
    void client.invalidateQueries({ queryKey: qk.modelPresets, exact: true }),
  );
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

  // One history entry per pause, not per keystroke: typing replaces, navigation pushes.
  useEffect(() => {
    if (draft === urlQuery) return;
    const timer = window.setTimeout(() => setFilters({ q: draft, page: "1" }, true), 200);
    return () => window.clearTimeout(timer);
  }, [draft, urlQuery, setFilters]);

  const [customOpen, setCustomOpen] = useState(false);

  // Modal re-focuses itself whenever `onClose` changes identity, so keep it stable or
  // every keystroke in the dialog loses focus after one character.
  const closeCustom = useCallback(() => setCustomOpen(false), []);

  const invalidateRegistry = () => {
    void client.invalidateQueries({ queryKey: qk.models });
    void client.invalidateQueries({ queryKey: qk.modelProviders });
    void client.invalidateQueries({ queryKey: qk.system });
  };

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

  const toast = useToast();
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

  // Coverage is computed from the whole registry, not the filtered page: the same model
  // served by two providers must say so even when a filter hides one of them.
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
  const isShared = (id: string) => (coverage.get(id)?.length ?? 0) > 1;
  const providerDisplay = useMemo(
    () => new Map((providers.data ?? []).map((p) => [p.id, p.display] as const)),
    [providers.data],
  );

  const sharedOnly = params.get("shared") === "1";
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
  const registryDisabled = useMemo(
    () => all.filter((m) => !m.enabled && matchesRegistry(m)),
    [all, effectiveProvider, appliedQuery, sharedOnly, coverage],
  );

  // Clamped, not corrected in the URL: a stale `?page=9` shows the last page instead of an
  // empty table, and the next click writes the real number.
  const lastPage = Math.max(1, Math.ceil(filtered.length / size));
  const rawPage = Number(params.get("page") ?? "1");
  const page = Math.min(Math.max(1, Number.isFinite(rawPage) ? rawPage : 1), lastPage);
  const pageRows = useMemo(
    () => filtered.slice((page - 1) * size, page * size),
    [filtered, page, size],
  );

  // A preset row is a registry model plus its place in the layout; a ref the registry no
  // longer lists is still shown, as unavailable, so ordering never silently drops.
  const byRef = useMemo(() => new Map(all.map((model) => [model.ref, model] as const)), [all]);
  const entryByRef = useMemo(
    () => new Map(layoutDraft.entries.map((entry) => [entry.ref, entry] as const)),
    [layoutDraft.entries],
  );
  const groupChoices = useMemo(() => {
    const knownEntries = layoutDraft.entries.filter((entry) => byRef.has(entry.ref));
    const knownProviders = new Set(Array.from(byRef.values(), (model) => model.provider));
    return effectiveGroups(
      knownEntries,
      layoutDraft.groups.filter(
        (group) => !group.provider || knownProviders.has(group.provider),
      ),
      (provider) => providerDisplay.get(provider) ?? provider,
    );
  }, [layoutDraft.entries, layoutDraft.groups, providerDisplay, byRef]);
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
  const entryRefs = useMemo(
    () => new Set(layoutDraft.entries.map((entry) => entry.ref)),
    [layoutDraft.entries],
  );
  const matchesRow = (row: LayoutRow) => {
    const needle = appliedQuery.trim().toLowerCase();
    const owner = row.model?.provider ?? row.ref.split("/")[0];
    if (effectiveProvider !== ALL && owner !== effectiveProvider) return false;
    if (sharedOnly && !(row.model && isShared(row.model.id))) return false;
    return (
      !needle ||
      row.ref.toLowerCase().includes(needle) ||
      (row.model?.display ?? "").toLowerCase().includes(needle)
    );
  };
  // Only an enabled model belongs in the active list; a disabled one moves to its own
  // table. Rows hidden from the preset stay hidden-only even when they are disabled too,
  // so no model appears twice.
  const visibleRows = layoutDraft.entries
    .filter((entry) => entry.visible)
    .map<LayoutRow>((entry) => ({
      ref: entry.ref,
      model: byRef.get(entry.ref) ?? null,
      visible: true,
      isNew: false,
    }))
    .filter((row) => row.model !== null)
    .filter(matchesRow);
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
    .map<LayoutRow>((model) => ({ ref: model.ref, model, visible: false, isNew: true }))
    .filter(matchesRow);
  const hiddenRows = layoutDraft.entries
    .filter((entry) => !entry.visible)
    .map<LayoutRow>((entry) => ({
      ref: entry.ref,
      model: byRef.get(entry.ref) ?? null,
      visible: false,
      isNew: false,
    }))
    .filter((row) => row.model !== null)
    .filter(matchesRow);
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
  // The disabled table serves both modes: the preset's visible entries and the plain
  // registry fallback, which has no layout to split.
  const disabledModels = presetsReady
    ? disabledRows.flatMap((row) => (row.model ? [row.model] : []))
    : registryDisabled;
  const disabledPage = Math.min(
    Math.max(1, Number(params.get("dpage") ?? "1") || 1),
    Math.max(1, Math.ceil(disabledModels.length / size)),
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
  const disabledSlice = disabledModels.slice((disabledPage - 1) * size, disabledPage * size);
  const inactiveSlice = [...hiddenRows, ...newRows].slice(
    (inactivePage - 1) * size,
    inactivePage * size,
  );
  const disabledPagination = {
    page: disabledPage,
    size,
    total: disabledModels.length,
    label: "disabled",
    onPage: (next: number) => setFilters({ dpage: String(next) }),
    onSize: (next: number) => setFilters({ size: String(next), dpage: "1" }),
  };

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

  // Pending ids rather than whole mutation objects: the column list must keep the same
  // identity while the toolbar is being typed into.
  const testTarget = test.isPending ? (test.variables?.ref ?? null) : null;
  const toggleTarget = toggle.isPending ? (toggle.variables?.id ?? null) : null;
  const removeTarget = remove.isPending ? (remove.variables?.ref ?? null) : null;
  const [deleting, setDeleting] = useState<Model | null>(null);
  const [editing, setEditing] = useState<Model | null>(null);

  const clearFilters = () => {
    setDraft("");
    setParams(new URLSearchParams(), { replace: false });
  };

  const columns = useMemo<Array<Column<Model>>>(() => {
    const list: Array<Column<Model>> = [
      {
        key: "model",
        title: "model",
        render: (m) => (
          <div className="flex min-w-[190px] max-w-[360px] flex-col">
            <div className="flex min-w-0 items-center gap-1.5">
              <span className="sr-only">Model </span>
              <span className="shrink-0 rounded-full border border-line bg-surface-2 px-1.5 py-px font-mono text-[10px] uppercase tracking-[0.06em] text-muted">
                {m.provider}
              </span>
              <span
                title={m.ref}
                className={cx(
                  // `min-w-0` matters: a truncating flex item keeps its full intrinsic
                  // width otherwise, and the table's auto layout would then scroll.
                  "min-w-0 truncate font-mono text-small",
                  m.enabled ? "text-fg" : "text-faint",
                )}
              >
                {m.id}
              </span>
              <CoveragePeers
                model={m}
                peers={coverage.get(m.id) ?? []}
                displayOf={(id: string) => providerDisplay.get(id) ?? id}
                onPick={(id: string) => setFilters({ provider: id, page: "1" })}
                onPrefer={(provider: string | null) => prefer.mutate({ model: m.id, provider })}
              />
              {m.preferred_provider === m.provider ? (
                <Badge tone="accent" title={`preferred provider for ${m.id}`}>
                  preferred
                </Badge>
              ) : null}
              {m.source === "custom" ? (
                <Badge tone="accent" title="added by an operator, not synced from the provider">
                  custom
                </Badge>
              ) : null}
              {m.deprecated ? (
                <Badge tone="warn" title="this family is retired by its provider">
                  deprecated
                </Badge>
              ) : null}
            </div>
            {m.display && m.display !== m.id ? (
              <div className="mt-0.5 min-w-0 truncate text-caption text-faint">{m.display}</div>
            ) : null}
            {/* Below xl the secondary columns are gone; their values ride in this cell
                instead of forcing a horizontal scroll. */}
            <div className="mt-1 flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1 text-caption text-faint xl:hidden">
              <span className="whitespace-nowrap tnum">ctx {compact(m.context)}</span>
              <span className="whitespace-nowrap tnum">max {compact(m.max_output)}</span>
              {priceLabel(m) ? (
                <span className="whitespace-nowrap tnum lg:hidden">{priceLabel(m)}</span>
              ) : null}
              <CapabilityChips capabilities={m.capabilities} max={2} subject={m.ref} />
            </div>
          </div>
        ),
      },
      {
        key: "context",
        title: "context",
        align: "right",
        width: "4.5rem",
        className: "hidden xl:table-cell",
        render: (m) => <span className="text-muted">{compact(m.context)}</span>,
      },
      {
        key: "max_output",
        title: "max\u00A0out",
        align: "right",
        width: "5.5rem",
        className: "hidden xl:table-cell",
        render: (m) => <span className="text-muted">{compact(m.max_output)}</span>,
      },
      {
        key: "cost",
        title: "price\u00A0/1M",
        align: "right",
        width: "7.5rem",
        className: "hidden lg:table-cell",
        render: (m) =>
          m.cost ? (
            <span
              className="whitespace-nowrap text-muted tnum"
              title={priceTitle(m)}
            >
              {priceLabel(m)}
            </span>
          ) : (
            <span className="text-faint">—</span>
          ),
      },
      {
        key: "capabilities",
        title: "capabilities",
        width: "9.5rem",
        className: "hidden xl:table-cell",
        render: (m) => (
          <CapabilityChips capabilities={m.capabilities} max={2} subject={m.ref} />
        ),
      },
      {
        key: "enabled",
        title: "enabled",
        width: "4.5rem",
        render: (m) => {
          // A reader sees the state; only an admin can flip it.
          if (!isAdmin) {
            return <Badge tone={m.enabled ? "pos" : "neutral"}>{m.enabled ? "on" : "off"}</Badge>;
          }
          const pending = toggleTarget === m.id;
          return (
            <Toggle
              size="sm"
              checked={m.enabled}
              label={`${m.ref} ${m.enabled ? "enabled" : "disabled"}`}
              disabled={pending}
              onChange={() => toggle.mutate(m)}
            />
          );
        },
      },
      {
        key: "actions",
        title: "Actions",
        srOnlyTitle: true,
        align: "right",
        width: "5rem",
        render: (m) => (
          <div className="flex items-center justify-end gap-1">
            <TestButton
              model={m}
              pending={testTarget === m.ref}
              onTest={() => test.mutate(m)}
            />
            <RowMenu
              model={m}
              pendingDelete={removeTarget === m.ref}
              onEdit={() => setEditing(m)}
              onDelete={() => setDeleting(m)}
              onHide={presetId ? () => hideRow.current(m.ref) : undefined}
              groups={isAdmin && presetId ? groupChoices : undefined}
              currentGroup={groupOf(m.ref)}
              onMoveGroup={(next) => layoutDraft.assignGroup(m.ref, next)}
            />
          </div>
        ),
      },
    ];
    // A reader sees the registry, not the controls that change it.
    return isAdmin ? list : list.filter((column) => column.key !== "actions");
  }, [
    isAdmin,
    testTarget,
    toggleTarget,
    removeTarget,
    presetId,
    coverage,
    providerDisplay,
    groupChoices,
    groupOf,
    layoutDraft,
  ]);

  const filteredOut = appliedQuery.trim() !== "" || effectiveProvider !== ALL;
  const providerName =
    connected.find((p) => p.id === effectiveProvider)?.display ?? effectiveProvider;
  const coverageBar = (
    <div className="flex flex-wrap items-center justify-between gap-2 border-b border-line px-5 py-2">
      <span className="text-caption text-faint tnum">
        {all.length.toLocaleString()} models · {uniqueIds.toLocaleString()} unique ids ·{" "}
        {sharedCount.toLocaleString()} shared by 2+ providers
      </span>
      <FilterChip
        on={sharedOnly}
        onClick={() => setFilters({ shared: sharedOnly ? null : "1", page: "1", ipage: "1" })}
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

  // The disabled table and the "everything is disabled" empty state reference emptyState,
  // so they are built after it.
  const activeEmpty =
    disabledModels.length > 0 ? (
      <div className="flex flex-col items-center gap-2">
        <p>Every model that matches is disabled.</p>
        <p className="text-caption text-faint">
          Enable one in Disabled models below to route through it again.
        </p>
      </div>
    ) : (
      emptyState
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
            testTarget={testTarget}
            toggleTarget={toggleTarget}
            removeTarget={removeTarget}
            onTest={(m) => test.mutate(m)}
            onToggle={(m) => toggle.mutate(m)}
            onEdit={setEditing}
            onDelete={setDeleting}
            coverageFor={(id) => coverage.get(id) ?? []}
            displayOfProvider={(id) => providerDisplay.get(id) ?? id}
            onPickProvider={(id) => setFilters({ provider: id, page: "1" })}
            onPreferPreference={(model, provider) => prefer.mutate({ model: model.id, provider })}
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


  const pagination = {
    page,
    size,
    total: filtered.length,
    label: "models",
    onPage: (next: number) => setFilters({ page: String(next) }),
    onSize: (next: number) => setFilters({ size: String(next), page: "1" }),
  };

  return (
    <>
      <ConfirmDialog
        open={deleting !== null}
        title="Delete custom model"
        message={<>Remove <span className="font-mono">{deleting?.ref}</span> from this gateway's registry? Requests naming it fail until it is listed again.</>}
        confirmLabel="Delete model"
        busy={remove.isPending}
        error={remove.error}
        onConfirm={() => {
          if (deleting) {
            remove.mutate(deleting, { onSuccess: () => setDeleting(null) });
          }
        }}
        onClose={() => {
          if (!remove.isPending) {
            remove.reset();
            setDeleting(null);
          }
        }}
      />

      <PageHeader
        title="Models"
        subtitle="Every model this gateway can route, with its limits, price, and capabilities."
        actions={
          <>
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
              <Button variant="primary" onClick={() => setCustomOpen(true)}>
                add custom model
              </Button>
            ) : null}
          </>
        }
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
      {sync.isError || toggle.isError || remove.isError ? (
        <div className="mb-3">
          <ErrorNote error={sync.error ?? toggle.error ?? remove.error} />
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
              missing feature, so name it and keep the plain registry usable. */}
          <div className="flex flex-col gap-2 px-5 py-3">
            <p className="text-caption text-faint">
              Presets could not be loaded; the registry below is the plain list.
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
                <span className="font-mono">{undo.ref}</span> hidden from this preset. It still
                routes; only this list changed.
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
            {coverageBar}
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
                          testTarget={testTarget}
                          toggleTarget={toggleTarget}
                          removeTarget={removeTarget}
                          onTest={(m) => test.mutate(m)}
                          onToggle={(m) => toggle.mutate(m)}
                          onEdit={setEditing}
                          onDelete={setDeleting}
                          onHide={(ref) => hideRow.current(ref)}
                          groups={isAdmin && presetId ? groupChoices : undefined}
                          currentGroup={groupOf(model.ref)}
                          onMoveGroup={(next) => layoutDraft.assignGroup(model.ref, next)}
                          coverageFor={(id) => coverage.get(id) ?? []}
                          displayOfProvider={(id) => providerDisplay.get(id) ?? id}
                          onPickProvider={(id) => setFilters({ provider: id, page: "1" })}
                          onPreferPreference={(model, provider) => prefer.mutate({ model: model.id, provider })}
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

          {disabledCard}

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
      ) : !models.isLoading && all.length === 0 ? (
        <EmptyRegistry onSync={() => sync.mutate()} syncing={sync.isPending} canAdmin={isAdmin} />
      ) : (
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
          {coverageBar}
          {phone && pageRows.length > 0 ? (
            <>
              <ModelCards
                rows={pageRows}
                isAdmin={isAdmin}
                testTarget={testTarget}
                toggleTarget={toggleTarget}
                removeTarget={removeTarget}
                onTest={(model) => test.mutate(model)}
                onToggle={(model) => toggle.mutate(model)}
                onEdit={setEditing}
                onDelete={setDeleting}
                coverageFor={(id) => coverage.get(id) ?? []}
                displayOfProvider={(id) => providerDisplay.get(id) ?? id}
                onPickProvider={(id) => setFilters({ provider: id, page: "1" })}
                onPreferPreference={(model, provider) => prefer.mutate({ model: model.id, provider })}
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
      )}

      {editing ? (
        <CustomModelModal
          providers={connected}
          defaultProvider={editing.provider}
          initial={editing}
          onClose={() => setEditing(null)}
          onSaved={invalidateRegistry}
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
      />

      {customOpen ? (
        <CustomModelModal
          providers={connected}
          defaultProvider={effectiveProvider !== ALL ? effectiveProvider : ""}
          onClose={closeCustom}
          onSaved={invalidateRegistry}
        />
      ) : null}
    </>
  );
}

/** A test is one icon: a word per row made the name repeat and the column jump. */
function TestButton({
  model,
  pending,
  onTest,
}: {
  model: Model;
  pending: boolean;
  onTest: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onTest}
      disabled={pending}
      aria-busy={pending || undefined}
      aria-label={
        pending ? `Testing ${model.ref}` : `Send a test completion to ${model.ref}`
      }
      title={`Send a 16-token completion to ${model.ref}`}
      className="flex h-8 w-8 shrink-0 items-center justify-center rounded-full text-muted transition-colors hover:bg-surface-3 hover:text-fg disabled:opacity-70"
    >
      {pending ? (
        <CircleNotch size={15} className="animate-spin" aria-hidden />
      ) : (
        <Play size={14} aria-hidden />
      )}
    </button>
  );
}

/**
 * Edit and delete live behind one trigger, so the row keeps a stable width.
 *
 * Menu semantics (one tab stop, arrows/Home/End inside) are hand-rolled: Radix Popover
 * already owns focus restore and Escape, and a `@radix-ui/react-dropdown-menu` dependency
 * for two items in one page was not worth the bundle.
 */
function RowMenu({
  model,
  pendingDelete,
  onEdit,
  onDelete,
  onHide,
  groups,
  currentGroup,
  onMoveGroup,
}: {
  model: Model;
  pendingDelete: boolean;
  onEdit: () => void;
  onDelete: () => void;
  /** Only while a preset is active: hides the row here, not from routing. */
  onHide?: () => void;
  /** Groups on this preset; omitted for callers without a layout. */
  groups?: Array<DraftGroup & { implicit?: boolean }>;
  currentGroup?: string;
  onMoveGroup?: (group: string | null) => void;
}) {
  const surface = useRef<HTMLDivElement>(null);
  const items = () =>
    [...(surface.current?.querySelectorAll<HTMLElement>('[role="menuitem"]') ?? [])];
  const focusItem = (index: number) => {
    const list = items();
    if (list.length === 0) return;
    list[(index + list.length) % list.length]?.focus();
  };
  const onKeyDown = (event: ReactKeyboardEvent<HTMLDivElement>) => {
    const index = items().indexOf(document.activeElement as HTMLElement);
    switch (event.key) {
      case "ArrowDown":
        event.preventDefault();
        focusItem(index + 1);
        break;
      case "ArrowUp":
        event.preventDefault();
        focusItem(index <= 0 ? -1 : index - 1);
        break;
      case "Home":
        event.preventDefault();
        focusItem(0);
        break;
      case "End":
        event.preventDefault();
        focusItem(-1);
        break;
      default:
        break;
    }
  };
  const item =
    "flex h-8 items-center rounded-[var(--radius-input)] px-2 text-left text-small text-fg hover:bg-surface-3 focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-[-2px] focus-visible:outline-accent-text";
  const movableGroups = groups?.filter(
    (group) => !group.provider || group.provider === model.provider,
  );
  return (
    <Popover.Root>
      <Popover.Trigger asChild>
        <button
          type="button"
          aria-label={`More actions for ${model.ref}`}
          aria-haspopup="menu"
          className="flex h-8 w-8 shrink-0 items-center justify-center rounded-full text-muted transition-colors hover:bg-surface-3 hover:text-fg focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent-text"
        >
          <DotsThree size={18} weight="bold" aria-hidden />
        </button>
      </Popover.Trigger>
      <Popover.Portal>
        <Popover.Content
          ref={surface}
          role="menu"
          side="bottom"
          align="end"
          sideOffset={4}
          aria-label={`Actions for ${model.ref}`}
          onOpenAutoFocus={(event) => {
            event.preventDefault();
            window.requestAnimationFrame(() => focusItem(0));
          }}
          onKeyDown={onKeyDown}
          className="popover-panel z-50 flex w-56 flex-col rounded-[var(--radius-card)] border border-line bg-surface p-1 shadow-lg focus:outline-none"
        >
          <Popover.Close asChild>
            <button type="button" role="menuitem" tabIndex={-1} onClick={onEdit} className={item}>
              Edit limits and price
            </button>
          </Popover.Close>
          {groups && onMoveGroup ? (
            <div className="mt-1 border-t border-line pt-1">
              <p className="px-2 py-1 text-caption text-faint">Move to group</p>
              <div className="max-h-48 overflow-y-auto">
                {movableGroups?.map((group) => {
                  const checked = currentGroup === group.id;
                  return (
                    <Popover.Close asChild key={group.id}>
                      <button
                        type="button"
                        role="menuitem"
                        tabIndex={-1}
                        aria-current={checked ? "true" : undefined}
                        onClick={() => onMoveGroup(group.provider ? null : group.id)}
                        className={cx(item, checked && "bg-surface-2 text-accent-text")}
                      >
                        <span className="min-w-0 flex-1 truncate">{group.name}</span>
                        {group.provider ? (
                          <span className="ml-2 shrink-0 text-caption text-faint">provider</span>
                        ) : null}
                        {checked ? <span className="ml-2 shrink-0 text-caption">current</span> : null}
                      </button>
                    </Popover.Close>
                  );
                })}
              </div>
            </div>
          ) : null}
          {onHide ? (
            <Popover.Close asChild>
              <button type="button" role="menuitem" tabIndex={-1} onClick={onHide} className={item}>
                Hide from this list
              </button>
            </Popover.Close>
          ) : null}
          {model.source === "custom" ? (
            <Popover.Close asChild>
              <button
                type="button"
                role="menuitem"
                tabIndex={-1}
                onClick={onDelete}
                disabled={pendingDelete}
                className={cx(item, "text-[var(--color-neg-ink)] hover:bg-[var(--color-neg-tint)]")}
              >
                Delete custom model
              </button>
            </Popover.Close>
          ) : null}
        </Popover.Content>
      </Popover.Portal>
    </Popover.Root>
  );
}

/** Below `sm` a row becomes a card: four table columns at 390 would scroll sideways. */
function ModelCards({
  rows,
  isAdmin,
  testTarget,
  toggleTarget,
  removeTarget,
  onTest,
  onToggle,
  onEdit,
  onDelete,
  coverageFor,
  displayOfProvider,
  onPickProvider,
  onPreferPreference,
}: {
  rows: Model[];
  isAdmin: boolean;
  testTarget: string | null;
  toggleTarget: string | null;
  removeTarget: string | null;
  onTest: (model: Model) => void;
  onToggle: (model: Model) => void;
  onEdit: (model: Model) => void;
  onDelete: (model: Model) => void;
  coverageFor: (id: string) => Model[];
  displayOfProvider: (id: string) => string;
  onPickProvider: (id: string) => void;
  onPreferPreference: (model: Model, provider: string | null) => void;
}) {
  return (
    <ul aria-label="Models registry" className="flex flex-col">
      {rows.map((m) => (
        <li
          key={m.ref}
          className="border-b border-[var(--color-rule)] px-4 py-3 last:border-b-0"
        >
          <ModelCardBody
            model={m}
            isAdmin={isAdmin}
            testTarget={testTarget}
            toggleTarget={toggleTarget}
            removeTarget={removeTarget}
            onTest={onTest}
            onToggle={onToggle}
            onEdit={onEdit}
            onDelete={onDelete}
            coverageFor={coverageFor}
            displayOfProvider={displayOfProvider}
            onPickProvider={onPickProvider}
            onPreferPreference={onPreferPreference}
          />
        </li>
      ))}
    </ul>
  );
}

/** One model as a phone card. Shared by the registry and the sortable active list. */
function ModelCardBody({
  model: m,
  isAdmin,
  testTarget,
  toggleTarget,
  removeTarget,
  onTest,
  onToggle,
  onEdit,
  onDelete,
  onHide,
  coverageFor,
  displayOfProvider,
  onPickProvider,
  onPreferPreference,
  groups,
  currentGroup,
  onMoveGroup,
}: {
  model: Model;
  isAdmin: boolean;
  testTarget: string | null;
  toggleTarget: string | null;
  removeTarget: string | null;
  onTest: (model: Model) => void;
  onToggle: (model: Model) => void;
  onEdit: (model: Model) => void;
  onDelete: (model: Model) => void;
  onHide?: (ref: string) => void;
  groups?: Array<DraftGroup & { implicit?: boolean }>;
  currentGroup?: string;
  onMoveGroup?: (group: string | null) => void;
  coverageFor: (id: string) => Model[];
  displayOfProvider: (id: string) => string;
  onPickProvider: (id: string) => void;
  onPreferPreference: (model: Model, provider: string | null) => void;
}) {
  return (
          <div className="flex items-start justify-between gap-3">
            <div className="min-w-0">
              <div className="flex min-w-0 flex-wrap items-center gap-1.5">
                <span className="shrink-0 rounded-full border border-line bg-surface-2 px-1.5 py-px font-mono text-[10px] uppercase tracking-[0.06em] text-muted">
                  {m.provider}
                </span>
                <h3
                  className={cx(
                    "min-w-0 truncate font-mono text-small",
                    m.enabled ? "text-fg" : "text-faint",
                  )}
                >
                  {m.id}
                </h3>
                <CoveragePeers
                  model={m}
                  peers={coverageFor(m.id)}
                  displayOf={displayOfProvider}
                  onPick={onPickProvider}
                  onPrefer={(provider) => onPreferPreference(m, provider)}
                />
                {m.preferred_provider === m.provider ? (
                  <Badge tone="accent" title={`preferred provider for ${m.id}`}>
                    preferred
                  </Badge>
                ) : null}
                {m.source === "custom" ? (
                  <Badge tone="accent" title="added by an operator, not synced from the provider">
                    custom
                  </Badge>
                ) : null}
                {m.deprecated ? (
                  <Badge tone="warn" title="this family is retired by its provider">
                    deprecated
                  </Badge>
                ) : null}
              </div>
              {m.display && m.display !== m.id ? (
                <p className="mt-0.5 truncate text-caption text-faint">{m.display}</p>
              ) : null}
              <div className="mt-1 flex flex-wrap items-center gap-x-2 gap-y-1 text-caption text-faint">
                <span className="whitespace-nowrap tnum">ctx {compact(m.context)}</span>
                <span className="whitespace-nowrap tnum">max {compact(m.max_output)}</span>
                {m.cost ? (
                  <span className="whitespace-nowrap tnum" title={priceTitle(m)}>
                    {cardPrice(m)}
                  </span>
                ) : null}
              </div>
              <div className="mt-1.5">
                <CapabilityChips capabilities={m.capabilities} max={2} subject={m.ref} />
              </div>
            </div>
            <div className="flex shrink-0 flex-col items-end gap-2">
              {isAdmin ? (
                <Toggle
                  size="sm"
                  checked={m.enabled}
                  label={`${m.ref} ${m.enabled ? "enabled" : "disabled"}`}
                  disabled={toggleTarget === m.id}
                  onChange={() => onToggle(m)}
                />
              ) : (
                <Badge tone={m.enabled ? "pos" : "neutral"}>{m.enabled ? "on" : "off"}</Badge>
              )}
              {isAdmin ? (
                <div className="flex items-center gap-1">
                  <TestButton
                    model={m}
                    pending={testTarget === m.ref}
                    onTest={() => onTest(m)}
                  />
                  <RowMenu
                    model={m}
                    pendingDelete={removeTarget === m.ref}
                    onEdit={() => onEdit(m)}
                    onDelete={() => onDelete(m)}
                    onHide={onHide ? () => onHide(m.ref) : undefined}
                    groups={groups}
                    currentGroup={currentGroup}
                    onMoveGroup={onMoveGroup}
                  />
                </div>
              ) : null}
            </div>
          </div>
  );
}

/** A section title above one group's table; the controls live in the group manager. */
function GroupHeading({
  group,
  count,
}: {
  group: DraftGroup & { implicit?: boolean };
  count: number;
}) {
  return (
    <div className="flex flex-wrap items-baseline justify-between gap-2 border-b border-line bg-surface-2 px-5 py-2.5">
      <div className="flex min-w-0 items-baseline gap-2">
        <h3 className="truncate text-small font-semibold text-fg">{group.name}</h3>
        <span className="shrink-0 text-caption text-faint">
          {group.provider ? `provider · ${group.provider}` : "custom group"}
        </span>
      </div>
      <span className="text-caption text-faint tnum">{count.toLocaleString()} models</span>
    </div>
  );
}

/** One group row in the manager; local text state keeps every keystroke cheap. */
function GroupNameField({ name, onCommit }: { name: string; onCommit: (name: string) => void }) {
  const [value, setValue] = useState(name);
  useEffect(() => setValue(name), [name]);
  return (
    <Input
      value={value}
      ariaLabel={`Rename group ${name}`}
      onChange={setValue}
      onBlur={() => {
        if (value.trim() && value.trim() !== name) onCommit(value.trim());
        else setValue(name);
      }}
      onKeyDown={(event) => {
        if (event.key === "Enter") event.currentTarget.blur();
        if (event.key === "Escape") {
          setValue(name);
          event.currentTarget.blur();
        }
      }}
    />
  );
}

/** Create, rename, reorder and remove the groups of the active preset. */
function GroupManager({
  open,
  groups,
  onClose,
  onCreate,
  onRename,
  onReorder,
  onDelete,
}: {
  open: boolean;
  groups: Array<DraftGroup & { implicit?: boolean }>;
  onClose: () => void;
  onCreate: (name: string) => void;
  onRename: (id: string, name: string) => void;
  onReorder: (from: string, to: string) => void;
  onDelete: (id: string) => void;
}) {
  const [name, setName] = useState("");
  useEffect(() => {
    if (open) setName("");
  }, [open]);
  return (
    <Modal
      title="model groups"
      description="Groups change this preset's board only; routing is unchanged. Provider buckets start with every model of that provider."
      open={open}
      onClose={onClose}
      width="lg"
      footer={<Button onClick={onClose}>done</Button>}
    >
      <ul className="flex flex-col gap-2">
        {groups.map((group, index) => (
          <li
            key={group.id}
            className="flex flex-wrap items-center gap-2 rounded-[var(--radius-card)] border border-line bg-surface-2 p-2"
          >
            <span className="min-w-0 flex-1">
              <GroupNameField name={group.name} onCommit={(next) => onRename(group.id, next)} />
            </span>
            <Badge tone={group.provider ? "neutral" : "accent"}>
              {group.provider ? "provider" : "custom"}
            </Badge>
            <Button
              variant="quiet"
              aria-label={`Move ${group.name} up`}
              disabled={index === 0}
              onClick={() => onReorder(group.id, groups[index - 1]?.id ?? group.id)}
            >
              ↑
            </Button>
            <Button
              variant="quiet"
              aria-label={`Move ${group.name} down`}
              disabled={index === groups.length - 1}
              onClick={() => onReorder(group.id, groups[index + 1]?.id ?? group.id)}
            >
              ↓
            </Button>
            {group.provider ? null : (
              <Button
                variant="quiet"
                aria-label={`Delete group ${group.name}`}
                onClick={() => onDelete(group.id)}
              >
                delete
              </Button>
            )}
          </li>
        ))}
      </ul>
      <form
        className="mt-4 flex items-end gap-2 border-t border-line pt-4"
        onSubmit={(event) => {
          event.preventDefault();
          if (!name.trim()) return;
          onCreate(name.trim());
          setName("");
        }}
      >
        <div className="min-w-0 flex-1">
          <Field label="new group">
            <Input
              value={name}
              placeholder="Fast models"
              onChange={setName}
            />
          </Field>
        </div>
        <Button variant="primary" type="submit" disabled={!name.trim()}>
          add group
        </Button>
      </form>
    </Modal>
  );
}

/** The one free-text model field in the app. Mounted only while open, so typing in it
 *  never re-renders the registry table behind the dialog. */
function CustomModelModal({
  providers,
  defaultProvider,
  initial,
  onClose,
  onSaved,
}: {
  providers: ModelProvider[] | undefined;
  defaultProvider: string;
  /** Present when editing an existing row: it becomes a custom override of that ref. */
  initial?: Model;
  onClose: () => void;
  onSaved: () => void;
}) {
  const [form, setForm] = useState({
    provider: initial?.provider ?? (defaultProvider || providers?.[0]?.id || ""),
    id: initial?.id ?? "",
    display: initial?.display ?? "",
    context: initial?.context ? String(initial.context) : "",
    maxOutput: initial?.max_output ? String(initial.max_output) : "",
    priceIn: initial?.cost ? String(initial.cost.input) : "",
    priceOut: initial?.cost ? String(initial.cost.output) : "",
  });
  const [caps, setCaps] = useState<Partial<Record<keyof Capabilities, boolean>>>(
    initial ? { ...initial.capabilities } : {},
  );

  const create = useMutation({
    mutationFn: () =>
      api.post<Model>("/api/models/custom", {
        provider: form.provider,
        id: form.id.trim(),
        display: form.display.trim() || null,
        context: numeric(form.context),
        max_output: numeric(form.maxOutput),
        cost:
          form.priceIn.trim() !== "" || form.priceOut.trim() !== ""
            ? {
                input: Number(form.priceIn) || 0,
                output: Number(form.priceOut) || 0,
              }
            : null,
        capabilities: Object.fromEntries(CAPABILITY_KEYS.map((key) => [key, caps[key] === true])),
      }),
    onSuccess: () => {
      onSaved();
      onClose();
    },
  });

  const ready = form.provider !== "" && form.id.trim() !== "" && !create.isPending;

  return (
    <Modal
      title={initial ? "edit model" : "add custom model"}
      description={initial ? `${initial.provider}/${initial.id} — saves an operator override for this model.` : undefined}
      open
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose}>cancel</Button>
          <Button variant="primary" disabled={!ready} onClick={() => create.mutate()}>
            {create.isPending ? "saving…" : initial ? "save changes" : "add model"}
          </Button>
        </>
      }
    >
      <div className="space-y-3">
        <div className="grid grid-cols-2 gap-3">
          <Field label="provider">
            <Select
              ariaLabel="custom model provider"
              value={form.provider}
              onChange={(v) => setForm((f) => ({ ...f, provider: v }))}
              disabled={Boolean(initial)}
            >
              {(providers ?? []).map((p) => (
                <option key={p.id} value={p.id}>
                  {p.display}
                </option>
              ))}
              {/* Editing a row whose provider lost its connection still needs a selected
                  option; the value never changes while this select is disabled. */}
              {form.provider !== "" && !(providers ?? []).some((p) => p.id === form.provider) ? (
                <option value={form.provider}>{form.provider} (not connected)</option>
              ) : null}
            </Select>
          </Field>
          <Field label="model id" hint="the only free-text model field">
            <Input
              value={form.id}
              onChange={(v) => setForm((f) => ({ ...f, id: v }))}
              placeholder="my-model"
              ariaLabel="custom model id"
              disabled={Boolean(initial)}
            />
          </Field>
        </div>
        <Field label="display">
          <Input
            value={form.display}
            onChange={(v) => setForm((f) => ({ ...f, display: v }))}
            placeholder="My Model"
            ariaLabel="custom model display name"
          />
        </Field>
        <div className="grid grid-cols-2 gap-3">
          <Field label="context" hint="tokens">
            <Input
              type="number"
              value={form.context}
              onChange={(v) => setForm((f) => ({ ...f, context: v }))}
              placeholder="128000"
              ariaLabel="custom model context window"
            />
          </Field>
          <Field label="max out" hint="tokens">
            <Input
              type="number"
              value={form.maxOutput}
              onChange={(v) => setForm((f) => ({ ...f, maxOutput: v }))}
              placeholder="8192"
              ariaLabel="custom model max output"
            />
          </Field>
        </div>
        <div className="grid grid-cols-2 gap-3">
          <Field label="price in /1M" hint="USD per million tokens; empty = unpublished">
            <Input
              type="number"
              value={form.priceIn}
              onChange={(v) => setForm((f) => ({ ...f, priceIn: v }))}
              placeholder="1.00"
              ariaLabel="custom model input price"
            />
          </Field>
          <Field label="price out /1M" hint="USD per million tokens">
            <Input
              type="number"
              value={form.priceOut}
              onChange={(v) => setForm((f) => ({ ...f, priceOut: v }))}
              placeholder="4.00"
              ariaLabel="custom model output price"
            />
          </Field>
        </div>
        <div className="flex min-w-0 flex-col gap-1.5">
          <span className="text-small font-semibold text-ink-soft">capabilities</span>
          <div className="grid grid-cols-3 gap-x-3 gap-y-1.5">
            {CAPABILITY_KEYS.map((key) => (
              <Checkbox
                key={key}
                checked={caps[key] === true}
                onChange={(next) => setCaps((c) => ({ ...c, [key]: next }))}
                label={CAPABILITY_LABELS[key].chip}
              />
            ))}
          </div>
        </div>
        {create.isError ? <ErrorNote error={create.error} /> : null}
      </div>
    </Modal>
  );
}

const rowKey = (model: Model) => model.ref;

/** Nothing here means nothing is connected: models come from providers, not catalogs. */
function EmptyRegistry({
  onSync,
  syncing,
  canAdmin,
}: {
  onSync: () => void;
  syncing: boolean;
  canAdmin: boolean;
}) {
  return (
    <Card className="px-6 py-12 text-center">
      <Heading level={2} className="text-heading font-medium">No models yet</Heading>
      <p className="mx-auto mt-1.5 max-w-[440px] text-body text-muted">
        Models are listed from the providers you connected — add one under{" "}
        <a href="/app/providers" className="underline underline-offset-2">
          Providers
        </a>
        . Metadata such as context window, capabilities, and price is filled in from models.dev when you sync.
      </p>
      {canAdmin ? (
        <div className="mt-5 flex justify-center gap-2">
          <Button variant="primary" onClick={onSync} disabled={syncing}>
            {syncing ? "syncing…" : "sync metadata"}
          </Button>
          <a
            href="/app/providers"
            className="inline-flex h-8 items-center rounded-lg border border-line-strong bg-surface-2 px-3 text-body text-fg hover:border-[var(--color-line-strong)]"
          >
            go to providers
          </a>
        </div>
      ) : null}
    </Card>
  );
}

/** `$2.50 / $10.00`, in the units the `price /1M` column names. */
function priceLabel(model: Model): string | null {
  return model.cost
    ? `$${model.cost.input.toFixed(2)} / $${model.cost.output.toFixed(2)}`
    : null;
}

/** A card has no column header, so the unit is spelled out. */
function cardPrice(model: Model): string | null {
  return model.cost
    ? `$${model.cost.input.toFixed(2)} in / $${model.cost.output.toFixed(2)} out`
    : null;
}

function priceTitle(model: Model): string {
  if (!model.cost) return "";
  const cache =
    model.cost.cache_read === undefined ? "" : ` · cache read $${model.cost.cache_read}`;
  return `input $${model.cost.input} · output $${model.cost.output}${cache} · per 1M tokens`;
}

/** "" and non-positive input mean "unknown", never zero. */
function numeric(value: string): number | null {
  const trimmed = value.trim();
  if (!trimmed) return null;
  const parsed = Number(trimmed);
  return Number.isFinite(parsed) && parsed > 0 ? Math.floor(parsed) : null;
}

/**
 * "2 providers" for a canonical model id, with the peers behind a popover. The row itself
 * stays one provider: enabling, testing and pricing are per connection, so rows are never
 * merged; this only says the same id is routable elsewhere and lets the operator jump there.
 */
function CoveragePeers({
  model,
  peers,
  displayOf,
  onPick,
  onPrefer,
}: {
  model: Model;
  peers: Model[];
  displayOf: (provider: string) => string;
  onPick: (provider: string) => void;
  /** `null` clears the preference; the same id carries it on every provider row. */
  onPrefer: (provider: string | null) => void;
}) {
  if (peers.length < 2) return null;
  const preferred = model.preferred_provider ?? null;
  const preferredMissing = preferred !== null && !peers.some((peer) => peer.provider === preferred);
  const refs = peers.map((peer) => peer.ref).join("\n");
  return (
    <Popover.Root>
      <Popover.Trigger asChild>
        <button
          type="button"
          aria-label={`${peers.length} providers serve ${model.id}`}
          aria-haspopup="dialog"
          title={refs}
          className="inline-flex h-5 shrink-0 items-center rounded-full border border-line bg-surface-2 px-1.5 text-caption text-muted transition-colors hover:bg-surface-3 hover:text-fg focus-visible:outline-none! outline-0!"
        >
          {peers.length} providers
        </button>
      </Popover.Trigger>
      <Popover.Portal>
        <Popover.Content
          side="bottom"
          align="start"
          sideOffset={4}
          aria-label={`Providers serving ${model.id}`}
          className="popover-panel z-50 w-80 rounded-[var(--radius-card)] border border-line bg-surface p-1.5 shadow-lg focus:outline-none"
        >
          <p className="px-1 pb-1 text-caption text-faint">
            <span className="font-mono text-muted">{model.id}</span> is served by
          </p>
          <ul className="flex flex-col">
            {peers.map((peer) => {
              const isPreferred = preferred === peer.provider;
              return (
                <li key={peer.ref} className="flex items-center gap-1">
                  <Popover.Close asChild>
                    <button
                      type="button"
                      onClick={() => onPick(peer.provider)}
                      className="flex min-w-0 flex-1 items-center gap-2 rounded-[var(--radius-input)] px-1.5 py-1.5 text-left text-small hover:bg-surface-3 focus-visible:outline-none! outline-0!"
                    >
                      <ProviderIcon provider={peer.provider} size={14} />
                      <span className="min-w-0 flex-1">
                        <span className="block truncate text-fg">{displayOf(peer.provider)}</span>
                        <span className="block truncate font-mono text-caption text-faint">
                          {peer.ref}
                        </span>
                      </span>
                      <span className="shrink-0 text-caption text-faint tnum">
                        {priceLabel(peer) ?? "no price"}
                      </span>
                      <Badge tone={peer.enabled ? "pos" : "neutral"}>
                        {peer.enabled ? "on" : "off"}
                      </Badge>
                    </button>
                  </Popover.Close>
                  <IconButton
                    label={
                      isPreferred
                        ? `Clear the preferred provider for ${model.id}`
                        : `Use ${displayOf(peer.provider)} as the preferred provider for ${model.id}`
                    }
                    aria-pressed={isPreferred}
                    onClick={() => onPrefer(isPreferred ? null : peer.provider)}
                    className={isPreferred ? "text-[var(--color-accent-text)]" : undefined}
                  >
                    <Star size={14} weight={isPreferred ? "fill" : "regular"} aria-hidden />
                  </IconButton>
                </li>
              );
            })}
          </ul>
          {preferredMissing ? (
            <p
              role="status"
              className="mt-1 flex items-center justify-between gap-2 rounded-[6px] border border-[var(--color-warn-line)] bg-[var(--color-warn-tint)] px-2 py-1.5 text-caption leading-relaxed text-[var(--color-warn-ink)]"
            >
              <span className="min-w-0">
                preferred provider <span className="font-mono">{preferred}</span> is not connected
                right now
              </span>
              <button
                type="button"
                onClick={() => onPrefer(null)}
                className="shrink-0 underline underline-offset-2"
              >
                clear
              </button>
            </p>
          ) : null}
        </Popover.Content>
      </Popover.Portal>
    </Popover.Root>
  );
}
