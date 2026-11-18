import type { Model } from "./api";

/**
 * The one place a model ref is resolved or built.
 *
 * The gateway accepts several spellings of the same model — a bare id, `provider/id`,
 * `code/id`, and the registry's own `group/code/id` ref — and a usage row stores the
 * string the client asked for, which may be any of them. Deriving a ref from that string
 * by hand (prefixing the provider, for instance) turns a registry ref into
 * `provider/group/code/id`, which the planner reads as provider plus a literal model id
 * and the upstream rejects. Two surfaces must never spell the same row differently, so
 * every ref comes from one of these two functions.
 */

/**
 * The registry row a model spelling names, or `undefined` when the registry does not list
 * it. Exact registry refs and bare ids win; otherwise the spelling is split at every
 * slash so both the provider id (`opencode-go/model`) and its shorthand (`ocg/model`)
 * resolve.
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
    const hit = models.find(
      (model) => (model.provider === head || model.code === head) && model.id === id,
    );
    if (hit) return hit;
  }
  return undefined;
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
