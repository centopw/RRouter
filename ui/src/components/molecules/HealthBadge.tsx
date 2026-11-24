import { cx } from "../../lib/cx";
import { Badge, type BadgeSize, type BadgeTone } from "../atoms/Badge";
import { Dot, type DotTone } from "../atoms/Dot";

/**
 * Shared platform status vocabulary across Usage, Providers, and Models:
 *   Healthy (pos) · Warning (warn) · Failing / Exhausted (neg) · Disabled / Inactive (neutral)
 *
 * Also maps Usage's `BoardStatusKind` keys (`at_risk`, `degraded`, `no_limit`, `loading`,
 * `admin_only`, `unavailable`) and Models' row states (`routed`, `hidden`, `not_in_preset`)
 * so every surface speaks one pill grammar.
 */
export type HealthStatus =
  | "healthy"
  | "warning"
  | "failing"
  | "exhausted"
  | "disabled"
  | "inactive"
  | "unavailable"
  | "info"
  | "at_risk"
  | "degraded"
  | "no_limit"
  | "loading"
  | "admin_only"
  | "routed"
  | "hidden"
  | "not_in_preset";

export type HealthBadgeSpec = {
  label: string;
  tone: BadgeTone;
  dot: DotTone;
  /** Hollow dot for intentionally disabled/hidden states so "off" never looks like "ready". */
  hollow?: boolean;
};

export const HEALTH_STATUS: Record<HealthStatus, HealthBadgeSpec> = {
  healthy: { label: "Healthy", tone: "pos", dot: "pos" },
  warning: { label: "Warning", tone: "warn", dot: "warn" },
  failing: { label: "Failing", tone: "neg", dot: "neg" },
  exhausted: { label: "Exhausted", tone: "neg", dot: "neg" },
  disabled: { label: "Disabled", tone: "neutral", dot: "muted", hollow: true },
  inactive: { label: "Inactive", tone: "neutral", dot: "muted", hollow: true },
  unavailable: { label: "Unavailable", tone: "warn", dot: "warn" },
  info: { label: "Info", tone: "info", dot: "info" },
  // Usage BoardStatusKind entries (moved from pages/usage/overview-panels.tsx)
  at_risk: { label: "Warning", tone: "warn", dot: "warn" },
  degraded: { label: "Warning", tone: "warn", dot: "warn" },
  no_limit: { label: "Healthy", tone: "neutral", dot: "muted" },
  loading: { label: "Checking…", tone: "neutral", dot: "muted" },
  admin_only: { label: "Active", tone: "neutral", dot: "muted" },
  // Models row status entries
  routed: { label: "Routed", tone: "pos", dot: "pos" },
  hidden: { label: "Hidden", tone: "neutral", dot: "muted", hollow: true },
  not_in_preset: { label: "Not in preset", tone: "neutral", dot: "muted", hollow: true },
};

export type HealthBadgeProps = {
  status: HealthStatus;
  /** Optional label override (e.g. `"Backoff"`, `"Suspended"`, `"No endpoint"`). */
  label?: string;
  /** Optional tone override when a domain state carries its own severity (e.g. refusal). */
  tone?: BadgeTone;
  size?: BadgeSize;
  title?: string;
  className?: string;
};

function dotForTone(tone: BadgeTone): DotTone {
  return tone === "neutral" ? "muted" : tone;
}

/** Tinted status pill with a leading indicator dot from the shared health vocabulary. */
export function HealthBadge({
  status,
  label,
  tone,
  size = "sm",
  title,
  className,
}: HealthBadgeProps) {
  const spec = HEALTH_STATUS[status];
  const resolvedTone = tone ?? spec.tone;
  const resolvedDot = tone ? dotForTone(tone) : spec.dot;
  const resolvedLabel = label ?? spec.label;
  const useHollow = Boolean(spec.hollow) && tone === undefined;

  return (
    <Badge tone={resolvedTone} size={size} title={title} className={className}>
      <span className="inline-flex min-w-0 items-center gap-1.5">
        {useHollow ? (
          <span
            aria-hidden
            className={cx("inline-block h-1.5 w-1.5 shrink-0 rounded-full border border-faint")}
          />
        ) : (
          <Dot tone={resolvedDot} className="shrink-0" />
        )}
        <span className="min-w-0 truncate">{resolvedLabel}</span>
      </span>
    </Badge>
  );
}
