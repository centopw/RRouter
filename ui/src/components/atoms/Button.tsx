import { type MouseEventHandler, type ReactNode, type Ref } from "react";
import { cx } from "../../lib/cx";

type ButtonVariant = "primary" | "secondary" | "ghost" | "danger" | "quiet";
type ButtonSize = "sm" | "md" | "lg";

const SECONDARY_STYLE =
  "border border-[var(--color-line-strong)] bg-surface text-fg enabled:hover:bg-surface-3 disabled:border-line disabled:bg-surface-2 disabled:text-faint";

/* A ghost is a text action with no resting chrome; hover is the only affordance.
   It is NOT a second bordered button — that role belongs to `secondary`. */
const GHOST_STYLE =
  "border border-transparent bg-transparent text-fg enabled:hover:bg-surface-3 disabled:bg-transparent disabled:text-faint";

const STYLES: Record<ButtonVariant, string> = {
  primary:
    "bg-accent text-[var(--color-accent-ink)] font-semibold border border-transparent enabled:hover:bg-[var(--comp-button-primary-hover)] disabled:bg-surface-3 disabled:text-faint",
  secondary: SECONDARY_STYLE,
  ghost: GHOST_STYLE,
  quiet:
    "border border-line bg-surface text-muted enabled:hover:border-[var(--color-line-strong)] enabled:hover:bg-surface-3 enabled:hover:text-fg disabled:border-line disabled:bg-surface-2 disabled:text-faint",
  danger:
    "border border-[var(--color-neg-line)] bg-[var(--color-neg-tint)] text-[var(--color-neg-ink)] enabled:hover:border-[var(--color-neg)] disabled:border-line disabled:bg-surface-2 disabled:text-faint",
};

const SIZES: Record<ButtonSize, string> = {
  sm: "h-7 px-2.5 text-small",
  md: "h-8 px-3.5 text-body",
  lg: "h-11 px-5 text-body",
};

export function Button({
  children,
  onClick,
  variant = "ghost",
  disabled = false,
  loading = false,
  title,
  type = "button",
  className,
  arrow = false,
  size = "md",
  ariaLabel,
  ref,
}: {
  children: ReactNode;
  onClick?: MouseEventHandler<HTMLButtonElement>;
  variant?: ButtonVariant;
  disabled?: boolean;
  loading?: boolean;
  title?: string;
  type?: "button" | "submit" | "reset";
  className?: string;
  /** Opt-in directional arrow on CTAs; defaults to `false`. */
  arrow?: boolean;
  /** `sm`: 28px compact, `md`: 32px standard, `lg`: 44px auth/primary action. */
  size?: ButtonSize;
  /** Required when the button is icon-only. */
  ariaLabel?: string;
  ref?: Ref<HTMLButtonElement>;
}) {
  const isDisabled = disabled || loading;
  return (
    <button
      ref={ref}
      type={type}
      title={title}
      aria-label={ariaLabel}
      aria-busy={loading || undefined}
      onClick={onClick}
      disabled={isDisabled}
      className={cx(
        "inline-flex shrink-0 cursor-pointer items-center justify-center gap-1.5 whitespace-nowrap rounded-full transition-colors focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--color-focus-ring)] disabled:cursor-not-allowed",
        SIZES[size],
        STYLES[variant],
        className,
      )}
    >
      {loading ? (
        <span
          aria-hidden
          className="h-3.5 w-3.5 shrink-0 animate-spin rounded-full border-2 border-current border-t-transparent motion-reduce:animate-none"
        />
      ) : null}
      {children}
      {arrow ? <span aria-hidden>→</span> : null}
    </button>
  );
}
