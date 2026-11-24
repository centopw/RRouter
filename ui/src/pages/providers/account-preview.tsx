import { HoverPreview } from "../../components/hover-preview";
import { Badge, Dot, cx } from "../../components/ui";
import type { AccountQuotaRow, AccountSubscription, Connection } from "../../lib/api";
import { ms } from "../../lib/format";
import { paceSentence } from "../usage/headline";
import { accountRisk, isAlarming } from "../usage/pace";
import { QuotaCell as UsageQuotaCell, quotaBars } from "../usage/quota";
import {
  derivedIdentifiers,
  hostOf,
  type AccountStatus,
  type ConnectionTestResult,
} from "./types";

const STATUS_TEXT = {
  muted: "text-muted",
  warn: "text-[var(--color-warn)]",
  neg: "text-[var(--color-neg)]",
  info: "text-[var(--color-info)]",
} as const;

/**
 * Account name as a deep-link (`?inspect=<id>`) with a non-modal, non-focus-stealing
 * hover/focus preview scoped to "why is this account (not) ready".
 *
 * Preview contract:
 *   1. Status word and its reason (refusal copy, or the last upstream error).
 *   2. Every limit bar for the account (shared `usage/quota.tsx` QuotaCell).
 *   3. Last test result from this session, if any.
 *   4. Owner (team or shared) and priority.
 *
 * The interaction policy (350 ms hover, keyboard-focus open, Escape/blur close, touch
 * tap navigates, zero controls inside) lives in `components/hover-preview.tsx`; this
 * file only supplies the link label and the panel body.
 */
export function AccountNameLink({
  connection,
  display,
  custom,
  inspectHref,
  status,
  quota,
  subscription,
  quotaLoaded,
  lastTest,
  ownerLabel,
}: {
  connection: Connection;
  display: string;
  custom: boolean;
  inspectHref: string;
  status: AccountStatus;
  quota?: AccountQuotaRow;
  subscription?: AccountSubscription;
  quotaLoaded: boolean;
  lastTest: ConnectionTestResult | null;
  ownerLabel: string;
}) {
  const derived = derivedIdentifiers(connection, display).filter(
    (part) => part.text !== `${display} (OAuth)`,
  );
  const host = connection.base_url_override ? hostOf(connection.base_url_override) : null;
  const name = custom ? null : connection.name;
  const primary = name ?? host ?? derived[0]?.text ?? null;

  const hasLimits =
    quotaBars(quota, subscription).length > 0 || Boolean(subscription?.error) || quotaLoaded;
  const now = Date.now();
  const risk = subscription ? accountRisk(subscription, now) : null;
  const alarmingPace =
    risk && isAlarming(risk.worst.pace.state) ? paceSentence(risk.worst.pace, now) : null;

  const showLastErrorSeparately =
    Boolean(connection.last_error) && connection.last_error !== status.reason;

  const content = (
    <>
      <div className="flex items-start justify-between gap-2">
        <span className="min-w-0 truncate font-medium text-fg">
          {primary ?? "Unnamed account"}
        </span>
        <span className="flex shrink-0 items-center gap-1.5 text-caption">
          {status.kind === "refusal" ? (
            <Badge tone={status.refusal.tone}>{status.refusal.label}</Badge>
          ) : (
            <>
              <Dot tone={status.tone} />
              <span className={STATUS_TEXT[status.tone]}>{status.label}</span>
            </>
          )}
        </span>
      </div>

      {status.kind === "refusal" ? (
        <div className="flex flex-col gap-1 text-caption leading-relaxed">
          <p className="font-medium text-fg">{status.refusal.title}</p>
          <p className="text-muted">{status.refusal.description}</p>
        </div>
      ) : status.reason ? (
        <p className="text-caption leading-relaxed text-muted">{status.reason}</p>
      ) : null}

      {showLastErrorSeparately ? (
        <p className="line-clamp-3 break-words rounded border border-[var(--color-neg-line)] bg-[var(--color-neg-tint)] px-2 py-1.5 font-mono text-caption text-[var(--color-neg-ink)]">
          {connection.last_error}
        </p>
      ) : null}

      {hasLimits ? (
        <div className="flex flex-col gap-1.5 border-t border-rule pt-2.5">
          <UsageQuotaCell
            quota={quota}
            subscription={subscription}
            loaded={quotaLoaded}
          />
          {alarmingPace ? (
            <p className="text-caption text-[var(--color-warn)] tnum">{alarmingPace}</p>
          ) : null}
        </div>
      ) : (
        <div className="border-t border-rule pt-2.5 text-caption text-faint">
          Limits —
        </div>
      )}

      {lastTest ? (
        <div className="flex items-center gap-1.5 border-t border-rule pt-2.5 text-caption tnum">
          <Dot tone={lastTest.ok ? "pos" : "neg"} />
          <span
            className={cx(
              "min-w-0 truncate",
              lastTest.ok ? "text-[var(--color-pos)]" : "text-[var(--color-neg)]",
            )}
          >
            {lastTest.ok
              ? `Last test: works · ${ms(lastTest.latency_ms)}`
              : `Last test: failed${lastTest.status ? ` · ${lastTest.status}` : ""}${lastTest.error ? ` · ${lastTest.error}` : ""}`}
          </span>
        </div>
      ) : null}

      <div className="flex flex-wrap items-center justify-between gap-2 border-t border-rule pt-2 text-caption text-faint tnum">
        <span>{ownerLabel}</span>
        <span>priority p{connection.priority}</span>
      </div>
    </>
  );

  return (
    <HoverPreview
      to={inspectHref}
      linkClassName={cx(
        "block min-w-0 truncate rounded-[var(--radius-input)] underline-offset-2 hover:text-[var(--color-accent-text)] hover:underline focus-visible:text-[var(--color-accent-text)] focus-visible:underline focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--color-focus-ring)]",
        name ? "font-medium text-fg" : "text-ink-soft",
      )}
      contentClassName="gap-3"
      content={content}
    >
      {primary ?? <span className="text-faint">unnamed</span>}
    </HoverPreview>
  );
}
