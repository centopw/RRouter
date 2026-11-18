import { useEffect, useState, type ReactNode } from "react";
import { cx } from "../../lib/cx";
import { X } from "@phosphor-icons/react";
import * as Dialog from "@radix-ui/react-dialog";
import { Heading } from "../atoms/Heading";
import { IconButton } from "../atoms/IconButton";
import { Text } from "../atoms/Text";
import { ConfirmDialog } from "./ConfirmDialog";

export type ModalWidth = "sm" | "md" | "lg" | "xl";

const DIALOG_WIDTHS: Record<ModalWidth, string> = {
  sm: "max-w-[420px]",
  md: "max-w-[560px]",
  lg: "max-w-[720px]",
  xl: "max-w-[900px]",
};

const DRAWER_WIDTHS: Record<ModalWidth, string> = {
  sm: "max-w-[400px]",
  md: "max-w-[520px]",
  lg: "max-w-[700px]",
  xl: "max-w-[880px]",
};

export function Modal({
  title,
  open,
  onClose,
  children,
  footer,
  width = "md",
  description,
  variant = "drawer",
  modal = true,
  dirty = false,
  onEscapeKeyDown,
}: {
  title: string;
  open: boolean;
  onClose: () => void;
  children: ReactNode;
  footer?: ReactNode;
  /** Dialog width: sm 420/400, md 560/520, lg 720/700, xl 900/880. */
  width?: ModalWidth;
  /** Optional second line under the title. */
  description?: string;
  /** Right-hand side sheet by default; centred dialog for focused tasks & confirmations. */
  variant?: "drawer" | "dialog";
  /** Set `false` for non-blocking side inspectors that keep the underlying page interactive. */
  modal?: boolean;
  /** Guard accidental dismissal when the surface has unsaved edits. */
  dirty?: boolean;
  /**
   * Last look at Escape before the dialog closes: `preventDefault` keeps it open, which is
   * what a field that consumes the first Escape (a search that clears) needs.
   */
  onEscapeKeyDown?: (event: KeyboardEvent) => void;
}) {
  const [confirmingDiscard, setConfirmingDiscard] = useState(false);
  useEffect(() => {
    if (!open || !dirty) setConfirmingDiscard(false);
  }, [open, dirty]);

  const drawer = variant === "drawer";

  const requestClose = () => {
    if (dirty) {
      setConfirmingDiscard(true);
      return;
    }
    onClose();
  };

  return (
    <>
      <Dialog.Root
        open={open}
        modal={modal}
        onOpenChange={(next) => {
          if (!next) requestClose();
        }}
      >
        <Dialog.Portal>
          {modal ? (
            <Dialog.Overlay
              className={cx(
                "fixed inset-0 z-[var(--z-modal)] backdrop-blur-[1.5px]",
                drawer ? "bg-[var(--color-scrim-drawer)]" : "bg-[var(--color-scrim-modal)]",
              )}
            />
          ) : null}
          <Dialog.Content
            onEscapeKeyDown={(event) => {
              onEscapeKeyDown?.(event);
              if (event.defaultPrevented) return;
              if (dirty) {
                event.preventDefault();
                setConfirmingDiscard(true);
              }
            }}
            onInteractOutside={(event) => {
              if (!modal) {
                event.preventDefault();
                return;
              }
              if (dirty) {
                event.preventDefault();
                setConfirmingDiscard(true);
              }
            }}
            className={cx(
              "fixed z-[var(--z-modal)] flex w-full flex-col overflow-hidden border bg-surface shadow-[var(--shadow-modal)] focus:outline-none",
              drawer
                ? cx("drawer-panel inset-y-0 right-0 h-full border-l border-line", DRAWER_WIDTHS[width])
                : cx(
                    "dialog-panel left-1/2 top-1/2 max-h-[92vh] -translate-x-1/2 -translate-y-1/2 rounded-[var(--radius-overlay)] border-line",
                    DIALOG_WIDTHS[width],
                  ),
            )}
          >
            <header className="flex items-start justify-between gap-4 border-b border-line px-5 py-4 min-[560px]:px-6">
              <div className="min-w-0">
                <Dialog.Title asChild>
                  <Heading level={2} className="leading-tight">
                    {title}
                  </Heading>
                </Dialog.Title>
                {description ? (
                  <Dialog.Description asChild>
                    <Text as="p" tone="muted" className="mt-1 leading-relaxed">
                      {description}
                    </Text>
                  </Dialog.Description>
                ) : (
                  <Dialog.Description className="sr-only">{title}</Dialog.Description>
                )}
              </div>
              <Dialog.Close asChild>
                <IconButton label="Close" className="-mr-1">
                  <X size={16} aria-hidden />
                </IconButton>
              </Dialog.Close>
            </header>
            <div className="min-h-0 flex-1 overflow-auto px-5 py-5 min-[560px]:px-6">{children}</div>
            {footer ? (
              <footer className="flex flex-wrap items-center justify-end gap-2 border-t border-line bg-surface-2 px-5 py-3.5 min-[560px]:px-6">
                {footer}
              </footer>
            ) : null}
          </Dialog.Content>
        </Dialog.Portal>
      </Dialog.Root>
      <ConfirmDialog
        open={confirmingDiscard}
        title="Discard unsaved changes?"
        message="You have unsaved changes. Closing this panel will discard them."
        confirmLabel="Discard changes"
        onConfirm={() => {
          setConfirmingDiscard(false);
          onClose();
        }}
        onClose={() => setConfirmingDiscard(false)}
      />
    </>
  );
}
