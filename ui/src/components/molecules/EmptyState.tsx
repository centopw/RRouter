import { type ReactNode } from "react";

/** Nothing to show: one quiet line. The message is primary content of the region, so it
 *  uses `muted` (secondary text), never `faint`. */
export function EmptyState({ children }: { children: ReactNode }) {
  return <div className="px-4 py-10 text-center text-body text-muted">{children}</div>;
}
