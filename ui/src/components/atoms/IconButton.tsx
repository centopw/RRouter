import type { HTMLAttributes, ReactNode, Ref } from "react";
import { cx } from "../../lib/cx";

/** Square ghost control for a single icon; the label is mandatory for screen readers. */
export function IconButton({
  label,
  children,
  className,
  disabled,
  ref,
  ...rest
}: {
  label: string;
  children: ReactNode;
  className?: string;
  disabled?: boolean;
  ref?: Ref<HTMLButtonElement>;
} & HTMLAttributes<HTMLButtonElement>) {
  return (
    <button
      ref={ref}
      type="button"
      aria-label={label}
      title={label}
      disabled={disabled}
      className={cx(
        "inline-flex h-8 w-8 shrink-0 items-center justify-center rounded-full text-faint transition-colors",
        "enabled:hover:bg-surface-3 enabled:hover:text-fg focus-visible:outline focus-visible:outline-2",
        "focus-visible:outline-offset-2 focus-visible:outline-[var(--color-focus-ring)] disabled:cursor-not-allowed disabled:opacity-50",
        className,
      )}
      {...rest}
    >
      {children}
    </button>
  );
}
