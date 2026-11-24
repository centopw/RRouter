import { type ReactNode } from "react";
import { cx } from "../../lib/cx";
import { Heading } from "../atoms/Heading";

const PADDING = {
  default: "px-5 py-3",
  compact: "px-4 py-3",
  none: "",
} as const;

export type CardHeaderPadding = keyof typeof PADDING;

/**
 * Section header used inside cards: optional icon, `text-body` semibold title,
 * quiet meta text on the left, and an optional action/legend slot on the right.
 */
export function CardHeader({
  title,
  icon,
  meta,
  action,
  padding = "default",
  className,
}: {
  title: string;
  icon?: ReactNode;
  meta?: ReactNode;
  action?: ReactNode;
  padding?: CardHeaderPadding;
  className?: string;
}) {
  return (
    <div className={cx("flex flex-wrap items-center justify-between gap-2", PADDING[padding], className)}>
      <div className="flex min-w-0 items-center gap-2">
        {icon ? (
          <span aria-hidden="true" className="inline-flex shrink-0 items-center text-accent-text">
            {icon}
          </span>
        ) : null}
        <Heading level={2} variant="body" className="truncate">
          {title}
        </Heading>
        {meta ? <span className="shrink-0 text-caption text-faint tnum">{meta}</span> : null}
      </div>
      {action}
    </div>
  );
}
