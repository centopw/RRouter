/**
 * Pure model-ref resolution, canonical copy refs, alternate spellings, and Activity
 * display helpers (`requestedRefOf`, `routedTargetOf`, `sameAsRef`).
 *
 * Run with `npm test` (Node's built-in runner over the TS sources).
 */
import assert from "node:assert/strict";
import { test } from "node:test";
import type { Model } from "../src/lib/api.ts";
import {
  alsoMatches,
  canonicalEventRef,
  canonicalRef,
  findModelRef,
  modelRefFor,
  requestedRefOf,
  routedTargetOf,
  sameAsRef,
  type ModelRefGroup,
} from "../src/lib/model-ref.ts";

function makeModel(overrides: Partial<Model> & Pick<Model, "provider" | "code" | "id">): Model {
  return {
    ref: overrides.ref ?? `${overrides.provider}/${overrides.id}`,
    display: null,
    context: 128_000,
    max_output: 8192,
    capabilities: {
      vision: false,
      pdf: false,
      audio_input: false,
      video_input: false,
      image_output: false,
      audio_output: false,
      tools: true,
      reasoning: false,
      search: false,
    },
    cost: null,
    source: "catalog",
    enabled: true,
    available: true,
    deprecated: false,
    ...overrides,
  };
}

const REGISTRY: Model[] = [
  makeModel({ provider: "opencode-go", code: "ocg", id: "deepseek-v4" }),
  makeModel({ provider: "opencode-go", code: "ocg", id: "deepseek-v4.1-flash" }),
  makeModel({ provider: "openai", code: "openai", id: "gpt-4o" }),
  makeModel({ provider: "openrouter", code: "or", id: "anthropic/claude-3.5-sonnet" }),
  makeModel({ provider: "rrouter", code: "rrouter", id: "fast-combo", ref: "fast-combo" }),
];

const GROUPS: ModelRefGroup[] = [
  { id: "g-main", name: "main" },
  { id: "g-prod", name: "Production", slug: "prod" },
  { id: "provider:opencode-go", name: "OpenCode Go", provider: "opencode-go" },
];

test("canonicalRef yields provider/id for ungrouped rows and provider buckets", () => {
  const row = REGISTRY[0]!;
  assert.equal(canonicalRef(row), "opencode-go/deepseek-v4");
  assert.equal(canonicalRef(row, GROUPS), "opencode-go/deepseek-v4");
  assert.equal(canonicalRef(row, GROUPS, "provider:opencode-go"), "opencode-go/deepseek-v4");
  assert.equal(canonicalRef(row, GROUPS, "missing-group"), "opencode-go/deepseek-v4");
});

test("canonicalRef yields group/code/id for custom groups and prefers stable slug", () => {
  const row = REGISTRY[0]!;
  assert.equal(canonicalRef(row, GROUPS, "g-main"), "main/ocg/deepseek-v4");
  assert.equal(canonicalRef({ ...row, group: "g-main" }, GROUPS), "main/ocg/deepseek-v4");
  assert.equal(canonicalRef(row, GROUPS, "g-prod"), "prod/ocg/deepseek-v4");
});

test("canonicalRef falls back to provider when code is absent and keeps rrouter callables bare", () => {
  assert.equal(
    canonicalRef({ provider: "openai", id: "gpt-4o" }, GROUPS, "g-main"),
    "main/openai/gpt-4o",
  );
  assert.equal(
    canonicalRef({ provider: "rrouter", id: "fast-combo" }, GROUPS, "g-main"),
    "fast-combo",
  );
});

test("alsoMatches returns other accepted spellings without duplicating canonicalRef", () => {
  const ocg = REGISTRY[0]!;
  assert.deepEqual(alsoMatches(ocg, GROUPS, "g-main"), [
    "ocg/deepseek-v4",
    "opencode-go/deepseek-v4",
  ]);
  assert.deepEqual(alsoMatches(ocg, GROUPS, "g-prod"), [
    "Production/ocg/deepseek-v4",
    "ocg/deepseek-v4",
    "opencode-go/deepseek-v4",
  ]);
  assert.deepEqual(alsoMatches(ocg, GROUPS, "provider:opencode-go"), [
    "ocg/deepseek-v4",
  ]);

  const openai = REGISTRY[2]!;
  assert.deepEqual(alsoMatches(openai, GROUPS, "g-main"), ["openai/gpt-4o"]);
  assert.deepEqual(alsoMatches(openai, GROUPS), []);

  assert.deepEqual(alsoMatches({ provider: "rrouter", id: "fast-combo" }, GROUPS, "g-main"), []);
});

test("findModelRef resolves exact refs, provider/id, code/id, 3-segment group refs, and slash-containing ids", () => {
  assert.equal(findModelRef(REGISTRY, "opencode-go/deepseek-v4")?.id, "deepseek-v4");
  assert.equal(findModelRef(REGISTRY, "ocg/deepseek-v4")?.id, "deepseek-v4");
  assert.equal(findModelRef(REGISTRY, "main/ocg/deepseek-v4.1-flash")?.id, "deepseek-v4.1-flash");
  assert.equal(findModelRef(REGISTRY, "main/opencode-go/deepseek-v4")?.id, "deepseek-v4");
  assert.equal(
    findModelRef(REGISTRY, "openrouter/anthropic/claude-3.5-sonnet")?.id,
    "anthropic/claude-3.5-sonnet",
  );
  assert.equal(
    findModelRef(REGISTRY, "main/or/anthropic/claude-3.5-sonnet")?.id,
    "anthropic/claude-3.5-sonnet",
  );
  assert.equal(findModelRef(REGISTRY, "unknown/model"), undefined);
});

test("requestedRefOf returns client literal requested_model verbatim and falls back to model", () => {
  assert.equal(
    requestedRefOf({ requested_model: "main/ocg/deepseek-v4.1-flash", model: "deepseek-v4.1-flash" }),
    "main/ocg/deepseek-v4.1-flash",
  );
  assert.equal(
    requestedRefOf({ requested_model: null, model: "ocg/deepseek-v4" }),
    "ocg/deepseek-v4",
  );
  assert.equal(
    requestedRefOf({ requested_model: "   ", model: "opencode-go/deepseek-v4" }),
    "opencode-go/deepseek-v4",
  );
});

test("routedTargetOf resolves actual provider/model served across all spellings", () => {
  assert.equal(
    routedTargetOf({ provider: "opencode-go", model: "main/ocg/deepseek-v4.1-flash" }, REGISTRY),
    "opencode-go/deepseek-v4.1-flash",
  );
  assert.equal(
    routedTargetOf({ provider: "opencode-go", model: "ocg/deepseek-v4" }, REGISTRY),
    "opencode-go/deepseek-v4",
  );
  assert.equal(
    routedTargetOf({ provider: "opencode-go", model: "opencode-go/deepseek-v4" }, REGISTRY),
    "opencode-go/deepseek-v4",
  );
  assert.equal(
    routedTargetOf({ provider: "opencode-go", model: "deepseek-v4" }, REGISTRY),
    "opencode-go/deepseek-v4",
  );
  // Failed before dispatch: no provider was served.
  assert.equal(routedTargetOf({ provider: "", model: "missing" }, REGISTRY), "");
});

test("sameAsRef flags alternate spellings neutrally and stays null when spellings match or destination is unknown", () => {
  assert.equal(
    sameAsRef(
      {
        provider: "opencode-go",
        model: "main/ocg/deepseek-v4.1-flash",
        requested_model: "main/ocg/deepseek-v4.1-flash",
      },
      REGISTRY,
    ),
    "opencode-go/deepseek-v4.1-flash",
  );
  assert.equal(
    sameAsRef(
      {
        provider: "opencode-go",
        model: "ocg/deepseek-v4",
        requested_model: "ocg/deepseek-v4",
      },
      REGISTRY,
    ),
    "opencode-go/deepseek-v4",
  );
  // Exact match with canonical ref -> no note.
  assert.equal(
    sameAsRef(
      {
        provider: "opencode-go",
        model: "opencode-go/deepseek-v4",
        requested_model: "opencode-go/deepseek-v4",
      },
      REGISTRY,
    ),
    null,
  );
  // Failed before dispatch -> no destination, so no same-as note.
  assert.equal(
    sameAsRef(
      {
        provider: "",
        model: "missing-model",
        requested_model: "missing-model",
      },
      REGISTRY,
    ),
    null,
  );
});

test("canonicalEventRef and modelRefFor stay consistent with registry refs", () => {
  const groupedRegistry: Model[] = [
    makeModel({
      provider: "opencode-go",
      code: "ocg",
      id: "deepseek-v4",
      ref: "main/ocg/deepseek-v4",
    }),
  ];
  assert.equal(
    canonicalEventRef({ provider: "opencode-go", model: "ocg/deepseek-v4" }, groupedRegistry),
    "main/ocg/deepseek-v4",
  );
  assert.equal(
    sameAsRef(
      {
        provider: "opencode-go",
        model: "opencode-go/deepseek-v4",
        requested_model: "opencode-go/deepseek-v4",
      },
      groupedRegistry,
    ),
    "main/ocg/deepseek-v4",
  );
  assert.equal(
    sameAsRef(
      {
        provider: "opencode-go",
        model: "main/ocg/deepseek-v4",
        requested_model: "main/ocg/deepseek-v4",
      },
      groupedRegistry,
    ),
    null,
  );
  assert.equal(
    modelRefFor("opencode-go", "deepseek-v4", groupedRegistry),
    "main/ocg/deepseek-v4",
  );
});

test("canonicalRef and alsoMatches honor server-emitted reference and also_matches (S2) when present", () => {
  const serverRow = {
    ...REGISTRY[0]!,
    reference: "prod/ocg/deepseek-v4",
    also_matches: ["Production/ocg/deepseek-v4", "ocg/deepseek-v4", "opencode-go/deepseek-v4"],
  };
  assert.equal(canonicalRef(serverRow), "prod/ocg/deepseek-v4");
  assert.deepEqual(alsoMatches(serverRow), [
    "Production/ocg/deepseek-v4",
    "ocg/deepseek-v4",
    "opencode-go/deepseek-v4",
  ]);
  // Explicit draft group override still takes precedence on the Models board.
  assert.equal(canonicalRef(serverRow, GROUPS, "g-main"), "main/ocg/deepseek-v4");
  assert.equal(canonicalRef(serverRow, GROUPS, "provider:opencode-go"), "opencode-go/deepseek-v4");
});
