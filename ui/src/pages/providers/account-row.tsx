import { useRef, type KeyboardEvent as ReactKeyboardEvent } from "react";
import * as Popover from "@radix-ui/react-popover";
import { DotsThree } from "@phosphor-icons/react";
import { ProviderIcon } from "../../components/provider-icon";
import { DataTable, type Column, type Pagination } from "../../components/table";
import { Badge, Button, Card, CardHeader, Dot, cx } from "../../components/ui";
import { type Connection, type ModelProvider } from "../../lib/api";
import { compact, usd } from "../../lib/format";
import {
  connectionLabel,
  credentialShapeOf,
  derivedIdentifiers,
  hostOf,
  isCustomConnection,
  needsBaseUrlOverride,
  protocolLabel,
  until,
  type ConnectionDrawerTab,
  type GroupRow,
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
  const derived = derivedIdentifiers(connection, display);
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

/** S0 multi-account provider summary header cell (`{kind: "provider"}`). */
export function ProviderGroupHeaderCell({ group }: { group: ProviderGroup }) {
  const counts = [
    `${group.connections.length} account${group.connections.length === 1 ? "" : "s"}`,
    `${group.enabled} enabled`,
    group.backoff > 0 ? `${group.backoff} in backoff` : null,
  ]
    .filter(Boolean)
    .join(" · ");
  return (
    <span
      className="provider-group-row flex min-w-0 items-center gap-2"
      title={`${group.label}${group.code ? ` · ${group.code}` : ""}: ${counts}`}
    >
      <ProviderIcon provider={group.provider ?? "custom"} size={17} />
      <span className="flex min-w-0 flex-col">
        <span className="flex min-w-0 items-center gap-1.5">
          <span className="min-w-0 break-words font-semibold sm:truncate">{group.label}</span>
          {group.code ? (
            <Badge tone="neutral" title="shorthand used in this provider's model refs">
              {group.code}
            </Badge>
          ) : null}
        </span>
        <span className="truncate text-caption text-faint tnum">{counts}</span>
      </span>
    </span>
  );
}

/** Single connection cell inside the provider column. */
export function ConnectionProviderCell({
  connection,
  display,
  grouped,
}: {
  connection: Connection;
  display: string;
  grouped: boolean;
}) {
  const custom = isCustomConnection(connection);
  // A custom endpoint is not OpenAI, so an unnamed one says so instead of claiming
  // the built-in provider; the operator's name replaces both once it exists.
  const label = custom ? (connection.name ?? `${display} (custom URL)`) : display;
  const protocol = custom ? protocolLabel(connection.provider) : null;
  const title = [label, protocol, connection.base_url_override].filter(Boolean).join(" · ");
  return (
    <span
      className={cx("flex min-w-0 items-center gap-2", grouped && "ml-4")}
      title={title}
    >
      <Dot
        tone={!connection.is_active ? "muted" : connection.backoff_level > 0 ? "warn" : "pos"}
      />
      <ProviderIcon provider={connection.provider} size={17} />
      <span className="flex min-w-0 flex-col">
        <span className="flex min-w-0 items-center gap-1.5">
          {/* Phones have no room for a 24-character provider name on one line; it
              wraps there and truncates only once the column is wide enough. */}
          <span className="min-w-0 break-words font-medium sm:truncate">{label}</span>
          {custom ? (
            <span className="shrink-0">
              <Badge
                tone="neutral"
                title="keyed account pointing at a custom base URL, not the provider's own API"
              >
                custom
              </Badge>
            </span>
          ) : null}
        </span>
        {protocol ? (
          <span className="truncate text-caption text-faint">{protocol}</span>
        ) : null}
      </span>
    </span>
  );
}

/** Health, backoff, and active/disabled state badges for provider groups and account rows. */
export function ConnectionStateBadges({
  row,
  providers,
}: {
  row: GroupRow;
  providers?: ModelProvider[];
}) {
  if (row.kind === "provider") {
    const { group } = row;
    return (
      <span className="flex flex-wrap gap-1">
        <Badge
          tone={group.state === "healthy" ? "pos" : group.state === "degraded" ? "warn" : "neg"}
          title={[
            "healthy: an enabled account is eligible",
            "degraded: an eligible account is in backoff, over quota or at 90%+ of a plan window",
            "down: every account is disabled, exhausted or in backoff",
          ].join(" · ")}
        >
          {group.state}
        </Badge>
        {group.overQuota > 0 ? (
          <Badge tone="neg" title="accounts over their operator quota; the router skips them">
            <span className="tnum">{group.overQuota} over quota</span>
          </Badge>
        ) : null}
      </span>
    );
  }
  const c = row.connection;
  return (
    <span className="flex flex-wrap gap-1">
      {needsBaseUrlOverride(c, providers) ? (
        <Badge tone="warn" title="cannot route until this account names its resource base URL">
          base url
        </Badge>
      ) : null}
      {c.backoff_level > 0 ? (
        <Badge tone="warn">
          <span className="tnum">backoff {c.backoff_level}</span>
        </Badge>
      ) : c.is_active ? (
        <Badge tone="pos">active</Badge>
      ) : (
        <Badge tone="neutral">disabled</Badge>
      )}
    </span>
  );
}

/**
 * Every per-row action behind one trigger.
 *
 * The menu keeps one 32px target, exposes direct tab jumps into the unified
 * `ConnectionDrawer`, and carries full menu keyboard semantics (arrows, Home/End,
 * Escape from the popover).
 */
export function ConnectionMenu({
  connection,
  display,
  scope,
  testing,
  onInspect,
  onTest,
  onToggle,
  onRemove,
}: {
  connection: Connection;
  display: string;
  /** Owner scope, priority and weight live in columns that collapse on narrow screens. */
  scope: string;
  testing: boolean;
  onInspect: (tab?: ConnectionDrawerTab) => void;
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
          {entry("Inspect & configure…", () => onInspect("overview"))}
          {entry("Routing & team…", () => onInspect("routing"))}
          {entry("Quotas & auto-ping…", () => onInspect("quotas"))}
          <div className="my-1 border-t border-line" role="separator" />
          {entry(testing ? "Testing…" : "Test", onTest, false, testing)}
          {entry(connection.is_active ? "Disable" : "Enable", onToggle)}
          {entry("Remove…", onRemove, true)}
        </Popover.Content>
      </Popover.Portal>
    </Popover.Root>
  );
}

/** Build DataTable column definitions for S0 provider summary headers and account rows. */
export function buildProviderColumns({
  providers,
  isAdmin,
  testingId,
  displayOf,
  teamName,
  onInspect,
  onTest,
  onToggle,
  onRemove,
}: {
  providers?: ModelProvider[];
  isAdmin: boolean;
  testingId: string | null;
  displayOf: (id: string) => string;
  teamName: (id: string | null) => string | null;
  onInspect: (connectionId: string, tab?: ConnectionDrawerTab) => void;
  onTest: (connectionId: string) => void;
  onToggle: (connection: Connection) => void;
  onRemove: (connection: Connection) => void;
}): Array<Column<GroupRow>> {
  return [
    {
      key: "provider",
      title: "provider",
      render: (r) =>
        r.kind === "provider" ? (
          <ProviderGroupHeaderCell group={r.group} />
        ) : (
          <ConnectionProviderCell
            connection={r.connection}
            display={displayOf(r.connection.provider)}
            grouped={r.grouped}
          />
        ),
    },
    {
      key: "owner",
      title: "owner",
      width: "9rem",
      className: "hidden xl:table-cell",
      render: (r) => {
        if (r.kind === "provider") {
          return (
            <span
              className="text-caption text-faint tnum"
              title="priority tiers: how many of this provider's accounts sit at each"
            >
              {r.group.tiers.map((tier) => `p${tier.priority} ×${tier.count}`).join(" · ")}
            </span>
          );
        }
        const name = teamName(r.connection.team_id);
        return name === null ? (
          <Badge
            tone="neutral"
            title="shared with the instance; a team key cannot use a shared account"
          >
            shared
          </Badge>
        ) : (
          <Badge tone="accent" title={`owned by team ${name}: only that team's keys route here`}>
            {name}
          </Badge>
        );
      },
    },
    {
      key: "account",
      title: "account",
      render: (r) => {
        if (r.kind === "provider") {
          const spend = r.group.spend;
          // The quota list is admin-only; a reader sees the counts and nothing invented.
          if (!spend) return null;
          return (
            <span
              className="text-caption text-faint tnum"
              title="current quota window, summed over this provider's accounts"
            >
              {usd(spend.cost_micros)} · {compact(spend.tokens)} tok · {compact(spend.requests)} req
            </span>
          );
        }
        return (
          <AccountCell
            connection={r.connection}
            display={displayOf(r.connection.provider)}
            custom={isCustomConnection(r.connection)}
          />
        );
      },
    },
    {
      key: "auth",
      title: "auth",
      width: "6.5rem",
      className: "hidden xl:table-cell",
      render: (r) => {
        if (r.kind === "provider") {
          const subscription = r.group.subscription;
          if (!subscription) return null;
          return (
            <span
              className="text-caption text-faint tnum"
              title={`worst subscription window in this provider · ${subscription.reporting} of ${r.group.connections.length} accounts reporting plan limits`}
            >
              {subscription.percent}%
              {subscription.resets_at ? ` · ${until(subscription.resets_at)}` : ""}
            </span>
          );
        }
        const c = r.connection;
        return (
          <Badge
            tone={c.auth_type === "oauth" ? "accent" : "neutral"}
            title={credentialShapeOf(c)}
          >
            {c.auth_type}
          </Badge>
        );
      },
    },
    {
      key: "priority",
      title: "priority",
      align: "right",
      width: "5rem",
      className: "hidden xl:table-cell",
      render: (r) =>
        r.kind === "provider" ? null : <span className="tnum">{r.connection.priority}</span>,
    },
    {
      key: "weight",
      title: "weight",
      align: "right",
      width: "5rem",
      className: "hidden xl:table-cell",
      render: (r) => {
        if (r.kind === "provider") return null;
        const c = r.connection;
        return (
          <span
            className={cx("tnum", c.weight === 100 ? "text-faint" : "text-fg")}
            title="traffic share inside this priority group"
          >
            {c.weight}
          </span>
        );
      },
    },
    {
      key: "state",
      title: "state",
      width: "7rem",
      className: "hidden sm:table-cell",
      render: (r) => <ConnectionStateBadges row={r} providers={providers} />,
    },
    // Mutations are admin-only at the API gate, so a reader gets no controls at all.
    ...(isAdmin
      ? [
          {
            key: "actions",
            title: "actions",
            srOnlyTitle: true,
            align: "right",
            width: "3.5rem",
            render: (r) => {
              if (r.kind === "provider") return null;
              const c = r.connection;
              return (
                <ConnectionMenu
                  connection={c}
                  display={displayOf(c.provider)}
                  scope={teamName(c.team_id) ?? "shared"}
                  testing={testingId === c.id}
                  onInspect={(tab) => onInspect(c.id, tab)}
                  onTest={() => onTest(c.id)}
                  onToggle={() => onToggle(c)}
                  onRemove={() => onRemove(c)}
                />
              );
            },
          } as Column<GroupRow>,
        ]
      : []),
  ];
}

/** Connected accounts table card with S0 provider summary header row tinting. */
export function ConnectedAccountsCard({
  columns,
  rows,
  groupCount,
  accountCount,
  selectedId,
  loading,
  isAdmin,
  pagination,
  onOpenCodes,
}: {
  columns: Array<Column<GroupRow>>;
  rows: GroupRow[];
  groupCount: number;
  accountCount: number;
  selectedId?: string;
  loading: boolean;
  isAdmin: boolean;
  pagination: Pagination;
  onOpenCodes: () => void;
}) {
  return (
    <Card>
      <CardHeader
        title="Connected"
        action={
          <span className="flex items-center gap-3">
            {isAdmin ? (
              <Button
                variant="quiet"
                title="shorthand codes and operator-defined providers"
                onClick={onOpenCodes}
              >
                provider codes…
              </Button>
            ) : null}
            <span className="text-caption text-faint tnum">
              {groupCount} endpoint{groupCount === 1 ? "" : "s"} · {accountCount} account
              {accountCount === 1 ? "" : "s"}
            </span>
          </span>
        }
      />
      {/* The header row marks itself; tint it so it reads as a divider, not an account. */}
      <div
        id="providers-connected"
        className="[&_tr:has(.provider-group-row)>td]:bg-[var(--color-surface-2)]"
      >
        <DataTable
          columns={columns}
          rows={loading ? undefined : rows}
          rowKey={(r) => (r.kind === "provider" ? `provider:${r.group.key}` : r.connection.id)}
          selectedKey={selectedId}
          loading={loading}
          tableLayout="fixed"
          stickyRight="actions"
          pagination={pagination}
          empty="No accounts yet."
        />
      </div>
    </Card>
  );
}
