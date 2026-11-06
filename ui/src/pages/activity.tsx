import { useMemo, useState } from "react";
import { Bars, Metric } from "../components/charts";
import { Card, CardHeader, PageHeader, Select, Badge, ErrorNote, Dot, cx } from "../components/ui";
import { DataTable, type Column } from "../components/table";
import { compact, ms, pct, when } from "../lib/format";
import { useConnections, useUsage } from "../lib/queries";
import type { UsageEvent } from "../lib/api";

const WINDOWS = [
  { id: "24h", label: "Last 24 hours", hours: 24 },
  { id: "7d", label: "Last 7 days", hours: 24 * 7 },
  { id: "30d", label: "Last 30 days", hours: 24 * 30 },
  { id: "all", label: "All time", hours: 0 },
] as const;

type WindowId = (typeof WINDOWS)[number]["id"];

export default function Activity() {
  const [win, setWin] = useState<WindowId>("7d");
  const usage = useUsage(1000);
  const connections = useConnections();

  const view = useMemo(() => {
    const hours = WINDOWS.find((w) => w.id === win)?.hours ?? 0;
    const now = Math.floor(Date.now() / 1000);
    const from = hours ? now - hours * 3600 : 0;
    const prevFrom = hours ? now - hours * 7200 : 0;
    const events = usage.data ?? [];
    const current = events.filter((e) => e.ts >= from);
    const previous = hours
      ? events.filter((e) => e.ts >= prevFrom && e.ts < from)
      : [];

    const total = (rows: UsageEvent[]) =>
      rows.reduce(
        (acc, e) => ({
          requests: acc.requests + 1,
          tokens: acc.tokens + e.input_tokens + e.output_tokens,
          latency: acc.latency + e.latency_ms,
          errors: acc.errors + (e.status >= 400 || e.error ? 1 : 0),
        }),
        { requests: 0, tokens: 0, latency: 0, errors: 0 },
      );

    const cur = total(current);
    const prev = total(previous);
    const delta = (a: number, b: number) => (b > 0 ? ((a - b) / b) * 100 : null);

    const byModel = groupBy(current, (e) => e.model);
    const byConn = groupBy(current, (e) => e.connection_id ?? "unknown");

    const names = new Map((connections.data ?? []).map((c) => [c.id, c.name ?? c.provider]));
    const buckets = series(current, from || (current.at(-1)?.ts ?? now), now, 24);

    return {
      cur,
      prev,
      delta,
      models: [...byModel.entries()]
        .map(([model, rows]) => ({ model, rows, tokens: total(rows).tokens }))
        .sort((a, b) => b.tokens - a.tokens),
      conns: [...byConn.entries()]
        .map(([id, rows]) => ({ id, name: names.get(id) ?? id.slice(0, 8), rows, tokens: total(rows).tokens }))
        .sort((a, b) => b.tokens - a.tokens),
      buckets,
      recent: [...current].sort((a, b) => b.ts - a.ts).slice(0, 12),
    };
  }, [usage.data, connections.data, win]);

  const { cur, prev, delta } = view;
  const errorRate = cur.requests ? (cur.errors / cur.requests) * 100 : 0;
  const prevErrorRate = prev.requests ? (prev.errors / prev.requests) * 100 : 0;
  const avgLatency = cur.requests ? cur.latency / cur.requests : 0;
  const prevLatency = prev.requests ? prev.latency / prev.requests : 0;

  const recentColumns: Array<Column<UsageEvent>> = [
    {
      key: "when",
      title: "when",
      render: (r) => <span className="text-muted">{when(r.ts)}</span>,
    },
    { key: "model", title: "model", render: (r) => <span className="font-mono text-[12px]">{r.model}</span> },
    {
      key: "status",
      title: "status",
      render: (r) =>
        r.status >= 400 || r.error ? (
          <Badge tone="neg" title={r.error ?? undefined}>
            {r.status || "err"}
          </Badge>
        ) : (
          <Badge tone="pos">{r.status}</Badge>
        ),
    },
    {
      key: "tokens",
      title: "tokens",
      align: "right",
      render: (r) => compact(r.input_tokens + r.output_tokens),
    },
    { key: "latency", title: "latency", align: "right", render: (r) => ms(r.latency_ms) },
    {
      key: "stream",
      title: "mode",
      render: (r) => <span className="text-muted">{r.streamed ? "stream" : "json"}</span>,
    },
  ];

  return (
    <>
      <PageHeader
        title="Activity"
        subtitle="Requests, tokens, and latency across every model this gateway serves."
        actions={
          <>
            <Select ariaLabel="time window" value={win} onChange={(v) => setWin(v as WindowId)} className="w-[150px]">
              {WINDOWS.map((w) => (
                <option key={w.id} value={w.id}>
                  {w.label}
                </option>
              ))}
            </Select>
          </>
        }
      />

      {usage.isError ? (
        <div className="mb-4">
          <ErrorNote error={usage.error} onRetry={() => void usage.refetch()} />
        </div>
      ) : null}

      <div className="grid grid-cols-2 gap-3 lg:grid-cols-5">
        <Metric
          label="Requests"
          value={compact(cur.requests)}
          delta={delta(cur.requests, prev.requests)}
          series={view.buckets.map((b) => b.count)}
        />
        <Metric
          label="Tokens"
          value={compact(cur.tokens)}
          delta={delta(cur.tokens, prev.tokens)}
          series={view.buckets.map((b) => b.tokens)}
        />
        <Metric
          label="Avg latency"
          value={cur.requests ? ms(avgLatency) : "—"}
          delta={prev.requests ? ((avgLatency - prevLatency) / (prevLatency || 1)) * 100 : null}
          tone="accent"
          hint="no previous window"
        />
        <Metric
          label="Error rate"
          value={pct(errorRate)}
          delta={prev.requests ? errorRate - prevErrorRate : null}
          tone="neg"
          hint="no errors logged"
        />
        <Metric label="Models in use" value={String(view.models.length)} hint={`of ${compact(view.recent.length)} recent`} />
      </div>

      <div className="mt-3 grid gap-3 lg:grid-cols-2">
        <Card>
          <CardHeader
            title="Top models"
            action={
              <span className="text-[11px] text-faint">
                tokens · {cur.requests === 0 ? "no traffic" : "window"}
              </span>
            }
          />
          <RankedList
            rows={view.models.map((m) => ({
              label: m.model,
              value: compact(m.tokens),
              meta: `${m.rows.length} req`,
            }))}
            empty="No requests in this window."
          />
        </Card>
        <Card>
          <CardHeader
            title="Top connections"
            action={<span className="text-[11px] text-faint">tokens</span>}
          />
          <RankedList
            rows={view.conns.map((c) => ({
              label: c.name,
              value: compact(c.tokens),
              meta: `${c.rows.length} req`,
            }))}
            empty="No connection has served traffic in this window."
          />
        </Card>
      </div>

      <Card className="mt-3">
        <CardHeader
          title="Traffic by model"
          action={<span className="text-[11px] text-faint">tokens per model, top 8</span>}
        />
        <Bars
          bars={view.models.slice(0, 8).map((m, i) => ({
            label: m.model,
            value: m.tokens,
            highlight: i === 0,
          }))}
        />
      </Card>

      <Card className="mt-3">
        <CardHeader
          title="Recent requests"
          action={<span className="text-[11px] text-faint">newest first · capped at 1000</span>}
        />
        <DataTable
          columns={recentColumns}
          rows={view.recent}
          rowKey={(r) => `${r.ts}-${r.model}-${r.latency_ms}`}
          empty="No requests yet. Point a client at /v1."
          loading={usage.isLoading}
        />
      </Card>
    </>
  );
}

function RankedList({
  rows,
  empty,
}: {
  rows: Array<{ label: string; value: string; meta: string }>;
  empty: string;
}) {
  if (rows.length === 0) {
    return <div className="px-5 pb-6 text-[13px] text-faint">{empty}</div>;
  }
  return (
    <ol className="px-5 pb-4">
      {rows.slice(0, 6).map((row, i) => (
        <li key={`${row.label}-${i}`} className="flex items-center gap-3 border-b border-[#1a1a1a] py-2.5 last:border-0">
          <span className="w-4 text-right text-[11px] text-faint tnum">{i + 1}</span>
          <Dot tone={i === 0 ? "pos" : i === 1 ? "accent" as never : "muted"} />
          <span className="min-w-0 flex-1">
            <span className="block truncate text-[13px]">{row.label}</span>
            <span className="block text-[11px] text-faint">{row.meta}</span>
          </span>
          <span className={cx("text-[13px] tnum text-muted")}>{row.value}</span>
        </li>
      ))}
    </ol>
  );
}

function groupBy<T>(rows: T[], key: (row: T) => string): Map<string, T[]> {
  const out = new Map<string, T[]>();
  for (const row of rows) {
    const k = key(row);
    const bucket = out.get(k);
    if (bucket) bucket.push(row);
    else out.set(k, [row]);
  }
  return out;
}

function series(rows: UsageEvent[], from: number, to: number, points: number) {
  const span = Math.max(1, to - from);
  const width = span / points;
  const out = Array.from({ length: points }, (_, i) => ({
    at: from + i * width,
    count: 0,
    tokens: 0,
  }));
  for (const row of rows) {
    const index = Math.min(points - 1, Math.max(0, Math.floor((row.ts - from) / width)));
    const slot = out[index];
    if (!slot) continue;
    slot.count += 1;
    slot.tokens += row.input_tokens + row.output_tokens;
  }
  return out.map((b) => ({ ...b, label: new Date(b.at * 1000).toLocaleDateString(undefined, { month: "short", day: "numeric" }) }));
}
