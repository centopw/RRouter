import { useEffect, useMemo, useRef, useState, type KeyboardEvent as ReactKeyboardEvent } from "react";
import { Link } from "react-router-dom";
import * as Popover from "@radix-ui/react-popover";
import { DotsThree } from "@phosphor-icons/react";
import { ProviderIcon } from "../../components/provider-icon";
import { PaginationBar } from "../../components/table";
import { Badge, Button, Card, Dot, Heading, cx } from "../../components/ui";
import {
  type AccountQuotaRow,
  type AccountSubscription,
  type Connection,
  type ModelProvider,
} from "../../lib/api";
import { ms } from "../../lib/format";
import { refusalView } from "../../lib/refusal";
import { QuotaMeter, resetsIn, worstQuotaBar } from "../usage/quota";
import { AccountNameLink } from "./account-preview";
import {
  accountStatus,
  barText,
  connectionLabel,
  derivedIdentifiers,
  hostOf,
  isCustomConnection,
  protocolLabel,
  type AccountStatus,
  type ConnectionTestResult,
  type ProviderGroup,
} from "./types";

/**
 * Aligned column grid from `md` (768px) upward:
 *   md:  identity | limit | status | actions
 *   lg+: identity | priority | limit | status | actions
 *
 * Below `md` (phone widths), each row stacks cleanly into two readable lines so account
 * names never collapse to a few letters and the actions menu stays reachable at every width.
 */
const DESKTOP_GRID =
  "md:grid md:items-center md:gap-x-3 md:grid-cols-[minmax(0,1fr)_13rem_8.5rem_3.25rem] " +
  "lg:grid-cols-[minmax(0,1fr)_3.25rem_16rem_9.5rem_3.25rem] " +
  "xl:grid-cols-[minmax(0,1fr)_3.25rem_18rem_9.5rem_3.25rem]";

const DOT_TONE = { muted: "muted", warn: "warn", neg: "neg" } as const;
const STATE_TEXT = {
  muted: "text-muted",
  warn: "text-[var(--color-warn)]",
  neg: "text-[var(--color-neg)]",
} as const;

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
 * plan windows (`worstQuotaBar`), rendered with the shared `QuotaMeter` and `barText`
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
    // Long spend ratios ("$14.2000 / $50.0000") need the label's horizontal room; keep
    // "resets in" inline when the right-hand detail is compact, and always show it in the
    // preview and drawer.
    const showResetInline = Boolean(reset) && bar.detail.length <= 10;
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
        <QuotaMeter
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
 * Standing status, or the live test result from this session when the operator just tested
 * this account. Standing state stays visible in the preview and drawer.
 */
function StatusCell({
  status,
  testing,
  lastTest,
}: {
  status: AccountStatus;
  testing: boolean;
  lastTest: ConnectionTestResult | null;
}) {
  if (testing) {
    return (
      <span role="status" className="flex min-w-0 items-center gap-1.5 text-caption text-muted">
        <Dot tone="muted" />
        <span className="truncate">testing…</span>
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

  if (status.kind === "refusal") {
    return (
      <span className="flex min-w-0 items-center">
        <Badge tone={status.refusal.tone}>{status.refusal.label}</Badge>
      </span>
    );
  }

  return (
    <span className="flex min-w-0 items-center gap-1.5 text-caption">
      <Dot tone={DOT_TONE[status.tone]} />
      <span className={cx("truncate", STATE_TEXT[status.tone])}>{status.label}</span>
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

  return (
    <li
      className={cx(
        "flex flex-col gap-2 px-4 py-2.5 transition-colors hover:bg-[var(--color-row-hover)] md:min-h-[52px] md:gap-0 md:py-2",
        DESKTOP_GRID,
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
          {custom ? <Badge tone="neutral">custom</Badge> : null}
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

      {/* Desktop lg+ col 2: priority */}
      <span className="hidden truncate text-right text-caption text-muted tnum lg:block">
        p{connection.priority}
      </span>

      {/* Mobile bottom row / Desktop cols 3 & 4: limit + status */}
      <div className="flex min-w-0 flex-wrap items-center justify-between gap-x-3 gap-y-2 pl-6 md:contents md:pl-0">
        <div className="min-w-0 flex-1">
          <LimitCell
            quota={quota}
            subscription={subscription}
            quotaLoaded={quotaLoaded}
            providerId={providerId}
          />
        </div>
        <div className="shrink-0 md:min-w-0">
          <StatusCell status={status} testing={testing} lastTest={lastTest} />
        </div>
      </div>

      {/* Desktop col 5: actions menu */}
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
    </li>
  );
}

/**
 * Provider header: name links to `/models?provider=<id>`, one visible readiness readout
 * ("2 of 3 ready", toned by `group.state`) at every width, and a direct link to Usage.
 * Accounts stay expanded underneath — no collapse caret.
 */
function ProviderHeader({ group }: { group: ProviderGroup }) {
  const providerId = group.provider ?? group.connections[0]?.provider ?? "";
  const stateTone =
    group.state === "healthy" ? "muted" : group.state === "degraded" ? "warn" : "neg";
  const readiness = `${group.ready} of ${group.connections.length} ready`;

  return (
    <div
      className={cx(
        "flex flex-wrap items-center justify-between gap-x-3 gap-y-1.5 bg-surface-2/70 px-4 py-2.5 md:min-h-[44px] md:py-2",
        DESKTOP_GRID,
      )}
    >
      <span className="flex min-w-0 items-center gap-2 md:col-start-1">
        <ProviderIcon provider={group.provider ?? "custom"} size={18} />
        <Link
          to={`/models?provider=${encodeURIComponent(providerId)}`}
          className="min-w-0 truncate font-medium text-fg underline-offset-2 hover:text-[var(--color-accent-text)] hover:underline focus-visible:text-[var(--color-accent-text)] focus-visible:underline focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--color-focus-ring)]"
        >
          {group.label}
        </Link>
        {group.code ? <Badge tone="neutral">{group.code}</Badge> : null}
      </span>

      <span className="flex items-center gap-3 md:contents">
        <span className="flex min-w-0 items-center gap-1.5 text-caption tnum md:col-start-3 lg:col-start-4">
          <Dot tone={DOT_TONE[stateTone]} />
          <span className={cx("truncate", STATE_TEXT[stateTone])}>{readiness}</span>
        </span>
        <Link
          to={`/usage?provider=${encodeURIComponent(providerId)}`}
          className="justify-self-end whitespace-nowrap rounded-[var(--radius-input)] text-caption text-muted underline-offset-2 hover:text-fg hover:underline focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--color-focus-ring)] md:col-start-4 lg:col-start-5"
        >
          usage
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
    <div aria-busy="true" className="border-t border-line">
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
          <span className="hidden h-3 w-8 animate-pulse rounded bg-surface-3 lg:ml-auto lg:block" />
          <span className="h-3 w-24 max-w-full animate-pulse rounded bg-surface-3" />
          <span className="h-3 w-16 max-w-full animate-pulse rounded bg-surface-3" />
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
  quotas,
  subscriptions,
  quotaLoaded,
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
  quotas?: AccountQuotaRow[];
  subscriptions?: AccountSubscription[];
  quotaLoaded: boolean;
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
  const accountCount = groups.reduce((total, group) => total + group.connections.length, 0);
  const readyCount = groups.reduce((total, group) => total + group.ready, 0);
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
  }, [size]);

  const summaryTone =
    accountCount === 0
      ? "text-faint"
      : readyCount === 0
        ? "text-[var(--color-neg)]"
        : readyCount < accountCount
          ? "text-[var(--color-warn)]"
          : "text-faint";

  return (
    <Card className="overflow-hidden">
      <div className="flex flex-wrap items-center justify-between gap-x-3 gap-y-2 px-4 py-3 sm:px-5">
        <Heading level={2}>Connections</Heading>
        <span className="flex flex-wrap items-center gap-x-3 gap-y-1.5">
          {isAdmin ? (
            <Button variant="quiet" size="sm" onClick={onOpenCodes}>
              Provider codes
            </Button>
          ) : null}
          {!loading ? (
            <span className={cx("text-caption tnum", summaryTone)}>
              {readyCount} of {accountCount} ready · {groups.length} provider
              {groups.length === 1 ? "" : "s"}
            </span>
          ) : null}
        </span>
      </div>
      <div id="providers-connected">
        {loading ? (
          <InventorySkeleton />
        ) : (
          <>
            <div
              aria-hidden="true"
              className={cx(
                "hidden border-t border-line bg-surface px-4 py-2 text-caption font-medium text-faint md:grid",
                DESKTOP_GRID,
              )}
            >
              <span>Account</span>
              <span className="hidden text-right lg:block">Priority</span>
              <span>Limit</span>
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
