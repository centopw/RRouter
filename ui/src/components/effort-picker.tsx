import { useRef, type KeyboardEvent } from "react";
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
  const buttonRefs = useRef<Array<HTMLButtonElement | null>>([]);
  const activeIndex = options.indexOf(value);
  const focusableIndex = activeIndex >= 0 ? activeIndex : 0;

  const moveFocusAndSelect = (targetIndex: number) => {
    const nextLevel = options[targetIndex];
    if (nextLevel === undefined) return;
    onChange(nextLevel);
    buttonRefs.current[targetIndex]?.focus();
  };

  const handleKeyDown = (event: KeyboardEvent<HTMLButtonElement>, index: number) => {
    if (options.length === 0) return;
    switch (event.key) {
      case "ArrowRight":
      case "ArrowDown":
        event.preventDefault();
        moveFocusAndSelect((index + 1) % options.length);
        break;
      case "ArrowLeft":
      case "ArrowUp":
        event.preventDefault();
        moveFocusAndSelect((index - 1 + options.length) % options.length);
        break;
      case "Home":
        event.preventDefault();
        moveFocusAndSelect(0);
        break;
      case "End":
        event.preventDefault();
        moveFocusAndSelect(options.length - 1);
        break;
      default:
        break;
    }
  };

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
          {options.map((level, index) => {
            const checked = level === value;
            return (
              <button
                key={level}
                ref={(el) => {
                  buttonRefs.current[index] = el;
                }}
                type="button"
                role="radio"
                aria-checked={checked}
                tabIndex={index === focusableIndex ? 0 : -1}
                onClick={() => onChange(level)}
                onKeyDown={(event) => handleKeyDown(event, index)}
                className={cx(
                  "rounded-md border px-2.5 py-1 text-caption transition-colors",
                  checked
                    ? "border-[var(--color-accent)] bg-[var(--color-selected)] font-medium text-fg"
                    : "border-line bg-surface-2 text-muted hover:border-line-strong hover:text-fg",
                )}
              >
                {LABELS[level] ?? level}
              </button>
            );
          })}
        </div>
      )}
    </div>
  );
}
