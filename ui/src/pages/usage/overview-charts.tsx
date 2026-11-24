/**
 * Inline SVG charts for the `/usage` Overview page:
 * - `MiniSparkline`: subtle trend sparkline inside KPI tiles (only drawn from real series data).
 * - `UsageDonut`: provider share donut ring with center total and legend.
 * - `UsageTrendCard`: multi-line Total + top-3-providers trend chart over the active range.
 *
 * Uses existing CSS theme tokens (`--color-series-*`, `--color-rule`, `--color-faint`) so
 * both Cool-Slate Light and Obsidian-Slate Dark render with high contrast and zero new deps.
 */

import { useCallback, useId, useState } from "react";
import { Link } from "react-router-dom";
import { ChartLineUp } from "@phosphor-icons/react";
import { Card, CardHeader } from "../../components/ui";
import { bucketLabel, niceMax, xLabelIndices, type RangeSpec } from "../../lib/activity";
import { compact } from "../../lib/format";
import { fitLabels, scale } from "./chart-geometry";
import type { UsageBreakdown, UsageTrend } from "./overview-model";

function useContainerWidth(): [(el: HTMLDivElement | null) => (() => void) | undefined, number] {
  const [width, setWidth] = useState(0);
  const ref = useCallback((el: HTMLDivElement | null) => {
    if (!el) return undefined;
    const initial = Math.round(el.getBoundingClientRect().width);
    if (initial > 0) setWidth(initial);
    const observer = new ResizeObserver(([entry]) => {
      if (entry) setWidth(Math.round(entry.contentRect.width));
    });
    observer.observe(el);
    return () => observer.disconnect();
  }, []);
  return [ref, width];
}

/** Build a smooth cubic spline SVG path through (x, y) points, clamped against overshoot. */
function smoothPath(points: Array<{ x: number; y: number }>, yBottom: number): string {
  if (points.length === 0) return "";
  if (points.length === 1) return `M${points[0]!.x.toFixed(1)},${points[0]!.y.toFixed(1)}`;
  let d = `M${points[0]!.x.toFixed(1)},${points[0]!.y.toFixed(1)}`;
  for (let i = 0; i < points.length - 1; i += 1) {
    const p0 = points[i]!;
    const p1 = points[i + 1]!;
    const midX = (p0.x + p1.x) / 2;
    const y0 = Math.min(yBottom, p0.y);
    const y1 = Math.min(yBottom, p1.y);
    d += ` C${midX.toFixed(1)},${y0.toFixed(1)} ${midX.toFixed(1)},${y1.toFixed(1)} ${p1.x.toFixed(1)},${y1.toFixed(1)}`;
  }
  return d;
}

/** Small sparkline from real series values; returns null when there is no real variation. */
export function MiniSparkline({
  values,
  color,
  width = 84,
  height = 30,
}: {
  values: ReadonlyArray<number>;
  color: string;
  width?: number;
  height?: number;
}) {
  const gradId = `spark-${useId().replace(/[^\w-]/g, "")}`;
  const max = Math.max(0, ...values);
  if (values.length < 2 || max <= 0) return null;

  const padY = 3;
  const x = scale(0, values.length - 1, 1, width - 1);
  const y = scale(0, max, height - padY, padY);
  const pts = values.map((v, i) => ({ x: x(i), y: y(v) }));
  const line = smoothPath(pts, height - padY);
  const area = `${line} L${pts[pts.length - 1]!.x.toFixed(1)},${height} L${pts[0]!.x.toFixed(1)},${height} Z`;

  return (
    <svg
      width={width}
      height={height}
      aria-hidden="true"
      className="shrink-0 overflow-visible"
    >
      <defs>
        <linearGradient id={gradId} x1="0" y1="0" x2="0" y2="1">
          <stop offset="0%" stopColor={color} stopOpacity="0.24" />
          <stop offset="100%" stopColor={color} stopOpacity="0" />
        </linearGradient>
      </defs>
      <path d={area} fill={`url(#${gradId})`} />
      <path d={line} fill="none" stroke={color} strokeWidth="1.75" strokeLinecap="round" />
    </svg>
  );
}

/** Provider usage breakdown donut card (tokens or requests across all providers). */
export function UsageBreakdownCard({
  breakdown,
  rangeLabel,
}: {
  breakdown: UsageBreakdown;
  rangeLabel: string;
}) {
  const size = 112;
  const stroke = 14;
  const radius = (size - stroke) / 2;
  const circumference = 2 * Math.PI * radius;
  const cx = size / 2;
  const cy = size / 2;

  const metricLabel = breakdown.metric === "tokens" ? "tokens" : "requests";
  const ariaSummary =
    breakdown.slices.length > 0
      ? `Usage breakdown (${rangeLabel}, ${metricLabel}): ${breakdown.slices
          .map((s) => `${s.label} ${s.share.toFixed(1)}%`)
          .join(", ")}`
      : `No provider usage recorded in the last ${rangeLabel}`;

  let offset = 0;
  const arcs = breakdown.slices.map((slice) => {
    const length = (slice.share / 100) * circumference;
    const gap = breakdown.slices.length > 1 ? Math.min(2.5, length * 0.15) : 0;
    const arc = {
      ...slice,
      dashArray: `${Math.max(0, length - gap).toFixed(2)} ${circumference.toFixed(2)}`,
      dashOffset: (-offset).toFixed(2),
    };
    offset += length;
    return arc;
  });

  return (
    <Card className="flex min-w-0 flex-col gap-3 p-4">
      <CardHeader
        title="Usage Breakdown"
        padding="none"
        action={<span className="text-caption text-faint">Last {rangeLabel}</span>}
      />

      {breakdown.slices.length === 0 ? (
        <p className="py-6 text-center text-small text-faint">
          No provider traffic in the last {rangeLabel}.
        </p>
      ) : (
        <div className="flex flex-wrap items-center justify-between gap-3 sm:flex-nowrap">
          <div className="relative mx-auto shrink-0 sm:mx-0" style={{ width: size, height: size }}>
            <svg width={size} height={size} role="img" aria-label={ariaSummary} className="block">
              <circle
                cx={cx}
                cy={cy}
                r={radius}
                fill="none"
                stroke="var(--color-rule)"
                strokeWidth={stroke}
              />
              <g transform={`rotate(-90 ${cx} ${cy})`}>
                {arcs.map((arc) => (
                  <circle
                    key={arc.id}
                    cx={cx}
                    cy={cy}
                    r={radius}
                    fill="none"
                    stroke={arc.color}
                    strokeWidth={stroke}
                    strokeDasharray={arc.dashArray}
                    strokeDashoffset={arc.dashOffset}
                  />
                ))}
              </g>
            </svg>
            <div className="pointer-events-none absolute inset-0 flex flex-col items-center justify-center text-center">
              <span className="text-lead font-semibold leading-tight text-fg tnum">
                {compact(breakdown.total)}
              </span>
              <span className="text-caption text-faint">{metricLabel}</span>
            </div>
          </div>

          <ul role="list" className="flex min-w-0 flex-1 flex-col gap-2 text-small">
            {breakdown.slices.map((slice) => (
              <li key={slice.id} className="flex items-center justify-between gap-2">
                <span className="flex min-w-0 items-center gap-2">
                  <span
                    aria-hidden
                    className="h-2.5 w-2.5 shrink-0 rounded-full"
                    style={{ backgroundColor: slice.color }}
                  />
                  <span className="truncate text-ink-soft" title={slice.label}>
                    {slice.label}
                  </span>
                </span>
                <span className="shrink-0 font-medium text-fg tnum">
                  {slice.share.toFixed(1)}%
                </span>
              </li>
            ))}
          </ul>
        </div>
      )}
    </Card>
  );
}

const TREND_HEIGHT = 200;
const TREND_PAD = { top: 18, right: 16, bottom: 24, left: 46 };
/** Minimum px between Usage Trend x-axis labels so dates/times never collide on narrow viewports. */
const TREND_X_LABEL_MIN_GAP = 72;

/** Multi-line Usage Trend card: Total + top 3 providers across the active range. */
export function UsageTrendCard({
  trend,
  range,
  now,
  loading,
}: {
  trend: UsageTrend;
  range: RangeSpec;
  now: number;
  loading: boolean;
}) {
  const [ref, width] = useContainerWidth();
  const gradId = `trend-total-${useId().replace(/[^\w-]/g, "")}`;
  const nowSec = Math.floor(now / 1000);

  const metricLabel = trend.metric === "tokens" ? "tokens" : "requests";
  const top = niceMax(trend.maxValue);
  const yTicks = [0, top * 0.25, top * 0.5, top * 0.75, top];
  const right = Math.max(TREND_PAD.left + 1, width - TREND_PAD.right);
  const bottom = TREND_HEIGHT - TREND_PAD.bottom;
  const x = scale(0, Math.max(1, trend.starts.length - 1), TREND_PAD.left, right);
  const y = scale(0, top, bottom, TREND_PAD.top);

  const totalPoints = trend.total.map((v, i) => ({ x: x(i), y: y(v) }));
  const totalLine = smoothPath(totalPoints, bottom);
  const totalArea =
    totalPoints.length >= 2
      ? `${totalLine} L${totalPoints[totalPoints.length - 1]!.x.toFixed(1)},${bottom} L${totalPoints[0]!.x.toFixed(1)},${bottom} Z`
      : "";

  const lastIdx = trend.starts.length - 1;
  const xLabels =
    width > 0
      ? fitLabels(
          xLabelIndices(trend.starts.length, range.labelEvery).flatMap((idx) => {
            const start = trend.starts[idx];
            if (start === undefined) return [];
            const isEdge = idx === 0 || idx === lastIdx;
            return [
              {
                key: String(idx),
                x: x(idx),
                text: bucketLabel(start, range.seconds, nowSec),
                priority: isEdge ? 0 : 1,
              },
            ];
          }),
          TREND_X_LABEL_MIN_GAP,
        )
      : [];
  const latestTotal = trend.total[trend.total.length - 1] ?? 0;
  const lastPt = totalPoints[totalPoints.length - 1];

  const ariaLabel =
    trend.maxValue > 0
      ? `Usage trend over ${range.id} (${metricLabel}): peak ${compact(trend.maxValue)}, latest ${compact(latestTotal)}`
      : `No usage recorded in the last ${range.id}`;

  return (
    <Card className="flex min-w-0 flex-col gap-3 p-4">
      <CardHeader
        icon={<ChartLineUp size={16} />}
        title="Usage Trend"
        meta={`(${metricLabel})`}
        padding="none"
        action={
          <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-caption">
            <span className="inline-flex items-center gap-1.5 text-muted">
              <span
                aria-hidden
                className="h-2 w-2 rounded-full"
                style={{ backgroundColor: "var(--color-series-1)" }}
              />
              Total
            </span>
            {trend.providers.map((series) => (
              <span key={series.id} className="inline-flex items-center gap-1.5 text-muted">
                <span
                  aria-hidden
                  className="h-2 w-2 rounded-full"
                  style={{ backgroundColor: series.color }}
                />
                {series.label}
              </span>
            ))}
            <Link
              to="/activity"
              className="text-accent-text underline-offset-2 hover:underline focus-visible:underline"
            >
              View details →
            </Link>
          </div>
        }
      />

      {loading && trend.maxValue === 0 ? (
        <div
          className="flex items-center justify-center text-small text-faint"
          style={{ height: TREND_HEIGHT }}
        >
          Loading usage trend…
        </div>
      ) : trend.maxValue === 0 ? (
        <div
          className="flex items-center justify-center text-small text-faint"
          style={{ height: TREND_HEIGHT }}
        >
          No traffic recorded in the last {range.id}.
        </div>
      ) : (
        <div ref={ref} className="w-full" style={{ height: TREND_HEIGHT }}>
          {width > 0 ? (
            <svg
              width={width}
              height={TREND_HEIGHT}
              role="img"
              aria-label={ariaLabel}
              className="block overflow-visible"
            >
              <defs>
                <linearGradient id={gradId} x1="0" y1="0" x2="0" y2="1">
                  <stop offset="0%" stopColor="var(--color-series-1)" stopOpacity="0.18" />
                  <stop offset="100%" stopColor="var(--color-series-1)" stopOpacity="0" />
                </linearGradient>
              </defs>

              {yTicks.map((tick, idx) => (
                <g key={idx}>
                  <line
                    x1={TREND_PAD.left}
                    x2={right}
                    y1={y(tick)}
                    y2={y(tick)}
                    className="stroke-[var(--color-rule)]"
                  />
                  <text
                    x={TREND_PAD.left - 8}
                    y={y(tick)}
                    dy="0.32em"
                    textAnchor="end"
                    className="fill-[var(--color-faint)] text-caption tnum"
                  >
                    {compact(Math.round(tick))}
                  </text>
                </g>
              ))}

              {totalArea ? <path d={totalArea} fill={`url(#${gradId})`} /> : null}

              {trend.providers.map((series) => {
                const pts = series.values.map((v, i) => ({ x: x(i), y: y(v) }));
                return (
                  <path
                    key={series.id}
                    d={smoothPath(pts, bottom)}
                    fill="none"
                    stroke={series.color}
                    strokeWidth="1.5"
                    strokeOpacity="0.85"
                    strokeLinecap="round"
                  />
                );
              })}

              <path
                d={totalLine}
                fill="none"
                stroke="var(--color-series-1)"
                strokeWidth="2.25"
                strokeLinecap="round"
              />

              {lastPt ? (
                <g>
                  <circle
                    cx={lastPt.x}
                    cy={lastPt.y}
                    r="3.5"
                    fill="var(--color-series-1)"
                    stroke="var(--color-surface)"
                    strokeWidth="2"
                  />
                  {(() => {
                    const badgeText = compact(latestTotal);
                    const badgeW = Math.max(38, badgeText.length * 7 + 14);
                    const badgeH = 20;
                    const bx = Math.max(TREND_PAD.left, lastPt.x - badgeW);
                    const by = Math.max(2, lastPt.y - badgeH - 8);
                    return (
                      <g>
                        <rect
                          x={bx}
                          y={by}
                          width={badgeW}
                          height={badgeH}
                          rx="6"
                          fill="var(--color-accent)"
                        />
                        <text
                          x={bx + badgeW / 2}
                          y={by + badgeH / 2 + 0.5}
                          dy="0.32em"
                          textAnchor="middle"
                          className="fill-[var(--color-accent-ink)] text-caption font-semibold tnum"
                        >
                          {badgeText}
                        </text>
                      </g>
                    );
                  })()}
                </g>
              ) : null}

              {xLabels.map((label) => {
                const idx = Number(label.key);
                const anchor = idx === 0 ? "start" : idx === lastIdx ? "end" : "middle";
                return (
                  <text
                    key={label.key}
                    x={label.x}
                    y={TREND_HEIGHT - 6}
                    textAnchor={anchor}
                    className="fill-[var(--color-faint)] text-caption tnum"
                  >
                    {label.text}
                  </text>
                );
              })}
            </svg>
          ) : null}
        </div>
      )}
    </Card>
  );
}
