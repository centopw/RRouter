import { cx } from "../lib/cx";

const LABELS: Record<string, string> = {
  none: "Off",
  minimal: "Minimal",
  low: "Low",
  medium: "Medium",
  high: "High",
  xhigh: "Extra high",
  max: "Max",
};

/**
 * Persistent thinking control under a model list. The API publishes levels highest first
 * while people read strength low to high, so the display order is reversed. A model with
 * no published levels still gets the footer: the absence is a fact, not a missing control.
 */
export function EffortPicker({
  levels,
  value,
  onChange,
  mandatory,
}: {
  levels: string[];
  value: string;
  onChange: (effort: string) => void;
  mandatory: boolean;
}) {
  const ordered = [...levels].reverse();
  const options = [...new Set(mandatory ? ordered : ["none", ...ordered])].filter(
    (level) => level !== "",
  );

  return (
    <div className="flex min-w-0 flex-col gap-1.5">
      <div className="flex items-center justify-between gap-2">
        <span className="text-caption font-medium text-muted">Thinking</span>
        <span className="text-caption text-faint">
          {options.length === 0 ? "not reported" : (LABELS[value] ?? value)}
        </span>
      </div>
      {options.length === 0 ? (
        <p className="text-caption text-faint">This model publishes no thinking levels.</p>
      ) : (
        <div role="radiogroup" aria-label="thinking level" className="flex flex-wrap gap-1">
          {options.map((level) => (
            <button
              key={level}
              type="button"
              role="radio"
              aria-checked={level === value}
              onClick={() => onChange(level)}
              className={cx(
                "rounded-md border px-2.5 py-1 text-caption transition-colors",
                level === value
                  ? "border-[var(--color-accent)] bg-[var(--color-selected)] font-medium text-fg"
                  : "border-line bg-surface-2 text-muted hover:border-line-strong hover:text-fg",
              )}
            >
              {LABELS[level] ?? level}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}
