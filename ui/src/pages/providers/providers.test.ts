import assert from "node:assert/strict";
import { test } from "node:test";
import type {
  AccountQuotaRow,
  AccountSubscription,
  Connection,
  ModelProvider,
} from "../../lib/api.ts";
import {
  accountStatus,
  barFill,
  barText,
  buildProviderGroups,
  providerLabel,
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
  assert.equal(status.label, "no endpoint");

  const [group] = buildProviderGroups([azure], [azureSpec]);
  assert.ok(group);
  assert.equal(group.ready, 0);
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

  const [group] = buildProviderGroups([conn], undefined, [quota], [sub]);
  assert.ok(group);
  assert.equal(group.ready, 0);
  assert.equal(group.state, "down");
});

test("buildProviderGroups counts ready accounts accurately across mixed states", () => {
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
  assert.equal(group.backoff, 1);
  assert.equal(group.refused, 1);
  assert.equal(group.state, "degraded");
});

test("healthy when an account is manually disabled but every enabled account is ready", () => {
  const accounts = [
    makeConnection({ id: "c-ok", provider: "openai", name: "primary" }),
    makeConnection({ id: "c-off", provider: "openai", name: "standby", is_active: false }),
  ];
  const [group] = buildProviderGroups(accounts);
  assert.ok(group);
  assert.equal(group.ready, 1);
  assert.equal(group.state, "healthy");
});

test("shared Usage exports (barFill, barText, until, providerLabel) stay intact", () => {
  assert.match(barFill(50), /bg-muted/);
  assert.match(barFill(75), /warn-fill/);
  assert.match(barFill(95), /neg-fill/);
  assert.match(barText(50), /text-muted/);
  assert.match(barText(75), /warn/);
  assert.match(barText(95), /neg/);
  assert.equal(providerLabel("opencode-go"), "OpenCode Go");
  assert.equal(until(null), "");
});
