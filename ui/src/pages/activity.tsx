import { useMemo, useState } from "react";
import { Bars, Metric } from "../components/charts";
import { Badge, Card, CardHeader, ErrorNote, PageHeader, Select, cx } from "../components/ui";
import { DataTable, type Column } from "../components/table";
import { compact, ms, pct, when } from "../lib/format";
import { useUsage, useUsageSeries, useUsageSummary } from "../lib/queries";
import type { UsageBucket, UsageEvent } from "../lib/api";

const WINDOWS = [
  { id: "24h", label: "Last 24 hours", hours: 24 },
  { id: "7d", label: "Last 7 days", hours: 24 * 7 },
  { id: "30d", label: "Last 30 days", hours: 24 * 30 },
] as const;

type WindowId = (typeof WINDOWS)[number]["id"];

export default function Activity() {
  const [win, setWin] = useState<WindowId>("7d");
  const hours = WINDOWS.find((w) => w.id === win)?.hours ?? 24;
  // One fetch covers the window and the one before it, so deltas are real.
  const series = useUsageSeries(Math.min(hours * 2, 24 * 365));
  const summary = useUsageSummary();
  const usage = useUsage(50);

  const view = useMemo(() => {
    const now = Math.floor(Date.now() / 1000);
    const from = now - hours * 3600;
    const buckets = series.data ?? [];
    const current = buckets.filter((b) => b.start >= from);
    const previous = buckets.filter((b) => b.start < from);

    const totals = (rows: UsageBucket[]) => {
      const requests = rows.reduce((n, b) => n + b.requests, 0);
      const tokens = rows.reduce((n, b) => n + b.input_tokens + b.output_tokens, 0);
      const errors = rows.reduce((n, b) => n + b.errors, 0);
      const latency = rows.reduce((n, b) => n + b.avg_latency_ms * b.requests, 0);
      return { requests, tokens, errors, latency: requests ? latency / requests : 0 };
    };

    const cur = totals(current);
    const prev = totals(previous);
    const delta = (a: number, b: number) => (b > 0 ? ((a - b) / b) * 100 : null);

    return {
      cur,
      prev,
      delta,
      buckets: current,
      errorRate: cur.requests ? (cur.errors / cur.requests) * 100 : 0,
      prevErrorRate: prev.requests ? (prev.errors / prev.requests) * 100 : 0,
    };
  }, [series.data, hours]);

  const recent = usage.data ?? [];
  const topModels = [...(summary.data ?? [])].sort(
    (a, b) => b.input_tokens + b.output_tokens - (a.input_tokens + a.output_tokens),
  );
  const { cur, prev, delta } = view;

  const columns: Array<Column<UsageEvent>> = [
    { key: "when", title: "when", render: (r) => <span className="text-muted">{when(r.ts)}</span> },
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
    { key: "mode", title: "mode", render: (r) => <span className="text-muted">{r.streamed ? "stream" : "json"}</span> },
  ];

  return (
    <>
      <PageHeader
        title="Activity"
        subtitle="Requests, tokens, and latency across every model this gateway serves."
        actions={
          <Select ariaLabel="time window" value={win} onChange={(v) => setWin(v as WindowId)} className="w-[150px]">
            {WINDOWS.map((w) => (
              <option key={w.id} value={w.id}>
                {w.label}
              </option>
            ))}
          </Select>
        }
      />

      {series.isError ? (
        <div className="mb-4">
          <ErrorNote error={series.error} onRetry={() => void series.refetch()} />
        </div>
      ) : null}

      <div className="grid grid-cols-2 gap-3 lg:grid-cols-5">
        <Metric
          label="Requests"
          value={compact(cur.requests)}
          delta={delta(cur.requests, prev.requests)}
          series={view.buckets.map((b) => b.requests)}
        />
        <Metric
          label="Tokens"
          value={compact(cur.tokens)}
          delta={delta(cur.tokens, prev.tokens)}
          series={view.buckets.map((b) => b.input_tokens + b.output_tokens)}
        />
        <Metric
          label="Avg latency"
          value={cur.requests ? ms(cur.latency) : "—"}
          delta={prev.latency > 0 ? ((cur.latency - prev.latency) / prev.latency) * 100 : null}
          tone="accent"
          hint="no previous window"
        />
        <Metric
          label="Error rate"
          value={pct(view.errorRate)}
          delta={prev.requests ? view.errorRate - view.prevErrorRate : null}
          tone="neg"
          hint="no errors logged"
        />
        <Metric
          label="Models in use"
          value={String(summary.data?.length ?? 0)}
          hint={`${compact(cur.requests)} requests in window`}
        />
      </div>

      <div className="mt-3 grid gap-3 lg:grid-cols-2">
        <Card>
          <CardHeader title="Top models" action={<span className="text-[11px] text-faint">tokens · all time</span>} />
          <RankedList
            rows={topModels.slice(0, 6).map((m) => ({
              label: m.model,
              value: compact(m.input_tokens + m.output_tokens),
              meta: `${m.requests} req · ${m.errors} err`,
            }))}
            empty="No requests yet."
          />
        </Card>
        <Card>
          <CardHeader title="Windows" action={<span className="text-[11px] text-faint">requests per bucket</span>} />
          <Bars
            fill
            height={168}
            bars={view.buckets.slice(-12).map((b, i, all) => ({
              label: labelFor(b.start, all.length > 1 ? b.start - all[0]!.start : 0),
              value: b.requests,
              highlight: i === all.length - 1,
            }))}
          />
        </Card>
      </div>

      <Card className="mt-3">
        <CardHeader
          title="Traffic by model"
          action={<span className="text-[11px] text-faint">tokens · all time · top 8</span>}
        />
        <Bars
          bars={topModels.slice(0, 8).map((m, i) => ({
            label: m.model,
            value: m.input_tokens + m.output_tokens,
            highlight: i === 0,
          }))}
        />
      </Card>

      <Card className="mt-3">
        <CardHeader title="Recent requests" action={<span className="text-[11px] text-faint">newest first</span>} />
        <DataTable
          columns={columns}
          rows={recent}
          rowKey={(r) => `${r.ts}-${r.model}-${r.latency_ms}-${r.status}`}
          empty="No requests yet. Point a client at /v1."
          loading={usage.isLoading}
        />
      </Card>
    </>
  );
}

function labelFor(start: number, widthSecs: number) {
  const d = new Date(start * 1000);
  if (widthSecs >= 86_400) return d.toLocaleDateString(undefined, { month: "short", day: "numeric" });
  return d.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" });
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
      {rows.map((row, i) => (
        <li key={`${row.label}-${i}`} className="flex items-center gap-3 border-b border-[#1a1a1a] py-2.5 last:border-0">
          <span className="w-4 text-right text-[11px] text-faint tnum">{i + 1}</span>
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
