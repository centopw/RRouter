import { useEffect, useId, useRef, useState, type ReactNode } from "react";
import { Check, Copy } from "@phosphor-icons/react";

/** Shared primitives. Every page composes these; none of them fetches. */

export function cx(...parts: Array<string | false | null | undefined>): string {
  return parts.filter(Boolean).join(" ");
}

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
          <h1 className="text-[32px] font-bold tracking-[-0.02em] text-fg">{title}</h1>
          {subtitle ? <p className="mt-1.5 text-[13.5px] text-muted">{subtitle}</p> : null}
        </div>
        {actions ? <div className="flex flex-wrap items-center gap-2">{actions}</div> : null}
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
    <div role="tablist" className="flex gap-6">
      {tabs.map((t) => {
        const active = t.id === value;
        return (
          <button
            key={t.id}
            role="tab"
            aria-selected={active}
            onClick={() => onChange(t.id)}
            className={cx(
              "-mb-px border-b-2 pb-2.5 text-sm transition-colors",
              active
                ? "border-accent text-fg"
                : "border-transparent text-muted hover:text-fg",
            )}
          >
            {t.label}
          </button>
        );
      })}
    </div>
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
}) {
  const styles: Record<ButtonVariant, string> = {
    primary:
      "bg-accent text-[var(--color-accent-ink)] font-semibold border border-transparent hover:bg-fg active:translate-y-[1px]",
    ghost:
      "border border-line-strong bg-surface text-fg hover:bg-surface-3 active:translate-y-[1px]",
    quiet: "border border-transparent text-muted hover:bg-surface-3 hover:text-fg",
    danger:
      "border border-[var(--color-neg-line)] bg-[var(--color-neg-tint)] text-[var(--color-neg-ink)] hover:border-[var(--color-neg)]",
  };
  return (
    <button
      type={type}
      title={title}
      onClick={onClick}
      disabled={disabled}
      className={cx(
        "inline-flex h-8 shrink-0 items-center gap-1.5 whitespace-nowrap rounded-full px-3.5 text-[13px] transition-colors focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent-text disabled:cursor-not-allowed disabled:opacity-40",
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
        "inline-flex items-center gap-1 rounded-full border px-2 py-0.5 text-[11px] leading-4",
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
              "h-[26px] rounded-full px-2.5 text-[12px] transition-colors max-[820px]:h-9",
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
        "inline-flex h-8 items-center gap-1.5 rounded-full border px-3 text-[12px] transition-colors max-[820px]:h-10",
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
          <div className="flex items-center gap-2 text-[14px] font-semibold text-[var(--color-neg-ink)]">
            <Dot tone="neg" />
            {title}
          </div>
          <div className="mt-1 text-[13px] text-muted">{children}</div>
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
    <label className="flex min-w-0 flex-col gap-1.5">
      <span className="text-[12px] font-semibold text-ink-soft">{label}</span>
      {children}
      {hint ? <span className="text-[11px] text-faint">{hint}</span> : null}
    </label>
  );
}

const control =
  "h-8 w-full min-w-0 rounded-[var(--radius-input)] border border-line-strong bg-surface px-2.5 text-[13px] text-fg placeholder:text-faint hover:border-[var(--color-fg)]/40 focus:border-[var(--color-accent-text)] focus:outline-none focus-visible:ring-2 focus-visible:ring-[var(--color-accent-text)]/25 disabled:bg-surface-3 disabled:text-muted";

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

export function Select({
  value,
  onChange,
  children,
  ariaLabel,
  disabled,
  className,
}: {
  value: string;
  onChange: (v: string) => void;
  children: ReactNode;
  ariaLabel: string;
  disabled?: boolean;
  className?: string;
}) {
  return (
    <select
      value={value}
      disabled={disabled}
      aria-label={ariaLabel}
      onChange={(e) => onChange(e.target.value)}
      className={cx(control, "appearance-none pr-7 bg-[right_0.5rem_center] bg-no-repeat", className)}
      style={{
        backgroundImage:
          "url(\"data:image/svg+xml;charset=utf-8,%3Csvg xmlns='http://www.w3.org/2000/svg' width='12' height='12' fill='none' stroke='%239b9b9b' stroke-width='1.5'%3E%3Cpath d='M3 5l3 3 3-3'/%3E%3C/svg%3E\")",
      }}
    >
      {children}
    </select>
  );
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
  onDark = false,
}: {
  value: string;
  prefix?: string;
  /** Shown instead of `value` when the full string cannot fit; the clipboard still gets
   *  the full value. */
  display?: string;
  /** On the near-black rail the light border would glow; use a hairline white one. */
  onDark?: boolean;
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
      title={`copy ${value}`}
      aria-label={`copy ${value}`}
      className={cx(
        "group inline-flex h-7 min-w-0 items-center gap-1.5 rounded-full border px-2 font-mono text-[11px]",
        onDark
          ? "border-white/15 bg-white/5 text-white/70 hover:border-white/25 hover:text-white"
          : "border-line bg-surface-2 text-muted hover:border-line-strong hover:text-fg",
      )}
    >
      {prefix ? (
        <span className={cx("shrink-0", onDark ? "text-white/45" : "text-faint")}>{prefix}</span>
      ) : null}
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
  return <div className="px-4 py-10 text-center text-[13px] text-faint">{children}</div>;
}

export function ErrorNote({ error, onRetry }: { error: unknown; onRetry?: () => void }) {
  const message = error instanceof Error ? error.message : String(error);
  return (
    <div className="flex items-start gap-3 rounded-lg border border-[var(--color-neg-line)] bg-[var(--color-neg-tint)] px-3 py-2 text-[13px] text-[var(--color-neg-ink)]">
      <span className="font-medium">Request failed</span>
      <span className="min-w-0 flex-1 break-words">{message}</span>
      {onRetry ? (
        <button onClick={onRetry} className="text-[12px] underline underline-offset-2">
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
      <h2 className="text-[16px] font-semibold text-fg">{title}</h2>
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
}: {
  title: string;
  open: boolean;
  onClose: () => void;
  children: ReactNode;
  footer?: ReactNode;
}) {
  const titleId = useId();
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    document.addEventListener("keydown", onKey);
    ref.current?.focus();
    return () => document.removeEventListener("keydown", onKey);
  }, [open, onClose]);

  if (!open) return null;
  return (
    <div className="fixed inset-0 z-50 flex items-start justify-center bg-black/70 p-4 pt-[10vh]">
      <div
        ref={ref}
        tabIndex={-1}
        // Focus lands here for screen readers; the accent focus ring is for controls.
        style={{ outline: "none" }}
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        className="w-full max-w-lg rounded-[var(--radius-card)] border border-line-strong bg-surface shadow-2xl focus:outline-none focus-visible:outline-none"
      >
        <div className="flex items-center justify-between border-b border-line px-5 py-3">
          <h2 id={titleId} className="text-[15px] font-medium">
            {title}
          </h2>
          <Button variant="quiet" onClick={onClose}>
            close
          </Button>
        </div>
        <div className="max-h-[65vh] overflow-auto px-5 py-4">{children}</div>
        {footer ? <div className="flex justify-end gap-2 border-t border-line px-5 py-3">{footer}</div> : null}
      </div>
    </div>
  );
}
