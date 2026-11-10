import { type ReactNode } from "react";
import { WarningOctagon } from "@phosphor-icons/react";
import * as AlertDialog from "@radix-ui/react-alert-dialog";
import { Button } from "../atoms/Button";
import { ErrorNote } from "./ErrorNote";

/**
 * Destructive confirmation on Radix AlertDialog: focus starts on the safe action, Escape
 * cancels, and the consequence is announced before the confirming verb.
 */
export function ConfirmDialog({
  open,
  title,
  message,
  confirmLabel = "Delete",
  busy = false,
  error,
  onConfirm,
  onClose,
}: {
  open: boolean;
  title: string;
  message: ReactNode;
  confirmLabel?: string;
  busy?: boolean;
  /** A failed confirmation stays visible instead of closing on the first click. */
  error?: unknown;
  onConfirm: () => void;
  onClose: () => void;
}) {
  return (
    <AlertDialog.Root open={open} onOpenChange={(next) => { if (!next) onClose(); }}>
      <AlertDialog.Portal>
        <AlertDialog.Overlay className="fixed inset-0 z-50 bg-[rgba(16,18,20,0.48)] backdrop-blur-[1.5px]" />
        <AlertDialog.Content className="fixed left-1/2 top-1/2 z-50 w-[min(92vw,520px)] -translate-x-1/2 -translate-y-1/2 rounded-[12px] border border-line bg-surface p-5 shadow-[0_24px_64px_rgba(16,18,20,0.22)] focus:outline-none">
          <AlertDialog.Title className="text-heading font-semibold leading-tight text-fg">
            {title}
          </AlertDialog.Title>
          <div className="mt-3 flex items-start gap-3">
            <span className="flex h-9 w-9 shrink-0 items-center justify-center rounded-full bg-[var(--color-neg-tint)] text-[var(--color-neg)]">
              <WarningOctagon size={19} aria-hidden />
            </span>
            <AlertDialog.Description asChild>
              <div className="min-w-0 text-body leading-relaxed text-ink-soft">{message}</div>
            </AlertDialog.Description>
          </div>
          {error ? (
            <div className="mt-3">
              <ErrorNote error={error} />
            </div>
          ) : null}
          <div className="mt-5 flex flex-wrap items-center justify-end gap-2">
            <AlertDialog.Cancel asChild>
              <Button disabled={busy}>cancel</Button>
            </AlertDialog.Cancel>
            <button
              type="button"
              onClick={onConfirm}
              disabled={busy}
              className="inline-flex h-8 items-center gap-1.5 rounded-full bg-[var(--color-neg-fill)] px-3.5 text-body font-semibold text-white transition-colors hover:bg-[var(--color-neg)] focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent-text disabled:cursor-not-allowed"
            >
              {busy ? "working…" : confirmLabel}
            </button>
          </div>
        </AlertDialog.Content>
      </AlertDialog.Portal>
    </AlertDialog.Root>
  );
}
