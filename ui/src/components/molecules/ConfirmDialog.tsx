import { type ReactNode } from "react";
import { WarningOctagon } from "@phosphor-icons/react";
import { Button } from "../atoms/Button";
import { Modal } from "./Modal";

/**
 * Destructive confirmation. Every destructive action in the dashboard routes through
 * this dialog instead of `window.confirm`, so the consequence, the object and the
 * confirming verb are all visible at once.
 */
export function ConfirmDialog({
  open,
  title,
  message,
  confirmLabel = "Delete",
  busy = false,
  onConfirm,
  onClose,
}: {
  open: boolean;
  title: string;
  message: ReactNode;
  confirmLabel?: string;
  busy?: boolean;
  onConfirm: () => void;
  onClose: () => void;
}) {
  return (
    <Modal
      title={title}
      open={open}
      onClose={onClose}
      variant="dialog"
      footer={
        <>
          <Button onClick={onClose} disabled={busy}>
            cancel
          </Button>
          <button
            type="button"
            onClick={onConfirm}
            disabled={busy}
            className="inline-flex h-8 items-center gap-1.5 rounded-full bg-[var(--color-neg-fill)] px-3.5 text-body font-semibold text-white transition-colors hover:bg-[var(--color-neg)] focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent-text disabled:cursor-not-allowed"
          >
            {busy ? "working…" : confirmLabel}
          </button>
        </>
      }
    >
      <div className="flex items-start gap-3">
        <span className="flex h-9 w-9 shrink-0 items-center justify-center rounded-full bg-[var(--color-neg-tint)] text-[var(--color-neg)]">
          <WarningOctagon size={19} aria-hidden />
        </span>
        <div className="min-w-0 text-body leading-relaxed text-ink-soft">{message}</div>
      </div>
    </Modal>
  );
}
