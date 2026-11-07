import type { ElementType, HTMLAttributes, ReactNode } from "react";
import { cx } from "../../lib/cx";
import { TONE, TYPE, type TextVariant, type Tone } from "./typography";

type TextProps = {
  as?: ElementType;
  variant?: TextVariant;
  tone?: Tone;
  mono?: boolean;
  numeric?: boolean;
  truncate?: boolean;
  className?: string;
  children?: ReactNode;
} & Omit<HTMLAttributes<HTMLElement>, "color">;

/** Any run of text: one type step, one semantic tone, optional mono/tabular. */
export function Text({
  as: Tag = "span",
  variant = "body",
  tone = "default",
  mono = false,
  numeric = false,
  truncate = false,
  className,
  ...rest
}: TextProps) {
  return (
    <Tag
      className={cx(
        TYPE[variant],
        TONE[tone],
        mono && "font-mono",
        numeric && "tnum",
        truncate && "truncate",
        className,
      )}
      {...rest}
    />
  );
}
