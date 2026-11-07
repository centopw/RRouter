/** The shared type steps and text tones. Kept separate from the components so the
 *  layout primitives can reuse the same class maps. */
export type TextVariant =
  | "hero"
  | "display"
  | "title"
  | "subtitle"
  | "heading"
  | "lead"
  | "body"
  | "small"
  | "caption";

export const TYPE: Record<TextVariant, string> = {
  hero: "text-hero",
  display: "text-display",
  title: "text-title",
  subtitle: "text-subtitle",
  heading: "text-heading",
  lead: "text-lead",
  body: "text-body",
  small: "text-small",
  caption: "text-caption",
};

export type Tone = "default" | "muted" | "faint" | "accent" | "neg" | "pos" | "warn" | "inherit";

export const TONE: Record<Tone, string> = {
  default: "text-fg",
  muted: "text-muted",
  faint: "text-faint",
  accent: "text-[var(--color-accent-text)]",
  neg: "text-neg",
  pos: "text-pos",
  warn: "text-warn",
  inherit: "",
};
