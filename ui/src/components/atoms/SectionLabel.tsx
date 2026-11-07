import type { ReactNode } from "react";
import { cx } from "../../lib/cx";
import { Text } from "./Text";

/** Small all-caps label used above groups of controls. */
export function SectionLabel({ children, className }: { children: ReactNode; className?: string }) {
  return (
    <Text
      as="div"
      variant="caption"
      tone="faint"
      className={cx("font-semibold uppercase tracking-[0.08em]", className)}
    >
      {children}
    </Text>
  );
}
