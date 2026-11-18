/**
 * Quota bars for the Usage page. Two sources feed one row: operator limits on the
 * connection (`used / limit`) and windows the provider itself reports (percent only).
 * Normal quota is uncoloured; warn/neg appear only once a threshold is crossed.
 */

import type { AccountQuotaRow, AccountSubscription, SubscriptionWindow } from "../../lib/api";
import { cx } from "../../components/ui";
import { compact, usd, when } from "../../lib/format";

export type QuotaBar = {
  id: string;
  /** Three-to-four characters so the bar stays one line inside a table cell. */
  label: string;
  percent: number;
  detail: string;
  title: string;
};

const WINDOW_SHORT: Record<string, string> = {
  five_hour: "5h",
  weekly: "wk",
  monthly: "mo",
  premium_interactions: "req",
  "premium-interactions": "req",
  completions: "code",
};

function shortWindowLabel(window: SubscriptionWindow): string {
  return WINDOW_SHORT[window.id] ?? window.label.slice(0, 4).toLowerCase();
}

/** Operator limits first, then the plan windows the provider reported. */
export function quotaBars(
  quota?: AccountQuotaRow,
  subscription?: AccountSubscription,
): QuotaBar[] {
  const bars: QuotaBar[] = [];
  if (quota) {
    const reset = quota.resets_at ? ` · resets ${when(quota.resets_at)}` : "";
    if (quota.max_requests > 0) {
      bars.push({
        id: "requests",
        label: "req",
        percent: (quota.requests / quota.max_requests) * 100,
        detail: `${compact(quota.requests)} / ${compact(quota.max_requests)}`,
        title: `${quota.window} request limit${reset}`,
      });
    }
    if (quota.max_tokens > 0) {
      bars.push({
        id: "tokens",
        label: "tok",
        percent: (quota.tokens / quota.max_tokens) * 100,
        detail: `${compact(quota.tokens)} / ${compact(quota.max_tokens)}`,
        title: `${quota.window} token limit${reset}`,
      });
    }
    if (quota.max_cost_micros > 0) {
      bars.push({
        id: "cost",
        label: "usd",
        percent: (quota.cost_micros / quota.max_cost_micros) * 100,
        detail: `${usd(quota.cost_micros)} / ${usd(quota.max_cost_micros)}`,
        title: `${quota.window} spend limit${reset}`,
      });
    }
  }
  for (const window of subscription?.windows ?? []) {
    bars.push({
      id: window.id,
      label: shortWindowLabel(window),
      percent: window.used_percent,
      detail: `${Math.round(window.used_percent)}%`,
      title: `${window.label}${window.resets_at ? ` · resets ${when(window.resets_at)}` : ""}`,
    });
  }
  return bars;
}

/** Worst bar in the row, used for the provider-level rollup. */
export function worstQuotaPercent(
  quota?: AccountQuotaRow,
  subscription?: AccountSubscription,
): number | null {
  const bars = quotaBars(quota, subscription);
  if (bars.length === 0) return null;
  return Math.max(...bars.map((bar) => bar.percent));
}

/** Green is not a state worth spending ink on: only warn and exhausted carry colour. */
export function quotaTone(percent: number): { fill: string; text: string } {
  if (percent >= 90) {
    return { fill: "bg-[var(--color-neg-fill)]", text: "text-[var(--color-neg)]" };
  }
  if (percent >= 70) {
    return { fill: "bg-[var(--color-warn)]", text: "text-[var(--color-warn)]" };
  }
  return { fill: "bg-[var(--color-line-strong)]", text: "text-muted" };
}

function Bar({ bar }: { bar: QuotaBar }) {
  const tone = quotaTone(bar.percent);
  const width = Math.min(100, Math.max(0, bar.percent));
  return (
    <div className="flex items-center gap-2 text-caption">
      <span className="w-7 shrink-0 text-faint">{bar.label}</span>
      <span
        role="progressbar"
        aria-label={bar.title}
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={Math.round(bar.percent)}
        title={bar.title}
        className="h-1 w-full max-w-[92px] overflow-hidden rounded-full bg-rule"
      >
        <span className={cx("block h-full rounded-full", tone.fill)} style={{ width: `${width}%` }} />
      </span>
      <span className={cx("shrink-0 text-right tnum", tone.text)}>{bar.detail}</span>
    </div>
  );
}

export function QuotaCell({
  quota,
  subscription,
}: {
  quota?: AccountQuotaRow;
  subscription?: AccountSubscription;
}) {
  const bars = quotaBars(quota, subscription);
  if (bars.length > 0) {
    return (
      <div className="flex flex-col gap-1">
        {bars.map((bar) => (
          <Bar key={bar.id} bar={bar} />
        ))}
      </div>
    );
  }
  if (subscription?.error) {
    return (
      <span className="text-caption text-faint" title={subscription.error}>
        quota unavailable
      </span>
    );
  }
  return <span className="text-caption text-faint">no limit</span>;
}
