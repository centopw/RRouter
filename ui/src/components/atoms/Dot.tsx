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

/** Colour is a status signal, so the default dot is neutral: pass a tone only when
 *  the dot marks real state (ok/warn/error/info). */
export function Dot({ tone = "muted", className }: { tone?: DotTone; className?: string }) {
  return <span aria-hidden className={cx("inline-block h-1.5 w-1.5 rounded-full", DOT_COLORS[tone], className)} />;
}
