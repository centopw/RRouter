import * as Separator from "@radix-ui/react-separator";
import { cx } from "../../lib/cx";

/** Radix Separator with the dashboard's hairline. */
export function Divider({
  vertical = false,
  className,
}: {
  vertical?: boolean;
  className?: string;
}) {
  return (
    <Separator.Root
      orientation={vertical ? "vertical" : "horizontal"}
      decorative
      className={cx("shrink-0 bg-line", vertical ? "h-6 w-px" : "h-px w-full", className)}
    />
  );
}
