/**
 * The gateway's name normaliser, mirrored in the UI so a form can preview the name it
 * will store. These cases are the ones `crates/rrouter-store/src/names.rs` pins on the
 * server side; a divergence here is a form promising a name the API will not keep.
 */
import assert from "node:assert/strict";
import { test } from "node:test";
import {
  MAX_CANONICAL_NAME,
  canonicalName,
  canonicalSlug,
  normalisedPreview,
} from "../src/lib/model-name.ts";

test("lower-cases and collapses runs of separators to one dash", () => {
  assert.equal(canonicalName("  Main Group  "), "main-group");
  assert.equal(canonicalName("main___group"), "main-group");
  assert.equal(canonicalName("main - - group"), "main-group");
  assert.equal(canonicalName("--main--"), "main");
  assert.equal(canonicalName("OtherCombo"), "othercombo");
});

test("keeps version dots and colons, drops characters a path segment cannot hold", () => {
  assert.equal(canonicalName("DeepSeek-V4.1-Flash-1017"), "deepseek-v4.1-flash-1017");
  assert.equal(canonicalName("combo:Main"), "combo:main");
  assert.equal(canonicalName("a?b#c%d"), "a-b-c-d");
  assert.equal(canonicalName("a/b"), "a-b");
});

test("a slash survives only between real parts", () => {
  assert.equal(canonicalSlug("Main / OAI // DeepSeek-V4.1"), "main/oai/deepseek-v4.1");
  assert.equal(canonicalSlug("main/"), "main");
  assert.equal(canonicalSlug("/main"), "main");
  assert.equal(canonicalSlug("main//x/"), "main/x");
  assert.equal(canonicalSlug("  "), "");
});

test("caps the length without leaving a trailing separator", () => {
  const long = "x".repeat(MAX_CANONICAL_NAME + 50);
  assert.equal(canonicalSlug(long).length, MAX_CANONICAL_NAME);
  const deep = `${"a".repeat(150)}/${"b".repeat(150)}`;
  const capped = canonicalSlug(deep);
  assert.ok(capped.length <= MAX_CANONICAL_NAME);
  assert.ok(capped.startsWith("a") && !capped.endsWith("/"));
});

test("a form previews only a name that changes on save", () => {
  assert.equal(normalisedPreview("OtherCombo"), "othercombo");
  assert.equal(normalisedPreview("  Main  Combo "), "main-combo");
  assert.equal(normalisedPreview("main-combo"), null, "already canonical");
  assert.equal(normalisedPreview("   "), null);
  assert.equal(normalisedPreview("///"), null, "nothing left to store");
});
