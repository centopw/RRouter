import { Check, Copy } from "@phosphor-icons/react";
import { cx } from "../../lib/cx";
import { useCopy } from "./useCopy";

/** A small, icon-only clipboard action that sits next to a selectable value. */
export function CopyIconButton({
  value,
  label = "Copy",
  className,
}: {
  value: string;
  label?: string;
  className?: string;
}) {
  const [copied, copy] = useCopy();
  const done = copied === value;
  return (
    <button
      type="button"
      onClick={() => copy(value)}
      aria-label={done ? "Copied" : label}
      title={done ? "Copied" : label}
      className={cx(
        "inline-flex h-6 w-6 shrink-0 cursor-pointer items-center justify-center rounded-md text-faint transition-colors hover:bg-surface-3 hover:text-fg focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent-text",
        className,
      )}
    >
      {done ? (
        <Check size={13} weight="bold" aria-hidden />
      ) : (
        <Copy size={13} aria-hidden />
      )}
    </button>
  );
}
