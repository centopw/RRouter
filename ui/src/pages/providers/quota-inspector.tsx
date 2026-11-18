import { useMemo, useState } from "react";
import { GearSix } from "@phosphor-icons/react";
import { IconButton, SectionLabel } from "../../components/atoms";
import {
  Badge,
  Button,
  Card,
  CardHeader,
  ConfirmDialog,
  Dot,
  ErrorNote,
  Field,
  Input,
  Segmented,
  cx,
} from "../../components/ui";
import {
  isEndpointMissing,
  type AccountQuotaRow,
  type AccountQuotaWindow,
  type AccountSubscription,
  type QuotaPingMode,
  type QuotaPingRow,
  type SubscriptionWindow,
  type UpdateQuotaPing,
} from "../../lib/api";
import { compact, parseUsd, usd, when } from "../../lib/format";
import { useClearAccountQuota, useUpdateAccountQuota, useUpdateQuotaPing } from "../../lib/queries";
import {
  PING_MODES,
  QUOTA_WINDOWS,
  WINDOW_LABEL,
  clockLabel,
  countProblem,
  parseAnchor,
  phaseLabel,
  pingBadgeText,
  providerLabel,
  until,
  usageTone,
  validTimezone,
} from "./types";

/** Progress bar for one subscription plan window (5h, weekly, completions, etc.). */
export function WindowBar({ window }: { window: SubscriptionWindow }) {
  const percent = Math.round(window.used_percent);
  const tone = usageTone(percent);
  const short =
    window.id === "five_hour"
      ? "5h"
      : window.id === "weekly"
        ? "wk"
        : window.id === "premium-interactions"
          ? "req"
          : window.id === "completions"
            ? "code"
            : window.label.toLowerCase();
  return (
    <div className="flex items-center gap-2 text-caption">
      <span className="w-11 shrink-0 text-faint" title={window.label}>
        {short}
      </span>
      <span className="h-1.5 min-w-[48px] flex-1 overflow-hidden rounded-full bg-rule" aria-hidden>
        <span className={cx("block h-full rounded-full", tone.fill)} style={{ width: `${percent}%` }} />
      </span>
      <span className={cx("w-9 shrink-0 text-right font-medium tnum", tone.text)}>{percent}%</span>
      {window.resets_at ? (
        <span
          className="w-14 shrink-0 text-right text-faint tnum"
          title={`resets ${when(window.resets_at)}`}
        >
          {until(window.resets_at)}
        </span>
      ) : null}
    </div>
  );
}

/** Auto-ping mode badge with schedule/next-ping tooltip. */
export function PingBadge({ ping }: { ping: QuotaPingRow }) {
  const detail = [
    `auto-ping ${ping.mode}`,
    ping.mode === "schedule"
      ? `${ping.anchor} ${ping.timezone}, ${ping.slots_per_day} slots/day`
      : null,
    ping.next_ping_at ? `next ${when(ping.next_ping_at)}` : "no ping scheduled",
    ping.last_error ? `last error: ${ping.last_error}` : null,
  ]
    .filter(Boolean)
    .join(" · ");
  const tone =
    ping.mode === "off" ? "neutral" : ping.mode === "reset" || ping.mode === "smart" ? "pos" : "accent";
  return (
    <Badge tone={tone} title={detail}>
      <span className="tnum">{pingBadgeText(ping)}</span>
    </Badge>
  );
}

/** One subscription account row displaying plan badge, auto-ping status, and window bars. */
export function SubscriptionRow({
  account,
  ping,
  onEdit,
}: {
  account: AccountSubscription;
  ping?: QuotaPingRow;
  /** Absent for a reader: the mode badge stays, the settings trigger does not. */
  onEdit?: (connectionId: string) => void;
}) {
  const exhausted = account.windows.find((window) => window.used_percent >= 100);
  const supportsPing = account.provider === "claude" || account.provider === "codex";
  return (
    <div className="border-b border-rule px-5 py-3 last:border-0">
      <div className="flex flex-wrap items-baseline gap-x-2 gap-y-1">
        <span className="text-body font-medium text-fg">{providerLabel(account.provider)}</span>
        <span className="min-w-0 truncate text-small text-muted">{account.name ?? ""}</span>
        {account.plan ? <Badge tone="neutral">{account.plan}</Badge> : null}
        {supportsPing && ping ? <PingBadge ping={ping} /> : null}
        {exhausted ? (
          <Badge
            tone="neg"
            title={exhausted.resets_at ? `resets ${when(exhausted.resets_at)}` : undefined}
          >
            quota exhausted
          </Badge>
        ) : null}
        <span className="ml-auto flex min-w-0 max-w-full items-center gap-1.5 text-caption text-faint">
          {account.error ? (
            <span className="min-w-0 truncate" title={account.error}>
              {account.error.length > 110 ? `${account.error.slice(0, 110)}…` : account.error}
            </span>
          ) : (
            <>
              <Dot tone="pos" />
              <span>live</span>
            </>
          )}
          {supportsPing && ping && onEdit ? (
            <IconButton
              label={`Auto-ping settings for ${account.name ?? providerLabel(account.provider)}`}
              onClick={() => onEdit(account.connection_id)}
            >
              <GearSix size={15} aria-hidden />
            </IconButton>
          ) : null}
        </span>
      </div>
      {account.windows.length > 0 ? (
        <div className="mt-2 grid gap-1.5 min-[700px]:grid-cols-2 min-[1100px]:grid-cols-3">
          {account.windows.map((window) => (
            <WindowBar key={window.id} window={window} />
          ))}
        </div>
      ) : null}
    </div>
  );
}

/** Page-level Subscription limits card showing provider-reported plan windows. */
export function SubscriptionLimitsCard({
  subscriptions,
  pings,
  pingsError,
  isAdmin,
  onEditPing,
  onRetrySubscriptions,
}: {
  subscriptions: {
    data?: AccountSubscription[];
    isLoading: boolean;
    isFetching: boolean;
    isError: boolean;
    error: unknown;
  };
  pings: QuotaPingRow[];
  pingsError: boolean;
  isAdmin: boolean;
  onEditPing: (connectionId: string) => void;
  onRetrySubscriptions: () => void;
}) {
  const pingByConnection = new Map(pings.map((ping) => [ping.connection_id, ping]));
  const rows = subscriptions.data ?? [];
  return (
    <div className="mt-4">
      <Card>
        <CardHeader
          title="Subscription limits"
          action={
            <span className="text-caption text-faint">
              {subscriptions.isFetching
                ? "refreshing…"
                : pingsError && isAdmin
                  ? "auto-ping unavailable"
                  : "from your provider plan · refreshes every five minutes"}
            </span>
          }
        />
        {subscriptions.isError ? (
          <div className="px-5 pb-4">
            {isAdmin ? (
              <ErrorNote error={subscriptions.error} onRetry={onRetrySubscriptions} />
            ) : (
              <p className="text-small text-muted">
                Subscription limits are available to administrators on this gateway.
              </p>
            )}
          </div>
        ) : subscriptions.isLoading ? (
          <p className="px-5 pb-4 text-small text-faint">reading plan limits…</p>
        ) : rows.length === 0 ? (
          <p className="px-5 pb-4 text-small text-muted">
            No subscription limits to show. Claude, Codex and Copilot sign-ins report their plan
            windows; OpenCode Go API keys report their rolling, weekly and monthly allowance.
          </p>
        ) : (
          <div>
            {rows.map((account) => (
              <SubscriptionRow
                key={account.connection_id}
                account={account}
                ping={pingByConnection.get(account.connection_id)}
                onEdit={isAdmin ? onEditPing : undefined}
              />
            ))}
          </div>
        )}
      </Card>
    </div>
  );
}

/**
 * What the account spent inside its window, against each limit it sets. A quota with
 * no limit still shows the spend: "unlimited" is a permission, not an absence of data.
 */
export function QuotaUsage({ row }: { row: AccountQuotaRow }) {
  const parts: string[] = [];
  if (row.max_requests > 0) {
    parts.push(`requests ${compact(row.requests)}/${compact(row.max_requests)}`);
  }
  if (row.max_tokens > 0) {
    parts.push(`tokens ${compact(row.tokens)}/${compact(row.max_tokens)}`);
  }
  if (row.max_cost_micros > 0) {
    parts.push(`${usd(row.cost_micros)}/${usd(row.max_cost_micros)}`);
  }
  return (
    <span className="text-muted tnum">
      {parts.length > 0
        ? parts.join(" · ")
        : `${usd(row.cost_micros)} · ${compact(row.tokens)} tokens · ${compact(row.requests)} requests`}
      {row.exhausted && row.resets_at ? (
        <span className="text-neg"> · resets {when(row.resets_at)}</span>
      ) : null}
    </span>
  );
}

export type OperatorQuotaDraft = {
  window: AccountQuotaWindow;
  requests: string;
  tokens: string;
  cost: string;
};

/**
 * Operator quota inspector & editor panel used inside ConnectionDrawer's "Quotas & Auto-Ping" tab.
 * Empty fields or `0` represent unlimited caps; saving sends the full form so clearing a
 * field removes that cap.
 */
export function OperatorQuotaPanel({
  row,
  isAdmin,
  loading,
  error,
  onRetry,
  draft,
  dirty,
  onChange,
  onCommitted,
}: {
  row?: AccountQuotaRow;
  isAdmin: boolean;
  loading: boolean;
  error: unknown;
  onRetry: () => void;
  draft: OperatorQuotaDraft;
  dirty: boolean;
  onChange: (patch: Partial<OperatorQuotaDraft>) => void;
  onCommitted: () => void;
}) {
  const update = useUpdateAccountQuota();
  const clear = useClearAccountQuota();
  const [failure, setFailure] = useState<unknown>(null);
  const [savedNotice, setSavedNotice] = useState<string | null>(null);
  const [confirmClear, setConfirmClear] = useState(false);

  if (!isAdmin) {
    return (
      <section className="flex flex-col gap-2 rounded-[var(--radius-card)] border border-line bg-surface-2/60 px-4 py-3">
        <SectionLabel>Operator quota</SectionLabel>
        {row ? (
          <div className="flex flex-wrap items-center justify-between gap-2 text-small">
            <span>
              <span className="font-medium text-fg">{WINDOW_LABEL[row.window]}</span> ·{" "}
              <QuotaUsage row={row} />
            </span>
            {row.exhausted ? <Badge tone="neg">exhausted</Badge> : null}
          </div>
        ) : (
          <p className="text-small text-muted">
            Operator quota caps are managed by administrators on this gateway.
          </p>
        )}
      </section>
    );
  }

  if (error) {
    return (
      <section className="flex flex-col gap-2 rounded-[var(--radius-card)] border border-line bg-surface-2/60 px-4 py-3">
        <SectionLabel>Operator quota</SectionLabel>
        <ErrorNote error={error} onRetry={onRetry} />
      </section>
    );
  }

  if (loading || !row) {
    return (
      <section className="flex flex-col gap-2 rounded-[var(--radius-card)] border border-line bg-surface-2/60 px-4 py-3">
        <SectionLabel>Operator quota</SectionLabel>
        <p className="text-small text-faint">reading current limits…</p>
      </section>
    );
  }

  const costParsed = parseUsd(draft.cost);
  const requestsProblem = countProblem(draft.requests);
  const tokensProblem = countProblem(draft.tokens);
  const costProblem = "problem" in costParsed ? costParsed.problem : null;
  const invalid = requestsProblem !== null || tokensProblem !== null || costProblem !== null;
  const busy = update.isPending || clear.isPending;
  const configured =
    row.max_requests > 0 || row.max_tokens > 0 || row.max_cost_micros > 0 || row.window !== "daily";

  const save = () => {
    if (invalid || !dirty || busy) return;
    setFailure(null);
    setSavedNotice(null);
    update.mutate(
      {
        id: row.connection_id,
        patch: {
          window: draft.window,
          max_requests: draft.requests.trim() === "" ? 0 : Number(draft.requests),
          max_tokens: draft.tokens.trim() === "" ? 0 : Number(draft.tokens),
          max_cost_micros: "micros" in costParsed ? costParsed.micros : 0,
        },
      },
      {
        onSuccess: () => {
          onCommitted();
          setSavedNotice("Operator limits saved");
        },
        onError: setFailure,
      },
    );
  };

  return (
    <section className="flex flex-col gap-4 rounded-[var(--radius-card)] border border-line bg-surface-2/40 p-4">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <SectionLabel>Operator limits</SectionLabel>
        {row.exhausted ? <Badge tone="neg">exhausted</Badge> : null}
      </div>

      <p className="text-small leading-relaxed text-muted">
        Measured from the start of the current UTC window. The router skips an account once a limit
        is reached and falls back to the next one.
      </p>

      <div className="rounded-lg border border-line bg-surface px-3 py-2 text-caption text-muted">
        <span className="font-medium text-fg">{WINDOW_LABEL[row.window]}</span> ·{" "}
        <QuotaUsage row={row} />
      </div>

      <Field label="window">
        <Segmented
          options={QUOTA_WINDOWS}
          value={draft.window}
          onChange={(v) => {
            if (busy) return;
            setFailure(null);
            setSavedNotice(null);
            onChange({ window: v });
          }}
          ariaLabel="quota window"
        />
      </Field>

      <div className="grid grid-cols-2 gap-3">
        <Field label="requests" hint="empty = unlimited" error={requestsProblem ?? undefined}>
          <Input
            type="number"
            value={draft.requests}
            onChange={(v) => {
              setFailure(null);
              setSavedNotice(null);
              onChange({ requests: v });
            }}
            ariaLabel="quota requests"
            className="tnum"
            disabled={busy}
          />
        </Field>
        <Field label="tokens" hint="empty = unlimited" error={tokensProblem ?? undefined}>
          <Input
            type="number"
            value={draft.tokens}
            onChange={(v) => {
              setFailure(null);
              setSavedNotice(null);
              onChange({ tokens: v });
            }}
            ariaLabel="quota tokens"
            className="tnum"
            disabled={busy}
          />
        </Field>
      </div>

      <Field label="spend (USD)" hint="empty = unlimited" error={costProblem ?? undefined}>
        <Input
          value={draft.cost}
          onChange={(v) => {
            setFailure(null);
            setSavedNotice(null);
            onChange({ cost: v });
          }}
          ariaLabel="quota spend"
          className="tnum"
          disabled={busy}
        />
      </Field>

      {savedNotice ? (
        <p role="status" className="text-caption text-[var(--color-pos-ink)]">
          {savedNotice}
        </p>
      ) : null}
      {failure ? <ErrorNote error={failure} /> : null}

      <div className="flex flex-wrap items-center justify-end gap-2 border-t border-line pt-3">
        {configured ? (
          <Button
            variant="danger"
            size="sm"
            disabled={busy}
            onClick={() => setConfirmClear(true)}
            className="mr-auto"
          >
            Clear limits
          </Button>
        ) : null}
        <Button
          variant="primary"
          size="sm"
          loading={update.isPending}
          disabled={!dirty || invalid || busy}
          onClick={save}
        >
          {update.isPending ? "Saving…" : "Save limits"}
        </Button>
      </div>

      <ConfirmDialog
        open={confirmClear}
        title="Clear account quota"
        message={
          <>
            The operator limits for <span className="font-medium">{row.name ?? row.provider}</span>{" "}
            are removed. Its subscription limits, when the provider exposes them, are unaffected.
          </>
        }
        confirmLabel="Clear quota"
        busy={clear.isPending}
        onConfirm={() => {
          setFailure(null);
          setSavedNotice(null);
          clear.mutate(row.connection_id, {
            onSuccess: () => {
              onCommitted();
              setSavedNotice("Operator limits cleared");
            },
            onError: setFailure,
          });
          setConfirmClear(false);
        }}
        onClose={() => setConfirmClear(false)}
      />
    </section>
  );
}

export type AutoPingDraft = {
  mode: QuotaPingMode;
  anchor: string;
  timezone: string;
  windowHours: string;
  slots: string;
};

/**
 * Auto-ping schedule inspector & editor for one Claude/Codex account inside ConnectionDrawer.
 */
export function AutoPingPanel({
  row,
  rows,
  isAdmin,
  supportsPing,
  pingsError,
  draft,
  dirty,
  onChange,
  onCommitted,
}: {
  row?: QuotaPingRow;
  rows: QuotaPingRow[];
  isAdmin: boolean;
  supportsPing: boolean;
  pingsError: unknown;
  draft: AutoPingDraft;
  dirty: boolean;
  onChange: (patch: Partial<AutoPingDraft>) => void;
  onCommitted: () => void;
}) {
  const update = useUpdateQuotaPing();
  const [failure, setFailure] = useState<unknown>(null);
  const [savedNotice, setSavedNotice] = useState<string | null>(null);

  const peers = useMemo(() => {
    if (!row) return [];
    // The scheduler staggers only enabled smart accounts; include the account being edited
    // so the preview describes the fleet as it will look after saving.
    const same = rows.filter(
      (ping) =>
        ping.provider === row.provider &&
        (ping.mode === "smart" || ping.connection_id === row.connection_id),
    );
    if (!same.some((ping) => ping.connection_id === row.connection_id)) same.push(row);
    return same.sort((a, b) => b.smart.recent_requests - a.smart.recent_requests);
  }, [rows, row]);

  if (!supportsPing) {
    return (
      <section className="flex flex-col gap-1.5 rounded-[var(--radius-card)] border border-line bg-surface-2/40 p-4">
        <SectionLabel>Auto-ping</SectionLabel>
        <p className="text-small text-muted">
          Auto-ping schedules apply to Claude and Codex subscription accounts.
        </p>
      </section>
    );
  }

  if (pingsError) {
    return (
      <section className="flex flex-col gap-2 rounded-[var(--radius-card)] border border-line bg-surface-2/40 p-4">
        <SectionLabel>Auto-ping</SectionLabel>
        {isEndpointMissing(pingsError) ? (
          <p className="text-small text-[var(--color-warn-ink)]">
            Auto-ping is not available on this build. Update the gateway and try again.
          </p>
        ) : (
          <ErrorNote error={pingsError} />
        )}
      </section>
    );
  }

  if (!row) {
    return (
      <section className="flex flex-col gap-1.5 rounded-[var(--radius-card)] border border-line bg-surface-2/40 p-4">
        <SectionLabel>Auto-ping</SectionLabel>
        <p className="text-small text-muted">
          No auto-ping schedule is active for this connection (disabled connections do not run
          auto-ping).
        </p>
      </section>
    );
  }

  const busy = update.isPending;
  const anchorMinutes = parseAnchor(draft.anchor);
  const hoursValue = Number(draft.windowHours);
  const slotsValue = Number(draft.slots);
  const windowValid = Number.isFinite(hoursValue) && hoursValue >= 0.5 && hoursValue <= 24;
  const maxSlots = windowValid ? Math.max(1, Math.floor(24 / hoursValue)) : 0;
  const slotsValid =
    Number.isInteger(slotsValue) && slotsValue >= 1 && (maxSlots === 0 || slotsValue <= maxSlots);
  const timezoneValid = validTimezone(draft.timezone);
  const scheduleInvalid = anchorMinutes === null || !timezoneValid || !windowValid || !slotsValid;

  const scheduleIssues: string[] = [];
  if (anchorMinutes === null) scheduleIssues.push("anchor must be 24-hour HH:MM, e.g. 09:30");
  if (!timezoneValid) scheduleIssues.push("timezone must be an IANA name, e.g. Europe/Paris or UTC");
  if (!windowValid) scheduleIssues.push("window must be between 0.5 and 24 hours");
  if (!slotsValid) {
    scheduleIssues.push(
      maxSlots > 0
        ? `slots per day must be a whole number from 1 to ${maxSlots}`
        : "slots per day must be a whole number",
    );
  }

  const preview =
    draft.mode === "schedule" && !scheduleInvalid
      ? (() => {
          const interval = Math.round(hoursValue * 60);
          const start = anchorMinutes ?? 0;
          return {
            slots: Array.from({ length: slotsValue }, (_, index) =>
              clockLabel(start + index * interval),
            ),
            coverage: clockLabel(start + slotsValue * interval),
          };
        })()
      : null;

  const smartCount = peers.length;
  const activeHint =
    PING_MODES.find((option) => option.id === draft.mode)?.hint ?? "No automatic pings.";

  const save = () => {
    if (!isAdmin || !dirty || (draft.mode === "schedule" && scheduleInvalid) || busy) return;
    setFailure(null);
    setSavedNotice(null);
    const patch: UpdateQuotaPing = { mode: draft.mode };
    if (draft.mode === "schedule") {
      patch.anchor = draft.anchor.trim();
      patch.timezone = draft.timezone.trim();
      patch.window_hours = hoursValue;
      patch.slots_per_day = slotsValue;
    }
    update.mutate(
      { id: row.connection_id, patch },
      {
        onSuccess: () => {
          onCommitted();
          setSavedNotice("Auto-ping settings saved");
        },
        onError: setFailure,
      },
    );
  };

  return (
    <section className="flex flex-col gap-4 rounded-[var(--radius-card)] border border-line bg-surface-2/40 p-4">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <SectionLabel>Auto-ping schedule</SectionLabel>
        <PingBadge ping={row} />
      </div>

      <div className="flex flex-wrap gap-x-4 gap-y-1 rounded-lg border border-line bg-surface px-3 py-2 text-caption text-muted tnum">
        <span>
          next ping{" "}
          <span className="font-medium text-fg">
            {row.next_ping_at ? when(row.next_ping_at) : "not scheduled"}
          </span>
        </span>
        <span>
          last ping{" "}
          <span className="font-medium text-fg">
            {row.last_ping_at ? when(row.last_ping_at) : "never"}
          </span>
        </span>
        {row.last_ping_reset_at ? (
          <span>
            reset seen <span className="font-medium text-fg">{when(row.last_ping_reset_at)}</span>
          </span>
        ) : null}
      </div>

      {isAdmin ? (
        <>
          <Field label="mode">
            <Segmented
              options={PING_MODES.map((option) => ({ id: option.id, label: option.label }))}
              value={draft.mode}
              onChange={(next) => {
                if (busy) return;
                setFailure(null);
                setSavedNotice(null);
                onChange({ mode: next });
              }}
              ariaLabel="auto-ping mode"
            />
          </Field>
          <p className="-mt-2 text-small leading-relaxed text-muted">{activeHint}</p>

          {draft.mode === "schedule" ? (
            <div className="flex flex-col gap-3 rounded-lg border border-line bg-surface p-3">
              <div className="grid gap-3 min-[520px]:grid-cols-2">
                <Field label="anchor" hint="local wall clock, HH:MM">
                  <Input
                    value={draft.anchor}
                    onChange={(value) => {
                      setFailure(null);
                      setSavedNotice(null);
                      onChange({ anchor: value });
                    }}
                    placeholder="09:30"
                    ariaLabel="auto-ping anchor"
                    className="font-mono tnum"
                    disabled={busy}
                  />
                </Field>
                <Field label="timezone" hint="IANA name, e.g. Europe/Paris">
                  <Input
                    value={draft.timezone}
                    onChange={(value) => {
                      setFailure(null);
                      setSavedNotice(null);
                      onChange({ timezone: value });
                    }}
                    placeholder="UTC"
                    ariaLabel="auto-ping timezone"
                    disabled={busy}
                  />
                </Field>
                <Field label="window hours" hint="0.5 to 24">
                  <Input
                    type="number"
                    value={draft.windowHours}
                    onChange={(value) => {
                      setFailure(null);
                      setSavedNotice(null);
                      onChange({ windowHours: value });
                    }}
                    ariaLabel="auto-ping window hours"
                    className="tnum"
                    disabled={busy}
                  />
                </Field>
                <Field
                  label="slots per day"
                  hint={maxSlots > 0 ? `1 to ${maxSlots}` : "whole number"}
                >
                  <Input
                    type="number"
                    value={draft.slots}
                    onChange={(value) => {
                      setFailure(null);
                      setSavedNotice(null);
                      onChange({ slots: value });
                    }}
                    ariaLabel="auto-ping slots per day"
                    className="tnum"
                    disabled={busy}
                  />
                </Field>
              </div>

              {scheduleIssues.length > 0 ? (
                <ul role="alert" className="flex flex-col gap-0.5 text-caption text-neg">
                  {scheduleIssues.map((issue) => (
                    <li key={issue}>{issue}</li>
                  ))}
                </ul>
              ) : preview ? (
                <div className="rounded-md bg-surface-2 px-3 py-2">
                  <p className="text-caption font-medium text-muted">
                    Local slot preview · {draft.timezone.trim()}
                  </p>
                  <p className="mt-1 break-words font-mono text-small text-fg tnum">
                    {preview.slots.join(" · ")}
                  </p>
                  <p className="mt-1 text-caption text-muted tnum">
                    Each slot starts a{" "}
                    {hoursValue.toLocaleString(undefined, { maximumFractionDigits: 2 })}-hour
                    window; coverage ends{" "}
                    <span className="font-mono text-fg">{preview.coverage}</span>.
                  </p>
                </div>
              ) : null}
            </div>
          ) : null}

          {draft.mode === "smart" ? (
            <div className="flex flex-col gap-2 rounded-lg border border-line bg-surface p-3">
              <div className="flex flex-wrap items-baseline justify-between gap-x-3 gap-y-1">
                <p className="text-small font-semibold text-fg">Accounts by recent usage</p>
                <p className="text-caption text-faint">highest first</p>
              </div>
              <ul className="flex flex-col gap-1">
                {peers.map((peer) => (
                  <li
                    key={peer.connection_id}
                    className={cx(
                      "flex flex-wrap items-baseline gap-x-2 gap-y-0.5 rounded-md px-2 py-1.5 text-caption",
                      peer.connection_id === row.connection_id && "bg-surface-3",
                    )}
                  >
                    <span className="min-w-0 flex-1 basis-36 truncate font-medium text-fg">
                      {peer.name ?? providerLabel(peer.provider)}
                      {peer.connection_id === row.connection_id ? " · this account" : ""}
                    </span>
                    <span className="tnum text-muted">
                      {peer.smart.recent_requests.toLocaleString()} req/24h
                    </span>
                    <span className="tnum text-muted">
                      offset +{phaseLabel(peer.smart.phase_offset_secs)}
                    </span>
                    <span className="tnum text-faint">
                      {peer.next_ping_at ? `next ${when(peer.next_ping_at)}` : "next —"}
                    </span>
                  </li>
                ))}
              </ul>
              <p className="text-small leading-relaxed text-muted">
                Smart orders accounts by usage and gives each a phase offset across one{" "}
                {row.window_hours}-hour window. The busiest account pings at its reset, then the
                others follow their assigned offsets ({smartCount} smart account
                {smartCount === 1 ? "" : "s"}), so resets stay staggered and one fresh window is
                available. Accounts with no requests in the last 24 hours and zero current usage
                are skipped.
              </p>
            </div>
          ) : null}
        </>
      ) : null}

      {row.last_error ? (
        <p
          role="status"
          className="break-words rounded-lg border border-[var(--color-warn-line)] bg-[var(--color-warn-tint)] px-3 py-2 text-caption text-[var(--color-warn-ink)]"
        >
          Last auto-ping error: {row.last_error}
        </p>
      ) : null}

      {savedNotice ? (
        <p role="status" className="text-caption text-[var(--color-pos-ink)]">
          {savedNotice}
        </p>
      ) : null}
      {failure ? (
        isEndpointMissing(failure) ? (
          <p role="alert" className="text-small text-[var(--color-warn-ink)]">
            Auto-ping is not available on this build. Update the gateway and try again.
          </p>
        ) : (
          <ErrorNote error={failure} />
        )
      ) : null}

      {isAdmin ? (
        <div className="flex justify-end border-t border-line pt-3">
          <Button
            variant="primary"
            size="sm"
            loading={update.isPending}
            disabled={!dirty || busy || (draft.mode === "schedule" && scheduleInvalid)}
            onClick={save}
          >
            {update.isPending ? "Saving…" : "Save auto-ping"}
          </Button>
        </div>
      ) : null}
    </section>
  );
}
