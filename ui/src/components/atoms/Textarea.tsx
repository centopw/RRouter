import type { KeyboardEvent } from "react";
import { cx } from "../../lib/cx";
import { controlInvalid, controlSurface, type ControlSize } from "./Input";

const TEXTAREA_SIZES: Record<ControlSize, string> = {
  sm: "px-2 py-1.5 text-small",
  md: "px-2.5 py-2 text-body",
  lg: "px-3.5 py-2.5 text-body",
};

/** Multi-line text input aligned with `Input` tokens, sizing, and validation state. */
export function Textarea({
  value,
  onChange,
  placeholder,
  rows = 3,
  size = "md",
  invalid = false,
  mono = false,
  disabled,
  readOnly,
  required,
  id,
  name,
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
  rows?: number;
  size?: ControlSize;
  invalid?: boolean;
  mono?: boolean;
  disabled?: boolean;
  readOnly?: boolean;
  required?: boolean;
  id?: string;
  name?: string;
  ariaLabel?: string;
  "aria-describedby"?: string;
  className?: string;
  onBlur?: () => void;
  onKeyDown?: (event: KeyboardEvent<HTMLTextAreaElement>) => void;
  autoFocus?: boolean;
}) {
  return (
    <textarea
      id={id}
      name={name}
      rows={rows}
      value={value}
      disabled={disabled}
      readOnly={readOnly}
      required={required}
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
        "resize-y read-only:bg-surface-2 read-only:text-muted",
        TEXTAREA_SIZES[size],
        mono && "font-mono",
        invalid && controlInvalid,
        className,
      )}
    />
  );
}
