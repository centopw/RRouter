import { type ReactNode } from "react";
import { Heading } from "../atoms";

/** Section header used inside cards: title on the left, an action on the right. */
export function CardHeader({ title, action }: { title: string; action?: ReactNode }) {
  return (
    <div className="flex items-center justify-between px-5 pt-4 pb-3">
      <Heading level={2}>{title}</Heading>
      {action}
    </div>
  );
}
