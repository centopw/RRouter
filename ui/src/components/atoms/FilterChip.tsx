import * as Toggle from "@radix-ui/react-toggle";
import type { ReactNode } from "react";
import { cx } from "../../lib/cx";

/** A filter pill built on Radix Toggle; `bad` marks an error count. */
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
    <Toggle.Root
      pressed={on}
      onPressedChange={onClick}
      aria-label={ariaLabel}
      className={cx(
        "inline-flex h-8 items-center gap-1.5 rounded-full border px-3 text-small transition-colors max-[820px]:h-10",
        "data-[state=off]:border-line data-[state=off]:bg-surface-2 data-[state=off]:text-muted",
        "data-[state=off]:hover:border-line-strong data-[state=off]:hover:text-fg",
        bad && "data-[state=off]:border-[var(--color-neg-line)] data-[state=off]:bg-[var(--color-neg-tint)] data-[state=off]:text-[var(--color-neg-ink)]",
        "data-[state=on]:border-accent data-[state=on]:bg-accent data-[state=on]:font-medium data-[state=on]:text-[var(--color-accent-ink)]",
      )}
    >
      {children}
    </Toggle.Root>
  );
}
