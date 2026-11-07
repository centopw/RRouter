import { type ReactNode } from "react";
import { Dot } from "../atoms/Dot";

/** One loud line for a real problem; silent when everything is healthy. */
export function AlertBanner({
  title,
  children,
  actions,
}: {
  title: string;
  children: ReactNode;
  actions?: ReactNode;
}) {
  return (
    <div
      role="alert"
      className="mb-5 rounded-[var(--radius-card)] border border-[var(--color-neg-line)] bg-[var(--color-neg-tint)] px-5 py-4"
    >
      <div className="flex flex-wrap items-start gap-x-4 gap-y-2">
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-2 text-lead font-semibold text-[var(--color-neg-ink)]">
            <Dot tone="neg" />
            {title}
          </div>
          <div className="mt-1 text-body text-muted">{children}</div>
        </div>
        {actions ? <div className="flex flex-wrap items-center gap-2">{actions}</div> : null}
      </div>
    </div>
  );
}
