import { CaretDown } from "@phosphor-icons/react";
import * as Accordion from "@radix-ui/react-accordion";
import type { ReactNode } from "react";
import { cx } from "../../lib/cx";

/** One collapsible section on Radix Accordion; the trigger and animation are shared. */
export function Disclosure({
  title,
  children,
  className,
  contentClassName,
}: {
  title: string;
  children: ReactNode;
  className?: string;
  contentClassName?: string;
}) {
  return (
    <Accordion.Root type="single" collapsible className={cx("rounded-lg border border-line bg-surface", className)}>
      <Accordion.Item value="content">
        <Accordion.Header>
          <Accordion.Trigger className="group flex w-full items-center justify-between gap-3 px-4 py-2.5 text-left text-small font-medium text-fg">
            {title}
            <CaretDown
              size={13}
              aria-hidden
              className="shrink-0 text-faint transition-transform group-data-[state=open]:rotate-180"
            />
          </Accordion.Trigger>
        </Accordion.Header>
        <Accordion.Content className="overflow-hidden border-t border-rule data-[state=closed]:hidden">
          <div className={cx("px-4 py-3", contentClassName)}>{children}</div>
        </Accordion.Content>
      </Accordion.Item>
    </Accordion.Root>
  );
}
