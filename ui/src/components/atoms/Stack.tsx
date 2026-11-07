import type { ElementType, HTMLAttributes, ReactNode } from "react";
import { cx } from "../../lib/cx";
import { ALIGN, GAP, JUSTIFY, type Gap } from "./layout";

export type StackProps = {
  as?: ElementType;
  gap?: Gap;
  align?: keyof typeof ALIGN;
  justify?: keyof typeof JUSTIFY;
  className?: string;
  children?: ReactNode;
} & HTMLAttributes<HTMLElement>;

/** Vertical flow primitive; spacing comes from the shared gap scale. */
export function Stack({
  as: Tag = "div",
  gap = "md",
  align,
  justify,
  className,
  children,
  ...rest
}: StackProps) {
  return (
    <Tag
      className={cx("flex flex-col", GAP[gap], align && ALIGN[align], justify && JUSTIFY[justify], className)}
      {...rest}
    >
      {children}
    </Tag>
  );
}
