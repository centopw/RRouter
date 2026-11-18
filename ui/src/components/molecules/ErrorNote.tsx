import { WarningCircle } from "@phosphor-icons/react";
import { cx } from "../../lib/cx";

export function ErrorNote({
  error,
  onRetry,
  title = "Request failed",
  className,
}: {
  error: unknown;
  onRetry?: () => void;
  title?: string;
  className?: string;
}) {
  const message = error instanceof Error ? error.message : String(error);
  return (
    <div
      role="alert"
      className={cx(
        "flex items-start gap-2.5 rounded-[var(--radius-surface)] border border-[var(--color-neg-line)] bg-[var(--color-neg-tint)] px-3 py-2 text-body text-[var(--color-neg-ink)]",
        className,
      )}
    >
      <WarningCircle size={16} aria-hidden className="mt-0.5 shrink-0 text-[var(--color-neg)]" />
      <span className="shrink-0 font-medium">{title}</span>
      <span className="min-w-0 flex-1 break-words">{message}</span>
      {onRetry ? (
        <button
          type="button"
          onClick={onRetry}
          className="shrink-0 cursor-pointer rounded-sm text-small font-medium underline underline-offset-2 hover:opacity-80 focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--color-focus-ring)]"
        >
          Retry
        </button>
      ) : null}
    </div>
  );
}
