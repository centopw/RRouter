/**
 * Thin API client for Free mode (`/api/free-mode*`).
 *
 * Matches the frozen backend contract:
 *   - `GET /api/free-mode[?refresh=true]`
 *   - `PATCH /api/free-mode`
 *   - `PATCH /api/free-mode/models`
 *   - `POST /api/free-mode/probe`
 *   - Advanced (backwards-compatible): `POST /api/free-mode/import`,
 *     `POST /api/free-mode/discover`, `DELETE /api/free-mode[?disable=true]`
 *
 * Pure data shapes and parsers live in `../pages/free-mode/model.ts` so the test runner
 * can exercise them without network or DOM imports.
 */
import { ApiError, api } from "./api";
import {
  parseDiscoverResult,
  parseFreeModeState,
  parseModelToggleResult,
  parseProbeAllResult,
  parseProbeSingleResult,
  type FreeAccount,
  type FreeCatalogStatus,
  type FreeDiscoverError,
  type FreeDiscoverProvider,
  type FreeDiscoverResult,
  type FreeImportResult,
  type FreeModePatch,
  type FreeModeState,
  type FreeModeSummary,
  type FreeModel,
  type FreeModelAuthMode,
  type FreeModelProbe,
  type FreeModelToggleResult,
  type FreeProbeAllResult,
  type FreeProbeSingleResult,
} from "../pages/free-mode/model";

export type {
  FreeAccount,
  FreeCatalogStatus,
  FreeDiscoverError,
  FreeDiscoverProvider,
  FreeDiscoverResult,
  FreeImportResult,
  FreeModePatch,
  FreeModeState,
  FreeModeSummary,
  FreeModel,
  FreeModelAuthMode,
  FreeModelProbe,
  FreeModelToggleResult,
  FreeProbeAllResult,
  FreeProbeSingleResult,
};

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function countOf(value: unknown): number | null {
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

// ---------------------------------------------------------------- API

/** Zero-arg so `useQuery({ queryKey: qk.freeMode, queryFn: getFreeMode })` stays type-safe. */
export async function getFreeMode(): Promise<FreeModeState> {
  const body = await api.get<unknown>("/api/free-mode");
  return parseFreeModeState(body);
}

/** Force-refresh public free-model catalogs without running live probes. */
export async function refreshFreeMode(): Promise<FreeModeState> {
  const body = await api.get<unknown>("/api/free-mode?refresh=true");
  return parseFreeModeState(body);
}

/** Toggle the free pool and/or automatic quota fallback; returns the full updated state. */
export async function patchFreeMode(patch: FreeModePatch): Promise<FreeModeState> {
  const body = await api.patch<unknown>("/api/free-mode", patch);
  return parseFreeModeState(body);
}

/** Enable or disable one discovered free model (JSON body handles ids containing `/`). */
export async function patchFreeModeModel(input: {
  provider: string;
  model: string;
  enabled: boolean;
}): Promise<FreeModelToggleResult> {
  const body = await api.patch<unknown>("/api/free-mode/models", input);
  return parseModelToggleResult(body);
}

/** Run one live probe against a single free model. */
export async function probeFreeModel(input: {
  provider: string;
  model: string;
}): Promise<FreeProbeSingleResult> {
  const body = await api.post<unknown>("/api/free-mode/probe", input);
  return parseProbeSingleResult(body);
}

/** Probe every enabled usable free model (optionally re-fetching public catalogs first). */
export async function probeAllFreeModels(opts?: {
  refreshCatalogs?: boolean;
}): Promise<FreeProbeAllResult> {
  const body = await api.post<unknown>("/api/free-mode/probe", {
    all: true,
    ...(opts?.refreshCatalogs ? { refresh_catalogs: true } : {}),
  });
  return parseProbeAllResult(body);
}

/** Advanced: import a JSON document of free-tier accounts and proxies. */
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

/** Advanced: run discovery across public catalogs and imported accounts. */
export function discoverFreeMode(providers: string[]): Promise<FreeDiscoverResult> {
  return api
    .post<unknown>("/api/free-mode/discover", providers.length === 0 ? {} : { providers })
    .then(parseDiscoverResult);
}

/** Advanced: clear imported document and account-discovered rows; credentials stay. */
export function teardownFreeMode(disableAccounts: boolean): Promise<unknown> {
  return api.del<unknown>(`/api/free-mode${disableAccounts ? "?disable=true" : ""}`);
}

/**
 * Every problem the gateway named, so Advanced validation lists them instead of collapsing
 * them into one sentence.
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

/** Provider keys present in imported accounts, in account order. */
export function providersOf(accounts: FreeAccount[]): string[] {
  const seen: string[] = [];
  for (const account of accounts) {
    const provider = account.provider.trim();
    if (provider !== "" && !seen.includes(provider)) seen.push(provider);
  }
  return seen;
}
