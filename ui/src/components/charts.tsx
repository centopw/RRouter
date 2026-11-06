import type { ReactNode } from "react";
import { compact, pct, usd } from "../lib/format";
import { niceMax, peakIndex, xLabelIndices } from "../lib/activity";
import { Dot, cx } from "./ui";

/**
 * Dependency-free charts. The dashboard draws a few dozen points; a chart library
 * would cost more bytes than the whole panel, so these are hand-built SVG.
 */

export function Sparkline({
  values,
  tone = "pos",
  width = 88,
  height = 28,
  fill = true,
}: {
  values: number[];
  tone?: "pos" | "neg" | "accent";
  width?: number;
  height?: number;
  fill?: boolean;
}) {
  // One series, one color: cyan for traffic, red only for an error series.
  const color = { pos: "var(--color-accent)", neg: "var(--color-neg)", accent: "var(--color-accent)" }[tone];
  const pts = normalize(values, width, height);
  if (pts.length < 2) return <div style={{ width, height }} aria-hidden />;

  const line = pts.map((p, i) => `${i === 0 ? "M" : "L"}${p.x.toFixed(2)},${p.y.toFixed(2)}`).join(" ");
  const area = `${line} L${width},${height} L0,${height} Z`;

  return (
    <svg width={width} height={height} viewBox={`0 0 ${width} ${height}`} role="img" aria-label="trend">
      {fill ? <path d={area} fill={color} fillOpacity="0.12" /> : null}
      <path d={line} fill="none" stroke={color} strokeWidth="1.5" strokeLinejoin="round" strokeLinecap="round" />
    </svg>
  );
}

function normalize(values: number[], w: number, h: number): Array<{ x: number; y: number }> {
  if (values.length === 0) return [];
  const max = Math.max(...values);
  const min = Math.min(...values);
  const span = max - min || 1;
  const step = values.length > 1 ? w / (values.length - 1) : w;
  return values.map((v, i) => ({
    x: i * step,
    y: h - 2 - ((v - min) / span) * (h - 4),
  }));
}

export type Bar = { label: string; value: number; highlight?: boolean };

/**
 * `fill` stretches bars across the panel: use it for contiguous time buckets.
 * Without it each bar keeps a fixed width, which is right for categories such as
 * models, where two rows must not become two half-panel columns.
 */
export function Bars({ bars, height = 220, fill = false }: { bars: Bar[]; height?: number; fill?: boolean }) {
  if (bars.length === 0) {
    return <div className="px-5 py-10 text-center text-[13px] text-faint">no usage in this window</div>;
  }
  const max = Math.max(...bars.map((b) => b.value), 1);
  const ticks = 4;
  return (
    <div className="px-5 pb-4">
      <div className="relative" style={{ height }}>
        {Array.from({ length: ticks + 1 }, (_, i) => {
          const value = (max / ticks) * (ticks - i);
          return (
            <div
              key={i}
              className="absolute inset-x-0 flex items-center gap-2 text-[10px] text-faint"
              style={{ top: `${(i / ticks) * 100}%` }}
            >
              <span className="w-10 shrink-0 text-right tnum">{compact(Math.round(value))}</span>
              <span className="h-px flex-1 bg-[var(--color-rule)]" />
            </div>
          );
        })}
        {/* Category bars: fixed width and left-aligned, so two models do not
            stretch into two halves of the panel. */}
        <div
          className={
            fill
              ? "absolute inset-y-0 left-12 right-0 flex items-end gap-1"
              : "absolute inset-y-0 left-12 right-0 flex items-end gap-4 overflow-x-auto"
          }
        >
          {bars.map((b, i) => (
            <div
              key={`${b.label}-${i}`}
              className={
                fill
                  ? "group flex h-full min-w-0 flex-1 items-end justify-start"
                  : "group flex h-full w-[46px] shrink-0 items-end"
              }
              title={`${b.label}: ${b.value.toLocaleString()} tokens`}
            >
              <div
                className={
                  fill
                    ? "w-full max-w-[40px] rounded-t-[3px] transition-opacity group-hover:opacity-80"
                    : "w-full rounded-t-[3px] transition-opacity group-hover:opacity-80"
                }
                style={{
                  height: `${Math.max(2, (b.value / max) * 100)}%`,
                  background: "var(--color-accent)",
                  opacity: b.highlight ? 1 : 0.45,
                }}
              />
            </div>
          ))}
        </div>
      </div>
      <div className={fill ? "mt-2 flex justify-between pl-12 text-[10px] text-faint" : "mt-2 flex gap-4 overflow-hidden pl-12"}>
        {fill ? (
          <>
            <span>{bars[0]?.label}</span>
            {bars.length > 1 ? <span>{bars[bars.length - 1]?.label}</span> : null}
          </>
        ) : (
          bars.map((b, i) => (
            <div key={`${b.label}-label-${i}`} className="w-[46px] shrink-0 truncate text-[10px] text-faint" title={b.label}>
              {shortId(b.label)}
            </div>
          ))
        )}
      </div>
    </div>
  );
}

/** `codex/gpt-5.6-luna` -> `gpt-5.6-luna`; the provider is already the row's context. */
function shortId(ref: string): string {
  const slash = ref.indexOf("/");
  return slash === -1 ? ref : ref.slice(slash + 1);
}

/** Big number with a period-over-period delta and an optional sparkline slot. */
export function Metric({
  label,
  value,
  delta,
  series,
  tone,
  hint,
}: {
  label: string;
  value: string;
  delta?: number | null;
  series?: number[];
  tone?: "pos" | "neg" | "accent";
  hint?: string;
}) {
  const up = (delta ?? 0) >= 0;
  return (
    <div className="rounded-[var(--radius-card)] border border-line bg-surface px-4 py-3.5">
      <div className="flex items-start justify-between gap-2">
        <span className="text-[12px] text-muted">{label}</span>
        {series && series.length > 1 ? (
          <Sparkline values={series} tone={tone ?? (up ? "pos" : "neg")} width={72} height={22} />
        ) : null}
      </div>
      <div className="mt-2 text-[26px] font-semibold leading-none tracking-[-0.02em] tnum">{value}</div>
      <div className="mt-2 flex items-center gap-1.5 text-[11px]">
        {delta !== null && delta !== undefined ? (
          <>
            <span className={up ? "text-pos" : "text-neg"}>
              {up ? "↑" : "↓"} {pct(Math.abs(delta))}
            </span>
            <span className="text-faint">vs prev period</span>
          </>
        ) : (
          <span className="text-faint">{hint ?? "\u00a0"}</span>
        )}
      </div>
    </div>
  );
}

/** A single number with its label and an optional second line. */
export function Readout({
  label,
  value,
  note,
  dot,
  tone,
  delta,
}: {
  label: string;
  value: string;
  note?: ReactNode;
  dot?: "pos" | "neg" | "warn" | "muted";
  tone?: "neg" | "fg";
  delta?: { up: boolean; text: string } | null;
}) {
  return (
    <div className="px-6 py-5">
      <div className="text-[13px] text-muted">{label}</div>
      <div
        className={cx(
          "mt-2 font-mono text-[34px] font-medium leading-none tracking-[-0.02em] tnum",
          tone === "neg" ? "text-neg" : "text-fg",
        )}
      >
        {value}
      </div>
      <div className="mt-2.5 flex min-h-[16px] flex-wrap items-center gap-1.5 text-[12.5px] text-faint">
        {dot ? <Dot tone={dot} /> : null}
        {note}
        {delta ? (
          <span className={delta.up && tone === "neg" ? "text-neg" : "text-muted"}>{delta.text}</span>
        ) : null}
      </div>
    </div>
  );
}

export type TrafficPart = { id: string; value: number };

export type TrafficBucket = {
  /** Short x-axis label. */
  label: string;
  /** Tooltip heading, e.g. "Aug 3, 14:00–15:00". */
  full: string;
  parts: TrafficPart[];
  total: number;
  errors: number;
};

const AXIS_WIDTH = 44;

/** What a chart column measures; `cost` values are USD micros, printed with `usd`. */
type TrafficUnit = "requests" | "tokens" | "cost";

/** One chart number in the current unit: money as USD, traffic as a compact count. */
function amount(value: number, unit: TrafficUnit, round: boolean): string {
  if (unit === "cost") return usd(Math.round(value));
  return compact(round ? Math.round(value) : value);
}

/**
 * Stacked traffic chart: one column per bucket, one segment per model, an error lane
 * under the plot, and a CSS-only tooltip (hover/focus; the peak bucket shows by
 * default). No chart library, no JS hover state.
 */
export function TrafficChart({
  buckets,
  colorOf,
  unit,
  height = 200,
  labelEvery,
}: {
  buckets: TrafficBucket[];
  colorOf: (id: string) => string;
  unit: TrafficUnit;
  height?: number;
  labelEvery?: number;
}) {
  const max = niceMax(Math.max(0, ...buckets.map((b) => b.total)));
  const peak = peakIndex(buckets.map((b) => b.total));
  const ticks = [1, 0.5, 0];
  const labelIdx = new Set(xLabelIndices(buckets.length, labelEvery ?? Math.ceil(buckets.length / 6)));

  return (
    <div className="traffic px-6 pb-4">
      <div className="flex">
        <div className="relative shrink-0" style={{ width: AXIS_WIDTH, height }}>
          {ticks.map((t) => (
            <span
              key={t}
              className="absolute right-2 -translate-y-1/2 text-[11px] text-faint tnum"
              style={{ top: `${(1 - t) * 100}%` }}
            >
              {amount(max * t, unit, true)}
            </span>
          ))}
        </div>
        <div className="relative min-w-0 flex-1" style={{ height }}>
          {ticks.map((t) => (
            <span
              key={t}
              aria-hidden
              className="absolute inset-x-0 h-px bg-[var(--color-rule)]"
              style={{ top: `${(1 - t) * 100}%` }}
            />
          ))}
          <div className="absolute inset-0 flex items-end">
            {buckets.map((b, i) => {
              const left = i < buckets.length / 2;
              const aria = `${b.full}: ${amount(b.total, unit, false)} ${unit}${b.errors ? `, ${b.errors} failed` : ""}`;
              const tip = <Tooltip bucket={b} colorOf={colorOf} unit={unit} />;
              return (
                <div
                  key={b.full + i}
                  tabIndex={0}
                  role="img"
                  aria-label={aria}
                  className="group relative flex h-full min-w-0 flex-1 items-end justify-center focus-visible:outline-none"
                >
                  <div
                    className="flex h-full w-full max-w-[56px] flex-col-reverse rounded-t-[3px] outline-none group-focus-visible:ring-2 group-focus-visible:ring-[var(--color-accent)]"
                    style={{ opacity: peak === i || peak === -1 ? 1 : 0.92 }}
                  >
                    {b.total === 0 ? (
                      <div
                        aria-hidden
                        className="w-full rounded-t-[1px] bg-[var(--color-rule)]"
                        style={{ height: 2 }}
                      />
                    ) : null}
                    {b.parts
                      .filter((part) => part.value > 0)
                      .map((part) => (
                        <div
                          key={part.id}
                          className="w-full border-t border-[var(--color-surface)] first:border-t-0"
                          style={{
                            height: `${(part.value / max) * 100}%`,
                            minHeight: 3,
                            background: colorOf(part.id),
                          }}
                        />
                      ))}
                  </div>
                  <div
                    className="pointer-events-none absolute top-1/2 z-20 hidden w-[208px] -translate-y-1/2 group-hover:block group-focus-within:block"
                    style={left ? { left: "calc(100% + 8px)" } : { right: "calc(100% + 8px)" }}
                  >
                    {tip}
                  </div>
                  {i === peak ? (
                    <div
                      className="traffic-peak-tip pointer-events-none absolute top-1/2 z-10 block w-[208px] -translate-y-1/2"
                      style={left ? { left: "calc(100% + 8px)" } : { right: "calc(100% + 8px)" }}
                    >
                      {tip}
                    </div>
                  ) : null}
                </div>
              );
            })}
          </div>
        </div>
      </div>

      {/* Error lane: one cell per bucket, aligned with the plot columns. */}
      <div className="mt-1 flex">
        <div className="shrink-0 text-[10px] leading-4 text-faint" style={{ width: AXIS_WIDTH }}>
          Errors
        </div>
        <div className="flex min-w-0 flex-1 gap-px">
          {buckets.map((b, i) => (
            <div
              key={`err-${i}`}
              title={`${b.full}: ${b.errors} failed`}
              className={cx(
                "h-1.5 min-w-0 flex-1 rounded-[1px]",
                b.errors > 0 ? "bg-[var(--color-neg)]" : "bg-[var(--color-rule)]",
              )}
            />
          ))}
        </div>
      </div>

      <div className="mt-2 flex">
        <div className="shrink-0" style={{ width: AXIS_WIDTH }} />
        <div className="flex min-w-0 flex-1">
          {buckets.map((b, i) => (
            <div key={`x-${i}`} className="min-w-0 flex-1 text-center">
              {labelIdx.has(i) ? (
                <span className="whitespace-nowrap text-[10.5px] text-faint">{b.label}</span>
              ) : null}
            </div>
          ))}
        </div>
      </div>
    </div>
  );
}

function Tooltip({
  bucket,
  colorOf,
  unit,
}: {
  bucket: TrafficBucket;
  colorOf: (id: string) => string;
  unit: TrafficUnit;
}) {
  return (
    <div className="w-[208px] rounded-lg border border-line bg-surface-2 px-3 py-2.5 text-left shadow-xl">
      <div className="text-[11px] text-faint">{bucket.full}</div>
      <div className="mt-1 text-[13px] font-medium text-fg tnum">
        {amount(bucket.total, unit, false)} {unit}
      </div>
      <div className="mt-2 space-y-1">
        {bucket.parts
          .filter((p) => p.value > 0)
          .slice(0, 5)
          .map((p) => (
            <div key={p.id} className="flex items-center gap-2 text-[11.5px]">
              <span
                aria-hidden
                className="h-2 w-2 shrink-0 rounded-[2px]"
                style={{ background: colorOf(p.id) }}
              />
              <span className="min-w-0 flex-1 truncate text-muted">{p.id}</span>
              <span className="text-fg tnum">{amount(p.value, unit, false)}</span>
            </div>
          ))}
      </div>
      {bucket.errors > 0 ? (
        <div className="mt-1.5 text-[11.5px] text-neg tnum">{bucket.errors} failed</div>
      ) : null}
    </div>
  );
}

