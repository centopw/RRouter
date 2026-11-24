import { useEffect, useMemo, useRef, useState, type KeyboardEvent as ReactKeyboardEvent } from "react";
import { Link } from "react-router-dom";
import * as Popover from "@radix-ui/react-popover";
import { DotsThree, PlugsConnected } from "@phosphor-icons/react";
import { HealthBadge } from "../../components/molecules/HealthBadge";
import { ProviderIcon } from "../../components/provider-icon";
import { PaginationBar } from "../../components/table";
import {
  Badge,
  Button,
  Card,
  CardHeader,
  Dot,
  Meter,
  Segmented,
  barText,
  cx,
} from "../../components/ui";
import {
  type AccountQuotaRow,
  type AccountSubscription,
  type Connection,
  type ModelProvider,
} from "../../lib/api";
import { compact, ms } from "../../lib/format";
import { refusalView } from "../../lib/refusal";
import { resetsIn, worstQuotaBar } from "../usage/quota";
import { AccountNameLink } from "./account-preview";
import {
  accountStatus,
  connectionLabel,
  derivedIdentifiers,
  hostOf,
  isCustomConnection,
  protocolLabel,
  type AccountFilter,
  type AccountStatus,
  type AccountTraffic24h,
  type ConnectionTestResult,
  type ProviderGroup,
} from "./types";

/**
 * Aligned column grid from `md` (768px) upward:
 *   md:  account | owner | limit | status | actions
 *   lg+: account | owner | priority | limit | 24h | status | actions
 *
 * Keeps Account proportional (~280–320 px at 1440) instead of a 530 px void, and gives
 * Owner, 24 h traffic, and the limit bar their own dedicated columns.
 */
const DESKTOP_GRID =
  "md:grid md:items-center md:gap-x-3 md:grid-cols-[minmax(0,1.15fr)_5.5rem_minmax(11rem,1fr)_7.75rem_2.25rem] " +
  "lg:grid-cols-[minmax(10rem,1.2fr)_5.5rem_2.75rem_minmax(12rem,1.15fr)_4.5rem_8rem_2.25rem] " +
  "xl:grid-cols-[minmax(12rem,1.3fr)_6.5rem_3rem_minmax(14rem,1.25fr)_5.25rem_8.5rem_2.5rem]";

const OWNER_TOOLTIP =
  "Team keys route only to their own team's accounts; shared accounts serve instance-wide keys.";

/**
 * Kept exported for backwards compatibility with any caller importing `AccountCell`.
 * Renders the primary label of one account without repeating the provider name on custom rows.
 */
export function AccountCell({
  connection,
  display,
  custom,
}: {
  connection: Connection;
  display: string;
  custom: boolean;
}) {
  const derived = derivedIdentifiers(connection, display).filter(
    (part) => part.text !== `${display} (OAuth)`,
  );
  const host = connection.base_url_override ? hostOf(connection.base_url_override) : null;
  const name = custom ? null : connection.name;
  const primary = name ?? host ?? derived[0]?.text ?? null;
  const title = [
    primary,
    ...derived.filter((part) => part.text !== primary).map((part) => part.text),
    host && host !== primary ? host : null,
    custom ? protocolLabel(connection.provider) : null,
  ]
    .filter(Boolean)
    .join(" · ");
  return (
    <span
      className={cx("block min-w-0 truncate", name ? "text-fg" : "text-muted")}
      title={title || undefined}
    >
      {primary ?? <span className="text-faint">unnamed</span>}
    </span>
  );
}

/**
 * Single limit readout for one account: the worst bar across operator caps and provider
 * plan windows (`worstQuotaBar`), rendered with the shared `Meter` and `barText`
 * ladder. Never mixes two different windows into one cell, and never prints "no cap set"
 * unless both limit sources have actually answered.
 */
function LimitCell({
  quota,
  subscription,
  quotaLoaded,
  providerId,
}: {
  quota?: AccountQuotaRow;
  subscription?: AccountSubscription;
  quotaLoaded: boolean;
  providerId: string;
}) {
  const bar = worstQuotaBar(quota, subscription);
  const href = `/usage?provider=${encodeURIComponent(providerId)}`;

  if (bar) {
    const reset = resetsIn(bar.resets_at);
    const showResetInline = Boolean(reset) && bar.detail.length <= 12;
    return (
      <Link
        to={href}
        className="flex min-w-0 flex-col gap-1 rounded-[var(--radius-input)] text-caption hover:opacity-90 focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--color-focus-ring)]"
      >
        <span className="flex min-w-0 items-baseline justify-between gap-2">
          <span className="min-w-0 truncate text-muted">
            {bar.label}
            {showResetInline ? (
              <span className="hidden text-faint lg:inline"> · {reset}</span>
            ) : null}
          </span>
          <span className={cx("shrink-0 tnum", barText(bar.percent))}>{bar.detail}</span>
        </span>
        <Meter
          percent={bar.percent}
          label={reset ? `${bar.label}, ${reset}` : bar.label}
        />
      </Link>
    );
  }

  if (subscription?.error) {
    return (
      <Link
        to={href}
        className="block truncate rounded-[var(--radius-input)] text-caption text-faint underline-offset-2 hover:text-muted hover:underline focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--color-focus-ring)]"
      >
        unavailable
      </Link>
    );
  }

  if (!quotaLoaded) {
    return (
      <span className="block text-caption text-faint" aria-label="Limits unavailable for this role">
        —
      </span>
    );
  }

  return (
    <Link
      to={href}
      className="block truncate rounded-[var(--radius-input)] text-caption text-faint underline-offset-2 hover:text-muted hover:underline focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--color-focus-ring)]"
    >
      no cap set
    </Link>
  );
}

/**
 * Standing status pill (`HealthBadge`), or the live test result from this session when
 * the operator just tested this account. Standing state stays visible in the preview and drawer.
 */
function StatusCell({
  connection,
  status,
  testing,
  lastTest,
}: {
  connection: Connection;
  status: AccountStatus;
  testing: boolean;
  lastTest: ConnectionTestResult | null;
}) {
  if (testing) {
    return (
      <span role="status" className="flex min-w-0 items-center gap-1.5 text-caption text-muted">
        <Dot tone="muted" />
        <span className="truncate">Testing…</span>
      </span>
    );
  }

  if (lastTest) {
    return (
      <span role="status" className="flex min-w-0 items-center gap-1.5 text-caption tnum">
        <Dot tone={lastTest.ok ? "pos" : "neg"} />
        <span
          className={cx(
            "truncate",
            lastTest.ok ? "text-[var(--color-pos)]" : "text-[var(--color-neg)]",
          )}
        >
          {lastTest.ok
            ? `works · ${ms(lastTest.latency_ms)}`
            : `test failed${lastTest.status ? ` · ${lastTest.status}` : ""}`}
        </span>
      </span>
    );
  }

  return (
    <span className="flex min-w-0 items-center">
      <HealthBadge
        status={status.health}
        label={status.label}
        tone={status.kind === "refusal" ? status.refusal.tone : undefined}
        title={
          status.kind === "backoff"
            ? `Backoff level ${connection.backoff_level}`
            : (status.reason ?? undefined)
        }
      />
    </span>
  );
}

/**
 * Every per-account action behind one trigger, visible at every viewport width.
 */
function ConnectionMenu({
  connection,
  display,
  scope,
  testing,
  onTest,
  onToggle,
  onRemove,
}: {
  connection: Connection;
  display: string;
  scope: string;
  testing: boolean;
  onTest: () => void;
  onToggle: () => void;
  onRemove: () => void;
}) {
  const surface = useRef<HTMLDivElement>(null);
  const items = () =>
    [...(surface.current?.querySelectorAll<HTMLElement>('[role="menuitem"]') ?? [])];
  const focusItem = (index: number) => {
    const list = items();
    if (list.length === 0) return;
    list[(index + list.length) % list.length]?.focus();
  };
  const onKeyDown = (event: ReactKeyboardEvent<HTMLDivElement>) => {
    const index = items().indexOf(document.activeElement as HTMLElement);
    if (event.key === "ArrowDown") {
      event.preventDefault();
      focusItem(index + 1);
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      focusItem(index <= 0 ? -1 : index - 1);
    } else if (event.key === "Home") {
      event.preventDefault();
      focusItem(0);
    } else if (event.key === "End") {
      event.preventDefault();
      focusItem(-1);
    }
  };
  const row =
    "flex h-8 items-center rounded-[var(--radius-input)] px-2 text-left text-small text-fg hover:bg-surface-3 focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-[-2px] focus-visible:outline-[var(--color-focus-ring)]";
  const label = connectionLabel(connection, display);
  const entry = (text: string, onClick: () => void, danger = false, disabled = false) => (
    <Popover.Close asChild>
      <button
        type="button"
        role="menuitem"
        tabIndex={-1}
        onClick={onClick}
        disabled={disabled}
        className={cx(
          row,
          danger && "text-[var(--color-neg-ink)] hover:bg-[var(--color-neg-tint)]",
          disabled && "opacity-60",
        )}
      >
        {text}
      </button>
    </Popover.Close>
  );
  return (
    <Popover.Root>
      <Popover.Trigger asChild>
        <button
          type="button"
          aria-label={`Actions for ${label}`}
          aria-haspopup="menu"
          className="flex h-8 w-8 shrink-0 items-center justify-center rounded-full text-muted transition-colors hover:bg-surface-3 hover:text-fg focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--color-focus-ring)]"
        >
          <DotsThree size={18} weight="bold" aria-hidden />
        </button>
      </Popover.Trigger>
      <Popover.Portal>
        <Popover.Content
          ref={surface}
          role="menu"
          aria-label={`Actions for ${label}`}
          side="bottom"
          align="end"
          sideOffset={4}
          onOpenAutoFocus={(event) => {
            event.preventDefault();
            window.requestAnimationFrame(() => focusItem(0));
          }}
          onKeyDown={onKeyDown}
          className="popover-panel z-[var(--z-popover-portal)] flex w-56 flex-col rounded-[var(--radius-overlay)] border border-[var(--color-line-strong)] bg-surface p-1 shadow-[var(--shadow-popover)] focus:outline-none"
        >
          <div className="px-2 pb-1.5 pt-1 text-caption leading-relaxed text-faint tnum">
            {scope} · p{connection.priority} · w{connection.weight}
          </div>
          {entry(testing ? "Testing…" : "Test", onTest, false, testing)}
          {entry(
            connection.is_active
              ? "Disable"
              : refusalView(connection.refusal)
                ? "Re-enable refused account…"
                : "Enable",
            onToggle,
          )}
          <div className="my-1 border-t border-line" role="separator" />
          {entry("Remove…", onRemove, true)}
        </Popover.Content>
      </Popover.Portal>
    </Popover.Root>
  );
}

function AccountRow({
  connection,
  group,
  providers,
  quota,
  subscription,
  quotaLoaded,
  traffic,
  isAdmin,
  selected,
  testing,
  lastTest,
  inspectHref,
  teamName,
  displayOf,
  onTest,
  onToggle,
  onRemove,
}: {
  connection: Connection;
  group: ProviderGroup;
  providers?: ModelProvider[];
  quota?: AccountQuotaRow;
  subscription?: AccountSubscription;
  quotaLoaded: boolean;
  traffic?: AccountTraffic24h;
  isAdmin: boolean;
  selected: boolean;
  testing: boolean;
  lastTest: ConnectionTestResult | null;
  inspectHref: string;
  teamName: (id: string | null) => string | null;
  displayOf: (id: string) => string;
  onTest: (connectionId: string) => void;
  onToggle: (connection: Connection) => void;
  onRemove: (connection: Connection) => void;
}) {
  const custom = isCustomConnection(connection);
  const display = displayOf(connection.provider);
  const providerId = group.provider ?? connection.provider;
  const status = accountStatus(connection, quota, subscription, providers);
  const ownerLabel = teamName(connection.team_id) ?? "Shared";
  const lastError = connection.last_error?.trim() || null;
  const requests24h = traffic?.requests ?? 0;
  const errors24h = traffic?.errors ?? 0;

  return (
    <li
      className={cx(
        "flex flex-col gap-2 px-4 py-2.5 transition-colors hover:bg-[var(--color-row-hover)] md:min-h-[52px] md:gap-y-0 md:py-2",
        DESKTOP_GRID,
        status.kind === "disabled" && "opacity-75",
        selected && "bg-[var(--color-selected)] hover:bg-[var(--color-selected)]",
      )}
    >
      {/* Mobile top row: identity + menu. Desktop col 1: indented identity. */}
      <div className="flex min-w-0 items-center justify-between gap-2 md:pl-4">
        <span className="flex min-w-0 flex-1 items-center gap-2">
          <ProviderIcon provider={connection.provider} size={17} />
          <AccountNameLink
            connection={connection}
            display={display}
            custom={custom}
            inspectHref={inspectHref}
            status={status}
            quota={quota}
            subscription={subscription}
            quotaLoaded={quotaLoaded}
            lastTest={lastTest}
            ownerLabel={ownerLabel}
          />
          {connection.free_mode ? <Badge tone="neutral">free</Badge> : null}
          {custom ? <Badge tone="neutral">custom</Badge> : null}
          {connection.team_id ? (
            <span className="shrink-0 truncate text-caption text-muted md:hidden">{ownerLabel}</span>
          ) : null}
        </span>
        {isAdmin ? (
          <span className="md:hidden">
            <ConnectionMenu
              connection={connection}
              display={display}
              scope={ownerLabel}
              testing={testing}
              onTest={() => onTest(connection.id)}
              onToggle={() => onToggle(connection)}
              onRemove={() => onRemove(connection)}
            />
          </span>
        ) : null}
      </div>

      {/* Desktop md+ col 2: owner */}
      <span
        title={ownerLabel}
        className={cx(
          "hidden min-w-0 truncate text-caption md:block",
          connection.team_id ? "font-medium text-fg" : "text-muted",
        )}
      >
        {ownerLabel}
      </span>

      {/* Desktop lg+ col 3: priority */}
      <span className="hidden truncate text-right text-caption text-muted tnum lg:block">
        p{connection.priority}
      </span>

      {/* Mobile bottom row / Desktop cols 4, 5, 6: limit + 24h traffic + status */}
      <div className="flex min-w-0 flex-wrap items-center justify-between gap-x-3 gap-y-2 pl-6 md:contents md:pl-0">
        <div className="min-w-0 flex-1">
          <LimitCell
            quota={quota}
            subscription={subscription}
            quotaLoaded={quotaLoaded}
            providerId={providerId}
          />
        </div>
        <span
          title={
            requests24h > 0
              ? `${requests24h.toLocaleString()} requests${errors24h > 0 ? ` · ${errors24h.toLocaleString()} errors` : ""} in last 24h`
              : "No requests in last 24h"
          }
          className={cx(
            "hidden min-w-0 flex-col items-end text-right text-caption leading-tight tnum lg:flex",
            requests24h > 0 ? "text-fg" : "text-faint",
          )}
        >
          {requests24h > 0 ? (
            <>
              <span className="truncate">{compact(requests24h)}</span>
              {errors24h > 0 ? (
                <span className="truncate text-[10px] text-[var(--color-neg)]">
                  {compact(errors24h)} err
                </span>
              ) : null}
            </>
          ) : (
            "—"
          )}
        </span>
        <div className="shrink-0 md:min-w-0">
          <StatusCell
            connection={connection}
            status={status}
            testing={testing}
            lastTest={lastTest}
          />
        </div>
      </div>

      {/* Desktop col 7: actions menu */}
      <span className="hidden items-center justify-end md:flex">
        {isAdmin ? (
          <ConnectionMenu
            connection={connection}
            display={display}
            scope={ownerLabel}
            testing={testing}
            onTest={() => onTest(connection.id)}
            onToggle={() => onToggle(connection)}
            onRemove={() => onRemove(connection)}
          />
        ) : null}
      </span>

      {/* Inline error sub-row whenever last_error is set, including stale errors when backoff is 0 */}
      {lastError ? (
        <div className="min-w-0 pl-6 md:col-span-full md:pl-10 md:pt-1">
          <Link
            to={inspectHref}
            title={lastError}
            className="flex min-w-0 items-baseline gap-1.5 rounded-[var(--radius-input)] text-caption text-[var(--color-neg-ink)] underline-offset-2 hover:underline focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--color-focus-ring)]"
          >
            <span aria-hidden className="shrink-0 text-[var(--color-neg)]">
              ↳
            </span>
            <span className="min-w-0 truncate font-mono">
              {status.kind === "last_error" ? "" : connection.backoff_level === 0 ? "Last error: " : ""}
              {lastError}
            </span>
          </Link>
        </div>
      ) : null}
    </li>
  );
}

/**
 * Provider header: name links to `/models?provider=<id>`, one visible readiness readout
 * scoped strictly to non-disabled accounts ("N/M enabled ready · K off"), and a direct
 * link to Usage. Accounts stay expanded underneath — no collapse caret.
 */
function ProviderHeader({ group }: { group: ProviderGroup }) {
  const providerId = group.provider ?? group.connections[0]?.provider ?? "";

  const readiness =
    group.expected === 0 && group.disabled > 0
      ? `${group.disabled} disabled`
      : group.disabled > 0
        ? `${group.ready}/${group.expected} enabled ready · ${group.disabled} off`
        : `${group.ready}/${group.expected} ready`;

  const textCls =
    group.state === "healthy"
      ? "text-muted"
      : group.state === "degraded"
        ? "text-[var(--color-warn)]"
        : group.state === "down"
          ? "text-[var(--color-neg)]"
          : "text-faint";

  return (
    <div className="flex flex-wrap items-center justify-between gap-x-4 gap-y-1.5 bg-surface-2/70 px-4 py-2.5 md:min-h-[42px] md:py-2">
      <span className="flex min-w-0 items-center gap-2">
        <ProviderIcon provider={group.provider ?? "custom"} size={18} />
        <Link
          to={`/models?provider=${encodeURIComponent(providerId)}`}
          className="min-w-0 truncate font-medium text-fg underline-offset-2 hover:text-[var(--color-accent-text)] hover:underline focus-visible:text-[var(--color-accent-text)] focus-visible:underline focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--color-focus-ring)]"
        >
          {group.label}
        </Link>
        {group.code ? <Badge tone="neutral">{group.code}</Badge> : null}
      </span>

      <span className="flex flex-wrap items-center gap-x-4 gap-y-1">
        <span className="flex min-w-0 items-center gap-1.5 text-caption tnum">
          {group.state === "disabled" ? (
            <span
              aria-hidden
              className="inline-block h-1.5 w-1.5 shrink-0 rounded-full border border-faint"
            />
          ) : (
            <Dot
              tone={
                group.state === "healthy"
                  ? "pos"
                  : group.state === "degraded"
                    ? "warn"
                    : "neg"
              }
            />
          )}
          <span className={cx("truncate", textCls)}>{readiness}</span>
        </span>
        <Link
          to={`/usage?provider=${encodeURIComponent(providerId)}`}
          className="whitespace-nowrap rounded-[var(--radius-input)] text-caption text-accent-text underline-offset-2 hover:underline focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--color-focus-ring)]"
        >
          Usage →
        </Link>
      </span>
    </div>
  );
}

/**
 * Loading state on the same grid so rows do not shift when accounts land.
 */
function InventorySkeleton() {
  return (
    <div aria-busy="true">
      <span className="sr-only">Loading accounts…</span>
      {Array.from({ length: 4 }, (_, index) => (
        <div
          key={index}
          aria-hidden="true"
          className={cx(
            "flex min-h-[52px] items-center justify-between gap-3 border-b border-line px-4 py-2 last:border-b-0",
            DESKTOP_GRID,
          )}
        >
          <span className="flex min-w-0 items-center gap-2">
            <span className="h-4 w-4 shrink-0 animate-pulse rounded bg-surface-3" />
            <span className="h-3 w-28 max-w-full animate-pulse rounded bg-surface-3" />
          </span>
          <span className="hidden h-3 w-14 animate-pulse rounded bg-surface-3 md:block" />
          <span className="hidden h-3 w-7 animate-pulse rounded bg-surface-3 lg:ml-auto lg:block" />
          <span className="h-3 w-24 max-w-full animate-pulse rounded bg-surface-3" />
          <span className="hidden h-3 w-10 animate-pulse rounded bg-surface-3 lg:ml-auto lg:block" />
          <span className="h-5 w-16 max-w-full animate-pulse rounded-full bg-surface-3" />
          <span className="ml-auto hidden h-3 w-6 animate-pulse rounded bg-surface-3 md:block" />
        </div>
      ))}
    </div>
  );
}

const PAGE_SIZES = [10, 25, 50, 100];
const DEFAULT_PAGE_SIZE = 10;

/**
 * The inventory: one section per provider, its accounts underneath, always expanded so
 * unhealthy accounts are never hidden behind a toggle. Providers are paged cleanly on
 * group boundaries.
 */
export function ProviderInventory({
  groups,
  totalAccounts,
  totalProviders,
  attentionCount,
  disabledCount,
  filter,
  onFilterChange,
  quotas,
  subscriptions,
  quotaLoaded,
  trafficById,
  providers,
  isAdmin,
  testingId,
  testsById,
  inspectHrefFor,
  displayOf,
  teamName,
  selectedId,
  loading,
  onTest,
  onToggle,
  onRemove,
  onOpenCodes,
}: {
  groups: ProviderGroup[];
  totalAccounts: number;
  totalProviders: number;
  attentionCount: number;
  disabledCount: number;
  filter: AccountFilter;
  onFilterChange: (next: AccountFilter) => void;
  quotas?: AccountQuotaRow[];
  subscriptions?: AccountSubscription[];
  quotaLoaded: boolean;
  trafficById: ReadonlyMap<string, AccountTraffic24h>;
  providers?: ModelProvider[];
  isAdmin: boolean;
  testingId: string | null;
  testsById: Record<string, ConnectionTestResult>;
  inspectHrefFor: (connectionId: string) => string;
  displayOf: (id: string) => string;
  teamName: (id: string | null) => string | null;
  selectedId?: string;
  loading: boolean;
  onTest: (connectionId: string) => void;
  onToggle: (connection: Connection) => void;
  onRemove: (connection: Connection) => void;
  onOpenCodes: () => void;
}) {
  const [page, setPage] = useState(1);
  const [size, setSize] = useState(DEFAULT_PAGE_SIZE);
  const quotaBy = useMemo(
    () => new Map((quotas ?? []).map((row) => [row.connection_id, row])),
    [quotas],
  );
  const subscriptionBy = useMemo(
    () => new Map((subscriptions ?? []).map((row) => [row.connection_id, row])),
    [subscriptions],
  );
  const lastPage = Math.max(1, Math.ceil(groups.length / size));
  const current = Math.min(page, lastPage);
  const visible = groups.slice((current - 1) * size, current * size);
  const previousTotal = useRef(groups.length);

  useEffect(() => {
    if (groups.length < previousTotal.current) setPage(1);
    previousTotal.current = groups.length;
  }, [groups.length]);
  useEffect(() => {
    setPage(1);
  }, [size, filter]);

  return (
    <Card className="overflow-hidden">
      <CardHeader
        icon={<PlugsConnected size={16} />}
        title="Accounts"
        meta={
          !loading
            ? `${totalAccounts} · ${totalProviders} provider${totalProviders === 1 ? "" : "s"}`
            : undefined
        }
        padding="compact"
        className="border-b border-line"
        action={
          <div className="flex flex-wrap items-center gap-2">
            <Segmented
              ariaLabel="Filter accounts by status"
              value={filter}
              onChange={onFilterChange}
              options={[
                { id: "all", label: "All" },
                {
                  id: "attention",
                  label:
                    attentionCount > 0 ? `Needs attention (${attentionCount})` : "Needs attention",
                },
                {
                  id: "disabled",
                  label: disabledCount > 0 ? `Disabled (${disabledCount})` : "Disabled",
                },
              ]}
            />
            {isAdmin ? (
              <Button variant="quiet" size="sm" onClick={onOpenCodes}>
                Provider codes
              </Button>
            ) : null}
          </div>
        }
      />

      <div id="providers-connected">
        {loading ? (
          <InventorySkeleton />
        ) : groups.length === 0 ? (
          <div className="flex flex-col items-center gap-2 px-4 py-10 text-center">
            <p className="text-small text-muted">
              {filter === "attention"
                ? "No accounts need attention right now."
                : "No accounts are disabled."}
            </p>
            <Button variant="quiet" size="sm" onClick={() => onFilterChange("all")}>
              Show all accounts
            </Button>
          </div>
        ) : (
          <>
            <div
              className={cx(
                "hidden bg-surface px-4 py-2 text-caption font-medium text-faint md:grid",
                DESKTOP_GRID,
              )}
            >
              <span>Account</span>
              <span>
                <span
                  title={OWNER_TOOLTIP}
                  className="cursor-help underline decoration-dotted decoration-faint/60 underline-offset-2"
                >
                  Owner
                </span>
              </span>
              <span className="hidden text-right lg:block">Prio</span>
              <span>Limit</span>
              <span className="hidden text-right lg:block">24h</span>
              <span>Status</span>
              <span className="sr-only">Actions</span>
            </div>
            <ul className="divide-y divide-line border-t border-line">
              {visible.map((group) => (
                <li key={group.key}>
                  <ProviderHeader group={group} />
                  <ul
                    id={`accounts-${group.key}`}
                    className="divide-y divide-rule border-t border-line"
                  >
                    {group.connections.map((connection) => (
                      <AccountRow
                        key={connection.id}
                        connection={connection}
                        group={group}
                        providers={providers}
                        quota={quotaBy.get(connection.id)}
                        subscription={subscriptionBy.get(connection.id)}
                        quotaLoaded={quotaLoaded}
                        traffic={trafficById.get(connection.id)}
                        isAdmin={isAdmin}
                        selected={selectedId === connection.id}
                        testing={testingId === connection.id}
                        lastTest={testsById[connection.id] ?? null}
                        inspectHref={inspectHrefFor(connection.id)}
                        teamName={teamName}
                        displayOf={displayOf}
                        onTest={onTest}
                        onToggle={onToggle}
                        onRemove={onRemove}
                      />
                    ))}
                  </ul>
                </li>
              ))}
            </ul>
          </>
        )}
        <PaginationBar
          pagination={{
            page: current,
            size,
            total: groups.length,
            sizes: PAGE_SIZES,
            label: "providers",
            onPage: setPage,
            onSize: setSize,
          }}
        />
      </div>
    </Card>
  );
}
