import { type ReactNode } from "react";
import { cx } from "../../lib/cx";

/** A pill that filters a list; `on` marks the active filter, `bad` an error count. */
export function FilterChip({
  children,
  on = false,
  bad = false,
  onClick,
  ariaLabel,
}: {
  children: ReactNode;
  on?: boolean;
  bad?: boolean;
  onClick: () => void;
  ariaLabel?: string;
}) {
  return (
    <button
      type="button"
      aria-pressed={on}
      aria-label={ariaLabel}
      onClick={onClick}
      className={cx(
        "inline-flex h-8 items-center gap-1.5 rounded-full border px-3 text-small transition-colors max-[820px]:h-10",
        on
          ? "border-accent bg-accent font-medium text-[var(--color-accent-ink)]"
          : bad
            ? "border-[var(--color-neg-line)] bg-[var(--color-neg-tint)] text-[var(--color-neg-ink)] hover:border-[var(--color-neg)]"
            : "border-line bg-surface-2 text-muted hover:border-line-strong hover:text-fg",
      )}
    >
      {children}
    </button>
  );
}
