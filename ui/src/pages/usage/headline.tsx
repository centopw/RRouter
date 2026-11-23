/**
 * The one sentence at the top of /usage: which plan window is closest to running out,
 * and when. The headline account is the one whose limit is hit earliest at its current
 * pace (exhausted counts as now); with no forecast anywhere, the highest used share.
 * Colour is spent only on `on_pace_to_hit` / `exhausted`. Renders nothing when no account
 * reports a window. Accounts come from the page; the headline window's daily history (for
 * the chart under the sentence) is fetched here, since only this card knows which window
 * that is.
 */

import { Link } from "react-router-dom";
import type { AccountSubscription, Connection } from "../../lib/api";
import { Card, cx } from "../../components/ui";
import { useAccountUsageHistory } from "../../lib/queries";
import { connectionLabel, providerLabel } from "../providers/types";
import { chartable, LimitChart } from "./limit-chart";
import { isAlarming, rankAccounts, type Pace, type WindowRisk } from "./pace";
import { capitalized, resetsIn, shortDate, windowLabel } from "./quota";

const DAY_MS = 86_400_000;

function clock(ms: number): string {
  return new Date(ms).toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" });
}

/** "Nov 27", or "at 14:30" when it is less than a day away; "~" when the window is a guess. */
export function hitWhen(pace: Pace, now: number): string {
  if (pace.limitHitAt === null) return "";
  const at = pace.limitHitAt - now < DAY_MS ? `at ${clock(pace.limitHitAt)}` : shortDate(pace.limitHitAt);
  return pace.approximate ? `~${at}` : at;
}

/** What the forecast says, or `null` when there is none to give. */
export function paceSentence(pace: Pace, now: number): string | null {
  switch (pace.state) {
    case "exhausted":
      return "Limit reached";
    case "on_pace_to_hit":
      return `On pace to hit the limit ${hitWhen(pace, now)}`;
    case "ok":
      return pace.projectedAtReset === null
        ? null
        : `On pace for ${pace.approximate ? "~" : ""}${Math.round(pace.projectedAtReset)}% by reset`;
    case "unknown":
      return null;
  }
}

/** "Renews Dec 18" for windows of a day or more, "resets in 2h 1m" for shorter ones. */
export function renewalText(risk: WindowRisk): string {
  const { window } = risk;
  if (window.resets_at === null) return "";
  if (window.window_seconds !== null && window.window_seconds >= 86_400) {
    return `Renews ${shortDate(window.resets_at * 1000)}`;
  }
  return resetsIn(window.resets_at);
}

/** "Claude Max": provider plus the plan the provider reported, when it did. */
export function planTitle(account: AccountSubscription): string {
  const provider = providerLabel(account.provider);
  return account.plan ? `${provider} ${capitalized(account.plan)}` : provider;
}

export function accountName(account: AccountSubscription, connections?: Connection[]): string {
  const connection = connections?.find((row) => row.id === account.connection_id);
  if (connection) return connectionLabel(connection, providerLabel(account.provider));
  return account.name ?? providerLabel(account.provider);
}

/** Account name as a link that opens its drawer on /providers (`?inspect=`). */
export function AccountLink({
  account,
  connections,
  className,
}: {
  account: AccountSubscription;
  connections?: Connection[];
  className?: string;
}) {
  return (
    <Link
      to={`/providers?inspect=${encodeURIComponent(account.connection_id)}`}
      className={cx(
        "text-accent-text underline-offset-2 hover:underline focus-visible:underline",
        className,
      )}
    >
      {accountName(account, connections)}
    </Link>
  );
}

export function Headline({
  accounts,
  connections,
  now,
  className,
}: {
  /** `/api/accounts/usage` rows. */
  accounts: AccountSubscription[];
  /** Connection roster, used only to name the account the way /providers does. */
  connections?: Connection[];
  /** Clock override in ms, for tests; defaults to render time. */
  now?: number;
  className?: string;
}) {
  const at = now ?? Date.now();
  const top = rankAccounts(accounts, at)[0];
  const showChart = top !== undefined && chartable(top.worst.window);
  const history = useAccountUsageHistory(
    showChart ? top.account.connection_id : null,
    showChart ? top.worst.window.id : null,
  );
  if (!top) return null;

  const { account, worst } = top;
  const label = windowLabel(worst.window);
  const used = Math.round(worst.window.used_percent);
  const renewal = renewalText(worst);
  const forecast = paceSentence(worst.pace, at);
  const alarm = isAlarming(worst.pace.state);

  return (
    <Card className={cx("flex flex-col gap-4 px-5 py-4", className)}>
      <p className="text-lead text-fg">
        <span className="font-medium">{planTitle(account)}</span>
        <span className="text-faint"> · </span>
        <AccountLink account={account} connections={connections} />
        <span className="text-faint"> — </span>
        <span className="tnum">
          {used}% of {label.charAt(0).toLowerCase() + label.slice(1)} used
        </span>
        {renewal ? (
          <>
            <span className="text-faint"> · </span>
            <span className="text-muted tnum">{renewal}</span>
          </>
        ) : null}
        {forecast ? (
          <>
            <span className="text-faint"> · </span>
            <span className={cx("tnum", alarm ? "font-medium text-[var(--color-warn)]" : "text-muted")}>
              {forecast}
            </span>
          </>
        ) : null}
      </p>
      {!showChart ? null : history.data ? (
        <LimitChart window={worst.window} label={label} pace={worst.pace} history={history.data} now={at} />
      ) : history.isError ? (
        <p className="text-small text-faint">Usage history is unavailable right now.</p>
      ) : null}
    </Card>
  );
}
