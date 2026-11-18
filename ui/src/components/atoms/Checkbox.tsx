import * as RadixCheckbox from "@radix-ui/react-checkbox";
import { Check, Minus } from "@phosphor-icons/react";
import type { Ref } from "react";
import { cx } from "../../lib/cx";

/** Radix Checkbox with the dashboard's square control and an optional clickable label. */
export function Checkbox({
  checked,
  indeterminate = false,
  onChange,
  label,
  ariaLabel,
  disabled = false,
  className,
  rootRef,
}: {
  checked: boolean;
  indeterminate?: boolean;
  onChange: (next: boolean) => void;
  /** Visible label: rendered beside the control and used as its accessible name. */
  label?: string;
  /** Accessible name only, for a control whose text sits in a sibling block. */
  ariaLabel?: string;
  disabled?: boolean;
  className?: string;
  rootRef?: Ref<HTMLButtonElement>;
}) {
  const control = (
    <RadixCheckbox.Root
      ref={rootRef}
      checked={indeterminate ? "indeterminate" : checked}
      onCheckedChange={(next) => onChange(next === true)}
      disabled={disabled}
      aria-label={ariaLabel ?? label}
      className={cx(
        "flex h-4 w-4 shrink-0 items-center justify-center rounded-[3px] border bg-surface text-[var(--color-accent-ink)] transition-colors",
        "data-[state=checked]:border-accent data-[state=checked]:bg-accent",
        "data-[state=indeterminate]:border-accent data-[state=indeterminate]:bg-accent",
        "data-[state=unchecked]:border-[var(--color-line-strong)] data-[state=unchecked]:hover:border-fg/50",
        "focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--color-focus-ring)]",
        "disabled:cursor-not-allowed disabled:opacity-50",
        className,
      )}
    >
      <RadixCheckbox.Indicator>
        {indeterminate ? (
          <Minus size={11} weight="bold" aria-hidden />
        ) : (
          <Check size={11} weight="bold" aria-hidden />
        )}
      </RadixCheckbox.Indicator>
    </RadixCheckbox.Root>
  );
  if (!label) return control;
  return (
    <label
      className={cx("flex cursor-pointer items-center gap-2 text-small", disabled && "cursor-not-allowed opacity-60")}
      onClick={(event) => {
        if ((event.target as HTMLElement).closest('[role="checkbox"]')) return;
        if (!disabled) onChange(indeterminate ? true : !checked);
      }}
    >
      {control}
      <span>{label}</span>
    </label>
  );
}
