/**
 * The refusal mapping is the only thing that keeps a suspended account from reading exactly
 * like one an operator switched off, so it is pinned here: each token keeps its own label and
 * tone, the severity order does not drift, and an absent/`NULL` field stays a plain disable.
 *
 * Run with `npm test` (Node's built-in runner over the TS sources; the UI has no other runner).
 */
import assert from "node:assert/strict";
import { test } from "node:test";
import { refusalView, reenableConsequence } from "../src/lib/refusal.ts";

test("each refusal token gets its own label and tone", () => {
  const views = ["account_suspended", "account_not_entitled", "client_version_rejected"].map(
    (token) => refusalView(token),
  );
  for (const view of views) assert.ok(view, "a known token must render a view");
  assert.deepEqual(
    views.map((view) => view?.label),
    ["suspended", "not entitled", "client outdated"],
  );
  assert.deepEqual(
    views.map((view) => view?.tone),
    ["neg", "warn", "info"],
  );
  // A badge nobody can tell apart defeats the purpose: labels and colours must both differ.
  assert.equal(new Set(views.map((view) => view?.label)).size, 3);
  assert.equal(new Set(views.map((view) => view?.tone)).size, 3);
});

test("severity runs suspended, not entitled, client version", () => {
  const severity = (token: string) => refusalView(token)?.severity ?? -1;
  assert.ok(severity("account_suspended") < severity("account_not_entitled"));
  assert.ok(severity("account_not_entitled") < severity("client_version_rejected"));
  assert.ok(severity("client_version_rejected") < severity("something_new"));
});

test("copy names the action the token actually needs", () => {
  const suspended = refusalView("account_suspended");
  assert.match(suspended?.description ?? "", /Terms of Service/);
  assert.match(suspended?.fix ?? "", /appeal/i);
  assert.match(suspended?.fix ?? "", /revoke RRouter's OAuth grant/);

  const notEntitled = refusalView("account_not_entitled");
  assert.match(notEntitled?.description ?? "", /client shape/);
  assert.match(notEntitled?.fix ?? "", /entitled/);

  const outdated = refusalView("client_version_rejected");
  assert.match(outdated?.fix ?? "", /Upgrade RRouter/);
  // The account is healthy: the wording must not ask an operator to touch it.
  assert.match(outdated?.description ?? "", /account itself is fine/);
});

test("every refusal carries the re-enable consequence", () => {
  for (const token of [
    "account_suspended",
    "account_not_entitled",
    "client_version_rejected",
    "something_new",
  ]) {
    const view = refusalView(token);
    assert.ok(view);
    assert.match(reenableConsequence(view) ?? "", /^Only re-enable after /);
    assert.match(reenableConsequence(view) ?? "", /stays refused/);
  }
});

test("an absent or empty field is a manual disable, not a refusal", () => {
  // Older payloads omit `refusal` entirely; a manual disable stores NULL.
  for (const value of [undefined, null, "", "   "]) {
    assert.equal(refusalView(value), null);
    assert.equal(reenableConsequence(refusalView(value)), null);
  }
});

test("an unnamed token still reads as a provider refusal", () => {
  const view = refusalView("account_quota_ban");
  assert.ok(view);
  assert.equal(view.label, "refused");
  assert.equal(view.tone, "neg");
  assert.match(view.description, /account_quota_ban/);
  assert.match(view.fix, /logs/);
});

test("tokens match exactly, ignoring surrounding whitespace", () => {
  assert.equal(refusalView(" account_suspended ")?.label, "suspended");
  // Case is not normalised: an unknown spelling is surfaced, never silently dropped as a
  // manual disable.
  assert.equal(refusalView("ACCOUNT_SUSPENDED")?.label, "refused");
});
