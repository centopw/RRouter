/**
 * Operator quota totals on Activity, one line per window (daily / weekly / monthly).
 *
 * Caps of different windows are never added together: a daily $5 and a monthly $100 are
 * not a $105 budget. Within one window the line sums one dimension — spend if any account
 * there carries a spend cap, else tokens, else requests — over the accounts that carry that
 * cap, and says how many of the active accounts that is. Disabled accounts take no part:
 * they route nothing, so their caps are not budget. With no limit anywhere the component
 * renders nothing — a bar needs a real budget behind it.
 *
 * Provider plan windows (`/api/accounts/usage`) are deliberately absent: they are percent
 * only, with no absolute budget, so they cannot be summed honestly. They are shown per
 * account on /usage (Limits Board) instead.
 */

import type { AccountQuotaRow, AccountQuotaWindow, Connection } from "../../lib/api";
import { cx } from "../../components/ui";
import { compact, pct, usd } from "../../lib/format";
import { barText, QUOTA_WINDOWS } from "../providers/types";
import { QuotaMeter, resetsIn } from "../usage/quota";

type Unit = "cost" | "tokens" | "requests";

type Dimension = {
  unit: Unit;
  /** Read in a sentence: "a daily spend limit". */
  noun: string;
  limit: (row: AccountQuotaRow) => number;
  used: (row: AccountQuotaRow) => number;
  format: (value: number) => string;
};

/** Money first: a spend cap is the limit an operator reads as "quota". */
const DIMENSIONS: Dimension[] = [
  {
    unit: "cost",
    noun: "spend",
    limit: (row) => row.max_cost_micros,
    used: (row) => row.cost_micros,
    format: (micros) => usd(micros),
  },
  {
    unit: "tokens",
    noun: "token",
    limit: (row) => row.max_tokens,
    used: (row) => row.tokens,
    format: (value) => compact(value),
  },
  {
    unit: "requests",
    noun: "request",
    limit: (row) => row.max_requests,
    used: (row) => row.requests,
    format: (value) => compact(value),
  },
];

export type QuotaTotal = {
  window: AccountQuotaWindow;
  unit: Unit;
  /** How the counted limit reads in a sentence: "spend", "token", "request". */
  noun: string;
  used: number;
  limit: number;
  percent: number;
  /** Active accounts the bar covers: those with a cap in this window and dimension. */
  limited: number;
  /** Every active account the caller can see. */
  accounts: number;
  detail: string;
  /** Earliest reset among the counted rows (unix seconds), `null` when none was given. */
  resets_at: number | null;
};

/** An active quota row that states a limit (one of its three caps is non-zero). */
export function hasLimit(row: AccountQuotaRow): boolean {
  return (
    row.is_active && (row.max_cost_micros > 0 || row.max_tokens > 0 || row.max_requests > 0)
  );
}

/** One total per window that has any active limit, in daily → weekly → monthly order. */
export function quotaTotals(quotas: AccountQuotaRow[], connections: Connection[]): QuotaTotal[] {
  const active = new Set<string>(
    connections.filter((connection) => connection.is_active).map((connection) => connection.id),
  );
  for (const quota of quotas) if (quota.is_active) active.add(quota.connection_id);

  const totals: QuotaTotal[] = [];
  for (const { id: window } of QUOTA_WINDOWS) {
    const limited = quotas.filter((row) => row.window === window && hasLimit(row));
    if (limited.length === 0) continue;
    const dimension = DIMENSIONS.find((candidate) =>
      limited.some((row) => candidate.limit(row) > 0),
    )!;
    const counted = limited.filter((row) => dimension.limit(row) > 0);
    const used = counted.reduce((total, row) => total + dimension.used(row), 0);
    const limit = counted.reduce((total, row) => total + dimension.limit(row), 0);
    const resets = counted
      .map((row) => row.resets_at)
      .filter((ts): ts is number => ts !== null);
    totals.push({
      window,
      unit: dimension.unit,
      noun: dimension.noun,
      used,
      limit,
      percent: limit > 0 ? (used / limit) * 100 : 0,
      limited: counted.length,
      accounts: Math.max(active.size, counted.length),
      detail: `${dimension.format(used)} of ${dimension.format(limit)}`,
      resets_at: resets.length > 0 ? Math.min(...resets) : null,
    });
  }
  return totals;
}

function TotalLine({ total }: { total: QuotaTotal }) {
  const name = `${total.window.charAt(0).toUpperCase()}${total.window.slice(1)} ${total.noun}`;
  const reset = resetsIn(total.resets_at);
  return (
    <div className="flex flex-wrap items-center gap-x-4 gap-y-1.5 text-small">
      <span className="w-[112px] shrink-0 font-medium text-fg">{name}</span>
      <span className={cx("tnum", barText(total.percent))}>{total.detail}</span>
      <QuotaMeter
        percent={total.percent}
        label={reset ? `${name} quota used, ${reset}` : `${name} quota used`}
        className="max-w-[220px]"
      />
      <span className={cx("tnum", barText(total.percent))}>{pct(total.percent)}</span>
      {reset ? <span className="text-faint tnum">{reset}</span> : null}
      <span className="text-faint tnum">
        {total.limited} of {total.accounts} active{" "}
        {total.accounts === 1 ? "account" : "accounts"} {total.limited === 1 ? "has" : "have"} a{" "}
        {total.window} {total.noun}{" "}
        limit
      </span>
    </div>
  );
}

export function QuotaTotalLine({
  quotas,
  connections,
}: {
  quotas: AccountQuotaRow[];
  connections: Connection[];
}) {
  const totals = quotaTotals(quotas, connections);
  if (totals.length === 0) return null;
  return (
    <div className="flex flex-col gap-2">
      {totals.map((total) => (
        <TotalLine key={total.window} total={total} />
      ))}
    </div>
  );
}
