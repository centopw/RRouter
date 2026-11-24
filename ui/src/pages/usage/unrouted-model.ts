/**
 * Pure model for the "Requests that never reached a provider" section on `/usage`.
 *
 * Requests the gateway rejects before dispatch are recorded with `provider === ""` (see
 * `resolveProviderDisplay`). They are not an account, so they no longer render as a
 * provider card: this module folds them into one summary with a per-reason breakdown.
 *
 * Reason source, best first:
 * 1. `api`    — a per-reason breakdown from the gateway (typed optional, `UnroutedApiBreakdown`).
 * 2. `recent` — stable reason ids on recent usage events (`unrouted_reason`), or, failing
 *               that, the event's `error` text classified by `classifyUnroutedError`.
 * 3. `none`   — no reason data at all; the section shows the requested models only.
 *
 * No React, so every edge is pinned in `ui/tests/usage-board.test.ts`.
 */

import type { UsageEvent, UsageSummaryByConnection } from "../../lib/api";

/** Stable machine-readable reasons the gateway may report; unknown ids pass through. */
export type UnroutedReasonId =
  | "unknown_model"
  | "no_healthy_candidate"
  | "group_empty"
  | "auth"
  | "governance"
  | "budget"
  | "rate_limited"
  | "overloaded"
  | "bad_request"
  | "upstream"
  | "translation"
  | "timeout"
  | "store"
  | "transport"
  | "other"
  | (string & {});

/** One row of the gateway's per-reason breakdown (optional until the API ships it). */
export type UnroutedReasonRow = {
  reason: UnroutedReasonId;
  requests: number;
  /** Unix seconds of the newest request with this reason. */
  last_ts?: number | null;
  /** A model the client asked for, e.g. the unknown slug. */
  sample_model?: string | null;
  /** One verbatim error message for this reason. */
  sample_error?: string | null;
};

export type UnroutedApiBreakdown = {
  requests?: number;
  reasons: ReadonlyArray<UnroutedReasonRow>;
};

/** A recent usage event, optionally carrying the gateway's stable reason id. */
export type UnroutedEvent = Pick<UsageEvent, "provider" | "ts" | "error" | "model"> &
  Partial<Pick<UsageEvent, "requested_model" | "status">> & {
    unrouted_reason?: string | null;
  };

export type UnroutedReasonInfo = {
  label: string;
  explanation: string;
  /** Where the operator fixes it. */
  fix: { label: string; href: string } | null;
};

const REASONS: Record<string, UnroutedReasonInfo> = {
  unknown_model: {
    label: "Unknown model",
    explanation: "The client asked for a model that no provider, alias or group serves.",
    fix: { label: "Check models", href: "/models" },
  },
  no_healthy_candidate: {
    label: "No account available",
    explanation: "Every account that serves the model was cooling down, disabled or out of quota.",
    fix: { label: "Check providers", href: "/providers" },
  },
  group_empty: {
    label: "Empty group",
    explanation: "The model group has no member models that can take the request.",
    fix: { label: "Check groups", href: "/models" },
  },
  auth: {
    label: "Missing or invalid key",
    explanation:
      "The request carried no RRouter API key, or a key that is invalid, disabled, or expired.",
    fix: { label: "Check keys", href: "/access/keys" },
  },
  governance: {
    label: "Key not allowed",
    explanation: "The key is out of scope or blocked from calling this endpoint or model.",
    fix: { label: "Check keys", href: "/access/keys" },
  },
  budget: {
    label: "Budget or quota reached",
    explanation: "The API key, team, or account hit its configured spend or quota ceiling.",
    fix: { label: "Check keys", href: "/access/keys" },
  },
  rate_limited: {
    label: "Rate limited",
    explanation:
      "The API key exceeded its configured requests-per-minute or tokens-per-minute limit.",
    fix: { label: "Check keys", href: "/access/keys" },
  },
  overloaded: {
    label: "Gateway busy",
    explanation: "RRouter shed the request because too many were already in flight.",
    fix: null,
  },
  bad_request: {
    label: "Invalid request",
    explanation: "The request body could not be parsed or was missing required fields.",
    fix: null,
  },
  translation: {
    label: "Unsupported request shape",
    explanation: "The request could not be translated into any provider's API format.",
    fix: null,
  },
  upstream: {
    label: "Upstream error",
    explanation: "The request failed before an account was charged with it.",
    fix: null,
  },
  timeout: {
    label: "Timed out",
    explanation: "The request timed out before RRouter could hand it to a provider.",
    fix: null,
  },
  transport: {
    label: "Connection error",
    explanation: "A network error stopped the request before it reached a provider.",
    fix: null,
  },
  store: {
    label: "Gateway storage error",
    explanation: "RRouter could not read its own configuration while routing the request.",
    fix: null,
  },
  other: {
    label: "Other",
    explanation: "Rejected before dispatch for a reason the gateway did not classify.",
    fix: null,
  },
};

/** Display info for a reason id; unknown ids get a readable label and the generic text. */
export function unroutedReasonInfo(reason: string): UnroutedReasonInfo {
  const known = REASONS[reason];
  if (known) return known;
  const words = reason.replace(/[_-]+/g, " ").trim();
  return {
    label: words ? words.charAt(0).toUpperCase() + words.slice(1) : "Other",
    explanation: REASONS.other!.explanation,
    fix: null,
  };
}

/**
 * Best-effort reason from the error text of a request rejected before dispatch. Only used
 * when the gateway has not recorded a stable reason id on the event.
 */
export function classifyUnroutedError(error: string | null | undefined): UnroutedReasonId {
  const text = (error ?? "").toLowerCase();
  if (!text.trim()) return "other";
  if (
    /no (healthy|available|eligible|usable) (candidate|account|provider|connection)|no candidates?\b|all (accounts|candidates|connections)|cool(ing)?[ -]?down|circuit (breaker )?open/.test(
      text,
    )
  ) {
    return "no_healthy_candidate";
  }
  if (/group\b.*\b(empty|no (members|models))|empty group/.test(text)) return "group_empty";
  if (
    /unknown model|model[^.]*not (found|supported|available)|no such model|unsupported model|model_not_found|does not exist|no route for model|not a known model/.test(
      text,
    )
  ) {
    return "unknown_model";
  }
  if (/inactive|expired|revoked|scope|forbidden|not allowed|not permitted/.test(text)) {
    return "governance";
  }
  if (/unauthori[sz]ed|api key|invalid key|missing (bearer|key|token)|authentication/.test(text)) {
    return "auth";
  }
  if (/budget|quota.*exhausted/.test(text)) return "budget";
  if (/rate[ _-]?limit|too many requests/.test(text)) return "rate_limited";
  if (/overloaded|too many (in flight)|concurrency/.test(text)) return "overloaded";
  if (/timed? ?out|deadline/.test(text)) return "timeout";
  if (/invalid|malformed|parse|json|missing field|bad request|unexpected/.test(text)) {
    return "bad_request";
  }
  return "other";
}

export type UnroutedReasonView = {
  id: string;
  info: UnroutedReasonInfo;
  requests: number;
  /** 0–100 share of the reason rows' total. */
  share: number;
  lastSeenAt: number | null;
  sampleModel: string | null;
  sampleError: string | null;
};

export type UnroutedSummary = {
  /** Range totals from `/api/usage/summary` (or the API breakdown when larger). */
  requests: number;
  errors: number;
  /** Unix seconds of the newest unrouted request seen, or `null`. */
  lastSeenAt: number | null;
  /** Requested models in range, most requests first. */
  models: { model: string; requests: number }[];
  reasons: UnroutedReasonView[];
  reasonSource: "api" | "recent" | "none";
  /** How many recent events the `recent` breakdown was built from. */
  sampled: number;
};

function isUnroutedProvider(provider: string): boolean {
  return provider.trim() === "";
}

function toViews(
  rows: Array<Omit<UnroutedReasonView, "share" | "info">>,
): UnroutedReasonView[] {
  const total = rows.reduce((sum, row) => sum + row.requests, 0);
  return rows
    .filter((row) => row.requests > 0)
    .map((row) => ({
      ...row,
      info: unroutedReasonInfo(row.id),
      share: total > 0 ? (row.requests / total) * 100 : 0,
    }))
    .sort(
      (a, b) =>
        b.requests - a.requests ||
        (b.lastSeenAt ?? 0) - (a.lastSeenAt ?? 0) ||
        a.id.localeCompare(b.id),
    );
}

export function buildUnroutedSummary(input: {
  summaryRows: ReadonlyArray<UsageSummaryByConnection>;
  recentEvents?: ReadonlyArray<UnroutedEvent>;
  breakdown?: UnroutedApiBreakdown | null;
}): UnroutedSummary {
  const { summaryRows, recentEvents = [], breakdown } = input;

  let requests = 0;
  let errors = 0;
  const modelCounts = new Map<string, number>();
  for (const row of summaryRows) {
    if (!isUnroutedProvider(row.provider)) continue;
    requests += row.requests;
    errors += row.errors;
    const model = row.model.trim() || "(no model)";
    modelCounts.set(model, (modelCounts.get(model) ?? 0) + row.requests);
  }
  const models = [...modelCounts]
    .map(([model, count]) => ({ model, requests: count }))
    .filter((m) => m.requests > 0)
    .sort((a, b) => b.requests - a.requests || a.model.localeCompare(b.model));

  const events = recentEvents.filter((event) => isUnroutedProvider(event.provider));
  let lastSeenAt: number | null = null;
  for (const event of events) {
    if (lastSeenAt === null || event.ts > lastSeenAt) lastSeenAt = event.ts;
  }

  if (breakdown && breakdown.reasons.length > 0) {
    const reasons = toViews(
      breakdown.reasons.map((row) => ({
        id: row.reason || "other",
        requests: row.requests,
        lastSeenAt: row.last_ts ?? null,
        sampleModel: row.sample_model?.trim() || null,
        sampleError: row.sample_error?.trim() || null,
      })),
    );
    for (const reason of reasons) {
      if (reason.lastSeenAt !== null && (lastSeenAt === null || reason.lastSeenAt > lastSeenAt)) {
        lastSeenAt = reason.lastSeenAt;
      }
    }
    const apiTotal = breakdown.requests ?? reasons.reduce((sum, r) => sum + r.requests, 0);
    return {
      requests: Math.max(requests, apiTotal),
      errors,
      lastSeenAt,
      models,
      reasons,
      reasonSource: "api",
      sampled: 0,
    };
  }

  if (events.length > 0) {
    const byReason = new Map<string, Omit<UnroutedReasonView, "share" | "info">>();
    // Newest first, so the sample on each reason is its most recent example.
    for (const event of [...events].sort((a, b) => b.ts - a.ts)) {
      const id = event.unrouted_reason?.trim() || classifyUnroutedError(event.error);
      const model = event.requested_model?.trim() || event.model.trim() || null;
      const row = byReason.get(id);
      if (row) {
        row.requests += 1;
        row.sampleModel ??= model;
        row.sampleError ??= event.error?.trim() || null;
      } else {
        byReason.set(id, {
          id,
          requests: 1,
          lastSeenAt: event.ts,
          sampleModel: model,
          sampleError: event.error?.trim() || null,
        });
      }
    }
    return {
      requests: Math.max(requests, events.length),
      errors,
      lastSeenAt,
      models,
      reasons: toViews([...byReason.values()]),
      reasonSource: "recent",
      sampled: events.length,
    };
  }

  return { requests, errors, lastSeenAt, models, reasons: [], reasonSource: "none", sampled: 0 };
}

/**
 * Defensive read of the `unrouted` key the gateway adds to `/api/usage/summary` (#13):
 * anything malformed is dropped rather than rendered as a false breakdown.
 */
export function normalizeUnroutedBreakdown(raw: unknown): UnroutedApiBreakdown | null {
  if (typeof raw !== "object" || raw === null) return null;
  const record = raw as { requests?: unknown; reasons?: unknown };
  if (!Array.isArray(record.reasons)) return null;
  const reasons: UnroutedReasonRow[] = [];
  for (const entry of record.reasons as unknown[]) {
    if (typeof entry !== "object" || entry === null) continue;
    const row = entry as Record<string, unknown>;
    const requests = typeof row.requests === "number" && Number.isFinite(row.requests) ? row.requests : 0;
    if (requests <= 0) continue;
    reasons.push({
      reason: typeof row.reason === "string" && row.reason.trim() ? row.reason.trim() : "other",
      requests,
      last_ts: typeof row.last_ts === "number" ? row.last_ts : null,
      sample_model: typeof row.sample_model === "string" ? row.sample_model : null,
      sample_error: typeof row.sample_error === "string" ? row.sample_error : null,
    });
  }
  return {
    requests: typeof record.requests === "number" ? record.requests : undefined,
    reasons,
  };
}
