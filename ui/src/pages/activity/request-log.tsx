import type { ReactNode } from "react";
import { Badge, Button, CopyButton, StatusPill, cx, statusText } from "../../components/ui";
import { compact, ms, splitRef, when } from "../../lib/format";
import type { UsageEvent } from "../../lib/api";

/** Master/detail for one request: what it cost, how long it took, where it went. */

export function RequestLog({
  rows,
  total,
  selectedId,
  onSelect,
  showAll,
  onToggleAll,
  empty,
}: {
  rows: UsageEvent[];
  /** Rows that pass the filters (may exceed what is rendered). */
  total: number;
  selectedId: number | null;
  onSelect: (row: UsageEvent) => void;
  showAll: boolean;
  onToggleAll: () => void;
  empty: ReactNode;
}) {
  const visible = showAll ? rows : rows.slice(0, 8);

  if (rows.length === 0) {
    return <div className="px-6 py-10 text-center text-[13px] text-faint">{empty}</div>;
  }

  return (
    <div className="min-w-0">
      <div className="overflow-x-auto">
        <div role="table" aria-label="request log" className="min-w-[560px]">
          <div
            role="row"
            className="grid grid-cols-[86px_minmax(0,1fr)_96px_76px_54px_82px] items-center gap-2 border-b border-line px-6 py-2 text-[11px] uppercase tracking-[0.06em] text-faint"
          >
            <span role="columnheader">Time</span>
            <span role="columnheader">Model</span>
            <span role="columnheader" className="text-right">
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
            return (
              <div
                key={row.id ?? `${row.ts}-${row.model}`}
                role="row"
                className={cx(
                  "grid grid-cols-[86px_minmax(0,1fr)_96px_76px_54px_82px] items-center gap-2 border-b border-[var(--color-rule)] px-6 py-2.5 text-[13px] last:border-0",
                  selected ? "bg-[var(--color-surface-2)]" : "hover:bg-[var(--color-row-hover)]",
                )}
              >
                <span role="cell">
                  <button
                    type="button"
                    aria-pressed={selected}
                    onClick={() => onSelect(row)}
                    className="font-mono text-[12px] text-muted hover:text-fg"
                    title={when(row.ts)}
                  >
                    {new Date(row.ts * 1000).toLocaleTimeString(undefined, {
                      hour: "2-digit",
                      minute: "2-digit",
                      second: "2-digit",
                    })}
                  </button>
                </span>
                <span role="cell" className="flex min-w-0 items-center gap-2">
                  <span className="min-w-0 truncate font-mono text-[12px]">
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
                <span role="cell" className="text-right font-mono text-[12px] text-muted tnum">
                  {failed ? "—" : `${compact(row.input_tokens)} → ${compact(row.output_tokens)}`}
                </span>
                <span role="cell" className="text-right font-mono text-[12px] text-muted tnum">
                  {ms(row.latency_ms)}
                </span>
                <span role="cell" className="text-[11.5px] text-faint">
                  {failed ? "—" : cacheLabel(row)}
                </span>
                <span role="cell">
                  <button type="button" onClick={() => onSelect(row)} className="block">
                    <Badge tone={failed ? "neg" : "pos"} title={row.error ?? undefined}>
                      <span className="font-mono">{row.status || "err"}</span>
                    </Badge>
                  </button>
                </span>
              </div>
            );
          })}
        </div>
      </div>
      <div className="flex flex-wrap items-center justify-between gap-2 border-t border-line px-6 py-2.5 text-[12px] text-faint">
        <span>
          Showing {visible.length} of {total}
        </span>
        {total > 8 ? (
          <Button variant="quiet" onClick={onToggleAll}>
            {showAll ? "Show fewer" : `Show all ${total}`}
          </Button>
        ) : null}
      </div>
    </div>
  );
}

function cacheLabel(row: UsageEvent): string {
  if (row.cache_read_tokens > 0) return "hit";
  if (row.cache_write_tokens > 0) return "write";
  return "miss";
}

export function RequestDetail({
  row,
  onClose,
  baseUrl,
}: {
  row: UsageEvent;
  onClose: () => void;
  baseUrl: string;
}) {
  const failed = row.status >= 400 || row.error !== null;
  const endpoint = row.endpoint ?? "/v1/chat/completions";
  return (
    <aside aria-label="request detail" className="min-w-0">
      <div className="flex items-start gap-2 border-b border-line px-5 py-3">
        <div className="min-w-0 flex-1">
          <div className="font-mono text-[11px] text-faint">
            {row.id !== null ? `request #${row.id}` : "request"}
          </div>
          <div className="mt-1 truncate text-[15px] font-semibold">
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
          </div>
        </div>
        <button
          type="button"
          onClick={onClose}
          aria-label="close request detail"
          className="shrink-0 rounded-lg p-1 text-muted hover:bg-surface-2 hover:text-fg"
        >
          <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" aria-hidden>
            <path d="M6 6l12 12M18 6L6 18" />
          </svg>
        </button>
      </div>

      <div className="space-y-3 px-5 py-4">
        <StatusPill status={row.status} title={row.error ?? undefined} />

        {failed ? (
          <div className="rounded-lg border border-[var(--color-neg)]/30 bg-[var(--color-neg)]/10 px-3 py-2 text-[12.5px] text-[#fca5a5]">
            {row.error ?? `upstream answered ${row.status} ${statusText(row.status)}`}
          </div>
        ) : (
          <div className="text-[13px] text-muted">
            Latency <span className="font-mono text-fg tnum">{ms(row.latency_ms)}</span>
            {row.streamed ? <span className="text-faint"> · streamed</span> : null}
          </div>
        )}

        <dl className="divide-y divide-[var(--color-rule)] text-[13px]">
          <Row k="When" v={when(row.ts)} />
          <Row k="Provider" v={row.provider} />
          {row.endpoint ? <Row k="Endpoint" v={row.endpoint} mono /> : null}
          <Row k="Tokens" v={failed ? "—" : `${row.input_tokens} in · ${row.output_tokens} out`} mono />
          <Row k="Cache" v={failed ? "—" : `${row.cache_read_tokens} read · ${row.cache_write_tokens} write`} mono />
          <Row k="Connection" v={row.connection_id ?? "—"} mono />
        </dl>

        <div className="flex flex-wrap gap-2 pt-1">
          {row.id !== null ? <CopyButton value={String(row.id)} label="Copy request ID" /> : null}
          <CopyButton
            value={curlFor(baseUrl, endpoint, row.model)}
            label="Copy as cURL"
          />
        </div>
      </div>
    </aside>
  );
}

function Row({ k, v, mono }: { k: string; v: string; mono?: boolean }) {
  return (
    <div className="flex items-center justify-between gap-3 py-2">
      <dt className="text-faint">{k}</dt>
      <dd className={cx("min-w-0 truncate text-right text-muted", mono && "font-mono text-[12px]")}>{v}</dd>
    </div>
  );
}

/** A replay command for the endpoint this request used; the key stays a placeholder. */
export function curlFor(baseUrl: string, endpoint: string, model: string): string {
  const base = baseUrl.replace(/\/$/, "");
  if (endpoint === "/v1/messages") {
    return `curl -s ${base}/v1/messages -H "x-api-key: sk-rr-..." -H "content-type: application/json" -d '{"model":"${model}","max_tokens":64,"messages":[{"role":"user","content":"ping"}]}'`;
  }
  if (endpoint === "/v1/responses") {
    return `curl -s ${base}/v1/responses -H "Authorization: Bearer sk-rr-..." -H "content-type: application/json" -d '{"model":"${model}","input":"ping","store":false}'`;
  }
  if (endpoint === "/v1/embeddings") {
    return `curl -s ${base}/v1/embeddings -H "Authorization: Bearer sk-rr-..." -H "content-type: application/json" -d '{"model":"${model}","input":"ping"}'`;
  }
  return `curl -s ${base}/v1/chat/completions -H "Authorization: Bearer sk-rr-..." -H "content-type: application/json" -d '{"model":"${model}","messages":[{"role":"user","content":"ping"}]}'`;
}
