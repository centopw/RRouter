import type { HTMLAttributes, ReactNode } from "react";
import { cx } from "../../lib/cx";
import { GAP, type Gap } from "./layout";

const COLS = {
  1: "grid-cols-1",
  2: "grid-cols-2",
  3: "grid-cols-3",
  4: "grid-cols-4",
  6: "grid-cols-6",
} as const;

export function Grid({
  cols = 2,
  gap = "md",
  className,
  children,
  ...rest
}: {
  cols?: keyof typeof COLS;
  gap?: Gap;
  className?: string;
  children?: ReactNode;
} & HTMLAttributes<HTMLDivElement>) {
  return (
    <div className={cx("grid", COLS[cols], GAP[gap], className)} {...rest}>
      {children}
    </div>
  );
}
