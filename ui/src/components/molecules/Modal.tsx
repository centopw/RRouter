import { type ReactNode } from "react";
import { cx } from "../../lib/cx";
import { X } from "@phosphor-icons/react";
import * as Dialog from "@radix-ui/react-dialog";
import { Heading, IconButton, Text } from "../atoms";

export function Modal({
  title,
  open,
  onClose,
  children,
  footer,
  width = "md",
  description,
  variant = "drawer",
}: {
  title: string;
  open: boolean;
  onClose: () => void;
  children: ReactNode;
  footer?: ReactNode;
  /** Dialog width: md 560, lg 720, xl 900. */
  width?: "md" | "lg" | "xl";
  /** Optional second line under the title. */
  description?: string;
  /** Right-hand side sheet by default; small centred dialog for confirmations. */
  variant?: "drawer" | "dialog";
}) {
  const drawer = variant === "drawer";
  const dialogWidths = { md: "max-w-[560px]", lg: "max-w-[720px]", xl: "max-w-[900px]" } as const;
  const drawerWidths = { md: "max-w-[520px]", lg: "max-w-[700px]", xl: "max-w-[880px]" } as const;
  return (
    <Dialog.Root open={open} onOpenChange={(next) => { if (!next) onClose(); }}>
      <Dialog.Portal>
        <Dialog.Overlay
          className={cx(
            "fixed inset-0 z-50 backdrop-blur-[1.5px]",
            drawer ? "bg-[rgba(16,18,20,0.38)]" : "bg-[rgba(16,18,20,0.48)]",
          )}
        />
        <Dialog.Content
          className={cx(
            "fixed z-50 flex w-full flex-col overflow-hidden border bg-surface shadow-[0_24px_64px_rgba(16,18,20,0.22)] focus:outline-none",
            drawer
              ? cx("drawer-panel inset-y-0 right-0 h-full border-l border-line", drawerWidths[width])
              : cx("left-1/2 top-1/2 max-h-[92vh] -translate-x-1/2 -translate-y-1/2 rounded-[12px] border-line", dialogWidths[width]),
          )}
        >
          <header className="flex items-start justify-between gap-4 border-b border-line px-5 py-4 min-[560px]:px-6">
            <div className="min-w-0">
              <Dialog.Title asChild>
                <Heading level={2} className="leading-tight">{title}</Heading>
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
              <IconButton label="close" className="-mr-1">
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
  );
}
