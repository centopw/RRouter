/**
 * Deep-link entity name with a non-modal, non-focus-stealing hover/focus preview.
 *
 * Both Providers and Usage put a name on screen whose real question ("what is this, and
 * why is it (not) ready?") belongs to that same screen, so the answer is a small panel
 * anchored to the name instead of a navigation. This component owns that whole policy
 * once, so the two screens cannot drift:
 *
 *   - mouse hover opens after 350 ms; leaving the link or the panel closes after 120 ms,
 *     and timers are cancelled when the pointer crosses between them;
 *   - keyboard focus opens immediately, but only for `:focus-visible` (mouse clicks and
 *     touch taps navigate straight through);
 *   - Escape on the focused link or the panel, or blurring it, closes immediately;
 *   - the panel never takes focus and contains zero interactive controls, so the link
 *     stays the single tab stop nobody has to tab out of.
 *
 * `children` is the link label; `content` is the panel body. Both callers vary only in
 * link styling and panel stack spacing, so those are the only styling hooks.
 */

import { useEffect, useRef, useState, type ReactNode } from "react";
import { Link } from "react-router-dom";
import * as Popover from "@radix-ui/react-popover";
import { cx } from "./ui";

const OPEN_DELAY_MS = 350;
const CLOSE_DELAY_MS = 120;

export function HoverPreview({
  to,
  linkClassName,
  contentClassName,
  children,
  content,
}: {
  to: string;
  linkClassName: string;
  contentClassName?: string;
  children: ReactNode;
  content: ReactNode;
}) {
  const [open, setOpen] = useState(false);
  const openTimer = useRef<number | null>(null);
  const closeTimer = useRef<number | null>(null);

  const clearTimers = () => {
    if (openTimer.current !== null) {
      window.clearTimeout(openTimer.current);
      openTimer.current = null;
    }
    if (closeTimer.current !== null) {
      window.clearTimeout(closeTimer.current);
      closeTimer.current = null;
    }
  };

  useEffect(() => clearTimers, []);

  const scheduleOpen = () => {
    clearTimers();
    openTimer.current = window.setTimeout(() => {
      openTimer.current = null;
      setOpen(true);
    }, OPEN_DELAY_MS);
  };

  const scheduleClose = () => {
    clearTimers();
    closeTimer.current = window.setTimeout(() => {
      closeTimer.current = null;
      setOpen(false);
    }, CLOSE_DELAY_MS);
  };

  const closeNow = () => {
    clearTimers();
    setOpen(false);
  };

  return (
    <Popover.Root open={open} modal={false}>
      <Popover.Anchor asChild>
        <Link
          to={to}
          onClick={closeNow}
          onPointerEnter={(event) => {
            if (event.pointerType !== "mouse") return;
            scheduleOpen();
          }}
          onPointerLeave={(event) => {
            if (event.pointerType !== "mouse") return;
            scheduleClose();
          }}
          onFocus={(event) => {
            // Only keyboard focus opens the preview; mouse clicks and touch taps navigate directly.
            if (event.currentTarget.matches(":focus-visible")) {
              clearTimers();
              setOpen(true);
            }
          }}
          onBlur={closeNow}
          onKeyDown={(event) => {
            if (event.key === "Escape" && open) {
              event.preventDefault();
              closeNow();
            }
          }}
          className={linkClassName}
        >
          {children}
        </Link>
      </Popover.Anchor>

      <Popover.Portal>
        <Popover.Content
          side="bottom"
          align="start"
          sideOffset={6}
          collisionPadding={12}
          onOpenAutoFocus={(event) => event.preventDefault()}
          onCloseAutoFocus={(event) => event.preventDefault()}
          onEscapeKeyDown={closeNow}
          onPointerEnter={(event) => {
            if (event.pointerType === "mouse") clearTimers();
          }}
          onPointerLeave={(event) => {
            if (event.pointerType === "mouse") scheduleClose();
          }}
          className={cx(
            "popover-panel z-[var(--z-popover-portal)] flex w-80 max-w-[calc(100vw-24px)] flex-col rounded-[var(--radius-overlay)] border border-[var(--color-line-strong)] bg-surface p-3.5 text-small shadow-[var(--shadow-popover)] focus:outline-none",
            contentClassName,
          )}
        >
          {content}
        </Popover.Content>
      </Popover.Portal>
    </Popover.Root>
  );
}
