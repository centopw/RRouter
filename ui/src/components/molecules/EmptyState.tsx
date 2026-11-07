import { type ReactNode } from "react";

export function EmptyState({ children }: { children: ReactNode }) {
  return <div className="px-4 py-10 text-center text-body text-faint">{children}</div>;
}
