import assert from "node:assert/strict";
import { test } from "node:test";
import type {
  AccountQuotaRow,
  AccountSubscription,
  Connection,
  ModelProvider,
  OAuthProvider,
  UsageSummaryByConnection,
} from "../../lib/api.ts";
import {
  accountNeedsAttention,
  accountSentence,
  accountStatus,
  apiKeyReplacement,
  buildProviderGroups,
  buildTrafficByConnection,
  filterProviderGroups,
  providerLabel,
  providerOptions,
  providerSentence,
  summarizeProviders,
  until,
} from "./types.ts";

function makeConnection(overrides: Partial<Connection> = {}): Connection {
  return {
    id: "c-1",
    provider: "openai",
    name: "prod",
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

const azureSpec: ModelProvider = {
  id: "azure",
  display: "Azure OpenAI",
  code: "az",
  kind: "api_key",
  has_connection: true,
  scope: "global",
  listed: true,
  requires_base_url_override: true,
  base_url_example: "https://<resource>.openai.azure.com",
};

test("accountStatus and buildProviderGroups agree on missing base URL (no endpoint)", () => {
  const azure = makeConnection({ id: "c-az", provider: "azure", base_url_override: null });
  const status = accountStatus(azure, undefined, undefined, [azureSpec]);
  assert.equal(status.kind, "no_endpoint");
  assert.equal(status.ready, false);
  assert.equal(status.label, "No endpoint");
  assert.equal(status.health, "warning");

  const [group] = buildProviderGroups([azure], [azureSpec]);
  assert.ok(group);
  assert.equal(group.ready, 0);
  assert.equal(group.expected, 1);
  assert.equal(group.state, "down");
});

test("plan window at 90%+ flags account as plan_at_limit even when operator quota is low", () => {
  const nowSec = Math.floor(Date.now() / 1000);
  const quota: AccountQuotaRow = {
    connection_id: "c-cl",
    name: "claude-backup",
    provider: "claude",
    is_active: true,
    window: "daily",
    max_requests: 0,
    max_tokens: 0,
    max_cost_micros: 100_000_000,
    requests: 12,
    tokens: 10_000,
    cost_micros: 12_000_000,
    exhausted: false,
    resets_at: nowSec + 3600,
  };
  const sub: AccountSubscription = {
    connection_id: "c-cl",
    name: "claude-backup",
    provider: "claude",
    plan: "max",
    windows: [
      {
        id: "weekly",
        label: "Weekly",
        used_percent: 95,
        window_seconds: 7 * 86400,
        resets_at: nowSec + 86400,
      },
    ],
    fetched_at: nowSec,
    error: null,
  };
  const conn = makeConnection({ id: "c-cl", provider: "claude" });
  const status = accountStatus(conn, quota, sub);
  assert.equal(status.kind, "plan_at_limit");
  assert.equal(status.ready, false);
  assert.equal(status.health, "warning");

  const [group] = buildProviderGroups([conn], undefined, [quota], [sub]);
  assert.ok(group);
  assert.equal(group.ready, 0);
  assert.equal(group.state, "down");
});

test("ready account nearing a limit (>=70%) reports Warning while staying ready", () => {
  const nowSec = Math.floor(Date.now() / 1000);
  const sub: AccountSubscription = {
    connection_id: "c-max",
    name: "claude-max",
    provider: "claude",
    plan: "max",
    windows: [
      {
        id: "5h",
        label: "5 hour",
        used_percent: 80,
        window_seconds: 5 * 3600,
        resets_at: nowSec + 3 * 3600,
      },
    ],
    fetched_at: nowSec,
    error: null,
  };
  const conn = makeConnection({ id: "c-max", provider: "claude", name: "claude-max" });
  const status = accountStatus(conn, undefined, sub);
  assert.equal(status.kind, "warning");
  assert.equal(status.ready, true);
  assert.equal(status.label, "Warning");
  assert.equal(status.health, "warning");
  assert.equal(accountNeedsAttention(conn, status), true);

  const [group] = buildProviderGroups([conn], undefined, undefined, [sub]);
  assert.ok(group);
  assert.equal(group.ready, 1);
  assert.equal(group.state, "healthy");
});

test("buildProviderGroups counts ready, disabled, and expected accurately across mixed states", () => {
  const accounts = [
    makeConnection({ id: "c-ok", provider: "claude", name: "ok" }),
    makeConnection({ id: "c-dis", provider: "claude", name: "dis", is_active: false }),
    makeConnection({
      id: "c-bo",
      provider: "claude",
      name: "bo",
      backoff_level: 2,
      last_error: "502 bad gateway",
    }),
    makeConnection({
      id: "c-ref",
      provider: "claude",
      name: "ref",
      is_active: false,
      refusal: "account_suspended",
    }),
  ];
  const [group] = buildProviderGroups(accounts);
  assert.ok(group);
  assert.equal(group.connections.length, 4);
  assert.equal(group.ready, 1);
  assert.equal(group.enabled, 2);
  assert.equal(group.disabled, 1);
  assert.equal(group.expected, 3);
  assert.equal(group.backoff, 1);
  assert.equal(group.refused, 1);
  assert.equal(group.state, "degraded");
});

test("disabled-on-purpose accounts never count against readiness or turn a group red/amber", () => {
  const mixed = [
    makeConnection({ id: "c-ok", provider: "openai", name: "primary" }),
    makeConnection({ id: "c-off", provider: "openai", name: "standby", is_active: false }),
  ];
  const [mixedGroup] = buildProviderGroups(mixed);
  assert.ok(mixedGroup);
  assert.equal(mixedGroup.ready, 1);
  assert.equal(mixedGroup.disabled, 1);
  assert.equal(mixedGroup.expected, 1);
  assert.equal(mixedGroup.state, "healthy");

  const summary = summarizeProviders(mixed);
  assert.equal(summary.ready, 1);
  assert.equal(summary.expected, 1);
  assert.equal(summary.disabled, 1);
  assert.equal(summary.attention, 0);

  const allOff = [
    makeConnection({ id: "c-off-1", provider: "openai", name: "parked", is_active: false }),
  ];
  const [offGroup] = buildProviderGroups(allOff);
  assert.ok(offGroup);
  assert.equal(offGroup.ready, 0);
  assert.equal(offGroup.expected, 0);
  assert.equal(offGroup.disabled, 1);
  assert.equal(offGroup.state, "disabled");
});

test("stale last_error with backoff 0 maps to warn Last error status and agrees with Needs attention", () => {
  const accounts = [
    makeConnection({ id: "c-ok", provider: "codex", name: "codex-work" }),
    makeConnection({
      id: "c-stale",
      provider: "openrouter",
      name: "openrouter-free",
      last_error: "Provider returned error",
      backoff_level: 0,
    }),
    makeConnection({ id: "c-off", provider: "openai", name: "openai-backup", is_active: false }),
  ];

  const staleStatus = accountStatus(accounts[1]!);
  assert.equal(staleStatus.kind, "last_error");
  assert.equal(staleStatus.ready, true);
  assert.equal(staleStatus.label, "Last error");
  assert.equal(staleStatus.health, "warning");
  assert.equal(accountNeedsAttention(accounts[1]!, staleStatus), true);

  const okStatus = accountStatus(accounts[0]!);
  assert.equal(okStatus.health, "healthy");
  assert.equal(accountNeedsAttention(accounts[0]!, okStatus), false);

  const groups = buildProviderGroups(accounts);
  const summary = summarizeProviders(accounts);
  assert.equal(summary.ready, 2);
  assert.equal(summary.attention, 1);
  assert.equal(summary.disabled, 1);

  // This account's stale error keeps it routable, so its provider still reads healthy and
  // the "Need attention" card filter does not claim it; the account-level count does.
  assert.deepEqual(
    groups.map((group) => group.state),
    ["healthy", "healthy", "disabled"],
  );
  assert.equal(filterProviderGroups(groups, "attention").length, 0);
  assert.equal(summary.attention, 1);

  const disabledGroups = filterProviderGroups(groups, "disabled");
  assert.equal(disabledGroups.length, 1);
  assert.equal(disabledGroups[0]?.connections[0]?.id, "c-off");
});

test("buildTrafficByConnection and summarizeProviders roll up 24h requests and errors", () => {
  const rows: UsageSummaryByConnection[] = [
    {
      connection_id: "c-1",
      provider: "claude",
      model: "claude-sonnet-4-5",
      requests: 800,
      errors: 2,
      input_tokens: 1000,
      output_tokens: 500,
      cache_read_tokens: 0,
      cache_write_tokens: 0,
      cost_micros: 1_000_000,
      avg_latency_ms: 400,
    },
    {
      connection_id: "c-1",
      provider: "claude",
      model: "claude-opus-4-5",
      requests: 120,
      errors: 1,
      input_tokens: 500,
      output_tokens: 200,
      cache_read_tokens: 0,
      cache_write_tokens: 0,
      cost_micros: 500_000,
      avg_latency_ms: 600,
    },
  ];
  const map = buildTrafficByConnection(rows);
  assert.deepEqual(map.get("c-1"), { requests: 920, errors: 3 });

  const summary = summarizeProviders([makeConnection({ id: "c-1", provider: "claude" })], undefined, undefined, undefined, rows);
  assert.equal(summary.traffic24h.requests, 920);
  assert.equal(summary.traffic24h.errors, 3);
});

test("providers exports (until, providerLabel) stay intact", () => {
  assert.equal(providerLabel("opencode-go"), "OpenCode Go");
  assert.equal(until(null), "");
  assert.equal(until(1000 + 3 * 3600 - 5, 1000), "2h 59m");
  assert.equal(until(1000 + 12 * 86_400 - 5, 1000), "11d 23h");
  assert.equal(until(1000 + 3595, 1000), "59m");
  assert.equal(until(1000 + 20, 1000), "1m");
});

test("summary filters pick whole provider cards by state, never a subset of accounts", () => {
  const groups = buildProviderGroups([
    makeConnection({ id: "c-ok", provider: "openai", name: "primary" }),
    makeConnection({ id: "c-off", provider: "openai", name: "standby", is_active: false }),
    makeConnection({ id: "c-bad", provider: "claude", name: "broken", backoff_level: 2 }),
    makeConnection({ id: "c-parked", provider: "gemini", name: "parked", is_active: false }),
  ]);
  assert.equal(filterProviderGroups(groups, "connected").length, 3);
  // A healthy group keeps its disabled account: the card's counts stay honest.
  const healthy = filterProviderGroups(groups, "healthy");
  assert.deepEqual(healthy.map((group) => group.key), ["openai"]);
  assert.equal(healthy[0]?.connections.length, 2);
  assert.deepEqual(
    filterProviderGroups(groups, "attention").map((group) => group.key),
    ["claude"],
  );
  assert.deepEqual(
    filterProviderGroups(groups, "disabled").map((group) => group.key),
    ["gemini"],
  );
});

test("providerSentence answers the card in words, with the pressing fact last", () => {
  const [ready] = buildProviderGroups([
    makeConnection({ id: "c-1", provider: "openai", name: "a" }),
    makeConnection({ id: "c-2", provider: "openai", name: "b", is_active: false }),
  ]);
  assert.ok(ready);
  assert.deepEqual(providerSentence(ready, false), { text: "Ready · 2 accounts · 1 account off", tone: "pos" });

  const [down] = buildProviderGroups([
    makeConnection({ id: "c-3", provider: "claude", name: "x", backoff_level: 1 }),
  ]);
  assert.ok(down);
  assert.deepEqual(providerSentence(down, false), { text: "Not ready · unreachable right now", tone: "neg" });

  const nowSec = Math.floor(Date.now() / 1000);
  const sub: AccountSubscription = {
    connection_id: "c-4",
    name: "plus",
    provider: "openai",
    plan: "plus",
    windows: [
      { id: "monthly", label: "Monthly", used_percent: 84, window_seconds: 30 * 86400, resets_at: nowSec + 22 * 86400 },
    ],
    fetched_at: nowSec,
    error: null,
  };
  const [near] = buildProviderGroups([makeConnection({ id: "c-4", provider: "openai" })], undefined, undefined, [sub]);
  assert.ok(near);
  const sentence = providerSentence(near, true);
  assert.equal(sentence.tone, "info");
  // Under 90 % is worth saying but not alarming.
  assert.match(sentence.text, /^Ready · 1 account · 84% of quota used, resets in 22d/);
  // A reader sees no quota figure, so the same card stays plain.
  assert.deepEqual(providerSentence(near, false), { text: "Ready · 1 account", tone: "pos" });
});

test("accountSentence says usage in words and keeps a non-ready reason", () => {
  const conn = makeConnection({ id: "c-1", provider: "claude" });
  assert.equal(
    accountSentence(accountStatus(conn), { requests: 1200, errors: 3 }),
    "Ready · 1,200 requests in 24h, 3 errors",
  );
  assert.equal(accountSentence(accountStatus(conn)), "Ready · No requests in 24h");
  assert.equal(
    accountSentence(accountStatus({ ...conn, is_active: false })),
    "Off · excluded from routing",
  );
});

test("a card offers only the add flows this build supports, and key replacement trims", () => {
  const spec: ModelProvider = {
    id: "openai",
    display: "OpenAI",
    code: "oa",
    kind: "api_key",
    has_connection: false,
    scope: "global",
    listed: true,
    requires_base_url_override: false,
    base_url_example: null,
  };
  assert.deepEqual(providerOptions(spec, undefined), [
    { id: "openai", display: "OpenAI", kind: "key" },
  ]);
  const oauthRow = {
    id: "claude",
    display: "Claude",
    flow: "browser",
  } as unknown as OAuthProvider;
  assert.deepEqual(providerOptions(undefined, oauthRow), [
    { id: "claude", display: "Claude", kind: "oauth" },
  ]);
  assert.deepEqual(providerOptions(undefined, undefined), []);
  assert.equal(apiKeyReplacement("   "), null);
  assert.deepEqual(apiKeyReplacement(" sk-new "), { credentials: { api_key: "sk-new" } });
});
