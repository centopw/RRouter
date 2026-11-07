import { useEffect, useRef, useState } from "react";
import { Check, Copy } from "@phosphor-icons/react";

/**
 * A value the operator needs verbatim (an endpoint, a key): monospace, and one click
 * away from the clipboard. Copying a truncated string would be worse than useless, so
 * the full value is in `value` even when the visible text is shortened.
 */
export function CopyChip({
  value,
  prefix,
  display,
}: {
  value: string;
  prefix?: string;
  /** Shown instead of `value` when the full string cannot fit; the clipboard still gets
   *  the full value. */
  display?: string;
}) {
  const [copied, setCopied] = useState(false);
  const timer = useRef<number | null>(null);

  useEffect(() => () => {
    if (timer.current !== null) window.clearTimeout(timer.current);
  }, []);

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(value);
    } catch {
      // Clipboard API needs a secure context; fall back to a hidden textarea.
      const area = document.createElement("textarea");
      area.value = value;
      area.style.position = "fixed";
      area.style.opacity = "0";
      document.body.appendChild(area);
      area.select();
      document.execCommand("copy");
      area.remove();
    }
    setCopied(true);
    if (timer.current !== null) window.clearTimeout(timer.current);
    timer.current = window.setTimeout(() => setCopied(false), 1_500);
  };

  return (
    <button
      type="button"
      onClick={copy}
      aria-label={`copy ${value}`}
      className="group inline-flex h-7 min-w-0 items-center gap-1.5 rounded-full border border-line bg-surface-2 px-2 font-mono text-caption text-muted hover:border-line-strong hover:text-fg"
    >
      {prefix ? <span className="shrink-0 text-faint">{prefix}</span> : null}
      <span className="truncate">{copied ? "copied" : (display ?? value)}</span>
      {copied ? (
        <Check size={12} weight="bold" aria-hidden className="shrink-0" />
      ) : (
        <Copy size={12} aria-hidden className="shrink-0 opacity-60 group-hover:opacity-100" />
      )}
    </button>
  );
}
