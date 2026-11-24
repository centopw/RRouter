import { type ReactNode } from "react";
import { cx } from "../../lib/cx";
import { Card } from "./Card";

export type KpiTone = "info" | "accent" | "pos" | "warn" | "neg" | "neutral";

const ICON_TONES: Record<KpiTone, string> = {
  pos: "bg-[var(--color-pos-tint)] text-[var(--color-pos)]",
  warn: "bg-[var(--color-warn-tint)] text-[var(--color-warn)]",
  neg: "bg-[var(--color-neg-tint)] text-[var(--color-neg)]",
  info: "bg-[var(--color-info-tint)] text-[var(--color-info)]",
  accent: "bg-[var(--color-info-tint)] text-[var(--color-accent-text)]",
  neutral: "bg-surface-2 text-muted",
};

/** Tinted 36×36 (32×32 on narrow viewports) leading icon badge for `KpiTile`. */
export function KpiIcon({ tone = "info", children }: { tone?: KpiTone; children: ReactNode }) {
  return (
    <span
      aria-hidden
      className={cx(
        "inline-flex h-8 w-8 shrink-0 items-center justify-center rounded-[10px] sm:h-9 sm:w-9",
        ICON_TONES[tone],
      )}
    >
      {children}
    </span>
  );
}

export type KpiTileProps = {
  /** Short muted label above the KPI value. */
  label: string;
  /** Primary KPI readout, rendered with the `--text-kpi` token (`text-kpi`). */
  value: ReactNode;
  /** Quiet inline suffix beside `value` (e.g. `"/ 7"`, `"req"`, `"of 29"`). */
  suffix?: ReactNode;
  /** Optional supporting line below the value/meter. */
  detail?: ReactNode;
  /** Optional leading icon rendered inside `KpiIcon`. */
  icon?: ReactNode;
  /** Semantic tone for the icon badge (defaults to `"info"`). */
  tone?: KpiTone;
  /** Optional progress bar / `Meter` rendered between the value and detail line. */
  meter?: ReactNode;
  /** Optional right-aligned visual beside the detail line (e.g. a sparkline). */
  aside?: ReactNode;
  /** Optional click handler to make the tile act as a quick-filter control. */
  onClick?: () => void;
  /** Marks the tile as active when used as a filter button. */
  active?: boolean;
  className?: string;
};

/**
 * Summary KPI tile shared across Usage Overview, Providers, and Models.
 * Uses the `--text-kpi` type token (`text-kpi`) and tabular numerals (`tnum`).
 */
export function KpiTile({
  label,
  value,
  suffix,
  detail,
  icon,
  tone = "info",
  meter,
  aside,
  onClick,
  active = false,
  className,
}: KpiTileProps) {
  const inner = (
    <>
      {icon ? <KpiIcon tone={tone}>{icon}</KpiIcon> : null}
      <div className="flex min-w-0 flex-1 flex-col gap-1">
        <span className="truncate text-caption font-medium text-muted">{label}</span>
        <div className="flex min-w-0 items-baseline gap-1 tnum">
          <span className="truncate font-semibold text-fg text-kpi max-sm:text-[22px] max-sm:leading-tight">
            {value}
          </span>
          {suffix ? (
            <span className="truncate text-small font-normal text-faint sm:text-lead">
              {suffix}
            </span>
          ) : null}
        </div>
        {meter ? <div className="mt-0.5">{meter}</div> : null}
        {detail || aside ? (
          <div className="flex min-w-0 items-end justify-between gap-2">
            {detail ? (
              <span
                title={typeof detail === "string" ? detail : undefined}
                className="min-w-0 flex-1 truncate text-caption text-faint tnum"
              >
                {detail}
              </span>
            ) : null}
            {aside ? <div className="shrink-0">{aside}</div> : null}
          </div>
        ) : null}
      </div>
    </>
  );

  if (onClick) {
    return (
      <Card
        as="article"
        className={cx(
          "flex min-w-0 transition-colors",
          active && "border-[var(--color-accent)] ring-1 ring-[var(--color-accent)]/25",
          className,
        )}
      >
        <button
          type="button"
          onClick={onClick}
          aria-pressed={active}
          className="flex min-w-0 flex-1 cursor-pointer items-start gap-2.5 rounded-[var(--radius-card)] p-3 text-left transition-colors hover:bg-[var(--color-row-hover)] focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--color-focus-ring)] sm:gap-3 sm:p-4"
        >
          {inner}
        </button>
      </Card>
    );
  }

  return (
    <Card
      as="article"
      className={cx("flex min-w-0 items-start gap-2.5 p-3 sm:gap-3 sm:p-4", className)}
    >
      {inner}
    </Card>
  );
}
