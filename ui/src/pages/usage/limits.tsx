/**
 * Multi-provider Limits Board for `/usage`.
 *
 * Replaces the old single-account Headline + LimitsCard hero: every account of every
 * provider is visible at once, sorted by risk (exhausted and earliest projected limit
 * hit first, then failing accounts, then highest used share). Each card shows all plan
 * windows and operator caps right next to the account's range activity (requests, tokens,
 * spend, error rate, last used). Expanding a card lazily fetches daily usage history for
 * the selected window and renders `LimitChart` — zero N-per-card polling on mount.
 */

import { memo, useId, useState } from "react";
import { Link } from "react-router-dom";
import { CaretRight, ChartLineUp } from "@phosphor-icons/react";
import { HoverPreview } from "../../components/hover-preview";
import { Badge, Button, Card, FilterChip, cx, type BadgeTone } from "../../components/ui";
import { ago, compact, pct, splitRef, usd } from "../../lib/format";
import { useAccountUsageHistory } from "../../lib/queries";
import { refusalView } from "../../lib/refusal";
import { barText } from "../providers/types";
import {
  defaultChartWindowId,
  windowPaceStatus,
  type BoardAccount,
  type BoardFilter,
  type BoardStatusKind,
  type BoardSummary,
  type OperatorBar,
} from "./board-model";
import { chartable, LimitChart } from "./limit-chart";
import type { WindowRisk } from "./pace";
import { UsageProviderIcon } from "./provider-badge";
import { capitalized, QuotaMeter, resetsIn, windowLabel } from "./quota";

const STATUS_META: Record<BoardStatusKind, { label: string; tone: BadgeTone }> = {
  exhausted: { label: "Exhausted", tone: "neg" },
  at_risk: { label: "At risk", tone: "warn" },
  failing: { label: "Failing", tone: "neg" },
  degraded: { label: "Degraded", tone: "warn" },
  healthy: { label: "Healthy", tone: "pos" },
  no_limit: { label: "No limit reported", tone: "neutral" },
  unavailable: { label: "Limits unavailable", tone: "warn" },
  loading: { label: "Checking limits…", tone: "neutral" },
  admin_only: { label: "Admin only", tone: "neutral" },
  inactive: { label: "Inactive", tone: "neutral" },
};

const PACE_TONE = {
  neg: "font-medium text-[var(--color-neg)]",
  warn: "font-medium text-[var(--color-warn)]",
  muted: "text-muted",
  faint: "text-faint",
} as const;

/**
 * Top summary strip: counts at-risk, failing, healthy, and no-limit accounts as
 * interactive filter chips alongside total range spend.
 */
export function SummaryStrip({
  summary,
  rangeLabel,
  filter,
  isAdmin,
  onFilterChange,
}: {
  summary: BoardSummary;
  rangeLabel: string;
  filter: BoardFilter;
  isAdmin: boolean;
  onFilterChange: (next: BoardFilter) => void;
}) {
  const toggle = (target: BoardFilter) => {
    onFilterChange(filter === target ? "all" : target);
  };

  const overallErrorRate =
    summary.totalRequests > 0 ? (summary.totalErrors / summary.totalRequests) * 100 : 0;

  return (
    <Card className="mb-4 flex flex-wrap items-center justify-between gap-x-4 gap-y-2.5 px-4 py-3">
      <div className="flex flex-wrap items-center gap-2" role="group" aria-label="Account status filter">
        <FilterChip
          on={filter === "all"}
          onClick={() => onFilterChange("all")}
          ariaLabel={`Show all ${summary.total} accounts`}
        >
          <span>All</span>
          <span className="tnum opacity-80">{summary.total}</span>
        </FilterChip>

        {isAdmin ? (
          <FilterChip
            on={filter === "at_risk"}
            onClick={() => toggle("at_risk")}
            ariaLabel={`Filter to ${summary.atRisk} at-risk accounts`}
          >
            <span
              className={cx(
                filter !== "at_risk" && summary.atRisk > 0 && "font-medium text-[var(--color-warn)]",
              )}
            >
              At risk
            </span>
            <span
              className={cx(
                "tnum",
                filter !== "at_risk" && summary.atRisk > 0 ? "text-[var(--color-warn)]" : "opacity-80",
              )}
            >
              {summary.atRisk}
            </span>
          </FilterChip>
        ) : null}

        <FilterChip
          on={filter === "failing"}
          bad={summary.failing > 0}
          onClick={() => toggle("failing")}
          ariaLabel={`Filter to ${summary.failing} failing accounts`}
        >
          <span
            className={cx(
              filter !== "failing" && summary.failing > 0 && "font-medium text-[var(--color-neg)]",
            )}
          >
            Failing
          </span>
          <span
            className={cx(
              "tnum",
              filter !== "failing" && summary.failing > 0
                ? "text-[var(--color-neg)]"
                : "opacity-90",
            )}
          >
            {summary.failing}
          </span>
        </FilterChip>

        {isAdmin ? (
          <>
            <FilterChip
              on={filter === "healthy"}
              onClick={() => toggle("healthy")}
              ariaLabel={`Filter to ${summary.healthy} healthy accounts`}
            >
              <span>Healthy</span>
              <span className="tnum opacity-80">{summary.healthy}</span>
            </FilterChip>

            <FilterChip
              on={filter === "no_limit"}
              onClick={() => toggle("no_limit")}
              ariaLabel={`Filter to ${summary.noLimit} accounts with no limit reported`}
            >
              <span>No limit reported</span>
              <span className="tnum opacity-80">{summary.noLimit}</span>
            </FilterChip>
          </>
        ) : (
          <span className="text-caption text-faint">Limits are admin-only</span>
        )}
      </div>

      <div className="flex flex-wrap items-baseline gap-x-4 gap-y-1 text-small tnum">
        <span className="text-muted">
          {rangeLabel} spend{" "}
          <span className="font-semibold text-fg">{usd(summary.totalSpendMicros)}</span>
        </span>
        <span className="text-faint">
          {compact(summary.totalRequests)} req
          {summary.totalErrors > 0 ? (
            <>
              {" · "}
              <span className={overallErrorRate >= 50 ? "font-medium text-[var(--color-neg)]" : "text-neg"}>
                {pct(overallErrorRate)} err
              </span>
            </>
          ) : null}
        </span>
      </div>
    </Card>
  );
}

/**
 * Account name link (`/providers?inspect=<id>`) with a non-modal, control-free hover/focus
 * preview showing top models served in the current range and standing connection details.
 */
function AccountPreviewLink({
  item,
  rangeLabel,
}: {
  item: BoardAccount;
  rangeLabel: string;
}) {
  if (!item.connectionId) {
    return <span className="truncate text-small text-muted">{item.accountName}</span>;
  }

  const href = `/providers?inspect=${encodeURIComponent(item.connectionId)}`;
  const topModels = item.models.slice(0, 3);
  const refusal = refusalView(item.refusal);

  const content = (
    <>
      <div className="flex flex-col gap-0.5">
        <div className="flex items-center justify-between gap-2">
          <span className="min-w-0 truncate font-medium text-fg">{item.accountName}</span>
          <Badge tone={STATUS_META[item.status].tone}>{STATUS_META[item.status].label}</Badge>
        </div>
        <span className="truncate text-caption text-faint">
          {item.plan ? `${item.providerLabel} ${capitalized(item.plan)}` : item.providerLabel}
          {item.email ? ` · ${item.email}` : ""}
        </span>
      </div>

      {refusal ? (
        <p className="rounded border border-[var(--color-neg-line)] bg-[var(--color-neg-tint)] px-2 py-1.5 text-caption text-[var(--color-neg-ink)]">
          {refusal.title}: {refusal.description}
        </p>
      ) : item.lastError ? (
        <p className="line-clamp-2 break-words rounded border border-[var(--color-neg-line)] bg-[var(--color-neg-tint)] px-2 py-1.5 font-mono text-caption text-[var(--color-neg-ink)]">
          {item.lastError}
        </p>
      ) : null}

      <div className="flex flex-col gap-1 border-t border-rule pt-2">
        <span className="text-caption font-medium text-muted">
          Top models ({rangeLabel})
        </span>
        {topModels.length > 0 ? (
          <ul role="list" className="flex flex-col gap-1 text-caption tnum">
            {topModels.map((model) => {
              const { id } = splitRef(model.model);
              return (
                <li key={model.model} className="flex items-baseline justify-between gap-2">
                  <span className="min-w-0 truncate font-mono text-ink-soft">
                    {id || model.model}
                  </span>
                  <span className="shrink-0 text-faint">
                    {compact(model.totals.requests)} req · {usd(model.totals.costMicros)}
                  </span>
                </li>
              );
            })}
            {item.models.length > 3 ? (
              <li className="text-faint">+{item.models.length - 3} more in breakdown table</li>
            ) : null}
          </ul>
        ) : (
          <span className="text-caption text-faint">No requests in the last {rangeLabel}.</span>
        )}
      </div>

      <div className="flex items-center justify-between gap-2 border-t border-rule pt-2 text-caption text-faint">
        <span>{item.fetchedAt ? `Limits checked ${ago(item.fetchedAt)}` : "Click to inspect on Providers"}</span>
      </div>
    </>
  );

  return (
    <HoverPreview
      to={href}
      linkClassName="min-w-0 truncate rounded-[var(--radius-control)] text-small text-accent-text underline-offset-2 hover:underline focus-visible:underline focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--color-focus-ring)]"
      contentClassName="gap-2.5"
      content={content}
    >
      {item.accountName}
    </HoverPreview>
  );
}

function PlanWindowRow({ risk, now }: { risk: WindowRisk; now: number }) {
  const { window } = risk;
  const label = windowLabel(window);
  const reset = resetsIn(window.resets_at);
  const status = windowPaceStatus(risk, now);

  return (
    <li className="flex flex-col gap-1">
      <div className="flex items-baseline justify-between gap-2 text-small">
        <span className="min-w-0 truncate">
          <span className="font-medium text-fg">{label}</span>
          {reset ? <span className="text-faint tnum"> · {reset}</span> : null}
        </span>
        <span className={cx("shrink-0 font-medium tnum", barText(window.used_percent))}>
          {Math.round(window.used_percent)}%
        </span>
      </div>
      <QuotaMeter
        percent={window.used_percent}
        label={reset ? `${label}, ${reset}` : label}
      />
      <span className={cx("text-caption tnum", PACE_TONE[status.tone])}>{status.text}</span>
    </li>
  );
}

function OperatorCapRow({ bar }: { bar: OperatorBar }) {
  const reset = resetsIn(bar.resets_at);
  return (
    <li className="flex flex-col gap-1">
      <div className="flex items-baseline justify-between gap-2 text-small">
        <span className="min-w-0 truncate">
          <span className="font-medium text-fg">{bar.label}</span>
          {reset ? <span className="text-faint tnum"> · {reset}</span> : null}
        </span>
        <span className={cx("shrink-0 font-medium tnum", barText(bar.percent))}>
          {bar.detail}
        </span>
      </div>
      <QuotaMeter percent={bar.percent} label={reset ? `${bar.label}, ${reset}` : bar.label} />
      <span
        className={cx(
          "text-caption tnum",
          bar.exhausted
            ? "font-medium text-[var(--color-neg)]"
            : bar.percent >= 90
              ? "font-medium text-[var(--color-warn)]"
              : "text-muted",
        )}
      >
        {bar.exhausted
          ? "Exhausted — operator cap reached"
          : `Operator cap · ${Math.round(bar.percent)}% used`}
      </span>
    </li>
  );
}

/**
 * Lazily mounted forecast chart panel: only fetches `/api/accounts/usage/{id}/history`
 * when the user opens this card's chart disclosure, and only for the selected window.
 */
function AccountForecastPanel({
  connectionId,
  windows,
  now,
}: {
  connectionId: string;
  windows: WindowRisk[];
  now: number;
}) {
  const [selectedId, setSelectedId] = useState<string | null>(() => defaultChartWindowId(windows));

  // Keep selected window valid if the provider's window set changes across polls.
  const activeRisk =
    windows.find((w) => w.window.id === selectedId) ??
    windows.find((w) => w.window.id === defaultChartWindowId(windows)) ??
    windows[0];

  const canChart = activeRisk !== undefined && chartable(activeRisk.window);
  const history = useAccountUsageHistory(
    canChart ? connectionId : null,
    canChart ? activeRisk.window.id : null,
  );

  if (!activeRisk) return null;

  const label = windowLabel(activeRisk.window);

  return (
    <div className="mt-3 flex flex-col gap-3 border-t border-rule pt-3">
      {windows.length > 1 ? (
        <div
          className="flex flex-wrap items-center gap-1.5"
          role="group"
          aria-label="Chart window"
        >
          {windows.map((risk) => {
            const active = risk.window.id === activeRisk.window.id;
            return (
              <button
                key={risk.window.id}
                type="button"
                aria-pressed={active}
                onClick={() => setSelectedId(risk.window.id)}
                className={cx(
                  "inline-flex min-h-[28px] items-center gap-1 rounded-full border px-2.5 py-0.5 text-caption transition-colors",
                  "focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--color-focus-ring)]",
                  active
                    ? "border-accent bg-accent font-medium text-[var(--color-accent-ink)]"
                    : "border-line bg-surface-2 text-muted hover:border-[var(--color-line-strong)] hover:text-fg",
                )}
              >
                <span>{windowLabel(risk.window)}</span>
                <span className="tnum opacity-80">{Math.round(risk.window.used_percent)}%</span>
              </button>
            );
          })}
        </div>
      ) : null}

      {!canChart ? (
        <p className="text-small text-faint">
          Daily forecast charts apply to windows of 2 days or longer ({label} is a short rolling
          window).
        </p>
      ) : history.isLoading ? (
        <p className="text-small text-faint">Loading usage history…</p>
      ) : history.isError ? (
        <p className="text-small text-faint">Usage history is unavailable right now.</p>
      ) : history.data ? (
        <LimitChart
          window={activeRisk.window}
          label={label}
          pace={activeRisk.pace}
          history={history.data}
          now={now}
        />
      ) : null}
    </div>
  );
}

function NoLimitsReason({
  item,
  isAdmin,
  quotaLoaded,
  quotaError,
}: {
  item: BoardAccount;
  isAdmin: boolean;
  quotaLoaded: boolean;
  quotaError: boolean;
}) {
  if (!isAdmin) {
    return (
      <p className="rounded-[var(--radius-control)] border border-rule bg-surface-2 px-3 py-2 text-caption text-muted">
        Plan limits and operator caps are visible to admins only.
      </p>
    );
  }
  if (item.isUnrouted) {
    return (
      <p className="rounded-[var(--radius-control)] border border-[var(--color-neg-line)] bg-[var(--color-neg-tint)] px-3 py-2 text-caption text-[var(--color-neg-ink)]">
        Requests failed before routing to any provider account (e.g. unknown model, auth, or no
        healthy candidate).
      </p>
    );
  }
  if (item.limitError) {
    return (
      <p className="rounded-[var(--radius-control)] border border-[var(--color-warn-line)] bg-[var(--color-warn-tint)] px-3 py-2 text-caption text-[var(--color-warn-ink)]">
        Limits unavailable: {item.limitError}
      </p>
    );
  }
  if (quotaError) {
    return (
      <p className="rounded-[var(--radius-control)] border border-[var(--color-warn-line)] bg-[var(--color-warn-tint)] px-3 py-2 text-caption text-[var(--color-warn-ink)]">
        Could not load plan limits right now.
      </p>
    );
  }
  if (!quotaLoaded) {
    return <p className="text-caption text-faint">Loading limits…</p>;
  }
  return (
    <p className="rounded-[var(--radius-control)] border border-rule bg-surface-2 px-3 py-2 text-caption text-muted">
      Not reported by {item.providerLabel}.
      {item.connectionId ? (
        <>
          {" "}
          <Link
            to={`/providers?inspect=${encodeURIComponent(item.connectionId)}`}
            className="text-accent-text underline-offset-2 hover:underline focus-visible:underline"
          >
            Set an operator cap
          </Link>
        </>
      ) : null}
    </p>
  );
}

const AccountLimitCard = memo(function AccountLimitCard({
  item,
  rangeLabel,
  isAdmin,
  quotaLoaded,
  quotaError,
  chartOpen,
  onToggleChart,
  now,
}: {
  item: BoardAccount;
  rangeLabel: string;
  isAdmin: boolean;
  quotaLoaded: boolean;
  quotaError: boolean;
  chartOpen: boolean;
  onToggleChart: (key: string) => void;
  now: number;
}) {
  const headingId = useId();
  const badge = STATUS_META[item.status];
  const canOpenChart = isAdmin && item.connectionId !== null && item.windows.length > 0;
  const refusal = refusalView(item.refusal);

  const cardBorder =
    item.status === "exhausted" || item.status === "failing"
      ? "border-[var(--color-neg-line)]"
      : item.status === "at_risk" || item.status === "degraded"
        ? "border-[var(--color-warn-line)]"
        : "border-line";

  const lastUsedText =
    item.lastUsedAt !== null
      ? `last used ${ago(item.lastUsedAt)}`
      : item.totals.requests > 0
        ? `used earlier in ${rangeLabel}`
        : `idle in ${rangeLabel}`;

  const titleText = item.plan
    ? `${item.providerLabel} ${capitalized(item.plan)}`
    : item.providerLabel;

  return (
    <article
      aria-labelledby={headingId}
      className={cx(
        "flex flex-col justify-between gap-3.5 rounded-[var(--radius-surface)] border bg-surface px-4 py-3.5",
        cardBorder,
        !item.isActive && "opacity-75",
      )}
    >
      <div className="flex flex-col gap-3">
        <header className="flex items-start justify-between gap-3">
          <div className="flex min-w-0 items-start gap-2.5">
            <span className="mt-0.5 shrink-0">
              <UsageProviderIcon provider={item.provider} size={18} />
            </span>
            <div className="flex min-w-0 flex-col">
              <div className="flex flex-wrap items-baseline gap-x-1.5 gap-y-0.5">
                <h3 id={headingId} className="text-body font-semibold text-fg">
                  {titleText}
                </h3>
                <span className="text-faint" aria-hidden>
                  ·
                </span>
                <AccountPreviewLink item={item} rangeLabel={rangeLabel} />
              </div>
              {item.email ? (
                <span className="truncate text-caption text-faint">{item.email}</span>
              ) : null}
            </div>
          </div>

          <div className="flex shrink-0 items-center gap-1.5">
            {item.isFailing && item.status !== "failing" ? (
              <Badge tone="neg">Failing</Badge>
            ) : null}
            <Badge tone={badge.tone}>{badge.label}</Badge>
          </div>
        </header>

        {isAdmin && item.hasLimits ? (
          <ul role="list" className="flex flex-col gap-3">
            {item.operatorBars.map((bar) => (
              <OperatorCapRow key={bar.id} bar={bar} />
            ))}
            {item.windows.map((risk) => (
              <PlanWindowRow key={risk.window.id} risk={risk} now={now} />
            ))}
          </ul>
        ) : (
          <NoLimitsReason
            item={item}
            isAdmin={isAdmin}
            quotaLoaded={quotaLoaded}
            quotaError={quotaError}
          />
        )}

        {refusal ? (
          <p className="rounded-[var(--radius-control)] border border-[var(--color-neg-line)] bg-[var(--color-neg-tint)] px-2.5 py-1.5 text-caption text-[var(--color-neg-ink)]">
            {refusal.title} — {refusal.description}
          </p>
        ) : item.isFailing && item.lastError ? (
          <p className="line-clamp-2 break-words rounded-[var(--radius-control)] border border-[var(--color-neg-line)] bg-[var(--color-neg-tint)] px-2.5 py-1.5 font-mono text-caption text-[var(--color-neg-ink)]">
            {item.lastError}
          </p>
        ) : null}
      </div>

      <footer className="flex flex-col border-t border-rule pt-2.5">
        <div className="flex flex-wrap items-center justify-between gap-x-3 gap-y-1 text-caption tnum">
          <div className="flex flex-wrap items-center gap-x-2.5 gap-y-0.5 text-muted">
            <span className="font-medium text-ink-soft">{rangeLabel}</span>
            <span>{compact(item.totals.requests)} req</span>
            <span>{compact(item.totals.tokens)} tok</span>
            <span className={item.totals.costMicros > 0 ? "font-medium text-fg" : undefined}>
              {usd(item.totals.costMicros)}
            </span>
            <span
              className={cx(
                item.isFailing
                  ? "font-semibold text-[var(--color-neg)]"
                  : item.totals.errors > 0
                    ? "text-neg"
                    : "text-faint",
              )}
            >
              {pct(item.errorRate)} err
              {item.isFailing && item.totals.requests > 0
                ? ` (${compact(item.totals.errors)}/${compact(item.totals.requests)})`
                : ""}
            </span>
          </div>

          <div className="flex flex-wrap items-center gap-x-2 text-faint">
            <span>{lastUsedText}</span>
            {item.fetchedAt !== null ? (
              <>
                <span aria-hidden>·</span>
                <span>checked {ago(item.fetchedAt)}</span>
              </>
            ) : null}
          </div>
        </div>

        {canOpenChart ? (
          <div className="mt-2 flex items-center">
            <button
              type="button"
              aria-expanded={chartOpen}
              onClick={() => onToggleChart(item.key)}
              className="inline-flex min-h-[28px] items-center gap-1.5 rounded-[var(--radius-control)] px-1.5 py-1 text-caption font-medium text-muted transition-colors hover:bg-surface-2 hover:text-fg focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--color-focus-ring)]"
            >
              <CaretRight
                size={12}
                aria-hidden
                className={cx("shrink-0 transition-transform", chartOpen && "rotate-90")}
              />
              <ChartLineUp size={14} aria-hidden className="shrink-0" />
              <span>
                Forecast chart
                <span className="sr-only">
                  {" "}
                  for {titleText} {item.accountName}
                </span>
              </span>
            </button>
          </div>
        ) : null}

        {canOpenChart && chartOpen && item.connectionId ? (
          <AccountForecastPanel
            connectionId={item.connectionId}
            windows={item.windows}
            now={now}
          />
        ) : null}
      </footer>
    </article>
  );
});

export function LimitsBoard({
  items,
  rangeLabel,
  isAdmin,
  quotaLoaded,
  quotaError,
  loading,
  activeFilter,
  onClearFilter,
  expandedCharts,
  onToggleChart,
  now,
}: {
  items: BoardAccount[];
  rangeLabel: string;
  isAdmin: boolean;
  quotaLoaded: boolean;
  quotaError: boolean;
  loading: boolean;
  activeFilter: BoardFilter;
  onClearFilter: () => void;
  expandedCharts: Set<string>;
  onToggleChart: (key: string) => void;
  now?: number;
}) {
  const at = now ?? Date.now();

  if (items.length === 0) {
    return (
      <Card className="mb-4 px-5 py-8 text-center text-body text-faint">
        {loading ? (
          "Loading accounts and limits…"
        ) : activeFilter !== "all" ? (
          <div className="flex flex-col items-center gap-2.5">
            <span>No accounts match this status filter.</span>
            <Button variant="quiet" size="sm" onClick={onClearFilter}>
              Show all accounts
            </Button>
          </div>
        ) : (
          <span>
            No accounts or traffic recorded yet.{" "}
            <Link to="/providers" className="text-accent-text underline-offset-2 hover:underline">
              Connect a provider
            </Link>
          </span>
        )}
      </Card>
    );
  }

  return (
    <section className="mb-5" aria-labelledby="usage-limits-board-heading">
      <div className="mb-2.5 flex flex-wrap items-baseline justify-between gap-2">
        <h2 id="usage-limits-board-heading" className="text-small font-semibold text-fg">
          {isAdmin ? "Provider limits & accounts" : "Accounts & range activity"}
        </h2>
        <span className="text-caption text-faint">
          Sorted by risk · limits are current-window; activity covers the last {rangeLabel}
        </span>
      </div>

      <div className="grid grid-cols-1 items-start gap-3.5 lg:grid-cols-2">
        {items.map((item) => (
          <AccountLimitCard
            key={item.key}
            item={item}
            rangeLabel={rangeLabel}
            isAdmin={isAdmin}
            quotaLoaded={quotaLoaded}
            quotaError={quotaError}
            chartOpen={expandedCharts.has(item.key)}
            onToggleChart={onToggleChart}
            now={at}
          />
        ))}
      </div>
    </section>
  );
}
