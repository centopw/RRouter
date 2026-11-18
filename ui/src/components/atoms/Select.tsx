import {
  Children,
  isValidElement,
  useId,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import * as Popover from "@radix-ui/react-popover";
import { cx } from "../../lib/cx";
import { CaretDown, Check, MagnifyingGlass, X } from "@phosphor-icons/react";
import { control, controlInvalid } from "./Input";

type Option = { value: string; label: string; group?: string; disabled?: boolean };

function readOptions(children: ReactNode, group?: string): Option[] {
  const out: Option[] = [];
  Children.forEach(children, (child) => {
    if (!isValidElement(child)) return;
    const props = child.props as { value?: unknown; children?: ReactNode; label?: string; disabled?: boolean };
    if (child.type === "option") {
      const raw = props.children;
      const label = Children.toArray(raw)
        .map((part) => (typeof part === "string" || typeof part === "number" ? String(part) : ""))
        .join("")
        .trim();
      out.push({ value: String(props.value ?? label), label, group, disabled: Boolean(props.disabled) });
    } else if (child.type === "optgroup") {
      out.push(...readOptions(props.children, props.label));
    }
  });
  return out;
}

/**
 * Searchable dropdown on Radix Popover. Same `value`/`onChange`/`<option>` surface as
 * before and the same filter-as-you-type behaviour, with positioning, outside dismissal
 * and focus containment from Radix.
 */
export function Select({
  value,
  onChange,
  children,
  ariaLabel,
  disabled,
  invalid = false,
  id,
  "aria-describedby": ariaDescribedBy,
  className,
  plain = false,
  footer,
}: {
  value: string;
  onChange: (v: string) => void;
  children: ReactNode;
  ariaLabel: string;
  disabled?: boolean;
  invalid?: boolean;
  id?: string;
  "aria-describedby"?: string;
  className?: string;
  /** Borderless trigger for a composer: the menu still has the full popover. */
  plain?: boolean;
  /** Persistent controls under the option list, outside its scroll area. */
  footer?: ReactNode;
}) {
  const options = useMemo(() => readOptions(children), [children]);
  const selected = options.find((option) => option.value === value);
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [active, setActive] = useState(0);
  const trigger = useRef<HTMLButtonElement>(null);
  const listId = useId();

  const filtered = useMemo(() => {
    const needle = query.trim().toLowerCase();
    if (!needle) return options;
    return options.filter(
      (option) =>
        option.label.toLowerCase().includes(needle) ||
        option.value.toLowerCase().includes(needle) ||
        (option.group?.toLowerCase().includes(needle) ?? false),
    );
  }, [options, query]);

  /**
   * Group consecutive options so the popover reads as provider sections rather than a
   * flat list with a repeated right-hand badge. Filtering keeps each option's index in
   * `filtered`, which is also the keyboard cursor.
   */
  const groups = useMemo(() => {
    const out: Array<{ label?: string; items: Array<{ option: Option; index: number }> }> = [];
    filtered.forEach((option, index) => {
      const last = out[out.length - 1];
      if (last && last.label === option.group) {
        last.items.push({ option, index });
      } else {
        out.push({ label: option.group, items: [{ option, index }] });
      }
    });
    return out;
  }, [filtered]);

  const close = () => setOpen(false);

  const choose = (option: Option) => {
    if (option.disabled) return;
    onChange(option.value);
    close();
    trigger.current?.focus();
  };

  const onKeyDown = (event: React.KeyboardEvent) => {
    if (!open) {
      if (["ArrowDown", "ArrowUp", "Enter", " "].includes(event.key)) {
        event.preventDefault();
        setOpen(true);
      }
      return;
    }
    if (event.key === "ArrowDown") {
      event.preventDefault();
      setActive((index) => Math.min(filtered.length - 1, index + 1));
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      setActive((index) => Math.max(0, index - 1));
    } else if (event.key === "Home") {
      event.preventDefault();
      setActive(0);
    } else if (event.key === "End") {
      event.preventDefault();
      setActive(Math.max(0, filtered.length - 1));
    } else if (event.key === "Enter") {
      event.preventDefault();
      const option = filtered[active];
      if (option) choose(option);
    }
  };

  return (
    <Popover.Root
      open={open}
      onOpenChange={(next) => {
        setOpen(next);
        if (next) {
          setQuery("");
          setActive(Math.max(0, options.findIndex((option) => option.value === value)));
        }
      }}
    >
      <Popover.Trigger asChild>
        <button
          id={id}
          ref={trigger}
          type="button"
          disabled={disabled}
          aria-label={ariaLabel}
          aria-invalid={invalid || undefined}
          aria-describedby={ariaDescribedBy}
          aria-haspopup="listbox"
          aria-expanded={open}
          aria-controls={open ? listId : undefined}
          onKeyDown={onKeyDown}
          className={cx(
            plain
              ? "flex h-8 w-full min-w-0 items-center justify-between gap-2 rounded-full border border-transparent bg-transparent px-2 text-left text-small text-ink-soft hover:bg-surface-3 focus-visible:border-[var(--color-focus-ring)] focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[var(--color-focus-ring)]/20"
              : cx(control, "flex items-center justify-between gap-2 text-left"),
            !plain && open && "border-[var(--color-focus-ring)] ring-2 ring-[var(--color-focus-ring)]/25",
            !plain && invalid && controlInvalid,
            className,
          )}
        >
          <span className={cx("truncate", selected ? "text-fg" : "text-faint")}>
            {selected?.label ?? "select…"}
          </span>
          <CaretDown size={12} aria-hidden className="shrink-0 text-faint" />
        </button>
      </Popover.Trigger>
      <Popover.Portal>
        <Popover.Content
          align="start"
          side="bottom"
          sideOffset={6}
          collisionPadding={12}
          sticky="always"
          className="popover-panel z-[var(--z-popover-portal)] max-w-[min(360px,calc(100vw-24px))] overflow-hidden rounded-[var(--radius-overlay)] border border-[var(--color-line-strong)] bg-surface shadow-[var(--shadow-popover)] focus:outline-none"
          style={{ width: "min(360px, max(220px, var(--radix-popover-trigger-width)))" }}
        >
          {options.length > 4 ? (
            <div className="flex items-center gap-2 border-b border-line bg-surface px-2.5 py-2 focus-within:border-[var(--color-focus-ring)] focus-within:ring-2 focus-within:ring-[var(--color-focus-ring)]/20">
              <MagnifyingGlass size={14} aria-hidden className="text-faint" />
              <input
                autoFocus
                value={query}
                onChange={(event) => {
                  setQuery(event.target.value);
                  setActive(0);
                }}
                onKeyDown={onKeyDown}
                placeholder="filter…"
                aria-label={`filter ${ariaLabel}`}
                aria-controls={listId}
                className="h-6 w-full bg-transparent text-small text-fg placeholder:text-faint focus:outline-none focus-visible:outline-none"
              />
              <X
                size={13}
                aria-hidden
                className="shrink-0 cursor-pointer text-faint hover:text-fg"
                onClick={() => setQuery("")}
              />
            </div>
          ) : null}
          <div id={listId} role="listbox" aria-label={ariaLabel}>
            <div className="max-h-[264px] overflow-auto py-1">
              {filtered.length === 0 ? (
                <div className="px-3 py-2 text-small text-faint">no match</div>
              ) : (
                groups.map((group, groupIndex) => (
                  <div
                    key={`${group.label ?? ""}:${groupIndex}`}
                    role={group.label ? "group" : undefined}
                    aria-label={group.label}
                  >
                    {group.label ? (
                      <div
                        aria-hidden
                        className="px-3 pb-1 pt-2 text-caption font-medium tracking-wide text-faint"
                      >
                        {group.label}
                      </div>
                    ) : null}
                    {group.items.map(({ option, index }) => (
                      <button
                        key={`${option.group ?? ""}:${option.value}`}
                        type="button"
                        role="option"
                        aria-selected={option.value === value}
                        disabled={option.disabled}
                        onMouseEnter={() => setActive(index)}
                        onClick={() => choose(option)}
                        className={cx(
                          "flex w-full items-center gap-2 px-3 py-1.5 text-left text-small focus-visible:outline-none",
                          index === active && "bg-[var(--color-selected)]",
                          option.value === value ? "font-medium text-fg" : "text-fg",
                          option.disabled && "cursor-not-allowed text-faint",
                        )}
                      >
                        <span className="w-3 shrink-0">
                          {option.value === value ? (
                            <Check size={12} weight="bold" aria-hidden />
                          ) : null}
                        </span>
                        <span className="min-w-0 flex-1 truncate">{option.label}</span>
                      </button>
                    ))}
                  </div>
                ))
              )}
            </div>
          </div>
          {footer ? (
            <div className="border-t border-line bg-surface px-2.5 py-2">{footer}</div>
          ) : null}
        </Popover.Content>
      </Popover.Portal>
    </Popover.Root>
  );
}
