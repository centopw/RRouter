import { cx } from "../../lib/cx";
import { ALIGN, GAP, JUSTIFY } from "./layout";
import type { StackProps } from "./Stack";

/** Horizontal flow primitive; `wrap` is the common toolbar behaviour. */
export function Inline({
  as: Tag = "div",
  wrap = false,
  gap = "md",
  align,
  justify,
  className,
  children,
  ...rest
}: StackProps & { wrap?: boolean }) {
  return (
    <Tag
      className={cx(
        "flex flex-row",
        GAP[gap],
        wrap && "flex-wrap",
        align && ALIGN[align],
        justify && JUSTIFY[justify],
        className,
      )}
      {...rest}
    >
      {children}
    </Tag>
  );
}
