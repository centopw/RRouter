import { type ReactNode } from "react";
import { cx } from "../../lib/cx";

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
  ariaLabel,
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
  /** Required when the button is icon-only. */
  ariaLabel?: string;
}) {
  const styles: Record<ButtonVariant, string> = {
    primary:
      "bg-accent text-[var(--color-accent-ink)] font-semibold border border-transparent hover:bg-fg disabled:bg-surface-3 disabled:text-faint",
    ghost:
      "border border-line-strong bg-surface text-fg hover:bg-surface-3 disabled:border-line disabled:bg-surface-2 disabled:text-faint",
    quiet:
      "border border-line bg-surface text-muted hover:border-line-strong hover:bg-surface-3 hover:text-fg disabled:border-line disabled:bg-surface-2 disabled:text-faint",
    danger:
      "border border-[var(--color-neg-line)] bg-[var(--color-neg-tint)] text-[var(--color-neg-ink)] hover:border-[var(--color-neg)] disabled:border-line disabled:bg-surface-2 disabled:text-faint",
  };
  return (
    <button
      type={type}
      title={title}
      aria-label={ariaLabel}
      onClick={onClick}
      disabled={disabled}
      className={cx(
        "inline-flex shrink-0 cursor-pointer items-center justify-center gap-1.5 whitespace-nowrap rounded-full transition-colors focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent-text disabled:cursor-not-allowed",
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
