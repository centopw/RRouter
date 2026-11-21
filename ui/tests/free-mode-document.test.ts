/**
 * The document rules the dashboard checks before it sends anything. They must stay a subset
 * of the gateway's own validation (which stays authoritative), so the cases pinned here are
 * the ones the page would otherwise let through to a 400: an unknown proxy key, a duplicate
 * account label, a `routing.models` that is not a list of ids.
 *
 * Run with `npm test` (Node's built-in runner over the TS sources).
 */
import assert from "node:assert/strict";
import { test } from "node:test";
import {
  FREE_DOCUMENT_EXAMPLE,
  freeRefHints,
  routingOf,
  validateFreeDocument,
  withRouting,
} from "../src/lib/free-mode-document.ts";

test("the shipped example validates with no problems", () => {
  const check = validateFreeDocument(FREE_DOCUMENT_EXAMPLE);
  assert.deepEqual(check.problems, []);
  assert.equal(check.providers, 1);
  assert.equal(check.accounts, 2);
  // The example has to show the model picker's shape, or it teaches the wrong document.
  const routing = (check.document?.routing ?? {}) as { models?: unknown };
  assert.deepEqual(routing.models, ["deepseek-v4.1-flash"]);
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
  assert.deepEqual(check.problems.map((problem) => problem.path), ["document"]);
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

test("the document's routing block wins over the stored knobs, models included", () => {
  const knobs = routingOf(
    { routing: { enabled: false, prefer: "cheapest", models: ["m-1", "", 7] } },
    { enabled: true, max_attempts: 4, per_account_rpm: 9, prefer: "spread", models: ["stored"] },
  );
  assert.equal(knobs.enabled, false);
  assert.equal(knobs.prefer, "cheapest");
  // Stored knobs fill only what the document leaves out.
  assert.equal(knobs.max_attempts, 4);
  assert.equal(knobs.per_account_rpm, 9);
  // Junk is dropped rather than shown as a picked id.
  assert.deepEqual(knobs.models, ["m-1"]);
});

test("withRouting rewrites the block and keeps every other key", () => {
  const document = { version: 1, providers: [], routing: { custom: "kept" } };
  const next = withRouting(document, {
    enabled: true,
    per_account_rpm: 5,
    max_attempts: 3,
    prefer: "spread",
    models: ["m-1"],
  });
  assert.equal(next.version, 1);
  const routing = next.routing as Record<string, unknown>;
  assert.equal(routing.custom, "kept");
  assert.deepEqual(routing.models, ["m-1"]);
  assert.equal(routing.max_attempts, 3);
});

test("ref hints use a real id when one is known and placeholders otherwise", () => {
  assert.deepEqual(
    freeRefHints({ provider: "openrouter", model: "deepseek-v4.1-flash" }).map((hint) => hint.ref),
    [
      "free",
      "free:openrouter",
      "free/deepseek-v4.1-flash",
      "free:openrouter/deepseek-v4.1-flash",
    ],
  );
  assert.deepEqual(
    freeRefHints(null).map((hint) => hint.ref),
    ["free", "free:<provider>", "free/<model-id>", "free:<provider>/<model-id>"],
  );
});
