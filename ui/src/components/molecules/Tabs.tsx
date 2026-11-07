import { cx } from "../../lib/cx";
import * as RadixTabs from "@radix-ui/react-tabs";

export function Tabs({
  tabs,
  value,
  onChange,
}: {
  tabs: Array<{ id: string; label: string }>;
  value: string;
  onChange: (id: string) => void;
}) {
  return (
    <RadixTabs.Root value={value} onValueChange={onChange}>
      <RadixTabs.List className="flex gap-6">
        {tabs.map((t) => (
          <RadixTabs.Trigger
            key={t.id}
            value={t.id}
            className={cx(
              "-mb-px border-b-2 pb-2.5 text-body transition-colors",
              "border-transparent text-muted hover:text-fg",
              "data-[state=active]:border-accent data-[state=active]:text-fg",
            )}
          >
            {t.label}
          </RadixTabs.Trigger>
        ))}
      </RadixTabs.List>
    </RadixTabs.Root>
  );
}
