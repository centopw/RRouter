import type { HTMLAttributes, ReactNode } from "react";
import { cx } from "../../lib/cx";

/** Square ghost control for a single icon; the label is mandatory for screen readers. */
export function IconButton({
  label,
  children,
  className,
  ...rest
}: {
  label: string;
  children: ReactNode;
  className?: string;
} & HTMLAttributes<HTMLButtonElement>) {
  return (
    <button
      type="button"
      aria-label={label}
      title={label}
      className={cx(
        "inline-flex h-8 w-8 shrink-0 items-center justify-center rounded-full text-faint transition-colors",
        "hover:bg-surface-3 hover:text-fg focus-visible:outline focus-visible:outline-2",
        "focus-visible:outline-offset-2 focus-visible:outline-accent-text disabled:cursor-not-allowed disabled:opacity-50",
        className,
      )}
      {...rest}
    >
      {children}
    </button>
  );
}
