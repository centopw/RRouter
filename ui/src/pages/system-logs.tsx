import { useEffect, useRef, useState } from "react";
import { Badge, Button, Card, CardHeader, EmptyState, ErrorNote, PageHeader, cx } from "../components/ui";
import { api } from "../lib/api";

type Entry = {
  id: number;
  ts: number;
  level: string;
  target: string;
  message: string;
};

type Snapshot = {
  enabled: boolean;
  entries: Entry[];
  capacity: number;
};

function clock(ms: number): string {
  const d = new Date(ms);
  const pad = (n: number, width = 2) => String(n).padStart(width, "0");
  return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}.${pad(d.getMilliseconds(), 3)}`;
}

const LEVEL_TONE: Record<string, string> = {
  ERROR: "text-[var(--color-neg)]",
  WARN: "text-[var(--color-warn-ink)]",
  INFO: "text-muted",
  DEBUG: "text-faint",
  TRACE: "text-faint",
};

/**
 * Opt-in platform diagnostics. The gateway always writes its console log; this page
 * streams the extra in-memory capture, which is off until someone turns it on.
 */
export default function SystemLogs() {
  const [enabled, setEnabled] = useState<boolean | null>(null);
  const [entries, setEntries] = useState<Entry[]>([]);
  const [capacity, setCapacity] = useState(2000);
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState<unknown>(null);
  const scroller = useRef<HTMLDivElement>(null);
  const lastId = useRef(0);

  const apply = (snapshot: Snapshot, append: boolean) => {
    setEnabled(snapshot.enabled);
    setCapacity(snapshot.capacity);
    if (!snapshot.enabled) {
      setEntries([]);
      lastId.current = 0;
      return;
    }
    if (!append) {
      setEntries(snapshot.entries);
      lastId.current = snapshot.entries.at(-1)?.id ?? 0;
      return;
    }
    // An empty poll is the common case; replacing with it would wipe the buffer.
    if (snapshot.entries.length === 0) return;
    setEntries((prev) => {
      const known = new Set(prev.map((e) => e.id));
      const fresh = snapshot.entries.filter((e) => !known.has(e.id));
      const next = [...prev, ...fresh];
      lastId.current = next.at(-1)?.id ?? lastId.current;
      return next.slice(-capacity);
    });
  };

  const load = async (append: boolean) => {
    const snapshot = await api.get<Snapshot>(`/api/system/logs?after=${append ? lastId.current : 0}`);
    apply(snapshot, append);
  };

  useEffect(() => {
    void load(false).catch(setFailure);
    // `load` only reads refs and state setters; polling owns its own interval.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    if (!enabled) return;
    const timer = window.setInterval(() => {
      void api
        .get<Snapshot>(`/api/system/logs?after=${lastId.current}`)
        .then((snapshot) => apply(snapshot, true))
        .catch(setFailure);
    }, 1500);
    return () => window.clearInterval(timer);
  }, [enabled, capacity]);

  useEffect(() => {
    const node = scroller.current;
    if (node) node.scrollTop = node.scrollHeight;
  }, [entries.length]);

  const toggle = async () => {
    const next = !(enabled ?? false);
    setBusy(true);
    setFailure(null);
    try {
      await api.put<{ enabled: boolean }>("/api/system/logs", { enabled: next });
      if (next) await api.del(`/api/system/logs`);
      await load(false);
    } catch (e) {
      setFailure(e);
    } finally {
      setBusy(false);
    }
  };

  const clear = async () => {
    setFailure(null);
    try {
      await api.del(`/api/system/logs`);
      lastId.current = 0;
      setEntries([]);
    } catch (e) {
      setFailure(e);
    }
  };

  return (
    <div className="mx-auto max-w-[1160px]">
      <PageHeader
        title="System logs"
        subtitle="Platform events for debugging the gateway itself. Off by default; nothing is written to disk."
        actions={
          <>
            <Badge tone={enabled ? "pos" : "neutral"}>{enabled ? "capturing" : "off"}</Badge>
            <Button variant={enabled ? "quiet" : "primary"} onClick={toggle} disabled={busy}>
              {busy ? "saving…" : enabled ? "disable logging" : "enable logging"}
            </Button>
            <Button variant="quiet" onClick={clear} disabled={!enabled || entries.length === 0}>
              clear
            </Button>
          </>
        }
      />

      {failure ? (
        <div className="mb-4">
          <ErrorNote error={failure} />
        </div>
      ) : null}

      <Card>
        <CardHeader
          title="Gateway events"
          action={
            <span className="text-caption text-faint tnum">
              {entries.length.toLocaleString()} / {capacity.toLocaleString()} in memory
            </span>
          }
        />
        {enabled === null ? (
          <div className="px-5 py-8 text-small text-faint">loading…</div>
        ) : !enabled ? (
          <EmptyState>
            <div>System logging is off.</div>
            <div className="mt-1.5 max-w-[460px] text-small text-muted">
              Enabling captures the gateway’s own log events in memory so you can debug a
              failure here. It is not persisted, and prompt or response content is never logged.
            </div>
          </EmptyState>
        ) : entries.length === 0 ? (
          <div className="px-5 py-8 text-small text-faint">
            Capturing. New platform events appear here.
          </div>
        ) : (
          <div ref={scroller} className="max-h-[560px] overflow-y-auto border-t border-line">
            <div className="divide-y divide-[var(--color-rule)] font-mono text-caption">
              {entries.map((entry) => (
                <div key={entry.id} className="flex gap-3 px-4 py-2 hover:bg-surface-2">
                  <span className="shrink-0 text-faint">{clock(entry.ts)}</span>
                  <span
                    className={cx("w-[42px] shrink-0 font-semibold", LEVEL_TONE[entry.level] ?? "text-muted")}
                  >
                    {entry.level}
                  </span>
                  <span className="min-w-0 flex-1 break-words text-fg">{entry.message}</span>
                  <span className="hidden max-w-[220px] shrink-0 truncate text-faint min-[900px]:block">
                    {entry.target}
                  </span>
                </div>
              ))}
            </div>
          </div>
        )}
      </Card>
    </div>
  );
}
