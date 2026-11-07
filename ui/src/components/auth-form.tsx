import { useEffect, useRef, useState, type ReactNode } from "react";
import { Link } from "react-router-dom";
import { Button, cx } from "./ui";
import { passwordStrength, MIN_PASSWORD } from "../lib/auth";

/**
 * The signed-out chrome: logo and name left, gateway address right, version below. No
 * sidebar, no navigation — every auth screen shares it.
 */
export function AuthShell({
  children,
  width = 440,
  footer = true,
}: {
  children: ReactNode;
  width?: number;
  footer?: boolean;
}) {
  return (
    <div className="flex min-h-full flex-col bg-bg text-fg">
      <header className="flex h-16 shrink-0 items-center gap-3 border-b border-line px-4 min-[820px]:px-7">
        <img
          src={`${import.meta.env.BASE_URL}logo-mark.png`}
          alt=""
          width={26}
          height={26}
          className="h-[26px] w-[26px] shrink-0 select-none"
        />
        <span className="text-[14px] font-semibold tracking-[-0.01em]">RRouter</span>
        <span className="ml-auto rounded-md border border-line bg-surface px-2.5 py-1 font-mono text-[11.5px] text-muted">
          {window.location.host}
        </span>
      </header>
      <main className="flex flex-1 justify-center px-4 py-10 min-[560px]:py-16">
        <div className="w-full" style={{ maxWidth: width }}>
          {children}
        </div>
      </main>
      {footer ? (
        <footer className="pb-8 text-center text-[11.5px] text-faint">
          Self-hosted · everything stays on this machine
        </footer>
      ) : null}
    </div>
  );
}

/** White panel, hairline border, generous padding. */
export function AuthCard({ children, className }: { children: ReactNode; className?: string }) {
  return (
    <section
      className={cx(
        "rounded-[8px] border border-line bg-surface px-6 py-8 shadow-[0_1px_2px_rgba(16,18,20,0.04)] min-[560px]:px-8",
        className,
      )}
    >
      {children}
    </section>
  );
}

export function AuthTitle({
  badge,
  title,
  lead,
}: {
  badge?: ReactNode;
  title: string;
  lead?: ReactNode;
}) {
  const ref = useRef<HTMLHeadingElement>(null);
  useEffect(() => {
    ref.current?.focus();
  }, []);
  return (
    <div className="mb-6">
      {badge ? <div className="mb-3">{badge}</div> : null}
      <h1
        ref={ref}
        tabIndex={-1}
        className="text-[24px] font-semibold leading-tight tracking-[-0.02em] focus:outline-none focus-visible:outline-none"
      >
        {title}
      </h1>
      {lead ? <p className="mt-2 text-[13.5px] leading-relaxed text-muted">{lead}</p> : null}
    </div>
  );
}

/** A label, a control and an optional hint or error. */
export function AuthField({
  label,
  hint,
  problem,
  htmlFor,
  children,
  aside,
}: {
  label: string;
  hint?: ReactNode;
  problem?: string | null;
  htmlFor: string;
  children: ReactNode;
  aside?: ReactNode;
}) {
  return (
    <div className="flex flex-col gap-1.5">
      <div className="flex items-baseline justify-between gap-3">
        <label htmlFor={htmlFor} className="text-[13px] font-medium">
          {label}
        </label>
        {aside}
      </div>
      {children}
      {problem ? (
        <span className="text-[11.5px] text-neg" role="alert">
          {problem}
        </span>
      ) : hint ? (
        <span className="text-[11.5px] leading-relaxed text-faint">{hint}</span>
      ) : null}
    </div>
  );
}

/** Tall text input used by every auth form. */
export function AuthInput({
  id,
  type = "text",
  value,
  onChange,
  autoComplete,
  disabled,
  ariaLabel,
}: {
  id: string;
  type?: string;
  value: string;
  onChange: (value: string) => void;
  autoComplete?: string;
  disabled?: boolean;
  ariaLabel?: string;
}) {
  return (
    <input
      id={id}
      type={type}
      value={value}
      onChange={(event) => onChange(event.target.value)}
      autoComplete={autoComplete}
      disabled={disabled}
      aria-label={ariaLabel}
      className={cx(
        "h-11 w-full rounded-[4px] border border-line-strong bg-surface px-3 text-[14px] text-fg",
        "placeholder:text-faint hover:border-fg/40 focus:border-accent-text focus:outline-none",
        "focus-visible:ring-2 focus-visible:ring-accent-text focus-visible:ring-offset-2",
        "disabled:cursor-not-allowed disabled:bg-surface-2 disabled:text-muted",
      )}
    />
  );
}

/**
 * Password input with an advisory meter. Only the 12-character minimum blocks submit;
 * the filled segments and the label are `aria-hidden`, the rule line is not.
 */
export function PasswordField({
  id,
  label,
  value,
  onChange,
  autoComplete,
  disabled,
  aside,
  rules = true,
}: {
  id: string;
  label: string;
  value: string;
  onChange: (value: string) => void;
  autoComplete?: string;
  disabled?: boolean;
  aside?: ReactNode;
  /** Set screens show the strength meter and the length rule; sign-in shows neither. */
  rules?: boolean;
}) {
  const [shown, setShown] = useState(false);
  const meter = passwordStrength(value);
  return (
    <div className="flex flex-col gap-1.5">
      <div className="flex items-baseline justify-between gap-3">
        <label htmlFor={id} className="text-[13px] font-medium">
          {label}
        </label>
        {aside}
      </div>
      <div className="relative">
        <input
          id={id}
          type={shown ? "text" : "password"}
          value={value}
          onChange={(event) => onChange(event.target.value)}
          autoComplete={autoComplete}
          disabled={disabled}
          className={cx(
            "h-11 w-full rounded-[4px] border border-line-strong bg-surface pl-3 pr-16 text-[14px] text-fg",
            "hover:border-fg/40 focus:border-accent-text focus:outline-none",
            "focus-visible:ring-2 focus-visible:ring-accent-text focus-visible:ring-offset-2",
            "disabled:cursor-not-allowed disabled:bg-surface-2 disabled:text-muted",
          )}
        />
        <button
          type="button"
          aria-pressed={shown}
          onClick={() => setShown((v) => !v)}
          className="absolute inset-y-0 right-1 my-1 rounded-md px-2 text-[11.5px] text-muted hover:text-fg"
        >
          {shown ? "hide" : "show"}
        </button>
      </div>
      {rules ? (
        <>
      <div className="flex items-center gap-2">
        <div aria-hidden className="flex flex-1 gap-1">
          {[0, 1, 2, 3].map((index) => (
            <span
              key={index}
              className={cx(
                "h-1 flex-1 rounded-full",
                index < meter.filled ? "bg-accent" : "bg-rule",
              )}
            />
          ))}
        </div>
        <span aria-hidden className="w-16 text-right text-[11px] text-muted">
          {value.length > 0 ? meter.label : ""}
        </span>
      </div>
      <span className="text-[11.5px] text-faint">
        At least {MIN_PASSWORD} characters. No composition rules.
      </span>
        </>
      ) : null}
    </div>
  );
}

/** `role="alert"` callout, tinted rather than red text alone. */
export function AuthNotice({
  tone = "neg",
  children,
}: {
  tone?: "neg" | "pos" | "warn";
  children: ReactNode;
}) {
  const styles = {
    neg: "border-[var(--color-neg-line)] bg-[var(--color-neg-tint)] text-[var(--color-neg-ink)]",
    pos: "border-[var(--color-pos)]/35 bg-[var(--color-pos-tint)] text-[var(--color-pos)]",
    warn: "border-[var(--color-warn-line)] bg-[var(--color-warn-tint)] text-[var(--color-warn-ink)]",
  } as const;
  return (
    <div role="alert" className={cx("rounded-[8px] border px-3 py-2 text-[12.5px]", styles[tone])}>
      {children}
    </div>
  );
}

/**
 * The ten recovery codes, shown exactly once. Codes live only in component state:
 * never storage, never the URL.
 */
export function RecoveryCodes({ codes }: { codes: string[] }) {
  const text = codes.join("\n");
  const download = () => {
    const blob = new Blob([`${text}\n`], { type: "text/plain" });
    const url = URL.createObjectURL(blob);
    const anchor = document.createElement("a");
    anchor.href = url;
    anchor.download = "rrouter-recovery-codes.txt";
    anchor.click();
    URL.revokeObjectURL(url);
  };
  return (
    <div className="flex flex-col gap-4">
      <div className="grid grid-cols-[repeat(auto-fit,minmax(130px,1fr))] gap-2">
        {codes.map((code) => (
          <code
            key={code}
            className="rounded-[4px] border border-line bg-surface-2 px-2 py-1.5 text-center font-mono text-[13px] tracking-wide"
          >
            {code}
          </code>
        ))}
      </div>
      <div className="flex flex-wrap gap-2">
        <Button
          onClick={() => {
            void navigator.clipboard?.writeText(text);
          }}
        >
          Copy all
        </Button>
        <Button onClick={download}>Download .txt</Button>
        <Button onClick={() => window.print()}>Print</Button>
      </div>
    </div>
  );
}

export function AuthFooterLink({ to, children }: { to: string; children: ReactNode }) {
  return (
    <Link to={to} className="font-medium text-accent-text underline underline-offset-2">
      {children}
    </Link>
  );
}
