import { useEffect, useRef, type ReactNode } from "react";
import { Link } from "react-router-dom";
import {
  Badge,
  Button,
  CopyButton,
  CopyChip,
  ErrorNote,
  Modal,
  StatusPill,
  cx,
  statusText,
} from "./ui";
import { ms, splitRef, usd, when } from "../lib/format";
import { useUsageDetail } from "../lib/queries";
import type { UsageEvent } from "../lib/api";

export type RequestInspectorDrawerProps = {
  row: UsageEvent | null;
  open?: boolean;
  onClose: () => void;
  baseUrl: string;
  /** Key id -> key name; ids missing from the roster print as a dash. */
  keyNames?: Map<string, string>;
  /** Connection label for this request, falling back to its recorded provider. */
  providerName?: string;
  onPrev?: (() => void) | null;
  onNext?: (() => void) | null;
  positionLabel?: string;
};

/** Non-blocking side drawer inspecting one gateway request, its timing, attempts, and replay links. */
export function RequestInspectorDrawer({
  row,
  open,
  onClose,
  baseUrl,
  keyNames,
  providerName,
  onPrev,
  onNext,
  positionLabel,
}: RequestInspectorDrawerProps) {
  const isOpen = (open ?? true) && row !== null;
  if (!isOpen || !row) return null;

  return (
    <RequestInspectorBody
      row={row}
      onClose={onClose}
      baseUrl={baseUrl}
      keyNames={keyNames}
      providerName={providerName}
      onPrev={onPrev}
      onNext={onNext}
      positionLabel={positionLabel}
    />
  );
}

function RequestInspectorBody({
  row,
  onClose,
  baseUrl,
  keyNames,
  providerName,
  onPrev,
  onNext,
  positionLabel,
}: Omit<RequestInspectorDrawerProps, "open" | "row"> & { row: UsageEvent }) {
  useEffect(() => {
    if (!onPrev && !onNext) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.metaKey || event.ctrlKey || event.altKey) return;
      const target = event.target as HTMLElement | null;
      if (
        target &&
        (target.tagName === "INPUT" ||
          target.tagName === "TEXTAREA" ||
          target.tagName === "SELECT" ||
          target.isContentEditable ||
          Boolean(target.closest("[role='listbox']")))
      ) {
        return;
      }
      if (event.key === "ArrowDown" || event.key === "j") {
        if (onNext) {
          event.preventDefault();
          onNext();
        }
      } else if (event.key === "ArrowUp" || event.key === "k") {
        if (onPrev) {
          event.preventDefault();
          onPrev();
        }
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [onPrev, onNext]);

  const failed = row.status >= 400 || row.error !== null;
  const endpoint = row.endpoint ?? "/v1/chat/completions";
  // Only streamed requests measure a first output, and only when it arrived before
  // the stream ended; otherwise the split would be an invention.
  const measured = !failed && row.ttft_ms > 0 && row.ttft_ms < row.latency_ms;
  const generation = row.latency_ms - row.ttft_ms;
  // The phases add up to the stored latency: upstream headers, model wait, generation.
  const phased = measured && row.upstream_ms > 0 && row.upstream_ms <= row.ttft_ms;
  const waitMs = phased ? row.ttft_ms - row.upstream_ms : 0;

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
  const maxAttemptLatency = attempts.reduce((max, a) => Math.max(max, a.latency_ms), 0);

  const modelRef = row.provider
    ? row.model.startsWith(`${row.provider}/`)
      ? row.model
      : `${row.provider}/${row.model}`
    : (row.requested_model ?? row.model);
  const inspectConnectionId =
    row.connection_id ?? attempts.find((attempt) => attempt.connection_id)?.connection_id ?? null;
  const hasStepper = onPrev !== undefined || onNext !== undefined;
  const { provider: parsedProvider, id: parsedId } = splitRef(row.model);
  const titleText = parsedProvider ? `${parsedProvider}/${parsedId}` : row.model;
  const descriptionText = row.request_id ? `request ${row.request_id.slice(0, 8)}` : "request";

  return (
    <Modal
      open
      variant="drawer"
      modal={false}
      width="md"
      title={titleText}
      description={descriptionText}
      onClose={onClose}
      footer={
        <div className="flex w-full flex-wrap items-center gap-2">
          {modelRef ? (
            <Link
              to={`/playground/chat?model=${encodeURIComponent(modelRef)}`}
              className="inline-flex h-8 items-center gap-1.5 rounded-full border border-[var(--color-line-strong)] bg-surface px-3 text-body font-medium text-fg transition-colors hover:bg-surface-3"
            >
              Replay in Playground →
            </Link>
          ) : null}
          {inspectConnectionId ? (
            <Link
              to={`/providers?inspect=${encodeURIComponent(inspectConnectionId)}`}
              className="inline-flex h-8 items-center gap-1.5 rounded-full border border-line bg-surface px-3 text-body text-muted transition-colors hover:border-[var(--color-line-strong)] hover:bg-surface-3 hover:text-fg"
            >
              Inspect provider →
            </Link>
          ) : null}
          {row.request_id ? <CopyChip value={row.request_id} label="request ID" /> : null}
          <CopyButton
            value={curlFor(baseUrl, endpoint, modelRef || row.model)}
            label="Copy as cURL"
            className="ml-auto"
          />
        </div>
      }
    >
      <div className="-mx-5 -mt-5 mb-4 flex flex-wrap items-center justify-between gap-2 border-b border-line bg-surface-2 px-5 py-2.5 min-[560px]:-mx-6 min-[560px]:px-6">
        <div className="flex min-w-0 items-center gap-2 tnum">
          <StatusPill status={row.status} title={row.error ?? undefined} />
          {row.streamed ? <Badge tone="neutral">streamed</Badge> : null}
        </div>
        {hasStepper ? (
          <div className="flex shrink-0 items-center gap-1.5">
            {positionLabel ? (
              <span className="px-1 font-mono text-caption text-faint tnum">{positionLabel}</span>
            ) : null}
            <Button
              size="sm"
              variant="quiet"
              onClick={() => onPrev?.()}
              disabled={!onPrev}
              ariaLabel="Previous request (ArrowUp or k)"
              title="Previous request (k / ↑)"
            >
              ‹ Prev
            </Button>
            <Button
              size="sm"
              variant="quiet"
              onClick={() => onNext?.()}
              disabled={!onNext}
              ariaLabel="Next request (ArrowDown or j)"
              title="Next request (j / ↓)"
            >
              Next ›
            </Button>
          </div>
        ) : null}
      </div>

      <div className="space-y-4">
        {failed ? (
          <div
            role="alert"
            className="rounded-[var(--radius-surface)] border border-[var(--color-neg-line)] bg-[var(--color-neg-tint)] px-3 py-2 text-small text-[var(--color-neg-ink)]"
          >
            {row.error ?? `upstream answered ${row.status} ${statusText(row.status)}`}
          </div>
        ) : (
          <LatencyBreakdown
            latencyMs={row.latency_ms}
            ttftMs={row.ttft_ms}
            upstreamMs={row.upstream_ms}
            streamed={row.streamed}
            measured={measured}
            phased={phased}
            waitMs={waitMs}
            generation={generation}
          />
        )}

        <dl className="divide-y divide-[var(--color-rule)] text-body">
          <DetailRow k="When" v={when(row.ts)} />
          <DetailRow k="Provider" v={providerName ?? row.provider} />
          <DetailRow k="API key" v={(row.key_id ? keyNames?.get(row.key_id) : undefined) ?? "—"} />
          {row.requested_model && row.requested_model !== row.model ? (
            <DetailRow k="Requested" v={row.requested_model} mono />
          ) : null}
          {row.endpoint ? <DetailRow k="Endpoint" v={`${row.method} ${row.endpoint}`} mono /> : null}
          {row.protocol ? <DetailRow k="Protocol" v={row.protocol} mono /> : null}
          <DetailRow
            k="Tokens"
            v={failed ? "—" : `${row.input_tokens} in · ${row.output_tokens} out`}
            mono
          />
          {row.reasoning_tokens > 0 || row.reasoning_effort ? (
            <DetailRow
              k="Reasoning"
              v={[row.reasoning_effort ?? "unknown effort", `${row.reasoning_tokens} tokens`].join(
                " · ",
              )}
              mono
            />
          ) : null}
          <DetailRow k="Finish" v={row.finish_reason ?? "—"} mono />
          <DetailRow k="Cost" v={usd(row.cost_micros, { dash: true })} mono />
          <DetailRow
            k="Cache"
            v={failed ? "—" : `${row.cache_read_tokens} read · ${row.cache_write_tokens} write`}
            mono
          />
          <DetailRow k="Connection" v={row.connection_id ?? "—"} mono />
          {row.session_id ? <DetailRow k="Session" v={row.session_id} mono /> : null}
        </dl>

        {showAttempts ? (
          <section aria-label="attempts" className="space-y-2">
            <div className="flex items-baseline justify-between gap-2">
              <h3 className="text-small font-medium text-muted">Attempts</h3>
              {attempts.length > 1 ? (
                <span className="text-caption text-faint tnum">
                  {attempts.length} in dispatch order
                </span>
              ) : null}
            </div>
            {detail.isError ? (
              <ErrorNote error={detail.error} onRetry={() => void detail.refetch()} />
            ) : detail.isPending ? (
              <div aria-hidden className="rounded-[var(--radius-surface)] border border-line px-3 py-2.5">
                <div className="h-3 w-2/3 rounded bg-surface-2" />
              </div>
            ) : (
              <ol className="divide-y divide-[var(--color-rule)] rounded-[var(--radius-surface)] border border-line">
                {attempts.map((attempt, index) => {
                  const attemptFailed =
                    attempt.status === 0 || attempt.status >= 400 || attempt.error !== null;
                  const { provider, id } = splitRef(attempt.model);
                  const served = !attemptFailed && index === attempts.length - 1;
                  const barPct =
                    maxAttemptLatency > 0
                      ? Math.max(4, Math.min(100, (attempt.latency_ms / maxAttemptLatency) * 100))
                      : 0;
                  return (
                    <li key={attempt.attempt} className="space-y-1.5 px-3 py-2">
                      <div className="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1 text-small">
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
                          <span className="font-mono tnum">{attempt.status || "err"}</span>
                        </Badge>
                        <span aria-hidden className="text-faint">
                          ·
                        </span>
                        <span className="font-mono text-muted tnum">{ms(attempt.latency_ms)}</span>
                        {served ? <span className="text-caption text-faint">served</span> : null}
                      </div>
                      {maxAttemptLatency > 0 ? (
                        <div
                          aria-hidden
                          className="h-1 overflow-hidden rounded-full bg-[var(--color-rule)]"
                        >
                          <div
                            className={cx(
                              "h-full rounded-full",
                              attemptFailed ? "bg-[var(--color-neg-fill)]" : "bg-[var(--color-pos-fill)]",
                            )}
                            style={{ width: `${barPct}%` }}
                          />
                        </div>
                      ) : null}
                      {attemptFailed && attempt.error ? (
                        <p className="break-words text-small leading-[1.45] text-neg">
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
      </div>
    </Modal>
  );
}

function LatencyBreakdown({
  latencyMs,
  ttftMs,
  upstreamMs,
  streamed,
  measured,
  phased,
  waitMs,
  generation,
}: {
  latencyMs: number;
  ttftMs: number;
  upstreamMs: number;
  streamed: boolean;
  measured: boolean;
  phased: boolean;
  waitMs: number;
  generation: number;
}): ReactNode {
  return (
    <div className="space-y-2">
      <div className="text-body text-muted">
        Latency <span className="font-mono text-fg tnum">{ms(latencyMs)}</span>
        {streamed ? <span className="text-faint"> · streamed</span> : null}
      </div>
      {measured ? (
        phased ? (
          <div>
            <div className="text-small text-faint">
              First output <span className="font-mono tnum">{ms(ttftMs)}</span> · generation{" "}
              <span className="font-mono tnum">{ms(generation)}</span>
            </div>
            <div
              aria-hidden
              className="mt-2 flex h-2 overflow-hidden rounded-full bg-[var(--color-rule)]"
              title="connect and headers, model wait, generation"
            >
              <span
                className="bg-[var(--color-series-3)]"
                style={{ width: `${(upstreamMs / latencyMs) * 100}%` }}
              />
              <span className="bg-accent" style={{ width: `${(waitMs / latencyMs) * 100}%` }} />
              <span
                className="bg-[var(--color-pos)]"
                style={{ width: `${(generation / latencyMs) * 100}%` }}
              />
            </div>
            <dl className="mt-2 grid grid-cols-[auto_1fr_auto] items-center gap-x-2 gap-y-1 text-caption">
              {[
                ["connect + headers", upstreamMs, "var(--color-series-3)"],
                ["model → first token", waitMs, "var(--color-accent)"],
                ["generation", generation, "var(--color-pos)"],
              ].map(([label, value, color]) => (
                <div key={String(label)} className="contents">
                  <span
                    aria-hidden
                    className="h-2 w-2 rounded-full"
                    style={{ backgroundColor: String(color) }}
                  />
                  <span className="text-muted">{label}</span>
                  <span className="text-right font-mono text-fg tnum">{ms(Number(value))}</span>
                </div>
              ))}
            </dl>
          </div>
        ) : (
          <div>
            <div className="text-small text-faint">
              First output <span className="font-mono tnum">{ms(ttftMs)}</span> · generation{" "}
              <span className="font-mono tnum">{ms(generation)}</span>
            </div>
            <div
              className="mt-1.5 flex h-1.5 overflow-hidden rounded-full bg-[var(--color-rule)]"
              title="time to first output vs generation"
            >
              <span
                aria-hidden
                className="bg-accent"
                style={{ width: `${(ttftMs / latencyMs) * 100}%` }}
              />
            </div>
          </div>
        )
      ) : null}
    </div>
  );
}

function DetailRow({ k, v, mono }: { k: string; v: string; mono?: boolean }) {
  return (
    <div className="flex items-center justify-between gap-3 py-2">
      <dt className="text-faint">{k}</dt>
      <dd className={cx("min-w-0 truncate text-right text-muted tnum", mono && "font-mono text-small")}>
        {v}
      </dd>
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
