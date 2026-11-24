import { cx } from "../../lib/cx";

/** Fill for a magnitude bar: neutral until 70%, then the state colour only. */
export function barFill(percent: number): string {
  return percent >= 90
    ? "bg-[var(--color-neg-fill)]"
    : percent >= 70
      ? "bg-[var(--color-warn-fill)]"
      : "bg-muted";
}

/** Text for the same bar: the number is a signal at 70%+, plain otherwise. */
export function barText(percent: number): string {
  return percent >= 90
    ? "text-[var(--color-neg)]"
    : percent >= 70
      ? "text-[var(--color-warn)]"
      : "text-muted";
}

/**
 * A 6px magnitude bar on the shared colour ladder (`barFill`), with an optional pace tick
 * marking where the window is projected to land at reset. Width is the caller's.
 */
export function Meter({
  percent,
  label,
  tick = null,
  className,
}: {
  percent: number;
  /** Accessible name, e.g. "Weekly, resets in 6d 18h". */
  label: string;
  /** Optional projected percentage (0–100) rendered as a tick on the track. */
  tick?: number | null;
  className?: string;
}) {
  const width = Math.min(100, Math.max(0, Number.isFinite(percent) ? percent : 0));
  const clampedTick =
    tick !== null && Number.isFinite(tick) ? Math.min(100, Math.max(0, tick)) : null;

  return (
    <span
      role="progressbar"
      aria-label={label}
      aria-valuemin={0}
      aria-valuemax={100}
      aria-valuenow={Math.round(width)}
      className={cx("relative block h-1.5 w-full overflow-hidden rounded-full bg-rule", className)}
    >
      <span
        className={cx("block h-full rounded-full", barFill(percent))}
        style={{ width: `${width}%` }}
      />
      {clampedTick !== null ? (
        <span
          aria-hidden="true"
          className="absolute inset-y-0 w-0.5 rounded-full bg-faint"
          style={{ left: `min(calc(${clampedTick}% - 1px), calc(100% - 2px))` }}
        />
      ) : null}
    </span>
  );
}
