import { type ReactNode } from "react";
import { cx } from "../../lib/cx";
import { Dot, type DotTone } from "../atoms/Dot";

export type AlertTone = "neg" | "warn" | "info" | "pos";

const TONE_STYLES: Record<
  AlertTone,
  {
    container: string;
    title: string;
    dismiss: string;
    dot: DotTone;
    role: "alert" | "status";
  }
> = {
  neg: {
    container: "border-[var(--color-neg-line)] bg-[var(--color-neg-tint)]",
    title: "text-[var(--color-neg-ink)]",
    dismiss: "text-[var(--color-neg-ink)] hover:bg-[var(--color-neg-line)]",
    dot: "neg",
    role: "alert",
  },
  warn: {
    container: "border-[var(--color-warn-line)] bg-[var(--color-warn-tint)]",
    title: "text-[var(--color-warn-ink)]",
    dismiss: "text-[var(--color-warn-ink)] hover:bg-[var(--color-warn-line)]",
    dot: "warn",
    role: "status",
  },
  info: {
    container: "border-[var(--color-info-line)] bg-[var(--color-info-tint)]",
    title: "text-[var(--color-info)]",
    dismiss: "text-[var(--color-info)] hover:bg-[var(--color-info-line)]",
    dot: "info",
    role: "status",
  },
  pos: {
    container: "border-[var(--color-pos-line)] bg-[var(--color-pos-tint)]",
    title: "text-[var(--color-pos-ink)]",
    dismiss: "text-[var(--color-pos-ink)] hover:bg-[var(--color-pos-line)]",
    dot: "pos",
    role: "status",
  },
};

/** Loud semantic banner for actionable incidents, warnings, or confirmations. */
export function AlertBanner({
  title,
  children,
  actions,
  onDismiss,
  tone = "neg",
  className,
}: {
  title: string;
  children: ReactNode;
  actions?: ReactNode;
  /** When set, the banner carries a close control and the caller drops it. */
  onDismiss?: () => void;
  tone?: AlertTone;
  className?: string;
}) {
  const styles = TONE_STYLES[tone];
  return (
    <div
      role={styles.role}
      className={cx(
        "mb-5 rounded-[var(--radius-card)] border px-5 py-4",
        styles.container,
        className,
      )}
    >
      <div className="flex flex-wrap items-start gap-x-4 gap-y-2">
        <div className="min-w-0 flex-1">
          <div className={cx("flex items-center gap-2 text-lead font-semibold", styles.title)}>
            <Dot tone={styles.dot} />
            {title}
          </div>
          <div className="mt-1 text-body text-muted">{children}</div>
        </div>
        {actions ? <div className="flex flex-wrap items-center gap-2">{actions}</div> : null}
        {onDismiss ? (
          <button
            type="button"
            onClick={onDismiss}
            aria-label="Dismiss alert"
            title="Dismiss"
            className={cx(
              "-mr-1 -mt-1 inline-flex h-7 w-7 shrink-0 cursor-pointer items-center justify-center rounded-full text-lead leading-none transition-colors focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--color-focus-ring)]",
              styles.dismiss,
            )}
          >
            <span aria-hidden>×</span>
          </button>
        ) : null}
      </div>
    </div>
  );
}
