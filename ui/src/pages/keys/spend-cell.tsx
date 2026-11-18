import type { ReactNode } from "react";
import { Key as KeyIcon } from "@phosphor-icons/react";
import { Badge, Dot, cx } from "../../components/ui";
import { compact, usd, when } from "../../lib/format";
import type { BudgetWindow, Guardrails, KeyResult } from "../../lib/api";

/** Spend counters are already scoped to the key's window; this only names that window. */
export const WINDOW_NOW: Record<BudgetWindow, string | null> = {
  total: null,
  daily: "today",
  weekly: "this week",
  monthly: "this month",
};

const EXPIRING_SOON_SECS = 7 * 86_400;

/** Inactive outranks expired: a revoked key is disabled whatever its date says. */
export function statusOf(row: KeyResult): {
  label: string;
  tone: "pos" | "neutral" | "warn";
  dot: "pos" | "warn" | "muted";
} {
  if (!row.is_active) return { label: "disabled", tone: "neutral", dot: "muted" };
  if (row.expires_at !== null && row.expires_at <= Date.now() / 1000) {
    return { label: "expired", tone: "warn", dot: "warn" };
  }
  return { label: "active", tone: "pos", dot: "pos" };
}

export function createdDay(ts: number): string {
  return new Date(ts * 1000).toLocaleDateString(undefined, {
    month: "short",
    day: "numeric",
  });
}

export function modelScopeLabel(row: KeyResult): string {
  const allowed = row.allowed_models;
  const mapped = row.models?.length ?? 0;
  const scope =
    allowed === null || allowed.length === 0
      ? "all models"
      : `${allowed.length} ${allowed.length === 1 ? "model" : "models"}`;
  return mapped > 0 ? `${scope} · ${mapped} mapped` : scope;
}

/** Everything hidden by a narrow breakpoint, kept as the row's title/aria summary. */
export function rowSummary(row: KeyResult, team: string | null): string {
  const limits = [
    row.rpm > 0 ? `${compact(row.rpm)} rpm` : "unlimited rpm",
    row.tpm > 0 ? `${compact(row.tpm)} tpm` : "unlimited tpm",
    row.budget_tokens > 0 ? `${compact(row.budget_tokens)} tokens` : "unlimited tokens",
    row.budget_micros > 0 ? `${usd(row.budget_micros)} budget` : null,
  ]
    .filter((part): part is string => part !== null)
    .join(" · ");
  const models = modelScopeLabel(row);
  return [
    `team ${team ?? "none"}`,
    `spend ${usd(row.spent_micros)} · ${compact(row.spent_tokens)} tokens used`,
    `limits ${limits}`,
    `models ${models}`,
    `created ${when(row.created_at)}`,
  ].join(" · ");
}

export function StatusBadge({ row }: { row: KeyResult }) {
  const { label, tone, dot } = statusOf(row);
  const title = [
    row.is_active ? "accepting requests" : "every request is rejected with 403",
    row.expires_at === null ? "never expires" : `expires ${when(row.expires_at)}`,
  ].join(" · ");
  return (
    <Badge tone={tone} title={title}>
      <Dot tone={dot} />
      {label}
    </Badge>
  );
}

/** Displays upcoming (or elapsed on a disabled key) expiration dates with `.tnum`. */
export function ExpiryBadge({
  expiresAt,
  isActive,
}: {
  expiresAt: number | null;
  isActive: boolean;
}) {
  if (expiresAt === null) return null;
  const nowSecs = Date.now() / 1000;
  const elapsed = expiresAt <= nowSecs;
  // Active expired keys already display "expired" in StatusBadge.
  if (elapsed && isActive) return null;
  const soon = !elapsed && expiresAt - nowSecs <= EXPIRING_SOON_SECS;
  return (
    <Badge
      tone={soon ? "warn" : "neutral"}
      className="tnum"
      title={elapsed ? `expired ${when(expiresAt)}` : `expires ${when(expiresAt)}`}
    >
      {elapsed ? `expired ${createdDay(expiresAt)}` : `exp ${createdDay(expiresAt)}`}
    </Badge>
  );
}

/** One key's identity: name, live status, expiry, tenancy/scope badges, and where it sits. */
export function KeyCell({ row, team }: { row: KeyResult; team: string | null }) {
  const scope = modelScopeLabel(row);
  const initial = (row.name ?? "?").trim().slice(0, 1).toUpperCase() || "?";
  const playground = row.kind === "playground";
  return (
    <div className="flex min-w-0 items-start gap-3">
      <span
        aria-hidden
        className={cx(
          "grid h-8 w-8 shrink-0 place-items-center rounded-lg border text-caption font-semibold",
          playground
            ? "border-[var(--color-accent)]/25 bg-[var(--color-accent)]/10 text-[var(--color-accent-text)]"
            : "border-line bg-[var(--color-surface-2)] text-muted",
        )}
      >
        {playground ? <KeyIcon size={14} /> : initial}
      </span>
      <div className="min-w-0">
        <div className="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-0.5">
          <span
            className="min-w-0 max-w-full truncate text-body font-semibold text-fg"
            title={rowSummary(row, team)}
          >
            {row.name ?? <span className="font-normal text-faint">unnamed</span>}
          </span>
          <StatusBadge row={row} />
          <ExpiryBadge expiresAt={row.expires_at} isActive={row.is_active} />
        </div>
        <div className="mt-0.5 flex min-w-0 flex-wrap items-center gap-x-1.5 gap-y-0.5 text-caption text-faint">
          {playground ? (
            <Badge
              tone="accent"
              title="issued by the gateway; /v1 accepts it only from the dashboard playground"
            >
              playground
            </Badge>
          ) : null}
          {row.scope === "admin" ? (
            <Badge tone="accent" title="manages the gateway; cannot call /v1 or spend tokens">
              admin
            </Badge>
          ) : null}
          <span
            className={cx("min-w-0 truncate", team !== null && "font-medium text-muted")}
            title={team === null ? "not assigned to a team; serves instance-wide keys" : "team"}
          >
            {team ?? "Shared"}
          </span>
          <span aria-hidden>·</span>
          <span className="shrink-0 tnum" title={`created ${when(row.created_at)}`}>
            {createdDay(row.created_at)}
          </span>
          <span aria-hidden>·</span>
          <span className="min-w-0 truncate tnum" title={scope}>
            {scope}
          </span>
        </div>
      </div>
    </div>
  );
}

/** Dense semantic chips keep repeated limits scannable without a wall of separators. */
function LimitChip({ children, title }: { children: ReactNode; title?: string }) {
  return (
    <span
      title={title}
      className="inline-flex items-center rounded-md border border-line bg-[var(--color-surface-2)] px-1.5 py-0.5 text-caption leading-4 text-muted tnum"
    >
      {children}
    </span>
  );
}

/** Faint table marker; the tooltip spells out every check the key carries. */
function Guarded({ guardrails }: { guardrails: Guardrails }) {
  const cap = guardrails.max_prompt_chars;
  // Defaults are dropped on the wire, so a policy that only caps prompts has no `blocked`.
  const blocked = guardrails.blocked ?? [];
  if (
    cap === undefined &&
    blocked.length === 0 &&
    !guardrails.scan_output &&
    !guardrails.redact_pii
  ) {
    return null;
  }
  const limits = [
    cap === undefined ? "prompt length: unlimited" : `prompt: at most ${compact(cap)} chars`,
    blocked.length === 0 ? "no blocked phrases" : `blocked: ${blocked.join(", ")}`,
    guardrails.scan_output ? "responses scanned too — a hit blocks the reply" : "prompts only",
    guardrails.redact_pii ? "PII redacted in prompts and response text, including streams" : null,
  ].filter((line): line is string => line !== null);
  return (
    <Badge title={limits.join("\n")}>
      {blocked.length > 0 ? `${blocked.length} blocked` : "guarded"}
    </Badge>
  );
}

export function LimitsCell({ row }: { row: KeyResult }) {
  const parts: Array<[number, string]> = [
    [row.rpm, "rpm"],
    [row.tpm, "tpm"],
    [row.budget_tokens, "tokens"],
  ];
  const limited = parts.filter(([value]) => value > 0);
  const hasBudget = row.budget_micros > 0;
  const resetWindow =
    (hasBudget || row.budget_tokens > 0) && row.budget_window !== "total"
      ? row.budget_window
      : null;
  const summary = [
    ...parts.map(([value, unit]) => (value > 0 ? `${compact(value)} ${unit}` : `unlimited ${unit}`)),
    hasBudget ? `${usd(row.budget_micros)} USD budget` : null,
    resetWindow ? `resets ${resetWindow}` : null,
    row.guardrails ? "guarded" : null,
  ]
    .filter((part): part is string => part !== null)
    .join(" · ");
  return (
    <div className="flex min-w-0 flex-col gap-1" title={summary}>
      <div className="flex flex-wrap items-center gap-1">
        {limited.length === 0 && !hasBudget ? (
          <span className="text-caption text-faint">No per-key limits</span>
        ) : null}
        {limited.map(([value, unit]) => (
          <LimitChip key={unit} title={summary}>
            {compact(value)} {unit}
          </LimitChip>
        ))}
        {hasBudget ? <LimitChip title={summary}>{usd(row.budget_micros)} budget</LimitChip> : null}
        {resetWindow ? <LimitChip title="budget reset window">{resetWindow}</LimitChip> : null}
        {row.guardrails ? <Guarded guardrails={row.guardrails} /> : null}
      </div>
    </div>
  );
}

export function SpendProgressBar({
  share,
  reached,
}: {
  share: number;
  reached: boolean;
}) {
  const pct = Math.min(100, Math.max(0, share));
  return (
    <span className="mt-1.5 block h-1 w-full overflow-hidden rounded-full bg-[var(--color-rule)]">
      <span
        className="block h-full rounded-full transition-[width] duration-[var(--duration-micro)]"
        style={{
          width: `${pct > 0 ? Math.max(2, pct) : 0}%`,
          background: reached ? "var(--color-neg)" : "var(--color-accent)",
        }}
      />
    </span>
  );
}

/** Money & token usage against configured budgets, with progress bar and `.tnum` readouts. */
export function SpendCell({ row }: { row: KeyResult }) {
  const now = WINDOW_NOW[row.budget_window];
  const hasUsdBudget = row.budget_micros > 0;
  const hasTokenBudget = row.budget_tokens > 0;
  const usdReached = hasUsdBudget && row.spent_micros >= row.budget_micros;
  const tokenReached = hasTokenBudget && row.spent_tokens >= row.budget_tokens;
  const reached = usdReached || tokenReached;

  const dollarNote = hasUsdBudget
    ? `${usd(row.spent_micros)} of ${usd(row.budget_micros)} USD used`
    : `${usd(row.spent_micros)} USD used · no USD budget`;
  const tokenNote = hasTokenBudget
    ? `${compact(row.spent_tokens)} of ${compact(row.budget_tokens)} tokens used`
    : `${compact(row.spent_tokens)} tokens used · no token budget`;
  const title = [dollarNote, tokenNote, ...(now ? [`resets ${row.budget_window}`] : [])].join(" · ");

  const usdShare = hasUsdBudget ? Math.min(100, (row.spent_micros / row.budget_micros) * 100) : 0;
  const tokenShare = hasTokenBudget
    ? Math.min(100, (row.spent_tokens / row.budget_tokens) * 100)
    : 0;
  const share =
    hasUsdBudget && hasTokenBudget
      ? Math.max(usdShare, tokenShare)
      : hasUsdBudget
        ? usdShare
        : tokenShare;

  return (
    <div className="min-w-0" title={title}>
      <div className="flex flex-wrap items-baseline gap-1 tnum">
        <span className={cx("text-body font-semibold", usdReached ? "text-neg" : "text-fg")}>
          {usd(row.spent_micros)}
        </span>
        {hasUsdBudget ? (
          <span className="text-caption text-faint">/ {usd(row.budget_micros)}</span>
        ) : null}
      </div>
      <div className={cx("text-caption tnum", tokenReached ? "text-neg" : "text-faint")}>
        {hasTokenBudget
          ? `${compact(row.spent_tokens)} / ${compact(row.budget_tokens)} tok`
          : `${compact(row.spent_tokens)} tok`}
      </div>
      {hasUsdBudget || hasTokenBudget ? (
        <>
          <SpendProgressBar share={share} reached={reached} />
          <div className="mt-1 flex items-center justify-between gap-2 text-caption">
            <span className="text-faint">{now ?? "total"}</span>
            {reached ? <span className="text-neg">budget reached</span> : null}
          </div>
        </>
      ) : null}
    </div>
  );
}
