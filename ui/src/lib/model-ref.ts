import type { Model, UsageEvent } from "./api";

/**
 * The one place a model ref is resolved or built.
 *
 * The gateway accepts several spellings of the same model — a bare id, `provider/id`,
 * `code/id`, and the group-qualified `group/code/id` ref — and a usage row stores the
 * string the client asked for (`requested_model`) alongside the resolved routing record.
 * Every surface derives canonical refs, alternate spellings, and activity display fields
 * from these pure functions so two screens never spell the same row differently.
 */

/** Minimal model shape needed to build canonical refs and accepted spellings. */
export type ModelRefRow = Pick<Model, "provider" | "id"> & {
  code?: string;
  ref?: string;
  /** Server-emitted canonical ref from `/api/models` (S2); used when present. */
  reference?: string;
  /** Server-emitted alternate accepted spellings from `/api/models` (S2). */
  also_matches?: readonly string[];
  group?: string;
};

/** Minimal group shape; `slug` is used when S2 stable group ids are present. */
export type ModelRefGroup = {
  id: string;
  name: string;
  slug?: string;
  provider?: string;
};

/**
 * The registry row a model spelling names, or `undefined` when the registry does not list
 * it. Exact registry refs and bare ids win; otherwise the spelling is split at every
 * slash so `provider/id`, `code/id`, and 3-segment `group/code/id` or `group/provider/id`
 * refs all resolve — including upstream ids that contain slashes themselves.
 */
export function findModelRef(
  models: readonly Model[] | undefined,
  wanted: string,
): Model | undefined {
  const ref = wanted.trim();
  if (!ref || !models?.length) return undefined;
  const exact = models.find((model) => model.ref === ref) ?? models.find((model) => model.id === ref);
  if (exact) return exact;
  for (let at = ref.indexOf("/"); at > 0; at = ref.indexOf("/", at + 1)) {
    const head = ref.slice(0, at);
    const id = ref.slice(at + 1);
    const direct = models.find(
      (model) => (model.provider === head || model.code === head) && model.id === id,
    );
    if (direct) return direct;
    // A 3-segment `group/segment/id` spelling places the provider or code in the trailing
    // segment of `head` (e.g. `main/ocg` -> `ocg`).
    const lastSlash = head.lastIndexOf("/");
    if (lastSlash > 0) {
      const segment = head.slice(lastSlash + 1);
      const grouped = models.find(
        (model) => (model.provider === segment || model.code === segment) && model.id === id,
      );
      if (grouped) return grouped;
    }
  }
  return undefined;
}

/**
 * The ONE canonical ref for a model row:
 * - `rrouter` entries (aliases and combos): bare `id`.
 * - Registry model in a custom (free-form) group: `<group>/<code-or-provider>/<id>`
 *   (using the stable `group.slug` when present, otherwise `group.name`).
 * - When the server supplied `row.reference` (S2) and no local draft group overrides it,
 *   that canonical ref is returned directly.
 * - Ungrouped registry model (or in a `provider:*` bucket): `<provider>/<id>`.
 */
export function canonicalRef(
  row: ModelRefRow,
  groups?: readonly ModelRefGroup[],
  groupId?: string,
): string {
  const id = row.id.trim();
  const provider = row.provider.trim();
  if (!id) return provider;
  if (provider === "rrouter" || !provider) return id;

  const targetGroupId = groupId ?? row.group;
  const group = targetGroupId ? groups?.find((candidate) => candidate.id === targetGroupId) : undefined;
  if (group && !group.provider) {
    const prefix = (group.slug ?? group.name).trim();
    if (prefix) {
      const code = (row.code ?? "").trim() || provider;
      return `${prefix}/${code}/${id}`;
    }
  }

  // If no explicit draft group was resolved, honor the server-emitted `reference` (S2).
  if (!group && row.reference?.trim()) {
    return row.reference.trim();
  }

  return `${provider}/${id}`;
}

/**
 * Other spellings the gateway also accepts for this model row (`code/id`, `provider/id`,
 * `group/code/id`, and legacy `group.name/code/id` when a stable `slug` differs),
 * merging any server-emitted `row.also_matches` (S2), deduplicated and excluding the
 * canonical ref itself.
 */
export function alsoMatches(
  row: ModelRefRow,
  groups?: readonly ModelRefGroup[],
  groupId?: string,
): string[] {
  const id = row.id.trim();
  const provider = row.provider.trim();
  if (!id || !provider || provider === "rrouter") return [];

  const canonical = canonicalRef(row, groups, groupId);
  const code = (row.code ?? "").trim();
  const targetGroupId = groupId ?? row.group;
  const group = targetGroupId ? groups?.find((candidate) => candidate.id === targetGroupId) : undefined;

  const candidates: string[] = [];
  if (row.also_matches?.length) {
    for (const spelling of row.also_matches) {
      const trimmed = spelling.trim();
      if (trimmed) candidates.push(trimmed);
    }
  }
  if (group && !group.provider) {
    const slugPrefix = (group.slug ?? "").trim();
    const namePrefix = group.name.trim();
    const seg = code || provider;
    if (slugPrefix) candidates.push(`${slugPrefix}/${seg}/${id}`);
    if (namePrefix && namePrefix !== slugPrefix) {
      candidates.push(`${namePrefix}/${seg}/${id}`);
    }
  }
  if (code && code !== provider) {
    candidates.push(`${code}/${id}`);
  }
  candidates.push(`${provider}/${id}`);

  const seen = new Set<string>([canonical]);
  const out: string[] = [];
  for (const candidate of candidates) {
    if (!candidate || seen.has(candidate)) continue;
    seen.add(candidate);
    out.push(candidate);
  }
  return out;
}

/**
 * The literal model spelling the client sent (`requested_model`), falling back to `model`
 * when older rows did not record `requested_model`. Never rewritten through the registry.
 */
export function requestedRefOf(event: Pick<UsageEvent, "requested_model" | "model">): string {
  const literal = event.requested_model?.trim();
  if (literal) return literal;
  return event.model.trim();
}

/**
 * The actual `provider/model` destination served for a usage event, or `""` when no
 * provider was recorded (request failed before dispatch).
 */
export function routedTargetOf(
  event: Pick<UsageEvent, "provider" | "model">,
  models?: readonly Model[],
): string {
  const serving = event.provider.trim();
  const raw = event.model.trim();
  if (!serving) return "";
  if (!raw) return serving;

  const exact = models?.find((entry) => entry.provider === serving && entry.id === raw);
  if (exact) return `${serving}/${exact.id}`;

  const matched = findModelRef(models, raw);
  if (matched && matched.provider === serving) {
    return `${serving}/${matched.id}`;
  }

  if (raw.startsWith(`${serving}/`)) return raw;
  return `${serving}/${raw}`;
}

/**
 * Canonical ref for an activity event: the registry's `ref` when the served `(provider, id)`
 * is listed, otherwise the resolved `provider/model` target.
 */
export function canonicalEventRef(
  event: Pick<UsageEvent, "provider" | "model">,
  models?: readonly Model[],
): string {
  const serving = event.provider.trim();
  const raw = event.model.trim();
  if (!serving) return "";

  const exact = models?.find((entry) => entry.provider === serving && entry.id === raw);
  if (exact) return exact.ref;

  const matched = findModelRef(models, raw);
  if (matched && matched.provider === serving) return matched.ref;

  return routedTargetOf(event, models);
}

/**
 * Returns the canonical ref when a request's literal spelling differs from it (so the UI
 * can show a neutral `same as <canonical>` note), or `null` when they already match or
 * the destination is unknown.
 */
export function sameAsRef(
  event: Pick<UsageEvent, "provider" | "model" | "requested_model">,
  models?: readonly Model[],
): string | null {
  const requested = requestedRefOf(event);
  const canonical = canonicalEventRef(event, models);
  if (!requested || !canonical || requested === canonical) return null;
  return canonical;
}

/**
 * The canonical ref for a `(provider, model)` pair: the ref the models listing advertises
 * when the registry knows the pair, otherwise the recorded spelling itself. A provider
 * prefix is only added to a bare id, so a namespaced spelling — `group/code/id`,
 * `code/id`, a combo name, or an upstream id carrying its own slash — is never prefixed
 * twice. `models` may be omitted while the registry is loading; the result is then
 * structural rather than registry-canonical.
 */
export function modelRefFor(
  provider: string,
  model: string,
  models?: readonly Model[],
): string {
  const id = model.trim();
  const serving = provider.trim();
  if (!id) return serving;
  const listed = models?.find((entry) => entry.provider === serving && entry.id === id);
  if (listed) return listed.ref;
  // `rrouter`-owned entries are callables, not provider rows: an alias or a combo name
  // must be used as written, whatever provider served the original request.
  const spelling = findModelRef(models, id);
  if (spelling && (!serving || spelling.provider === serving || spelling.provider === "rrouter")) {
    return spelling.ref;
  }
  if (id.startsWith("combo:") || id.includes("/") || !serving) return id;
  return `${serving}/${id}`;
}
