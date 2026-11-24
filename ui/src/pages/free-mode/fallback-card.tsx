import { Card, CardHeader, Toggle, cx } from "../../components/ui";
import { fallbackPreview, type FreeModeState } from "./model";

export function FallbackCard({
  state,
  busy,
  onToggleFallback,
}: {
  state: FreeModeState;
  busy: boolean;
  onToggleFallback: (enabled: boolean) => void;
}) {
  const preview = fallbackPreview(state);

  return (
    <Card>
      <CardHeader
        title="When your paid quota runs out"
        action={
          <div className="flex items-center gap-2">
            <span
              className={cx(
                "text-caption font-medium",
                state.fallback_on_quota ? "text-fg" : "text-faint",
              )}
            >
              {state.fallback_on_quota ? "On" : "Off"}
            </span>
            <Toggle
              checked={state.fallback_on_quota}
              onChange={onToggleFallback}
              label="Use free models automatically when paid quota runs out"
              disabled={busy}
              size="md"
            />
          </div>
        }
      />

      <div className="flex flex-col gap-2 px-5 pb-4">
        <p
          role="status"
          className={cx(
            "text-small leading-relaxed",
            preview.warning
              ? "rounded-[var(--radius-surface)] border border-[var(--color-warn-line)] bg-[var(--color-warn-tint)] px-3 py-2 text-[var(--color-warn-ink)]"
              : "text-fg",
          )}
        >
          {preview.summary}
        </p>
        <p className="text-caption leading-relaxed text-faint">{preview.footnote}</p>
      </div>
    </Card>
  );
}
