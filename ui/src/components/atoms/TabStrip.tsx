import type { ReactNode } from "react";
import * as Tabs from "@radix-ui/react-tabs";
import { cx } from "../../lib/cx";

/**
 * In-page views as tabs, drawn as a segmented pill.
 *
 * `Segmented` is the right control for a setting (it is a labelled radio group), but
 * switching the whole page is a tab set: arrows must move *and* select, and each view
 * must own a `tabpanel` labelled by its tab. Radix Tabs gives both for free, and it
 * unmounts the panels that are not active, so a view cannot fetch data it does not show.
 */
export function TabStrip<T extends string>({
  options,
  value,
  onChange,
  ariaLabel,
  className,
  children,
}: {
  options: ReadonlyArray<{ id: T; label: string }>;
  value: T;
  onChange: (v: T) => void;
  ariaLabel: string;
  className?: string;
  /** The `TabPanel`s, one per option. */
  children: ReactNode;
}) {
  return (
    <Tabs.Root value={value} onValueChange={(next) => onChange(next as T)}>
      <Tabs.List
        aria-label={ariaLabel}
        className={cx(
          "inline-flex items-center gap-0.5 rounded-full border border-line bg-surface-3 p-0.5",
          className,
        )}
      >
        {options.map((option) => (
          <Tabs.Trigger
            key={option.id}
            value={option.id}
            className={cx(
              "h-[26px] rounded-full px-2.5 text-small transition-colors max-[820px]:h-9",
              "text-muted hover:text-fg",
              "data-[state=active]:bg-accent data-[state=active]:font-medium data-[state=active]:text-[var(--color-accent-ink)]",
            )}
          >
            {option.label}
          </Tabs.Trigger>
        ))}
      </Tabs.List>
      {children}
    </Tabs.Root>
  );
}

/** One view's content; only the active panel is mounted. */
export const TabPanel = Tabs.Content;
