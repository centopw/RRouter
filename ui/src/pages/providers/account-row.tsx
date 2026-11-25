import { useEffect, useMemo, useRef, useState, type KeyboardEvent as ReactKeyboardEvent } from "react";
import { Link } from "react-router-dom";
import * as Popover from "@radix-ui/react-popover";
import {
  CaretDown,
  CheckCircle,
  DotsThree,
  MinusCircle,
  Plus,
  SlidersHorizontal,
  WarningCircle,
} from "@phosphor-icons/react";
import { HealthBadge } from "../../components/molecules/HealthBadge";
import { KpiTile, type KpiTone } from "../../components/molecules/KpiTile";
import { ProviderIcon } from "../../components/provider-icon";
import { PaginationBar } from "../../components/table";
import {
  Badge,
  Button,
  Card,
  Dot,
  Meter,
  barText,
  cx,
} from "../../components/ui";
import {
  type AccountQuotaRow,
  type AccountSubscription,
  type Connection,
  type ModelProvider,
  type OAuthProvider,
} from "../../lib/api";
import { compact, ms } from "../../lib/format";
import { refusalView } from "../../lib/refusal";
import { accountRisk } from "../usage/pace";
import { quotaBars, resetsIn } from "../usage/quota";
import { AccountNameLink } from "./account-preview";
import {
  accountSentence,
  accountStatus,
  connectionLabel,
  derivedIdentifiers,
  hostOf,
  isCustomConnection,
  protocolLabel,
  providerOptions,
  providerSentence,
  type AccountStatus,
  type AccountTraffic24h,
  type ConnectionTestResult,
  type ProviderFilter,
  type ProviderGroup,
  type ProvidersSummary,
} from "./types";

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
 * The top strip answers "are my providers OK?" in four counts: connected (every provider),
 * healthy (every account ready), need attention (an account needs an operator) and
 * disabled (every account switched off). Each tile doubles as the filter for the cards.
 */
export function ProvidersKpiRow({
  summary,
  healthyProviders,
  attentionProviders,
  disabledProviders,
  connectedProviders,
  filter,
  onFilter,
}: {
  summary: ProvidersSummary;
  connectedProviders: number;
  healthyProviders: number;
  attentionProviders: number;
  disabledProviders: number;
  filter: ProviderFilter;
  onFilter: (next: ProviderFilter) => void;
}) {
  const overall = providerVerdict(summary);
  const readyPercent =
    summary.expected > 0 ? (summary.ready / summary.expected) * 100 : summary.disabled > 0 ? 0 : 100;
  const readyTone: KpiTone =
    summary.expected === 0
      ? "neutral"
      : summary.ready === 0
        ? "neg"
        : summary.ready < summary.expected
          ? "warn"
          : "pos";

  const toggle = (id: ProviderFilter) => onFilter(filter === id ? "connected" : id);

  return (
    <div className="mb-4 flex flex-col gap-2.5">
      <div
        className={cx(
          "flex items-center gap-2 rounded-[var(--radius-surface)] border border-line bg-surface-2/60 px-3 py-2 text-small",
          VERDICT_TEXT[overall.tone],
        )}
      >
        <Dot tone={overall.dot} />
        <span className="min-w-0 flex-1">{overall.sentence}</span>
        <span className="hidden shrink-0 text-caption text-faint tnum sm:block">
          {summary.traffic24h.requests > 0
            ? `${compact(summary.traffic24h.requests)} requests · ${summary.traffic24h.errorRate > 0 ? `${summary.traffic24h.errorRate.toFixed(1)}% errors` : "no errors"} (24h)`
            : "No traffic in 24h"}
        </span>
      </div>
      <div className="grid grid-cols-2 gap-2.5 sm:gap-3 xl:grid-cols-4">
        <KpiTile
          icon={<CheckCircle size={18} weight="fill" />}
          tone="info"
          label="Connected"
          value={connectedProviders}
          suffix={connectedProviders === 1 ? "provider" : "providers"}
          detail={
            summary.total === 0
              ? "Nothing connected"
              : `${summary.total} account${summary.total === 1 ? "" : "s"}`
          }
          onClick={() => toggle("connected")}
          active={filter === "connected"}
        />
        <KpiTile
          icon={<CheckCircle size={18} weight="fill" />}
          tone={readyTone}
          label="Healthy"
          value={healthyProviders}
          suffix={healthyProviders === 1 ? "provider" : "providers"}
          meter={<Meter percent={readyPercent} label={`${Math.round(readyPercent)}% of accounts ready`} />}
          detail={
            summary.expected === 0
              ? "No enabled accounts"
              : `${summary.ready} of ${summary.expected} account${summary.expected === 1 ? "" : "s"} ready`
          }
          onClick={() => toggle("healthy")}
          active={filter === "healthy"}
        />
        <KpiTile
          icon={<WarningCircle size={18} weight="fill" />}
          tone={summary.attention === 0 ? "pos" : summary.severe > 0 ? "neg" : "warn"}
          label="Need attention"
          value={attentionProviders}
          suffix={attentionProviders === 1 ? "provider" : "providers"}
          detail={
            summary.attention === 0
              ? "Everything is routing"
              : `${summary.attention} account${summary.attention === 1 ? "" : "s"} · ${summary.attentionDetail}`
          }
          onClick={() => toggle("attention")}
          active={filter === "attention"}
        />
        <KpiTile
          icon={<MinusCircle size={18} weight="fill" />}
          tone="neutral"
          label="Disabled"
          value={disabledProviders}
          suffix={disabledProviders === 1 ? "provider" : "providers"}
          detail={
            summary.disabled === 0
              ? "None turned off"
              : `${summary.disabled} account${summary.disabled === 1 ? "" : "s"} off`
          }
          onClick={() => toggle("disabled")}
          active={filter === "disabled"}
        />
      </div>
    </div>
  );
}

const VERDICT_TEXT: Record<"pos" | "warn" | "neg" | "muted", string> = {
  pos: "text-fg",
  warn: "text-[var(--color-warn-ink)]",
  neg: "text-[var(--color-neg-ink)]",
  muted: "text-muted",
};

/** One sentence a non-expert can act on, built from the same counts as the tiles. */
function providerVerdict(summary: ProvidersSummary): {
  sentence: string;
  tone: "pos" | "warn" | "neg" | "muted";
  dot: "pos" | "warn" | "neg" | "muted";
} {
  if (summary.total === 0) {
    return { sentence: "No provider accounts yet.", tone: "muted", dot: "muted" };
  }
  if (summary.expected === 0) {
    return {
      sentence: `Every account is turned off (${summary.disabled}) — nothing can route.`,
      tone: "muted",
      dot: "muted",
    };
  }
  if (summary.attention === 0) {
    return {
      sentence: "All providers are routing. Nothing needs your attention.",
      tone: "pos",
      dot: "pos",
    };
  }
  if (summary.ready === 0) {
    return {
      sentence: `Nothing can route: all ${summary.expected} enabled accounts need attention.`,
      tone: "neg",
      dot: "neg",
    };
  }
  return {
    sentence: `${summary.attention} of ${summary.expected} enabled accounts need attention; the rest still route.`,
    tone: summary.severe > 0 ? "neg" : "warn",
    dot: summary.severe > 0 ? "neg" : "warn",
  };
}

/**
 * Every per-account action behind one trigger: configuration, test, enable/disable and
 * remove. The trigger is labelled with the account name for screen readers.
 */
function ConnectionMenu({
  connection,
  display,
  scope,
  testing,
  onConfigure,
  onTest,
  onToggle,
  onRemove,
}: {
  connection: Connection;
  display: string;
  scope: string;
  testing: boolean;
  onConfigure: () => void;
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
          <div className="px-2 pb-1.5 text-caption leading-relaxed text-faint">{display}</div>
          {entry("Configure…", onConfigure)}
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

/**
 * Every limit bar for an account, labelled in words. Shares `quotaBars` with Usage so the
 * two surfaces can never disagree about which window is tightest.
 */
function LimitBars({
  quota,
  subscription,
  quotaLoaded,
}: {
  quota?: AccountQuotaRow;
  subscription?: AccountSubscription;
  quotaLoaded: boolean;
}) {
  const bars = quotaBars(quota, subscription);
  if (bars.length > 0) {
    return (
      <ul className="flex min-w-0 flex-col gap-1.5">
        {bars.map((bar) => {
          const reset = resetsIn(bar.resets_at);
          return (
            <li key={bar.id} className="flex min-w-0 flex-col gap-1">
              <span className="flex min-w-0 items-baseline justify-between gap-2 text-caption">
                <span className="min-w-0 truncate text-muted">
                  {bar.label}
                  {reset ? <span className="text-faint"> · {reset}</span> : null}
                </span>
                <span className={cx("shrink-0 tnum", barText(bar.percent))}>{bar.detail}</span>
              </span>
              <Meter percent={bar.percent} label={reset ? `${bar.label}, ${reset}` : bar.label} />
            </li>
          );
        })}
      </ul>
    );
  }
  if (subscription?.error) {
    return <span className="text-caption text-faint">Limits unavailable — the provider did not answer.</span>;
  }
  if (!quotaLoaded) {
    return (
      <span className="text-caption text-faint" aria-label="Limits unavailable for this role">
        Limits hidden for this role
      </span>
    );
  }
  return <span className="text-caption text-faint">No limit set — this account is uncapped.</span>;
}

/**
 * "How soon does it run out?" for a subscription account, from the same pace model Usage
 * uses. Only the windows projected to hit their limit are named.
 */
function PaceNote({ subscription }: { subscription?: AccountSubscription }) {
  if (!subscription || subscription.windows.length === 0) return null;
  const risk = accountRisk(subscription, Date.now());
  const alarming = (risk?.windows ?? []).filter(
    (entry) => entry.pace.state === "on_pace_to_hit" || entry.pace.state === "exhausted",
  );
  if (alarming.length === 0) return null;
  return (
    <li className="text-caption text-[var(--color-warn)]">
      At this pace,{" "}
      {alarming
        .map((entry) => `${entry.window.label} ${entry.pace.state === "exhausted" ? "is already used up" : "runs out before it resets"}`)
        .join("; ")}
      .
    </li>
  );
}

/** Status pill, or the live test result from this session when the operator just tested. */
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
 * One account inside an expanded provider card. No cryptic columns: the name, one plain
 * sentence ("Ready · 920 requests in 24h"), barred limits in words, and the actions menu.
 */
function AccountRow({
  connection,
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
  onConfigure,
  onTest,
  onToggle,
  onRemove,
}: {
  connection: Connection;
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
  onConfigure: () => void;
  onTest: (connectionId: string) => void;
  onToggle: (connection: Connection) => void;
  onRemove: (connection: Connection) => void;
}) {
  const custom = isCustomConnection(connection);
  const display = displayOf(connection.provider);
  const status = accountStatus(connection, quota, subscription, providers);
  const ownerLabel = teamName(connection.team_id) ?? "Shared";
  const lastError = connection.last_error?.trim() || null;
  const requests24h = traffic?.requests ?? 0;
  const errors24h = traffic?.errors ?? 0;
  const sentence = accountSentence(status, traffic);

  return (
    <li
      className={cx(
        "flex flex-col gap-2 rounded-[var(--radius-input)] px-2 py-2 transition-colors hover:bg-[var(--color-row-hover)]",
        status.kind === "disabled" && "opacity-75",
        selected && "bg-[var(--color-selected)] hover:bg-[var(--color-selected)]",
      )}
    >
      <div className="flex min-w-0 items-center justify-between gap-2">
        <span className="flex min-w-0 flex-1 flex-wrap items-center gap-x-2 gap-y-1">
          <ProviderIcon provider={connection.provider} size={16} />
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
          <span className="shrink-0 text-caption text-faint">
            {connection.team_id ? `${ownerLabel} team` : "Shared"}
            {requests24h > 0
              ? ` · ${compact(requests24h)} req/24h${errors24h > 0 ? ` · ${compact(errors24h)} errors` : ""}`
              : " · idle 24h"}
          </span>
        </span>
        <span className="flex shrink-0 items-center gap-1.5">
          <StatusCell connection={connection} status={status} testing={testing} lastTest={lastTest} />
          {isAdmin ? (
            <ConnectionMenu
              connection={connection}
              display={display}
              scope={ownerLabel}
              testing={testing}
              onConfigure={onConfigure}
              onTest={() => onTest(connection.id)}
              onToggle={() => onToggle(connection)}
              onRemove={() => onRemove(connection)}
            />
          ) : null}
        </span>
      </div>

      <p className="min-w-0 text-caption text-muted [overflow-wrap:anywhere]">{sentence}</p>

      {lastError ? (
        <Link
          to={inspectHref}
          title={lastError}
          className="flex min-w-0 items-baseline gap-1.5 rounded-[var(--radius-input)] text-caption text-[var(--color-neg-ink)] underline-offset-2 hover:underline focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--color-focus-ring)]"
        >
          <span aria-hidden className="shrink-0 text-[var(--color-neg)]">
            ↳
          </span>
          <span className="min-w-0 truncate font-mono">{lastError}</span>
        </Link>
      ) : null}

      <div className="grid min-w-0 gap-2.5 border-t border-rule pt-2 sm:grid-cols-2">
        <div className="min-w-0">
          <span className="mb-1 block text-caption font-medium text-ink-soft">Limits</span>
          <LimitBars quota={quota} subscription={subscription} quotaLoaded={quotaLoaded} />
          <PaceNote subscription={subscription} />
        </div>
        <dl className="grid min-w-0 grid-cols-2 gap-x-3 gap-y-1.5 text-caption">
          <div className="min-w-0">
            <dt className="text-faint">Used by</dt>
            <dd className="truncate text-fg">{connection.team_id ? `${ownerLabel} team` : "Shared"}</dd>
          </div>
          <div className="min-w-0">
            <dt className="text-faint">Priority</dt>
            <dd className="truncate text-fg tnum" title="Lower numbers are tried first; equal priorities share traffic.">
              {connection.priority}
            </dd>
          </div>
          <div className="min-w-0">
            <dt className="text-faint">Traffic share</dt>
            <dd
              className="truncate text-fg tnum"
              title="Share of traffic among accounts with equal priority. Only used by the weighted strategy."
            >
              {connection.weight}
            </dd>
          </div>
          <div className="min-w-0">
            <dt className="text-faint">Endpoint</dt>
            <dd className="truncate text-fg" title={connection.base_url_override ?? undefined}>
              {custom && connection.base_url_override
                ? hostOf(connection.base_url_override)
                : "Provider default"}
            </dd>
          </div>
        </dl>
      </div>
    </li>
  );
}

/**
 * One provider, one card: logo, name, a plain-language sentence, and the two actions a
 * reader needs (Configure, Usage). Accounts live inside, collapsed behind a labelled
 * disclosure that says how many are there, and never a subset of what the header counted.
 */
function ProviderCard({
  group,
  expanded,
  onToggleExpanded,
  quotas,
  subscriptions,
  quotaLoaded,
  trafficById,
  providers,
  oauth,
  isAdmin,
  testingId,
  testsById,
  inspectHrefFor,
  displayOf,
  teamName,
  selectedId,
  onConfigure,
  onAdd,
  onTest,
  onToggle,
  onRemove,
}: {
  group: ProviderGroup;
  expanded: boolean;
  onToggleExpanded: () => void;
  quotas?: AccountQuotaRow[];
  subscriptions?: AccountSubscription[];
  quotaLoaded: boolean;
  trafficById: ReadonlyMap<string, AccountTraffic24h>;
  providers?: ModelProvider[];
  oauth?: OAuthProvider[];
  isAdmin: boolean;
  testingId: string | null;
  testsById: Record<string, ConnectionTestResult>;
  inspectHrefFor: (connectionId: string) => string;
  displayOf: (id: string) => string;
  teamName: (id: string | null) => string | null;
  selectedId?: string;
  onConfigure: (connection: Connection) => void;
  onAdd: (provider: string) => void;
  onTest: (connectionId: string) => void;
  onToggle: (connection: Connection) => void;
  onRemove: (connection: Connection) => void;
}) {
  const quotaBy = useMemo(
    () => new Map((quotas ?? []).map((row) => [row.connection_id, row])),
    [quotas],
  );
  const subscriptionBy = useMemo(
    () => new Map((subscriptions ?? []).map((row) => [row.connection_id, row])),
    [subscriptions],
  );
  const providerId = group.provider ?? group.connections[0]?.provider ?? "";
  const custom = group.key.startsWith("custom:");
  const sentence = providerSentence(group, isAdmin);
  const accounts = group.connections.length;
  /** Configure edits the first account when there is only one; otherwise the reader picks. */
  const primary = group.connections.find((account) => account.is_active) ?? group.connections[0];
  const addable = providerOptions(
    providers?.find((row) => row.id === providerId),
    oauth?.find((row) => row.id === providerId),
  );
  const mixedState = group.state !== "healthy" && group.state !== "disabled";
  const addLabel = addable[0]?.kind === "oauth" ? "Add account" : "Add API key";
  const addTitle =
    addable.length === 0
      ? "This provider is not listed in this build's catalogue."
      : addable[0]!.kind === "oauth"
        ? `Sign in to another ${addable[0]!.display} account.`
        : `Add another ${addable[0]!.display} API key.`;
  const iconBase = group.connections.find((c) => c.base_url_override)?.base_url_override;

  return (
    <li
      className={cx(
        "rounded-[var(--radius-card)] border bg-surface transition-colors",
        mixedState && group.state !== "disabled"
          ? "border-[var(--color-line-strong)]"
          : "border-line",
      )}
    >
      <div className="flex flex-wrap items-start gap-x-3 gap-y-2 px-3 py-2.5 sm:px-4">
        <span className="flex min-w-0 flex-1 items-start gap-3">
          <span className="mt-0.5 flex h-8 w-8 shrink-0 items-center justify-center rounded-[10px] bg-surface-2">
            <ProviderIcon
              provider={group.provider ?? "custom"}
              size={20}
              baseUrl={custom ? iconBase : undefined}
            />
          </span>
          <span className="min-w-0 flex-1">
            <span className="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1">
              <Link
                to={`/models?provider=${encodeURIComponent(providerId)}`}
                className="min-w-0 truncate font-medium text-fg underline-offset-2 hover:text-[var(--color-accent-text)] hover:underline focus-visible:text-[var(--color-accent-text)] focus-visible:underline focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--color-focus-ring)]"
                title="Models this provider serves"
              >
                {group.label}
              </Link>
              {custom ? <Badge tone="neutral">custom endpoint</Badge> : null}
              {group.code && !custom ? (
                <Badge tone="neutral" title="Shorthand used in model references">
                  {group.code}
                </Badge>
              ) : null}
            </span>
            <span
              className={cx("mt-0.5 block text-small [overflow-wrap:anywhere]", SENTENCE_TEXT[sentence.tone])}
            >
              {sentence.text}
            </span>
          </span>
        </span>

        <span className="flex shrink-0 flex-wrap items-center gap-1.5">
          {isAdmin && primary ? (
            <Button variant="secondary" size="sm" onClick={() => onConfigure(primary)}>
              Configure
            </Button>
          ) : null}
          <Link
            to={`/usage?provider=${encodeURIComponent(providerId)}`}
            className="inline-flex h-7 items-center rounded-[var(--radius-input)] px-2 text-small text-accent-text underline-offset-2 hover:underline focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--color-focus-ring)]"
          >
            Usage
          </Link>
          {isAdmin ? (
            <Button
              variant="quiet"
              size="sm"
              onClick={() => onAdd(providerId)}
              disabled={addable.length === 0}
              title={addTitle}
            >
              <span className="flex items-center gap-1">
                <Plus size={13} aria-hidden />
                {addLabel}
              </span>
            </Button>
          ) : null}
        </span>
      </div>

      <div className="border-t border-rule px-3 sm:px-4">
        <button
          type="button"
          aria-expanded={expanded}
          aria-controls={`accounts-${group.key}`}
          onClick={onToggleExpanded}
          className="flex w-full items-center gap-2 rounded-[var(--radius-input)] py-1.5 text-left text-caption text-muted transition-colors hover:text-fg focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--color-focus-ring)]"
        >
          <CaretDown
            size={12}
            aria-hidden
            className={cx("shrink-0 text-faint transition-transform", expanded && "rotate-180")}
          />
          <span className="tnum">
            {accounts} account{accounts === 1 ? "" : "s"}
          </span>
          {group.disabled > 0 ? <span className="tnum">· {group.disabled} off</span> : null}
          <span className="ml-auto text-faint">{expanded ? "Hide" : "Show"}</span>
        </button>
      </div>

      {expanded ? (
        <ul
          id={`accounts-${group.key}`}
          className="flex flex-col divide-y divide-rule border-t border-rule px-1.5 py-1.5"
        >
          {group.connections.map((connection) => (
            <AccountRow
              key={connection.id}
              connection={connection}
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
              onConfigure={() => onConfigure(connection)}
              onTest={onTest}
              onToggle={onToggle}
              onRemove={onRemove}
            />
          ))}
        </ul>
      ) : null}
    </li>
  );
}

const SENTENCE_TEXT: Record<"pos" | "info" | "warn" | "neg" | "muted", string> = {
  pos: "text-muted",
  info: "text-muted",
  warn: "text-[var(--color-warn-ink)]",
  neg: "text-[var(--color-neg-ink)]",
  muted: "text-faint",
};

/**
 * Loading state on the card shape so cards do not jump when accounts land.
 */
function InventorySkeleton() {
  return (
    <div aria-busy="true" className="flex flex-col gap-2">
      <span className="sr-only">Loading providers…</span>
      {Array.from({ length: 3 }, (_, index) => (
        <div
          key={index}
          aria-hidden="true"
          className="rounded-[var(--radius-card)] border border-line bg-surface px-4 py-3"
        >
          <div className="flex items-center gap-3">
            <span className="h-8 w-8 shrink-0 animate-pulse rounded-[10px] bg-surface-3" />
            <span className="flex min-w-0 flex-1 flex-col gap-1.5">
              <span className="h-3 w-32 max-w-full animate-pulse rounded bg-surface-3" />
              <span className="h-2.5 w-48 max-w-full animate-pulse rounded bg-surface-3" />
            </span>
          </div>
        </div>
      ))}
    </div>
  );
}

const PAGE_SIZES = [10, 25, 50, 100];
const DEFAULT_PAGE_SIZE = 10;

/**
 * The inventory: one card per provider, its accounts collapsed underneath. The card's
 * sentence and counts always describe every account — the filter picks whole cards, and
 * expanding shows the accounts themselves.
 */
export function ProviderInventory({
  groups,
  filter,
  onFilterChange,
  quotas,
  subscriptions,
  quotaLoaded,
  trafficById,
  providers,
  oauth,
  isAdmin,
  testingId,
  testsById,
  inspectHrefFor,
  displayOf,
  teamName,
  selectedId,
  loading,
  onConfigure,
  onAdd,
  onTest,
  onToggle,
  onRemove,
  onOpenCodes,
}: {
  groups: ProviderGroup[];
  filter: ProviderFilter;
  onFilterChange: (next: ProviderFilter) => void;
  quotas?: AccountQuotaRow[];
  subscriptions?: AccountSubscription[];
  quotaLoaded: boolean;
  trafficById: ReadonlyMap<string, AccountTraffic24h>;
  providers?: ModelProvider[];
  oauth?: OAuthProvider[];
  isAdmin: boolean;
  testingId: string | null;
  testsById: Record<string, ConnectionTestResult>;
  inspectHrefFor: (connectionId: string) => string;
  displayOf: (id: string) => string;
  teamName: (id: string | null) => string | null;
  selectedId?: string;
  loading: boolean;
  onConfigure: (connection: Connection) => void;
  onAdd: (provider: string) => void;
  onTest: (connectionId: string) => void;
  onToggle: (connection: Connection) => void;
  onRemove: (connection: Connection) => void;
  onOpenCodes: () => void;
}) {
  const [page, setPage] = useState(1);
  const [size, setSize] = useState(DEFAULT_PAGE_SIZE);
  const [expanded, setExpanded] = useState<ReadonlySet<string>>(new Set());
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

  // An inspected account's card opens so the deep link lands on a visible row.
  useEffect(() => {
    if (!selectedId) return;
    const group = groups.find((row) => row.connections.some((c) => c.id === selectedId));
    if (!group) return;
    setExpanded((prev) => (prev.has(group.key) ? prev : new Set(prev).add(group.key)));
  }, [selectedId, groups]);

  const toggleExpanded = (key: string) =>
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });

  const emptyCopy =
    filter === "healthy"
      ? "No provider is fully healthy right now."
      : filter === "attention"
        ? "Nothing needs attention — every provider is routing."
        : filter === "disabled"
          ? "No provider is turned off."
          : "No providers match.";

  return (
    <div id="providers-connected" className="flex flex-col gap-2">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h2 className="text-small font-semibold text-fg">
          Providers
          {!loading ? (
            <span className="ml-2 font-normal text-faint tnum">
              {groups.length} shown
            </span>
          ) : null}
        </h2>
        {isAdmin ? (
          <Button variant="quiet" size="sm" onClick={onOpenCodes}>
            <span className="flex items-center gap-1">
              <SlidersHorizontal size={13} aria-hidden />
              Provider codes
            </span>
          </Button>
        ) : null}
      </div>

      {loading ? (
        <InventorySkeleton />
      ) : groups.length === 0 ? (
        <Card className="flex flex-col items-center gap-2 px-4 py-10 text-center">
          <p className="text-small text-muted">{emptyCopy}</p>
          {filter !== "connected" ? (
            <Button variant="quiet" size="sm" onClick={() => onFilterChange("connected")}>
              Show all providers
            </Button>
          ) : null}
        </Card>
      ) : (
        <>
          <ul className="flex flex-col gap-2" aria-label="Provider accounts">
            {visible.map((group) => (
              <ProviderCard
                key={group.key}
                group={group}
                expanded={expanded.has(group.key)}
                onToggleExpanded={() => toggleExpanded(group.key)}
                quotas={quotas}
                subscriptions={subscriptions}
                quotaLoaded={quotaLoaded}
                trafficById={trafficById}
                providers={providers}
                oauth={oauth}
                isAdmin={isAdmin}
                testingId={testingId}
                testsById={testsById}
                inspectHrefFor={inspectHrefFor}
                displayOf={displayOf}
                teamName={teamName}
                selectedId={selectedId}
                onConfigure={onConfigure}
                onAdd={onAdd}
                onTest={onTest}
                onToggle={onToggle}
                onRemove={onRemove}
              />
            ))}
          </ul>
          {groups.length > size ? (
            <div className="rounded-[var(--radius-card)] border border-line bg-surface">
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
          ) : null}
        </>
      )}
    </div>
  );
}
