import { cx } from "../../lib/cx";

export type DotTone = "pos" | "neg" | "warn" | "info" | "accent" | "muted";

const DOT_COLORS: Record<DotTone, string> = {
  pos: "bg-[var(--color-pos-fill)]",
  neg: "bg-[var(--color-neg-fill)]",
  warn: "bg-[var(--color-warn-fill)]",
  info: "bg-[var(--color-info)]",
  accent: "bg-accent",
  muted: "bg-faint",
};

export function Dot({ tone = "pos", className }: { tone?: DotTone; className?: string }) {
  return <span aria-hidden className={cx("inline-block h-1.5 w-1.5 rounded-full", DOT_COLORS[tone], className)} />;
}
