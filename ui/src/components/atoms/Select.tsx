import { useEffect, useId, useMemo, useRef, useState, Children, isValidElement, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { cx } from "../../lib/cx";
import { CaretDown, Check, MagnifyingGlass, X } from "@phosphor-icons/react";
import { control } from "../atoms/Input";

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
      out.push({
        value: String(props.value ?? label),
        label,
        group,
        disabled: Boolean(props.disabled),
      });
    } else if (child.type === "optgroup") {
      out.push(...readOptions(props.children, props.label));
    }
  });
  return out;
}

/**
 * Searchable dropdown. Drop-in for the old native `<select>`: the same `value`,
 * `onChange` and `<option>`/`<optgroup>` children, but the menu is ours — filter-as-you-
 * type, keyboard navigation, and no OS styling. The popover is portalled and positioned
 * from the trigger, so it is never clipped by a scrolling modal.
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
  const [rect, setRect] = useState<{
    left: number;
    top?: number;
    bottom?: number;
    width: number;
  } | null>(null);
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

  const place = () => {
    const box = trigger.current?.getBoundingClientRect();
    if (!box) return;
    const estimated = Math.min(320, options.length * 30 + 52);
    const opensBelow = window.innerHeight - box.bottom > estimated + 12;
    setRect({
      left: box.left,
      // Anchor the menu by the edge nearest the trigger, so the real content height
      // never leaves a gap: 6px under the trigger, or 6px above it.
      ...(opensBelow
        ? { top: box.bottom + 6 }
        : { bottom: window.innerHeight - box.top + 6 }),
      width: box.width,
    });
  };

  const show = () => {
    if (disabled) return;
    setQuery("");
    const index = options.findIndex((option) => option.value === value);
    setActive(Math.max(0, index));
    place();
    setOpen(true);
  };

  const close = () => setOpen(false);

  useEffect(() => {
    if (!open) return;
    const onDown = (event: MouseEvent) => {
      const target = event.target as Node;
      if (trigger.current?.contains(target)) return;
      if (document.getElementById(listId)?.contains(target)) return;
      close();
    };
    // Scrolling *inside* the menu must not dismiss it; only the page moving under the
    // trigger invalidates the fixed position.
    const onScroll = (event: Event) => {
      const target = event.target as Node | null;
      if (target && document.getElementById(listId)?.contains(target)) return;
      close();
    };
    const onResize = () => close();
    document.addEventListener("mousedown", onDown);
    window.addEventListener("scroll", onScroll, true);
    window.addEventListener("resize", onResize);
    return () => {
      document.removeEventListener("mousedown", onDown);
      window.removeEventListener("scroll", onScroll, true);
      window.removeEventListener("resize", onResize);
    };
  }, [open, listId]);

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
        show();
      }
      return;
    }
    if (event.key === "Escape") {
      event.preventDefault();
      close();
    } else if (event.key === "ArrowDown") {
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
    <>
      <button
        ref={trigger}
        type="button"
        disabled={disabled}
        aria-label={ariaLabel}
        aria-haspopup="listbox"
        aria-expanded={open}
        onClick={() => (open ? close() : show())}
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
      {open && rect
        ? createPortal(
            <div
              id={listId}
              role="listbox"
              aria-label={ariaLabel}
              style={{
                left: Math.min(rect.left, Math.max(8, window.innerWidth - Math.max(rect.width, 220) - 8)),
                ...(rect.top !== undefined ? { top: rect.top } : { bottom: rect.bottom }),
                width: Math.max(rect.width, 220),
              }}
              className="fixed z-[70] overflow-hidden rounded-[10px] border border-line-strong bg-surface shadow-[0_16px_40px_rgba(16,18,20,0.18)]"
            >
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
                    style={{ outline: "none" }}
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
            </div>,
            document.body,
          )
        : null}
    </>
  );
}
