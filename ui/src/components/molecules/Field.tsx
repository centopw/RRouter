import { cloneElement, isValidElement, useId, type ReactElement, type ReactNode } from "react";
import * as Label from "@radix-ui/react-label";
import { cx } from "../../lib/cx";
import { Input } from "../atoms/Input";
import { Select } from "../atoms/Select";
import { Text } from "../atoms/Text";
import { Textarea } from "../atoms/Textarea";

type WireableControlProps = {
  id?: string;
  "aria-describedby"?: string;
  "aria-invalid"?: boolean;
  invalid?: boolean;
};

/**
 * Form field wrapper pairing a label, optional/required cues, aside metadata,
 * control wiring (`useId()` for `htmlFor`, `aria-describedby`, `invalid`), and inline
 * error/hint messaging.
 */
export function Field({
  label,
  hint,
  error,
  required = false,
  optional = false,
  aside,
  htmlFor,
  className,
  children,
}: {
  label: string;
  hint?: ReactNode;
  error?: ReactNode;
  required?: boolean;
  optional?: boolean;
  aside?: ReactNode;
  htmlFor?: string;
  className?: string;
  children: ReactNode;
}) {
  const generatedId = useId();
  const isElement = isValidElement<WireableControlProps>(children);
  const isCustomAtom =
    isElement && (children.type === Input || children.type === Textarea || children.type === Select);
  const isNativeControl =
    isElement &&
    typeof children.type === "string" &&
    (children.type === "input" || children.type === "textarea" || children.type === "select");
  const isDirectControl = isCustomAtom || isNativeControl;

  const existingId = isElement ? children.props.id : undefined;
  const controlId = htmlFor ?? existingId ?? generatedId;
  const errorId = error ? `${controlId}-error` : undefined;
  const hintId = hint ? `${controlId}-hint` : undefined;
  const describedBy = [errorId, hintId].filter(Boolean).join(" ") || undefined;

  let renderedControl = children;
  if (isDirectControl) {
    const child = children as ReactElement<WireableControlProps>;
    const mergedDescribedBy =
      [child.props["aria-describedby"], describedBy].filter(Boolean).join(" ") || undefined;
    const nextProps: WireableControlProps = {
      id: existingId ?? controlId,
      "aria-describedby": mergedDescribedBy,
    };
    if (error) {
      if (isCustomAtom) {
        nextProps.invalid = child.props.invalid ?? true;
      } else {
        nextProps["aria-invalid"] = child.props["aria-invalid"] ?? true;
      }
    }
    renderedControl = cloneElement(child, nextProps);
  }

  return (
    <div className={cx("flex min-w-0 flex-col gap-1.5", className)}>
      <div className="flex items-baseline justify-between gap-2">
        <Label.Root
          htmlFor={isDirectControl || htmlFor ? controlId : undefined}
          className="inline-flex items-baseline gap-1 text-small font-semibold text-ink-soft"
        >
          <span>{label}</span>
          {required ? (
            <>
              <span className="text-[var(--color-neg)]" aria-hidden="true">
                *
              </span>
              <span className="sr-only"> (required)</span>
            </>
          ) : null}
          {optional ? (
            <Text as="span" variant="caption" tone="faint" className="font-normal">
              (optional)
            </Text>
          ) : null}
        </Label.Root>
        {aside ? <div className="shrink-0 text-caption text-faint">{aside}</div> : null}
      </div>
      {renderedControl}
      {error ? (
        <span id={errorId} role="alert" className="text-caption text-[var(--color-neg-ink)]">
          {error}
        </span>
      ) : null}
      {hint ? (
        <Text as="span" id={hintId} variant="caption" tone="faint">
          {hint}
        </Text>
      ) : null}
    </div>
  );
}
