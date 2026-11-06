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
 *
 * Rows stay one line tall in the table: at most `max` chips, the rest folded into
 * a `+N` badge whose tooltip names them.
 */
export function CapabilityChips({
  capabilities,
  max = 3,
}: {
  capabilities: Partial<Capabilities> | null | undefined;
  max?: number;
}) {
  const on = CAPABILITY_KEYS.filter((key) => capabilities?.[key] === true);
  if (on.length === 0) return <span className="text-faint">—</span>;
  const shown = on.slice(0, max);
  const rest = on.slice(max);
  return (
    <span className="flex flex-nowrap items-center gap-1">
      {shown.map((key) => (
        <Badge key={key} tone="neutral" title={CAPABILITY_LABELS[key].title}>
          {CAPABILITY_LABELS[key].chip}
        </Badge>
      ))}
      {rest.length > 0 ? (
        <Badge tone="neutral" title={rest.map((key) => CAPABILITY_LABELS[key].title).join(", ")}>
          +{rest.length}
        </Badge>
      ) : null}
    </span>
  );
}
