import { useCallback, useEffect, useRef, useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import type { HealthStatus } from "../../components/molecules/HealthBadge";
import {
  api,
  type AccountQuotaRow,
  type AccountQuotaWindow,
  type AccountSubscription,
  type Connection,
  type ModelProvider,
  type OAuthProvider,
  type QuotaPingMode,
  type QuotaPingRow,
  type UsageSummaryByConnection,
} from "../../lib/api";
import { duration } from "../../lib/format";
import { qk } from "../../lib/queries";
import { refusalView, type RefusalView } from "../../lib/refusal";

export type ProviderNotice = { tone: "pos" | "neg"; text: string };

/**
 * A provider-level filter: `connected` is every card, `healthy` the ready ones, `attention`
 * those with an account that needs an operator, `disabled` those an operator switched off.
 * Cards are filtered whole, so a card never lists a subset of its accounts.
 */
export type ProviderFilter = "connected" | "healthy" | "attention" | "disabled";

/**
 * The one-line, plain-language answer to "is this okay?" under a provider name. The
 * sentence states what it is first (accounts, readiness) and appends the single most
 * pressing fact (a limit, a reset, an outage), never a column of codes.
 */
export type ProviderSentence = {
  text: string;
  /** `info` is "ready, with a limit worth knowing" — never amber, which means act. */
  tone: "pos" | "info" | "warn" | "neg" | "muted";
};

export function providerSentence(group: ProviderGroup, isAdmin: boolean): ProviderSentence {
  if (group.disabled > 0 && group.expected === 0) {
    return {
      text: `Turned off · ${group.disabled} account${group.disabled === 1 ? "" : "s"} disabled`,
      tone: "muted",
    };
  }
  const total = group.connections.length;
  const accounts = `${total} account${total === 1 ? "" : "s"}`;
  if (group.ready === 0) {
    const cause =
      group.refused > 0
        ? "the provider refused an account"
        : group.overQuota > 0
          ? "out of quota"
          : group.backoff > 0
            ? "unreachable right now"
            : "not ready";
    return { text: `Not ready · ${cause}`, tone: "neg" };
  }
  const plain =
    group.ready === group.expected
      ? `Ready · ${accounts}`
      : `Partly ready · ${group.ready} of ${group.expected} usable`;
  const off =
    group.disabled > 0
      ? ` · ${group.disabled} account${group.disabled === 1 ? "" : "s"} off`
      : "";
  if (group.ready === group.expected && group.disabled === 0) {
    const worst = group.subscription;
    if (isAdmin && worst && worst.percent >= 70) {
      const reset = worst.resets_at ? until(worst.resets_at) : "";
      return {
        text: `${plain} · ${worst.percent}% of quota used${reset ? `, resets in ${reset}` : ""}`,
        tone: worst.percent >= 90 ? "warn" : "info",
      };
    }
    return { text: plain, tone: "pos" };
  }
  if (group.backoff === 0 && group.refused === 0 && group.overQuota === 0) {
    return { text: `${plain}${off}`, tone: "pos" };
  }
  const problems: string[] = [];
  if (group.backoff > 0) problems.push(`${group.backoff} unreachable`);
  if (group.refused > 0) problems.push(`${group.refused} refused`);
  if (group.overQuota > 0) problems.push(`${group.overQuota} out of quota`);
  return {
    text: `${plain}${off} · ${problems.join(", ")}`,
    tone: group.overQuota > 0 || group.refused > 0 ? "neg" : "warn",
  };
}

/** Human sentence for one account, used when the account list is expanded. */
export function accountSentence(status: AccountStatus, traffic?: AccountTraffic24h): string {
  const usage =
    traffic && traffic.requests > 0
      ? `${traffic.requests.toLocaleString()} request${traffic.requests === 1 ? "" : "s"} in 24h`
      : "No requests in 24h";
  const errors = traffic && traffic.errors > 0 ? `, ${traffic.errors.toLocaleString()} errors` : "";
  if (status.kind === "ready") return `Ready · ${usage}${errors}`;
  if (status.kind === "disabled") return "Off · excluded from routing";
  return `${status.label}${status.reason ? ` · ${status.reason}` : ""}`;
}

export type AccountTraffic24h = {
  requests: number;
  errors: number;
};

export type ConnectionTestResult = {
  id: string;
  ok: boolean;
  status: number;
  latency_ms: number;
  error?: string;
};

/**
 * Single source of truth for whether one account can take traffic right now, and why not.
 * Both the provider header readiness readout, KPI summary, each account row, the hover/focus
 * preview, and the connection drawer read from this function so they can never disagree.
 */
export type AccountStatus =
  | {
      kind: "refusal";
      ready: false;
      label: string;
      tone: "neg" | "warn" | "info";
      health: HealthStatus;
      reason: string;
      refusal: RefusalView;
    }
  | {
      kind:
        | "disabled"
        | "backoff"
        | "no_endpoint"
        | "over_quota"
        | "plan_at_limit"
        | "warning"
        | "last_error"
        | "ready";
      ready: boolean;
      label: string;
      tone: "pos" | "muted" | "warn" | "neg";
      health: HealthStatus;
      reason: string | null;
    };

/**
 * Grouping is a read projection over the connection list, never a routing entity: the
 * accounts inside a group stay ordinary rows under one provider header, and every provider
 * keeps its own header so two connections are never merged into one row.
 */
export type ProviderGroup = {
  /** `provider` for a built-in provider, `custom:<name|id>` for one endpoint of its own. */
  key: string;
  /** Provider id used for the icon; `null` for a custom endpoint with no protocol row. */
  provider: string | null;
  label: string;
  code: string | null;
  connections: Connection[];
  /** Distinct priorities, ascending, with how many accounts sit at each. */
  tiers: Array<{ priority: number; count: number }>;
  /** Accounts whose `accountStatus().ready` is true right now. */
  ready: number;
  /** Accounts with `is_active === true`. */
  enabled: number;
  /** Accounts disabled on purpose by an operator (`!is_active` with no provider refusal). */
  disabled: number;
  /** Non-disabled accounts expected to be ready (`enabled + refused`). */
  expected: number;
  backoff: number;
  overQuota: number;
  /** Accounts a provider retired (`refusal` set): out of rotation, and not by an operator. */
  refused: number;
  /** Worst subscription window in the group; `null` when no account reported one. */
  subscription: { percent: number; resets_at: number | null; reporting: number } | null;
  /** `"disabled"` when every account in the group was turned off on purpose (never red/amber). */
  state: "healthy" | "degraded" | "down" | "disabled";
};

export type TightestLimitSummary = {
  connectionId: string;
  accountName: string;
  providerLabel: string;
  windowLabel: string;
  percent: number;
  resetsAt: number | null;
};

export type ProvidersSummary = {
  total: number;
  ready: number;
  enabled: number;
  disabled: number;
  refused: number;
  /** Accounts not disabled on purpose (`total - disabled`). */
  expected: number;
  /** Accounts needing operator attention (never includes intentionally disabled accounts). */
  attention: number;
  /** Severe non-ready accounts (backoff, refused, exhausted, no endpoint). */
  severe: number;
  attentionDetail: string;
  tightest: TightestLimitSummary | null;
  traffic24h: {
    requests: number;
    errors: number;
    errorRate: number;
  };
};

/**
 * The provider each card would get from "Add account": a sign-in provider when it has an
 * OAuth flow, an API-key provider when it takes a key, nothing when this build cannot add
 * one (so the card shows no Add button rather than a dead one).
 */
export function providerOptions(
  provider: ModelProvider | undefined,
  oauth: OAuthProvider | undefined,
): Array<{ id: string; display: string; kind: "oauth" | "key" }> {
  const out: Array<{ id: string; display: string; kind: "oauth" | "key" }> = [];
  if (oauth) out.push({ id: oauth.id, display: oauth.display, kind: "oauth" });
  if (provider && provider.kind !== "oauth") {
    out.push({ id: provider.id, display: provider.display, kind: "key" });
  }
  return out;
}

/**
 * PATCH body that replaces the API key of a keyed connection. Absent from the drawer for
 * a sign-in account: its credential is a token pair the gateway rotates itself, and a key
 * pasted over it would make the row lie about how it authenticates.
 */
export function apiKeyReplacement(key: string): { credentials: { api_key: string } } | null {
  const trimmed = key.trim();
  return trimmed === "" ? null : { credentials: { api_key: trimmed } };
}

export const PLACEHOLDER_LABELS = /^(resource|your-resource|placeholder)$/i;

/**
 * Extra names an operator might type for a provider. The matcher also covers the display
 * name and the id, so this table only holds the words neither of those contains
 * ("google" for Gemini, "chatgpt" for Codex, "microsoft" for Azure, …).
 */
export const PROVIDER_ALIASES: Record<string, string[]> = {
  claude: ["anthropic"],
  anthropic: ["claude"],
  codex: ["chatgpt", "openai"],
  openai: ["gpt", "chatgpt", "codex"],
  azure: ["microsoft", "openai"],
  gemini: ["google"],
  "google-antigravity": ["google", "antigravity", "gemini", "cloud code assist"],
  github: ["copilot"],
  xai: ["grok"],
  mistral: ["mixtral"],
  ollama: ["local"],
  jina: ["rerank"],
};

export const DIALECT_LABELS: Record<string, string> = {
  openai_chat: "Chat",
  openai_responses: "Responses",
  anthropic_messages: "Messages",
  openai_embeddings: "Embeddings",
  google_code_assist: "Google Code Assist",
  qoder_agent: "Qoder agent",
};

/** Wire spelling to the phrase the row shows; the server stores `daily` and friends. */
export const WINDOW_LABEL: Record<AccountQuotaWindow, string> = {
  daily: "daily",
  weekly: "weekly",
  monthly: "monthly",
};

export const QUOTA_WINDOWS: ReadonlyArray<{ id: AccountQuotaWindow; label: string }> = [
  { id: "daily", label: "daily" },
  { id: "weekly", label: "weekly" },
  { id: "monthly", label: "monthly" },
];

export const PING_MODES: ReadonlyArray<{ id: QuotaPingMode; label: string; hint: string }> = [
  { id: "off", label: "Off", hint: "No automatic pings. The account is never warmed." },
  {
    id: "reset",
    label: "On reset",
    hint: "Ping just after the provider's current quota window resets.",
  },
  {
    id: "schedule",
    label: "Schedule",
    hint: "Ping at fixed wall-clock slots in the chosen timezone.",
  },
  {
    id: "smart",
    label: "Smart",
    hint: "Stagger resets across accounts by usage; idle accounts are skipped.",
  },
];

/**
 * A keyed OpenAI/Anthropic account pointed at an operator-supplied origin. Until an
 * endpoint carries its own identity, this pairing is the only thing that separates it
 * from the real provider, so every surface that names the provider checks it.
 */
export function isCustomConnection(connection: Connection): boolean {
  return (
    connection.auth_type === "api_key" &&
    connection.base_url_override !== null &&
    (connection.provider === "openai" || connection.provider === "anthropic")
  );
}

/** A custom endpoint is its own source even though it borrows a protocol provider id. */
export function groupKeyOf(connection: Connection): string {
  return isCustomConnection(connection)
    ? `custom:${connection.name ?? connection.id}`
    : connection.provider;
}

/** The wire shape a protocol id speaks, as the table names it. */
export function protocolLabel(protocol: string): string {
  return protocol === "anthropic" ? "Anthropic Messages" : "OpenAI-compatible";
}

/** The host an override points at, for the account cell. */
export function hostOf(url: string): string {
  return url
    .replace(/^[a-z][a-z0-9+.-]*:\/\//i, "")
    .replace(/[/?#].*$/, "")
    .replace(/^[^@]*@/, "")
    .replace(/:\d+$/, "");
}

/**
 * What identifies an account when it carries no operator name: the sign-in's email, the
 * Cloud Code Assist project, and the provider's own OAuth marker. The first one is the
 * placeholder the rename field offers, because it is what the row would read anyway.
 */
export function derivedIdentifiers(
  connection: Connection,
  display: string,
): Array<{ text: string; mono?: boolean }> {
  const parts: Array<{ text: string; mono?: boolean }> = [];
  if (connection.email) parts.push({ text: connection.email });
  if (connection.project_id) {
    parts.push({ text: `project ${connection.project_id}`, mono: true });
  }
  if (connection.auth_type === "oauth") parts.push({ text: `${display} (OAuth)` });
  return parts;
}

/** Preferred display label for one connection across headers, menus, and confirmations. */
export function connectionLabel(connection: Connection, display: string): string {
  if (isCustomConnection(connection)) {
    return connection.name ?? `${display} (custom URL)`;
  }
  return connection.name ?? derivedIdentifiers(connection, display)[0]?.text ?? display;
}

/** Human-readable summary of stored credential tokens on a connection. */
export function credentialShapeOf(connection: Connection): string {
  return [
    connection.auth_type,
    connection.has_access_token ? "access token" : null,
    connection.has_refresh_token ? "refresh token" : null,
    connection.has_api_key ? "api key" : null,
  ]
    .filter(Boolean)
    .join(" · ");
}

/** An account that cannot route until it names its own resource (a legacy Azure row). */
export function needsBaseUrlOverride(
  connection: Connection,
  providers?: ModelProvider[],
): boolean {
  return (
    providers?.find((p) => p.id === connection.provider)?.requires_base_url_override === true &&
    !connection.base_url_override
  );
}

/**
 * Why this base URL cannot be saved, or `null`.
 *
 * Mirrors the server, so the form never offers a value the API will refuse: a provider
 * whose manifest host is a placeholder (Azure) needs the deployment's own resource URL,
 * and `RESOURCE` copied from the manifest is refused for every provider because the host
 * cannot resolve.
 */
export function baseUrlProblem(value: string, required: boolean): string | null {
  const trimmed = value.trim();
  if (trimmed === "") {
    return required
      ? "This provider needs the resource endpoint of your own deployment."
      : null;
  }
  if (/<\s*resource\s*>/i.test(trimmed)) {
    return "Replace <resource> with your own resource name.";
  }
  const label = hostOf(trimmed).split(".")[0] ?? "";
  if (PLACEHOLDER_LABELS.test(label)) {
    return "The manifest host is a placeholder — replace RESOURCE with your own resource name.";
  }
  return null;
}

/**
 * Why this shorthand cannot be saved, or `null`.
 *
 * Mirrors `valid_code` on the server — 1–16 characters of a–z, 0–9, “-”, “_” or “.”, not
 * starting with “.”, not the reserved “rrouter” — plus the instance-wide uniqueness the
 * provider list already answers.
 */
export function codeProblemFor(
  value: string,
  providers: ModelProvider[],
  selfId: string | null,
): string | null {
  const text = value.trim();
  if (text === "") return "required";
  if (text.length > 16) return "16 characters at most";
  if (!/^[a-z0-9._-]+$/.test(text)) return "lowercase a–z, 0–9, “-”, “_” or “.” only";
  if (text.startsWith(".")) return "cannot start with “.”";
  if (text === "rrouter") return "“rrouter” is reserved";
  const taken = providers.find((p) => p.id !== selfId && p.code === text);
  return taken ? `already the code of ${taken.display}` : null;
}

/** Whole number inside `min..=max`; the server clamps, but a silent clamp would hide typos. */
export function intProblem(value: string, min: number, max: number): string | null {
  const text = value.trim();
  if (text === "") return "required";
  if (!/^-?\d+$/.test(text)) return "whole number only";
  const n = Number(text);
  if (!Number.isSafeInteger(n) || n < min || n > max) return `must be ${min}…${max}`;
  return null;
}

/**
 * Why this egress proxy cannot be saved, or `null`.
 *
 * Empty is not an error: the raw URL never comes back to the browser, so an untouched field
 * means "keep the saved one". Clearing is the drawer's own explicit control, which sends an
 * empty value the server reads as a removal.
 */
export function proxyProblem(value: string): string | null {
  const text = value.trim();
  if (text === "") return null;
  if (!/^(https?|socks5h?):\/\/[^\s/]+$/i.test(text)) {
    return "scheme://[user:pass@]host[:port], with scheme http, https, socks5 or socks5h";
  }
  return null;
}

/** Optional per-account cap: empty means the free-mode routing default, never zero. */
export function freeRpmProblem(value: string): string | null {
  const text = value.trim();
  if (text === "") return null;
  return /^\d+$/.test(text) && Number(text) >= 1
    ? null
    : "requests per minute as a whole number, or empty for the document default";
}

/** Non-negative whole number or empty (unlimited) for operator quota counters. */
export function countProblem(value: string): string | null {
  const text = value.trim();
  if (text === "") return null;
  return /^\d+$/.test(text) && Number.isSafeInteger(Number(text)) ? null : "whole number only";
}

/** The protocol and native surface, not a line every API-key provider shares. */
export function providerMeta(provider: ModelProvider): string {
  const parts: string[] = [];
  const dialect =
    provider.compaction === "openai_responses"
      ? "Responses"
      : provider.compaction === "anthropic_messages"
        ? "Messages"
        : provider.dialect
          ? (DIALECT_LABELS[provider.dialect] ?? provider.dialect)
          : "API key";
  parts.push(dialect);
  if (provider.kind === "local") parts.push("local");
  if (provider.compaction === "openai_responses") parts.push("server compact");
  else if (provider.compaction === "anthropic_messages") parts.push("compaction beta");
  if (provider.requires_base_url_override) parts.push("custom URL required");
  if (!provider.listed) parts.push("manual models");
  return parts.join(" · ");
}

/** Does a provider answer to this search? Display, id and aliases, prefix-matched. */
export function matchesProvider(
  needle: string,
  provider: { id: string; display: string },
): boolean {
  if (needle === "") return true;
  if (
    provider.display.toLowerCase().includes(needle) ||
    provider.id.toLowerCase().includes(needle)
  ) {
    return true;
  }
  return (PROVIDER_ALIASES[provider.id] ?? []).some(
    (alias) => alias.startsWith(needle) || needle.startsWith(alias),
  );
}

/** Both spellings render the same: the spec id and the short alias. */
export function isAntigravity(id: string): boolean {
  return id === "google-antigravity" || id === "antigravity";
}

/** Resolve published default URL if a provider manifest declares a non-placeholder host. */
export function publishedDefaultUrl(provider?: ModelProvider): string | null {
  const own = provider?.base_url?.trim() ?? "";
  if (own !== "") return own;
  const candidate = provider?.base_url_example?.trim() ?? "";
  if (candidate === "") return null;
  if (/[<>]/.test(candidate)) return null;
  if (PLACEHOLDER_LABELS.test(hostOf(candidate).split(".")[0] ?? "")) return null;
  return candidate;
}

/** Friendly display name for subscription and auto-ping provider ids. */
export function providerLabel(provider: string): string {
  return provider === "claude"
    ? "Claude"
    : provider === "codex"
      ? "Codex"
      : provider === "github"
        ? "GitHub Copilot"
        : provider === "opencode-go"
          ? "OpenCode Go"
          : provider === "opencode"
            ? "OpenCode"
            : provider;
}

/**
 * Share of the tightest limit this account has used this window, or `null` when every
 * counter is unlimited. A bar is a magnitude, so the callers keep the fill neutral until
 * the number itself is a warning.
 */
export function quotaUsagePercent(row?: AccountQuotaRow): number | null {
  if (!row) return null;
  const ratios = [
    row.max_requests > 0 ? row.requests / row.max_requests : null,
    row.max_tokens > 0 ? row.tokens / row.max_tokens : null,
    row.max_cost_micros > 0 ? row.cost_micros / row.max_cost_micros : null,
  ].filter((value): value is number => value !== null);
  if (ratios.length === 0) return null;
  return Math.min(100, Math.round(Math.max(...ratios) * 100));
}

/** Worst plan window the provider reported for this account, or `null` when silent. */
export function subscriptionUsagePercent(row?: AccountSubscription): number | null {
  if (!row || row.windows.length === 0) return null;
  return Math.round(Math.max(...row.windows.map((window) => window.used_percent)));
}

/** "1h 35m" until the window rolls over; empty when the provider gave no timestamp. */
export function until(ts: number | null, nowSec = Math.floor(Date.now() / 1000)): string {
  if (!ts) return "";
  const secs = Math.max(0, ts - nowSec);
  return duration(Math.max(60, secs));
}

export function parseAnchor(value: string): number | null {
  const match = /^([01]\d|2[0-3]):([0-5]\d)$/.exec(value.trim());
  return match ? Number(match[1]) * 60 + Number(match[2]) : null;
}

export function clockLabel(minutes: number): string {
  const day = Math.floor(minutes / 1440);
  const normalized = ((minutes % 1440) + 1440) % 1440;
  const hh = String(Math.floor(normalized / 60)).padStart(2, "0");
  const mm = String(normalized % 60).padStart(2, "0");
  return `${hh}:${mm}${day > 0 ? ` +${day}d` : ""}`;
}

export function validTimezone(zone: string): boolean {
  const value = zone.trim();
  if (!value) return false;
  try {
    new Intl.DateTimeFormat("en-US", { timeZone: value }).format();
    return true;
  } catch {
    return false;
  }
}

export function phaseLabel(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds <= 0) return "0m";
  const total = Math.round(seconds / 60);
  const hours = Math.floor(total / 60);
  const minutes = total % 60;
  return hours > 0 ? `${hours}h${minutes > 0 ? ` ${minutes}m` : ""}` : `${Math.max(1, minutes)}m`;
}

export function pingBadgeText(ping: QuotaPingRow): string {
  if (ping.mode === "off") return "auto-ping off";
  if (ping.mode === "reset") return "on reset";
  return `${ping.mode} ${ping.slots_per_day}/day`;
}

function capitalizeFirst(text: string): string {
  return text ? text.charAt(0).toUpperCase() + text.slice(1) : text;
}

function shortWindowLabel(label: string, windowSeconds: number | null): string {
  if (windowSeconds === 5 * 3600 || /^5[\s-]?hour$/i.test(label.trim())) {
    return "5h";
  }
  return label;
}

function tightestLimitForAccount(
  quota?: AccountQuotaRow,
  subscription?: AccountSubscription,
): { label: string; percent: number; resetsAt: number | null } | null {
  let best: { label: string; percent: number; resetsAt: number | null } | null = null;
  const consider = (label: string, percent: number, resetsAt: number | null) => {
    if (!Number.isFinite(percent)) return;
    if (best === null || percent > best.percent) {
      best = { label, percent, resetsAt };
    }
  };
  if (quota) {
    const win = capitalizeFirst(WINDOW_LABEL[quota.window] ?? quota.window);
    if (quota.max_requests > 0) {
      consider(`${win} req`, (quota.requests / quota.max_requests) * 100, quota.resets_at);
    }
    if (quota.max_tokens > 0) {
      consider(`${win} tok`, (quota.tokens / quota.max_tokens) * 100, quota.resets_at);
    }
    if (quota.max_cost_micros > 0) {
      consider(`${win} spend`, (quota.cost_micros / quota.max_cost_micros) * 100, quota.resets_at);
    }
  }
  for (const window of subscription?.windows ?? []) {
    consider(
      shortWindowLabel(window.label, window.window_seconds),
      window.used_percent,
      window.resets_at,
    );
  }
  return best;
}

/**
 * Standing readiness of one connection: what stops it routing, else whether it is nearing
 * a limit (`Warning`, still ready) or `Healthy`.
 *
 * A provider refusal comes first and reads as its own badge, because it is not the same
 * fact as an operator disable. Live breaker state has no API yet (S1); persisted probe
 * backoff, missing required base URL, exhausted operator quota, and a plan window at the
 * 90% reserve threshold are every signal the page can observe honestly.
 */
export function accountStatus(
  connection: Connection,
  quota?: AccountQuotaRow,
  subscription?: AccountSubscription,
  providers?: ModelProvider[],
): AccountStatus {
  const refusal = refusalView(connection.refusal);
  if (refusal) {
    return {
      kind: "refusal",
      ready: false,
      label: capitalizeFirst(refusal.label),
      tone: refusal.tone,
      health: refusal.tone === "neg" ? "failing" : refusal.tone === "warn" ? "warning" : "info",
      reason: refusal.description,
      refusal,
    };
  }
  if (!connection.is_active) {
    return {
      kind: "disabled",
      ready: false,
      label: "Disabled",
      tone: "muted",
      health: "disabled",
      reason: "Disabled by an operator; excluded from routing.",
    };
  }
  if (connection.backoff_level > 0) {
    return {
      kind: "backoff",
      ready: false,
      label: "Backoff",
      tone: "neg",
      health: "failing",
      reason: connection.last_error ?? "In probe backoff after a recent upstream error.",
    };
  }
  if (needsBaseUrlOverride(connection, providers)) {
    return {
      kind: "no_endpoint",
      ready: false,
      label: "No endpoint",
      tone: "warn",
      health: "warning",
      reason: "Resource base URL is not configured yet.",
    };
  }
  if (quota?.exhausted) {
    const reset = until(quota.resets_at);
    return {
      kind: "over_quota",
      ready: false,
      label: "Exhausted",
      tone: "neg",
      health: "exhausted",
      reason: `Operator ${WINDOW_LABEL[quota.window] ?? quota.window} cap reached${reset ? ` · resets in ${reset}` : ""}.`,
    };
  }
  const worstWindow =
    subscription && subscription.windows.length > 0
      ? subscription.windows.reduce((worst, window) =>
          window.used_percent > worst.used_percent ? window : worst,
        )
      : null;
  if (worstWindow && worstWindow.used_percent >= 90) {
    const reset = until(worstWindow.resets_at);
    const exhausted = worstWindow.used_percent >= 100;
    return {
      kind: "plan_at_limit",
      ready: false,
      label: exhausted ? "Exhausted" : "Warning",
      tone: exhausted ? "neg" : "warn",
      health: exhausted ? "exhausted" : "warning",
      reason: `${worstWindow.label} at ${Math.round(worstWindow.used_percent)}%${reset ? ` · resets in ${reset}` : ""}.`,
    };
  }
  const tightest = tightestLimitForAccount(quota, subscription);
  if (tightest && tightest.percent >= 70) {
    const reset = until(tightest.resetsAt);
    return {
      kind: "warning",
      ready: true,
      label: "Warning",
      tone: "warn",
      health: "warning",
      reason: `${tightest.label} at ${Math.round(tightest.percent)}%${reset ? ` · resets in ${reset}` : ""}.`,
    };
  }
  const staleError = connection.last_error?.trim() || null;
  if (staleError) {
    return {
      kind: "last_error",
      ready: true,
      label: "Last error",
      tone: "warn",
      health: "warning",
      reason: staleError,
    };
  }
  return {
    kind: "ready",
    ready: true,
    label: "Healthy",
    tone: "pos",
    health: "healthy",
    reason: null,
  };
}

/**
 * Whether an account needs operator attention. Intentionally disabled accounts never do,
 * and every account that needs attention carries a non-Healthy status pill.
 */
export function accountNeedsAttention(_connection: Connection, status: AccountStatus): boolean {
  if (status.kind === "disabled") return false;
  return !status.ready || status.health !== "healthy";
}

/** Fold 24h usage summary rows by `connection_id` for the Providers 24h column. */
export function buildTrafficByConnection(
  rows?: ReadonlyArray<UsageSummaryByConnection>,
): Map<string, AccountTraffic24h> {
  const map = new Map<string, AccountTraffic24h>();
  for (const row of rows ?? []) {
    if (!row.connection_id) continue;
    const prev = map.get(row.connection_id);
    if (prev) {
      prev.requests += row.requests;
      prev.errors += row.errors;
    } else {
      map.set(row.connection_id, { requests: row.requests, errors: row.errors });
    }
  }
  return map;
}

/**
 * Pick the cards one summary tile stands for. The whole card is kept, so its sentence and
 * counts still describe every account it has; only which cards are shown changes.
 */
export function filterProviderGroups(
  groups: ProviderGroup[],
  filter: ProviderFilter,
): ProviderGroup[] {
  const match: Record<ProviderFilter, (group: ProviderGroup) => boolean> = {
    connected: () => true,
    healthy: (group) => group.state === "healthy",
    attention: (group) => group.state === "degraded" || group.state === "down",
    disabled: (group) => group.state === "disabled",
  };
  return groups.filter(match[filter]);
}

/** Pure KPI strip rollup across all visible accounts, limits, and 24h traffic. */
export function summarizeProviders(
  accounts: Connection[],
  providers?: ModelProvider[],
  quotas?: AccountQuotaRow[],
  subscriptions?: AccountSubscription[],
  trafficRows?: ReadonlyArray<UsageSummaryByConnection>,
): ProvidersSummary {
  const quotaBy = new Map((quotas ?? []).map((row) => [row.connection_id, row]));
  const subscriptionBy = new Map((subscriptions ?? []).map((row) => [row.connection_id, row]));
  const providerDisplay = new Map((providers ?? []).map((p) => [p.id, p.display]));

  let ready = 0;
  let enabled = 0;
  let disabled = 0;
  let refused = 0;
  let attention = 0;
  let severe = 0;
  let backoffCount = 0;
  let exhaustedCount = 0;
  let noEndpointCount = 0;
  let warningCount = 0;
  let staleErrorCount = 0;
  let tightest: TightestLimitSummary | null = null;

  for (const account of accounts) {
    const quota = quotaBy.get(account.id);
    const subscription = subscriptionBy.get(account.id);
    const status = accountStatus(account, quota, subscription, providers);

    if (status.ready) ready += 1;
    if (account.is_active) enabled += 1;
    if (status.kind === "disabled") disabled += 1;
    if (status.kind === "refusal") refused += 1;

    if (accountNeedsAttention(account, status)) {
      attention += 1;
      if (status.kind === "backoff") {
        backoffCount += 1;
        severe += 1;
      } else if (status.kind === "refusal") {
        severe += 1;
      } else if (status.kind === "over_quota" || status.kind === "plan_at_limit") {
        exhaustedCount += 1;
        severe += 1;
      } else if (status.kind === "no_endpoint") {
        noEndpointCount += 1;
        severe += 1;
      } else if (status.kind === "warning") {
        warningCount += 1;
      } else if (status.kind === "last_error") {
        staleErrorCount += 1;
      }
    }

    if (account.is_active) {
      const limit = tightestLimitForAccount(quota, subscription);
      if (limit && (tightest === null || limit.percent > tightest.percent)) {
        const display = providerDisplay.get(account.provider) ?? providerLabel(account.provider);
        tightest = {
          connectionId: account.id,
          accountName: connectionLabel(account, display),
          providerLabel: display,
          windowLabel: limit.label,
          percent: limit.percent,
          resetsAt: limit.resetsAt,
        };
      }
    }
  }

  const reasons: string[] = [];
  if (backoffCount > 0) reasons.push(`${backoffCount} backoff`);
  if (refused > 0) reasons.push(`${refused} refused`);
  if (exhaustedCount > 0) reasons.push(`${exhaustedCount} at limit`);
  if (noEndpointCount > 0) reasons.push(`${noEndpointCount} no endpoint`);
  if (warningCount > 0) reasons.push(`${warningCount} limit${warningCount === 1 ? "" : "s"}`);
  if (staleErrorCount > 0) reasons.push(`${staleErrorCount} error${staleErrorCount === 1 ? "" : "s"}`);

  let requests = 0;
  let errors = 0;
  for (const row of trafficRows ?? []) {
    requests += row.requests;
    errors += row.errors;
  }

  return {
    total: accounts.length,
    ready,
    enabled,
    disabled,
    refused,
    expected: accounts.length - disabled,
    attention,
    severe,
    attentionDetail: reasons.length > 0 ? reasons.join(" · ") : "All enabled healthy",
    tightest,
    traffic24h: {
      requests,
      errors,
      errorRate: requests > 0 ? (errors / requests) * 100 : 0,
    },
  };
}

/**
 * Build one summary entry per provider or custom endpoint in server order
 * (`provider, priority, created_at`). Everything here is a read over queries the page
 * already makes; nothing is routed differently.
 */
export function buildProviderGroups(
  accounts: Connection[],
  providers?: ModelProvider[],
  quotas?: AccountQuotaRow[],
  subscriptions?: AccountSubscription[],
): ProviderGroup[] {
  const quotaBy = new Map((quotas ?? []).map((row) => [row.connection_id, row]));
  const subscriptionBy = new Map(
    (subscriptions ?? []).map((row) => [row.connection_id, row]),
  );
  const order: string[] = [];
  const byKey = new Map<string, Connection[]>();
  for (const account of accounts) {
    const key = groupKeyOf(account);
    const list = byKey.get(key);
    if (list) list.push(account);
    else {
      byKey.set(key, [account]);
      order.push(key);
    }
  }
  return order.map((key) => {
    const connections = byKey.get(key) ?? [];
    const first = connections[0]!;
    const provider = first.provider;
    const custom = key.startsWith("custom:");
    const providerRow = custom ? undefined : providers?.find((row) => row.id === provider);
    const label = custom
      ? (first.name ?? `${providerRow?.display ?? provider} (custom URL)`)
      : (providerRow?.display ?? provider);
    const tiers = Array.from(
      connections.reduce((counts, account) => {
        counts.set(account.priority, (counts.get(account.priority) ?? 0) + 1);
        return counts;
      }, new Map<number, number>()),
      ([priority, count]) => ({ priority, count }),
    ).sort((a, b) => a.priority - b.priority);

    let ready = 0;
    let enabled = 0;
    let disabled = 0;
    let overQuota = 0;
    let backoff = 0;
    let refused = 0;
    let worstPercent = 0;
    let worstResets: number | null = null;
    let reporting = 0;

    for (const account of connections) {
      const quota = quotaBy.get(account.id);
      const subscription = subscriptionBy.get(account.id);
      const status = accountStatus(account, quota, subscription, providers);
      if (status.ready) ready += 1;
      if (account.is_active) enabled += 1;
      if (status.kind === "disabled") disabled += 1;
      if (quota?.exhausted) overQuota += 1;
      if (account.backoff_level > 0) backoff += 1;
      if (status.kind === "refusal") refused += 1;
      const windows = subscription?.windows ?? [];
      if (windows.length > 0) reporting += 1;
      const accountWorst = windows.reduce((max, window) => Math.max(max, window.used_percent), 0);
      if (accountWorst > worstPercent) {
        worstPercent = accountWorst;
        worstResets =
          windows.find((window) => window.used_percent === accountWorst)?.resets_at ?? null;
      }
    }

    const expected = connections.length - disabled;
    const state: ProviderGroup["state"] =
      expected === 0
        ? "disabled"
        : ready === 0
          ? "down"
          : ready < expected
            ? "degraded"
            : "healthy";

    return {
      key,
      provider,
      label,
      code: providerRow?.code ?? null,
      connections,
      tiers,
      ready,
      enabled,
      disabled,
      expected,
      backoff,
      overQuota,
      refused,
      subscription:
        reporting === 0
          ? null
          : {
              percent: Math.round(worstPercent),
              resets_at: worstResets,
              reporting,
            },
      state,
    };
  });
}

/** Synthesize an unconfigured quota row when the server list omits a connection. */
export function emptyQuotaRowFor(connection: Connection): AccountQuotaRow {
  return {
    connection_id: connection.id,
    name: connection.name,
    provider: connection.provider,
    is_active: connection.is_active,
    window: "daily",
    max_requests: 0,
    max_tokens: 0,
    max_cost_micros: 0,
    requests: 0,
    tokens: 0,
    cost_micros: 0,
    exhausted: false,
    resets_at: null,
  };
}

/** Shared invalidation helper for mutations across Providers sub-modules. */
export function useInvalidateProviders() {
  const client = useQueryClient();
  const refreshTimer = useRef<number | null>(null);

  useEffect(
    () => () => {
      if (refreshTimer.current !== null) {
        window.clearTimeout(refreshTimer.current);
      }
    },
    [],
  );

  const invalidate = useCallback(() => {
    void client.invalidateQueries({ queryKey: qk.connections });
    void client.invalidateQueries({ queryKey: qk.models });
    void client.invalidateQueries({ queryKey: qk.modelProviders });
    void client.invalidateQueries({ queryKey: qk.system });
  }, [client]);

  /**
   * A new connection's model list is fetched by the gateway in the background, so the
   * immediate invalidation can land before it exists. One follow-up read picks it up; this
   * is a single retry, not an interval.
   */
  const refreshRegistrySoon = useCallback(() => {
    if (refreshTimer.current !== null) {
      window.clearTimeout(refreshTimer.current);
    }
    refreshTimer.current = window.setTimeout(() => {
      refreshTimer.current = null;
      void client.invalidateQueries({ queryKey: qk.models });
      void client.invalidateQueries({ queryKey: qk.modelProviders });
    }, 1500);
  }, [client]);

  return { invalidate, refreshRegistrySoon };
}

/** Connection test, optimistic toggle, and removal mutations shared on the Providers page. */
export function useConnectionMutations(onNotice: (notice: ProviderNotice) => void) {
  const client = useQueryClient();
  const { invalidate, refreshRegistrySoon } = useInvalidateProviders();
  const [lastTest, setLastTest] = useState<ConnectionTestResult | null>(null);
  const [testsById, setTestsById] = useState<Record<string, ConnectionTestResult>>({});

  const recordTest = useCallback((result: ConnectionTestResult) => {
    setLastTest(result);
    setTestsById((prev) => ({ ...prev, [result.id]: result }));
  }, []);

  const remove = useMutation({
    mutationFn: (id: string) => api.del<{ deleted: boolean }>(`/api/connections/${id}`),
    onSuccess: invalidate,
    onError: (e) => onNotice({ tone: "neg", text: e instanceof Error ? e.message : String(e) }),
  });

  const toggle = useMutation({
    mutationFn: ({ id, active }: { id: string; active: boolean }) =>
      api.patch<Connection>(`/api/connections/${id}`, { is_active: active }),
    onMutate: async ({ id, active }) => {
      await client.cancelQueries({ queryKey: qk.connections });
      const previous = client.getQueriesData<Connection[]>({ queryKey: qk.connections });
      client.setQueriesData<Connection[]>({ queryKey: qk.connections }, (old) =>
        old?.map((conn) => (conn.id === id ? { ...conn, is_active: active } : conn)),
      );
      return { previous };
    },
    onError: (e, _vars, ctx) => {
      for (const [key, data] of ctx?.previous ?? []) client.setQueryData(key, data);
      onNotice({ tone: "neg", text: e instanceof Error ? e.message : String(e) });
    },
    onSettled: invalidate,
  });

  const test = useMutation({
    mutationFn: (id: string) =>
      api.post<{ ok: boolean; status: number; latency_ms: number; error?: string }>(
        `/api/connections/${id}/test`,
      ),
    onSuccess: (res, id) => {
      recordTest({ id, ...res });
    },
    onError: (error, id) => {
      const msg = error instanceof Error ? error.message : String(error);
      recordTest({ id, ok: false, status: 0, latency_ms: 0, error: msg });
    },
  });

  return { invalidate, refreshRegistrySoon, remove, toggle, test, lastTest, testsById, recordTest };
}
