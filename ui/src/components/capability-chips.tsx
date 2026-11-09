import * as Popover from "@radix-ui/react-popover";
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
 * Only the capabilities that are actually true, each with the sentence that explains it.
 * Sparse capability maps (older rows) read as false, never as unknown, so an empty row is
 * honestly `—`.
 *
 * The table shows at most `max` chips; the remainder is a real button that opens a popover
 * listing every hidden capability with its description. A tooltip alone was the old
 * behaviour and it was unreachable by keyboard and touch.
 */
export function CapabilityChips({
  capabilities,
  max = 2,
  subject,
}: {
  capabilities: Partial<Capabilities> | null | undefined;
  max?: number;
  /** The model or row this belongs to, so the trigger's name is unique on the page. */
  subject?: string;
}) {
  const on = CAPABILITY_KEYS.filter((key) => capabilities?.[key] === true);
  if (on.length === 0) return <span className="text-faint">—</span>;
  const shown = on.slice(0, max);
  const rest = on.slice(max);
  const who = subject ? ` for ${subject}` : "";
  return (
    <span className="flex flex-nowrap items-center gap-1">
      {shown.map((key) => (
        <Badge key={key} tone="neutral" title={CAPABILITY_LABELS[key].title}>
          {CAPABILITY_LABELS[key].chip}
        </Badge>
      ))}
      {rest.length > 0 ? (
        <Popover.Root>
          <Popover.Trigger asChild>
            <button
              type="button"
              aria-label={`Show ${rest.length} more ${rest.length === 1 ? "capability" : "capabilities"}${who}`}
              aria-haspopup="dialog"
              className="inline-flex h-5 shrink-0 items-center rounded-full border border-line bg-surface-2 px-1.5 text-caption text-muted transition-colors hover:bg-surface-3 hover:text-fg focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent-text"
            >
              +{rest.length}
            </button>
          </Popover.Trigger>
          <Popover.Portal>
            <Popover.Content
              side="bottom"
              align="end"
              sideOffset={4}
              aria-label={`Hidden capabilities${who}`}
              className="popover-panel z-50 w-64 rounded-[var(--radius-card)] border border-line bg-surface p-2 shadow-lg focus:outline-none"
            >
              <p className="px-1.5 pb-1 text-caption font-semibold uppercase tracking-[0.08em] text-faint">
                Also supports
              </p>
              <ul className="flex flex-col">
                {rest.map((key) => (
                  <li key={key} className="flex items-baseline gap-2 rounded px-1.5 py-1">
                    <span className="shrink-0 text-small font-medium text-fg">
                      {CAPABILITY_LABELS[key].chip}
                    </span>
                    <span className="min-w-0 text-caption text-muted">
                      {CAPABILITY_LABELS[key].title}
                    </span>
                  </li>
                ))}
              </ul>
              <Popover.Arrow className="fill-[var(--color-line)]" width={10} height={5} />
            </Popover.Content>
          </Popover.Portal>
        </Popover.Root>
      ) : null}
    </span>
  );
}
