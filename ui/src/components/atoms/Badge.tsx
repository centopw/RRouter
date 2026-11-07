import { type ReactNode } from "react";
import { cx } from "../../lib/cx";

export function Badge({
  children,
  tone = "neutral",
  title,
}: {
  children: ReactNode;
  tone?: "neutral" | "pos" | "neg" | "warn" | "accent";
  title?: string;
}) {
  const tones = {
    neutral: "border-line-strong bg-surface-2 text-muted",
    pos: "border-[var(--color-pos)]/35 bg-[var(--color-pos-tint)] text-[var(--color-pos)]",
    neg: "border-[var(--color-neg-line)] bg-[var(--color-neg-tint)] text-[var(--color-neg-ink)]",
    warn: "border-[var(--color-warn-line)] bg-[var(--color-warn-tint)] text-[var(--color-warn-ink)]",
    accent: "border-[var(--color-accent-text)]/35 bg-[var(--color-accent)]/10 text-[var(--color-accent-text)]",
  } as const;
  return (
    <span
      title={title}
      className={cx(
        "inline-flex items-center gap-1 rounded-full border px-2 py-0.5 text-caption leading-4",
        tones[tone],
      )}
    >
      {children}
    </span>
  );
}
