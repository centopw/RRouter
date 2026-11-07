import { type ReactNode } from "react";

/** Card footer for form actions: a hairline separates actions from the fields. */
export function CardFooter({ children }: { children: ReactNode }) {
  return <div className="flex items-center justify-end gap-2 border-t border-line px-5 py-3">{children}</div>;
}
