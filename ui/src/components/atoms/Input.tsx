import type { KeyboardEvent } from "react";
import { cx } from "../../lib/cx";

export const control =
  "h-8 w-full min-w-0 rounded-[var(--radius-input)] border border-line-strong bg-surface px-2.5 text-body text-fg placeholder:text-faint hover:border-[var(--color-fg)]/40 focus:border-[var(--color-accent-text)] focus:outline-none focus-visible:ring-2 focus-visible:ring-[var(--color-accent-text)]/25 disabled:bg-surface-3 disabled:text-muted";

export function Input({
  value,
  onChange,
  placeholder,
  type = "text",
  disabled,
  ariaLabel,
  className,
  onBlur,
  onKeyDown,
  autoFocus,
}: {
  value: string;
  onChange: (v: string) => void;
  placeholder?: string;
  type?: "text" | "number" | "password";
  disabled?: boolean;
  ariaLabel?: string;
  className?: string;
  onBlur?: () => void;
  onKeyDown?: (event: KeyboardEvent<HTMLInputElement>) => void;
  autoFocus?: boolean;
}) {
  return (
    <input
      type={type}
      value={value}
      disabled={disabled}
      aria-label={ariaLabel ?? placeholder}
      placeholder={placeholder}
      onBlur={onBlur}
      onKeyDown={onKeyDown}
      autoFocus={autoFocus}
      onChange={(e) => onChange(e.target.value)}
      className={cx(control, className)}
    />
  );
}
