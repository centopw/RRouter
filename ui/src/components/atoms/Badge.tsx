import { type ReactNode } from "react";
import { cx } from "../../lib/cx";

export type BadgeTone = "neutral" | "pos" | "neg" | "warn" | "info" | "accent";
export type BadgeSize = "sm" | "md";

const TONES: Record<BadgeTone, string> = {
  neutral: "border-[var(--color-line-strong)] bg-surface-2 text-muted",
  pos: "border-[var(--color-pos-line)] bg-[var(--color-pos-tint)] text-[var(--color-pos-ink)]",
  neg: "border-[var(--color-neg-line)] bg-[var(--color-neg-tint)] text-[var(--color-neg-ink)]",
  warn: "border-[var(--color-warn-line)] bg-[var(--color-warn-tint)] text-[var(--color-warn-ink)]",
  info: "border-[var(--color-info-line)] bg-[var(--color-info-tint)] text-[var(--color-info)]",
  accent: "border-[var(--color-info-line)] bg-[var(--color-info-tint)] text-[var(--color-accent-text)]",
};

const SIZES: Record<BadgeSize, string> = {
  sm: "px-2 py-0.5 text-caption leading-4",
  md: "px-2.5 py-0.5 text-small leading-5",
};

export function Badge({
  children,
  tone = "neutral",
  size = "sm",
  title,
  className,
}: {
  children: ReactNode;
  tone?: BadgeTone;
  size?: BadgeSize;
  title?: string;
  className?: string;
}) {
  return (
    <span
      title={title}
      className={cx(
        "inline-flex max-w-full min-w-0 items-center gap-1 rounded-full border",
        SIZES[size],
        TONES[tone],
        className,
      )}
    >
      <span className="min-w-0 truncate">{children}</span>
    </span>
  );
}
