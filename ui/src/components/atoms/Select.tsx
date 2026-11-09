import { Children, isValidElement, useId, useMemo, useRef, useState, type ReactNode } from "react";
import * as Popover from "@radix-ui/react-popover";
import { cx } from "../../lib/cx";
import { CaretDown, Check, MagnifyingGlass, X } from "@phosphor-icons/react";
import { control } from "./Input";

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
 * before and the same filter-as-you-type behaviour, but positioning, outside dismissal
 * and focus containment come from Radix rather than a hand-rolled portal.
 */
export function Select({
  value,
  onChange,
  children,
  ariaLabel,
  disabled,
  className,
  plain = false,
}: {
  value: string;
  onChange: (v: string) => void;
  children: ReactNode;
  ariaLabel: string;
  disabled?: boolean;
  className?: string;
  /** Borderless trigger for a composer: the menu still has the full popover. */
  plain?: boolean;
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
          ref={trigger}
          type="button"
          disabled={disabled}
          aria-label={ariaLabel}
          onKeyDown={onKeyDown}
          className={cx(
            plain
              ? "flex h-8 w-full min-w-0 items-center justify-between gap-2 rounded-full border border-transparent bg-transparent px-2 text-left text-small text-ink-soft hover:bg-surface-3 focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent-text"
              : control,
            !plain &&
              "flex items-center justify-between gap-2 text-left focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent-text",
            !plain && open && "border-accent-text",
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
          className="z-[70] max-w-[min(360px,calc(100vw-24px))] overflow-hidden rounded-[10px] border border-line-strong bg-surface shadow-[0_16px_40px_rgba(16,18,20,0.18)] focus:outline-none"
          style={{ width: "min(360px, max(220px, var(--radix-popover-trigger-width)))" }}
        >
          <div id={listId} role="listbox" aria-label={ariaLabel}>
            {options.length > 4 ? (
              <div className="flex items-center gap-2 border-b border-line bg-surface px-2.5 py-2 focus-within:border-accent-text">
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
                  className="h-6 w-full bg-transparent text-small text-fg placeholder:text-faint focus:outline-none"
                />
                <X
                  size={13}
                  aria-hidden
                  className="shrink-0 cursor-pointer text-faint hover:text-fg"
                  onClick={() => setQuery("")}
                />
              </div>
            ) : null}
            <div className="max-h-[264px] overflow-auto py-1">
              {filtered.length === 0 ? (
                <div className="px-3 py-2 text-small text-faint">no match</div>
              ) : (
                filtered.map((option, index) => (
                  <button
                    key={`${option.group ?? ""}:${option.value}`}
                    type="button"
                    role="option"
                    aria-selected={option.value === value}
                    disabled={option.disabled}
                    onMouseEnter={() => setActive(index)}
                    onClick={() => choose(option)}
                    className={cx(
                      "flex w-full items-center gap-2 px-3 py-1.5 text-left text-small",
                      index === active && "bg-[var(--color-selected)]",
                      option.value === value ? "font-medium text-fg" : "text-fg",
                      option.disabled && "cursor-not-allowed text-faint",
                    )}
                  >
                    <span className="w-3 shrink-0">
                      {option.value === value ? <Check size={12} weight="bold" aria-hidden /> : null}
                    </span>
                    <span className="min-w-0 flex-1 truncate">{option.label}</span>
                    {option.group ? (
                      <span className="shrink-0 text-caption text-faint">{option.group}</span>
                    ) : null}
                  </button>
                ))
              )}
            </div>
          </div>
        </Popover.Content>
      </Popover.Portal>
    </Popover.Root>
  );
}
