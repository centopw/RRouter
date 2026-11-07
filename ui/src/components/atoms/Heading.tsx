import type { ElementType, HTMLAttributes, ReactNode } from "react";
import { cx } from "../../lib/cx";
import { TONE, TYPE, type TextVariant, type Tone } from "./typography";

/** Semantic heading; its type step follows the level unless `variant` overrides it. */
export function Heading({
  level = 2,
  variant,
  tone = "default",
  className,
  children,
  ...rest
}: {
  level?: 1 | 2 | 3;
  variant?: TextVariant;
  tone?: Tone;
  className?: string;
  children?: ReactNode;
} & HTMLAttributes<HTMLHeadingElement>) {
  const Tag = (`h${level}` as const) as ElementType;
  const size = variant ?? (level === 1 ? "display" : level === 2 ? "heading" : "lead");
  return (
    <Tag className={cx(TYPE[size], TONE[tone], "font-semibold", className)} {...rest}>
      {children}
    </Tag>
  );
}
