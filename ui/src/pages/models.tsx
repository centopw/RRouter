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
import { CircleNotch, DotsThree, Play } from "@phosphor-icons/react";
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
import { CAPABILITY_KEYS, CAPABILITY_LABELS, CapabilityChips } from "../components/capability-chips";
import { Checkbox, Heading, Toggle } from "../components/atoms";
import { compact } from "../lib/format";
import { usePhone } from "../lib/media";
import { compareModels } from "../lib/models";
import { useToast } from "../components/toast";
import { qk, useIsAdmin, useModelProviders, useModels } from "../lib/queries";
import { api, type Capabilities, type Model, type ModelProvider, type ModelTest, type SyncResult } from "../lib/api";

const ALL = "all";
/** Page sizes the registry offers; 50 is the default because it fills a laptop viewport. */
const SIZES = [25, 50, 100, 200] as const;
const DEFAULT_SIZE = 50;

export default function Models() {
  const models = useModels();
  const providers = useModelProviders();
  const isAdmin = useIsAdmin();
  const phone = usePhone();
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
          (key === "page" && Number(value) === 1);
        if (defaulted) next.delete(key);
        else next.set(key, value);
      }
      setParams(next, { replace });
    },
    [params, setParams],
  );

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
  const remove = useMutation({
    mutationFn: (model: Model) =>
      api.del<{ deleted: boolean }>(
        `/api/models/custom?provider=${encodeURIComponent(model.provider)}&id=${encodeURIComponent(model.id)}`,
      ),
    onSuccess: invalidateRegistry,
  });

  const all = useMemo(
    () =>
      [...(models.data ?? [])].sort(
        (a, b) => a.provider.localeCompare(b.provider) || compareModels(a, b),
      ),
    [models.data],
  );

  const appliedQuery = useDeferredValue(urlQuery);
  const filtered = useMemo(() => {
    const q = appliedQuery.trim().toLowerCase();
    return all.filter(
      (m) =>
        (provider === ALL || m.provider === provider) &&
        (!q || m.ref.toLowerCase().includes(q) || (m.display ?? "").toLowerCase().includes(q)),
    );
  }, [all, provider, appliedQuery]);

  // Clamped, not corrected in the URL: a stale `?page=9` shows the last page instead of an
  // empty table, and the next click writes the real number.
  const lastPage = Math.max(1, Math.ceil(filtered.length / size));
  const rawPage = Number(params.get("page") ?? "1");
  const page = Math.min(Math.max(1, Number.isFinite(rawPage) ? rawPage : 1), lastPage);
  const pageRows = useMemo(
    () => filtered.slice((page - 1) * size, page * size),
    [filtered, page, size],
  );

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
            />
          </div>
        ),
      },
    ];
    // A reader sees the registry, not the controls that change it.
    return isAdmin ? list : list.filter((column) => column.key !== "actions");
  }, [isAdmin, testTarget, toggleTarget, removeTarget]);

  const filteredOut = appliedQuery.trim() !== "" || provider !== ALL;
  const providerName = providers.data?.find((p) => p.id === provider)?.display ?? provider;
  const emptyState = filteredOut ? (
    <div className="flex flex-col items-center gap-2">
      <p>
        No models match{" "}
        {appliedQuery.trim() !== "" ? <span className="font-mono text-fg">{appliedQuery}</span> : null}
        {appliedQuery.trim() !== "" && provider !== ALL ? " in " : null}
        {provider !== ALL ? <span className="text-fg">{providerName}</span> : null}.
      </p>
      <Button variant="ghost" onClick={clearFilters}>
        Clear filters
      </Button>
    </div>
  ) : (
    "no models for this provider"
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
        onConfirm={() => {
          if (deleting) remove.mutate(deleting);
          setDeleting(null);
        }}
        onClose={() => setDeleting(null)}
      />

      <PageHeader
        title="Models"
        subtitle="Every model this gateway can route, with its limits, price, and capabilities."
        actions={
          <>
            <div className="w-[190px]">
              <Select
                ariaLabel="provider filter"
                value={provider}
                onChange={(next) => setFilters({ provider: next, page: "1" })}
              >
                <option value={ALL}>all providers</option>
                {(providers.data ?? []).map((p) => (
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

      {!models.isLoading && all.length === 0 ? (
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
              maxBodyHeight="clamp(18rem, calc(100vh - 21rem), 52rem)"
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
          providers={providers.data}
          defaultProvider={editing.provider}
          initial={editing}
          onClose={() => setEditing(null)}
          onSaved={invalidateRegistry}
        />
      ) : null}

      {customOpen ? (
        <CustomModelModal
          providers={providers.data}
          defaultProvider={provider !== ALL ? provider : ""}
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
}: {
  model: Model;
  pendingDelete: boolean;
  onEdit: () => void;
  onDelete: () => void;
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
}) {
  return (
    <ul aria-label="Models registry" className="flex flex-col">
      {rows.map((m) => (
        <li
          key={m.ref}
          className="border-b border-[var(--color-rule)] px-4 py-3 last:border-b-0"
        >
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
                  />
                </div>
              ) : null}
            </div>
          </div>
        </li>
      ))}
    </ul>
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
