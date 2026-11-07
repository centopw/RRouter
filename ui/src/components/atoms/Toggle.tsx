import * as Switch from "@radix-ui/react-switch";
import { cx } from "../../lib/cx";

/** Radix Switch with our visual states: controlled, keyboard and form semantics for free. */
export function Toggle({
  checked,
  onChange,
  label,
  disabled = false,
  size = "md",
}: {
  checked: boolean;
  onChange: (next: boolean) => void;
  label: string;
  disabled?: boolean;
  /** `sm` is the in-table switch; `md` is the settings switch. */
  size?: "sm" | "md";
}) {
  const small = size === "sm";
  return (
    <Switch.Root
      checked={checked}
      onCheckedChange={onChange}
      aria-label={label}
      disabled={disabled}
      className={cx(
        "relative shrink-0 rounded-full border transition-colors disabled:opacity-50",
        small ? "h-4 w-7" : "h-6 w-11",
        "data-[state=checked]:border-accent data-[state=checked]:bg-accent",
        "data-[state=unchecked]:border-line-strong data-[state=unchecked]:bg-surface-3",
      )}
    >
      <Switch.Thumb
        className={cx(
          "absolute rounded-full bg-[var(--color-muted)] transition-transform duration-150",
          "data-[state=checked]:bg-white",
          small ? "h-2.5 w-2.5" : "top-1 h-4 w-4",
          small
            ? "translate-x-[3px] data-[state=checked]:translate-x-[13px]"
            : "translate-x-[3px] data-[state=checked]:translate-x-[22px]",
        )}
      />
    </Switch.Root>
  );
}
