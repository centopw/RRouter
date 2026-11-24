/**
 * Unit tests for Free mode:
 *   1. Advanced document validation (`free-mode-document.ts`)
 *   2. Frozen API contract parsers, cache reducers, status classification, headline copy,
 *      fallback preview, and sort/filter partitioning (`pages/free-mode/model.ts`)
 *
 * Run with `npm test` (Node's built-in runner over the TS sources).
 */
import assert from "node:assert/strict";
import { test } from "node:test";
import {
  FREE_DOCUMENT_EXAMPLE,
  validateFreeDocument,
} from "../src/lib/free-mode-document.ts";
import {
  applyProbeAllToState,
  applyProbeSingleToState,
  applyToggleToState,
  authModeLabel,
  compactEndpoint,
  fallbackPreview,
  filterUsableModels,
  modelStatus,
  parseFreeModeState,
  parseModelToggleResult,
  parseProbeAllResult,
  parseProbeSingleResult,
  partitionModels,
  statusHeadline,
  type FreeModel,
  type FreeModeState,
} from "../src/pages/free-mode/model.ts";

// ---------------------------------------------------------------- document validation

test("the shipped example validates with no problems", () => {
  const check = validateFreeDocument(FREE_DOCUMENT_EXAMPLE);
  assert.deepEqual(check.problems, []);
  assert.equal(check.providers, 1);
  assert.equal(check.accounts, 2);
});

test("broken JSON is one problem, never a crash", () => {
  const check = validateFreeDocument('{"providers": [}');
  assert.equal(check.document, null);
  assert.equal(check.problems.length, 1);
  assert.equal(check.problems[0]?.path, "document");
  assert.match(check.problems[0]?.message ?? "", /not valid JSON/);
});

test("an empty editor asks for a document", () => {
  const check = validateFreeDocument("   \n");
  assert.deepEqual(
    check.problems.map((problem) => problem.path),
    ["document"],
  );
});

test("an unknown proxy key is named exactly like the gateway names it", () => {
  const check = validateFreeDocument(
    JSON.stringify({
      providers: [{ provider: "openrouter", accounts: [{ label: "acc-1", proxy: "p9" }] }],
    }),
  );
  assert.deepEqual(check.problems, [
    { path: "providers[0].accounts[0].proxy", message: "unknown proxy key p9" },
  ]);
});

test("a proxy key resolves through proxies, and a full URL needs an allowed scheme", () => {
  const ok = validateFreeDocument(
    JSON.stringify({
      proxies: { p1: "socks5h://10.0.0.2:1080" },
      providers: [
        {
          provider: "openrouter",
          accounts: [
            { label: "a", proxy: "p1" },
            { label: "b", proxy: "https://127.0.0.1:1080" },
          ],
        },
      ],
    }),
  );
  assert.deepEqual(ok.problems, []);

  const bad = validateFreeDocument(
    JSON.stringify({
      proxies: { p1: "ftp://10.0.0.2:21" },
      providers: [{ provider: "openrouter", accounts: [{ label: "a", proxy: "p1" }] }],
    }),
  );
  assert.deepEqual(
    bad.problems.map((problem) => problem.path),
    ["proxies.p1", "providers[0].accounts[0].proxy"],
  );
});

test("accounts are identified by (provider, label), so a duplicate is a problem", () => {
  const check = validateFreeDocument(
    JSON.stringify({
      providers: [
        {
          provider: "openrouter",
          accounts: [{ label: "acc-1" }, { label: " acc-1 " }],
        },
      ],
    }),
  );
  assert.equal(check.problems.length, 1);
  assert.equal(check.problems[0]?.path, "providers[0].accounts[1].label");
  assert.match(check.problems[0]?.message ?? "", /duplicate label acc-1/);
});

test("routing.models must be a list of model ids", () => {
  const check = validateFreeDocument(
    JSON.stringify({
      routing: { models: ["ok", 3] },
      providers: [{ provider: "openrouter", accounts: [{ label: "a" }] }],
    }),
  );
  assert.deepEqual(check.problems, [
    { path: "routing.models", message: "must be a list of model ids" },
  ]);
});

// ---------------------------------------------------------------- frozen API contract

function fixturePayload() {
  return {
    enabled: false,
    fallback_on_quota: false,
    fallback_triggers: ["quota", "429", "402"],
    ref: "free",
    last_checked_at: 1763890000,
    discovered_at: 1763890000,
    summary: { total: 4, usable: 3, enabled: 3, working: 1, unusable: 1 },
    next_fallback_model: "big-pickle",
    models: [
      {
        provider: "opencode",
        provider_display: "OpenCode",
        model: "big-pickle",
        display: "Big Pickle",
        ref: "free/big-pickle",
        endpoint: "https://opencode.ai/inference/openai/v1/chat/completions",
        dialect: "openai_chat",
        free: true,
        enabled: true,
        usable: true,
        unusable_reason: null,
        auth_mode: "key_or_anonymous",
        has_account: false,
        needs_key: false,
        price: 0,
        source: "public-catalog",
        discovered_at: 1763890000,
        probe: {
          ok: true,
          status: 200,
          latency_ms: 820,
          message: null,
          needs_key: false,
          at: 1763890000,
        },
      },
      {
        provider: "opencode",
        provider_display: "OpenCode",
        model: "nemotron-3.5-super-vl-free",
        display: "Nemotron 3.5 Super VL Free",
        ref: "free/nemotron-3.5-super-vl-free",
        endpoint: "https://opencode.ai/inference/openai/v1/chat/completions",
        dialect: "openai_chat",
        free: true,
        enabled: true,
        usable: true,
        unusable_reason: null,
        auth_mode: "key_or_anonymous",
        has_account: false,
        needs_key: true,
        price: 0,
        source: "public-catalog",
        discovered_at: 1763890000,
        probe: {
          ok: false,
          status: 429,
          latency_ms: 190,
          message: "Free usage limit reached.",
          needs_key: true,
          at: 1763890000,
        },
      },
      {
        provider: "openrouter",
        provider_display: "OpenRouter",
        model: "inclusionai/ling-3.2-flash:free",
        display: "Ling 3.2 Flash",
        ref: "free/inclusionai/ling-3.2-flash:free",
        endpoint: "https://openrouter.ai/api/v1/chat/completions",
        dialect: "openai_chat",
        free: true,
        enabled: true,
        usable: true,
        unusable_reason: null,
        auth_mode: "key_required",
        has_account: false,
        needs_key: true,
        price: 0,
        source: "public-catalog",
        discovered_at: 1763890000,
        probe: null,
      },
      {
        provider: "opencode",
        provider_display: "OpenCode",
        model: "jev-1.13-free",
        display: "Jev 1.13 Free",
        ref: "free/jev-1.13-free",
        endpoint: "https://opencode.ai/inference/systemone/v1",
        dialect: "systemone",
        free: true,
        enabled: false,
        usable: false,
        unusable_reason: "Speaks the systemone dialect, which RRouter does not support",
        auth_mode: "key_or_anonymous",
        has_account: false,
        needs_key: false,
        price: 0,
        source: "public-catalog",
        discovered_at: 1763890000,
        probe: null,
      },
    ],
    catalogs: [
      {
        provider: "opencode",
        url: "https://opencode.ai/inference/v1/models",
        discovered_at: 1763890000,
        models: 11,
        usable: 10,
        error: null,
      },
      {
        provider: "openrouter",
        url: "https://openrouter.ai/api/v1/models",
        discovered_at: 1763890000,
        models: 15,
        usable: 15,
        error: null,
      },
    ],
    document: null,
    routing: {
      enabled: false,
      fallback_on_quota: false,
      per_account_rpm: null,
      max_attempts: 8,
      prefer: "spread",
      disabled_models: [],
    },
    updated_at: null,
    accounts: [],
  };
}

test("parseFreeModeState parses the frozen contract and preserves zero-config defaults", () => {
  const state = parseFreeModeState(fixturePayload());
  assert.equal(state.enabled, false);
  assert.equal(state.fallback_on_quota, false);
  assert.deepEqual(state.fallback_triggers, ["quota", "429", "402"]);
  assert.equal(state.ref, "free");
  assert.equal(state.summary.total, 4);
  assert.equal(state.summary.usable, 3);
  assert.equal(state.summary.unusable, 1);
  assert.equal(state.next_fallback_model, "big-pickle");
  assert.equal(state.catalogs.length, 2);
  assert.equal(state.models[0]?.probe?.latency_ms, 820);
  assert.equal(state.models[3]?.usable, false);
  assert.equal(state.models[3]?.enabled, false);
});

test("modelStatus classifies working, rate-limited with needs_key, unprobed key-required, off, and unusable", () => {
  const state = parseFreeModeState(fixturePayload());
  const [working, rateLimited, keyReq, unusable] = state.models as [
    FreeModel,
    FreeModel,
    FreeModel,
    FreeModel,
  ];

  const s1 = modelStatus(working);
  assert.equal(s1.kind, "working");
  assert.equal(s1.tone, "pos");
  assert.equal(s1.showAddKeyLink, false);

  const s2 = modelStatus(rateLimited);
  assert.equal(s2.kind, "rate_limited");
  assert.equal(s2.tone, "warn");
  assert.equal(s2.detail, "Free usage limit reached.");
  assert.equal(s2.showAddKeyLink, true);

  const s3 = modelStatus(keyReq);
  assert.equal(s3.kind, "needs_key");
  assert.equal(s3.tone, "warn");
  assert.equal(s3.showAddKeyLink, true);

  const s4 = modelStatus(unusable);
  assert.equal(s4.kind, "unusable");
  assert.match(s4.detail ?? "", /systemone/);

  const sOff = modelStatus({ ...working, enabled: false });
  assert.equal(sOff.kind, "off");
  assert.equal(sOff.label, "off");
});

test("statusHeadline and fallbackPreview accurately reflect click 0, click 1, and click 2", () => {
  // Click 0: fresh zero-config state (off)
  const click0 = parseFreeModeState(fixturePayload());
  const h0 = statusHeadline(click0, false);
  assert.equal(h0.title, "Free models are off.");
  assert.match(h0.detail, /3 free models found on OpenCode and OpenRouter/);
  const f0 = fallbackPreview(click0);
  assert.equal(f0.warning, false);
  assert.match(f0.summary, /Turn Free models on/);

  // Click 1: pool turned on, probes returned
  const click1: FreeModeState = { ...click0, enabled: true };
  const h1 = statusHeadline(click1, false);
  assert.equal(h1.title, "Free models are on. 1 of 3 answer right now.");
  assert.match(h1.detail, /When paid quota runs out: off/);
  const f1 = fallbackPreview(click1);
  assert.match(f1.summary, /big-pickle first/);

  // Click 2: automatic fallback turned on
  const click2: FreeModeState = { ...click1, fallback_on_quota: true };
  const h2 = statusHeadline(click2, false);
  assert.match(h2.detail, /retry on a free model/);
  const f2 = fallbackPreview(click2);
  assert.equal(f2.warning, false);
  assert.match(f2.summary, /429\/402 or an exhausted quota/);
  assert.match(f2.summary, /big-pickle first/);

  // Edge: fallback on while pool is off warns honestly
  const mismatch = fallbackPreview({ ...click0, fallback_on_quota: true });
  assert.equal(mismatch.warning, true);
  assert.match(mismatch.summary, /free pool is off/);
});

test("partitionModels separates unusable models into footnote and ranks working fastest first", () => {
  const state = parseFreeModeState(fixturePayload());
  const buckets = partitionModels(state.models);
  assert.equal(buckets.usable.length, 3);
  assert.equal(buckets.unusable.length, 1);
  assert.equal(buckets.unusable[0]?.model, "jev-1.13-free");
  assert.equal(buckets.usable[0]?.model, "big-pickle");
  assert.equal(buckets.counts.all, 3);
  assert.equal(buckets.counts.working, 1);
  assert.equal(buckets.counts.needs_key, 2);
  assert.equal(buckets.counts.off, 0);

  const openrouterOnly = filterUsableModels(buckets.usable, "all", "openrouter");
  assert.equal(openrouterOnly.length, 1);
  assert.equal(openrouterOnly[0]?.model, "inclusionai/ling-3.2-flash:free");
});

test("cache reducers update model toggle and probe outcomes consistently", () => {
  const base = parseFreeModeState(fixturePayload());

  const toggleRes = parseModelToggleResult({
    ok: true,
    provider: "opencode",
    model: "big-pickle",
    enabled: false,
    summary: { total: 4, usable: 3, enabled: 2, working: 0, unusable: 1 },
    next_fallback_model: "nemotron-3.5-super-vl-free",
  });
  const afterToggle = applyToggleToState(base, toggleRes);
  assert.equal(afterToggle.models[0]?.enabled, false);
  assert.equal(afterToggle.summary.working, 0);
  assert.equal(afterToggle.next_fallback_model, "nemotron-3.5-super-vl-free");

  const probeRes = parseProbeSingleResult({
    provider: "opencode",
    model: "nemotron-3.5-super-vl-free",
    endpoint: "https://opencode.ai/inference/openai/v1/chat/completions",
    probe: {
      ok: true,
      status: 200,
      latency_ms: 410,
      message: null,
      needs_key: false,
      at: 1763890050,
    },
  });
  const afterProbe = applyProbeSingleToState(afterToggle, probeRes);
  assert.equal(afterProbe.models[1]?.probe?.ok, true);
  assert.equal(afterProbe.models[1]?.needs_key, false);
  assert.equal(afterProbe.summary.working, 1);
  assert.equal(afterProbe.last_checked_at, 1763890050);

  const probeAll = parseProbeAllResult({
    ok: true,
    checked_at: 1763890099,
    results: [
      {
        provider: "openrouter",
        model: "inclusionai/ling-3.2-flash:free",
        endpoint: "https://openrouter.ai/api/v1/chat/completions",
        probe: {
          ok: true,
          status: 200,
          latency_ms: 250,
          message: null,
          needs_key: false,
          at: 1763890099,
        },
      },
    ],
  });
  const afterAll = applyProbeAllToState(afterProbe, probeAll);
  assert.equal(afterAll.summary.working, 2);
  assert.equal(afterAll.next_fallback_model, "inclusionai/ling-3.2-flash:free");
  assert.equal(afterAll.last_checked_at, 1763890099);
});

test("authModeLabel and compactEndpoint surface anonymous mode and readable endpoints", () => {
  assert.equal(authModeLabel({ auth_mode: "key_or_anonymous", has_account: false }), "anonymous");
  assert.equal(authModeLabel({ auth_mode: "key_or_anonymous", has_account: true }), "account connected");
  assert.equal(authModeLabel({ auth_mode: "key_required", has_account: false }), "key required");
  assert.equal(
    compactEndpoint("https://opencode.ai/inference/openai/v1/chat/completions"),
    "opencode.ai/inference/openai/v1/chat/completions",
  );
  assert.equal(compactEndpoint(null), "—");
});
