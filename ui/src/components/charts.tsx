import { useId } from "react";
import { compact, pct } from "../lib/format";

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
  const id = useId().replace(/[:]/g, "");
  // Brand cyan marks a trend; red is reserved for an error series.
  const color = { pos: "var(--color-accent)", neg: "var(--color-neg)", accent: "var(--color-accent-2)" }[tone];
  const pts = normalize(values, width, height);
  if (pts.length < 2) return <div style={{ width, height }} aria-hidden />;

  const line = pts.map((p, i) => `${i === 0 ? "M" : "L"}${p.x.toFixed(2)},${p.y.toFixed(2)}`).join(" ");
  const area = `${line} L${width},${height} L0,${height} Z`;

  return (
    <svg width={width} height={height} viewBox={`0 0 ${width} ${height}`} role="img" aria-label="trend">
      <defs>
        <linearGradient id={`g${id}`} x1="0" y1="0" x2="0" y2="1">
          <stop offset="0%" stopColor={color} stopOpacity="0.28" />
          <stop offset="100%" stopColor={color} stopOpacity="0" />
        </linearGradient>
      </defs>
      {fill ? <path d={area} fill={`url(#g${id})`} /> : null}
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
                  background: b.highlight
                    ? "linear-gradient(180deg, var(--color-accent) 0%, var(--color-accent-2) 100%)"
                    : "var(--color-line)",
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
