import { useQuery } from "@tanstack/react-query";
import { api } from "../lib/api";
import { CopyButton, Disclosure, ErrorNote, cx } from "./ui";

/** What `GET /api/usage/{request_id}/capture` returns. */
export type CapturePayload = {
  request_id: string;
  /** `false` when capture was off (or the row is gone): an answer, not an error. */
  captured: boolean;
  /** Whether capture is on right now, so the viewer can explain the difference. */
  enabled: boolean;
  ts?: number;
  request_headers?: Record<string, string> | null;
  request_body?: string;
  response_body?: string | null;
  request_bytes?: number;
  response_bytes?: number;
  request_truncated?: boolean;
  response_truncated?: boolean;
};

const CAPTURE_KEY = ["request-capture"] as const;

/**
 * The stored request/response bodies for one request.
 *
 * Capture is opt-in and per request, so "nothing was stored" is a first-class state and
 * is spelled out instead of rendering an empty panel. Mounted by the request inspector;
 * it owns its own fetch so the drawer only has to pass the request id.
 */
export function CaptureViewer({
  requestId,
  className,
}: {
  /** Usage row request id; `null` renders nothing (no request selected). */
  requestId: string | null;
  className?: string;
}) {
  const query = useQuery({
    queryKey: [...CAPTURE_KEY, requestId ?? ""],
    queryFn: () => api.get<CapturePayload>(`/api/usage/${encodeURIComponent(requestId ?? "")}/capture`),
    enabled: Boolean(requestId),
    staleTime: 30_000,
  });

  if (!requestId) return null;

  if (query.isPending) {
    return (
      <section aria-label="captured bodies" className={cx("space-y-2", className)}>
        <div aria-hidden className="rounded-[var(--radius-surface)] border border-line px-3 py-2.5">
          <div className="h-3 w-1/3 rounded bg-surface-2" />
        </div>
        <div aria-hidden className="rounded-[var(--radius-surface)] border border-line px-3 py-2.5">
          <div className="h-3 w-2/3 rounded bg-surface-2" />
        </div>
      </section>
    );
  }

  if (query.isError) {
    return (
      <div className={className}>
        <ErrorNote error={query.error} title="Captured bodies" onRetry={() => void query.refetch()} />
      </div>
    );
  }

  const payload = query.data;
  if (!payload) return null;

  if (!payload.captured) {
    return (
      <section
        aria-label="captured bodies"
        className={cx("rounded-[var(--radius-surface)] border border-line px-4 py-3", className)}
      >
        <p className="text-small font-medium text-fg">No bodies were captured for this request</p>
        <p className="mt-1 max-w-[62ch] text-small leading-relaxed text-muted">
          {payload.enabled
            ? "Request capture was off when this request ran. It is on now, so requests made from now on are stored."
            : "Request capture is off. Turn it on in Settings › Runtime to store request and response bodies."}
        </p>
      </section>
    );
  }

  const headers = Object.entries(payload.request_headers ?? {});
  return (
    <section aria-label="captured bodies" className={cx("space-y-3", className)}>
      <div className="flex items-baseline justify-between gap-3">
        <h3 className="text-small font-medium text-muted">Captured bodies</h3>
        <span className="text-caption text-faint">stored on this gateway</span>
      </div>
      <BodyBlock
        label="Request"
        text={payload.request_body ?? ""}
        bytes={payload.request_bytes}
        truncated={payload.request_truncated}
      />
      {payload.response_body == null ? (
        <div className="rounded-[var(--radius-surface)] border border-line px-4 py-3">
          <p className="text-small font-medium text-fg">Response</p>
          <p className="mt-1 text-small leading-relaxed text-muted">
            No response body was recorded — the client disconnected before it finished, or the
            request failed before a body was sent.
          </p>
        </div>
      ) : (
        <BodyBlock
          label="Response"
          text={payload.response_body}
          bytes={payload.response_bytes}
          truncated={payload.response_truncated}
        />
      )}
      {headers.length > 0 ? (
        <Disclosure title={`Request headers · ${headers.length}`}>
          <dl className="grid gap-x-4 gap-y-1 text-caption min-[560px]:grid-cols-[minmax(0,14rem)_1fr]">
            {headers.map(([name, value]) => (
              <div key={name} className="contents">
                <dt className="min-w-0 truncate font-mono text-faint" title={name}>
                  {name}
                </dt>
                <dd className="min-w-0 truncate font-mono text-muted" title={value}>
                  {value}
                </dd>
              </div>
            ))}
          </dl>
        </Disclosure>
      ) : null}
    </section>
  );
}

function BodyBlock({
  label,
  text,
  bytes,
  truncated,
}: {
  label: string;
  text: string;
  bytes?: number;
  truncated?: boolean;
}) {
  const shown = prettyJson(text);
  const empty = text.length === 0;
  return (
    <div className="overflow-hidden rounded-[var(--radius-surface)] border border-line">
      <div className="flex min-w-0 items-center justify-between gap-3 border-b border-rule bg-surface-2 px-3 py-2">
        <div className="flex min-w-0 items-baseline gap-2">
          <span className="text-small font-medium text-fg">{label}</span>
          <span className="text-caption text-faint tnum">
            {bytesLabel(bytes)}
            {truncated ? " · truncated" : ""}
          </span>
        </div>
        {empty ? null : <CopyButton value={text} label="Copy" />}
      </div>
      {empty ? (
        <p className="px-3 py-3 text-small text-faint">(empty body)</p>
      ) : (
        <pre className="max-h-[24rem] overflow-auto whitespace-pre-wrap break-words px-3 py-3 font-mono text-caption leading-relaxed text-muted">
          {shown}
        </pre>
      )}
    </div>
  );
}

/**
 * Pretty-print a JSON body. Anything unparseable — a truncated capture, an SSE frame
 * stream, a form body — is shown exactly as stored rather than replaced with an error.
 */
function prettyJson(text: string): string {
  const trimmed = text.trim();
  if (!trimmed.startsWith("{") && !trimmed.startsWith("[")) return text;
  try {
    return JSON.stringify(JSON.parse(trimmed), null, 2);
  } catch {
    return text;
  }
}

function bytesLabel(bytes: number | undefined): string {
  const value = bytes ?? 0;
  if (value < 1024) return `${value} B`;
  return `${(value / 1024).toFixed(1)} KiB`;
}
