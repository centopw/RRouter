import { useState, type ReactNode } from "react";
import { Trash } from "@phosphor-icons/react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { Badge, ConfirmDialog } from "../../components/ui";
import { DataTable, type Column } from "../../components/table";
import { compact, ms, usd, when } from "../../lib/format";
import { ApiError, api, type UsageEvent } from "../../lib/api";

export {
  curlFor,
  RequestInspectorDrawer,
  RequestInspectorDrawer as RequestDetailDialog,
  type RequestInspectorDrawerProps,
} from "../../components/request-inspector-drawer";

/** Master/detail for one request: what it cost, how long it took, where it went. */

/** One log line: the usage row plus the canonical model ref built by `lib/model-ref`. */
export type LogRow = UsageEvent & { model: string };

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
      width: "96px",
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
      // The canonical ref already names its provider; the Provider column names the
      // account that served it.
      render: (row) => <span className="font-mono text-small">{row.model}</span>,
      cellTitle: (row) => row.model,
    },
    {
      key: "provider",
      title: "Provider",
      width: "140px",
      render: (row) => <span className="text-small text-muted">{providerOf(row) || "—"}</span>,
      cellTitle: (row) => providerOf(row) || "—",
    },
    {
      key: "key",
      title: "API key",
      width: "150px",
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
      width: "124px",
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
      width: "88px",
      render: (row) => (
        <span className="whitespace-nowrap font-mono text-small text-muted tnum">
          {ms(row.latency_ms)}
        </span>
      ),
    },
    {
      key: "cache",
      title: "Cache",
      width: "76px",
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
      width: "88px",
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
      width: "56px",
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
                {when(pending.ts)} · <span className="font-mono">{pending.model}</span> · status{" "}
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
  return row.id !== null ? String(row.id) : `${row.ts}-${row.model}`;
}

export function cacheLabel(row: UsageEvent): string {
  if (row.cache_read_tokens > 0) return "hit";
  if (row.cache_write_tokens > 0) return "write";
  return "miss";
}
