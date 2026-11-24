/**
 * Detailed drill-down table for `/usage`: totals row, provider rows, then accounts and
 * models inside each provider. Requests, tokens, spend, and errors cover the selected
 * range; the Quota column shows each account's current window using the shared QuotaCell.
 */

import { Fragment, memo, useState, type ReactNode } from "react";
import { Link } from "react-router-dom";
import { CaretRight } from "@phosphor-icons/react";
import { Badge, Button, barText, cx } from "../../components/ui";
import { ago, compact, pct, splitRef, usdDisplay } from "../../lib/format";
import type { AccountQuotaRow, AccountSubscription } from "../../lib/api";
import { errorRateOf, type AccountNode, type ProviderNode, type Totals } from "./aggregate";
import { resolveProviderDisplay } from "./board-model";
import { UsageProviderIcon } from "./provider-badge";
import { QuotaCell, worstQuotaBar, type QuotaBar } from "./quota";

export type AccountInfo = { name: string | null; email: string | null; isActive: boolean | null };

/** Child models shown at a time, so a 200-model account cannot bury the next account. */
const MODEL_PAGE = 25;

const HEAD_LEFT = "px-3 py-2 text-left text-caption font-medium text-faint";
const HEAD_RIGHT = "px-3 py-2 text-right text-caption font-medium text-faint";

function Num({ children, className }: { children: ReactNode; className?: string }) {
  return (
    <td className={cx("px-3 py-2.5 text-right text-small tnum", className)}>{children}</td>
  );
}

/** Errors as a share of requests: one unit in every row, so rows compare directly. */
function ErrorRate({ totals, className }: { totals: Totals; className?: string }) {
  return (
    <Num className={cx(totals.errors > 0 ? "text-neg" : "text-faint", className)}>
      {pct(errorRateOf(totals))}
    </Num>
  );
}

export const UsageTable = memo(function UsageTable({
  providers,
  totals,
  accountsByProvider,
  accountInfo,
  quotaByConnection,
  subscriptionByConnection,
  providerDisplayMap,
  showQuota,
  quotaLoaded,
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
  /** Optional catalog display names (`provider id -> display`). */
  providerDisplayMap?: ReadonlyMap<string, string>;
  /** False for callers who cannot read quota at all: the column is not rendered. */
  showQuota: boolean;
  /**
   * True once both quota sources answered. Until then an account without a bar shows
   * "—", never "Not reported", because the absence is ours, not the account's.
   */
  quotaLoaded: boolean;
  loading: boolean;
  emptyText: string;
  expandedProviders: Set<string>;
  expandedAccounts: Set<string>;
  onToggleProvider: (provider: string) => void;
  onToggleAccount: (key: string) => void;
}) {
  const [allModelsShown, setAllModelsShown] = useState<Set<string>>(() => new Set());
  const columns = showQuota ? 6 : 5;

  // Worst reported window per provider, active accounts only: a disabled account at
  // 97% says nothing about whether this provider can still serve.
  const worstByProvider = new Map<string, QuotaBar>();
  const note = (provider: string, connectionId: string, worst: QuotaBar | null) => {
    if (worst === null || accountInfo.get(connectionId)?.isActive === false) return;
    const prior = worstByProvider.get(provider);
    if (!prior || worst.percent > prior.percent) worstByProvider.set(provider, worst);
  };
  if (showQuota) {
    for (const quota of quotaByConnection.values()) {
      note(
        quota.provider,
        quota.connection_id,
        worstQuotaBar(quota, subscriptionByConnection.get(quota.connection_id)),
      );
    }
    for (const subscription of subscriptionByConnection.values()) {
      note(
        subscription.provider,
        subscription.connection_id,
        worstQuotaBar(undefined, subscription),
      );
    }
  }

  return (
    <div className="overflow-x-auto">
      <table
        className={cx(
          "w-full border-collapse text-small",
          showQuota ? "min-w-[820px]" : "min-w-[600px]",
        )}
      >
        <caption className="sr-only">
          Consumption by provider, account and model
          {showQuota ? ", with each account's current quota window" : ""}
        </caption>
        <thead>
          <tr className="border-b border-line">
            <th scope="col" className={cx(HEAD_LEFT, "px-4")}>
              Provider / account / model
            </th>
            <th scope="col" className={cx(HEAD_RIGHT, "w-[88px]")}>
              Requests
            </th>
            <th scope="col" className={cx(HEAD_RIGHT, "w-[96px]")}>
              Tokens
            </th>
            <th scope="col" className={cx(HEAD_RIGHT, "w-[104px]")}>
              Spend
            </th>
            <th scope="col" className={cx(HEAD_RIGHT, "w-[78px]")}>
              Errors
            </th>
            {showQuota ? (
              <th scope="col" className={cx(HEAD_LEFT, "w-[232px] pl-6")}>
                Quota (now)
              </th>
            ) : null}
          </tr>
        </thead>

        <tbody>
          <tr className="border-b border-line">
            <th scope="row" className="px-4 py-3 text-left text-body font-semibold text-fg">
              All providers
            </th>
            <Num className="font-semibold">{compact(totals.requests)}</Num>
            <Num className="font-semibold">{compact(totals.tokens)}</Num>
            <Num className="font-semibold">{usdDisplay(totals.costMicros)}</Num>
            <ErrorRate totals={totals} className="font-semibold" />
            {showQuota ? <td /> : null}
          </tr>
        </tbody>

        {providers.length === 0 ? (
          <tbody>
            <tr>
              <td colSpan={columns} className="px-4 py-10 text-center text-body text-faint">
                {loading ? "loading…" : emptyText}
              </td>
            </tr>
          </tbody>
        ) : null}

        {providers.map((node) => {
          const providerOpen = expandedProviders.has(node.provider);
          const accounts = accountsByProvider.get(node.provider) ?? [];
          const worst = worstByProvider.get(node.provider) ?? null;
          const resolved = resolveProviderDisplay(node.provider, providerDisplayMap);
          const providerFailing = node.totals.requests > 0 && errorRateOf(node.totals) >= 50;

          return (
            <tbody key={node.provider || "__unrouted__"}>
              <tr className="border-b border-line hover:bg-[var(--color-row-hover)]">
                <th scope="row" className="px-4 py-2.5 text-left text-body font-medium text-fg">
                  <button
                    type="button"
                    aria-expanded={providerOpen}
                    onClick={() => onToggleProvider(node.provider)}
                    className="flex w-full min-w-0 items-center gap-2 rounded text-left focus-visible:outline focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-[var(--color-focus-ring)]"
                  >
                    <CaretRight
                      size={11}
                      aria-hidden
                      className={cx(
                        "shrink-0 text-faint transition-transform",
                        providerOpen && "rotate-90",
                      )}
                    />
                    <UsageProviderIcon provider={node.provider} size={14} />
                    <span className="truncate">{resolved.label}</span>
                    {providerFailing ? <Badge tone="neg">failing</Badge> : null}
                    <span className="shrink-0 text-caption text-faint tnum">
                      {node.models.length} {node.models.length === 1 ? "model" : "models"}
                    </span>
                  </button>
                </th>
                <Num>{compact(node.totals.requests)}</Num>
                <Num>{compact(node.totals.tokens)}</Num>
                <Num>{usdDisplay(node.totals.costMicros)}</Num>
                <ErrorRate totals={node.totals} />
                {showQuota ? (
                  <td className="px-3 py-2.5 pl-6 text-caption tnum">
                    {worst === null ? (
                      <span className="text-faint">—</span>
                    ) : (
                      <span className={barText(worst.percent)}>
                        {Math.round(worst.percent)}%
                        <span className="text-faint"> {worst.label}</span>
                      </span>
                    )}
                  </td>
                ) : null}
              </tr>

              {providerOpen
                ? accounts.map((account) => {
                    const key = `${node.provider}:${account.connectionId ?? "unattributed"}`;
                    return (
                      <Fragment key={key}>
                        <AccountRow
                          account={account}
                          isUnrouted={resolved.isUnrouted}
                          info={
                            account.connectionId
                              ? accountInfo.get(account.connectionId)
                              : undefined
                          }
                          quota={
                            account.connectionId
                              ? quotaByConnection.get(account.connectionId)
                              : undefined
                          }
                          subscription={
                            account.connectionId
                              ? subscriptionByConnection.get(account.connectionId)
                              : undefined
                          }
                          showQuota={showQuota}
                          quotaLoaded={quotaLoaded}
                          open={expandedAccounts.has(key)}
                          onToggle={() => onToggleAccount(key)}
                        />

                        {expandedAccounts.has(key) ? (
                          <ModelRows
                            account={account}
                            accountKey={key}
                            columns={columns}
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
});

function AccountRow({
  account,
  isUnrouted,
  info,
  quota,
  subscription,
  showQuota,
  quotaLoaded,
  open,
  onToggle,
}: {
  account: AccountNode;
  isUnrouted: boolean;
  info: AccountInfo | undefined;
  quota: AccountQuotaRow | undefined;
  subscription: AccountSubscription | undefined;
  showQuota: boolean;
  quotaLoaded: boolean;
  open: boolean;
  onToggle: () => void;
}) {
  const id = account.connectionId;
  const name =
    info?.name ??
    quota?.name ??
    subscription?.name ??
    (id ? "Unnamed account" : isUnrouted ? "Failed before dispatch" : "Unattributed");
  const planFetchedAt =
    subscription && subscription.windows.length > 0 && subscription.fetched_at > 0
      ? subscription.fetched_at
      : null;

  return (
    <tr className="border-b border-line hover:bg-[var(--color-row-hover)]">
      <th scope="row" className="py-2 pl-8 pr-3 text-left align-top font-normal">
        <span className="flex w-full min-w-0 items-start gap-2 text-body">
          {account.models.length > 0 ? (
            <button
              type="button"
              aria-expanded={open}
              aria-label={`Models for ${name}`}
              onClick={onToggle}
              className="mt-[5px] flex shrink-0 items-center rounded focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--color-focus-ring)]"
            >
              <CaretRight
                size={10}
                aria-hidden
                className={cx("text-faint transition-transform", open && "rotate-90")}
              />
            </button>
          ) : (
            <span aria-hidden className="w-[10px] shrink-0" />
          )}
          <span className="flex min-w-0 flex-col">
            <span className="flex min-w-0 items-center gap-2">
              {id ? (
                <Link
                  to={`/providers?inspect=${encodeURIComponent(id)}`}
                  className="truncate rounded text-muted underline-offset-2 hover:text-fg hover:underline focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--color-focus-ring)]"
                >
                  {name}
                </Link>
              ) : (
                <span className="truncate text-muted">{name}</span>
              )}
              {info?.isActive === false ? <Badge>inactive</Badge> : null}
            </span>
            {info?.email ? (
              <span className="truncate text-caption text-faint">{info.email}</span>
            ) : null}
          </span>
        </span>
      </th>
      <AccountNums account={account} />
      {showQuota ? (
        <td className="px-3 py-2 pl-6 align-top">
          {id === null ? (
            <span className="text-caption text-faint">—</span>
          ) : (
            <div className="flex flex-col gap-1">
              <QuotaCell quota={quota} subscription={subscription} loaded={quotaLoaded} />
              {planFetchedAt !== null ? (
                <span className="text-caption text-faint tnum">
                  updated {ago(planFetchedAt)}
                </span>
              ) : null}
            </div>
          )}
        </td>
      ) : null}
    </tr>
  );
}

/** One account's models, paged at MODEL_PAGE with an explicit show-all control. */
function ModelRows({
  account,
  accountKey,
  columns,
  showAll,
  onToggle,
}: {
  account: AccountNode;
  accountKey: string;
  columns: number;
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
              <span className="block truncate font-mono text-caption text-muted">
                {id || model.model}
              </span>
            </th>
            <Num className="text-muted">{compact(model.totals.requests)}</Num>
            <Num className="text-muted">{compact(model.totals.tokens)}</Num>
            <Num className="text-muted">{usdDisplay(model.totals.costMicros)}</Num>
            <ErrorRate totals={model.totals} />
            {columns > 5 ? <td /> : null}
          </tr>
        );
      })}
      {total > MODEL_PAGE ? (
        <tr className="border-b border-line last:border-0">
          <td colSpan={columns} className="py-2 pl-14 pr-3">
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

/** A known-but-idle account has no requests in range; zeros would look like real traffic. */
function AccountNums({ account }: { account: AccountNode }) {
  if (!account.observed) {
    return (
      <>
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
      <Num>{usdDisplay(account.totals.costMicros)}</Num>
      <ErrorRate totals={account.totals} />
    </>
  );
}
