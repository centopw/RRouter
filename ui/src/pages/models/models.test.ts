/**
 * Pure unit tests for the Models screen: price vocabulary, zero-price Free detection,
 * combo aggregate/member formatting, row availability, unified summary counts, and
 * honest empty-state classification.
 *
 * Run with `node --test src/pages/models/models.test.ts`.
 */
import assert from "node:assert/strict";
import { test } from "node:test";
import type { Capabilities, Combo, Model } from "../../lib/api.ts";
import { cacheText, dollars, isFree, priceText, rangeText } from "./price.ts";
import {
  comboBlocked,
  comboCapabilitySplit,
  comboIsFree,
  comboPriceText,
  emptyStateKind,
  nameDetail,
  qualifiedTokenRange,
  rowIsAvailable,
  rowIsFree,
  rowsFor,
  summarizeRows,
  type ComboAggregate,
} from "./unified-rows.ts";

const EMPTY_CAPS: Capabilities = {
  vision: false,
  pdf: false,
  audio_input: false,
  video_input: false,
  image_output: false,
  audio_output: false,
  tools: false,
  reasoning: false,
  search: false,
};

function makeModel(overrides: Partial<Model> & { ref: string; provider: string; id: string }): Model {
  return {
    code: overrides.provider,
    display: overrides.id,
    context: 200_000,
    max_output: 16_384,
    capabilities: { ...EMPTY_CAPS, tools: true },
    cost: { input: 3, output: 15 },
    source: "catalog",
    enabled: true,
    available: true,
    deprecated: false,
    ...overrides,
  };
}

test("isFree: strictly zero on every published price dimension; missing is unknown, never free", () => {
  assert.equal(isFree(null), false);
  assert.equal(isFree(undefined), false);
  assert.equal(isFree({ input: 0, output: 0 }), true);
  assert.equal(isFree({ input: 0, output: 0, cache_read: 0, cache_write: 0 }), true);
  assert.equal(isFree({ input: 0, output: 0.5 }), false);
  assert.equal(isFree({ input: 0, output: 0, cache_read: 0.1 }), false);
});

test("priceText and cacheText keep units explicit and never fabricate $0 for unknown", () => {
  assert.equal(priceText(null), "price —");
  assert.equal(priceText({ input: 0, output: 0 }), "Free");
  assert.equal(priceText({ input: 0.075, output: 0.3 }), "$0.075 in · $0.30 out /1M");
  assert.equal(dollars(3), "$3.00");
  assert.equal(cacheText({ input: 0, output: 0, cache_read: 0 }), null);
  assert.equal(
    cacheText({ input: 3, output: 15, cache_read: 0.3, cache_write: 3.75 }),
    "cache read $0.30 · cache write $3.75 /1M",
  );
  assert.equal(rangeText({ min: 1, max: 5 }), "$1.00–$5.00");
  assert.equal(rangeText({ min: 3, max: 3 }), "$3.00");
  assert.equal(rangeText({ min: null, max: 5 }), null);
});

test("combo ranges are always qualified (guaranteed/up to or all candidates), never bare model specs", () => {
  assert.equal(
    qualifiedTokenRange({ min: 256_000, max: 1_000_000 }, "ctx"),
    "guaranteed 256K · up to 1M ctx",
  );
  assert.equal(
    qualifiedTokenRange({ min: 200_000, max: 200_000 }, "ctx"),
    "all candidates 200K ctx",
  );
  assert.equal(qualifiedTokenRange({ min: null, max: null }, "ctx"), null);
});

test("comboIsFree requires every candidate to be priced and zero", () => {
  const freeAgg: ComboAggregate = {
    context: { min: 128_000, max: 256_000 },
    max_output: { min: 8_192, max: 16_384 },
    pricing: {
      input: { min: 0, max: 0 },
      output: { min: 0, max: 0 },
      cache_read: { min: 0, max: 0 },
      cache_write: { min: 0, max: 0 },
      known: 2,
      total: 2,
    },
    availability: "ok",
  };
  assert.equal(comboIsFree(freeAgg), true);
  assert.equal(comboPriceText(freeAgg), "Free");

  // Partial pricing coverage: one unpriced candidate could be paid => not free.
  const partialAgg: ComboAggregate = {
    ...freeAgg,
    pricing: { ...freeAgg.pricing!, known: 1, total: 2 },
  };
  assert.equal(comboIsFree(partialAgg), false);
  assert.equal(
    comboPriceText(partialAgg),
    "all candidates $0.00 in · $0.00 out /1M (1/2 priced)",
  );

  // Paid range includes candidate range qualifier.
  const paidAgg: ComboAggregate = {
    ...freeAgg,
    pricing: {
      input: { min: 1, max: 5 },
      output: { min: 5, max: 25 },
      known: 2,
      total: 2,
    },
  };
  assert.equal(
    comboPriceText(paidAgg),
    "$1.00–$5.00 in · $5.00–$25.00 out /1M · candidate range",
  );
});

test("comboCapabilitySplit separates guaranteed (AND) from some-only (OR)", () => {
  const split = comboCapabilitySplit({
    context: null,
    max_output: null,
    pricing: null,
    guaranteed_capabilities: { ...EMPTY_CAPS, tools: true },
    supported_capabilities: { ...EMPTY_CAPS, tools: true, vision: true, reasoning: true },
    availability: "ok",
  });
  assert.equal(split.known, true);
  assert.deepEqual(split.guaranteed, ["tools"]);
  assert.deepEqual(split.someOnly, ["vision", "reasoning"]);
});

test("rowsFor, summarizeRows, rowIsAvailable and rowIsFree agree across models, aliases, combos", () => {
  const paid = makeModel({
    ref: "anthropic/claude-sonnet-4-6",
    provider: "anthropic",
    id: "claude-sonnet-4-6",
    cost: { input: 3, output: 15 },
    available: true,
  });
  const disconnected = makeModel({
    ref: "openai/gpt-5",
    provider: "openai",
    id: "gpt-5",
    cost: { input: 2.5, output: 10 },
    available: false,
  });
  const free = makeModel({
    ref: "opencode/big-pickle",
    provider: "opencode",
    id: "big-pickle",
    cost: { input: 0, output: 0 },
    available: true,
  });

  const combo: Combo & { aggregate: ComboAggregate } = {
    name: "free-chain",
    models: ["opencode/big-pickle"],
    strategy: "fallback",
    aggregate: {
      context: { min: 200_000, max: 200_000 },
      max_output: { min: 16_384, max: 16_384 },
      pricing: {
        input: { min: 0, max: 0 },
        output: { min: 0, max: 0 },
        known: 1,
        total: 1,
      },
      availability: "ok",
    },
  };
  const brokenCombo: Combo & { aggregate: ComboAggregate } = {
    name: "broken",
    models: ["openai/gpt-5"],
    strategy: "fallback",
    aggregate: {
      context: null,
      max_output: null,
      pricing: null,
      availability: "no-targets",
    },
  };

  const entries = new Map([
    [paid.ref, { ref: paid.ref, visible: true }],
    [disconnected.ref, { ref: disconnected.ref, visible: true }],
    [free.ref, { ref: free.ref, visible: true }],
    ["rrouter/free-alias", { ref: "rrouter/free-alias", visible: true }],
    ["rrouter/free-chain", { ref: "rrouter/free-chain", visible: true }],
    ["rrouter/broken", { ref: "rrouter/broken", visible: true }],
  ]);
  const modelByRef = new Map([
    [paid.ref, paid],
    [disconnected.ref, disconnected],
    [free.ref, free],
  ]);

  const rows = rowsFor({
    models: [paid, disconnected, free],
    aliases: [{ alias: "free-alias", target: free.ref }],
    combos: [combo, brokenCombo],
    entries,
    presetAware: true,
    autoInclude: true,
  });

  assert.equal(rows.length, 6);
  const byRef = new Map(rows.map((r) => [r.ref, r]));

  assert.equal(rowIsAvailable(byRef.get(paid.ref)!, modelByRef), true);
  assert.equal(rowIsAvailable(byRef.get(disconnected.ref)!, modelByRef), false);
  assert.equal(rowIsAvailable(byRef.get("rrouter/free-alias")!, modelByRef), true);
  assert.equal(rowIsAvailable(byRef.get("rrouter/free-chain")!, modelByRef), true);
  assert.equal(rowIsAvailable(byRef.get("rrouter/broken")!, modelByRef), false);
  assert.equal(comboBlocked(brokenCombo.aggregate), "no candidate is connected");

  assert.equal(rowIsFree(byRef.get(paid.ref)!, modelByRef), false);
  assert.equal(rowIsFree(byRef.get(free.ref)!, modelByRef), true);
  assert.equal(rowIsFree(byRef.get("rrouter/free-alias")!, modelByRef), true);
  assert.equal(rowIsFree(byRef.get("rrouter/free-chain")!, modelByRef), true);
  assert.equal(rowIsFree(byRef.get("rrouter/broken")!, modelByRef), false);

  const summary = summarizeRows(rows, modelByRef);
  assert.deepEqual(summary, {
    total: 6,
    routed: 6,
    available: 4,
    routedAvailable: 4,
    free: 3,
    routedFree: 3,
  });

  // Free combo omits redundant price text in nameDetail because the row carries FreeBadge.
  const freeComboRow = byRef.get("rrouter/free-chain")!;
  assert.equal(freeComboRow.kind, "combo");
  if (freeComboRow.kind === "combo") {
    assert.equal(nameDetail(freeComboRow), "1 candidate · all candidates 200K ctx");
  }
});

test("emptyStateKind never falsely blames the preset when filters or errors cause 0 rows", () => {
  const base = {
    hasError: false,
    query: "",
    providerIsAll: true,
    kindIsAll: true,
    customOnly: false,
    freeOnly: false,
    statusAware: true,
    statuses: new Set(["routed"]),
    draftEntryCount: 10,
    counts: { routed: 0, hidden: 0, disabled: 0, unrouted: 0 },
  };
  assert.equal(emptyStateKind({ ...base, hasError: true }), "error");
  assert.equal(emptyStateKind({ ...base, freeOnly: true }), "filtered_free");
  assert.equal(emptyStateKind({ ...base, kindIsAll: false }), "filtered");
  assert.equal(
    emptyStateKind({ ...base, draftEntryCount: 0, counts: { ...base.counts, unrouted: 5 } }),
    "preset_empty",
  );
  assert.equal(
    emptyStateKind({ ...base, counts: { ...base.counts, disabled: 3 } }),
    "all_disabled",
  );
  assert.equal(
    emptyStateKind({ ...base, counts: { ...base.counts, hidden: 2 } }),
    "all_hidden",
  );
});
