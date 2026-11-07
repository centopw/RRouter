import { memo, useCallback, useDeferredValue, useMemo, useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
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
  PageHeader,
  Select,
  cx,
} from "../components/ui";
import { DataTable, type Column } from "../components/table";
import { CAPABILITY_KEYS, CAPABILITY_LABELS, CapabilityChips } from "../components/capability-chips";
import { ago, compact, ms } from "../lib/format";
import { compareModels } from "../lib/models";
import { qk, useConnections, useModelProviders, useModels } from "../lib/queries";
import { api, type Capabilities, type FetchResult, type Model, type ModelProvider, type ModelTest, type SyncResult } from "../lib/api";

const ALL = "all";

export default function Models() {
  const models = useModels();
  const providers = useModelProviders();
  const connections = useConnections();
  const client = useQueryClient();

  const [provider, setProvider] = useState(ALL);
  const [query, setQuery] = useState("");
  const [connectionId, setConnectionId] = useState("");
  const [tested, setTested] = useState<Record<string, ModelTest>>({});
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

  const providerInfo = providers.data?.find((p) => p.id === provider);
  const providerConnections = useMemo(
    () =>
      (connections.data ?? []).filter((c) => provider === ALL || c.provider === provider),
    [connections.data, provider],
  );
  const fetchConnection =
    providerConnections.find((c) => c.id === connectionId) ?? providerConnections[0];

  // The provider `fetch` would actually talk to: the filter, or the chosen connection's
  // provider when the filter is "all".
  const fetchProviderId = provider === ALL ? fetchConnection?.provider : provider;
  const fetchProvider = providers.data?.find((p) => p.id === fetchProviderId);

  // Why `fetch` cannot run, phrased so the disabled control's title says what to do next.
  const fetchBlocker = (() => {
    if (!connections.data || !providers.data) return "loading connections…";
    if (provider !== ALL && !providerInfo) return `${provider} is not a provider this gateway knows`;
    if (providerConnections.length === 0)
      return provider === ALL
        ? "no connections yet; add a connection first"
        : `${provider} has no connection; add one on the connections page`;
    if (fetchProvider && !fetchProvider.listed)
      return `${fetchProvider.id} has no model list endpoint; add models with add custom model`;
    return undefined;
  })();

  const fetchUpstream = useMutation({
    mutationFn: (id: string) => api.post<FetchResult>("/api/models/fetch", { connection_id: id }),
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
    onSuccess: (result, model) => setTested((prev) => ({ ...prev, [model.ref]: result })),
    onError: (error, model) =>
      setTested((prev) => ({
        ...prev,
        [model.ref]: {
          ok: false,
          status: 0,
          latency_ms: 0,
          model: model.ref,
          error: error instanceof Error ? error.message : String(error),
        },
      })),
  });

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

  // 9k rows re-render on every keystroke otherwise; the input stays synchronous while
  // the table catches up.
  const deferredQuery = useDeferredValue(query);
  const rows = useMemo(() => {
    if (!models.data) return undefined;
    const q = deferredQuery.trim().toLowerCase();
    return all.filter(
      (m) =>
        (provider === ALL || m.provider === provider) &&
        (!q || m.ref.toLowerCase().includes(q) || (m.display ?? "").toLowerCase().includes(q)),
    );
  }, [all, models.data, provider, deferredQuery]);

  // Pending ids rather than whole mutation objects: the column list must keep the same
  // identity while the toolbar is being typed into, or every keystroke re-renders 9k rows.
  const testTarget = test.isPending ? (test.variables?.ref ?? null) : null;
  const toggleTarget = toggle.isPending ? (toggle.variables?.id ?? null) : null;
  const removeTarget = remove.isPending ? (remove.variables?.ref ?? null) : null;
  const [deleting, setDeleting] = useState<Model | null>(null);

  const columns = useMemo<Array<Column<Model>>>(() => [
    {
      key: "provider",
      title: "provider",
      width: "8.5rem",
      render: (m) => <span className="text-muted">{m.provider}</span>,
    },
    {
      key: "model",
      title: "model",
      render: (m) => (
        <div className="min-w-[190px] max-w-[300px]">
          <div className="flex min-w-0 items-center gap-1.5">
            <span
              title={m.ref}
              className={cx(
                "truncate font-mono text-[12px]",
                m.enabled ? "text-fg" : "text-faint",
              )}
            >
              {m.ref}
            </span>
            {m.deprecated ? (
              <Badge tone="warn" title="this family is retired by its provider">
                deprecated
              </Badge>
            ) : null}
          </div>
          {m.display && m.display !== m.id ? (
            <div className="mt-0.5 truncate text-[11px] text-faint">{m.display}</div>
          ) : null}
        </div>
      ),
    },
    {
      key: "context",
      title: "context",
      align: "right",
      width: "5rem",
      render: (m) => <span className="text-muted">{compact(m.context)}</span>,
    },
    {
      key: "max_output",
      title: "max\u00A0out",
      align: "right",
      width: "6.5rem",
      render: (m) => <span className="text-muted">{compact(m.max_output)}</span>,
    },
    {
      key: "cost",
      title: "price\u00A0/1M",
      align: "right",
      width: "8rem",
      render: (m) =>
        m.cost ? (
          <span
            className="whitespace-nowrap text-muted tnum"
            title={
              "input $" +
              m.cost.input +
              " · output $" +
              m.cost.output +
              (m.cost.cache_read === undefined ? "" : ` · cache read $${m.cost.cache_read}`)
            }
          >
            ${m.cost.input.toFixed(2)} / ${m.cost.output.toFixed(2)}
          </span>
        ) : (
          <span className="text-faint">—</span>
        ),
    },
    {
      key: "source",
      title: "source",
      width: "6rem",
      render: (m) => <Badge tone={m.source === "custom" ? "accent" : "neutral"}>{m.source}</Badge>,
    },
    {
      key: "capabilities",
      title: "capabilities",
      width: "15rem",
      render: (m) => <CapabilityChips capabilities={m.capabilities} />,
    },
    {
      key: "enabled",
      title: "enabled",
      width: "5rem",
      render: (m) => {
        const pending = toggleTarget === m.id;
        return (
          <button
            type="button"
            role="switch"
            aria-checked={m.enabled}
            aria-label={`${m.ref} ${m.enabled ? "enabled" : "disabled"}`}
            title={m.enabled ? "disable this model" : "enable this model"}
            disabled={pending}
            onClick={() => toggle.mutate(m)}
            className={cx(
              "relative inline-flex h-4 w-7 items-center rounded-full border transition-colors disabled:opacity-40",
              m.enabled ? "border-transparent bg-accent" : "border-line bg-surface-2",
            )}
          >
            <span
              aria-hidden
              className={cx(
                "absolute h-2.5 w-2.5 rounded-full transition-all",
                m.enabled ? "left-[13px] bg-white" : "left-[3px] bg-[var(--color-muted)]",
              )}
            />
          </button>
        );
      },
    },
    {
      key: "actions",
      title: "",
      align: "right",
      render: (m) => {
        const pending = testTarget === m.ref;
        const result = tested[m.ref];
        const detail = result
          ? [result.reply ?? result.error ?? "no reply", `${result.latency_ms}ms`].join(" · ")
          : undefined;
        return (
          <div className="flex flex-col items-end gap-1">
            <div className="flex items-center gap-1.5">
              <Button
                variant="quiet"
                onClick={() => test.mutate(m)}
                disabled={pending}
                title={`send a 16-token completion to ${m.ref}`}
              >
                {pending ? "testing…" : "test"}
              </Button>
              {m.source === "custom" ? (
                <Button
                  variant="danger"
                  onClick={() => setDeleting(m)}
                  disabled={removeTarget === m.ref}
                  title="delete this custom row"
                >
                  delete
                </Button>
              ) : null}
            </div>
            {result ? (
              <span
                title={detail}
                className={cx(
                  "block max-w-[200px] truncate font-mono text-[11px] tnum",
                  result.ok ? "text-pos" : "text-neg",
                )}
              >
                {result.ok ? "ok" : "err"} · {result.status} · {ms(result.latency_ms)} ·{" "}
                {result.reply ?? result.error ?? "no reply"}
              </span>
            ) : null}
          </div>
        );
      },
    },
  ], [tested, testTarget, toggleTarget, removeTarget]);

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
                onChange={(v) => {
                  setProvider(v);
                  setConnectionId("");
                }}
              >
                <option value={ALL}>all providers</option>
                {(providers.data ?? []).map((p) => (
                  <option key={p.id} value={p.id}>
                    {p.display}
                  </option>
                ))}
              </Select>
            </div>
            <div className="w-[180px]">
              <Input
                value={query}
                onChange={setQuery}
                placeholder="filter models…"
                ariaLabel="filter models"
              />
            </div>
            <Button
              onClick={() => sync.mutate()}
              disabled={sync.isPending}
              title="refresh context, capabilities, and pricing from models.dev (does not add models)"
            >
              {sync.isPending ? "syncing…" : "sync metadata"}
            </Button>
            <span title={fetchBlocker} className="inline-flex w-[200px]">
              <Select
                ariaLabel="fetch connection"
                value={fetchConnection?.id ?? ""}
                onChange={setConnectionId}
                disabled={providerConnections.length === 0}
              >
                {providerConnections.length === 0 ? (
                  <option value="">no connection</option>
                ) : (
                  providerConnections.map((c) => (
                    <option key={c.id} value={c.id}>
                      {c.provider} · {c.email ?? c.name ?? c.id.slice(0, 8)}
                    </option>
                  ))
                )}
              </Select>
            </span>
            <Button
              onClick={() => fetchConnection && fetchUpstream.mutate(fetchConnection.id)}
              disabled={!fetchConnection || fetchUpstream.isPending || Boolean(fetchBlocker)}
              title={fetchBlocker ?? "store this connection's own model list as upstream rows"}
            >
              {fetchUpstream.isPending ? "fetching…" : "fetch"}
            </Button>
            <Button variant="primary" onClick={() => setCustomOpen(true)}>
              add custom model
            </Button>
          </>
        }
      />

      {sync.data ? (
        <p className="mb-2 text-[12px] text-muted">
          synced <span className="tnum text-fg">{sync.data.models}</span> models over{" "}
          <span className="tnum text-fg">{sync.data.providers}</span> providers{" "}
          <span className="text-faint">· {ago(sync.data.synced_at)}</span>
        </p>
      ) : null}
      {fetchUpstream.data ? (
        <p className="mb-2 text-[12px] text-muted">
          <span className="tnum text-fg">{fetchUpstream.data.added}</span> new of{" "}
          <span className="tnum text-fg">{fetchUpstream.data.count}</span> listed by{" "}
          {fetchUpstream.data.provider}
        </p>
      ) : null}

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
      {sync.isError || fetchUpstream.isError || toggle.isError || remove.isError ? (
        <div className="mb-3">
          <ErrorNote error={sync.error ?? fetchUpstream.error ?? toggle.error ?? remove.error} />
        </div>
      ) : null}

      {!models.isLoading && all.length === 0 ? (
        <EmptyRegistry onSync={() => sync.mutate()} syncing={sync.isPending} />
      ) : (
        <Card>
          <CardHeader
            title="Registry"
            action={
              <span className="text-[11px] text-faint tnum">
                {rows ? `${rows.length} of ${all.length} models` : "loading…"}
              </span>
            }
          />
          <RegistryTable
            columns={columns}
            rows={rows}
            empty={
              deferredQuery || provider !== ALL ? "no models match this filter" : "no models for this provider"
            }
            loading={models.isLoading}
          />
        </Card>
      )}

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

/** The one free-text model field in the app. Mounted only while open, so typing in it
 *  never re-renders the registry table behind the dialog. */
function CustomModelModal({
  providers,
  defaultProvider,
  onClose,
  onSaved,
}: {
  providers: ModelProvider[] | undefined;
  defaultProvider: string;
  onClose: () => void;
  onSaved: () => void;
}) {
  const [form, setForm] = useState({
    provider: defaultProvider || providers?.[0]?.id || "",
    id: "",
    display: "",
    context: "",
    maxOutput: "",
  });
  const [caps, setCaps] = useState<Partial<Record<keyof Capabilities, boolean>>>({});

  const create = useMutation({
    mutationFn: () =>
      api.post<Model>("/api/models/custom", {
        provider: form.provider,
        id: form.id.trim(),
        display: form.display.trim() || null,
        context: numeric(form.context),
        max_output: numeric(form.maxOutput),
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
      title="add custom model"
      open
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose}>cancel</Button>
          <Button variant="primary" disabled={!ready} onClick={() => create.mutate()}>
            {create.isPending ? "adding…" : "add model"}
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
        <div className="flex min-w-0 flex-col gap-1.5">
          <span className="text-[11px] font-medium uppercase tracking-[0.08em] text-faint">
            capabilities
          </span>
          <div className="grid grid-cols-3 gap-x-3 gap-y-1.5">
            {CAPABILITY_KEYS.map((key) => (
              <label key={key} className="flex items-center gap-2 text-[12px] text-muted">
                <input
                  type="checkbox"
                  checked={caps[key] === true}
                  onChange={(e) => setCaps((c) => ({ ...c, [key]: e.target.checked }))}
                  className="h-3.5 w-3.5 rounded border border-line-strong bg-[var(--color-surface-2)] accent-[var(--color-accent)]"
                />
                {CAPABILITY_LABELS[key].chip}
              </label>
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
function EmptyRegistry({ onSync, syncing }: { onSync: () => void; syncing: boolean }) {
  return (
    <Card className="px-6 py-12 text-center">
      <h2 className="text-[16px] font-medium">No models yet</h2>
      <p className="mx-auto mt-1.5 max-w-[440px] text-[13px] text-muted">
        Models are listed from the providers you connected — add one under{" "}
        <a href="/app/providers" className="underline underline-offset-2">
          Providers
        </a>
        . Metadata such as context window, capabilities, and price is filled in from models.dev when you sync.
      </p>
      <div className="mt-5 flex justify-center gap-2">
        <Button variant="primary" onClick={onSync} disabled={syncing}>
          {syncing ? "syncing…" : "sync metadata"}
        </Button>
        <a
          href="/app/providers"
          className="inline-flex h-8 items-center rounded-lg border border-line-strong bg-surface-2 px-3 text-[13px] text-fg hover:border-[var(--color-line-strong)]"
        >
          go to providers
        </a>
      </div>
    </Card>
  );
}

/** The table is the expensive part: memoized so toolbar edits never reconcile 9k rows. */
const RegistryTable = memo(function RegistryTable({
  columns,
  rows,
  empty,
  loading,
}: {
  columns: Array<Column<Model>>;
  rows: Model[] | undefined;
  empty: string;
  loading: boolean;
}) {
  return <DataTable columns={columns} rows={rows} rowKey={rowKey} empty={empty} loading={loading} />;
});

/** "" and non-positive input mean "unknown", never zero. */
function numeric(value: string): number | null {
  const trimmed = value.trim();
  if (!trimmed) return null;
  const parsed = Number(trimmed);
  return Number.isFinite(parsed) && parsed > 0 ? Math.floor(parsed) : null;
}
