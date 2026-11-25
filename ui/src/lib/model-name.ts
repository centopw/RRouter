/**
 * The server's name normaliser, mirrored so a form can preview the name it will store.
 *
 * The gateway canonicalises every combo, alias, preset group and custom model name on
 * write (`rrouter-store`'s `canonical_name` / `canonical_ref`); showing the result while
 * the operator types is what stops a name being silently rewritten after save. Keep this
 * in step with `crates/rrouter-store/src/names.rs` — the tests pin the rules both sides
 * must agree on.
 */

/** Longest canonical name the server stores. */
export const MAX_CANONICAL_NAME = 200;

/** One path-segment ref normalised: lower-case, `-` between words, path-hostile characters dropped. */
export function canonicalName(raw: string): string {
  let out = "";
  let pendingDash = false;
  for (const ch of raw.trim()) {
    if (ch.trim() === "" || ch === "_" || ch === "\u3000") {
      pendingDash = true;
      continue;
    }
    if (ch === "/") {
      pendingDash = true;
      continue;
    }
    const keep = /[\p{L}\p{N}.:-]/u.test(ch);
    if (!keep) {
      pendingDash = true;
      continue;
    }
    if (ch === "-") {
      pendingDash = true;
      continue;
    }
    if (pendingDash && out !== "" && !out.endsWith("-") && !out.endsWith("/")) {
      out += "-";
    }
    pendingDash = false;
    out += ch.toLowerCase();
  }
  if (out.length > MAX_CANONICAL_NAME) out = out.slice(0, MAX_CANONICAL_NAME);
  // Trimming characters can expose a trailing separator, so trim until stable.
  let trimmed = out.replace(/^[-/.]+|[-/.]+$/g, "");
  while (trimmed !== out) {
    out = trimmed;
    trimmed = out.replace(/^[-/.]+|[-/.]+$/g, "");
  }
  return trimmed;
}

/**
 * Splits a ref on `/`, normalises each segment, drops empty ones and joins with one `/`:
 * `Main / OAI // DeepSeek-V4.1` becomes `main/oai/deepseek-v4.1`.
 */
export function canonicalSlug(raw: string): string {
  const parts = raw
    .split("/")
    .map(canonicalName)
    .filter((part) => part !== "");
  let joined = parts.join("/");
  while (joined.length > MAX_CANONICAL_NAME && parts.length > 0) {
    const last = parts[parts.length - 1]!;
    if (last.length > 1) {
      parts[parts.length - 1] = last.slice(0, -1);
    } else {
      parts.pop();
    }
    joined = parts.join("/");
  }
  return joined;
}

/**
 * The hint under a name field: the canonical name a save will store, or `null` when the
 * typed name is already canonical (nothing to preview) or normalises to nothing.
 */
export function normalisedPreview(raw: string): string | null {
  const typed = raw.trim();
  if (typed === "") return null;
  const canonical = canonicalSlug(typed);
  if (canonical === "" || canonical === typed) return null;
  return canonical;
}
