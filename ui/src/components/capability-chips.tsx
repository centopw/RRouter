import type { Capabilities } from "../lib/api";
import { Badge } from "./ui";

/** The nine capabilities the registry advertises, in the order models are read. */
export const CAPABILITY_KEYS = [
  "vision",
  "pdf",
  "audio_input",
  "video_input",
  "image_output",
  "audio_output",
  "tools",
  "reasoning",
  "search",
] as const satisfies ReadonlyArray<keyof Capabilities>;

export const CAPABILITY_LABELS: Record<keyof Capabilities, { chip: string; title: string }> = {
  vision: { chip: "vision", title: "accepts image input" },
  pdf: { chip: "pdf", title: "accepts pdf input" },
  audio_input: { chip: "audio in", title: "accepts audio input" },
  video_input: { chip: "video in", title: "accepts video input" },
  image_output: { chip: "image out", title: "returns images" },
  audio_output: { chip: "audio out", title: "returns audio" },
  tools: { chip: "tools", title: "supports tool calls" },
  reasoning: { chip: "reasoning", title: "exposes reasoning output" },
  search: { chip: "search", title: "supports server-side search" },
};

/**
 * Only the capabilities that are actually true, each with the sentence that
 * explains it. Sparse capability maps (older rows) read as false, never as
 * unknown, so an empty row is honestly `—`.
 */
export function CapabilityChips({ capabilities }: { capabilities: Partial<Capabilities> | null | undefined }) {
  const on = CAPABILITY_KEYS.filter((key) => capabilities?.[key] === true);
  if (on.length === 0) return <span className="text-faint">—</span>;
  return (
    <span className="flex flex-wrap items-center gap-1">
      {on.map((key) => (
        <Badge key={key} tone="neutral" title={CAPABILITY_LABELS[key].title}>
          {CAPABILITY_LABELS[key].chip}
        </Badge>
      ))}
    </span>
  );
}
