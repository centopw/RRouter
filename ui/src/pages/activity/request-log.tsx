import { useState, type ReactNode } from "react";
import { Trash } from "@phosphor-icons/react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { Badge, ConfirmDialog, cx } from "../../components/ui";
import { DataTable, PaginationBar, type Column } from "../../components/table";
import { compact, ms, usd, when } from "../../lib/format";
import { ApiError, api, type UsageEvent } from "../../lib/api";

export {
  curlFor,
  RequestInspectorDrawer,
  RequestInspectorDrawer as RequestDetailDialog,
  type RequestInspectorDrawerProps,
} from "../../components/request-inspector-drawer";

/** Master/detail for one request: what it cost, how long it took, where it went. */

/**
 * One log line: the unmutated usage event plus the two separate routing display fields
 * (`requested` verbatim from the client, `routedTo` resolved `provider/model`) and an
 * optional neutral `sameAs` canonical ref when the requested spelling differs.
 */
export type LogRow = UsageEvent & {
  requested: string;
  routedTo: string;
  sameAs: string | null;
};

/** Page sizes the table footer offers, smallest first. */
export const LOG_SIZES = [10, 25, 50, 100];

export function RequestLog({
  rows,
  page,
  size,
  onPage,
  onSize,
  selectedId,
  onSelect,
  loading,
  empty,
  providerOf,
  keyNames,
  canDelete = false,
  onDeleted,
}: {
  /** Every row that passes the filters; the table pages them. */
  rows: LogRow[];
  page: number;
  size: number;
  onPage: (page: number) => void;
  onSize: (size: number) => void;
  selectedId: number | null;
  onSelect: (row: LogRow) => void;
  loading?: boolean;
  empty: ReactNode;
  /** Connection's provider, falling back to the one recorded on the request. */
  providerOf: (row: UsageEvent) => string;
  /** Key id -> key name; ids missing from the roster print as a dash. */
  keyNames: Map<string, string>;
  /** Deletion is an admin action: a reader gets no per-row action at all. */
  canDelete?: boolean;
  /** Lets the page close the inspector when its request is gone. */
  onDeleted?: (row: LogRow) => void;
}) {
  const queryClient = useQueryClient();
  const [pending, setPending] = useState<LogRow | null>(null);
  const [failure, setFailure] = useState<unknown>(null);
  const [notice, setNotice] = useState<string | null>(null);

  // 204 is the delete; 404 is not an error here — another tab (or an earlier click) got
  // there first, and the row is gone either way.
  const remove = useMutation({
    mutationFn: async (row: LogRow) => {
      const handle = deleteHandle(row);
      if (!handle) throw new Error("this row has no id to delete");
      try {
        await api.del<void>(`/api/usage/${encodeURIComponent(handle)}`);
        return "deleted" as const;
      } catch (error) {
        if (error instanceof ApiError && error.status === 404) return "already-gone" as const;
        throw error;
      }
    },
    onSuccess: (outcome, row) => {
      setPending(null);
      setFailure(null);
      setNotice(
        outcome === "already-gone" ? "That request was already deleted — nothing left to remove." : null,
      );
      // The row is gone on the server: the list, the totals, the live card and the
      // per-account quota consumption all move.
      void queryClient.invalidateQueries({ queryKey: ["usage"] });
      void queryClient.invalidateQueries({ queryKey: ["accounts", "quotas"] });
      onDeleted?.(row);
    },
    onError: (error) => setFailure(error),
  });

  const askDelete = (row: LogRow) => {
    setFailure(null);
    setNotice(null);
    setPending(row);
  };

  const columns: Array<Column<LogRow>> = [
    {
      key: "ts",
      title: "Time",
      width: "92px",
      render: (row) => (
        <span className="whitespace-nowrap font-mono text-small text-muted tnum">
          {new Date(row.ts * 1000).toLocaleTimeString(undefined, {
            hour: "2-digit",
            minute: "2-digit",
            second: "2-digit",
          })}
        </span>
      ),
      cellTitle: (row) => when(row.ts),
    },
    {
      key: "model",
      title: "Model",
      wrap: true,
      // Literal spelling the client sent; when it differs from the canonical ref, a small
      // neutral note confirms both spellings reach the same destination.
      render: (row) => (
        <div className="min-w-0">
          <span className="block truncate font-mono text-small text-fg">{row.requested}</span>
          {row.sameAs ? (
            <span className="block truncate font-mono text-caption text-faint">
              same as {row.sameAs}
            </span>
          ) : null}
        </div>
      ),
      cellTitle: (row) => (row.sameAs ? `${row.requested} (same as ${row.sameAs})` : row.requested),
    },
    {
      key: "routed",
      title: "Routed to",
      width: "210px",
      render: (row) => (
        <span className="block truncate font-mono text-small text-muted">
          {row.routedTo || "—"}
        </span>
      ),
      cellTitle: (row) => row.routedTo || "—",
    },
    {
      key: "provider",
      title: "Provider",
      width: "128px",
      className: "hidden xl:table-cell",
      render: (row) => <span className="text-small text-muted">{providerOf(row) || "—"}</span>,
      cellTitle: (row) => providerOf(row) || "—",
    },
    {
      key: "key",
      title: "API key",
      width: "136px",
      className: "hidden lg:table-cell",
      render: (row) => (
        <span className="text-small text-muted">
          {row.key_id ? (keyNames.get(row.key_id) ?? "—") : "—"}
        </span>
      ),
      cellTitle: (row) => (row.key_id ? (keyNames.get(row.key_id) ?? "—") : "—"),
    },
    {
      key: "tokens",
      title: "Tokens in → out",
      align: "right",
      width: "120px",
      render: (row) => {
        const failed = row.status >= 400 || row.error !== null;
        return (
          <span className="whitespace-nowrap font-mono text-small text-muted tnum">
            {failed ? "—" : `${compact(row.input_tokens)} → ${compact(row.output_tokens)}`}
          </span>
        );
      },
      cellTitle: (row) =>
        row.status >= 400 || row.error !== null
          ? "no usage recorded for a failed request"
          : `${row.input_tokens} in → ${row.output_tokens} out tokens · cost ${usd(row.cost_micros, { dash: true })}`,
    },
    {
      key: "latency",
      title: "Latency",
      align: "right",
      width: "84px",
      render: (row) => (
        <span className="whitespace-nowrap font-mono text-small text-muted tnum">
          {ms(row.latency_ms)}
        </span>
      ),
    },
    {
      key: "cache",
      title: "Cache",
      width: "72px",
      className: "hidden md:table-cell",
      render: (row) => (
        <span className="text-caption text-faint">
          {row.status >= 400 || row.error !== null ? "—" : cacheLabel(row)}
        </span>
      ),
    },
    {
      key: "status",
      title: "Status",
      width: "80px",
      render: (row) => {
        const failed = row.status >= 400 || row.error !== null;
        return (
          <Badge tone={failed ? "neg" : "pos"}>
            <span className="font-mono tnum">{row.status || "err"}</span>
          </Badge>
        );
      },
      cellTitle: (row) => row.error ?? undefined,
    },
  ];

  if (canDelete) {
    columns.push({
      key: "actions",
      title: "Actions",
      srOnlyTitle: true,
      align: "right",
      width: "52px",
      render: (row) =>
        deleteHandle(row) ? (
          <button
            type="button"
            onClick={() => askDelete(row)}
            aria-label={`Delete request ${shortRequest(row)}`}
            title={`Delete request ${shortRequest(row)}`}
            className="inline-flex h-7 w-7 items-center justify-center rounded-[var(--radius-input)] text-faint transition-colors hover:text-fg focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent-text"
          >
            <Trash size={15} aria-hidden />
          </button>
        ) : null,
    });
  }

  // Controlled pagination: the table renders exactly what it is given, so the page's
  // slice is cut here and the footer keeps the full count.
  const pageRows = rows.slice((page - 1) * size, page * size);

  return (
    <>
      {notice ? (
        <div
          role="status"
          className="flex items-center gap-2 border-b border-line px-4 py-2 text-small text-muted"
        >
          <span className="min-w-0 truncate">{notice}</span>
          <button
            type="button"
            onClick={() => setNotice(null)}
            aria-label="Dismiss notice"
            className="ml-auto rounded-[var(--radius-input)] px-1 text-faint transition-colors hover:text-fg"
          >
            ×
          </button>
        </div>
      ) : null}

      {/* Mobile stacked view (< 640px): keeps requested ref, neutral same-as note, and
          Routed to readable without horizontal scrolling. */}
      {!loading && pageRows.length > 0 ? (
        <div className="sm:hidden">
          <ul aria-label="request log" className="divide-y divide-[var(--color-rule)]">
            {pageRows.map((row) => {
              const failed = row.status >= 400 || row.error !== null;
              const selected = row.id !== null && row.id === selectedId;
              const timeText = new Date(row.ts * 1000).toLocaleTimeString(undefined, {
                hour: "2-digit",
                minute: "2-digit",
                second: "2-digit",
              });
              return (
                <li key={rowKey(row)}>
                  <div
                    className={cx(
                      "flex items-start gap-2 px-4 py-3 transition-colors",
                      selected && "bg-[var(--color-selected)]",
                    )}
                  >
                    <button
                      type="button"
                      onClick={() => onSelect(row)}
                      className="min-w-0 flex-1 text-left focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent-text"
                    >
                      <div className="flex items-start justify-between gap-2">
                        <span className="min-w-0 break-all font-mono text-small text-fg">
                          {row.requested}
                        </span>
                        <Badge tone={failed ? "neg" : "pos"}>
                          <span className="font-mono tnum">{row.status || "err"}</span>
                        </Badge>
                      </div>
                      {row.sameAs ? (
                        <div className="mt-0.5 break-all font-mono text-caption text-faint">
                          same as {row.sameAs}
                        </div>
                      ) : null}
                      <div className="mt-1 flex flex-wrap items-baseline gap-x-1.5 text-caption text-muted">
                        <span className="text-faint">Routed to</span>
                        <span className="break-all font-mono">{row.routedTo || "—"}</span>
                      </div>
                      <div className="mt-1 flex flex-wrap items-center gap-x-2 gap-y-0.5 text-caption text-faint tnum">
                        <span className="font-mono">{timeText}</span>
                        <span>·</span>
                        <span>{providerOf(row) || "—"}</span>
                        <span>·</span>
                        <span className="font-mono">
                          {failed
                            ? "—"
                            : `${compact(row.input_tokens)} → ${compact(row.output_tokens)}`}
                        </span>
                        <span>·</span>
                        <span className="font-mono">{ms(row.latency_ms)}</span>
                      </div>
                    </button>
                    {canDelete && deleteHandle(row) ? (
                      <button
                        type="button"
                        onClick={() => askDelete(row)}
                        aria-label={`Delete request ${shortRequest(row)}`}
                        title={`Delete request ${shortRequest(row)}`}
                        className="mt-0.5 inline-flex h-7 w-7 shrink-0 items-center justify-center rounded-[var(--radius-input)] text-faint transition-colors hover:text-fg focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent-text"
                      >
                        <Trash size={15} aria-hidden />
                      </button>
                    ) : null}
                  </div>
                </li>
              );
            })}
          </ul>
          <PaginationBar
            pagination={{
              page,
              size,
              total: rows.length,
              sizes: LOG_SIZES,
              label: "requests",
              onPage,
              onSize,
            }}
          />
        </div>
      ) : null}

      <div className={loading || pageRows.length === 0 ? undefined : "hidden sm:block"}>
        <DataTable
          columns={columns}
          rows={pageRows}
          rowKey={rowKey}
          rowHeaderKey="model"
          onRowClick={onSelect}
          selectedKey={selectedId ?? undefined}
          scrollLabel="request log"
          tableLayout="fixed"
          loading={loading}
          empty={empty}
          pagination={{
            page,
            size,
            total: rows.length,
            sizes: LOG_SIZES,
            label: "requests",
            onPage,
            onSize,
          }}
        />
      </div>

      {pending ? (
        <ConfirmDialog
          open
          title={`Delete request ${shortRequest(pending)}?`}
          confirmLabel="Delete request"
          busy={remove.isPending}
          error={failure}
          onClose={() => {
            if (remove.isPending) return;
            setPending(null);
            setFailure(null);
          }}
          onConfirm={() => remove.mutate(pending)}
          message={
            <>
              <p className="tnum">
                {when(pending.ts)} · <span className="font-mono">{pending.requested}</span> · status{" "}
                <span className="font-mono">{pending.status || "err"}</span>
              </p>
              <p className="mt-2">
                Deleting this request also deletes its captured request and response bodies and
                every recorded attempt, in one transaction. It cannot be undone.
              </p>
            </>
          }
        />
      ) : null}
    </>
  );
}

/** The usage row id is the exact row; the request id is the fallback for an id-less row. */
function deleteHandle(row: LogRow): string {
  if (row.id !== null) return String(row.id);
  return row.request_id || "";
}

/** Short handle for the action's accessible name: the request id's first 8 characters. */
function shortRequest(row: LogRow): string {
  if (row.request_id) return row.request_id.slice(0, 8);
  return row.id !== null ? `#${row.id}` : "unknown";
}

/** A usage row id is stable; the ts+model pair only covers rows the server sent without one. */
function rowKey(row: LogRow): string {
  return row.id !== null ? String(row.id) : `${row.ts}-${row.requested}`;
}

export function cacheLabel(row: UsageEvent): string {
  if (row.cache_read_tokens > 0) return "hit";
  if (row.cache_write_tokens > 0) return "write";
  return "miss";
}
