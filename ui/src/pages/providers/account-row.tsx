import { useMemo, useRef, useState, type KeyboardEvent as ReactKeyboardEvent } from "react";
import { Link } from "react-router-dom";
import * as Popover from "@radix-ui/react-popover";
import { CaretRight, DotsThree } from "@phosphor-icons/react";
import { ProviderIcon } from "../../components/provider-icon";
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
 * The account column: the operator's name first, then the identifiers that name the
 * upstream account itself. Clearing the name leaves those lines alone, so a row is never
 * blank: the first identifier becomes the primary line.
 *
 * A custom endpoint's name is the provider's name, already on screen in its own cell, so
 * repeating it here would print the same text twice; the host names the account instead.
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
  const secondary = derived.filter((part) => part.text !== primary);
  const title = [primary, ...secondary.map((part) => part.text)].filter(Boolean).join(" · ");
  return (
    <div className="min-w-0">
      <span
        className={cx("block min-w-0 truncate", name ? "text-fg" : "text-muted")}
        title={title || undefined}
      >
        {primary ?? <span className="text-faint">unnamed</span>}
      </span>
      {secondary.map((part) => (
        <div
          key={part.text}
          className={cx("truncate text-caption text-faint", part.mono && "font-mono tnum")}
          title={part.text}
        >
          {part.text}
        </div>
      ))}
      {host !== null && host !== primary ? (
        <div
          className="truncate font-mono text-caption text-faint"
          title={connection.base_url_override ?? undefined}
        >
          {host}
        </div>
      ) : null}
    </div>
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

/** Thin magnitude bar + the number, linking out to the aggregate surface that owns the detail. */
function QuotaSummary({
  quota,
  subscription,
  providerId,
}: {
  quota?: AccountQuotaRow;
  subscription?: AccountSubscription;
  providerId: string;
}) {
  const quotaPercent = quotaUsagePercent(quota);
  const planPercent = subscriptionUsagePercent(subscription);
  const percent = quotaPercent ?? planPercent;
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
  const label =
    quotaPercent !== null ? "quota used" : worstWindow ? `plan · ${worstWindow.label}` : "usage";
  return (
    <Link
      to={`/usage?provider=${encodeURIComponent(providerId)}`}
      title={detail || "no quota or plan limit reported for this account"}
      className="flex min-w-0 flex-col gap-1 rounded-[var(--radius-input)] focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--color-focus-ring)]"
    >
      <span className="flex items-center gap-2">
        <span className="h-1 min-w-0 flex-1 overflow-hidden rounded-full bg-rule" aria-hidden>
          {percent === null ? null : (
            <span
              className={cx("block h-full rounded-full", barFill(percent))}
              style={{ width: `${percent}%` }}
            />
          )}
        </span>
        <span
          className={cx(
            "w-9 shrink-0 text-right text-caption tnum",
            percent === null ? "text-faint" : barText(percent),
          )}
        >
          {percent === null ? "—" : `${percent}%`}
        </span>
      </span>
      <span className="truncate text-caption text-faint">{label}</span>
    </Link>
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
          className="flex h-8 w-8 items-center justify-center rounded-full text-muted transition-colors hover:bg-surface-3 hover:text-fg focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--color-focus-ring)]"
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
  const planPercent = subscriptionUsagePercent(subscription);
  const state = stateOf(connection, quota, planPercent, providers);
  const label = connectionLabel(connection, display);
  const auth = connection.auth_type === "oauth" ? "oauth" : "api key";
  return (
    <li
      className={cx(
        "flex flex-wrap items-center gap-x-4 gap-y-2 px-4 py-2.5",
        selected && "bg-[var(--color-selected)]",
      )}
    >
      <div className="flex min-w-0 flex-[1_1_16rem] items-center gap-2">
        <ProviderIcon provider={connection.provider} size={17} />
        <div className="min-w-0 flex-1">
          <AccountCell connection={connection} display={display} custom={custom} />
          {custom ? (
            <span className="mt-0.5 flex flex-wrap items-center gap-1.5 text-caption text-faint">
              <span>{protocolLabel(connection.provider)}</span>
              <Badge
                tone="neutral"
                title="keyed account pointing at a custom base URL, not the provider's own API"
              >
                custom
              </Badge>
            </span>
          ) : null}
        </div>
      </div>

      <span
        className="hidden w-14 shrink-0 text-right text-caption text-faint sm:block"
        title={credentialShapeOf(connection)}
      >
        {auth}
      </span>

      <button
        type="button"
        onClick={() => onConfigure(connection.id)}
        aria-label={`Routing for ${label}: priority ${connection.priority}, weight ${connection.weight}`}
        title="priority · weight inside one priority group (lower priority is tried first) — open routing"
        className="hidden w-20 shrink-0 text-right text-caption text-muted tnum underline-offset-2 hover:text-fg hover:underline focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--color-focus-ring)] md:block"
      >
        p{connection.priority} · w{connection.weight}
      </button>

      <div className="w-28 shrink-0 sm:w-32">
        <QuotaSummary quota={quota} subscription={subscription} providerId={providerId} />
      </div>

      <span className="flex w-[6.5rem] shrink-0 items-center gap-1.5 text-caption">
        <Dot tone={DOT_TONE[state.tone]} />
        <span className={cx("truncate", STATE_TEXT[state.tone])} title={`${label}: ${state.label}`}>
          {state.label}
        </span>
      </span>

      <div className="ml-auto flex shrink-0 items-center gap-1">
        <Button variant="quiet" size="sm" onClick={() => onConfigure(connection.id)}>
          {isAdmin ? "Configure" : "Details"}
        </Button>
        {isAdmin ? (
          <ConnectionMenu
            connection={connection}
            display={display}
            scope={teamName(connection.team_id) ?? "shared"}
            testing={testing}
            onTest={() => onTest(connection.id)}
            onToggle={() => onToggle(connection)}
            onRemove={() => onRemove(connection)}
          />
        ) : null}
      </div>
    </li>
  );
}

/** Provider header: identity, one status readout, counts, and the link to aggregate usage. */
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
    <div className="flex items-center gap-2 px-4 py-2.5">
      <button
        type="button"
        aria-expanded={open}
        aria-controls={`accounts-${group.key}`}
        onClick={onToggle}
        className="flex min-w-0 flex-1 items-center gap-2 rounded-[var(--radius-input)] text-left focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--color-focus-ring)]"
      >
        <CaretRight
          size={12}
          weight="bold"
          aria-hidden
          className={cx("shrink-0 text-faint transition-transform", open && "rotate-90")}
        />
        <ProviderIcon provider={group.provider ?? "custom"} size={18} />
        <span className="min-w-0 break-words font-medium text-fg sm:truncate">{group.label}</span>
        {group.code ? (
          <Badge tone="neutral" title="shorthand used in this provider's model refs">
            {group.code}
          </Badge>
        ) : null}
        <span className="flex shrink-0 items-center gap-1.5 text-caption">
          <Dot tone={stateTone === "muted" ? "muted" : stateTone} />
          <span className={STATE_TEXT[stateTone]}>{group.state}</span>
        </span>
      </button>
      <span className="hidden shrink-0 text-caption text-faint tnum sm:block">{counts}</span>
      <Link
        to={`/usage?provider=${encodeURIComponent(providerId)}`}
        className="shrink-0 text-caption text-muted underline underline-offset-2 hover:text-fg"
      >
        usage
      </Link>
    </div>
  );
}

/**
 * The inventory: one section per provider, its accounts underneath, all expanded until the
 * operator folds one away. No pagination — the provider headers are the structure, and a
 * page boundary inside a provider would split a header from its own accounts.
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
  const quotaBy = useMemo(
    () => new Map((quotas ?? []).map((row) => [row.connection_id, row])),
    [quotas],
  );
  const subscriptionBy = useMemo(
    () => new Map((subscriptions ?? []).map((row) => [row.connection_id, row])),
    [subscriptions],
  );
  const accountCount = groups.reduce((total, group) => total + group.connections.length, 0);
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
          <p className="border-t border-line px-4 py-4 text-small text-muted">Loading accounts…</p>
        ) : (
          <ul className="divide-y divide-line border-t border-line">
            {groups.map((group) => {
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
      </div>
    </Card>
  );
}
