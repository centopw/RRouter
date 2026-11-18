/**
 * One dense table for the whole Usage page: a totals row, provider rows, then the
 * accounts and models inside each provider. Expansion is local; the range and the
 * provider filter live in the URL.
 */

import { Fragment, useState, type ReactNode } from "react";
import { CaretRight } from "@phosphor-icons/react";
import { Badge, Button, cx } from "../../components/ui";
import { ProviderIcon } from "../../components/provider-icon";
import { compact, ms, pct, splitRef, usd } from "../../lib/format";
import type { AccountQuotaRow, AccountSubscription } from "../../lib/api";
import { providerLabel } from "../providers/types";
import {
  errorRateOf,
  latencyOf,
  type AccountNode,
  type ProviderNode,
  type Totals,
} from "./aggregate";
import { QuotaCell, quotaTone, worstQuotaPercent } from "./quota";

export type AccountInfo = { name: string | null; email: string | null; isActive: boolean | null };

/** Child models shown at a time, so a 200-model account cannot bury the next account. */
const MODEL_PAGE = 25;

function Num({ children, className }: { children: ReactNode; className?: string }) {
  return (
    <td className={cx("px-3 py-2.5 text-right text-small tnum", className)}>{children}</td>
  );
}

export function UsageTable({
  providers,
  totals,
  accountsByProvider,
  accountInfo,
  quotaByConnection,
  subscriptionByConnection,
  loading,
  emptyText,
  expandedProviders,
  expandedAccounts,
  onToggleProvider,
  onToggleAccount,
}: {
  providers: ProviderNode[];
  totals: Totals;
  accountsByProvider: Map<string, AccountNode[]>;
  accountInfo: Map<string, AccountInfo>;
  quotaByConnection: Map<string, AccountQuotaRow>;
  subscriptionByConnection: Map<string, AccountSubscription>;
  loading: boolean;
  emptyText: string;
  expandedProviders: Set<string>;
  expandedAccounts: Set<string>;
  onToggleProvider: (provider: string) => void;
  onToggleAccount: (key: string) => void;
}) {
  const [allModelsShown, setAllModelsShown] = useState<Set<string>>(() => new Set());

  // Worst configured/reported window per provider, for one glanceable quota number.
  const worstByProvider = new Map<string, number>();
  for (const quota of quotaByConnection.values()) {
    const worst = worstQuotaPercent(quota, subscriptionByConnection.get(quota.connection_id));
    if (worst === null) continue;
    worstByProvider.set(quota.provider, Math.max(worst, worstByProvider.get(quota.provider) ?? 0));
  }
  for (const subscription of subscriptionByConnection.values()) {
    const worst = worstQuotaPercent(undefined, subscription);
    if (worst === null) continue;
    worstByProvider.set(
      subscription.provider,
      Math.max(worst, worstByProvider.get(subscription.provider) ?? 0),
    );
  }

  return (
    <div className="overflow-x-auto">
      <table className="w-full min-w-[900px] border-collapse text-small">
        <caption className="sr-only">
          Consumption by provider, account and model, with quota for each account
        </caption>
        <thead>
          <tr className="border-b border-line">
            <th
              scope="col"
              className="px-4 py-2 text-left text-caption font-medium uppercase tracking-[0.06em] text-faint"
            >
              Provider / account / model
            </th>
            <th scope="col" className="w-[88px] px-3 py-2 text-right text-caption font-medium uppercase tracking-[0.06em] text-faint">
              Requests
            </th>
            <th scope="col" className="w-[96px] px-3 py-2 text-right text-caption font-medium uppercase tracking-[0.06em] text-faint">
              Tokens
            </th>
            <th scope="col" className="w-[104px] px-3 py-2 text-right text-caption font-medium uppercase tracking-[0.06em] text-faint">
              Spend
            </th>
            <th scope="col" className="w-[78px] px-3 py-2 text-right text-caption font-medium uppercase tracking-[0.06em] text-faint">
              Errors
            </th>
            <th scope="col" className="w-[86px] px-3 py-2 text-right text-caption font-medium uppercase tracking-[0.06em] text-faint">
              Latency
            </th>
            <th
              scope="col"
              className="w-[232px] px-3 py-2 pl-6 text-left text-caption font-medium uppercase tracking-[0.06em] text-faint"
            >
              Quota
            </th>
          </tr>
        </thead>

        <tbody>
          <tr className="border-b border-line">
            <th scope="row" className="px-4 py-3 text-left text-body font-semibold text-fg">
              All providers
            </th>
            <Num className="font-semibold">{compact(totals.requests)}</Num>
            <Num className="font-semibold">{compact(totals.tokens)}</Num>
            <Num className="font-semibold">{usd(totals.costMicros)}</Num>
            <td
              className={cx(
                "px-3 py-3 text-right text-small font-semibold tnum",
                totals.errors > 0 ? "text-neg" : "text-faint",
              )}
            >
              {pct(errorRateOf(totals))}
            </td>
            <Num className="font-semibold">{ms(latencyOf(totals))}</Num>
            <td className="px-3 py-3 pl-6 text-caption text-faint">
              {providers.length === 1 ? "1 provider" : `${providers.length} providers`}
            </td>
          </tr>
        </tbody>

        {providers.length === 0 ? (
          <tbody>
            <tr>
              <td colSpan={7} className="px-4 py-10 text-center text-body text-faint">
                {loading ? "loading…" : emptyText}
              </td>
            </tr>
          </tbody>
        ) : null}

        {providers.map((node) => {
          const providerOpen = expandedProviders.has(node.provider);
          const accounts = accountsByProvider.get(node.provider) ?? [];
          const worst = worstByProvider.get(node.provider) ?? null;
          const tone = worst === null ? null : quotaTone(worst);
          return (
            <tbody key={node.provider}>
              <tr className="border-b border-line hover:bg-[var(--color-row-hover)]">
                <th scope="row" className="px-4 py-2.5 text-left text-body font-medium text-fg">
                  <button
                    type="button"
                    aria-expanded={providerOpen}
                    onClick={() => onToggleProvider(node.provider)}
                    className="flex w-full min-w-0 items-center gap-2 rounded text-left focus-visible:outline focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-accent-text"
                  >
                    <CaretRight
                      size={11}
                      aria-hidden
                      className={cx(
                        "shrink-0 text-faint transition-transform",
                        providerOpen && "rotate-90",
                      )}
                    />
                    <ProviderIcon provider={node.provider} size={14} />
                    <span className="truncate">{providerLabel(node.provider)}</span>
                    <span className="shrink-0 text-caption text-faint tnum">
                      {node.models.length} {node.models.length === 1 ? "model" : "models"}
                    </span>
                  </button>
                </th>
                <Num>{compact(node.totals.requests)}</Num>
                <Num>{compact(node.totals.tokens)}</Num>
                <Num>{usd(node.totals.costMicros)}</Num>
                <Num className={node.totals.errors > 0 ? "text-neg" : "text-faint"}>
                  {compact(node.totals.errors)}
                </Num>
                <Num className="text-muted">{ms(latencyOf(node.totals))}</Num>
                <td className="px-3 py-2.5 pl-6 text-caption tnum">
                  {worst === null ? (
                    <span className="text-faint">—</span>
                  ) : (
                    <span className={tone?.text} title="worst window across this provider's accounts">
                      {Math.round(worst)}%
                    </span>
                  )}
                </td>
              </tr>

              {providerOpen
                ? accounts.map((account) => {
                    const key = `${node.provider}:${account.connectionId ?? "unattributed"}`;
                    const accountOpen = expandedAccounts.has(key);
                    const info = account.connectionId
                      ? accountInfo.get(account.connectionId)
                      : undefined;
                    const quota = account.connectionId
                      ? quotaByConnection.get(account.connectionId)
                      : undefined;
                    const subscription = account.connectionId
                      ? subscriptionByConnection.get(account.connectionId)
                      : undefined;
                    const name =
                      info?.name ??
                      quota?.name ??
                      subscription?.name ??
                      (account.connectionId ? "Unnamed account" : "Unattributed");
                    return (
                      <Fragment key={key}>
                        <tr className="border-b border-line hover:bg-[var(--color-row-hover)]">
                          <th scope="row" className="py-2 pl-8 pr-3 text-left font-normal">
                            <span className="flex w-full min-w-0 items-center gap-2 text-body">
                              {account.models.length > 0 ? (
                                <button
                                  type="button"
                                  aria-expanded={accountOpen}
                                  onClick={() => onToggleAccount(key)}
                                  className="flex min-w-0 items-center gap-2 rounded text-left focus-visible:outline focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-accent-text"
                                >
                                  <CaretRight
                                    size={10}
                                    aria-hidden
                                    className={cx(
                                      "shrink-0 text-faint transition-transform",
                                      accountOpen && "rotate-90",
                                    )}
                                  />
                                  <span
                                    className="truncate text-muted"
                                    title={info?.email ?? account.connectionId ?? undefined}
                                  >
                                    {name}
                                  </span>
                                </button>
                              ) : (
                                <>
                                  <span aria-hidden className="w-[10px] shrink-0" />
                                  <span
                                    className="truncate text-muted"
                                    title={info?.email ?? account.connectionId ?? undefined}
                                  >
                                    {name}
                                  </span>
                                </>
                              )}
                              {info?.isActive === false ? <Badge>inactive</Badge> : null}
                            </span>
                          </th>
                          <AccountNums account={account} />
                          <td className="px-3 py-2 pl-6">
                            <QuotaCell quota={quota} subscription={subscription} />
                          </td>
                        </tr>

                        {accountOpen ? (
                          <ModelRows
                            account={account}
                            accountKey={key}
                            showAll={allModelsShown.has(key)}
                            onToggle={() =>
                              setAllModelsShown((prev) => {
                                const next = new Set(prev);
                                if (next.has(key)) next.delete(key);
                                else next.add(key);
                                return next;
                              })
                            }
                          />
                        ) : null}
                      </Fragment>
                    );
                  })
                : null}
            </tbody>
          );
        })}
      </table>
    </div>
  );
}

/** One account's models, paged at MODEL_PAGE with an explicit show-all control. */
function ModelRows({
  account,
  accountKey,
  showAll,
  onToggle,
}: {
  account: AccountNode;
  accountKey: string;
  showAll: boolean;
  onToggle: () => void;
}) {
  const total = account.models.length;
  const shown = showAll ? account.models : account.models.slice(0, MODEL_PAGE);
  return (
    <>
      {shown.map((model) => {
        const { id } = splitRef(model.model);
        return (
          <tr key={`${accountKey}:${model.model}`} className="border-b border-line last:border-0">
            <th scope="row" className="py-2 pl-14 pr-3 text-left font-normal">
              <span
                className="block truncate font-mono text-caption text-muted"
                title={model.model}
              >
                {id || model.model}
              </span>
            </th>
            <Num className="text-muted">{compact(model.totals.requests)}</Num>
            <Num className="text-muted">{compact(model.totals.tokens)}</Num>
            <Num className="text-muted">{usd(model.totals.costMicros)}</Num>
            <Num className={model.totals.errors > 0 ? "text-neg" : "text-faint"}>
              {compact(model.totals.errors)}
            </Num>
            <Num className="text-muted">{ms(latencyOf(model.totals))}</Num>
            <td className="px-3 py-2 pl-6 text-caption text-faint">—</td>
          </tr>
        );
      })}
      {total > MODEL_PAGE ? (
        <tr className="border-b border-line last:border-0">
          <td colSpan={7} className="py-2 pl-14 pr-3">
            <span className="flex flex-wrap items-center gap-3 text-caption text-faint">
              <span className="tnum">
                {showAll ? `all ${total} models` : `showing ${MODEL_PAGE} of ${total} models`}
              </span>
              <Button variant="quiet" size="sm" onClick={onToggle} aria-expanded={showAll}>
                {showAll ? "Show fewer" : `Show all ${total}`}
              </Button>
            </span>
          </td>
        </tr>
      ) : null}
    </>
  );
}

/** A known-but-idle account has no sampled metrics; zeros would look like real traffic. */
function AccountNums({ account }: { account: AccountNode }) {
  if (!account.observed) {
    return (
      <>
        <Num className="text-faint">—</Num>
        <Num className="text-faint">—</Num>
        <Num className="text-faint">—</Num>
        <Num className="text-faint">—</Num>
        <Num className="text-faint">—</Num>
      </>
    );
  }
  return (
    <>
      <Num>{compact(account.totals.requests)}</Num>
      <Num>{compact(account.totals.tokens)}</Num>
      <Num>{usd(account.totals.costMicros)}</Num>
      <Num className={account.totals.errors > 0 ? "text-neg" : "text-faint"}>
        {compact(account.totals.errors)}
      </Num>
      <Num className="text-muted">{ms(latencyOf(account.totals))}</Num>
    </>
  );
}
