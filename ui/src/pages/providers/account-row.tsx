import { useEffect, useMemo, useRef, useState, type KeyboardEvent as ReactKeyboardEvent } from "react";
import { Link } from "react-router-dom";
import * as Popover from "@radix-ui/react-popover";
import { CaretRight, DotsThree } from "@phosphor-icons/react";
import { ProviderIcon } from "../../components/provider-icon";
import { PaginationBar } from "../../components/table";
import { Badge, Button, Card, CardHeader, Dot, cx } from "../../components/ui";
import {
  type AccountQuotaRow,
  type AccountSubscription,
  type Connection,
  type ModelProvider,
} from "../../lib/api";
import { compact, usd } from "../../lib/format";
import {
  barFill,
  barText,
  connectionLabel,
  credentialShapeOf,
  derivedIdentifiers,
  hostOf,
  isCustomConnection,
  needsBaseUrlOverride,
  protocolLabel,
  quotaUsagePercent,
  subscriptionUsagePercent,
  until,
  type ProviderGroup,
} from "./types";

/**
 * One grid for the provider header and every account row, so a column starts at the same
 * x on every line of the list. Columns, in order: identity | auth | priority·weight |
 * quota | plan window | state | actions. The secondary columns drop out of the grid as
 * the window narrows — plan window at `xl`, auth and priority at `lg`, the quota bar at
 * 820px — and `display: none` takes no track, so the remaining cells keep the same x from
 * row to row at every width.
 */
const ROW_GRID =
  "grid grid-cols-[minmax(0,1fr)_3.5rem_4.25rem_5.5rem] items-center gap-x-2 " +
  "min-[820px]:grid-cols-[minmax(0,1fr)_7rem_5.5rem_7.5rem] min-[820px]:gap-x-3 " +
  "lg:grid-cols-[minmax(0,1fr)_4.5rem_5.5rem_7.5rem_6rem_7.5rem] " +
  "xl:grid-cols-[minmax(0,1fr)_4.5rem_5.5rem_7.5rem_6rem_7rem_7.5rem]";

/** Every row is this tall; nothing in it may wrap, so the height never varies. */
const ROW_HEIGHT = "h-12";

/**
 * The account column: the operator's name first, then the identifiers that name the
 * upstream account itself. Clearing the name leaves those lines alone, so a row is never
 * blank: the first identifier becomes the primary line.
 *
 * A custom endpoint's name is the provider's name, already on screen in its own cell, so
 * repeating it here would print the same text twice; the host names the account instead.
 * The row is one line, so the rest of the identifiers live in the title.
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
  // The provider header names the provider and the auth column names the credential, so
  // the derived `"<display> (OAuth)"` marker would only repeat both.
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

type AccountState = { label: string; tone: "muted" | "warn" | "neg" };

const DOT_TONE = { muted: "muted", warn: "warn", neg: "neg" } as const;
const STATE_TEXT = {
  muted: "text-muted",
  warn: "text-[var(--color-warn)]",
  neg: "text-[var(--color-neg)]",
} as const;

/** The one status readout an account gets: what stops it routing, else that it is ready. */
function stateOf(
  connection: Connection,
  quota: AccountQuotaRow | undefined,
  planPercent: number | null,
  providers?: ModelProvider[],
): AccountState {
  if (!connection.is_active) return { label: "disabled", tone: "muted" };
  if (connection.backoff_level > 0)
    return { label: `backoff ${connection.backoff_level}`, tone: "warn" };
  if (needsBaseUrlOverride(connection, providers)) return { label: "no endpoint", tone: "warn" };
  if (quota?.exhausted) return { label: "over quota", tone: "neg" };
  if (planPercent !== null && planPercent >= 90) return { label: "plan at limit", tone: "warn" };
  return { label: "ready", tone: "muted" };
}

type QuotaView = {
  /** Worst current-window usage, or `null` when nothing reports a limit. */
  percent: number | null;
  /** Full current-window detail, carried as the tooltip of both the bar and the window. */
  detail: string;
  /** Which window owns the number: a plan window's name, or the operator quota. */
  window: string;
};

/**
 * One read of the two limit sources: the operator quota and the provider's own plan
 * windows. The number and the window it belongs to are split out here so the row can
 * give each its own fixed column instead of letting the layout be driven by text length.
 */
function quotaView(quota?: AccountQuotaRow, subscription?: AccountSubscription): QuotaView {
  const quotaPercent = quotaUsagePercent(quota);
  const worstWindow =
    subscription && subscription.windows.length > 0
      ? subscription.windows.reduce((worst, window) =>
          window.used_percent > worst.used_percent ? window : worst,
        )
      : null;
  const detail = [
    quota && quotaPercent !== null
      ? [
          quota.max_requests > 0 ? `req ${compact(quota.requests)}/${compact(quota.max_requests)}` : null,
          quota.max_tokens > 0 ? `tok ${compact(quota.tokens)}/${compact(quota.max_tokens)}` : null,
          quota.max_cost_micros > 0
            ? `${usd(quota.cost_micros)}/${usd(quota.max_cost_micros)}`
            : null,
        ]
          .filter(Boolean)
          .join(" · ")
      : null,
    worstWindow
      ? `${worstWindow.label} ${Math.round(worstWindow.used_percent)}%${
          worstWindow.resets_at ? ` · resets in ${until(worstWindow.resets_at)}` : ""
        }`
      : null,
  ]
    .filter(Boolean)
    .join(" · ");
  return {
    percent: quotaPercent ?? subscriptionUsagePercent(subscription),
    detail,
    window: worstWindow
      ? `plan · ${worstWindow.label}`
      : quotaPercent !== null
        ? "operator quota"
        : "—",
  };
}

/**
 * Thin magnitude bar at a fixed width + the number in its own right-aligned cell,
 * linking out to the aggregate surface that owns the detail. Below 820px only the number
 * is shown: the bar has no room to be read and the percentage carries the signal.
 */
function QuotaCell({ view, providerId }: { view: QuotaView; providerId: string }) {
  return (
    <Link
      to={`/usage?provider=${encodeURIComponent(providerId)}`}
      title={view.detail || "no quota or plan limit reported for this account"}
      className="flex min-w-0 items-center justify-end gap-2 rounded-[var(--radius-input)] sm:justify-start focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--color-focus-ring)]"
    >
      <span
        className="hidden h-1 w-16 shrink-0 overflow-hidden rounded-full bg-rule min-[820px]:block"
        aria-hidden
      >
        {view.percent === null ? null : (
          <span
            className={cx("block h-full rounded-full", barFill(view.percent))}
            style={{ width: `${view.percent}%` }}
          />
        )}
      </span>
      <span
        className={cx(
          "block w-10 shrink-0 text-right text-caption tnum",
          view.percent === null ? "text-faint" : barText(view.percent),
        )}
      >
        {view.percent === null ? "—" : `${view.percent}%`}
      </span>
    </Link>
  );
}

/** The window the number belongs to: one truncated line, the detail in the title. */
function PlanCell({ view }: { view: QuotaView }) {
  return (
    <span
      className="hidden min-w-0 truncate text-caption text-muted xl:block"
      title={view.detail || "no plan window reported for this account"}
    >
      {view.window}
    </span>
  );
}

/**
 * Every per-account action behind one trigger. The drawer below already holds inspection
 * and configuration, so this menu only carries the state changes and the destructive one.
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
            {scope} · priority {connection.priority} · weight {connection.weight}
          </div>
          {entry(testing ? "Testing…" : "Test", onTest, false, testing)}
          {entry(connection.is_active ? "Disable" : "Enable", onToggle)}
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
  isAdmin,
  selected,
  testing,
  teamName,
  displayOf,
  onConfigure,
  onTest,
  onToggle,
  onRemove,
}: {
  connection: Connection;
  group: ProviderGroup;
  providers?: ModelProvider[];
  quota?: AccountQuotaRow;
  subscription?: AccountSubscription;
  isAdmin: boolean;
  selected: boolean;
  testing: boolean;
  teamName: (id: string | null) => string | null;
  displayOf: (id: string) => string;
  onConfigure: (connectionId: string) => void;
  onTest: (connectionId: string) => void;
  onToggle: (connection: Connection) => void;
  onRemove: (connection: Connection) => void;
}) {
  const custom = isCustomConnection(connection);
  const display = displayOf(connection.provider);
  const providerId = group.provider ?? connection.provider;
  const view = quotaView(quota, subscription);
  const state = stateOf(connection, quota, subscriptionUsagePercent(subscription), providers);
  const label = connectionLabel(connection, display);
  const auth = connection.auth_type === "oauth" ? "oauth" : "api key";
  return (
    <li
      className={cx(
        ROW_GRID,
        ROW_HEIGHT,
        "px-4",
        selected && "bg-[var(--color-selected)]",
      )}
    >
      <span className="flex min-w-0 items-center gap-2">
        <ProviderIcon provider={connection.provider} size={17} />
        <AccountCell connection={connection} display={display} custom={custom} />
        {custom ? (
          <Badge
            tone="neutral"
            title="keyed account pointing at a custom base URL, not the provider's own API"
          >
            custom
          </Badge>
        ) : null}
      </span>

      <span
        className="hidden truncate text-right text-caption text-faint lg:block"
        title={credentialShapeOf(connection)}
      >
        {auth}
      </span>

      <button
        type="button"
        onClick={() => onConfigure(connection.id)}
        aria-label={`Routing for ${label}: priority ${connection.priority}, weight ${connection.weight}`}
        title="priority · weight inside one priority group (lower priority is tried first) — open routing"
        className="hidden w-full truncate text-right text-caption text-muted tnum underline-offset-2 hover:text-fg hover:underline focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--color-focus-ring)] lg:block"
      >
        p{connection.priority} · w{connection.weight}
      </button>

      <QuotaCell view={view} providerId={providerId} />

      <PlanCell view={view} />

      <span className="flex min-w-0 items-center gap-1.5 text-caption">
        <Dot tone={DOT_TONE[state.tone]} />
        <span className={cx("truncate", STATE_TEXT[state.tone])} title={`${label}: ${state.label}`}>
          {state.label}
        </span>
      </span>

      <span className="flex items-center justify-end gap-1">
        <Button variant="quiet" size="sm" onClick={() => onConfigure(connection.id)}>
          {isAdmin ? "Configure" : "Details"}
        </Button>
        {/* The phone width has no room for both; the drawer carries test/toggle/remove. */}
        {isAdmin ? (
          <span className="hidden min-[820px]:block">
            <ConnectionMenu
              connection={connection}
              display={display}
              scope={teamName(connection.team_id) ?? "shared"}
              testing={testing}
              onTest={() => onTest(connection.id)}
              onToggle={() => onToggle(connection)}
              onRemove={() => onRemove(connection)}
            />
          </span>
        ) : null}
      </span>
    </li>
  );
}

/**
 * Provider header: identity, one status readout, counts, and the link to aggregate usage.
 * It shares the account grid, and pins each cell to its column with an explicit
 * `col-start` so a cell that is hidden at a narrow width never shifts the others.
 */
function ProviderHeader({
  group,
  open,
  onToggle,
}: {
  group: ProviderGroup;
  open: boolean;
  onToggle: () => void;
}) {
  const providerId = group.provider ?? group.connections[0]?.provider ?? "";
  const stateTone = group.state === "healthy" ? "muted" : group.state === "degraded" ? "warn" : "neg";
  const counts = [
    `${group.connections.length} account${group.connections.length === 1 ? "" : "s"}`,
    `${group.enabled} enabled`,
    group.backoff > 0 ? `${group.backoff} in backoff` : null,
    group.overQuota > 0 ? `${group.overQuota} over quota` : null,
  ]
    .filter(Boolean)
    .join(" · ");
  return (
    <div className={cx(ROW_GRID, ROW_HEIGHT, "px-4")}>
      <button
        type="button"
        aria-expanded={open}
        aria-controls={`accounts-${group.key}`}
        onClick={onToggle}
        className="col-start-1 flex min-w-0 items-center gap-2 rounded-[var(--radius-input)] text-left focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--color-focus-ring)]"
      >
        <CaretRight
          size={12}
          weight="bold"
          aria-hidden
          className={cx("shrink-0 text-faint transition-transform", open && "rotate-90")}
        />
        <ProviderIcon provider={group.provider ?? "custom"} size={18} />
        <span className="min-w-0 truncate font-medium text-fg">{group.label}</span>
        {group.code ? (
          <Badge tone="neutral" title="shorthand used in this provider's model refs">
            {group.code}
          </Badge>
        ) : null}
      </button>
      <span
        className="col-start-2 hidden min-w-0 truncate text-caption text-faint tnum lg:col-span-3 lg:block xl:col-span-4"
        title={counts}
      >
        {counts}
      </span>
      <span className="col-start-3 flex min-w-0 items-center gap-1.5 text-caption lg:col-start-5 xl:col-start-6">
        <Dot tone={stateTone === "muted" ? "muted" : stateTone} />
        <span className={cx("truncate", STATE_TEXT[stateTone])}>{group.state}</span>
      </span>
      <Link
        to={`/usage?provider=${encodeURIComponent(providerId)}`}
        className="col-start-4 justify-self-end whitespace-nowrap text-caption text-muted underline underline-offset-2 hover:text-fg lg:col-start-6 xl:col-start-7"
      >
        usage
      </Link>
    </div>
  );
}

/**
 * Loading state on the same grid and the same row height, so the list does not move when
 * the accounts land. Placeholders sit in the columns they will be replaced by.
 */
function InventorySkeleton() {
  return (
    <div aria-busy="true" className="border-t border-line">
      <span className="sr-only">Loading accounts…</span>
      {Array.from({ length: 4 }, (_, index) => (
        <div
          key={index}
          aria-hidden="true"
          className={cx(ROW_GRID, ROW_HEIGHT, "border-b border-line px-4 last:border-b-0")}
        >
          <span className="flex min-w-0 items-center gap-2">
            <span className="h-4 w-4 shrink-0 animate-pulse rounded bg-surface-3" />
            <span className="h-3 w-28 max-w-full animate-pulse rounded bg-surface-3" />
          </span>
          <span className="hidden h-3 w-10 animate-pulse rounded bg-surface-3 lg:block" />
          <span className="hidden h-3 w-12 animate-pulse rounded bg-surface-3 lg:ml-auto lg:block" />
          <span className="h-3 w-16 max-w-full animate-pulse rounded bg-surface-3 lg:justify-self-start" />
          <span className="hidden h-3 w-14 animate-pulse rounded bg-surface-3 xl:block" />
          <span className="h-3 w-10 max-w-full animate-pulse rounded bg-surface-3" />
          <span className="ml-auto h-3 w-10 animate-pulse rounded bg-surface-3" />
        </div>
      ))}
    </div>
  );
}

const PAGE_SIZES = [10, 25, 50, 100];
const DEFAULT_PAGE_SIZE = 10;

/**
 * The inventory: one section per provider, its accounts underneath, all expanded until the
 * operator folds one away. Providers are paged — every list in the app has a footer — and
 * the page boundary is drawn between provider groups, never inside one, so a header is
 * never separated from its own accounts.
 */
export function ProviderInventory({
  groups,
  quotas,
  subscriptions,
  providers,
  isAdmin,
  testingId,
  displayOf,
  teamName,
  selectedId,
  loading,
  onConfigure,
  onTest,
  onToggle,
  onRemove,
  onOpenCodes,
}: {
  groups: ProviderGroup[];
  quotas?: AccountQuotaRow[];
  subscriptions?: AccountSubscription[];
  providers?: ModelProvider[];
  isAdmin: boolean;
  testingId: string | null;
  displayOf: (id: string) => string;
  teamName: (id: string | null) => string | null;
  selectedId?: string;
  loading: boolean;
  onConfigure: (connectionId: string) => void;
  onTest: (connectionId: string) => void;
  onToggle: (connection: Connection) => void;
  onRemove: (connection: Connection) => void;
  onOpenCodes: () => void;
}) {
  const [collapsed, setCollapsed] = useState<ReadonlySet<string>>(() => new Set());
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
  const lastPage = Math.max(1, Math.ceil(groups.length / size));
  const current = Math.min(page, lastPage);
  const visible = groups.slice((current - 1) * size, current * size);
  const previousTotal = useRef(groups.length);

  // A smaller list starts at the top; a page-size change does too.
  useEffect(() => {
    if (groups.length < previousTotal.current) setPage(1);
    previousTotal.current = groups.length;
  }, [groups.length]);
  useEffect(() => {
    setPage(1);
  }, [size]);

  const toggle = (key: string) =>
    setCollapsed((current) => {
      const next = new Set(current);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });

  return (
    <Card className="overflow-hidden">
      <CardHeader
        title="Connections"
        action={
          <span className="flex items-center gap-3">
            {isAdmin ? (
              <Button
                variant="quiet"
                size="sm"
                title="shorthand codes and operator-defined providers"
                onClick={onOpenCodes}
              >
                provider codes…
              </Button>
            ) : null}
            <span className="text-caption text-faint tnum">
              {groups.length} provider{groups.length === 1 ? "" : "s"} · {accountCount} account
              {accountCount === 1 ? "" : "s"}
            </span>
          </span>
        }
      />
      <div id="providers-connected">
        {loading ? (
          <InventorySkeleton />
        ) : (
          <ul className="divide-y divide-line border-t border-line">
            {visible.map((group) => {
              const open = !collapsed.has(group.key);
              return (
                <li key={group.key}>
                  <ProviderHeader group={group} open={open} onToggle={() => toggle(group.key)} />
                  {open ? (
                    <ul id={`accounts-${group.key}`} className="divide-y divide-line border-t border-line">
                      {group.connections.map((connection) => (
                        <AccountRow
                          key={connection.id}
                          connection={connection}
                          group={group}
                          providers={providers}
                          quota={quotaBy.get(connection.id)}
                          subscription={subscriptionBy.get(connection.id)}
                          isAdmin={isAdmin}
                          selected={selectedId === connection.id}
                          testing={testingId === connection.id}
                          teamName={teamName}
                          displayOf={displayOf}
                          onConfigure={onConfigure}
                          onTest={onTest}
                          onToggle={onToggle}
                          onRemove={onRemove}
                        />
                      ))}
                    </ul>
                  ) : null}
                </li>
              );
            })}
          </ul>
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
