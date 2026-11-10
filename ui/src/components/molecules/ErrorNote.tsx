

export function ErrorNote({ error, onRetry }: { error: unknown; onRetry?: () => void }) {
  const message = error instanceof Error ? error.message : String(error);
  return (
    <div
      role="alert"
      className="flex items-start gap-3 rounded-lg border border-[var(--color-neg-line)] bg-[var(--color-neg-tint)] px-3 py-2 text-body text-[var(--color-neg-ink)]"
    >
      <span className="font-medium">Request failed</span>
      <span className="min-w-0 flex-1 break-words">{message}</span>
      {onRetry ? (
        <button
          type="button"
          onClick={onRetry}
          className="text-small underline underline-offset-2"
        >
          retry
        </button>
      ) : null}
    </div>
  );
}
