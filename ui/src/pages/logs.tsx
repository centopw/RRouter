import { useMemo, useState } from "react";
import { useSearchParams } from "react-router-dom";
import { Badge, Button, Card, EmptyState, ErrorNote, PageHeader, Segmented, Select, cx } from "../components/ui";
import { Divider } from "../components/atoms";
import { ago, compact, ms, splitRef } from "../lib/format";
import {
  usageFilterQuery,
  useConnections,
  useKeys,
  useModels,
  useUsagePage,
  type UsageFilters,
} from "../lib/queries";
import { download, saveBlob, type Model, type UsageEvent } from "../lib/api";
import { cacheLabel, RequestDetailDialog } from "./activity/request-log";

/** Incident review: every request newest first, filtered on the server, paged backwards on demand. */

const ALL = "all";

const RANGES = [
  { id: "1h", label: "1h", hours: 1 },
  { id: "24h", label: "24h", hours: 24 },
  { id: "7d", label: "7d", hours: 168 },
  { id: "all", label: "all", hours: null },
] as const;

type RangeId = (typeof RANGES)[number]["id"];
type Status = "all" | "errors" | "2xx";

export default function Logs() {
  // The Activity incident banner links here with the failing model preselected. Read
  // once: these are landing filters, and the selects stay local from then on.
  const [params, setParams] = useSearchParams();
  const [status, setStatus] = useState<Status>(() => {
    const value = params.get("status");
    return value === "error" || value === "errors" ? "errors" : value === "2xx" ? "2xx" : "all";
  });
  const [provider, setProvider] = useState(() => params.get("provider") || ALL);
  const [model, setModel] = useState(() => params.get("model") || ALL);
  const [keyId, setKeyId] = useState(ALL);
  const [rangeId, setRangeId] = useState<RangeId>(() => {
    const value = params.get("range");
    return RANGES.some((range) => range.id === value) ? (value as RangeId) : "24h";
  });
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [exporting, setExporting] = useState(false);
  const [exportError, setExportError] = useState<string | null>(null);

  const range = RANGES.find((r) => r.id === rangeId) ?? RANGES[1];
  // Every filter reaches the server and is part of the query key: a change restarts from page one.
  const filters: UsageFilters = {
    hours: range.hours,
    status: status === "all" ? undefined : status === "errors" ? "error" : "2xx",
    provider: provider === ALL ? undefined : provider,
    model: model === ALL ? undefined : model,
    keyId: keyId === ALL ? undefined : keyId,
  };
  const usage = useUsagePage(filters);
  const connections = useConnections();
  const models = useModels();
  const keys = useKeys();

  const loaded = useMemo(() => {
    const byKey = new Map<string, UsageEvent>();
    for (const page of usage.data?.pages ?? []) {
      for (const row of page.data) {
        const key = rowKey(row);
        if (!byKey.has(key)) byKey.set(key, row);
      }
    }
    return [...byKey.values()];
  }, [usage.data]);

  // Selection is by request id, so it survives refetches and paging.
  const selected = selectedId === null ? null : (loaded.find((r) => rowKey(r) === selectedId) ?? null);

  const keyNames = useMemo(
    () => new Map((keys.data ?? []).map((k) => [k.id, k.name ?? "—"] as const)),
    [keys.data],
  );

  const providers = useMemo(
    () => [...new Set((connections.data ?? []).map((c) => c.provider))].sort(),
    [connections.data],
  );

  const modelGroups = useMemo(() => {
    const groups = new Map<string, Model[]>();
    for (const m of models.data ?? []) {
      const list = groups.get(m.provider) ?? [];
      list.push(m);
      groups.set(m.provider, list);
    }
    return [...groups.entries()].sort(([a], [b]) => a.localeCompare(b));
  }, [models.data]);
  // A deep-linked incident can name a model or provider the catalogue does not list
  // (an unknown id, or a connection that was removed). Keep its value visible and
  // filterable instead of silently snapping the select to "All".
  const modelListed = model === ALL || modelGroups.some(([, list]) => list.some((m) => m.ref === model));
  const providerListed = provider === ALL || providers.includes(provider);

  const filtersActive = status !== "all" || provider !== ALL || model !== ALL || keyId !== ALL;
  const baseUrl = `${window.location.origin}/v1`;

  const dirty =
    status !== "all" || provider !== ALL || model !== ALL || keyId !== ALL || rangeId !== "24h";

  const clearFilters = () => {
    setStatus("all");
    setProvider(ALL);
    setModel(ALL);
    setKeyId(ALL);
    // The landing deep link is a starting point, not a permanent state: once the
    // operator clears it, a refresh must not bring the incident filter back.
    if (params.size > 0) setParams({}, { replace: true });
  };

  // Same filters as the table, without its cursor: the server streams every matching row.
  const exportCsv = async () => {
    setExporting(true);
    setExportError(null);
    try {
      const query = usageFilterQuery(filters);
      const { blob, filename } = await download(`/api/usage/export.csv?${query}`);
      const now = new Date();
      const stamp = `${now.getFullYear()}-${String(now.getMonth() + 1).padStart(2, "0")}-${String(
        now.getDate(),
      ).padStart(2, "0")}`;
      saveBlob(blob, filename ?? `rrouter-usage-${stamp}.csv`);
    } catch (error) {
      setExportError(error instanceof Error ? error.message : String(error));
    } finally {
      setExporting(false);
    }
  };

  return (
    <div className="mx-auto max-w-[1160px] max-[820px]:-mx-4">
      <PageHeader
        title="Logs"
        subtitle="Every request, newest first. Filter, select one to inspect it, load older on demand."
        actions={
          <>
            <Button variant="quiet" onClick={() => void exportCsv()} disabled={exporting}>
              {exporting ? "Exporting…" : "Export CSV"}
            </Button>
            <Button variant="quiet" onClick={() => void usage.refetch()} disabled={usage.isFetching}>
              {usage.isFetching ? "Refreshing…" : "Refresh"}
            </Button>
          </>
        }
      />

      {exportError ? (
        <div className="mb-4">
          <ErrorNote error={exportError} />
        </div>
      ) : null}

      <Card>
        {/* The four selects are one row from 900px up. The segmented control only shares
          that row (and the loaded/clear meta joins it) from 1100px, where four columns
          still fit their labels instead of collapsing to "All …". */}
        <div className="flex flex-col gap-2 border-b border-line px-5 py-3 min-[1100px]:flex-row min-[1100px]:items-center">
          <Segmented
            options={RANGES.map((r) => ({ id: r.id, label: r.label }))}
            value={rangeId}
            onChange={setRangeId}
            ariaLabel="time range"
            className="shrink-0 self-start min-[1100px]:self-center"
          />
          <Divider vertical className="hidden min-[1100px]:block" />
          {/* Four columns from 900px: the selects share the row instead of wrapping
              ("All keys" used to drop to a second line on a narrow desktop). Below 900
              two columns stay readable; each cell truncates its own label. */}
          <div className="grid min-w-0 flex-1 grid-cols-2 gap-2 min-[900px]:grid-cols-4">
            <Select className="w-full min-w-0" value={status} onChange={(v) => setStatus(v as Status)} ariaLabel="status">
              <option value="all">All statuses</option>
              <option value="errors">Errors</option>
              <option value="2xx">2xx only</option>
            </Select>
            <Select className="w-full min-w-0" value={provider} onChange={setProvider} ariaLabel="provider">
              <option value={ALL}>All providers</option>
              {!providerListed ? <option value={provider}>{provider}</option> : null}
              {providers.map((p) => (
                <option key={p} value={p}>
                  {p}
                </option>
              ))}
            </Select>
            <Select className="w-full min-w-0" value={model} onChange={setModel} ariaLabel="model">
              <option value={ALL}>All models</option>
              {!modelListed ? <option value={model}>{model}</option> : null}
              {modelGroups.map(([p, list]) => (
                <optgroup key={p} label={p}>
                  {list.map((m) => (
                    <option key={m.ref} value={m.ref}>
                      {m.display ?? m.id}
                    </option>
                  ))}
                </optgroup>
              ))}
            </Select>
            <Select className="w-full min-w-0" value={keyId} onChange={setKeyId} ariaLabel="key">
              <option value={ALL}>All keys</option>
              {(keys.data ?? []).map((k) => (
                <option key={k.id} value={k.id}>
                  {k.name ?? k.id}
                </option>
              ))}
            </Select>
          </div>
          <span className="flex shrink-0 items-center gap-2 text-caption text-faint tnum min-[1100px]:text-right">
            {dirty ? (
              <Button variant="quiet" onClick={clearFilters}>
                clear filters
              </Button>
            ) : null}
            <span>{loaded.length.toLocaleString()} loaded</span>
          </span>
        </div>

        <div className="border-t border-line">
          {usage.isPending ? (
            <div aria-hidden className="space-y-2 px-6 py-4">
              {[0, 1, 2].map((i) => (
                <div key={i} className="h-4 rounded bg-[var(--color-surface-2)]" />
              ))}
            </div>
          ) : usage.isError && loaded.length === 0 ? (
            <div className="px-5 py-4">
              <ErrorNote error={usage.error} onRetry={() => void usage.refetch()} />
            </div>
          ) : loaded.length === 0 ? (
            <EmptyState>
              {filtersActive ? (
                <>
                  <div>No requests match these filters.</div>
                  <div className="mt-3">
                    <Button variant="quiet" onClick={clearFilters}>
                      Clear filters
                    </Button>
                  </div>
                </>
              ) : (
                <>
                  <div>No requests in this window.</div>
                  {rangeId !== "all" ? (
                    <div className="mt-3">
                      <Button variant="quiet" onClick={() => setRangeId("all")}>
                        Show all time
                      </Button>
                    </div>
                  ) : null}
                </>
              )}
            </EmptyState>
          ) : (
            <div className="pb-3">
              <div>
                {usage.isError ? (
                  <div className="px-6 pt-3">
                    <ErrorNote error={usage.error} onRetry={() => void usage.refetch()} />
                  </div>
                ) : null}
                <LogTable
                  rows={loaded}
                  selectedId={selectedId}
                  onSelect={setSelectedId}
                  keyNames={keyNames}
                />
                <div className="flex flex-wrap items-center justify-between gap-2 border-t border-line px-6 py-2.5 text-small text-faint">
                  <span>{loaded.length} loaded</span>
                  {usage.hasNextPage ? (
                    <Button variant="quiet" onClick={() => void usage.fetchNextPage()} disabled={usage.isFetchingNextPage}>
                      {usage.isFetchingNextPage ? "Loading…" : "Load older"}
                    </Button>
                  ) : (
                    <span>No older requests</span>
                  )}
                </div>
              </div>
            </div>
          )}
        </div>
      </Card>
      {selected ? (
        <RequestDetailDialog
          row={selected}
          baseUrl={baseUrl}
          onClose={() => setSelectedId(null)}
        />
      ) : null}
    </div>
  );
}

const GRID = "grid grid-cols-[104px_minmax(0,1.3fr)_96px_72px_54px_70px_minmax(0,0.8fr)_72px] items-center gap-2";

function LogTable({
  rows,
  selectedId,
  onSelect,
  keyNames,
}: {
  rows: UsageEvent[];
  selectedId: string | null;
  onSelect: (id: string) => void;
  keyNames: Map<string, string>;
}) {
  if (rows.length === 0) return null;
  return (
    <div className="overflow-x-auto">
      <div role="table" aria-label="request logs" className="min-w-[760px]">
        <div
          role="row"
          className={cx(
            GRID,
            "border-b border-line px-6 py-2 text-caption uppercase tracking-[0.06em] text-faint",
          )}
        >
          <span role="columnheader">Time</span>
          <span role="columnheader">Provider / model</span>
          <span role="columnheader" className="text-right">
            Tokens in → out
          </span>
          <span role="columnheader" className="text-right">
            Latency
          </span>
          <span role="columnheader">Cache</span>
          <span role="columnheader">Status</span>
          <span role="columnheader">Key</span>
          <span role="columnheader">Request</span>
        </div>
        {rows.map((row) => {
          const key = rowKey(row);
          const failed = isFailed(row);
          const selected = key === selectedId;
          const { provider, id } = splitRef(refOf(row));
          return (
            <div
              key={key}
              role="row"
              tabIndex={0}
              aria-label={`open request ${key}`}
              onClick={() => onSelect(key)}
              onKeyDown={(event) => {
                if (event.key === "Enter" || event.key === " ") {
                  event.preventDefault();
                  onSelect(key);
                }
              }}
              className={cx(
                GRID,
                "cursor-pointer border-b border-[var(--color-rule)] px-6 py-2.5 text-body last:border-0",
                selected ? "bg-[var(--color-surface-2)]" : "hover:bg-[var(--color-row-hover)]",
              )}
            >
              <span role="cell" className="flex flex-col" title={new Date(row.ts * 1000).toLocaleString()}>
                <span className="font-mono text-small text-fg">
                  {new Date(row.ts * 1000).toLocaleTimeString(undefined, {
                    hour: "2-digit",
                    minute: "2-digit",
                    second: "2-digit",
                  })}
                </span>
                <span className="text-caption text-faint">{ago(row.ts)}</span>
              </span>
              <span role="cell" className="min-w-0 truncate font-mono text-small">
                <span className="text-faint">{provider ?? row.provider}/</span>
                {id}
              </span>
              <span role="cell" className="text-right font-mono text-small text-muted tnum">
                {failed ? "—" : `${compact(row.input_tokens)} → ${compact(row.output_tokens)}`}
              </span>
              <span role="cell" className="text-right font-mono text-small text-muted tnum">
                {ms(row.latency_ms)}
              </span>
              <span role="cell" className="text-caption text-faint">
                {failed ? "—" : cacheLabel(row)}
              </span>
              <span role="cell">
                <button type="button" onClick={() => onSelect(key)} className="block">
                  <Badge tone={failed ? "neg" : "pos"} title={row.error ?? undefined}>
                    <span className="font-mono">{row.status || "err"}</span>
                  </Badge>
                </button>
              </span>
              <span role="cell" className="min-w-0 truncate text-small text-muted">
                {row.key_id ? (keyNames.get(row.key_id) ?? "—") : "—"}
              </span>
              <span role="cell">
                <button
                  type="button"
                  onClick={() => onSelect(key)}
                  className="font-mono text-caption text-faint hover:text-fg"
                  title={row.request_id || undefined}
                >
                  {row.request_id ? row.request_id.slice(0, 8) : "—"}
                </button>
              </span>
            </div>
          );
        })}
      </div>
    </div>
  );
}

/** Stable across pages: the request id, falling back only for rows that lack one. */
function rowKey(row: UsageEvent): string {
  if (row.request_id) return row.request_id;
  return row.id !== null ? `#${row.id}` : `${row.ts}:${row.model}:${row.latency_ms}`;
}

function isFailed(row: UsageEvent): boolean {
  return row.status >= 400 || row.error !== null;
}

/** The registry ref (`provider/id`), the same form the model filter options use. */
function refOf(row: UsageEvent): string {
  const prefix = `${row.provider}/`;
  return row.model.startsWith(prefix) ? row.model : prefix + row.model;
}
