/**
 * Unit tests for the multi-provider `/usage` Limits Board model:
 * - Provider display resolution (empty `""` -> "Unrouted", catalog names, unknown id fallback)
 * - Risk sorting (exhausted -> earliest projected hit -> failing -> highest % -> no-limit -> inactive)
 * - Summary strip counts & status filtering
 * - Non-admin honest "admin_only" state (never false "no_limit")
 * - Loading honesty: admin sees "loading", never a false "no_limit"/"healthy", until both
 *   limit sources answer
 * - Default chart window selection & per-window pace status wording (only notable pace printed)
 * - Board grouping (attention / ok / untracked / inactive) and one-line failure summaries
 * - Unrouted requests: kept off the board, summarised per reason (API breakdown → recent
 *   events' reason ids → classified error text → none)
 *
 * Run with `npm test` (Node's built-in runner over TS sources).
 */
import assert from "node:assert/strict";
import { test } from "node:test";
import {
  isSameOriginPath,
  playgroundHeaders,
  type AccountQuotaRow,
  type AccountSubscription,
  type Connection,
  type UsageSummaryByConnection,
} from "../src/lib/api.ts";
import { duration, uptime, usd, usdDisplay } from "../src/lib/format.ts";
import { emptyTotals } from "../src/pages/usage/aggregate.ts";
import {
  boardGroupOf,
  buildBoardAccounts,
  defaultChartWindowId,
  failureSummary,
  filterBoardAccounts,
  groupBoardAccounts,
  projectionTick,
  resolveProviderDisplay,
  summarizeBoard,
  windowPaceStatus,
} from "../src/pages/usage/board-model.ts";
import { windowRisk } from "../src/pages/usage/pace.ts";
import {
  buildUnroutedSummary,
  classifyUnroutedError,
  normalizeUnroutedBreakdown,
  unroutedReasonInfo,
} from "../src/pages/usage/unrouted-model.ts";
import {
  bucketAccountWindows,
  buildQuotaAlerts,
  buildUsageBreakdown,
  buildUsageTrend,
  classifyWindowColumn,
  computeOverviewKpis,
  trackedQuotaAccounts,
} from "../src/pages/usage/overview-model.ts";

const HOUR = 3600;
const DAY = 86_400;
const NOW_S = 1_800_000_000;
const NOW = NOW_S * 1000;

function makeConnection(overrides: Partial<Connection> & { id: string; provider: string }): Connection {
  return {
    name: null,
    email: null,
    auth_type: "api_key",
    team_id: null,
    owner_user_id: null,
    base_url_override: null,
    priority: 0,
    weight: 100,
    is_active: true,
    backoff_level: 0,
    has_access_token: false,
    has_refresh_token: false,
    has_api_key: true,
    last_error: null,
    ...overrides,
  };
}

function makeSub(
  connection_id: string,
  provider: string,
  windows: AccountSubscription["windows"],
  extra?: Partial<AccountSubscription>,
): AccountSubscription {
  return {
    connection_id,
    provider,
    name: connection_id,
    plan: null,
    windows,
    error: null,
    fetched_at: NOW_S - 24,
    ...extra,
  };
}

function makeSummary(
  overrides: Partial<UsageSummaryByConnection> & { provider: string; model: string },
): UsageSummaryByConnection {
  return {
    connection_id: null,
    requests: 0,
    errors: 0,
    input_tokens: 0,
    output_tokens: 0,
    cache_read_tokens: 0,
    cache_write_tokens: 0,
    cost_micros: 0,
    avg_latency_ms: 0,
    ...overrides,
  };
}

test("resolveProviderDisplay never returns blank and labels empty provider as Unrouted", () => {
  const catalog = new Map([
    ["openrouter", "OpenRouter"],
    ["custom-llm", "Internal vLLM"],
  ]);
  assert.deepEqual(resolveProviderDisplay("", catalog), { label: "Unrouted", isUnrouted: true });
  assert.deepEqual(resolveProviderDisplay("   ", catalog), { label: "Unrouted", isUnrouted: true });
  assert.deepEqual(resolveProviderDisplay("openrouter", catalog), {
    label: "OpenRouter",
    isUnrouted: false,
  });
  assert.deepEqual(resolveProviderDisplay("custom-llm", catalog), {
    label: "Internal vLLM",
    isUnrouted: false,
  });
  assert.deepEqual(resolveProviderDisplay("my-private-gateway", catalog), {
    label: "my-private-gateway",
    isUnrouted: false,
  });
});

test("board includes every account of every provider sorted by risk; unrouted traffic is not a card", () => {
  const connections: Connection[] = [
    makeConnection({ id: "c-ocg", provider: "opencode-go", name: "opencode-rrouter" }),
    makeConnection({
      id: "c-claude",
      provider: "claude",
      name: "Claude OAuth",
      auth_type: "oauth",
    }),
    makeConnection({ id: "c-codex", provider: "codex", name: "codex-work", auth_type: "oauth" }),
    makeConnection({ id: "c-or", provider: "openrouter", name: "or-prod" }),
    makeConnection({ id: "c-oa", provider: "openai", name: "openai-payg" }),
    makeConnection({
      id: "c-off",
      provider: "anthropic",
      name: "disabled-key",
      is_active: false,
    }),
  ];

  const subscriptions: AccountSubscription[] = [
    // Exhausted (100%)
    makeSub("c-codex", "codex", [
      {
        id: "5h",
        label: "5 hour",
        used_percent: 100,
        window_seconds: 5 * HOUR,
        resets_at: NOW_S + HOUR,
      },
    ]),
    // On pace to hit before reset (27% in 34m of 5h window -> hits in ~1.5h)
    makeSub(
      "c-claude",
      "claude",
      [
        {
          id: "5h",
          label: "5 hour",
          used_percent: 27,
          window_seconds: 5 * HOUR,
          resets_at: NOW_S + 5 * HOUR - 2040,
        },
        {
          id: "weekly",
          label: "Weekly",
          used_percent: 18,
          window_seconds: 7 * DAY,
          resets_at: NOW_S + 4 * DAY,
        },
      ],
      { plan: "max" },
    ),
    // Healthy with 3 windows (54% monthly, 20 days in -> projected ~81% at reset)
    makeSub("c-ocg", "opencode-go", [
      {
        id: "5h",
        label: "5 hour",
        used_percent: 0,
        window_seconds: 5 * HOUR,
        resets_at: NOW_S + 4 * HOUR,
      },
      {
        id: "weekly",
        label: "Weekly",
        used_percent: 9,
        window_seconds: 7 * DAY,
        resets_at: NOW_S + 5 * DAY,
      },
      {
        id: "monthly",
        label: "Monthly",
        used_percent: 54,
        window_seconds: 30 * DAY,
        resets_at: NOW_S + 10 * DAY,
      },
    ]),
  ];

  const summaryRows: UsageSummaryByConnection[] = [
    makeSummary({
      connection_id: "c-ocg",
      provider: "opencode-go",
      model: "opencode-go/deepseek-v4",
      requests: 3830,
      errors: 4,
      input_tokens: 200_000_000,
      output_tokens: 116_000_000,
      cost_micros: 13_229_400,
    }),
    makeSummary({
      connection_id: "c-or",
      provider: "openrouter",
      model: "openrouter/llama-3",
      requests: 6,
      errors: 6,
    }),
    // Empty provider string (failed before dispatch)
    makeSummary({
      connection_id: null,
      provider: "",
      model: "unknown-model",
      requests: 21,
      errors: 21,
    }),
    makeSummary({
      connection_id: "c-oa",
      provider: "openai",
      model: "openai/gpt-4.1",
      requests: 40,
      errors: 0,
      input_tokens: 100_000,
      output_tokens: 50_000,
      cost_micros: 850_000,
    }),
  ];

  const items = buildBoardAccounts({
    connections,
    subscriptions,
    quotas: [],
    summaryRows,
    recentEvents: [
      { connection_id: "c-ocg", provider: "opencode-go", ts: NOW_S - 60 },
      { connection_id: null, provider: "", ts: NOW_S - 300 },
    ],
    isAdmin: true,
    quotaLoaded: true,
    now: NOW,
  });

  // Order: Exhausted (codex) -> On-pace (claude) -> Failing (openrouter) -> Healthy (ocg) -> No limit (openai) -> Inactive (anthropic).
  // The provider "" bucket (failed before dispatch) is not an account and never becomes a card.
  assert.deepEqual(
    items.map((i) => i.key),
    ["c-codex", "c-claude", "c-or", "c-ocg", "c-oa", "c-off"],
  );
  assert.equal(items.some((i) => i.provider.trim() === ""), false);
  assert.equal(items[0]!.status, "exhausted");
  assert.equal(items[1]!.status, "at_risk");
  assert.equal(items[2]!.status, "failing");
  assert.equal(items[2]!.providerLabel, "OpenRouter");
  assert.equal(items[3]!.status, "healthy");
  assert.equal(items[3]!.windows.length, 3);
  assert.equal(items[3]!.lastUsedAt, NOW_S - 60);
  assert.equal(items[4]!.status, "no_limit");
  assert.equal(items[5]!.status, "inactive");

  // Grouped so each grid row holds cards of one shape, risk order kept inside each group.
  assert.deepEqual(
    groupBoardAccounts(items).map(({ group, items: g }) => [group, g.map((i) => i.key)]),
    [
      ["attention", ["c-codex", "c-claude", "c-or"]],
      ["ok", ["c-ocg"]],
      ["untracked", ["c-oa"]],
      ["inactive", ["c-off"]],
    ],
  );
  assert.equal(failureSummary(items[2]!), "All 6 requests failed");
  assert.equal(failureSummary(items[3]!), null);

  const rangeTotals = {
    requests: 3897,
    errors: 31,
    tokens: 316_150_000,
    costMicros: 14_079_400,
  };
  const summary = summarizeBoard(items, rangeTotals);
  assert.equal(summary.total, 6);
  assert.equal(summary.atRisk, 2);
  assert.equal(summary.failing, 1);
  assert.equal(summary.healthy, 1);
  assert.equal(summary.noLimit, 1);
  assert.equal(summary.totalSpendMicros, 14_079_400);

  assert.equal(filterBoardAccounts(items, "at_risk").length, 2);
  assert.equal(filterBoardAccounts(items, "failing").length, 1);
  assert.equal(filterBoardAccounts(items, "healthy").length, 1);
  assert.equal(filterBoardAccounts(items, "no_limit").length, 1);
});

test("non-admin sees activity cards with admin_only status and no false no_limit", () => {
  const items = buildBoardAccounts({
    connections: [makeConnection({ id: "c1", provider: "claude", name: "Claude" })],
    subscriptions: [
      makeSub("c1", "claude", [
        { id: "5h", label: "5 hour", used_percent: 95, window_seconds: 5 * HOUR, resets_at: NOW_S + HOUR },
      ]),
    ],
    quotas: [],
    summaryRows: [],
    isAdmin: false,
    quotaLoaded: false,
    now: NOW,
  });

  assert.equal(items.length, 1);
  assert.equal(items[0]!.status, "admin_only");
  assert.equal(items[0]!.hasLimits, false);
  assert.equal(items[0]!.windows.length, 0);
  assert.equal(summarizeBoard(items, emptyTotals()).noLimit, 0);
  assert.equal(boardGroupOf(items[0]!), "untracked");
});

test("admin sees loading, never a false no_limit or healthy, until both limit sources answer", () => {
  const connections = [makeConnection({ id: "c1", provider: "claude", name: "Claude" })];
  const subscriptions = [
    makeSub("c1", "claude", [
      {
        id: "5h",
        label: "5 hour",
        used_percent: 100,
        window_seconds: 5 * HOUR,
        resets_at: NOW_S + HOUR,
      },
    ]),
  ];

  // No subscription data yet: the old code claimed "no_limit" here.
  const empty = buildBoardAccounts({
    connections,
    subscriptions: [],
    quotas: [],
    summaryRows: [],
    isAdmin: true,
    quotaLoaded: false,
    now: NOW,
  });
  assert.equal(empty.length, 1);
  assert.equal(empty[0]!.status, "loading");
  assert.equal(summarizeBoard(empty, emptyTotals()).noLimit, 0);
  assert.equal(filterBoardAccounts(empty, "no_limit").length, 0);

  // Plan windows already answered but operator caps are still pending: the old code
  // claimed "healthy" (and would count the account as at-risk) from half the data.
  const partial = buildBoardAccounts({
    connections,
    subscriptions,
    quotas: [],
    summaryRows: [],
    isAdmin: true,
    quotaLoaded: false,
    now: NOW,
  });
  assert.equal(partial[0]!.status, "loading");
  assert.equal(partial[0]!.hasLimits, true);
  assert.equal(partial[0]!.isAtRisk, false);
  assert.equal(partial[0]!.earliestHitAt, null);
  const partialSummary = summarizeBoard(partial, emptyTotals());
  assert.equal(partialSummary.atRisk, 0);
  assert.equal(partialSummary.healthy, 0);
  assert.equal(partialSummary.noLimit, 0);
  assert.equal(filterBoardAccounts(partial, "at_risk").length, 0);
  assert.equal(filterBoardAccounts(partial, "healthy").length, 0);
  assert.equal(filterBoardAccounts(partial, "no_limit").length, 0);

  // Once both sources answer, the same account is honestly exhausted.
  const loaded = buildBoardAccounts({
    connections,
    subscriptions,
    quotas: [],
    summaryRows: [],
    isAdmin: true,
    quotaLoaded: true,
    now: NOW,
  });
  assert.equal(loaded[0]!.status, "exhausted");
  assert.equal(loaded[0]!.isAtRisk, true);
  assert.equal(summarizeBoard(loaded, emptyTotals()).atRisk, 1);
});

test("operator quota cap marks account exhausted and includes cap bars", () => {
  const quota: AccountQuotaRow = {
    connection_id: "c-oa",
    name: "openai-capped",
    provider: "openai",
    is_active: true,
    window: "daily",
    max_requests: 0,
    max_tokens: 0,
    max_cost_micros: 5_000_000,
    requests: 120,
    tokens: 900_000,
    cost_micros: 5_000_000,
    exhausted: true,
    resets_at: NOW_S + 3 * HOUR,
  };
  const items = buildBoardAccounts({
    connections: [makeConnection({ id: "c-oa", provider: "openai", name: "openai-capped" })],
    subscriptions: [],
    quotas: [quota],
    summaryRows: [],
    isAdmin: true,
    quotaLoaded: true,
    now: NOW,
  });

  assert.equal(items[0]!.status, "exhausted");
  assert.equal(items[0]!.operatorBars.length, 1);
  assert.equal(items[0]!.operatorBars[0]!.label, "Daily spend");
  assert.equal(items[0]!.operatorBars[0]!.detail, "$5.00 / $5.00");
  assert.equal(items[0]!.hasLimits, true);
});

test("defaultChartWindowId prefers riskiest chartable window (>= 2 days)", () => {
  const shortRisky = windowRisk(
    { id: "5h", label: "5 hour", used_percent: 80, window_seconds: 5 * HOUR, resets_at: NOW_S + HOUR },
    NOW,
  );
  const weekly = windowRisk(
    { id: "weekly", label: "Weekly", used_percent: 40, window_seconds: 7 * DAY, resets_at: NOW_S + 3 * DAY },
    NOW,
  );
  const monthly = windowRisk(
    { id: "monthly", label: "Monthly", used_percent: 60, window_seconds: 30 * DAY, resets_at: NOW_S + 10 * DAY },
    NOW,
  );
  assert.equal(defaultChartWindowId([shortRisky, weekly, monthly]), "monthly");
  assert.equal(defaultChartWindowId([shortRisky]), "5h");
  assert.equal(defaultChartWindowId([]), null);
});

test("windowPaceStatus prints only notable pace; quiet states stay in the meter", () => {
  const exhausted = windowPaceStatus(
    windowRisk({ id: "a", label: "5h", used_percent: 100, window_seconds: 5 * HOUR, resets_at: NOW_S + HOUR }, NOW),
    NOW,
  );
  assert.equal(exhausted.tone, "neg");
  assert.equal(exhausted.notable, true);
  assert.equal(exhausted.text, "Limit reached");

  const onPace = windowPaceStatus(
    windowRisk({ id: "b", label: "Monthly", used_percent: 50, window_seconds: 30 * DAY, resets_at: NOW_S + 20 * DAY }, NOW),
    NOW,
  );
  assert.equal(onPace.tone, "warn");
  assert.equal(onPace.notable, true);
  assert.match(onPace.text, /^On pace to hit the limit ~/);

  const zero = windowPaceStatus(
    windowRisk({ id: "c", label: "5h", used_percent: 0, window_seconds: 5 * HOUR, resets_at: NOW_S + 2 * HOUR }, NOW),
    NOW,
  );
  assert.equal(zero.notable, false);
  assert.equal(zero.text, "Unused this window");

  // 9% after 2 of 7 days -> on pace for ~32%: announced, not printed, drawn as a tick.
  const weeklyRisk = windowRisk(
    { id: "w", label: "Weekly", used_percent: 9, window_seconds: 7 * DAY, resets_at: NOW_S + 5 * DAY },
    NOW,
  );
  const weekly = windowPaceStatus(weeklyRisk, NOW);
  assert.equal(weekly.notable, false);
  assert.equal(weekly.tone, "muted");
  assert.match(weekly.text, /^On pace for \d+% by reset$/);
  const tick = projectionTick(weeklyRisk);
  assert.ok(tick !== null && tick > 9 && tick <= 100, `tick ${tick}`);
  assert.equal(projectionTick(windowRisk(
    { id: "x", label: "5h", used_percent: 100, window_seconds: 5 * HOUR, resets_at: NOW_S + HOUR },
    NOW,
  )), null);

  const noWindow = windowPaceStatus(
    windowRisk({ id: "d", label: "Monthly", used_percent: 40, window_seconds: null, resets_at: null }, NOW),
    NOW,
  );
  assert.equal(noWindow.tone, "faint");
  assert.equal(noWindow.notable, false);
  assert.equal(noWindow.text, "No reset window reported");
});

test("failureSummary explains partial failures and backoff in one sentence", () => {
  const base = {
    refusal: null,
    isFailing: false,
    status: "degraded" as const,
    backoffLevel: 0,
  };
  assert.equal(
    failureSummary({ ...base, totals: { requests: 1200, errors: 150, tokens: 0, costMicros: 0 } }),
    "150 of 1.2K requests failed",
  );
  assert.equal(
    failureSummary({ ...base, backoffLevel: 2, totals: emptyTotals() }),
    "Backing off after recent upstream errors",
  );
  // A refusal explains itself through refusalView; no second sentence.
  assert.equal(
    failureSummary({ ...base, refusal: "account_suspended", isFailing: true, totals: emptyTotals() }),
    null,
  );
  assert.equal(
    failureSummary({ ...base, status: "healthy", totals: { requests: 10, errors: 1, tokens: 0, costMicros: 0 } }),
    null,
  );
});

const UNROUTED_ROWS: UsageSummaryByConnection[] = [
  makeSummary({ provider: "", model: "gpt-5-turbo", requests: 21, errors: 21 }),
  makeSummary({ provider: "", model: "claude-opus", requests: 4, errors: 4 }),
  makeSummary({ connection_id: "c-or", provider: "openrouter", model: "x", requests: 6, errors: 6 }),
];

test("unrouted summary prefers the gateway's per-reason breakdown", () => {
  const breakdown = normalizeUnroutedBreakdown({
    requests: 25,
    reasons: [
      { reason: "governance", requests: 4, last_ts: NOW_S - 30, sample_model: "claude-opus", sample_error: "key expired" },
      { reason: "unknown_model", requests: 21, last_ts: NOW_S - 300, sample_model: "gpt-5-turbo", sample_error: null },
      { reason: "brand_new_code", requests: 0 },
      "garbage",
    ],
  });
  assert.ok(breakdown);
  assert.equal(breakdown.reasons.length, 2);

  const summary = buildUnroutedSummary({ summaryRows: UNROUTED_ROWS, breakdown });
  assert.equal(summary.reasonSource, "api");
  assert.equal(summary.requests, 25);
  assert.equal(summary.errors, 25);
  assert.equal(summary.lastSeenAt, NOW_S - 30);
  assert.deepEqual(summary.reasons.map((r) => [r.id, r.requests, Math.round(r.share)]), [
    ["unknown_model", 21, 84],
    ["governance", 4, 16],
  ]);
  assert.equal(summary.reasons[0]!.info.label, "Unknown model");
  assert.equal(summary.reasons[0]!.info.fix?.href, "/models");
  assert.equal(summary.reasons[1]!.sampleError, "key expired");
  assert.deepEqual(summary.models, [
    { model: "gpt-5-turbo", requests: 21 },
    { model: "claude-opus", requests: 4 },
  ]);

  assert.equal(normalizeUnroutedBreakdown(undefined), null);
  assert.equal(normalizeUnroutedBreakdown({ requests: 3 }), null);
  assert.deepEqual(normalizeUnroutedBreakdown({ requests: 0, reasons: [] }), {
    requests: 0,
    reasons: [],
  });
});

test("unrouted summary falls back to recent events: stable reason ids first, error text second", () => {
  const summary = buildUnroutedSummary({
    summaryRows: UNROUTED_ROWS,
    breakdown: null,
    recentEvents: [
      { provider: "", ts: NOW_S - 10, model: "gpt-5-turbo", error: "whatever", unrouted_reason: "unknown_model" },
      { provider: "", ts: NOW_S - 20, model: "gpt-5-turbo", error: "unknown model: gpt-5-turbo" },
      { provider: "", ts: NOW_S - 40, model: "claude-opus", error: "no healthy candidate for claude-opus" },
      { provider: "openrouter", ts: NOW_S - 5, model: "x", error: "429" },
    ],
  });
  assert.equal(summary.reasonSource, "recent");
  assert.equal(summary.sampled, 3);
  assert.equal(summary.requests, 25);
  assert.equal(summary.lastSeenAt, NOW_S - 10);
  assert.deepEqual(summary.reasons.map((r) => [r.id, r.requests]), [
    ["unknown_model", 2],
    ["no_healthy_candidate", 1],
  ]);
  assert.equal(summary.reasons[0]!.sampleError, "whatever");

  const none = buildUnroutedSummary({ summaryRows: UNROUTED_ROWS });
  assert.equal(none.reasonSource, "none");
  assert.equal(none.reasons.length, 0);
  assert.equal(none.requests, 25);

  const empty = buildUnroutedSummary({ summaryRows: [UNROUTED_ROWS[2]!] });
  assert.equal(empty.requests, 0);
});

test("classifyUnroutedError and unroutedReasonInfo are honest about what they know", () => {
  assert.equal(classifyUnroutedError("Unknown model 'foo'"), "unknown_model");
  assert.equal(classifyUnroutedError("model gpt-9 not found"), "unknown_model");
  assert.equal(classifyUnroutedError("No healthy candidate for group fast"), "no_healthy_candidate");
  assert.equal(classifyUnroutedError("all accounts cooling down"), "no_healthy_candidate");
  assert.equal(classifyUnroutedError("api key expired"), "governance");
  assert.equal(classifyUnroutedError("401 Unauthorized: missing bearer token"), "auth");
  assert.equal(classifyUnroutedError("team budget exhausted"), "budget");
  assert.equal(classifyUnroutedError("429 rate_limited: rpm exceeded"), "rate_limited");
  assert.equal(classifyUnroutedError("invalid JSON body"), "bad_request");
  assert.equal(classifyUnroutedError(null), "other");
  assert.equal(classifyUnroutedError("something odd"), "other");

  assert.equal(unroutedReasonInfo("no_healthy_candidate").label, "No account available");
  assert.equal(unroutedReasonInfo("budget").label, "Budget or quota reached");
  assert.equal(unroutedReasonInfo("budget").fix?.href, "/access/keys");
  assert.equal(unroutedReasonInfo("rate_limited").label, "Rate limited");
  assert.equal(unroutedReasonInfo("rate_limited").fix?.href, "/access/keys");
  const unknown = unroutedReasonInfo("rate_shaped_by_policy");
  assert.equal(unknown.label, "Rate shaped by policy");
  assert.equal(unknown.fix, null);
});

test("playgroundHeaders attaches x-admin-token only for same-origin paths when token is set", () => {
  assert.deepEqual(playgroundHeaders("/v1/chat/completions", "adm-secret"), {
    "x-rrouter-playground": "1",
    "x-admin-token": "adm-secret",
  });
  assert.deepEqual(playgroundHeaders("/v1/messages", "  adm-secret  "), {
    "x-rrouter-playground": "1",
    "x-admin-token": "adm-secret",
  });
  // Without an admin token (session-cookie operator or Node test env), only the marker is sent.
  assert.deepEqual(playgroundHeaders("/v1/chat/completions", null), {
    "x-rrouter-playground": "1",
  });
  assert.deepEqual(playgroundHeaders("/v1/chat/completions", "   "), {
    "x-rrouter-playground": "1",
  });
  assert.deepEqual(playgroundHeaders("/v1/chat/completions"), {
    "x-rrouter-playground": "1",
  });
  // Cross-origin, scheme-relative, backslash, or non-root paths must never receive x-admin-token.
  for (const unsafe of [
    "https://other.example/v1/chat/completions",
    "http://localhost:9999/v1/messages",
    "//other.example/v1/chat/completions",
    "/\\other.example/v1/chat/completions",
    "v1/chat/completions",
  ]) {
    assert.equal(isSameOriginPath(unsafe), false, unsafe);
    assert.deepEqual(
      playgroundHeaders(unsafe, "adm-secret"),
      { "x-rrouter-playground": "1" },
      unsafe,
    );
  }
});

test("trackedQuotaAccounts excludes untracked/failing no-quota providers while totals keep them", () => {
  const connections: Connection[] = [
    makeConnection({ id: "c-ocg", provider: "opencode-go", name: "opencode-go" }),
    makeConnection({ id: "c-claude", provider: "claude", name: "claude-oauth", auth_type: "oauth" }),
    makeConnection({ id: "c-or", provider: "openrouter", name: "openrouter-key" }),
    makeConnection({ id: "c-off", provider: "openai", name: "openai-off", is_active: false }),
  ];
  const subscriptions: AccountSubscription[] = [
    makeSub("c-ocg", "opencode-go", [
      { id: "5h", label: "5 hour", used_percent: 12, window_seconds: 5 * HOUR, resets_at: NOW_S + 4 * HOUR },
      { id: "weekly", label: "Weekly", used_percent: 25, window_seconds: 7 * DAY, resets_at: NOW_S + 5 * DAY },
      { id: "monthly", label: "Monthly", used_percent: 54, window_seconds: 30 * DAY, resets_at: NOW_S + 10 * DAY },
    ]),
    makeSub("c-claude", "claude", [
      { id: "5h", label: "5 hour", used_percent: 80, window_seconds: 5 * HOUR, resets_at: NOW_S + 4 * HOUR },
    ]),
  ];
  const summaryRows: UsageSummaryByConnection[] = [
    makeSummary({ connection_id: "c-ocg", provider: "opencode-go", model: "ocg/a", requests: 100, input_tokens: 10_000, cost_micros: 1_000_000 }),
    makeSummary({ connection_id: "c-or", provider: "openrouter", model: "or/free", requests: 6, errors: 6, cost_micros: 500_000 }),
  ];

  const board = buildBoardAccounts({
    connections,
    subscriptions,
    quotas: [],
    summaryRows,
    isAdmin: true,
    quotaLoaded: true,
    now: NOW,
  });

  // Only tracked quota accounts appear in the Providers quota table: OpenRouter (failing, no quota)
  // and inactive OpenAI (no quota) are excluded.
  const tracked = trackedQuotaAccounts(board);
  assert.deepEqual(tracked.map((a) => a.key), ["c-claude", "c-ocg"]);
  assert.equal(tracked.some((a) => a.provider === "openrouter"), false);

  // Bucketing maps 5h/weekly/monthly into their columns and leaves missing windows null.
  const ocgBuckets = bucketAccountWindows(tracked[1]!, NOW);
  assert.equal(ocgBuckets["5h"]?.primary, "12%");
  assert.equal(ocgBuckets.daily, null);
  assert.equal(ocgBuckets.weekly?.primary, "25%");
  assert.equal(ocgBuckets.monthly?.primary, "54%");

  const claudeBuckets = bucketAccountWindows(tracked[0]!, NOW);
  assert.equal(claudeBuckets["5h"]?.primary, "80%");
  assert.equal(claudeBuckets.daily, null);
  assert.equal(claudeBuckets.weekly, null);
  assert.equal(claudeBuckets.monthly, null);

  // KPIs reflect peak tracked usage (80%), active/total providers (3/4), pace countdown, and ALL spend ($1.50).
  const kpis = computeOverviewKpis({
    boardAccounts: board,
    trackedAccounts: tracked,
    totals: { requests: 106, errors: 6, tokens: 10_000, costMicros: 1_500_000 },
    isAdmin: true,
    quotaLoaded: true,
    now: NOW,
  });
  assert.equal(kpis.peakUsage?.percent, 80);
  assert.equal(kpis.peakUsage?.providerLabel, "Claude");
  assert.equal(kpis.providers.active, 3);
  assert.equal(kpis.providers.total, 4);
  assert.equal(kpis.providers.tracked, 2);
  assert.equal(kpis.estimatedDaysLeft.kind, "countdown");
  assert.equal(kpis.estimatedDaysLeft.title, "Estimated Time Left");
  assert.equal(kpis.spend.costMicros, 1_500_000);

  // Multi-day pace hit keeps "Estimated Days Left" title.
  const multiDayBoard = buildBoardAccounts({
    connections: [makeConnection({ id: "c-m", provider: "opencode-go", name: "ocg" })],
    subscriptions: [
      makeSub("c-m", "opencode-go", [
        { id: "monthly", label: "Monthly", used_percent: 60, window_seconds: 30 * DAY, resets_at: NOW_S + 20 * DAY },
      ]),
    ],
    quotas: [],
    summaryRows: [],
    isAdmin: true,
    quotaLoaded: true,
    now: NOW,
  });
  const multiDayKpis = computeOverviewKpis({
    boardAccounts: multiDayBoard,
    trackedAccounts: trackedQuotaAccounts(multiDayBoard),
    totals: emptyTotals(),
    isAdmin: true,
    quotaLoaded: true,
    now: NOW,
  });
  assert.equal(multiDayKpis.estimatedDaysLeft.kind, "countdown");
  assert.equal(multiDayKpis.estimatedDaysLeft.title, "Estimated Days Left");
  assert.match(multiDayKpis.estimatedDaysLeft.label, /days?$/);

  // Alerts include Claude's on-pace 5h window with time-to-hit cue (matching the tile, not reset time)
  // and never include untracked OpenRouter.
  const alerts = buildQuotaAlerts(tracked, NOW);
  assert.ok(alerts.length >= 1);
  assert.equal(alerts[0]!.severity, "warn");
  assert.match(alerts[0]!.title, /Claude/);
  assert.equal(alerts[0]!.cue, `in ${kpis.estimatedDaysLeft.label}`);
  assert.equal(alerts.some((a) => /OpenRouter/i.test(a.title)), false);
});

test("classifyWindowColumn, buildUsageBreakdown, and buildUsageTrend handle all shapes honestly", () => {
  assert.equal(classifyWindowColumn({ id: "5h", label: "5 hour", window_seconds: 5 * HOUR }), "5h");
  assert.equal(classifyWindowColumn({ id: "d", label: "Daily", window_seconds: DAY }), "daily");
  assert.equal(classifyWindowColumn({ id: "w", label: "Weekly", window_seconds: 7 * DAY }), "weekly");
  assert.equal(classifyWindowColumn({ id: "m", label: "Monthly", window_seconds: 30 * DAY }), "monthly");
  assert.equal(classifyWindowColumn({ id: "session", label: "Short", window_seconds: null }), "5h");

  // Breakdown folds tail past 4 providers into Others and excludes unrouted "".
  const breakdown = buildUsageBreakdown([
    { provider: "", totals: { requests: 10, errors: 10, tokens: 0, costMicros: 0 }, models: [] },
    { provider: "opencode-go", totals: { requests: 50, errors: 0, tokens: 500, costMicros: 0 }, models: [] },
    { provider: "claude", totals: { requests: 20, errors: 0, tokens: 200, costMicros: 0 }, models: [] },
    { provider: "openai", totals: { requests: 10, errors: 0, tokens: 100, costMicros: 0 }, models: [] },
    { provider: "gemini", totals: { requests: 10, errors: 0, tokens: 100, costMicros: 0 }, models: [] },
    { provider: "openrouter", totals: { requests: 5, errors: 0, tokens: 60, costMicros: 0 }, models: [] },
    { provider: "groq", totals: { requests: 5, errors: 0, tokens: 40, costMicros: 0 }, models: [] },
  ]);
  assert.equal(breakdown.metric, "tokens");
  assert.equal(breakdown.total, 1000);
  assert.deepEqual(breakdown.slices.map((s) => [s.label, Math.round(s.share)]), [
    ["OpenCode Go", 50],
    ["Claude", 20],
    ["Gemini", 10],
    ["OpenAI", 10],
    ["Others", 10],
  ]);

  // Trend aligns buckets to range and folds Total + top providers + spendMicros.
  const bucketEnd = Math.floor(NOW_S / DAY) * DAY;
  const trend = buildUsageTrend(
    [
      {
        start: bucketEnd - DAY,
        provider: "opencode-go",
        model: "m1",
        requests: 4,
        errors: 0,
        input_tokens: 100,
        output_tokens: 50,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        cost_micros: 200_000,
        avg_latency_ms: 10,
      },
      {
        start: bucketEnd,
        provider: "claude",
        model: "m2",
        requests: 2,
        errors: 0,
        input_tokens: 300,
        output_tokens: 100,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        cost_micros: 500_000,
        avg_latency_ms: 10,
      },
    ],
    { seconds: DAY, cells: 3 },
    NOW,
  );
  assert.equal(trend.metric, "tokens");
  assert.deepEqual(trend.total, [0, 150, 400]);
  assert.deepEqual(trend.spendMicros, [0, 200_000, 500_000]);
  assert.deepEqual(trend.providers.map((p) => p.label), ["Claude", "OpenCode Go"]);
});

test("duration and uptime floor remainders and never print 60m or 24h", () => {
  assert.equal(duration(0), "0m");
  assert.equal(duration(-42), "0m");
  assert.equal(duration(Number.NaN), "0m");
  assert.equal(duration(59), "0m");
  assert.equal(duration(60), "1m");
  assert.equal(duration(3595), "59m");
  assert.equal(duration(3600), "1h 0m");
  assert.equal(duration(3 * HOUR - 5), "2h 59m");
  assert.equal(duration(9 * HOUR - 5), "8h 59m");
  assert.equal(duration(DAY - 1), "23h 59m");
  assert.equal(duration(DAY), "1d 0h");
  assert.equal(duration(2 * DAY - 5), "1d 23h");
  assert.equal(duration(12 * DAY - 5), "11d 23h");
  assert.equal(uptime(12 * DAY - 5), "11d 23h");
  assert.equal(uptime(3 * HOUR - 5), "2h 59m");
});

test("usdDisplay shows two decimals from $0.01 up while preserving sub-cent precision and usd()", () => {
  assert.equal(usdDisplay(24_100_000), "$24.10");
  assert.equal(usdDisplay(25_000_000), "$25.00");
  assert.equal(usdDisplay(42_169_400), "$42.17");
  assert.equal(usdDisplay(1_000_000), "$1.00");
  assert.equal(usdDisplay(999_000), "$1.00");
  assert.equal(usdDisplay(500_000), "$0.50");
  assert.equal(usdDisplay(430_000), "$0.43");
  assert.equal(usdDisplay(12_300), "$0.01");
  assert.equal(usdDisplay(10_000), "$0.01");
  assert.equal(usdDisplay(9_900), "$0.0099");
  assert.equal(usdDisplay(500), "$0.0005");
  assert.equal(usdDisplay(0), "$0.00");
  assert.equal(usdDisplay(0, { dash: true }), "—");
  assert.equal(usdDisplay(null), "$0.00");
  assert.equal(usdDisplay(null, { dash: true }), "—");
  assert.equal(usdDisplay(undefined, { dash: true }), "—");
  // Per-request usd() stays four decimals from $0.01 up.
  assert.equal(usd(24_100_000), "$24.1000");
  assert.equal(usd(42_169_400), "$42.1694");
  assert.equal(usd(430_000), "$0.4300");
});
