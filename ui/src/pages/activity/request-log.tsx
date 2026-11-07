import { useEffect, useRef, type ReactNode } from "react";
import { Badge, Button, CopyButton, ErrorNote, StatusPill, cx, statusText } from "../../components/ui";
import { compact, ms, splitRef, usd, when } from "../../lib/format";
import { useUsageDetail } from "../../lib/queries";
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
            className="grid grid-cols-[86px_minmax(0,1fr)_112px_76px_54px_82px] items-center gap-2 border-b border-line px-6 py-2 text-[11px] uppercase tracking-[0.06em] text-faint"
          >
            <span role="columnheader">Time</span>
            <span role="columnheader">Model</span>
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
                  "grid cursor-pointer grid-cols-[86px_minmax(0,1fr)_112px_76px_54px_82px] items-center gap-2 border-b border-[var(--color-rule)] px-6 py-2.5 text-[13px] last:border-0 focus-visible:outline focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-accent-text",
                  selected ? "bg-[var(--color-surface-2)]" : "hover:bg-[var(--color-row-hover)]",
                )}
              >
                <span role="cell" className="font-mono text-[12px] text-muted" title={when(row.ts)}>
                  {new Date(row.ts * 1000).toLocaleTimeString(undefined, {
                    hour: "2-digit",
                    minute: "2-digit",
                    second: "2-digit",
                  })}
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
                <span
                  role="cell"
                  className="whitespace-nowrap text-right font-mono text-[12px] text-muted tnum"
                  title={`Cost ${usd(row.cost_micros, { dash: true })}`}
                >
                  {failed ? "—" : `${compact(row.input_tokens)} → ${compact(row.output_tokens)}`}
                </span>
                <span role="cell" className="text-right font-mono text-[12px] text-muted tnum">
                  {ms(row.latency_ms)}
                </span>
                <span role="cell" className="text-[11.5px] text-faint">
                  {failed ? "—" : cacheLabel(row)}
                </span>
                <span role="cell">
                  <Badge tone={failed ? "neg" : "pos"} title={row.error ?? undefined}>
                    <span className="font-mono">{row.status || "err"}</span>
                  </Badge>
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

export function cacheLabel(row: UsageEvent): string {
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
  // Only streamed requests measure a first output, and only when it arrived before
  // the stream ended; otherwise the split would be an invention.
  const measured = !failed && row.ttft_ms > 0 && row.ttft_ms < row.latency_ms;
  const generation = row.latency_ms - row.ttft_ms;

  const detail = useUsageDetail(row.request_id || null);
  const { refetch } = detail;
  // Attempts land when the request finishes, so reload on the transition from the
  // in-flight status the log polled to the final one. Selecting another row is
  // already a fresh query and must not double-fetch.
  const seen = useRef({ id: row.request_id, status: row.status });
  useEffect(() => {
    const prev = seen.current;
    seen.current = { id: row.request_id, status: row.status };
    if (prev.id === row.request_id && prev.status !== row.status) void refetch();
  }, [row.request_id, row.status, refetch]);

  const attempts = detail.data?.attempts ?? [];
  // Status 0 means the attempt got no response at all, so it is a failure too.
  const failedAttempts = attempts.filter(
    (a) => a.status === 0 || a.status >= 400 || a.error !== null,
  );
  const showAttempts =
    row.request_id !== "" &&
    (detail.isPending || detail.isError || attempts.length > 1 || failedAttempts.length > 0);
  return (
    <aside aria-label="request detail" className="min-w-0">
      <div className="flex items-start gap-2 border-b border-line px-5 py-3">
        <div className="min-w-0 flex-1">
          <div
            className="truncate font-mono text-[11px] text-faint"
            title={row.request_id || undefined}
          >
            {row.request_id ? `request ${row.request_id.slice(0, 8)}` : "request"}
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
          <div className="rounded-lg border border-[var(--color-neg)]/30 bg-[var(--color-neg)]/10 px-3 py-2 text-[12.5px] text-[var(--color-neg-ink)]">
            {row.error ?? `upstream answered ${row.status} ${statusText(row.status)}`}
          </div>
        ) : (
          <>
            <div className="text-[13px] text-muted">
              Latency <span className="font-mono text-fg tnum">{ms(row.latency_ms)}</span>
              {row.streamed ? <span className="text-faint"> · streamed</span> : null}
            </div>
            {measured ? (
              <div>
                <div className="text-[12.5px] text-faint">
                  First output <span className="font-mono tnum">{ms(row.ttft_ms)}</span> · generation{" "}
                  <span className="font-mono tnum">{ms(generation)}</span>
                </div>
                <div
                  className="mt-1.5 flex h-1.5 overflow-hidden rounded-full bg-[var(--color-rule)]"
                  title="time to first output vs generation"
                >
                  <span
                    aria-hidden
                    className="bg-accent"
                    style={{ width: `${(row.ttft_ms / row.latency_ms) * 100}%` }}
                  />
                </div>
              </div>
            ) : null}
          </>
        )}

        <dl className="divide-y divide-[var(--color-rule)] text-[13px]">
          <Row k="When" v={when(row.ts)} />
          <Row k="Provider" v={row.provider} />
          {row.endpoint ? <Row k="Endpoint" v={row.endpoint} mono /> : null}
          <Row k="Tokens" v={failed ? "—" : `${row.input_tokens} in · ${row.output_tokens} out`} mono />
          <Row k="Cost" v={usd(row.cost_micros, { dash: true })} mono />
          <Row k="Cache" v={failed ? "—" : `${row.cache_read_tokens} read · ${row.cache_write_tokens} write`} mono />
          <Row k="Connection" v={row.connection_id ?? "—"} mono />
        </dl>

        {showAttempts ? (
          <section aria-label="attempts" className="space-y-2">
            <div className="flex items-baseline justify-between gap-2">
              <h3 className="text-[12px] font-medium text-muted">Attempts</h3>
              {attempts.length > 1 ? (
                <span className="text-[11.5px] text-faint">{attempts.length} in dispatch order</span>
              ) : null}
            </div>
            {detail.isError ? (
              <ErrorNote error={detail.error} onRetry={() => void detail.refetch()} />
            ) : detail.isPending ? (
              <div aria-hidden className="rounded-lg border border-line px-3 py-2.5">
                <div className="h-3 w-2/3 rounded bg-[var(--color-surface-2)]" />
              </div>
            ) : (
              <ol className="divide-y divide-[var(--color-rule)] rounded-lg border border-line">
                {attempts.map((attempt, i) => {
                  const attemptFailed = failedAttempts.includes(attempt);
                  const { provider, id } = splitRef(attempt.model);
                  const served = !attemptFailed && i === attempts.length - 1;
                  return (
                    <li key={attempt.attempt} className="px-3 py-2">
                      <div className="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1 text-[12px]">
                        <span className="font-mono text-faint tnum">#{attempt.attempt}</span>
                        <span className="min-w-0 flex-1 truncate font-mono">
                          <span className="text-faint">{provider ?? attempt.provider}/</span>
                          {id}
                        </span>
                        <span aria-hidden className="text-faint">
                          ·
                        </span>
                        <Badge
                          tone={attemptFailed ? "neg" : "pos"}
                          title={
                            attempt.status === 0
                              ? "no response: transport failure or open breaker"
                              : statusText(attempt.status)
                          }
                        >
                          <span className="font-mono">{attempt.status || "err"}</span>
                        </Badge>
                        <span aria-hidden className="text-faint">
                          ·
                        </span>
                        <span className="font-mono text-muted tnum">{ms(attempt.latency_ms)}</span>
                        {served ? <span className="text-[11px] text-faint">served</span> : null}
                      </div>
                      {attemptFailed && attempt.error ? (
                        <p className="mt-1 break-words text-[12px] leading-[1.45] text-neg">
                          {attempt.error}
                        </p>
                      ) : null}
                    </li>
                  );
                })}
              </ol>
            )}
          </section>
        ) : null}

        <div className="flex flex-wrap gap-2 pt-1">
          {row.request_id ? <CopyButton value={row.request_id} label="Copy request ID" /> : null}
          <CopyButton
            value={curlFor(baseUrl, endpoint, row.model)}
            label="Copy as cURL"
          />
        </div>
      </div>
    </aside>
  );
}

/** The detail slides in from the right, matching every other inspector. */
export function RequestDetailDialog({
  row,
  onClose,
  baseUrl,
}: {
  row: UsageEvent;
  onClose: () => void;
  baseUrl: string;
}) {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    document.addEventListener("keydown", onKey);
    ref.current?.focus();
    const previous = document.body.style.overflow;
    document.body.style.overflow = "hidden";
    return () => {
      document.removeEventListener("keydown", onKey);
      document.body.style.overflow = previous;
    };
  }, [onClose]);

  return (
    <div
      className="fixed inset-0 z-50 flex justify-end bg-[rgba(16,18,20,0.38)] backdrop-blur-[1.5px]"
      role="presentation"
      onClick={onClose}
    >
      <div
        ref={ref}
        tabIndex={-1}
        role="dialog"
        aria-modal="true"
        aria-label="request detail"
        style={{ outline: "none" }}
        className="drawer-panel h-full w-full max-w-[560px] overflow-y-auto border-l border-line bg-surface shadow-[0_24px_64px_rgba(16,18,20,0.22)]"
        onClick={(e) => e.stopPropagation()}
      >
        <RequestDetail row={row} onClose={onClose} baseUrl={baseUrl} />
      </div>
    </div>
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
