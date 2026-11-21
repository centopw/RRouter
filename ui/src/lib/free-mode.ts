/**
 * Free mode: one imported document declares free-tier accounts with their own egress
 * proxies, and the gateway spreads the reserved ref `free` across them.
 *
 * Everything the page shows comes from `GET /api/free-mode`; the document in the editor is
 * the operator's draft until Import writes it. Nothing here echoes credential material: the
 * server masks proxies (`scheme://***@host:port`) and never returns a key.
 *
 * The document's own shape and local checks live in `free-mode-document.ts`, which has no
 * network imports so the test runner can load it.
 */
import { ApiError, api } from "./api";
import type { FreeRouting } from "./free-mode-document";

/** One imported account. `proxy` is the masked form; the raw URL never leaves the server. */
export type FreeAccount = {
  connection_id: string;
  provider: string;
  label: string | null;
  proxy: string | null;
  free_rpm: number | null;
  is_active: boolean;
  last_error: string | null;
};

/** One model discovery classified. `source` is how "free" was decided. */
export type FreeModel = {
  provider: string;
  model: string;
  free: boolean;
  /** Registry price in USD per million tokens; `null` when no catalogue covers the model. */
  price: number | null;
  /** `document`, `provider-list` or `registry`. */
  source: string;
  discovered_at: number | null;
};

/** `GET /api/free-mode`: the stored document, its knobs, and what discovery found. */
export type FreeModeState = {
  document: Record<string, unknown> | null;
  routing: Partial<FreeRouting> | null;
  updated_at: number | null;
  accounts: FreeAccount[];
  models: FreeModel[];
  discovered_at: number | null;
};

/** `POST /api/free-mode/import`: counts only, never a credential. */
export type FreeImportResult = {
  ok: boolean;
  /** `null` when the answer omitted the count: the page shows an em dash, never a guess. */
  providers: number | null;
  accounts: { created: number | null; updated: number | null };
  disabled: number | null;
};

/** One provider's slice of a discovery run. The server may report only the requested ones. */
export type FreeDiscoverProvider = {
  provider: string;
  /** Models the provider listed, when the run counted them. */
  models: number | null;
  /** Of those, how many classify as free. */
  free: number | null;
  accounts: number | null;
};

/** One account that failed during discovery; the run still stores every other account. */
export type FreeDiscoverError = { provider: string; label: string | null; error: string };

export type FreeDiscoverResult = {
  providers: FreeDiscoverProvider[];
  errors: FreeDiscoverError[];
};

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function countOf(value: unknown): number | null {
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

function textOf(value: unknown): string | null {
  return typeof value === "string" && value.trim() !== "" ? value : null;
}

// ---------------------------------------------------------------- API

export async function getFreeMode(): Promise<FreeModeState> {
  const body = await api.get<Partial<FreeModeState>>("/api/free-mode");
  return {
    document: isObject(body.document) ? body.document : null,
    routing: isObject(body.routing) ? (body.routing as Partial<FreeRouting>) : null,
    updated_at: countOf(body.updated_at),
    accounts: (body.accounts ?? []).map((row) => ({
      connection_id: String(row.connection_id ?? ""),
      provider: String(row.provider ?? ""),
      label: row.label ?? null,
      proxy: row.proxy ?? null,
      free_rpm: countOf(row.free_rpm),
      // Absent is not active: an older build that omits the flag must not be read as `true`.
      is_active: row.is_active === true,
      last_error: row.last_error ?? null,
    })),
    models: (body.models ?? []).map((row) => ({
      provider: String(row.provider ?? ""),
      model: String(row.model ?? ""),
      free: row.free === true,
      price: countOf(row.price),
      source: String(row.source ?? ""),
      discovered_at: countOf(row.discovered_at),
    })),
    discovered_at: countOf(body.discovered_at),
  };
}

/** The document is the body; the gateway validates all of it before it writes anything. */
export async function importFreeMode(document: unknown): Promise<FreeImportResult> {
  const body = await api.post<Partial<FreeImportResult> & { accounts?: Record<string, unknown> }>(
    "/api/free-mode/import",
    document,
  );
  const accounts: Record<string, unknown> = isObject(body.accounts) ? body.accounts : {};
  return {
    ok: body.ok === true,
    providers: countOf(body.providers),
    accounts: { created: countOf(accounts.created), updated: countOf(accounts.updated) },
    disabled: countOf(body.disabled),
  };
}

/** An empty list means "every imported provider", which the server spells as `{}`. */
export function discoverFreeMode(providers: string[]): Promise<FreeDiscoverResult> {
  return api
    .post<Record<string, unknown>>(
      "/api/free-mode/discover",
      providers.length === 0 ? {} : { providers },
    )
    .then(readDiscoverResult);
}

/** Teardown clears the document, the knobs and the discovered rows; credentials stay. */
export function teardownFreeMode(disableAccounts: boolean): Promise<unknown> {
  return api.del<unknown>(`/api/free-mode${disableAccounts ? "?disable=true" : ""}`);
}

/**
 * The discovery answer is a summary, not the catalogue: the page always re-reads
 * `GET /api/free-mode` for the models it lists. This reads only what the run reported.
 */
function readDiscoverResult(body: Record<string, unknown>): FreeDiscoverResult {
  const providers = Array.isArray(body.providers) ? body.providers : [];
  const errors = Array.isArray(body.errors) ? body.errors : [];
  return {
    providers: providers.flatMap((entry) => {
      if (!isObject(entry)) return [];
      const provider = textOf(entry.provider);
      if (provider === null) return [];
      return [
        {
          provider,
          models: countOf(entry.models) ?? countOf(entry.count),
          free: countOf(entry.free),
          accounts: countOf(entry.accounts),
        },
      ];
    }),
    errors: errors.flatMap((entry) => {
      if (!isObject(entry)) return [];
      const message = textOf(entry.error) ?? textOf(entry.message);
      if (message === null) return [];
      return [
        {
          provider: textOf(entry.provider) ?? "",
          label: textOf(entry.label) ?? textOf(entry.account),
          error: message,
        },
      ];
    }),
  };
}

/**
 * Every problem the gateway named, so the page can list them instead of collapsing them
 * into one sentence. A failure that is not an `ApiError` still reads as one line.
 */
export function freeModeProblems(error: unknown): string[] {
  if (!(error instanceof ApiError)) {
    return [error instanceof Error ? error.message : String(error)];
  }
  const body = error.body;
  const failure = isObject(body) && isObject(body.error) ? body.error : null;
  const details = failure && Array.isArray(failure.details) ? failure.details : [];
  const named = details.filter((detail): detail is string => typeof detail === "string");
  return named.length > 0 ? named : [error.message];
}

/** The provider keys a discovery run can be narrowed to, in the order accounts list them. */
export function providersOf(accounts: FreeAccount[]): string[] {
  const seen: string[] = [];
  for (const account of accounts) {
    const provider = account.provider.trim();
    if (provider !== "" && !seen.includes(provider)) seen.push(provider);
  }
  return seen;
}

/**
 * The sample account+model the "call it like this" hints spell their refs with. A picked
 * model wins; otherwise the first free model discovery recorded. `null` when nothing has
 * been discovered, so the hints can show `<placeholders>` rather than an invented id.
 */
export function refSample(
  models: FreeModel[],
  picked: string[],
): { provider: string; model: string } | null {
  for (const id of picked) {
    const match = models.find((row) => row.free && row.model === id);
    if (match) return { provider: match.provider, model: match.model };
  }
  const first = models.find((row) => row.free);
  return first ? { provider: first.provider, model: first.model } : null;
}
