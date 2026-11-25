import { useEffect, useMemo, useRef, useState } from "react";
import { Link } from "react-router-dom";
import { Trash } from "@phosphor-icons/react";
import { DataTable, type Column } from "../components/table";
import { useToast } from "../components/toast";
import { Badge, Button, Card, CardHeader, ConfirmDialog, EmptyState, ErrorNote, PageHeader, cx } from "../components/ui";
import { ApiError, api } from "../lib/api";

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
export function SystemLogs() {
  const [enabled, setEnabled] = useState<boolean | null>(null);
  const [entries, setEntries] = useState<Entry[]>([]);
  const [capacity, setCapacity] = useState(2000);
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState<unknown>(null);
  const [pendingDelete, setPendingDelete] = useState<Entry | null>(null);
  const [deleting, setDeleting] = useState(false);
  const [deleteError, setDeleteError] = useState<unknown>(null);
  const toast = useToast();
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

  // No optimistic removal: the row stays until the gateway answers 204 and the refetch
  // reports the new count. A 404 means it rotated out of the ring first, which is not a
  // failure to report as one.
  const deleteEntry = async () => {
    if (!pendingDelete) return;
    setDeleting(true);
    setDeleteError(null);
    try {
      await api.del(`/api/system/logs/${pendingDelete.id}`);
      toast.success("Log entry deleted");
    } catch (e) {
      if (!(e instanceof ApiError && e.status === 404)) {
        setDeleteError(e);
        setDeleting(false);
        return;
      }
      toast.info("That entry was already gone", "It rotated out of the buffer before the delete.");
    }
    setDeleting(false);
    setPendingDelete(null);
    await load(false).catch(setFailure);
  };

  const subtitle =
    "Platform events for debugging the gateway itself. Off by default; nothing is written to disk.";

  // Newest first: the buffer grows at the end, and a paged table must open on the events
  // that just happened rather than on the oldest page of a 2,000-entry ring.
  const logRows = useMemo(() => [...entries].reverse(), [entries]);

  const logColumns: Array<Column<Entry>> = [
    {
      key: "ts",
      title: "Time",
      width: "104px",
      className: "font-mono tnum",
      render: (entry) => clock(entry.ts),
    },
    {
      key: "level",
      title: "Level",
      width: "76px",
      className: "font-mono",
      render: (entry) => (
        <span className={cx("font-semibold", LEVEL_TONE[entry.level] ?? "text-muted")}>
          {entry.level}
        </span>
      ),
      cellTitle: (entry) => entry.level,
    },
    {
      key: "message",
      title: "Message",
      className: "font-mono",
      render: (entry) => entry.message,
      cellTitle: (entry) => entry.message,
    },
    {
      key: "target",
      title: "Target",
      width: "240px",
      className: "hidden font-mono text-faint min-[900px]:table-cell",
      render: (entry) => entry.target,
      cellTitle: (entry) => entry.target,
    },
    {
      key: "actions",
      title: "Actions",
      srOnlyTitle: true,
      align: "right",
      width: "56px",
      render: (entry) => {
        const what = `log entry ${clock(entry.ts)} ${entry.level}`;
        return (
          <button
            type="button"
            aria-label={`Delete ${what}`}
            title={`Delete ${what}`}
            onClick={() => {
              setDeleteError(null);
              setPendingDelete(entry);
            }}
            // 24px keeps the row inside both density heights (36px compact, 48px roomy).
            className="inline-flex h-6 w-6 items-center justify-center rounded-full text-faint transition-colors hover:bg-[var(--color-neg-tint)] hover:text-[var(--color-neg-ink)] focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--color-focus-ring)]"
          >
            <Trash size={14} aria-hidden />
          </button>
        );
      },
    },
  ];

  const controls = (
    <>
      <Badge tone={enabled ? "pos" : "neutral"}>{enabled ? "capturing" : "off"}</Badge>
      <Button variant={enabled ? "quiet" : "primary"} onClick={toggle} disabled={busy}>
        {busy ? "Saving…" : enabled ? "Disable logging" : "Enable logging"}
      </Button>
      <Button variant="quiet" onClick={clear} disabled={!enabled || entries.length === 0}>
        Clear
      </Button>
    </>
  );

  const body = (
    <>
      <PageHeader
        title="System logs"
        subtitle={subtitle}
        actions={
          <>
            <Link
              to="/system/health"
              className="inline-flex h-8 items-center rounded-full border border-line bg-surface px-3 text-body text-muted transition-colors hover:border-line-strong hover:bg-surface-3 hover:text-fg"
            >
              System health →
            </Link>
            {controls}
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
        ) : (
          <DataTable<Entry>
            columns={logColumns}
            rows={logRows}
            rowKey={(entry) => String(entry.id)}
            tableLayout="fixed"
            caption="Gateway events"
            scrollLabel="events"
            empty="Capturing. New platform events appear here."
          />
        )}
      </Card>

      <ConfirmDialog
        open={pendingDelete !== null}
        title="Delete this log entry?"
        message={
          pendingDelete ? (
            <>
              <span className="font-mono">{pendingDelete.level}</span> at{" "}
              <span className="font-mono tnum">{clock(pendingDelete.ts)}</span> on{" "}
              <span className="font-mono tnum">
                {new Date(pendingDelete.ts).toLocaleDateString()}
              </span>
              . It is dropped from the in-memory buffer and cannot be recovered.
            </>
          ) : null
        }
        confirmLabel="Delete entry"
        busy={deleting}
        error={deleteError}
        onConfirm={() => void deleteEntry()}
        onClose={() => {
          setPendingDelete(null);
          setDeleteError(null);
        }}
      />
    </>
  );

  return <div className="mx-auto max-w-[1160px]">{body}</div>;
}

export default SystemLogs;
