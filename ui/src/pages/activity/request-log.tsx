import { type ReactNode } from "react";
import { Badge, Button, cx } from "../../components/ui";
import { compact, ms, splitRef, usd, when } from "../../lib/format";
import type { UsageEvent } from "../../lib/api";

export {
  curlFor,
  RequestInspectorDrawer,
  RequestInspectorDrawer as RequestDetailDialog,
  type RequestInspectorDrawerProps,
} from "../../components/request-inspector-drawer";

/** Master/detail for one request: what it cost, how long it took, where it went. */

/** Columns shared by the header and every row, so they cannot drift apart. */
const GRID =
  "grid grid-cols-[86px_minmax(0,1fr)_108px_108px_112px_76px_54px_82px] items-center gap-2";
/** Rows before "Show all": enough to scan a burst of failures without a wall. */
export const LOG_PAGE = 20;

export function RequestLog({
  rows,
  total,
  selectedId,
  onSelect,
  showAll,
  onToggleAll,
  empty,
  providerOf,
  keyNames,
}: {
  rows: UsageEvent[];
  /** Rows that pass the filters (may exceed what is rendered). */
  total: number;
  selectedId: number | null;
  onSelect: (row: UsageEvent) => void;
  showAll: boolean;
  onToggleAll: () => void;
  empty: ReactNode;
  /** Connection's provider, falling back to the one recorded on the request. */
  providerOf: (row: UsageEvent) => string;
  /** Key id -> key name; ids missing from the roster print as a dash. */
  keyNames: Map<string, string>;
}) {
  const visible = showAll ? rows : rows.slice(0, LOG_PAGE);

  if (rows.length === 0) {
    return <div className="px-6 py-10 text-center text-body text-faint">{empty}</div>;
  }

  return (
    <div className="min-w-0" data-log-table>
      <div className="overflow-x-auto">
        <div role="table" aria-label="request log" className="min-w-[880px]">
          <div
            role="row"
            className={cx(
              GRID,
              "border-b border-line px-6 py-2 text-caption uppercase tracking-[0.06em] text-faint",
            )}
          >
            <span role="columnheader">Time</span>
            <span role="columnheader">Model</span>
            <span role="columnheader">Provider</span>
            <span role="columnheader" className="whitespace-nowrap">
              API key
            </span>
            <span role="columnheader" className="whitespace-nowrap text-right">
              Tokens in → out
            </span>
            <span role="columnheader" className="text-right">
              Latency
            </span>
            <span role="columnheader">Cache</span>
            <span role="columnheader">Status</span>
          </div>
          {visible.map((row) => {
            const failed = row.status >= 400 || row.error !== null;
            const selected = row.id !== null && row.id === selectedId;
            const provider = providerOf(row);
            const keyName = row.key_id ? (keyNames.get(row.key_id) ?? "—") : "—";
            return (
              <div
                key={row.id ?? `${row.ts}-${row.model}`}
                role="row"
                tabIndex={0}
                aria-selected={selected}
                onClick={() => onSelect(row)}
                onKeyDown={(event) => {
                  if (event.key === "Enter" || event.key === " ") {
                    event.preventDefault();
                    onSelect(row);
                  }
                }}
                className={cx(
                  GRID,
                  "cursor-pointer border-b border-[var(--color-rule)] px-6 py-2.5 text-body last:border-0 focus-visible:outline focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-accent-text",
                  selected
                    ? "bg-[var(--color-selected)] shadow-[inset_2px_0_0_var(--color-fg)]"
                    : "hover:bg-[var(--color-row-hover)]",
                )}
              >
                <span role="cell" className="font-mono text-small text-muted tnum" title={when(row.ts)}>
                  {new Date(row.ts * 1000).toLocaleTimeString(undefined, {
                    hour: "2-digit",
                    minute: "2-digit",
                    second: "2-digit",
                  })}
                </span>
                <span role="cell" className="flex min-w-0 items-center gap-2">
                  <span className="min-w-0 truncate font-mono text-small">
                    {(() => {
                      const { provider, id } = splitRef(row.model);
                      return provider ? (
                        <>
                          <span className="text-faint">{provider}/</span>
                          {id}
                        </>
                      ) : (
                        row.model
                      );
                    })()}
                  </span>
                </span>
                <span role="cell" className="min-w-0 truncate text-small text-muted" title={provider}>
                  {provider || "—"}
                </span>
                <span role="cell" className="min-w-0 truncate text-small text-muted" title={keyName}>
                  {keyName}
                </span>
                <span
                  role="cell"
                  className="whitespace-nowrap text-right font-mono text-small text-muted tnum"
                  title={`Cost ${usd(row.cost_micros, { dash: true })}`}
                >
                  {failed ? "—" : `${compact(row.input_tokens)} → ${compact(row.output_tokens)}`}
                </span>
                <span role="cell" className="text-right font-mono text-small text-muted tnum">
                  {ms(row.latency_ms)}
                </span>
                <span role="cell" className="text-caption text-faint">
                  {failed ? "—" : cacheLabel(row)}
                </span>
                <span role="cell">
                  <Badge tone={failed ? "neg" : "pos"} title={row.error ?? undefined}>
                    <span className="font-mono tnum">{row.status || "err"}</span>
                  </Badge>
                </span>
              </div>
            );
          })}
        </div>
      </div>
      <div className="flex flex-wrap items-center justify-between gap-2 border-t border-line px-6 py-2.5 text-small text-faint">
        <span className="tnum">
          Showing {visible.length} of {total}
        </span>
        {total > LOG_PAGE ? (
          <Button variant="quiet" onClick={onToggleAll}>
            {showAll ? "Show fewer" : `Show all ${total}`}
          </Button>
        ) : null}
      </div>
    </div>
  );
}

export function cacheLabel(row: UsageEvent): string {
  if (row.cache_read_tokens > 0) return "hit";
  if (row.cache_write_tokens > 0) return "write";
  return "miss";
}
