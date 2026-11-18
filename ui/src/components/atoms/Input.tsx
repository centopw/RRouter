import type { KeyboardEvent } from "react";
import { cx } from "../../lib/cx";

export type ControlSize = "sm" | "md" | "lg";

export const controlSurface =
  "w-full min-w-0 rounded-[var(--radius-input)] border border-[var(--color-line-strong)] bg-surface text-fg placeholder:text-faint hover:border-[var(--color-fg)]/40 focus:border-[var(--color-focus-ring)] focus:outline-none focus-visible:ring-2 focus-visible:ring-[var(--color-focus-ring)]/25 disabled:bg-surface-3 disabled:text-muted";

export const controlInvalid =
  "border-[var(--color-neg)] focus:border-[var(--color-neg)] focus-visible:ring-[var(--color-neg)]/25";

export const INPUT_SIZES: Record<ControlSize, string> = {
  sm: "h-7 px-2 text-small",
  md: "h-8 px-2.5 text-body",
  lg: "h-11 px-3.5 text-body",
};

export const control = cx(INPUT_SIZES.md, controlSurface);

export function Input({
  value,
  onChange,
  placeholder,
  type = "text",
  size = "md",
  invalid = false,
  mono = false,
  disabled,
  readOnly,
  required,
  id,
  name,
  autoComplete,
  ariaLabel,
  "aria-describedby": ariaDescribedBy,
  className,
  onBlur,
  onKeyDown,
  autoFocus,
}: {
  value: string;
  onChange: (v: string) => void;
  placeholder?: string;
  type?: "text" | "number" | "password" | "email" | "url" | "search";
  size?: ControlSize;
  invalid?: boolean;
  mono?: boolean;
  disabled?: boolean;
  readOnly?: boolean;
  required?: boolean;
  id?: string;
  name?: string;
  autoComplete?: string;
  ariaLabel?: string;
  "aria-describedby"?: string;
  className?: string;
  onBlur?: () => void;
  onKeyDown?: (event: KeyboardEvent<HTMLInputElement>) => void;
  autoFocus?: boolean;
}) {
  return (
    <input
      id={id}
      name={name}
      type={type}
      value={value}
      disabled={disabled}
      readOnly={readOnly}
      required={required}
      autoComplete={autoComplete}
      aria-label={ariaLabel ?? placeholder}
      aria-invalid={invalid || undefined}
      aria-describedby={ariaDescribedBy}
      placeholder={placeholder}
      onBlur={onBlur}
      onKeyDown={onKeyDown}
      autoFocus={autoFocus}
      onChange={(e) => onChange(e.target.value)}
      className={cx(
        controlSurface,
        INPUT_SIZES[size],
        mono && "font-mono",
        invalid && controlInvalid,
        className,
      )}
    />
  );
}
