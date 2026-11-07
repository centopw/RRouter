import { type ReactNode } from "react";
import { Heading, Inline, Text } from "../atoms";

export function PageHeader({
  title,
  subtitle,
  actions,
  tabs,
}: {
  title: string;
  subtitle?: string;
  actions?: ReactNode;
  tabs?: ReactNode;
}) {
  return (
    <div className="mb-6">
      <div className="flex flex-wrap items-start justify-between gap-4">
        <div>
          <Heading level={1} className="font-bold">{title}</Heading>
          {subtitle ? <Text as="p" tone="muted" className="mt-1.5">{subtitle}</Text> : null}
        </div>
        {actions ? <Inline gap="sm" wrap>{actions}</Inline> : null}
      </div>
      {tabs ? <div className="mt-5 border-b border-line">{tabs}</div> : null}
    </div>
  );
}
