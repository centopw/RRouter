/**
 * Quota bars for the Usage page. Two sources feed one row: operator limits on the
 * connection (`used / limit`) and windows the provider itself reports (percent only).
 * Every bar names its window in full and says when it resets in plain text; nothing
 * lives in a hover title. Colour comes from the one shared ladder (providers/types.ts
 * `barFill` / `barText`): neutral until 70%, warn to 90%, then negative.
 */

import type { AccountQuotaRow, AccountSubscription, SubscriptionWindow } from "../../lib/api";
import { cx } from "../../components/ui";
import { compact, usd } from "../../lib/format";
import { barFill, barText, until, WINDOW_LABEL } from "../providers/types";

export type QuotaBar = {
  id: string;
  /** The window in full: "Weekly", "5-hour window", "Daily spend". */
  label: string;
  percent: number;
  /** "54%" for a plan window, "1.2K / 5K" for an operator limit. */
  detail: string;
  /** Unix seconds of the next reset, `null` when the source gave none. */
  resets_at: number | null;
};

/** The gateway spells the rolling window "5 hour"; the page says what it is. */
export function windowLabel(window: Pick<SubscriptionWindow, "id" | "label" | "window_seconds">): string {
  if (window.window_seconds === 5 * 3600 || /^5[\s-]?hour$/i.test(window.label.trim())) {
    return "5-hour window";
  }
  return window.label;
}

/** "resets in 6d 18h", or empty when the source gave no timestamp. */
export function resetsIn(resetsAt: number | null): string {
  const left = until(resetsAt);
  return left ? `resets in ${left}` : "";
}

/** "Nov 27" in the reader's locale. */
export function shortDate(ms: number): string {
  return new Date(ms).toLocaleDateString(undefined, { month: "short", day: "numeric" });
}

export function capitalized(text: string): string {
  return text.charAt(0).toUpperCase() + text.slice(1);
}

/** Operator limits first, then the plan windows the provider reported. */
export function quotaBars(
  quota?: AccountQuotaRow,
  subscription?: AccountSubscription,
): QuotaBar[] {
  const bars: QuotaBar[] = [];
  if (quota) {
    const window = capitalized(WINDOW_LABEL[quota.window] ?? quota.window);
    const resets_at = quota.resets_at;
    if (quota.max_requests > 0) {
      bars.push({
        id: "requests",
        label: `${window} requests`,
        percent: (quota.requests / quota.max_requests) * 100,
        detail: `${compact(quota.requests)} / ${compact(quota.max_requests)}`,
        resets_at,
      });
    }
    if (quota.max_tokens > 0) {
      bars.push({
        id: "tokens",
        label: `${window} tokens`,
        percent: (quota.tokens / quota.max_tokens) * 100,
        detail: `${compact(quota.tokens)} / ${compact(quota.max_tokens)}`,
        resets_at,
      });
    }
    if (quota.max_cost_micros > 0) {
      bars.push({
        id: "cost",
        label: `${window} spend`,
        percent: (quota.cost_micros / quota.max_cost_micros) * 100,
        detail: `${usd(quota.cost_micros)} / ${usd(quota.max_cost_micros)}`,
        resets_at,
      });
    }
  }
  for (const window of subscription?.windows ?? []) {
    bars.push({
      id: window.id,
      label: windowLabel(window),
      percent: window.used_percent,
      detail: `${Math.round(window.used_percent)}%`,
      resets_at: window.resets_at,
    });
  }
  return bars;
}

/** Worst bar in the row (label included), for the provider rollup and "Quota (now)". */
export function worstQuotaBar(
  quota?: AccountQuotaRow,
  subscription?: AccountSubscription,
): QuotaBar | null {
  const bars = quotaBars(quota, subscription);
  if (bars.length === 0) return null;
  return bars.reduce((worst, bar) => (bar.percent > worst.percent ? bar : worst));
}

/** Worst bar's percent, used for the provider-level rollup. */
export function worstQuotaPercent(
  quota?: AccountQuotaRow,
  subscription?: AccountSubscription,
): number | null {
  return worstQuotaBar(quota, subscription)?.percent ?? null;
}

/**
 * A magnitude bar: 6px tall, `barFill` on `bg-rule` (neutral fill ~6:1 in both themes),
 * and the number beside it carries the same tone. Width is the caller's.
 */
export function QuotaMeter({
  percent,
  label,
  className,
}: {
  percent: number;
  /** Accessible name, e.g. "Weekly, resets in 6d 18h". */
  label: string;
  className?: string;
}) {
  const width = Math.min(100, Math.max(0, percent));
  return (
    <span
      role="progressbar"
      aria-label={label}
      aria-valuemin={0}
      aria-valuemax={100}
      aria-valuenow={Math.round(percent)}
      className={cx("block h-1.5 w-full overflow-hidden rounded-full bg-rule", className)}
    >
      <span
        className={cx("block h-full rounded-full", barFill(percent))}
        style={{ width: `${width}%` }}
      />
    </span>
  );
}

function Bar({ bar }: { bar: QuotaBar }) {
  const reset = resetsIn(bar.resets_at);
  return (
    <div className="flex min-w-[148px] max-w-[220px] flex-col gap-1 text-caption">
      <div className="flex items-baseline justify-between gap-2">
        <span className="min-w-0 truncate text-muted">
          {bar.label}
          {reset ? <span className="text-faint"> · {reset}</span> : null}
        </span>
        <span className={cx("shrink-0 tnum", barText(bar.percent))}>{bar.detail}</span>
      </div>
      <QuotaMeter percent={bar.percent} label={reset ? `${bar.label}, ${reset}` : bar.label} />
    </div>
  );
}

export function QuotaCell({
  quota,
  subscription,
  loaded = true,
}: {
  quota?: AccountQuotaRow;
  subscription?: AccountSubscription;
  /**
   * Whether the quota sources have answered for this reader. `false` (non-admin, loading,
   * error) renders "—": absent data is not a claim that the account has no limit.
   */
  loaded?: boolean;
}) {
  const bars = quotaBars(quota, subscription);
  if (bars.length > 0) {
    return (
      <div className="flex flex-col gap-2">
        {bars.map((bar) => (
          <Bar key={bar.id} bar={bar} />
        ))}
      </div>
    );
  }
  if (subscription?.error) {
    return (
      <span className="flex max-w-[220px] flex-col text-caption">
        <span className="text-faint">quota unavailable</span>
        <span className="line-clamp-2 break-words text-faint">{subscription.error}</span>
      </span>
    );
  }
  if (!loaded) return <span className="text-caption text-faint">—</span>;
  return <span className="text-caption text-faint">no limit</span>;
}
