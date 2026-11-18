/**
 * One line of total quota across every account that actually carries an operator limit.
 * The limit and its current-window consumption come from the account quota rows (the only
 * source of limits); the roster supplies how many accounts exist, so "N of M" means what
 * it says. Accounts without a limit take no part in the average, and with no limit anywhere
 * the line is not rendered at all — a bar needs a real budget behind it.
 */

import type { AccountQuotaRow, Connection } from "../../lib/api";
import { cx } from "../../components/ui";
import { compact, pct, usd } from "../../lib/format";

type Unit = "cost" | "tokens" | "requests";

type Dimension = {
  unit: Unit;
  /** Read in a sentence: "report a spend limit". */
  noun: string;
  limit: (row: AccountQuotaRow) => number;
  used: (row: AccountQuotaRow) => number;
  format: (value: number) => string;
};

/** Money first: a spend cap is the limit an operator reads as "quota". */
const COST: Dimension = {
  unit: "cost",
  noun: "spend",
  limit: (row) => row.max_cost_micros,
  used: (row) => row.cost_micros,
  format: (micros) => usd(micros),
};

const DIMENSIONS: Dimension[] = [
  COST,
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
  unit: Unit;
  /** How the counted limit reads in a sentence: "spend", "token", "request". */
  noun: string;
  used: number;
  limit: number;
  percent: number;
  /** Accounts the bar covers: those carrying a limit in the counted dimension. */
  limited: number;
  /** Every account the caller can see. */
  accounts: number;
  detail: string;
};

/** A quota row states a limit only when one of its three caps is non-zero. */
export function hasLimit(row: AccountQuotaRow): boolean {
  return row.max_cost_micros > 0 || row.max_tokens > 0 || row.max_requests > 0;
}

export function quotaTotal(
  quotas: AccountQuotaRow[],
  connections: Connection[],
): QuotaTotal | null {
  const limited = quotas.filter(hasLimit);
  if (limited.length === 0) return null;

  const dimension =
    DIMENSIONS.find((candidate) => limited.some((row) => candidate.limit(row) > 0)) ?? COST;
  const inDimension = limited.filter((row) => dimension.limit(row) > 0);
  const used = inDimension.reduce((total, row) => total + dimension.used(row), 0);
  const limit = inDimension.reduce((total, row) => total + dimension.limit(row), 0);

  const roster = new Set<string>(connections.map((connection) => connection.id));
  for (const quota of quotas) roster.add(quota.connection_id);

  return {
    unit: dimension.unit,
    noun: dimension.noun,
    used,
    limit,
    percent: limit > 0 ? (used / limit) * 100 : 0,
    limited: inDimension.length,
    accounts: Math.max(roster.size, limited.length),
    detail: `${dimension.format(used)} of ${dimension.format(limit)}`,
  };
}

/** Muted until the window is nearly spent; colour is a warning, not decoration. */
function fillTone(percent: number): string {
  if (percent >= 100) return "bg-[var(--color-neg-fill)]";
  if (percent >= 90) return "bg-[var(--color-warn)]";
  return "bg-[var(--color-line-strong)]";
}

export function QuotaTotalLine({
  quotas,
  connections,
}: {
  quotas: AccountQuotaRow[];
  connections: Connection[];
}) {
  const total = quotaTotal(quotas, connections);
  if (!total) return null;

  const width = Math.min(100, Math.max(0, total.percent));
  const tone = fillTone(total.percent);

  return (
    <div className="flex flex-wrap items-center gap-x-4 gap-y-1.5 text-small">
      <span className="font-medium text-fg">Quota this window</span>
      <span className={cx("tnum", total.percent >= 90 ? "text-warn" : "text-muted")}>
        {total.detail}
      </span>
      <span
        role="progressbar"
        aria-label={`total ${total.unit} quota used`}
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={Math.round(total.percent)}
        className="h-1.5 w-full max-w-[220px] overflow-hidden rounded-full bg-rule"
      >
        <span className={cx("block h-full rounded-full", tone)} style={{ width: `${width}%` }} />
      </span>
      <span className={cx("tnum", total.percent >= 90 ? "text-warn" : "text-muted")}>
        {pct(total.percent)}
      </span>
      <span className="text-faint tnum">
        {total.limited} of {total.accounts}{" "}
        {total.limited === 1 ? "account reports" : "accounts report"} a {total.noun} limit
      </span>
    </div>
  );
}
