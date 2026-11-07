import * as ToggleGroup from "@radix-ui/react-toggle-group";
import { cx } from "../../lib/cx";

/** A small set of mutually exclusive options, backed by Radix ToggleGroup. */
export function Segmented<T extends string>({
  options,
  value,
  onChange,
  ariaLabel,
  className,
}: {
  options: ReadonlyArray<{ id: T; label: string }>;
  value: T;
  onChange: (v: T) => void;
  ariaLabel: string;
  className?: string;
}) {
  return (
    <ToggleGroup.Root
      type="single"
      value={value}
      onValueChange={(next) => {
        if (next) onChange(next as T);
      }}
      aria-label={ariaLabel}
      className={cx(
        "inline-flex items-center gap-0.5 rounded-full border border-line bg-surface-3 p-0.5",
        className,
      )}
    >
      {options.map((option) => (
        <ToggleGroup.Item
          key={option.id}
          value={option.id}
          className={cx(
            "h-[26px] rounded-full px-2.5 text-small transition-colors max-[820px]:h-9",
            "text-muted hover:text-fg",
            "data-[state=on]:bg-accent data-[state=on]:font-medium data-[state=on]:text-[var(--color-accent-ink)]",
          )}
        >
          {option.label}
        </ToggleGroup.Item>
      ))}
    </ToggleGroup.Root>
  );
}
