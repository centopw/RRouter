/**
 * Shared pace/plan/account formatting helpers for `/usage` and `/providers` previews.
 *
 * The single-account `<Headline>` hero card was deleted in the multi-provider Limits Board
 * redesign (`design/usage-redesign.md`); these pure formatters and `AccountLink` stay here
 * with stable export names and signatures because `providers/account-preview.tsx` imports
 * `paceSentence` from this module.
 */

import { Link } from "react-router-dom";
import type { AccountSubscription, Connection } from "../../lib/api";
import { cx } from "../../components/ui";
import { connectionLabel, providerLabel } from "../providers/types";
import type { Pace, WindowRisk } from "./pace";
import { capitalized, resetsIn, shortDate } from "./quota";

const DAY_MS = 86_400_000;

function clock(ms: number): string {
  return new Date(ms).toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" });
}

/** "Nov 27", or "at 14:30" when it is less than a day away; "~" when the window is a guess. */
export function hitWhen(pace: Pace, now: number): string {
  if (pace.limitHitAt === null) return "";
  const at =
    pace.limitHitAt - now < DAY_MS ? `at ${clock(pace.limitHitAt)}` : shortDate(pace.limitHitAt);
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
