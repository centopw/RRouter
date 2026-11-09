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
        "relative shrink-0 cursor-pointer rounded-full border transition-colors disabled:cursor-not-allowed disabled:opacity-50",
        // A 48×46 target without touching the visual: the pseudo-element is transparent
        // and extends past the painted box (16px tall + 15px each side, 28px wide + 10).
        "before:absolute before:-inset-x-[10px] before:-inset-y-[15px] before:content-['']",
        small ? "h-4 w-7" : "h-6 w-11",
        "data-[state=checked]:border-accent data-[state=checked]:bg-accent",
        // `line-strong` on white is 1.9:1; the off state's boundary needs 3:1 to be seen.
        "data-[state=unchecked]:border-[var(--color-faint)] data-[state=unchecked]:bg-surface-3",
      )}
    >
      <Switch.Thumb
        className={cx(
          "absolute left-0 top-1/2 -translate-y-1/2 rounded-full bg-[var(--color-muted)] transition-transform duration-150",
          "data-[state=checked]:bg-white",
          small ? "h-2.5 w-2.5" : "h-4 w-4",
          small
            ? checked ? "translate-x-[13px]" : "translate-x-[3px]"
            : checked ? "translate-x-[22px]" : "translate-x-[3px]",
        )}
      />
    </Switch.Root>
  );
}
