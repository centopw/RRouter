/**
 * Limits card for /usage: every plan window of the headline account as one row
 * ("5-hour window · resets in 2h 1m", "21% used", full-width bar), then the other
 * accounts collapsed, riskiest first. These are current-window values, so the card
 * ignores the page's range control; "resets in" is what says so. Props-driven.
 */

import type { AccountSubscription, Connection } from "../../lib/api";
import { Card, cx } from "../../components/ui";
import { barText } from "../providers/types";
import { isAlarming, rankAccounts, windowRisk, type AccountRisk, type WindowRisk } from "./pace";
import { paceSentence, planTitle, AccountLink } from "./headline";
import { QuotaMeter, resetsIn, windowLabel } from "./quota";

function WindowRow({ risk, now }: { risk: WindowRisk; now: number }) {
  const { window, pace } = risk;
  const label = windowLabel(window);
  const reset = resetsIn(window.resets_at);
  const forecast = isAlarming(pace.state) ? paceSentence(pace, now) : null;
  return (
    <li className="flex flex-col gap-1.5">
      <div className="flex items-baseline justify-between gap-3 text-small">
        <span className="min-w-0 truncate">
          <span className="text-fg">{label}</span>
          {reset ? <span className="text-faint tnum"> · {reset}</span> : null}
        </span>
        <span className={cx("shrink-0 tnum", barText(window.used_percent))}>
          {Math.round(window.used_percent)}% used
        </span>
      </div>
      <QuotaMeter
        percent={window.used_percent}
        label={reset ? `${label}, ${reset}` : label}
      />
      {forecast ? (
        <span className="text-caption text-[var(--color-warn)] tnum">{forecast}</span>
      ) : null}
    </li>
  );
}

function OtherAccount({
  risk,
  connections,
}: {
  risk: AccountRisk;
  connections?: Connection[];
}) {
  const { account, worst } = risk;
  const label = windowLabel(worst.window);
  const reset = resetsIn(worst.window.resets_at);
  return (
    <li className="flex flex-col gap-1.5">
      <div className="flex items-baseline justify-between gap-3 text-small">
        <span className="min-w-0 truncate">
          <AccountLink account={account} connections={connections} />
          <span className="text-faint"> · {planTitle(account)}</span>
        </span>
        <span className={cx("shrink-0 tnum", barText(worst.window.used_percent))}>
          {Math.round(worst.window.used_percent)}% used
        </span>
      </div>
      <QuotaMeter
        percent={worst.window.used_percent}
        label={`${label}${reset ? `, ${reset}` : ""}`}
      />
      <span className="text-caption text-faint tnum">
        {label}
        {reset ? ` · ${reset}` : ""}
      </span>
    </li>
  );
}

export function LimitsCard({
  accounts,
  connections,
  now,
  className,
}: {
  /** `/api/accounts/usage` rows. */
  accounts: AccountSubscription[];
  /** Connection roster, used only to name accounts the way /providers does. */
  connections?: Connection[];
  /** Clock override in ms, for tests; defaults to render time. */
  now?: number;
  className?: string;
}) {
  const at = now ?? Date.now();
  const ranked = rankAccounts(accounts, at);
  const [top, ...rest] = ranked;
  if (!top) return null;

  // Provider order (5-hour before weekly) reads better than risk order inside one account.
  const rows = top.account.windows.map((window) => windowRisk(window, at));

  return (
    <Card className={cx("flex flex-col gap-4 px-5 py-4", className)}>
      <div className="flex flex-wrap items-baseline justify-between gap-x-3 gap-y-0.5">
        <h2 className="text-small font-semibold text-fg">Plan limits</h2>
        <span className="min-w-0 truncate text-small text-muted">
          <AccountLink account={top.account} connections={connections} />
          <span className="text-faint"> · {planTitle(top.account)}</span>
        </span>
      </div>

      <ul className="flex flex-col gap-3.5">
        {rows.map((risk) => (
          <WindowRow key={risk.window.id} risk={risk} now={at} />
        ))}
      </ul>

      {rest.length > 0 ? (
        <details className="group border-t border-rule pt-3">
          <summary className="cursor-pointer select-none text-small text-muted hover:text-fg">
            {rest.length} other {rest.length === 1 ? "account" : "accounts"}, by risk
          </summary>
          <ul className="mt-3 flex flex-col gap-3">
            {rest.map((risk) => (
              <OtherAccount key={risk.account.connection_id} risk={risk} connections={connections} />
            ))}
          </ul>
        </details>
      ) : null}
    </Card>
  );
}
