import { useEffect, useMemo, useState } from "react";
import { Link } from "react-router-dom";
import {
  AlertBanner,
  Button,
  Card,
  ErrorNote,
  FilterChip,
  Segmented,
  cx,
  statusText,
} from "../../components/ui";
import { Readout, TrafficChart, type TrafficBucket, type TrafficPart } from "../../components/charts";
import { compact, ms, pct } from "../../lib/format";
import {
  RANGES,
  bucketLabel,
  bucketRangeLabel,
  deltaLabel,
  incidentOf,
  rangeOf,
  seriesColors,
  windowStarts,
  type RangeId,
  type RangeSpec,
} from "../../lib/activity";
import { useUsage, useUsageByModel, useUsageSeries, useUsageSummary } from "../../lib/queries";
import type { UsageBucket, UsageEvent, UsageModelBucket, UsageSummaryRow } from "../../lib/api";
import { ModelTable, type ModelRow } from "./model-table";
import { RequestDetail, RequestLog } from "./request-log";
import { Onboarding } from "./onboarding";

/** The one id the chart uses when the per-model split is unavailable. */
const ALL = "__all__";
const METRICS = [
  { id: "requests", label: "Requests" },
  { id: "tokens", label: "Tokens" },
] as const;

type Metric = (typeof METRICS)[number]["id"];

type Totals = {
  requests: number;
  tokens: number;
  input: number;
  errors: number;
  cacheRead: number;
  cacheWrite: number;
  /** Request-weighted mean, in milliseconds. */
  latency: number;
};

const ZERO: Totals = {
  requests: 0,
  tokens: 0,
  input: 0,
  errors: 0,
  cacheRead: 0,
  cacheWrite: 0,
  latency: 0,
};

export default function Activity() {
  const [rangeId, setRangeId] = useState<RangeId>("7d");
  const [metric, setMetric] = useState<Metric>("requests");
  const [modelFilter, setModelFilter] = useState<string | null>(null);
  const [errorsOnly, setErrorsOnly] = useState(false);
  const [selectedId, setSelectedId] = useState<number | null>(null);
  const [showAll, setShowAll] = useState(false);
  const [closed, setClosed] = useState(false);

  const spec = rangeOf(rangeId);
  const now = Math.floor(Date.now() / 1000);
  const from = now - spec.hours * 3600;
  // Bucket boundaries are aligned server-side, so the window starts at the
  // first aligned bucket that still contains in-window traffic. Summing from
  // there equals the chart's columns exactly.
  const alignedFrom = Math.floor(from / spec.seconds) * spec.seconds;

  // Current + previous window in one fetch: deltas are real, not estimated.
  const series = useUsageSeries(spec.hours * 2, spec.bucket);
  const byModel = useUsageByModel(spec.hours, spec.bucket);
  const summary = useUsageSummary(spec.hours);
  const usage = useUsage(200, spec.hours);
  // One row proves the gateway has ever served traffic; the window-scoped
  // queries cannot tell "never used" from "quiet right now".
  const ever = useUsage(1);

  const currentBuckets = (series.data ?? []).filter((b) => b.start >= alignedFrom);
  const cur = totalsOf(currentBuckets);
  const prev = totalsOf((series.data ?? []).filter((b) => b.start < alignedFrom));

  const rows = useMemo(() => modelRows(summary.data ?? []), [summary.data]);
  const colors = useMemo(() => seriesColors(rows.map((r) => r.ref)), [rows]);
  const totalTokens = rows.reduce((n, r) => n + r.tokens, 0);

  const events = usage.data ?? [];
  const failedEvents = events.filter(isFailure);
  const filtered = useMemo(
    () =>
      events.filter(
        (e) => (!errorsOnly || isFailure(e)) && (!modelFilter || splitModel(e.provider, e.model).ref === modelFilter),
      ),
    [events, errorsOnly, modelFilter],
  );
  // Selection survives refetch by id; a filter that hides the row falls back to
  // the first visible one. Closing the panel is an explicit "show nothing".
  const selected = closed
    ? null
    : (filtered.find((e) => e.id !== null && e.id === selectedId) ?? filtered[0] ?? null);
  const logRows = useMemo(
    () => filtered.map((e) => ({ ...e, model: splitModel(e.provider, e.model).name })),
    [filtered],
  );
  const selectedRow = selected
    ? { ...selected, model: splitModel(selected.provider, selected.model).name }
    : null;

  const errorRate = cur.requests > 0 ? (cur.errors / cur.requests) * 100 : 0;
  const prevErrorRate = prev.requests > 0 ? (prev.errors / prev.requests) * 100 : 0;
  const slowest = rows.reduce<ModelRow | null>(
    (best, r) => (r.requests > 0 && (!best || r.avg_latency_ms > best.avg_latency_ms) ? r : best),
    null,
  );
  const latencyDelta = deltaLabel(cur.latency, prev.latency);

  const incident = incidentOf(rows);
  const incidentRef = incident ? splitModel(incident.provider, incident.model).ref : null;
  const dominant = incidentRef
    ? dominantStatus(events.filter((e) => isFailure(e) && splitModel(e.provider, e.model).ref === incidentRef))
    : null;

  const chartBuckets = useMemo(
    () =>
      buildChart({
        spec,
        now,
        alignedFrom,
        series: series.data ?? [],
        byModel: byModel.data ?? [],
        metric,
        rankedRefs: rows.map((r) => r.ref),
      }),
    [spec, now, alignedFrom, series.data, byModel.data, metric, rows],
  );
  const chartColor = (id: string) => (id === ALL ? "var(--color-series-1)" : colors.colorOf(id));
  const legend = useMemo(() => {
    const hasSplit = (byModel.data ?? []).some((b) => b.start >= alignedFrom);
    if (!hasSplit) {
      return cur.requests > 0
        ? [{ id: ALL, label: "All models", color: "var(--color-series-1)" }]
        : [];
    }
    const entries = colors.ids
      .filter((id) => rows.some((r) => r.ref === id))
      .map((id) => ({ id, label: nameOf(id), color: colors.colorOf(id) }));
    if (colors.other) entries.push({ id: "other", label: "Other", color: "var(--color-series-6)" });
    return entries;
  }, [colors, rows, cur.requests, byModel.data, alignedFrom]);

  // Honest "all traffic is recent" test: no older bucket in the window carries a
  // request, so the hint is only offered when it is certainly true.
  const hasOlderTraffic = currentBuckets.some((b) => b.start < now - 3600 && b.requests > 0);
  const onlyLastHour = rangeId !== "1h" && cur.requests > 0 && !hasOlderTraffic;
  const cacheNote = cur.cacheRead === 0;

  const onboard = ever.isSuccess && (ever.data?.length ?? 0) === 0;
  const showPanels = !onboard;
  const baseUrl = `${window.location.origin}/v1`;

  const selectModel = (ref: string) => {
    setModelFilter((current) => (current === ref ? null : ref));
    setSelectedId(null);
    setClosed(false);
  };

  return (
    <div className="mx-auto max-w-[1160px] max-[820px]:-mx-4">
      <header className="mb-5 flex flex-wrap items-start justify-between gap-3">
        <div>
          <h1 className="text-[30px] font-semibold tracking-[-0.02em] text-fg">Activity</h1>
          <p className="mt-1 text-[13.5px] text-muted">
            What this gateway is serving, how fast, and whether anything is failing.
          </p>
        </div>
        <div className="flex flex-wrap items-center gap-3">
          <LiveStamp at={series.dataUpdatedAt} waiting={onboard} />
          <Segmented
            options={RANGES.map((r) => ({ id: r.id, label: r.id }))}
            value={rangeId}
            onChange={setRangeId}
            ariaLabel="time range"
          />
        </div>
      </header>

      {incident && showPanels ? (
        <AlertBanner
          title={`${incident.provider}/${incident.model} is failing`}
          actions={
            <>
              <Button
                variant="primary"
                onClick={() => {
                  setModelFilter(incidentRef);
                  setErrorsOnly(true);
                  setSelectedId(null);
                  setClosed(false);
                }}
              >
                View failing requests
              </Button>
              <Link
                to="/combos"
                className="inline-flex h-8 items-center rounded-lg px-3 text-[13px] text-muted transition-colors hover:bg-surface-2 hover:text-fg"
              >
                Open Combos →
              </Link>
            </>
          }
        >
          <p>
            {incident.errors} of its {incident.requests} requests{" "}
            {dominant !== null
              ? `returned ${dominant} ${statusText(dominant)}`
              : "failed"}{" "}
            ({pct(incident.rate * 100)}).
          </p>
          <p className="mt-1 text-faint">
            Check the provider connection, or route around it with a combo while it recovers.
          </p>
        </AlertBanner>
      ) : null}

      <div className="space-y-5">
        <Card>
          {series.isError ? (
            <div className="p-5">
              <ErrorNote error={series.error} onRetry={() => void series.refetch()} />
            </div>
          ) : (
            <>
              {series.isLoading && !series.data ? (
                <div
                  className="grid gap-px bg-[var(--color-line)]"
                  style={{ gridTemplateColumns: "repeat(auto-fit, minmax(150px, 1fr))" }}
                >
                  {Array.from({ length: 4 }, (_, i) => (
                    <div key={i} className="bg-surface">
                      <ReadoutSkeleton />
                    </div>
                  ))}
                </div>
              ) : (
                <div
                  className="grid gap-px overflow-hidden rounded-t-[var(--radius-card)] bg-[var(--color-line)]"
                  style={{ gridTemplateColumns: "repeat(auto-fit, minmax(150px, 1fr))" }}
                >
                  <div className="bg-surface">
                    <Readout
                      label="Requests"
                      value={compact(cur.requests)}
                      note={
                        cur.errors > 0
                          ? `${compact(cur.requests - cur.errors)} succeeded · ${compact(cur.errors)} failed`
                          : "All succeeded"
                      }
                      delta={deltaLabel(cur.requests, prev.requests)}
                    />
                  </div>
                  <div className="bg-surface">
                    <Readout
                      label="Tokens"
                      value={compact(cur.tokens)}
                      note={
                        cur.requests > 0
                          ? `~${compact(Math.round(cur.tokens / cur.requests))} per request`
                          : "No tokens yet"
                      }
                      delta={deltaLabel(cur.tokens, prev.tokens)}
                    />
                  </div>
                  <div className="bg-surface">
                    <Readout
                      label="Avg latency"
                      value={cur.requests > 0 ? `${(cur.latency / 1000).toFixed(2)}s` : "—"}
                      note={
                        slowest ? (
                          <>
                            <span>
                              Slowest: {slowest.model} {ms(slowest.avg_latency_ms)}
                            </span>
                            {latencyDelta ? (
                              <span className={latencyDelta.up ? "text-warn" : "text-muted"}>
                                {latencyDelta.text}
                              </span>
                            ) : null}
                          </>
                        ) : (
                          "No timed requests"
                        )
                      }
                    />
                  </div>
                  <div className="bg-surface">
                    <Readout
                      label="Error rate"
                      value={cur.requests > 0 ? pct(errorRate) : "—"}
                      tone={cur.errors > 0 ? "neg" : "fg"}
                      dot={cur.errors > 0 ? "neg" : "pos"}
                      note={
                        cur.errors > 0
                          ? `${compact(cur.errors)} of ${compact(cur.requests)} failed`
                          : "No failures"
                      }
                      delta={deltaLabel(errorRate, prevErrorRate)}
                    />
                  </div>
                  {cur.cacheRead > 0 ? (
                    <div className="bg-surface">
                      <Readout
                        label="Cache hit"
                        value={pct(hitRate(cur))}
                        note={`${compact(cur.cacheRead)} read · ${compact(cur.cacheWrite)} write`}
                        delta={deltaLabel(hitRate(cur), hitRate(prev))}
                      />
                    </div>
                  ) : null}
                </div>
              )}

              {showPanels ? (
                <div className="border-t border-line">
                  <div className="flex flex-wrap items-center gap-x-3 gap-y-2 px-6 pb-2 pt-4">
                    <h2 className="text-[15px] font-semibold">Traffic</h2>
                    <Segmented
                      options={METRICS}
                      value={metric}
                      onChange={setMetric}
                      ariaLabel="chart metric"
                    />
                    <div className="ml-auto flex flex-wrap items-center gap-x-3 gap-y-1">
                      {legend.map((entry) => (
                        <span
                          key={entry.id}
                          title={entry.id}
                          className="inline-flex items-center gap-1.5 text-[11.5px] text-muted"
                        >
                          <span
                            aria-hidden
                            className="h-2 w-2 rounded-[2px]"
                            style={{ background: entry.color }}
                          />
                          {entry.label}
                        </span>
                      ))}
                    </div>
                  </div>
                  {byModel.isError ? (
                    <div className="px-6 pb-3">
                      <ErrorNote error={byModel.error} onRetry={() => void byModel.refetch()} />
                    </div>
                  ) : null}
                  {series.isLoading && !series.data ? (
                    <div className="px-6 py-4">
                      <Skeleton height={200} width="100%" />
                    </div>
                  ) : (
                    <TrafficChart
                      buckets={chartBuckets}
                      colorOf={chartColor}
                      unit={metric}
                      labelEvery={spec.labelEvery}
                    />
                  )}
                  {cacheNote || onlyLastHour ? (
                    <div className="flex flex-wrap items-center justify-between gap-2 border-t border-line px-6 py-3 text-[13px] text-muted">
                      <span>
                        {cacheNote
                          ? "Cache hits: none recorded in this window. They appear once a request reuses an earlier prompt prefix."
                          : ""}
                      </span>
                      {onlyLastHour ? (
                        <span className="flex flex-wrap items-center gap-2">
                          <span>
                            All {compact(cur.requests)} requests arrived in the last hour.
                          </span>
                          <Button variant="quiet" onClick={() => setRangeId("1h")}>
                            Zoom to 1h
                          </Button>
                        </span>
                      ) : null}
                    </div>
                  ) : null}
                </div>
              ) : null}
            </>
          )}
        </Card>

        {onboard ? <Onboarding baseUrl={baseUrl} /> : null}

        {showPanels ? (
          <>
            <Card>
              <div className="flex flex-wrap items-baseline justify-between gap-x-4 gap-y-1 px-6 py-4">
                <div className="flex flex-wrap items-baseline gap-2">
                  <h2 className="text-[15px] font-semibold">Models</h2>
                  <span className="text-[12.5px] text-faint">
                    {rows.length} in use · ranked by tokens
                  </span>
                </div>
                <span className="text-[12px] text-faint">
                  Select a model to filter the requests below
                </span>
              </div>
              {summary.isError ? (
                <div className="px-6 pb-5">
                  <ErrorNote error={summary.error} onRetry={() => void summary.refetch()} />
                </div>
              ) : summary.isLoading && !summary.data ? (
                <RowsSkeleton rows={3} />
              ) : (
                <ModelTable
                  rows={rows}
                  totalTokens={totalTokens}
                  colorOf={colors.colorOf}
                  selected={modelFilter}
                  onSelect={selectModel}
                />
              )}
            </Card>

            <Card>
              <div className="flex flex-wrap items-center justify-between gap-3 px-6 py-4">
                <div className="flex flex-wrap items-baseline gap-2">
                  <h2 className="text-[15px] font-semibold">Requests</h2>
                  <span className="text-[12.5px] text-faint">Most recent first</span>
                </div>
                <div className="flex flex-wrap items-center gap-2">
                  <FilterChip
                    on={!errorsOnly && !modelFilter}
                    onClick={() => {
                      setErrorsOnly(false);
                      setModelFilter(null);
                    }}
                  >
                    All {events.length}
                  </FilterChip>
                  <FilterChip
                    on={errorsOnly}
                    bad={failedEvents.length > 0}
                    onClick={() => setErrorsOnly((v) => !v)}
                  >
                    Errors {failedEvents.length}
                  </FilterChip>
                  {modelFilter ? (
                    <FilterChip
                      on
                      onClick={() => setModelFilter(null)}
                      ariaLabel={`clear model filter ${modelFilter}`}
                    >
                      <span className="max-w-[220px] truncate">{nameOf(modelFilter)}</span>
                      <span aria-hidden className="-mr-0.5 text-[14px] leading-none">
                        ×
                      </span>
                    </FilterChip>
                  ) : null}
                </div>
              </div>

              <div className="border-t border-line">
                {usage.isError ? (
                  <div className="p-5">
                    <ErrorNote error={usage.error} onRetry={() => void usage.refetch()} />
                  </div>
                ) : usage.isLoading && !usage.data ? (
                  <RowsSkeleton rows={3} />
                ) : events.length === 0 ? (
                  <div className="px-6 py-10 text-center text-[13px] text-faint">
                    No requests in this window. Try 30d.
                    {rangeId !== "30d" ? (
                      <div className="mt-3">
                        <Button variant="quiet" onClick={() => setRangeId("30d")}>
                          Widen to 30d
                        </Button>
                      </div>
                    ) : null}
                  </div>
                ) : filtered.length === 0 ? (
                  <div className="px-6 py-10 text-center text-[13px] text-faint">
                    No requests match these filters.
                    <div className="mt-3">
                      <Button
                        variant="quiet"
                        onClick={() => {
                          setErrorsOnly(false);
                          setModelFilter(null);
                        }}
                      >
                        Clear filters
                      </Button>
                    </div>
                  </div>
                ) : (
                  <div className="flex flex-wrap items-start gap-3 pb-3">
                    <div className="min-w-0 flex-[999_1_620px]">
                      <RequestLog
                        rows={logRows}
                        total={logRows.length}
                        selectedId={selected?.id ?? null}
                        onSelect={(row) => {
                          if (row.id !== null) setSelectedId(row.id);
                          setClosed(false);
                        }}
                        showAll={showAll}
                        onToggleAll={() => setShowAll((v) => !v)}
                        empty=""
                      />
                    </div>
                    <div className="min-w-0 flex-[1_1_320px] max-w-[400px] p-3 max-[1100px]:max-w-none max-[1100px]:flex-[1_1_100%]">
                      {selectedRow ? (
                        <RequestDetail
                          row={selectedRow}
                          baseUrl={baseUrl}
                          onClose={() => setClosed(true)}
                        />
                      ) : (
                        <aside
                          aria-label="request detail"
                          className="rounded-[var(--radius-card)] border border-line bg-[var(--color-surface)] px-5 py-6 text-[13px] text-faint"
                        >
                          Select a request to inspect its tokens, latency and routing.
                        </aside>
                      )}
                    </div>
                  </div>
                )}
              </div>
            </Card>
          </>
        ) : null}
      </div>
    </div>
  );
}

/* ---------------------------------------------------------------- helpers */

function totalsOf(buckets: UsageBucket[]): Totals {
  const t: Totals = { ...ZERO };
  for (const b of buckets) {
    t.requests += b.requests;
    t.input += b.input_tokens;
    t.tokens += b.input_tokens + b.output_tokens;
    t.errors += b.errors;
    t.cacheRead += b.cache_read_tokens;
    t.cacheWrite += b.cache_write_tokens;
    t.latency += b.avg_latency_ms * b.requests;
  }
  if (t.requests > 0) t.latency /= t.requests;
  return t;
}

/**
 * Share of prompt tokens served from cache. Reads are priced at a discount and
 * writes at a premium, so a low ratio is the difference between a cheap session
 * and a full-price one.
 */
function hitRate(t: Totals): number {
  const prompt = t.input + t.cacheRead + t.cacheWrite;
  return prompt > 0 ? (t.cacheRead / prompt) * 100 : 0;
}

/** Summary rows arrive sorted alphabetically; the page ranks them by traffic. */
function modelRows(rows: UsageSummaryRow[]): ModelRow[] {
  // Clients may send `gpt-5.6-luna` or `codex/gpt-5.6-luna`; both normalize to
  // one ref, so merge them instead of rendering duplicate rows.
  const merged = new Map<string, ModelRow>();
  for (const r of rows) {
    const { ref, name } = splitModel(r.provider, r.model);
    const prior = merged.get(ref);
    if (!prior) {
      merged.set(ref, { ...r, model: name, ref, tokens: r.input_tokens + r.output_tokens });
      continue;
    }
    const requests = prior.requests + r.requests;
    const latency =
      requests > 0 ? (prior.avg_latency_ms * prior.requests + r.avg_latency_ms * r.requests) / requests : 0;
    merged.set(ref, {
      ...prior,
      requests,
      errors: prior.errors + r.errors,
      input_tokens: prior.input_tokens + r.input_tokens,
      output_tokens: prior.output_tokens + r.output_tokens,
      cache_read_tokens: prior.cache_read_tokens + r.cache_read_tokens,
      cache_write_tokens: prior.cache_write_tokens + r.cache_write_tokens,
      avg_latency_ms: Math.round(latency),
      tokens: prior.tokens + r.input_tokens + r.output_tokens,
    });
  }
  return [...merged.values()].sort((a, b) => b.tokens - a.tokens || a.ref.localeCompare(b.ref));
}

/**
 * Usage rows may store the client's model string, which already carries the
 * provider prefix (`codex/gpt-5.6-luna`). This keeps one canonical ref without
 * ever doubling the prefix; the name is what the tables show under `provider/`.
 */
function splitModel(provider: string, model: string): { ref: string; name: string } {
  const prefix = `${provider}/`;
  return model.startsWith(prefix)
    ? { ref: model, name: model.slice(prefix.length) }
    : { ref: prefix + model, name: model };
}

/** Everything after the provider prefix: one label for chips and legends. */
function nameOf(ref: string): string {
  const slash = ref.indexOf("/");
  return slash === -1 ? ref : ref.slice(slash + 1);
}

function isFailure(row: UsageEvent): boolean {
  return row.status >= 400 || row.error !== null;
}

function dominantStatus(rows: UsageEvent[]): number | null {
  const counts = new Map<number, number>();
  for (const r of rows) counts.set(r.status, (counts.get(r.status) ?? 0) + 1);
  let best: number | null = null;
  let bestCount = 0;
  for (const [status, count] of counts) {
    if (count > bestCount) {
      best = status;
      bestCount = count;
    }
  }
  return best;
}

/**
 * One chart cell per aligned bucket in the window, zero cells included. Parts
 * come from the server-side per-model split; when that query has no data the
 * chart falls back to one total-coloured segment per bucket (never a guess).
 */
function buildChart({
  spec,
  now,
  alignedFrom,
  series,
  byModel,
  metric,
  rankedRefs,
}: {
  spec: RangeSpec;
  now: number;
  alignedFrom: number;
  series: UsageBucket[];
  byModel: UsageModelBucket[];
  metric: Metric;
  rankedRefs: string[];
}): TrafficBucket[] {
  const starts = new Set<number>(windowStarts(spec, now));
  const totals = new Map<number, UsageBucket>();
  for (const b of series) {
    if (b.start < alignedFrom) continue;
    totals.set(b.start, b);
    starts.add(b.start);
  }

  const parts = new Map<number, Map<string, number>>();
  const partErrors = new Map<number, number>();
  let hasModelRows = false;
  for (const b of byModel) {
    if (b.start < alignedFrom) continue;
    hasModelRows = true;
    starts.add(b.start);
    const ref = splitModel(b.provider, b.model).ref;
    let cell = parts.get(b.start);
    if (!cell) {
      cell = new Map<string, number>();
      parts.set(b.start, cell);
    }
    const value = metric === "requests" ? b.requests : b.input_tokens + b.output_tokens;
    cell.set(ref, (cell.get(ref) ?? 0) + value);
    partErrors.set(b.start, (partErrors.get(b.start) ?? 0) + b.errors);
  }

  return [...starts].sort((a, b) => a - b).map((start) => {
    const total = totals.get(start);
    const cell = parts.get(start);
    let segments: TrafficPart[] = [];
    if (cell && cell.size > 0) {
      segments = rankedRefs
        .filter((ref) => (cell.get(ref) ?? 0) > 0)
        .map((ref) => ({ id: ref, value: cell.get(ref) ?? 0 }));
    } else if (!hasModelRows && total) {
      const value = metric === "requests" ? total.requests : total.input_tokens + total.output_tokens;
      if (value > 0) segments = [{ id: ALL, value }];
    }
    return {
      label: bucketLabel(start, spec.seconds, now),
      full: bucketRangeLabel(start, spec.seconds),
      parts: segments,
      total: segments.reduce((n, p) => n + p.value, 0),
      errors: total?.errors ?? partErrors.get(start) ?? 0,
    };
  });
}

/**
 * The clock lives in its own component so a one-second tick never re-renders
 * the page, its charts or the selected row.
 */
function LiveStamp({ at, waiting }: { at: number; waiting: boolean }) {
  const [, setTick] = useState(0);
  useEffect(() => {
    const id = window.setInterval(() => setTick((n) => n + 1), 1000);
    return () => window.clearInterval(id);
  }, []);

  const secs = at > 0 ? Math.max(0, Math.floor((Date.now() - at) / 1000)) : 0;
  const text = waiting
    ? "Waiting for the first request"
    : at === 0
      ? "Live · connecting"
      : secs < 5
        ? "Live · updated just now"
        : `Live · updated ${secs}s ago`;

  return (
    <span className="inline-flex items-center gap-2 text-[12px] text-muted">
      <span
        aria-hidden
        className={cx(
          "h-2 w-2 rounded-full",
          waiting || at === 0 ? "bg-faint" : "bg-accent pulse",
        )}
      />
      <span>{text}</span>
    </span>
  );
}

function Skeleton({
  height,
  width = "100%",
  className,
}: {
  height: number;
  width?: number | string;
  className?: string;
}) {
  return (
    <div
      aria-hidden
      className={cx("rounded bg-[var(--color-surface-2)]", className)}
      style={{ height, width }}
    />
  );
}

function ReadoutSkeleton() {
  return (
    <div className="px-6 py-5">
      <Skeleton height={11} width={64} />
      <div className="mt-3">
        <Skeleton height={30} width={104} />
      </div>
      <div className="mt-3">
        <Skeleton height={11} width={148} />
      </div>
    </div>
  );
}

function RowsSkeleton({ rows }: { rows: number }) {
  return (
    <div aria-hidden className="divide-y divide-[var(--color-rule)]">
      {Array.from({ length: rows }, (_, i) => (
        <div key={i} className="flex items-center gap-3 px-6 py-3.5">
          <Skeleton height={12} width="38%" />
          <div className="ml-auto">
            <Skeleton height={12} width={72} />
          </div>
        </div>
      ))}
    </div>
  );
}
