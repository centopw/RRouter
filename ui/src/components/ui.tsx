import {
  Children,
  isValidElement,
  useEffect,
  useId,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { createPortal } from "react-dom";
import * as Dialog from "@radix-ui/react-dialog";
import * as Label from "@radix-ui/react-label";
import * as RadixTabs from "@radix-ui/react-tabs";
import { cx } from "../lib/cx";
import { Heading, IconButton, Inline, Text } from "./atoms";
import {
  CaretDown,
  Check,
  Copy,
  MagnifyingGlass,
  WarningOctagon,
  X,
} from "@phosphor-icons/react";

/** Shared primitives. Every page composes these; none of them fetches. */

export { cx } from "../lib/cx";

export function Card({
  children,
  className,
  as: As = "section",
}: {
  children: ReactNode;
  className?: string;
  as?: "section" | "div" | "article";
}) {
  return (
    <As
      className={cx(
        "rounded-[var(--radius-card)] border border-[var(--color-line)] bg-[var(--color-surface)]",
        className,
      )}
    >
      {children}
    </As>
  );
}

export function PageHeader({
  title,
  subtitle,
  actions,
  tabs,
}: {
  title: string;
  subtitle?: string;
  actions?: ReactNode;
  tabs?: ReactNode;
}) {
  return (
    <div className="mb-6">
      <div className="flex flex-wrap items-start justify-between gap-4">
        <div>
          <Heading level={1} className="font-bold">{title}</Heading>
          {subtitle ? <Text as="p" tone="muted" className="mt-1.5">{subtitle}</Text> : null}
        </div>
        {actions ? <Inline gap="sm" wrap>{actions}</Inline> : null}
      </div>
      {tabs ? <div className="mt-5 border-b border-line">{tabs}</div> : null}
    </div>
  );
}

export function Tabs({
  tabs,
  value,
  onChange,
}: {
  tabs: Array<{ id: string; label: string }>;
  value: string;
  onChange: (id: string) => void;
}) {
  return (
    <RadixTabs.Root value={value} onValueChange={onChange}>
      <RadixTabs.List className="flex gap-6">
        {tabs.map((t) => (
          <RadixTabs.Trigger
            key={t.id}
            value={t.id}
            className={cx(
              "-mb-px border-b-2 pb-2.5 text-body transition-colors",
              "border-transparent text-muted hover:text-fg",
              "data-[state=active]:border-accent data-[state=active]:text-fg",
            )}
          >
            {t.label}
          </RadixTabs.Trigger>
        ))}
      </RadixTabs.List>
    </RadixTabs.Root>
  );
}

type ButtonVariant = "primary" | "ghost" | "danger" | "quiet";

export function Button({
  children,
  onClick,
  variant = "ghost",
  disabled,
  title,
  type = "button",
  className,
  arrow,
  size = "md",
}: {
  children: ReactNode;
  onClick?: () => void;
  variant?: ButtonVariant;
  disabled?: boolean;
  title?: string;
  type?: "button" | "submit";
  className?: string;
  /** Primary actions carry a directional arrow; that is part of the brand. */
  arrow?: boolean;
  /** `lg` is the auth/primary-action height: 44px, comfortably tappable. */
  size?: "md" | "lg";
}) {
  const styles: Record<ButtonVariant, string> = {
    primary:
      "bg-accent text-[var(--color-accent-ink)] font-semibold border border-transparent hover:bg-fg disabled:bg-surface-3 disabled:text-faint",
    ghost:
      "border border-line-strong bg-surface text-fg hover:bg-surface-3 disabled:border-line disabled:bg-surface-2 disabled:text-faint",
    quiet: "border border-transparent text-muted hover:bg-surface-3 hover:text-fg disabled:text-faint",
    danger:
      "border border-[var(--color-neg-line)] bg-[var(--color-neg-tint)] text-[var(--color-neg-ink)] hover:border-[var(--color-neg)] disabled:border-line disabled:bg-surface-2 disabled:text-faint",
  };
  return (
    <button
      type={type}
      title={title}
      onClick={onClick}
      disabled={disabled}
      className={cx(
        "inline-flex shrink-0 items-center justify-center gap-1.5 whitespace-nowrap rounded-full transition-colors focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent-text disabled:cursor-not-allowed",
        size === "lg" ? "h-11 px-5 text-body" : "h-8 px-3.5 text-body",
        styles[variant],
        className,
      )}
    >
      {children}
      {arrow ?? variant === "primary" ? <span aria-hidden>→</span> : null}
    </button>
  );
}

export function Badge({
  children,
  tone = "neutral",
  title,
}: {
  children: ReactNode;
  tone?: "neutral" | "pos" | "neg" | "warn" | "accent";
  title?: string;
}) {
  const tones = {
    neutral: "border-line-strong bg-surface-2 text-muted",
    pos: "border-[var(--color-pos)]/35 bg-[var(--color-pos-tint)] text-[var(--color-pos)]",
    neg: "border-[var(--color-neg-line)] bg-[var(--color-neg-tint)] text-[var(--color-neg-ink)]",
    warn: "border-[var(--color-warn-line)] bg-[var(--color-warn-tint)] text-[var(--color-warn-ink)]",
    accent: "border-[var(--color-accent-text)]/35 bg-[var(--color-accent)]/10 text-[var(--color-accent-text)]",
  } as const;
  return (
    <span
      title={title}
      className={cx(
        "inline-flex items-center gap-1 rounded-full border px-2 py-0.5 text-caption leading-4",
        tones[tone],
      )}
    >
      {children}
    </span>
  );
}

/** A small set of mutually exclusive options: range pickers, chart metrics. */
export function Segmented<T extends string>({
  options,
  value,
  onChange,
  ariaLabel,
  className,
}: {
  options: ReadonlyArray<{ id: T; label: string }>;
  value: T;
  onChange: (v: T) => void;
  ariaLabel: string;
  className?: string;
}) {
  return (
    <div
      role="group"
      aria-label={ariaLabel}
      className={cx(
        "inline-flex items-center gap-0.5 rounded-full border border-line bg-surface-3 p-0.5",
        className,
      )}
    >
      {options.map((o) => {
        const on = o.id === value;
        return (
          <button
            key={o.id}
            type="button"
            aria-pressed={on}
            onClick={() => onChange(o.id)}
            className={cx(
              "h-[26px] rounded-full px-2.5 text-small transition-colors max-[820px]:h-9",
              on ? "bg-accent font-medium text-[var(--color-accent-ink)]" : "text-muted hover:text-fg",
            )}
          >
            {o.label}
          </button>
        );
      })}
    </div>
  );
}

/** A pill that filters a list; `on` marks the active filter, `bad` an error count. */
export function FilterChip({
  children,
  on = false,
  bad = false,
  onClick,
  ariaLabel,
}: {
  children: ReactNode;
  on?: boolean;
  bad?: boolean;
  onClick: () => void;
  ariaLabel?: string;
}) {
  return (
    <button
      type="button"
      aria-pressed={on}
      aria-label={ariaLabel}
      onClick={onClick}
      className={cx(
        "inline-flex h-8 items-center gap-1.5 rounded-full border px-3 text-small transition-colors max-[820px]:h-10",
        on
          ? "border-accent bg-accent font-medium text-[var(--color-accent-ink)]"
          : bad
            ? "border-[var(--color-neg-line)] bg-[var(--color-neg-tint)] text-[var(--color-neg-ink)] hover:border-[var(--color-neg)]"
            : "border-line bg-surface-2 text-muted hover:border-line-strong hover:text-fg",
      )}
    >
      {children}
    </button>
  );
}

const STATUS_TEXT: Record<number, string> = {
  200: "OK",
  201: "Created",
  400: "Bad Request",
  401: "Unauthorized",
  402: "Payment Required",
  403: "Forbidden",
  404: "Not Found",
  408: "Timeout",
  413: "Payload Too Large",
  422: "Unprocessable",
  429: "Too Many Requests",
  499: "Client Closed",
  500: "Internal Error",
  502: "Bad Gateway",
  503: "Unavailable",
  504: "Gateway Timeout",
};

export function statusText(status: number): string {
  return STATUS_TEXT[status] ?? (status >= 500 ? "Server Error" : status >= 400 ? "Client Error" : "OK");
}

/** HTTP outcome as a pill: tinted by class, and always carrying the code in text. */
export function StatusPill({ status, title }: { status: number; title?: string }) {
  const ok = status >= 200 && status < 400;
  return (
    <Badge tone={ok ? "pos" : "neg"} title={title}>
      <span className="font-mono">
        {status} {statusText(status)}
      </span>
    </Badge>
  );
}

/** One loud line for a real problem; silent when everything is healthy. */
export function AlertBanner({
  title,
  children,
  actions,
}: {
  title: string;
  children: ReactNode;
  actions?: ReactNode;
}) {
  return (
    <div
      role="alert"
      className="mb-5 rounded-[var(--radius-card)] border border-[var(--color-neg-line)] bg-[var(--color-neg-tint)] px-5 py-4"
    >
      <div className="flex flex-wrap items-start gap-x-4 gap-y-2">
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-2 text-lead font-semibold text-[var(--color-neg-ink)]">
            <Dot tone="neg" />
            {title}
          </div>
          <div className="mt-1 text-body text-muted">{children}</div>
        </div>
        {actions ? <div className="flex flex-wrap items-center gap-2">{actions}</div> : null}
      </div>
    </div>
  );
}

export function Dot({ tone = "pos" }: { tone?: "pos" | "neg" | "warn" | "muted" }) {
  const color = { pos: "bg-pos", neg: "bg-neg", warn: "bg-warn", muted: "bg-faint" }[tone];
  return <span aria-hidden className={cx("inline-block h-1.5 w-1.5 rounded-full", color)} />;
}

export function Field({ label, hint, children }: { label: string; hint?: string; children: ReactNode }) {
  return (
    <Label.Root className="flex min-w-0 flex-col gap-1.5">
      <Text as="span" variant="small" className="font-semibold text-ink-soft">{label}</Text>
      {children}
      {hint ? <Text as="span" variant="caption" tone="faint">{hint}</Text> : null}
    </Label.Root>
  );
}

const control =
  "h-8 w-full min-w-0 rounded-[var(--radius-input)] border border-line-strong bg-surface px-2.5 text-body text-fg placeholder:text-faint hover:border-[var(--color-fg)]/40 focus:border-[var(--color-accent-text)] focus:outline-none focus-visible:ring-2 focus-visible:ring-[var(--color-accent-text)]/25 disabled:bg-surface-3 disabled:text-muted";

export function Input({
  value,
  onChange,
  placeholder,
  type = "text",
  disabled,
  ariaLabel,
  className,
}: {
  value: string;
  onChange: (v: string) => void;
  placeholder?: string;
  type?: "text" | "number" | "password";
  disabled?: boolean;
  ariaLabel?: string;
  className?: string;
}) {
  return (
    <input
      type={type}
      value={value}
      disabled={disabled}
      aria-label={ariaLabel ?? placeholder}
      placeholder={placeholder}
      onChange={(e) => onChange(e.target.value)}
      className={cx(control, className)}
    />
  );
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

type Option = { value: string; label: string; group?: string; disabled?: boolean };

/** Flatten `<option>`/`<optgroup>` children into plain option data. */
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

/** Card footer for form actions: a hairline separates actions from the fields. */
export function CardFooter({ children }: { children: ReactNode }) {
  return <div className="flex items-center justify-end gap-2 border-t border-line px-5 py-3">{children}</div>;
}

/**
 * A value the operator needs verbatim (an endpoint, a key): monospace, and one click
 * away from the clipboard. Copying a truncated string would be worse than useless, so
 * the full value is in `value` even when the visible text is shortened.
 */
export function CopyChip({
  value,
  prefix,
  display,
}: {
  value: string;
  prefix?: string;
  /** Shown instead of `value` when the full string cannot fit; the clipboard still gets
   *  the full value. */
  display?: string;
}) {
  const [copied, setCopied] = useState(false);
  const timer = useRef<number | null>(null);

  useEffect(() => () => {
    if (timer.current !== null) window.clearTimeout(timer.current);
  }, []);

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(value);
    } catch {
      // Clipboard API needs a secure context; fall back to a hidden textarea.
      const area = document.createElement("textarea");
      area.value = value;
      area.style.position = "fixed";
      area.style.opacity = "0";
      document.body.appendChild(area);
      area.select();
      document.execCommand("copy");
      area.remove();
    }
    setCopied(true);
    if (timer.current !== null) window.clearTimeout(timer.current);
    timer.current = window.setTimeout(() => setCopied(false), 1_500);
  };

  return (
    <button
      type="button"
      onClick={copy}
      aria-label={`copy ${value}`}
      className="group inline-flex h-7 min-w-0 items-center gap-1.5 rounded-full border border-line bg-surface-2 px-2 font-mono text-caption text-muted hover:border-line-strong hover:text-fg"
    >
      {prefix ? <span className="shrink-0 text-faint">{prefix}</span> : null}
      <span className="truncate">{copied ? "copied" : (display ?? value)}</span>
      {copied ? (
        <Check size={12} weight="bold" aria-hidden className="shrink-0" />
      ) : (
        <Copy size={12} aria-hidden className="shrink-0 opacity-60 group-hover:opacity-100" />
      )}
    </button>
  );
}

/** Clipboard write with the pre-Clipboard-API fallback; returns a transient flag. */
export function useCopy(): [string | null, (value: string) => void] {
  const [copied, setCopied] = useState<string | null>(null);
  const timer = useRef<number | null>(null);
  useEffect(() => () => {
    if (timer.current !== null) window.clearTimeout(timer.current);
  }, []);
  return [
    copied,
    (value: string) => {
      const done = () => {
        setCopied(value);
        if (timer.current !== null) window.clearTimeout(timer.current);
        timer.current = window.setTimeout(() => setCopied(null), 1_500);
      };
      void navigator.clipboard
        .writeText(value)
        .then(done)
        .catch(() => {
          const area = document.createElement("textarea");
          area.value = value;
          area.style.position = "fixed";
          area.style.opacity = "0";
          document.body.appendChild(area);
          area.select();
          document.execCommand("copy");
          area.remove();
          done();
        });
    },
  ];
}

export function CopyButton({
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
    <Button variant="quiet" onClick={() => copy(value)} className={className}>
      {done ? "Copied" : label}
    </Button>
  );
}

export function EmptyState({ children }: { children: ReactNode }) {
  return <div className="px-4 py-10 text-center text-body text-faint">{children}</div>;
}

export function ErrorNote({ error, onRetry }: { error: unknown; onRetry?: () => void }) {
  const message = error instanceof Error ? error.message : String(error);
  return (
    <div className="flex items-start gap-3 rounded-lg border border-[var(--color-neg-line)] bg-[var(--color-neg-tint)] px-3 py-2 text-body text-[var(--color-neg-ink)]">
      <span className="font-medium">Request failed</span>
      <span className="min-w-0 flex-1 break-words">{message}</span>
      {onRetry ? (
        <button onClick={onRetry} className="text-small underline underline-offset-2">
          retry
        </button>
      ) : null}
    </div>
  );
}

/** Section header used inside cards: title on the left, an action on the right. */
export function CardHeader({ title, action }: { title: string; action?: ReactNode }) {
  return (
    <div className="flex items-center justify-between px-5 pt-4 pb-3">
      <Heading level={2}>{title}</Heading>
      {action}
    </div>
  );
}

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
