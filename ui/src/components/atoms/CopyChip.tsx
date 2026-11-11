import { type FocusEvent } from "react";
import { cx } from "../../lib/cx";
import { CopyIconButton } from "./CopyIconButton";

/**
 * A value the operator needs verbatim (an endpoint, a key): shown in a real read-only
 * input so it can always be selected and copied by hand, with one small button for the
 * clipboard. Never make the whole value a button — that forbids manual selection.
 */
export function CopyChip({
  value,
  label,
  className,
}: {
  value: string;
  /** Accessible name for the value, used by the input and the copy button. */
  label?: string;
  className?: string;
}) {
  const select = (event: FocusEvent<HTMLInputElement>) => {
    event.currentTarget.select();
  };
  return (
    <div
      className={cx(
        "inline-flex min-w-0 max-w-full items-center gap-1 rounded-full border border-line bg-surface-2 py-0.5 pl-2.5 pr-1",
        className,
      )}
    >
      <input
        readOnly
        value={value}
        aria-label={label ?? value}
        title={value}
        spellCheck={false}
        onFocus={select}
        className="w-[11rem] min-w-0 flex-1 bg-transparent font-mono text-caption text-muted outline-none"
      />
      <CopyIconButton value={value} label={label ? `Copy ${label}` : `Copy ${value}`} />
    </div>
  );
}
