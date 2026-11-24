/**
 * Unit tests for the multi-provider `/usage` Limits Board model:
 * - Provider display resolution (empty `""` -> "Unrouted", catalog names, unknown id fallback)
 * - Risk sorting (exhausted -> earliest projected hit -> failing -> highest % -> no-limit -> inactive)
 * - Summary strip counts & status filtering
 * - Non-admin honest "admin_only" state (never false "no_limit")
 * - Loading honesty: admin sees "loading", never a false "no_limit"/"healthy", until both
 *   limit sources answer
 * - Default chart window selection & per-window pace status wording
 *
 * Run with `npm test` (Node's built-in runner over TS sources).
 */
import assert from "node:assert/strict";
import { test } from "node:test";
import type {
  AccountQuotaRow,
  AccountSubscription,
  Connection,
  UsageSummaryByConnection,
} from "../src/lib/api.ts";
import { emptyTotals } from "../src/pages/usage/aggregate.ts";
import {
  buildBoardAccounts,
  defaultChartWindowId,
  filterBoardAccounts,
  resolveProviderDisplay,
  summarizeBoard,
  windowPaceStatus,
} from "../src/pages/usage/board-model.ts";
import { windowRisk } from "../src/pages/usage/pace.ts";

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

test("board includes every account of every provider sorted by risk, failing surfaced honestly", () => {
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

  // Order: Exhausted (codex) -> On-pace (claude) -> Failing (unrouted, openrouter) -> Healthy (ocg) -> No limit (openai) -> Inactive (anthropic)
  assert.deepEqual(
    items.map((i) => i.key),
    ["c-codex", "c-claude", "unattributed:", "c-or", "c-ocg", "c-oa", "c-off"],
  );
  assert.equal(items[0]!.status, "exhausted");
  assert.equal(items[1]!.status, "at_risk");
  assert.equal(items[2]!.status, "failing");
  assert.equal(items[2]!.providerLabel, "Unrouted");
  assert.equal(items[2]!.isUnrouted, true);
  assert.equal(items[3]!.status, "failing");
  assert.equal(items[3]!.providerLabel, "OpenRouter");
  assert.equal(items[4]!.status, "healthy");
  assert.equal(items[4]!.windows.length, 3);
  assert.equal(items[4]!.lastUsedAt, NOW_S - 60);
  assert.equal(items[5]!.status, "no_limit");
  assert.equal(items[6]!.status, "inactive");

  const rangeTotals = {
    requests: 3897,
    errors: 31,
    tokens: 316_150_000,
    costMicros: 14_079_400,
  };
  const summary = summarizeBoard(items, rangeTotals);
  assert.equal(summary.total, 7);
  assert.equal(summary.atRisk, 2);
  assert.equal(summary.failing, 2);
  assert.equal(summary.healthy, 1);
  assert.equal(summary.noLimit, 1);
  assert.equal(summary.totalSpendMicros, 14_079_400);

  assert.equal(filterBoardAccounts(items, "at_risk").length, 2);
  assert.equal(filterBoardAccounts(items, "failing").length, 2);
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

test("windowPaceStatus returns honest per-window pace messages", () => {
  const exhausted = windowPaceStatus(
    windowRisk({ id: "a", label: "5h", used_percent: 100, window_seconds: 5 * HOUR, resets_at: NOW_S + HOUR }, NOW),
    NOW,
  );
  assert.equal(exhausted.tone, "neg");
  assert.match(exhausted.text, /Exhausted/);

  const onPace = windowPaceStatus(
    windowRisk({ id: "b", label: "Monthly", used_percent: 50, window_seconds: 30 * DAY, resets_at: NOW_S + 20 * DAY }, NOW),
    NOW,
  );
  assert.equal(onPace.tone, "warn");
  assert.match(onPace.text, /^On pace to hit ~/);

  const zero = windowPaceStatus(
    windowRisk({ id: "c", label: "5h", used_percent: 0, window_seconds: 5 * HOUR, resets_at: NOW_S + 2 * HOUR }, NOW),
    NOW,
  );
  assert.equal(zero.tone, "muted");
  assert.equal(zero.text, "OK · 0% used");

  const noWindow = windowPaceStatus(
    windowRisk({ id: "d", label: "Monthly", used_percent: 40, window_seconds: null, resets_at: null }, NOW),
    NOW,
  );
  assert.equal(noWindow.tone, "faint");
  assert.equal(noWindow.text, "No reset window reported");
});
