/**
 * Multi-provider Limits Board for `/usage`.
 *
 * Every account of every provider is visible at once, sorted by risk and split into
 * sections (needs attention → within limits → no limits reported → inactive) so a grid row
 * only ever holds cards of the same shape. Each card has exactly one status, one compact
 * line per quota window (label · meter · % · time left), at most one plain failure
 * sentence, and a fixed four-column stats footer that lines up across cards.
 *
 * Requests that never reached a provider are not an account and live in `unrouted.tsx`.
 * Expanding a card lazily fetches daily usage history for the selected window and renders
 * `LimitChart` — zero N-per-card polling on mount.
 */

import { memo, useId, useState } from "react";
import { Link } from "react-router-dom";
import { ChartLineUp, WarningCircle } from "@phosphor-icons/react";
import { HoverPreview } from "../../components/hover-preview";
import { Badge, Button, Card, Dot, FilterChip, cx, type BadgeTone, type DotTone } from "../../components/ui";
import { ago, compact, pct, splitRef, usd } from "../../lib/format";
import { useAccountUsageHistory } from "../../lib/queries";
import { refusalView } from "../../lib/refusal";
import { barFill, barText, until } from "../providers/types";
import {
  defaultChartWindowId,
  failureSummary,
  groupBoardAccounts,
  projectionTick,
  windowPaceStatus,
  type BoardAccount,
  type BoardFilter,
  type BoardGroup,
  type BoardStatusKind,
  type BoardSummary,
  type OperatorBar,
} from "./board-model";
import { chartable, LimitChart } from "./limit-chart";
import type { WindowRisk } from "./pace";
import { UsageProviderIcon } from "./provider-badge";
import { capitalized, resetsIn, windowLabel } from "./quota";

const STATUS_META: Record<BoardStatusKind, { label: string; tone: BadgeTone; dot: DotTone }> = {
  exhausted: { label: "Exhausted", tone: "neg", dot: "neg" },
  at_risk: { label: "At risk", tone: "warn", dot: "warn" },
  failing: { label: "Failing", tone: "neg", dot: "neg" },
  degraded: { label: "Degraded", tone: "warn", dot: "warn" },
  healthy: { label: "Healthy", tone: "pos", dot: "pos" },
  no_limit: { label: "Not reported", tone: "neutral", dot: "muted" },
  unavailable: { label: "Limits unavailable", tone: "warn", dot: "warn" },
  loading: { label: "Checking…", tone: "neutral", dot: "muted" },
  admin_only: { label: "Active", tone: "neutral", dot: "muted" },
  inactive: { label: "Inactive", tone: "neutral", dot: "muted" },
};

const PACE_TONE = {
  neg: "font-medium text-[var(--color-neg)]",
  warn: "font-medium text-[var(--color-warn)]",
  muted: "text-muted",
  faint: "text-faint",
} as const;

/** label · meter · % · time left. Shared by plan windows and operator caps so they align. */
const WINDOW_ROW =
  "grid grid-cols-[minmax(0,6.25rem)_minmax(0,1fr)_2.5rem_minmax(0,5rem)] items-center gap-x-2.5";
/** Indent that lines a note up with the meter column of `WINDOW_ROW`. */
const WINDOW_NOTE_INDENT = "pl-[calc(6.25rem+0.625rem)]";

/** Requests · tokens · spend · errors · chart toggle: identical columns on every card. */
const STATS_ROW =
  "grid grid-cols-[repeat(4,minmax(0,1fr))_1.75rem] items-center gap-x-2 text-caption tnum";

/**
 * Top summary strip: counts at-risk, failing, healthy, and no-limit accounts as
 * interactive filter chips alongside total range spend, plus a quiet pointer to the
 * requests that never reached a provider.
 */
export function SummaryStrip({
  summary,
  rangeLabel,
  filter,
  isAdmin,
  onFilterChange,
  unroutedRequests = 0,
}: {
  summary: BoardSummary;
  rangeLabel: string;
  filter: BoardFilter;
  isAdmin: boolean;
  onFilterChange: (next: BoardFilter) => void;
  /** Range requests rejected before dispatch; links to their section when > 0. */
  unroutedRequests?: number;
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
            {summary.atRisk > 0 && filter !== "at_risk" ? <Dot tone="warn" /> : null}
            <span>At risk</span>
            <span className="tnum opacity-80">{summary.atRisk}</span>
          </FilterChip>
        ) : null}

        <FilterChip
          on={filter === "failing"}
          onClick={() => toggle("failing")}
          ariaLabel={`Filter to ${summary.failing} failing accounts`}
        >
          {summary.failing > 0 && filter !== "failing" ? <Dot tone="neg" /> : null}
          <span>Failing</span>
          <span className="tnum opacity-80">{summary.failing}</span>
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
              <span>Not reported</span>
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
          {summary.totalErrors > 0 ? ` · ${pct(overallErrorRate)} err` : null}
        </span>
        {unroutedRequests > 0 ? (
          <a
            href="#usage-unrouted"
            className="text-caption text-muted underline-offset-2 hover:text-fg hover:underline focus-visible:underline"
          >
            {compact(unroutedRequests)} never reached a provider
          </a>
        ) : null}
      </div>
    </Card>
  );
}

/**
 * Account name link (`/providers?inspect=<id>`) with a non-modal, control-free hover/focus
 * preview showing top models served in the current range and standing connection details.
 */
export function AccountPreviewLink({
  item,
  rangeLabel,
}: {
  item: BoardAccount;
  rangeLabel: string;
}) {
  if (!item.connectionId) {
    return <span className="min-w-0 truncate text-muted">{item.accountName}</span>;
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
      linkClassName="min-w-0 truncate rounded-[var(--radius-control)] text-accent-text underline-offset-2 hover:underline focus-visible:underline focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--color-focus-ring)]"
      contentClassName="gap-2.5"
      content={content}
    >
      {item.accountName}
    </HoverPreview>
  );
}

/** One status per account: a coloured dot and a word, no pill stack. */
function StatusLabel({ status }: { status: BoardStatusKind }) {
  const meta = STATUS_META[status];
  const loud = meta.dot === "neg" || meta.dot === "warn";
  return (
    <span
      className={cx(
        "inline-flex shrink-0 items-center gap-1.5 text-caption",
        loud ? "font-medium text-ink-soft" : "text-muted",
      )}
    >
      <Dot tone={meta.dot} />
      {meta.label}
    </span>
  );
}

/**
 * 6px meter on the shared colour ladder, with an optional tick where the window is
 * projected to land at reset.
 */
export function LimitMeter({
  percent,
  tick,
  label,
}: {
  percent: number;
  tick: number | null;
  label: string;
}) {
  const width = Math.min(100, Math.max(0, percent));
  return (
    <span
      role="progressbar"
      aria-label={label}
      aria-valuemin={0}
      aria-valuemax={100}
      aria-valuenow={Math.round(percent)}
      className="relative block h-1.5 w-full overflow-hidden rounded-full bg-rule"
    >
      <span
        className={cx("block h-full rounded-full", barFill(percent))}
        style={{ width: `${width}%` }}
      />
      {tick !== null ? (
        <span
          aria-hidden
          className="absolute inset-y-0 w-0.5 rounded-full bg-faint"
          style={{ left: `min(calc(${tick}% - 1px), calc(100% - 2px))` }}
        />
      ) : null}
    </span>
  );
}

function shortWindowLabel(risk: WindowRisk): string {
  return windowLabel(risk.window).replace(/\s+window$/i, "");
}

function timeLeft(resetsAt: number | null): string {
  const left = until(resetsAt);
  return left ? `${left} left` : "";
}

function PlanWindowRow({ risk, now }: { risk: WindowRisk; now: number }) {
  const { window } = risk;
  const label = shortWindowLabel(risk);
  const status = windowPaceStatus(risk, now);
  const reset = resetsIn(window.resets_at);
  const used = Math.round(window.used_percent);
  const left = timeLeft(window.resets_at);

  return (
    <li className="flex flex-col gap-0.5">
      <div className={WINDOW_ROW}>
        <span className="truncate text-small text-ink-soft">{label}</span>
        <LimitMeter
          percent={window.used_percent}
          tick={projectionTick(risk)}
          label={[`${windowLabel(window)}: ${used}% used`, reset, status.text]
            .filter(Boolean)
            .join(", ")}
        />
        <span className={cx("text-right text-small font-medium tnum", barText(window.used_percent))}>
          {used}%
        </span>
        <span className="truncate text-right text-caption text-faint tnum">{left || "—"}</span>
      </div>
      {status.notable ? (
        <span className={cx("text-caption tnum", WINDOW_NOTE_INDENT, PACE_TONE[status.tone])}>
          {status.text}
        </span>
      ) : null}
    </li>
  );
}

function OperatorCapRow({ bar }: { bar: OperatorBar }) {
  const reset = resetsIn(bar.resets_at);
  const used = Math.round(bar.percent);
  const left = timeLeft(bar.resets_at);
  const note = bar.exhausted
    ? { text: `Cap reached · ${bar.detail}`, tone: PACE_TONE.neg }
    : bar.percent >= 90
      ? { text: `Close to cap · ${bar.detail}`, tone: PACE_TONE.warn }
      : null;

  return (
    <li className="flex flex-col gap-0.5">
      <div className={WINDOW_ROW}>
        <span className="truncate text-small text-ink-soft">{bar.label}</span>
        <LimitMeter
          percent={bar.percent}
          tick={null}
          label={[`Operator cap, ${bar.label}: ${bar.detail}`, reset].filter(Boolean).join(", ")}
        />
        <span
          title={bar.detail}
          className={cx("text-right text-small font-medium tnum", barText(bar.percent))}
        >
          {used}%
        </span>
        <span className="truncate text-right text-caption text-faint tnum">{left || "—"}</span>
      </div>
      {note ? (
        <span className={cx("text-caption tnum", WINDOW_NOTE_INDENT, note.tone)}>{note.text}</span>
      ) : null}
    </li>
  );
}

/**
 * Lazily mounted forecast chart panel: only fetches `/api/accounts/usage/{id}/history`
 * when the user opens this card's chart disclosure, and only for the selected window.
 */
export function AccountForecastPanel({
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
    <div className="flex flex-col gap-3 border-t border-rule px-4 py-3">
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

/** What the limits slot says when there are no bars to draw: one quiet line. */
function NoLimitsLine({
  item,
  isAdmin,
  quotaLoaded,
  quotaError,
  hasFailure,
}: {
  item: BoardAccount;
  isAdmin: boolean;
  quotaLoaded: boolean;
  quotaError: boolean;
  hasFailure: boolean;
}) {
  if (!isAdmin) {
    return <p className="text-caption text-faint">Limits are visible to admins only.</p>;
  }
  if (item.limitError || quotaError) {
    return (
      <p className="flex min-w-0 flex-col gap-0.5 text-caption">
        <span className="text-[var(--color-warn)]">Limits unavailable</span>
        <span className="line-clamp-2 break-words text-faint">
          {item.limitError ?? "Could not load plan limits right now."}
        </span>
      </p>
    );
  }
  if (!quotaLoaded) {
    return <p className="text-caption text-faint">Checking limits…</p>;
  }
  // When the card is already explaining a failure, don't clutter it with a "no cap" note.
  if (hasFailure) return null;
  if (!item.isActive) {
    return <p className="text-caption text-faint">Disabled on Providers</p>;
  }
  return (
    <div className="flex items-center justify-between gap-2 text-caption text-faint">
      <span>No operator cap</span>
      {item.connectionId ? (
        <Link
          to={`/providers?inspect=${encodeURIComponent(item.connectionId)}`}
          className="shrink-0 text-accent-text underline-offset-2 hover:underline focus-visible:underline"
        >
          Set cap
        </Link>
      ) : null}
    </div>
  );
}

/** At most one plain sentence about failures, with the newest error text under it. */
function FailureNote({ item }: { item: BoardAccount }) {
  const refusal = refusalView(item.refusal);
  const summary = failureSummary(item);
  if (!refusal && !summary) return null;
  const severe = Boolean(refusal) || item.status === "failing" || item.isFailing;
  const detail = refusal ? refusal.description : item.lastError;

  return (
    <div className="flex min-w-0 items-start gap-2 text-caption">
      <WarningCircle
        size={14}
        weight="fill"
        aria-hidden
        className={cx(
          "mt-px shrink-0",
          severe ? "text-[var(--color-neg)]" : "text-[var(--color-warn)]",
        )}
      />
      <div className="flex min-w-0 flex-col gap-0.5">
        <span className="text-ink-soft">{refusal ? refusal.title : summary}</span>
        {detail ? (
          <span
            className={cx("line-clamp-2 break-words text-faint", !refusal && "font-mono")}
          >
            {detail}
          </span>
        ) : null}
      </div>
    </div>
  );
}

function StatCell({
  value,
  unit,
  className,
}: {
  value: string;
  unit?: string;
  className?: string;
}) {
  return (
    <span className={cx("min-w-0 truncate", className)}>
      {value}
      {unit ? <span className="text-faint"> {unit}</span> : null}
    </span>
  );
}

const AccountLimitCard = memo(function AccountLimitCard({
  item,
  rangeLabel,
  isAdmin,
  quotaLoaded,
  quotaError,
  chartOpen,
  stretch,
  onToggleChart,
  now,
}: {
  item: BoardAccount;
  rangeLabel: string;
  isAdmin: boolean;
  quotaLoaded: boolean;
  quotaError: boolean;
  chartOpen: boolean;
  stretch: boolean;
  onToggleChart: (key: string) => void;
  now: number;
}) {
  const headingId = useId();
  const canOpenChart = isAdmin && item.connectionId !== null && item.windows.length > 0;
  const hasFailure = Boolean(refusalView(item.refusal) || failureSummary(item));

  const lastUsedText =
    item.lastUsedAt !== null
      ? `used ${ago(item.lastUsedAt)}`
      : item.totals.requests > 0
        ? `used earlier in ${rangeLabel}`
        : `idle in ${rangeLabel}`;

  const titleText = item.plan
    ? `${item.providerLabel} ${capitalized(item.plan)}`
    : item.providerLabel;

  const errorTone =
    item.totals.errors === 0
      ? "text-faint"
      : item.isFailing
        ? "font-medium text-[var(--color-neg)]"
        : item.status === "degraded"
          ? "text-[var(--color-warn)]"
          : "text-muted";

  return (
    <article
      aria-labelledby={headingId}
      className={cx(
        "flex flex-col rounded-[var(--radius-surface)] border border-line bg-surface",
        stretch && "h-full",
        !item.isActive && "opacity-75",
      )}
    >
      <div className="flex flex-1 flex-col gap-3 px-4 pb-3 pt-3.5">
        <header className="flex items-start justify-between gap-3">
          <div className="flex min-w-0 items-start gap-2.5">
            <span className="mt-0.5 shrink-0">
              <UsageProviderIcon provider={item.provider} size={18} />
            </span>
            <div className="flex min-w-0 flex-col">
              <h3 id={headingId} className="truncate text-body font-semibold text-fg">
                {titleText}
              </h3>
              <div className="flex min-w-0 items-baseline gap-1.5 text-caption">
                <AccountPreviewLink item={item} rangeLabel={rangeLabel} />
                <span className="shrink-0 text-faint">· {lastUsedText}</span>
              </div>
            </div>
          </div>
          <StatusLabel status={item.status} />
        </header>

        {isAdmin && item.hasLimits ? (
          <ul role="list" className="flex flex-col gap-2">
            {item.operatorBars.map((bar) => (
              <OperatorCapRow key={bar.id} bar={bar} />
            ))}
            {item.windows.map((risk) => (
              <PlanWindowRow key={risk.window.id} risk={risk} now={now} />
            ))}
          </ul>
        ) : (
          <NoLimitsLine
            item={item}
            isAdmin={isAdmin}
            quotaLoaded={quotaLoaded}
            quotaError={quotaError}
            hasFailure={hasFailure}
          />
        )}

        <FailureNote item={item} />
      </div>

      <footer className={cx(STATS_ROW, "min-h-11 border-t border-rule px-4 py-2 text-muted")}>
        <StatCell value={compact(item.totals.requests)} unit="req" />
        <StatCell value={compact(item.totals.tokens)} unit="tok" />
        <StatCell
          value={usd(item.totals.costMicros)}
          className={item.totals.costMicros > 0 ? "text-ink-soft" : undefined}
        />
        <StatCell value={pct(item.errorRate)} unit="err" className={errorTone} />
        {canOpenChart ? (
          <button
            type="button"
            aria-expanded={chartOpen}
            aria-label={`Forecast chart for ${titleText} ${item.accountName}`}
            onClick={() => onToggleChart(item.key)}
            className={cx(
              "inline-flex h-7 w-7 items-center justify-center rounded-[var(--radius-control)] transition-colors",
              "hover:bg-surface-2 hover:text-fg focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--color-focus-ring)]",
              chartOpen ? "bg-surface-2 text-fg" : "text-faint",
            )}
          >
            <ChartLineUp size={15} aria-hidden />
          </button>
        ) : (
          <span aria-hidden className="h-7 w-7" />
        )}
      </footer>

      {canOpenChart && chartOpen && item.connectionId ? (
        <AccountForecastPanel connectionId={item.connectionId} windows={item.windows} now={now} />
      ) : null}
    </article>
  );
});

function groupHeading(group: BoardGroup, isAdmin: boolean): { title: string; note: string | null } {
  switch (group) {
    case "attention":
      return { title: "Needs attention", note: "Out of quota, close to a limit, or failing requests" };
    case "ok":
      return { title: isAdmin ? "Within limits" : "Active", note: null };
    case "untracked":
      return isAdmin
        ? {
            title: "No limits reported",
            note: "These providers don't report plan windows. Add an operator cap on Providers to track them against a limit.",
          }
        : { title: "Accounts", note: null };
    case "inactive":
      return { title: "Inactive", note: "Disabled accounts take no traffic" };
  }
}

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

  const groups = groupBoardAccounts(items);

  return (
    <section className="mb-5" aria-labelledby="usage-limits-board-heading">
      <div className="mb-3 flex flex-wrap items-baseline justify-between gap-2">
        <h2 id="usage-limits-board-heading" className="text-small font-semibold text-fg">
          {isAdmin ? "Provider limits & accounts" : "Accounts & range activity"}
        </h2>
        <span className="text-caption text-faint">
          {isAdmin
            ? `Bars show the current window; the tick is where it will land at reset. Activity covers the last ${rangeLabel}.`
            : `Activity covers the last ${rangeLabel}.`}
        </span>
      </div>

      <div className="flex flex-col gap-4">
        {groups.map(({ group, items: groupItems }) => {
          const heading = groupHeading(group, isAdmin);
          const headingId = `usage-board-group-${group}`;
          const anyChartOpen = groupItems.some((item) => expandedCharts.has(item.key));
          return (
            <section key={group} aria-labelledby={headingId} className="flex flex-col gap-2">
              <div className="flex flex-wrap items-baseline gap-x-2 gap-y-0.5">
                <h3 id={headingId} className="text-caption font-medium text-muted">
                  {heading.title} <span className="tnum text-faint">{groupItems.length}</span>
                </h3>
                {heading.note ? (
                  <span className="text-caption text-faint">{heading.note}</span>
                ) : null}
              </div>
              <div
                className={cx(
                  "grid grid-cols-1 gap-3 lg:grid-cols-2 2xl:grid-cols-3",
                  anyChartOpen ? "items-start" : "items-stretch",
                )}
              >
                {groupItems.map((item) => (
                  <AccountLimitCard
                    key={item.key}
                    item={item}
                    rangeLabel={rangeLabel}
                    isAdmin={isAdmin}
                    quotaLoaded={quotaLoaded}
                    quotaError={quotaError}
                    chartOpen={expandedCharts.has(item.key)}
                    stretch={!anyChartOpen}
                    onToggleChart={onToggleChart}
                    now={at}
                  />
                ))}
              </div>
            </section>
          );
        })}
      </div>
    </section>
  );
}
