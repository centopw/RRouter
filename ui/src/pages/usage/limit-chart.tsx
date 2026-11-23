/**
 * The headline window over time: sampled usage (solid, broken at days without samples),
 * the straight-line forecast to reset from pace.ts (dashed, omitted when there is none),
 * the 100% limit (dotted) and, when the forecast crosses it before reset, a ring at the
 * crossing with the time past the limit hatched. Inline SVG sized to its container; no
 * animation, so `prefers-reduced-motion` has nothing to turn off. Everything the chart
 * shows is also in its text summary (`role="img"`) and the visible footer.
 */

import { useCallback, useId, useState } from "react";
import type { AccountUsageHistory, SubscriptionWindow } from "../../lib/api";
import { cx } from "../../components/ui";
import { actualSegments, fitLabels, historyDrawable, linePath, scale, yTicks, type XLabel } from "./chart-geometry";
import { isAlarming, type Pace } from "./pace";
import { shortDate } from "./quota";

const HEIGHT = 168;
const PAD = { top: 14, right: 12, bottom: 22, left: 36 };
/** Labels closer than this give way to the more important one. */
const LABEL_GAP = 52;
/** Windows shorter than this have at most one daily sample: nothing to draw. */
const MIN_CHART_WINDOW_S = 2 * 86_400;

/** Whether a daily-sampled chart can say anything about this window. */
export function chartable(window: SubscriptionWindow): boolean {
  return window.window_seconds === null || window.window_seconds >= MIN_CHART_WINDOW_S;
}

function percentText(value: number): string {
  return value < 100 ? `${value.toFixed(1)}%` : `${Math.round(value)}%`;
}

/**
 * Width of the element in px, kept current with ResizeObserver. A callback ref, so it
 * attaches whenever the element mounts (it does not exist while history is collecting).
 */
function useWidth(): [(element: HTMLDivElement | null) => (() => void) | undefined, number] {
  const [width, setWidth] = useState(0);
  const ref = useCallback((element: HTMLDivElement | null) => {
    if (!element) return undefined;
    const observer = new ResizeObserver(([entry]) => {
      if (entry) setWidth(Math.round(entry.contentRect.width));
    });
    observer.observe(element);
    return () => observer.disconnect();
  }, []);
  return [ref, width];
}

function Swatch({ dash, className }: { dash?: string; className: string }) {
  return (
    <svg width="18" height="6" aria-hidden="true" className="shrink-0">
      <line
        x1="1"
        y1="3"
        x2="17"
        y2="3"
        strokeWidth="2"
        strokeLinecap="round"
        strokeDasharray={dash}
        className={className}
      />
    </svg>
  );
}

export function LimitChart({
  window,
  label,
  pace,
  history,
  now,
}: {
  window: SubscriptionWindow;
  /** The window's display name, e.g. "Weekly". */
  label: string;
  pace: Pace;
  history: AccountUsageHistory;
  now: number;
}) {
  const [ref, width] = useWidth();
  const patternId = `limit-hatch-${useId().replace(/[^\w-]/g, "")}`;

  if (!historyDrawable(history, window)) {
    return <p className="text-small text-faint">Collecting usage history…</p>;
  }

  const firstDay = Math.min(...history.data.map((point) => point.day)) * 1000;
  const startMs = history.window_start !== null ? history.window_start * 1000 : (pace.startMs ?? firstDay);
  const resetMs = window.resets_at !== null ? window.resets_at * 1000 : null;
  const endMs = resetMs !== null && resetMs > now ? resetMs : now;

  const used = window.used_percent;
  const segments = actualSegments(history.data, startMs, now, used);
  const forecast =
    resetMs !== null && pace.projectedAtReset !== null && (pace.state === "ok" || pace.state === "on_pace_to_hit")
      ? pace.projectedAtReset
      : null;
  const hit = pace.limitHitAt !== null && pace.limitHitAt <= endMs ? Math.max(pace.limitHitAt, startMs) : null;
  const alarm = isAlarming(pace.state);
  const approx = pace.approximate ? "~" : "";

  const sampledMax = Math.max(...segments.flat().map((point) => point.v));
  const { top, ticks } = yTicks(Math.max(sampledMax, forecast ?? 0));

  const right = Math.max(PAD.left + 1, width - PAD.right);
  const x = scale(startMs, endMs, PAD.left, right);
  const y = scale(0, top, HEIGHT - PAD.bottom, PAD.top);
  const latest = segments[segments.length - 1]!.at(-1)!;

  const candidates: XLabel[] = [
    { key: "start", x: x(startMs), text: shortDate(startMs), priority: 3 },
    { key: "today", x: x(now), text: "Today", priority: 1 },
  ];
  if (resetMs !== null) candidates.push({ key: "reset", x: x(endMs), text: shortDate(endMs), priority: 2 });
  if (hit !== null && pace.state === "on_pace_to_hit") {
    candidates.push({ key: "limit", x: x(hit), text: `${approx}${shortDate(hit)}`, priority: 0 });
  }
  const xLabels = width > 0 ? fitLabels(candidates, LABEL_GAP) : [];

  const forecastText =
    forecast !== null && resetMs !== null
      ? ` Forecast ${approx}${Math.round(forecast)}% at reset on ${shortDate(resetMs)}${
          pace.state === "on_pace_to_hit" && hit !== null ? `, crossing the limit ${approx}${shortDate(hit)}` : ""
        }.`
      : "";
  const gaps = segments.length > 1 ? " Days without samples are left blank." : "";
  const summary =
    `${label} usage since ${shortDate(startMs)}: ${Math.round(used)}% used now` +
    (pace.paceDaily !== null ? `, averaging ${percentText(pace.paceDaily)} a day.` : ".") +
    (pace.state === "exhausted" ? " The limit is reached." : forecastText) +
    gaps;

  const forecastStroke = alarm ? "stroke-[var(--color-warn)]" : "stroke-[var(--color-muted)]";

  return (
    <figure className="flex flex-col gap-2">
      <div ref={ref} className="w-full" style={{ height: HEIGHT }}>
        {width > 0 ? (
          <svg width={width} height={HEIGHT} role="img" aria-label={summary} className="block overflow-visible">
            <defs>
              <pattern id={patternId} width="6" height="6" patternUnits="userSpaceOnUse" patternTransform="rotate(45)">
                <line x1="0" y1="0" x2="0" y2="6" strokeWidth="1.5" className="stroke-[var(--color-warn-line)]" />
              </pattern>
            </defs>

            {hit !== null ? (
              <rect
                x={x(hit)}
                y={PAD.top}
                width={Math.max(0, x(endMs) - x(hit))}
                height={HEIGHT - PAD.top - PAD.bottom}
                fill={`url(#${patternId})`}
              />
            ) : null}

            {ticks.map((tick) => (
              <g key={tick}>
                {tick !== 100 ? (
                  <line x1={PAD.left} x2={right} y1={y(tick)} y2={y(tick)} className="stroke-[var(--color-rule)]" />
                ) : null}
                <text
                  x={PAD.left - 6}
                  y={y(tick)}
                  dy="0.32em"
                  textAnchor="end"
                  className="num fill-[var(--color-faint)] text-caption"
                >
                  {tick}%
                </text>
              </g>
            ))}

            <line
              x1={PAD.left}
              x2={right}
              y1={y(100)}
              y2={y(100)}
              strokeWidth="1.5"
              strokeDasharray="1 4"
              strokeLinecap="round"
              className="stroke-[var(--color-muted)]"
            />
            <text x={PAD.left + 4} y={y(100) - 5} className="fill-[var(--color-muted)] text-caption">
              Limit
            </text>

            {forecast !== null ? (
              <>
                <line
                  x1={x(now)}
                  y1={y(used)}
                  x2={x(endMs)}
                  y2={y(forecast)}
                  strokeWidth="2"
                  strokeDasharray="5 4"
                  strokeLinecap="round"
                  className={forecastStroke}
                />
                <text
                  x={x(endMs)}
                  y={y(forecast) - 7}
                  textAnchor="end"
                  className={cx(
                    "num text-small font-medium",
                    alarm ? "fill-[var(--color-warn)]" : "fill-[var(--color-muted)]",
                  )}
                >
                  {approx}
                  {Math.round(forecast)}%
                </text>
              </>
            ) : null}

            {segments.map((run, index) =>
              run.length > 1 ? (
                <path
                  key={index}
                  d={linePath(run, x, y)}
                  fill="none"
                  strokeWidth="2"
                  strokeLinejoin="round"
                  strokeLinecap="round"
                  className="stroke-[var(--color-pos)]"
                />
              ) : (
                <circle key={index} cx={x(run[0]!.t)} cy={y(run[0]!.v)} r="2" className="fill-[var(--color-pos)]" />
              ),
            )}
            <circle
              cx={x(latest.t)}
              cy={y(latest.v)}
              r="3.5"
              strokeWidth="2"
              className="fill-[var(--color-pos)] stroke-[var(--color-surface)]"
            />

            {hit !== null && pace.state === "on_pace_to_hit" ? (
              <circle
                cx={x(hit)}
                cy={y(100)}
                r="5.5"
                strokeWidth="2"
                className="fill-[var(--color-surface)] stroke-[var(--color-warn)]"
              />
            ) : null}

            {xLabels.map((tick) => {
              const anchor = tick.x - PAD.left < 24 ? "start" : right - tick.x < 24 ? "end" : "middle";
              return (
                <text
                  key={tick.key}
                  x={tick.x}
                  y={HEIGHT - 6}
                  textAnchor={anchor}
                  className={cx(
                    "num text-caption",
                    tick.key === "limit" ? "fill-[var(--color-warn)] font-semibold" : "fill-[var(--color-faint)]",
                  )}
                >
                  {tick.text}
                </text>
              );
            })}
          </svg>
        ) : null}
      </div>

      <figcaption className="flex flex-wrap items-center justify-between gap-x-4 gap-y-1 text-small text-muted">
        <span className="num">{pace.paceDaily !== null ? `Averaging ${percentText(pace.paceDaily)} a day` : ""}</span>
        <span className="flex flex-wrap items-center gap-x-3 gap-y-1">
          <span className="inline-flex items-center gap-1.5">
            <Swatch className="stroke-[var(--color-pos)]" />
            Usage
          </span>
          {forecast !== null ? (
            <span className="inline-flex items-center gap-1.5">
              <Swatch dash="4 3" className={forecastStroke} />
              Forecast
            </span>
          ) : null}
          <span className="inline-flex items-center gap-1.5">
            <Swatch dash="1 4" className="stroke-[var(--color-muted)]" />
            Limit
          </span>
        </span>
      </figcaption>
    </figure>
  );
}
