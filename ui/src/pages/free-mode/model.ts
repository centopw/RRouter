/**
 * Pure data shapes, defensive wire parsers, cache reducers, and derived copy for the
 * redesigned Free mode page.
 *
 * Kept free of React and network imports so `node --test` can load it directly and pin
 * every contract field, status classification, fallback preview, and sort/filter rule.
 */
import type { FreeRouting } from "../../lib/free-mode-document.ts";

export type FreeModelAuthMode = "key_or_anonymous" | "key_required" | "anonymous";

/** Live probe outcome for one free model (`POST /api/free-mode/probe` or `GET /api/free-mode`). */
export type FreeModelProbe = {
  ok: boolean;
  status: number;
  latency_ms: number;
  message: string | null;
  needs_key: boolean;
  /** Unix seconds. */
  at: number;
};

/** One discovered free model from public catalogs or an imported free-mode document. */
export type FreeModel = {
  provider: string;
  provider_display: string;
  model: string;
  display: string;
  ref: string;
  endpoint: string | null;
  dialect: string | null;
  free: boolean;
  enabled: boolean;
  usable: boolean;
  unusable_reason: string | null;
  auth_mode: FreeModelAuthMode;
  has_account: boolean;
  needs_key: boolean;
  price: number | null;
  source: string;
  discovered_at: number | null;
  probe: FreeModelProbe | null;
};

/** Server-computed pool counts; derived from `models` only if an older build omits `summary`. */
export type FreeModeSummary = {
  total: number;
  usable: number;
  enabled: number;
  working: number;
  unusable: number;
};

/** Sync state of one public free-model catalog (`opencode`, `openrouter`, …). */
export type FreeCatalogStatus = {
  provider: string;
  url: string;
  discovered_at: number | null;
  models: number;
  usable: number;
  error: string | null;
};

/** One imported account (Advanced). `proxy` is masked by the server; raw keys never leave. */
export type FreeAccount = {
  connection_id: string;
  provider: string;
  label: string | null;
  proxy: string | null;
  free_rpm: number | null;
  is_active: boolean;
  last_error: string | null;
};

/** Complete `GET /api/free-mode` (and `PATCH /api/free-mode`) response. */
export type FreeModeState = {
  enabled: boolean;
  fallback_on_quota: boolean;
  fallback_triggers: string[];
  ref: string;
  last_checked_at: number | null;
  discovered_at: number | null;
  summary: FreeModeSummary;
  next_fallback_model: string | null;
  models: FreeModel[];
  catalogs: FreeCatalogStatus[];
  document: Record<string, unknown> | null;
  routing: Partial<FreeRouting> | null;
  updated_at: number | null;
  accounts: FreeAccount[];
};

/** Request body for `PATCH /api/free-mode`. */
export type FreeModePatch = {
  enabled?: boolean;
  fallback_on_quota?: boolean;
  probe_on_enable?: boolean;
};

/** Response body for `PATCH /api/free-mode/models`. */
export type FreeModelToggleResult = {
  ok: boolean;
  provider: string;
  model: string;
  enabled: boolean;
  summary: FreeModeSummary;
  next_fallback_model: string | null;
};

/** Single-model response from `POST /api/free-mode/probe`. */
export type FreeProbeSingleResult = {
  provider: string;
  model: string;
  endpoint: string | null;
  probe: FreeModelProbe;
};

/** All-models response from `POST /api/free-mode/probe` with `{ all: true }`. */
export type FreeProbeAllResult = {
  ok: boolean;
  checked_at: number | null;
  results: FreeProbeSingleResult[];
};

/** `POST /api/free-mode/import`: counts only, never a credential. */
export type FreeImportResult = {
  ok: boolean;
  providers: number | null;
  accounts: { created: number | null; updated: number | null };
  disabled: number | null;
};

/** One provider's slice of an Advanced discovery run. */
export type FreeDiscoverProvider = {
  provider: string;
  models: number | null;
  free: number | null;
  accounts: number | null;
};

/** One account that failed during an Advanced discovery run. */
export type FreeDiscoverError = { provider: string; label: string | null; error: string };

export type FreeDiscoverResult = {
  providers: FreeDiscoverProvider[];
  errors: FreeDiscoverError[];
};

// ---------------------------------------------------------------- wire parsers

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function numOf(value: unknown): number | null {
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

function textOf(value: unknown): string | null {
  return typeof value === "string" && value.trim() !== "" ? value.trim() : null;
}

function authModeOf(value: unknown): FreeModelAuthMode {
  if (value === "key_or_anonymous" || value === "key_required" || value === "anonymous") {
    return value;
  }
  return "key_required";
}

function parseProbe(value: unknown): FreeModelProbe | null {
  if (!isObject(value)) return null;
  return {
    ok: value.ok === true,
    status: numOf(value.status) ?? 0,
    latency_ms: numOf(value.latency_ms) ?? 0,
    message: textOf(value.message),
    needs_key: value.needs_key === true,
    at: numOf(value.at) ?? 0,
  };
}

function parseModel(value: unknown): FreeModel | null {
  if (!isObject(value)) return null;
  const provider = textOf(value.provider) ?? "";
  const model = textOf(value.model) ?? "";
  if (provider === "" || model === "") return null;
  const usable = value.usable !== false;
  const probe = parseProbe(value.probe);
  const authMode = authModeOf(value.auth_mode);
  const hasAccount = value.has_account === true;
  return {
    provider,
    provider_display: textOf(value.provider_display) ?? provider,
    model,
    display: textOf(value.display) ?? model,
    ref: textOf(value.ref) ?? `free/${model}`,
    endpoint: textOf(value.endpoint),
    dialect: textOf(value.dialect),
    free: value.free !== false,
    enabled: usable && value.enabled !== false,
    usable,
    unusable_reason: textOf(value.unusable_reason),
    auth_mode: authMode,
    has_account: hasAccount,
    needs_key: value.needs_key === true || (authMode === "key_required" && !hasAccount),
    price: numOf(value.price),
    source: textOf(value.source) ?? "",
    discovered_at: numOf(value.discovered_at),
    probe,
  };
}

/** Recompute summary counts from the current model list (used as fallback or after local probe merge). */
export function summarizeModels(models: readonly FreeModel[]): FreeModeSummary {
  const freeOnly = models.filter((row) => row.free);
  let usable = 0;
  let enabled = 0;
  let working = 0;
  let unusable = 0;
  for (const row of freeOnly) {
    if (!row.usable) {
      unusable += 1;
      continue;
    }
    usable += 1;
    if (row.enabled) {
      enabled += 1;
      if (row.probe?.ok) working += 1;
    }
  }
  return { total: freeOnly.length, usable, enabled, working, unusable };
}

function parseSummary(value: unknown, fallbackModels: FreeModel[]): FreeModeSummary {
  if (!isObject(value)) return summarizeModels(fallbackModels);
  const derived = summarizeModels(fallbackModels);
  return {
    total: numOf(value.total) ?? derived.total,
    usable: numOf(value.usable) ?? derived.usable,
    enabled: numOf(value.enabled) ?? derived.enabled,
    working: numOf(value.working) ?? derived.working,
    unusable: numOf(value.unusable) ?? derived.unusable,
  };
}

function parseCatalog(value: unknown): FreeCatalogStatus | null {
  if (!isObject(value)) return null;
  const provider = textOf(value.provider);
  if (provider === null) return null;
  return {
    provider,
    url: textOf(value.url) ?? "",
    discovered_at: numOf(value.discovered_at),
    models: numOf(value.models) ?? 0,
    usable: numOf(value.usable) ?? 0,
    error: textOf(value.error),
  };
}

function parseAccount(value: unknown): FreeAccount | null {
  if (!isObject(value)) return null;
  return {
    connection_id: typeof value.connection_id === "string" ? value.connection_id : "",
    provider: typeof value.provider === "string" ? value.provider : "",
    label: textOf(value.label),
    proxy: textOf(value.proxy),
    free_rpm: numOf(value.free_rpm),
    is_active: value.is_active === true,
    last_error: textOf(value.last_error),
  };
}

export function parseFreeModeState(body: unknown): FreeModeState {
  const raw = isObject(body) ? body : {};
  const routing = isObject(raw.routing) ? (raw.routing as Partial<FreeRouting>) : null;
  const models = Array.isArray(raw.models)
    ? raw.models.map(parseModel).filter((row): row is FreeModel => row !== null)
    : [];
  const catalogs = Array.isArray(raw.catalogs)
    ? raw.catalogs.map(parseCatalog).filter((row): row is FreeCatalogStatus => row !== null)
    : [];
  const accounts = Array.isArray(raw.accounts)
    ? raw.accounts.map(parseAccount).filter((row): row is FreeAccount => row !== null)
    : [];
  const triggers = Array.isArray(raw.fallback_triggers)
    ? raw.fallback_triggers.filter((item): item is string => typeof item === "string" && item.trim() !== "")
    : ["quota", "429", "402"];

  return {
    enabled: typeof raw.enabled === "boolean" ? raw.enabled : routing?.enabled === true,
    fallback_on_quota:
      typeof raw.fallback_on_quota === "boolean"
        ? raw.fallback_on_quota
        : routing?.fallback_on_quota === true,
    fallback_triggers: triggers,
    ref: textOf(raw.ref) ?? "free",
    last_checked_at: numOf(raw.last_checked_at),
    discovered_at: numOf(raw.discovered_at),
    summary: parseSummary(raw.summary, models),
    next_fallback_model: textOf(raw.next_fallback_model),
    models,
    catalogs,
    document: isObject(raw.document) ? raw.document : null,
    routing,
    updated_at: numOf(raw.updated_at),
    accounts,
  };
}

export function parseModelToggleResult(body: unknown): FreeModelToggleResult {
  const raw = isObject(body) ? body : {};
  return {
    ok: raw.ok === true,
    provider: textOf(raw.provider) ?? "",
    model: textOf(raw.model) ?? "",
    enabled: raw.enabled === true,
    summary: parseSummary(raw.summary, []),
    next_fallback_model: textOf(raw.next_fallback_model),
  };
}

export function parseProbeSingleResult(body: unknown): FreeProbeSingleResult {
  const raw = isObject(body) ? body : {};
  return {
    provider: textOf(raw.provider) ?? "",
    model: textOf(raw.model) ?? "",
    endpoint: textOf(raw.endpoint),
    probe: parseProbe(raw.probe) ?? {
      ok: false,
      status: 0,
      latency_ms: 0,
      message: "empty probe response",
      needs_key: false,
      at: 0,
    },
  };
}

export function parseProbeAllResult(body: unknown): FreeProbeAllResult {
  const raw = isObject(body) ? body : {};
  const results = Array.isArray(raw.results) ? raw.results.map(parseProbeSingleResult) : [];
  return {
    ok: raw.ok === true,
    checked_at: numOf(raw.checked_at),
    results,
  };
}

export function parseDiscoverResult(body: unknown): FreeDiscoverResult {
  const raw = isObject(body) ? body : {};
  const providers = Array.isArray(raw.providers) ? raw.providers : [];
  const errors = Array.isArray(raw.errors) ? raw.errors : [];
  return {
    providers: providers.flatMap((entry) => {
      if (!isObject(entry)) return [];
      const provider = textOf(entry.provider);
      if (provider === null) return [];
      return [
        {
          provider,
          models: numOf(entry.models) ?? numOf(entry.count),
          free: numOf(entry.free),
          accounts: numOf(entry.accounts),
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

// ---------------------------------------------------------------- state reducers

/** Stable identity key for one discovered model row. */
export function modelKey(model: Pick<FreeModel, "provider" | "model">): string {
  return `${model.provider}/${model.model}`;
}

/** Pick the next fallback model from the current model list when updating cache locally. */
export function deriveNextFallbackModel(models: readonly FreeModel[]): string | null {
  const working = models
    .filter((row) => row.free && row.usable && row.enabled && row.probe?.ok)
    .sort((a, b) => (a.probe?.latency_ms ?? Infinity) - (b.probe?.latency_ms ?? Infinity));
  if (working.length > 0) return working[0]!.model;
  const ready = models.find(
    (row) =>
      row.free &&
      row.usable &&
      row.enabled &&
      (row.has_account || row.auth_mode !== "key_required"),
  );
  return ready ? ready.model : null;
}

/** Merge a `PATCH /api/free-mode/models` response into cached `FreeModeState`. */
export function applyToggleToState(
  state: FreeModeState,
  result: FreeModelToggleResult,
): FreeModeState {
  const target = modelKey(result);
  const models = state.models.map((row) =>
    modelKey(row) === target ? { ...row, enabled: result.enabled } : row,
  );
  return {
    ...state,
    models,
    summary: result.summary.total > 0 ? result.summary : summarizeModels(models),
    next_fallback_model: result.next_fallback_model,
  };
}

/** Merge a single-model `POST /api/free-mode/probe` response into cached `FreeModeState`. */
export function applyProbeSingleToState(
  state: FreeModeState,
  result: FreeProbeSingleResult,
): FreeModeState {
  const target = modelKey(result);
  const models = state.models.map((row) => {
    if (modelKey(row) !== target) return row;
    const needsKey = result.probe.ok
      ? false
      : result.probe.needs_key || (row.auth_mode === "key_required" && !row.has_account);
    return {
      ...row,
      endpoint: result.endpoint ?? row.endpoint,
      probe: result.probe,
      needs_key: needsKey,
    };
  });
  const at = result.probe.at > 0 ? result.probe.at : state.last_checked_at;
  return {
    ...state,
    models,
    summary: summarizeModels(models),
    next_fallback_model: deriveNextFallbackModel(models),
    last_checked_at:
      at !== null && (state.last_checked_at === null || at >= state.last_checked_at)
        ? at
        : state.last_checked_at,
  };
}

/** Merge an all-models `POST /api/free-mode/probe` response into cached `FreeModeState`. */
export function applyProbeAllToState(
  state: FreeModeState,
  result: FreeProbeAllResult,
): FreeModeState {
  const byKey = new Map(result.results.map((entry) => [modelKey(entry), entry]));
  const models = state.models.map((row) => {
    const hit = byKey.get(modelKey(row));
    if (!hit) return row;
    const needsKey = hit.probe.ok
      ? false
      : hit.probe.needs_key || (row.auth_mode === "key_required" && !row.has_account);
    return {
      ...row,
      endpoint: hit.endpoint ?? row.endpoint,
      probe: hit.probe,
      needs_key: needsKey,
    };
  });
  return {
    ...state,
    models,
    summary: summarizeModels(models),
    next_fallback_model: deriveNextFallbackModel(models),
    last_checked_at: result.checked_at ?? state.last_checked_at,
  };
}

// ---------------------------------------------------------------- status & copy

export type ModelStatusKind =
  | "unusable"
  | "off"
  | "working"
  | "rate_limited"
  | "needs_key"
  | "failed"
  | "untested";

export type ModelStatusView = {
  kind: ModelStatusKind;
  tone: "pos" | "warn" | "neg" | "muted";
  label: string;
  detail: string | null;
  showAddKeyLink: boolean;
};

/**
 * Single source of truth for a free model row's live state.
 * Used by both the status cell and the filter chips so counts never disagree.
 */
export function modelStatus(row: FreeModel): ModelStatusView {
  if (!row.usable) {
    return {
      kind: "unusable",
      tone: "muted",
      label: "not usable",
      detail: row.unusable_reason,
      showAddKeyLink: false,
    };
  }
  if (!row.enabled) {
    return {
      kind: "off",
      tone: "muted",
      label: "off",
      detail: null,
      showAddKeyLink: false,
    };
  }
  if (row.probe) {
    if (row.probe.ok) {
      return {
        kind: "working",
        tone: "pos",
        label: "answered",
        detail: null,
        showAddKeyLink: false,
      };
    }
    const needsKey = row.probe.needs_key || row.needs_key;
    if (row.probe.status === 429) {
      return {
        kind: "rate_limited",
        tone: "warn",
        label: "rate limited",
        detail: row.probe.message,
        showAddKeyLink: needsKey,
      };
    }
    if (needsKey) {
      const article = /^[aeiou]/i.test(row.provider_display) ? "an" : "a";
      return {
        kind: "needs_key",
        tone: "warn",
        label: "needs key",
        detail:
          row.probe.message ??
          `Add ${article} ${row.provider_display} API key on Providers to use its free models.`,
        showAddKeyLink: true,
      };
    }
    return {
      kind: "failed",
      tone: "neg",
      label: row.probe.status > 0 ? `failed · ${row.probe.status}` : "failed",
      detail: row.probe.message,
      showAddKeyLink: false,
    };
  }
  if (row.needs_key) {
    const article = /^[aeiou]/i.test(row.provider_display) ? "an" : "a";
    return {
      kind: "needs_key",
      tone: "warn",
      label: "needs key",
      detail: `Add ${article} ${row.provider_display} API key on Providers to use its free models.`,
      showAddKeyLink: true,
    };
  }
  return {
    kind: "untested",
    tone: "muted",
    label: "not tested",
    detail: null,
    showAddKeyLink: false,
  };
}

/** Honest auth badge text beside the provider name (never hides anonymous keyless routing). */
export function authModeLabel(row: Pick<FreeModel, "auth_mode" | "has_account">): string {
  if (row.has_account) return "account connected";
  if (row.auth_mode === "key_or_anonymous" || row.auth_mode === "anonymous") {
    return "anonymous";
  }
  return "key required";
}

/** Strip scheme for compact display while keeping host + path scannable. */
export function compactEndpoint(endpoint: string | null): string {
  if (!endpoint) return "—";
  return endpoint.replace(/^https?:\/\//i, "");
}

function joinList(items: string[]): string {
  if (items.length === 0) return "";
  if (items.length === 1) return items[0]!;
  if (items.length === 2) return `${items[0]} and ${items[1]}`;
  return `${items.slice(0, -1).join(", ")}, and ${items[items.length - 1]}`;
}

/** Derived status headline answering: is it on, what works, what happens on quota exhaustion. */
export function statusHeadline(
  state: FreeModeState,
  probing: boolean,
): { title: string; detail: string; tone: "pos" | "warn" | "muted" } {
  const usable = state.models.filter((row) => row.free && row.usable);
  const enabledModels = usable.filter((row) => row.enabled);
  const testedCount = enabledModels.filter((row) => row.probe !== null).length;
  const providerNames = Array.from(new Set(usable.map((row) => row.provider_display)));

  if (!state.enabled) {
    const foundLine =
      usable.length === 0
        ? "No usable free models found yet."
        : providerNames.length > 0
          ? `${usable.length} free model${usable.length === 1 ? "" : "s"} found on ${joinList(providerNames)}.`
          : `${usable.length} free model${usable.length === 1 ? "" : "s"} found.`;
    return {
      title: "Free models are off.",
      detail: `${foundLine} Turn on to route ${state.ref} and test which ones answer right now.`,
      tone: "muted",
    };
  }

  const fallbackSentence = state.fallback_on_quota
    ? "When paid quota runs out: retry on a free model."
    : "When paid quota runs out: off.";

  if (probing) {
    return {
      title: "Free models are on. Testing enabled models…",
      detail: `Requests to ${state.ref} use them. ${fallbackSentence}`,
      tone: "muted",
    };
  }

  const enabledCount = state.summary.enabled;
  const workingCount = state.summary.working;

  if (workingCount > 0) {
    return {
      title: `Free models are on. ${workingCount} of ${enabledCount} answer right now.`,
      detail: `Requests to ${state.ref} use them. ${fallbackSentence}`,
      tone: "pos",
    };
  }

  if (testedCount > 0) {
    return {
      title: `Free models are on. 0 of ${enabledCount} answered the last check.`,
      detail: `Requests to ${state.ref} will wait for a working model or provider key. ${fallbackSentence}`,
      tone: "warn",
    };
  }

  if (enabledCount === 0) {
    return {
      title: "Free models are on, but every model is switched off.",
      detail: `Turn at least one model on below to route ${state.ref}. ${fallbackSentence}`,
      tone: "warn",
    };
  }

  return {
    title: `Free models are on. ${enabledCount} enabled, not tested yet.`,
    detail: `Requests to ${state.ref} use them. ${fallbackSentence}`,
    tone: "muted",
  };
}

function triggerPhrase(triggers: readonly string[]): string {
  const codes = triggers.filter((item) => /^\d{3}$/.test(item));
  const hasQuota = triggers.includes("quota");
  if (hasQuota && codes.length > 0) {
    return `${codes.join("/")} or an exhausted quota`;
  }
  if (codes.length > 0) return codes.join("/");
  if (hasQuota) return "an exhausted quota";
  return "a quota error";
}

/**
 * Plain-language preview of the automatic fallback switch, derived from real settings.
 * Always reflects what will actually happen before and after flipping the switch.
 */
export function fallbackPreview(state: FreeModeState): {
  summary: string;
  footnote: string;
  warning: boolean;
} {
  const triggers = triggerPhrase(state.fallback_triggers);
  const next = state.next_fallback_model;
  const footnote = `Requests that already name ${state.ref} or ${state.ref}/<model> never fall back. If the free pool also fails, RRouter returns the original paid error.`;

  if (!state.enabled) {
    if (state.fallback_on_quota) {
      return {
        summary:
          "Automatic fallback is switched on, but the free pool is off — nothing will fall back until you turn Free models on above.",
        footnote,
        warning: true,
      };
    }
    return {
      summary: `Off. Turn Free models on and enable this switch so paid requests that fail with ${triggers} retry once on the free pool instead of failing.`,
      footnote,
      warning: false,
    };
  }

  if (!state.fallback_on_quota) {
    return {
      summary: `Off. Paid requests that fail with ${triggers} return the error as usual. Turn on to retry them once on the free pool${next ? ` (${next} first)` : ""}.`,
      footnote,
      warning: false,
    };
  }

  if (!next) {
    return {
      summary: `On, but no free model is ready to take traffic right now. Paid requests that fail with ${triggers} will still return their original error until at least one free model is enabled and has access.`,
      footnote,
      warning: true,
    };
  }

  return {
    summary: `A paid request that fails with ${triggers} is retried once on the free pool (${next} first). The response headers and the request log mark it as a free fallback.`,
    footnote,
    warning: false,
  };
}

// ---------------------------------------------------------------- filtering & sorting

export type ModelFilter = "all" | "working" | "needs_key" | "off";

export type ModelBuckets = {
  usable: FreeModel[];
  unusable: FreeModel[];
  counts: Record<ModelFilter, number>;
  providers: Array<{ id: string; display: string; count: number }>;
};

function sortRank(row: FreeModel): number {
  const status = modelStatus(row);
  switch (status.kind) {
    case "working":
      return 0;
    case "untested":
      return 1;
    case "rate_limited":
      return 2;
    case "needs_key":
      return 3;
    case "failed":
      return 4;
    case "off":
      return 5;
    case "unusable":
      return 6;
  }
}

export function sortFreeModels(models: readonly FreeModel[]): FreeModel[] {
  return [...models].sort((a, b) => {
    const rankDiff = sortRank(a) - sortRank(b);
    if (rankDiff !== 0) return rankDiff;
    if (a.probe?.ok && b.probe?.ok) {
      const latDiff = a.probe.latency_ms - b.probe.latency_ms;
      if (latDiff !== 0) return latDiff;
    }
    return (
      a.provider_display.localeCompare(b.provider_display) ||
      a.model.localeCompare(b.model)
    );
  });
}

export function partitionModels(models: readonly FreeModel[]): ModelBuckets {
  const freeOnly = models.filter((row) => row.free);
  const usable = sortFreeModels(freeOnly.filter((row) => row.usable));
  const unusable = sortFreeModels(freeOnly.filter((row) => !row.usable));

  const counts: Record<ModelFilter, number> = {
    all: usable.length,
    working: 0,
    needs_key: 0,
    off: 0,
  };
  const providerMap = new Map<string, { id: string; display: string; count: number }>();

  for (const row of usable) {
    const status = modelStatus(row);
    if (status.kind === "working") counts.working += 1;
    if (status.kind === "needs_key" || status.showAddKeyLink) counts.needs_key += 1;
    if (status.kind === "off") counts.off += 1;

    const existing = providerMap.get(row.provider);
    if (existing) existing.count += 1;
    else providerMap.set(row.provider, { id: row.provider, display: row.provider_display, count: 1 });
  }

  return {
    usable,
    unusable,
    counts,
    providers: Array.from(providerMap.values()),
  };
}

export function filterUsableModels(
  usable: readonly FreeModel[],
  filter: ModelFilter,
  provider: string | null,
): FreeModel[] {
  return usable.filter((row) => {
    if (provider !== null && row.provider !== provider) return false;
    if (filter === "all") return true;
    const status = modelStatus(row);
    if (filter === "working") return status.kind === "working";
    if (filter === "needs_key") return status.kind === "needs_key" || status.showAddKeyLink;
    if (filter === "off") return status.kind === "off";
    return true;
  });
}
