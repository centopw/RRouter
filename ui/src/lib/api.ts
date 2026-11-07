/** Every admin call goes through here: one error shape, one JSON envelope. */

export class ApiError extends Error {
  readonly status: number;
  readonly body: unknown;
  constructor(status: number, message: string, body: unknown) {
    super(message);
    this.name = "ApiError";
    this.status = status;
    this.body = body;
  }
}

type Json = Record<string, unknown>;

/**
 * Admin API token (`RROUTER_ADMIN_TOKEN`). Kept in sessionStorage, never localStorage: it
 * should not outlive the tab, and the dashboard must be able to authenticate through a
 * reverse proxy where the loopback shortcut does not apply.
 */
const ADMIN_TOKEN_KEY = "rrouter:admin-token";

export const ADMIN_REQUIRED_EVENT = "rrouter:admin-token-required";
/** Fired when the admin API answers 401 and no admin token is stored: the guard sends
 *  the operator to `/sign-in`. */
export const SIGN_IN_REQUIRED_EVENT = "rrouter:sign-in-required";

export function adminToken(): string | null {
  try {
    return window.sessionStorage.getItem(ADMIN_TOKEN_KEY);
  } catch {
    return null;
  }
}

export function setAdminToken(token: string | null): void {
  try {
    if (token) window.sessionStorage.setItem(ADMIN_TOKEN_KEY, token);
    else window.sessionStorage.removeItem(ADMIN_TOKEN_KEY);
  } catch {
    // Storage can be unavailable (private mode); the request simply goes without it.
  }
}

function isAdminPath(path: string): boolean {
  return path.startsWith("/api/") || path.startsWith("/metrics");
}

/** Methods that mutate state need the CSRF header when the caller is a dashboard
 *  session; adding it unconditionally is harmless for token and loopback callers. */
function csrfHeader(method: string | undefined): Record<string, string> {
  const verb = (method ?? "GET").toUpperCase();
  return verb === "GET" || verb === "HEAD" ? {} : { "x-rrouter-csrf": "1" };
}

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const token = isAdminPath(path) ? adminToken() : null;
  const res = await fetch(path, {
    ...init,
    headers: {
      ...(init?.body ? { "content-type": "application/json" } : {}),
      ...csrfHeader(init?.method),
      ...(token ? { "x-admin-token": token } : {}),
      ...init?.headers,
    },
  });
  const text = await res.text();
  const body: unknown = text ? safeJson(text) : null;
  if (!res.ok) throw apiError(res, body, path);
  return body as T;
}

/**
 * The one failure shape: the server's `{error: {message}}` when it sent one, otherwise the
 * status line. A 401 on an admin path re-asks for the token, whichever fetch hit it.
 */
function apiError(res: Response, body: unknown, path: string): ApiError {
  const message =
    (isObject(body) && isObject(body.error) && typeof body.error.message === "string"
      ? body.error.message
      : null) ?? `${res.status} ${res.statusText}`;
  if (res.status === 401 && isAdminPath(path)) {
    window.dispatchEvent(new Event(adminToken() ? ADMIN_REQUIRED_EVENT : SIGN_IN_REQUIRED_EVENT));
  }
  return new ApiError(res.status, message, body);
}

/**
 * Binary fetch for the CSV export: same token and failure shape as `request`, but the body
 * stays a blob and the server's `content-disposition` filename comes along.
 */
export async function download(path: string): Promise<{ blob: Blob; filename: string | null }> {
  const token = isAdminPath(path) ? adminToken() : null;
  const res = await fetch(path, { headers: token ? { "x-admin-token": token } : {} });
  if (!res.ok) {
    const text = await res.text();
    const body: unknown = text ? safeJson(text) : null;
    throw apiError(res, body, path);
  }
  return {
    blob: await res.blob(),
    filename: attachmentName(res.headers.get("content-disposition")),
  };
}

/** `filename="x.csv"`, or RFC 5987 `filename*=UTF-8''x.csv`; null when unnamed. */
function attachmentName(header: string | null): string | null {
  if (!header) return null;
  const encoded = /filename\*\s*=\s*UTF-8''([^;]+)/i.exec(header)?.[1]?.trim();
  if (encoded) {
    try {
      return decodeURIComponent(encoded);
    } catch {
      return null;
    }
  }
  return /filename\s*=\s*"?([^";]+)"?/i.exec(header)?.[1]?.trim() ?? null;
}

function safeJson(text: string): unknown {
  try {
    return JSON.parse(text);
  } catch {
    return text;
  }
}

function isObject(v: unknown): v is Json {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

export const api = {
  get: <T>(path: string) => request<T>(path),
  post: <T>(path: string, body?: unknown) =>
    request<T>(path, { method: "POST", body: body === undefined ? undefined : JSON.stringify(body) }),
  put: <T>(path: string, body?: unknown) =>
    request<T>(path, { method: "PUT", body: body === undefined ? undefined : JSON.stringify(body) }),
  patch: <T>(path: string, body?: unknown) =>
    request<T>(path, { method: "PATCH", body: body === undefined ? undefined : JSON.stringify(body) }),
  del: <T>(path: string) => request<T>(path, { method: "DELETE" }),
};

export async function getAccountSubscriptions(): Promise<AccountSubscription[]> {
  const body = await api.get<{ accounts?: AccountSubscription[] }>("/api/accounts/usage");
  return body.accounts ?? [];
}

export async function getAccountQuotas(): Promise<AccountQuotaRow[]> {
  const body = await api.get<{ quotas?: AccountQuotaRow[] }>("/api/accounts/quotas");
  return body.quotas ?? [];
}

/** Send only what changes: omitted fields keep their stored value. */
export function updateAccountQuota(id: string, patch: UpdateAccountQuota): Promise<{ quota: AccountQuotaRow }> {
  return api.put<{ quota: AccountQuotaRow }>(`/api/accounts/quotas/${encodeURIComponent(id)}`, patch);
}

export function clearAccountQuota(id: string): Promise<void> {
  return api.del<void>(`/api/accounts/quotas/${encodeURIComponent(id)}`);
}

// ---------------------------------------------------------------- shapes

export type Health = { ok: boolean; version: string; uptime_secs: number };

export type SystemInfo = {
  version: string;
  uptime_secs: number;
  require_api_key: boolean;
  allow_remote_admin: boolean;
  static_assets: boolean;
  master_key_source: string;
  custom_cors: boolean;
  counts: {
    connections: number;
    active_connections: number;
    api_keys: number;
    aliases: number;
    combos: number;
    disabled_models: number;
    providers: string[];
  };
};

export type Connection = {
  id: string;
  provider: string;
  name: string | null;
  email: string | null;
  auth_type: string;
  priority: number;
  /** Share of traffic inside one priority group; only used by the weighted strategy. */
  weight: number;
  is_active: boolean;
  backoff_level: number;
  has_access_token: boolean;
  has_refresh_token: boolean;
  has_api_key: boolean;
  last_error: string | null;
};

/** The UTC calendar window a quota resets on; `total` (all time) is API-only. */
export type AccountQuotaWindow = "daily" | "weekly" | "monthly";

/**
 * One upstream account's quota and the spend of its current window. `0` limits are
 * unlimited; `resets_at` is unix **seconds** and null for an all-time quota.
 */
export type AccountQuotaRow = {
  connection_id: string;
  name: string | null;
  provider: string;
  is_active: boolean;
  window: AccountQuotaWindow;
  max_requests: number;
  max_tokens: number;
  max_cost_micros: number;
  requests: number;
  tokens: number;
  cost_micros: number;
  exhausted: boolean;
  resets_at: number | null;
};

/** One window reported by the provider's own usage endpoint. */
export type SubscriptionWindow = {
  id: string;
  label: string;
  used_percent: number;
  window_seconds: number | null;
  resets_at: number | null;
};

/** Plan limits read from the provider; `error` explains why there are none. */
export type AccountSubscription = {
  connection_id: string;
  provider: string;
  name: string | null;
  plan: string | null;
  windows: SubscriptionWindow[];
  error: string | null;
  fetched_at: number;
};

export type UpdateAccountQuota = Partial<{
  window: AccountQuotaWindow;
  max_requests: number;
  max_tokens: number;
  max_cost_micros: number;
}>;

/**
 * What a key is allowed to reach. `inference` keys call `/v1` and carry limits and
 * guardrails; `admin` keys manage the gateway (`/api/*`, `/metrics`) and never spend tokens.
 */
export type KeyScope = "inference" | "admin";

/** One rewrite on a key: `from` is what the client sends, `to` a registry ref. */
export type KeyModelMap = { from: string; to: string };

/** Reset cadence for spend counters and budgets; `total` never resets. */
export type BudgetWindow = "total" | "daily" | "weekly" | "monthly";

/**
 * Traffic policy on one key. `max_prompt_chars` absent means unlimited — `0` is a real
 * cap ("no prompt at all"), not the "unlimited" spelling the other numeric limits use.
 * `blocked` holds case-insensitive substrings, stored trimmed, lowercased and unique;
 * `scan_output` also checks responses, where a hit cuts a stream with an error.
 * The server omits defaulted fields and stores an all-default policy as null, so a
 * policy that only caps prompts arrives without `blocked` or `scan_output`.
 */
export type Guardrails = {
  max_prompt_chars?: number;
  blocked: string[];
  scan_output: boolean;
};

/**
 * A gateway key with its governance limits. `rpm`/`tpm`/`budget_tokens`/
 * `budget_micros` use 0 for "unlimited"; `allowed_models` null means any model;
 * `expires_at` null means never. `spent_micros` is the key's USD spend in
 * millionths of a dollar, so 1 USD = 1_000_000.
 * `models` is the key's rewrite map, absent when the server predates it.
 * `guardrails` null means no checks at all.
 * `scope` decides where the key is accepted; admin keys are rejected on `/v1`.
 */
export type KeyResult = {
  id: string;
  name: string | null;
  created_at: number;
  is_active: boolean;
  scope: KeyScope;
  rpm: number;
  tpm: number;
  budget_tokens: number;
  spent_tokens: number;
  /** USD budget in micros; 0 = unlimited. */
  budget_micros: number;
  /** USD spend in micros, computed per request like `spent_tokens`. */
  spent_micros: number;
  /** Reset cadence; `spent_*` already count only the current window. */
  budget_window: BudgetWindow;
  allowed_models: string[] | null;
  expires_at: number | null;
  models?: KeyModelMap[];
  guardrails: Guardrails | null;
};

/** Exactly the fields `PATCH /api/keys/{id}` accepts; null clears a nullable field. */
export type UpdateKey = Partial<{
  is_active: boolean;
  /** `admin` keys manage the gateway instead of calling `/v1`. */
  scope: KeyScope;
  rpm: number;
  tpm: number;
  budget_tokens: number;
  /** USD budget in micros; 0 clears it. */
  budget_micros: number;
  /** Reset cadence for the budgets; `total` never resets. */
  budget_window: BudgetWindow;
  allowed_models: string[] | null;
  expires_at: number | null;
  /** Whole policy replace; null clears every check. */
  guardrails: Guardrails | null;
}>;

/** Exactly the fields `PATCH /api/connections/{id}` accepts; weight is clamped to 1..=1000. */
export type UpdateConnection = Partial<{
  priority: number;
  weight: number;
}>;

export type NewKey = { id: string; key: string };

export type Alias = { alias: string; target: string };
export type Combo = { name: string; models: string[] };

/** A reusable system prompt. `version` counts body revisions; a rename does not bump it. */
export type Template = {
  id: string;
  name: string;
  body: string;
  version: number;
  created_at: number;
  updated_at: number;
};

/** One historical body; the highest `version` is also the live one. */
export type TemplateVersion = { version: number; body: string; created_at: number };

export type UsageSummaryRow = {
  provider: string;
  model: string;
  requests: number;
  errors: number;
  input_tokens: number;
  output_tokens: number;
  cache_read_tokens: number;
  cache_write_tokens: number;
  /** USD in micros, priced from catalog prices; 0 when a model has no price. */
  cost_micros: number;
  avg_latency_ms: number;
};

export type UsageEvent = {
  /** Usage row id: what the master/detail list selects on. */
  id: number | null;
  /** Stable across fallback attempts; `/api/usage/{request_id}` takes this. */
  request_id: string;
  /** Key that authenticated the request; null for unauthenticated calls. */
  key_id: string | null;
  ts: number;
  provider: string;
  model: string;
  connection_id: string | null;
  /** Client endpoint, e.g. `/v1/messages`. */
  endpoint: string | null;
  input_tokens: number;
  output_tokens: number;
  cache_read_tokens: number;
  cache_write_tokens: number;
  /** USD in micros for this request; 0 when prices are unknown. */
  cost_micros: number;
  latency_ms: number;
  /** Time to the first model output; 0 when unmeasured (non-streaming, failures). */
  ttft_ms: number;
  /** Time from sending upstream to its response headers; 0 when unmeasured. */
  upstream_ms: number;
  status: number;
  streamed: boolean;
  error: string | null;
};

/** One dispatch attempt behind a request; attempt 1 is the first send, not the winner. */
export type RequestAttempt = {
  /** 1-based, in dispatch order. */
  attempt: number;
  ts: number;
  provider: string;
  model: string;
  connection_id: string | null;
  /** 0 means no response at all: transport failure or an open breaker. */
  status: number;
  error: string | null;
  latency_ms: number;
};

/** `GET /api/usage/{request_id}`: the usage row plus every attempt it dispatched. */
export type UsageDetail = {
  request: UsageEvent;
  attempts: RequestAttempt[];
};

export type UsageBucket = {
  start: number;
  requests: number;
  errors: number;
  input_tokens: number;
  output_tokens: number;
  cache_read_tokens: number;
  cache_write_tokens: number;
  /** USD in micros for the bucket; 0 when prices are unknown. */
  cost_micros: number;
  avg_latency_ms: number;
};

/** One chart cell for one model (`/api/usage/series?by=model`). */
export type UsageModelBucket = {
  start: number;
  provider: string;
  model: string;
  requests: number;
  errors: number;
  input_tokens: number;
  output_tokens: number;
  cache_read_tokens: number;
  cache_write_tokens: number;
  /** USD in micros for this model in this bucket. */
  cost_micros: number;
  avg_latency_ms: number;
};

export type Capabilities = {
  vision: boolean;
  pdf: boolean;
  audio_input: boolean;
  video_input: boolean;
  image_output: boolean;
  audio_output: boolean;
  tools: boolean;
  reasoning: boolean;
  search: boolean;
};

export type ModelCost = {
  input: number;
  output: number;
  cache_read?: number;
  cache_write?: number;
};

export type Model = {
  ref: string;
  provider: string;
  id: string;
  display: string | null;
  context: number | null;
  max_output: number | null;
  capabilities: Capabilities;
  /** USD per million tokens, from models.dev. Absent when no catalog covers the model. */
  cost: ModelCost | null;
  source: "catalog" | "upstream" | "custom" | "synced";
  enabled: boolean;
  available: boolean;
  /** A family its provider retired (or models.dev marks deprecated): listed last. */
  deprecated: boolean;
};

export type ModelProvider = {
  id: string;
  display: string;
  /** How the provider authenticates: a subscription sign-in, an API key, or local. */
  kind: "oauth" | "api_key" | "local";
  has_connection: boolean;
  listed: boolean;
};

export type SyncResult = { ok: boolean; models: number; providers: number; synced_at: number };
export type FetchResult = {
  provider: string;
  status: number;
  count: number;
  added: number;
  data: string[];
};
export type ModelTest = {
  ok: boolean;
  status: number;
  latency_ms: number;
  model: string;
  reply?: string;
  error?: string;
};

export type OAuthProvider = { id: string; display: string; flow: string };
export type OAuthChallenge =
  | { kind: "pkce"; authorize_url: string; redirect_uri: string }
  | {
      kind: "device_code";
      verification_uri: string;
      user_code: string;
      expires_in: number;
      interval: number;
    };
export type OAuthStart = {
  id: string;
  provider: string;
  flow: string;
  challenge: OAuthChallenge;
};
export type OAuthStatus = { id: string; provider: string; pending: boolean };

/** Unwrap the `{data: T[]}` envelope every list endpoint uses. */
export async function list<T>(path: string): Promise<T[]> {
  const body = await api.get<{ data?: T[] }>(path);
  return body.data ?? [];
}

/** One page of a keyset-paged list: `next_*` are null once the walk has reached the end. */
export type Page<T> = {
  data: T[];
  next_before: number | null;
  next_before_id: number | null;
};

/** Like `list()`, but keeps the cursor fields the envelope carries. */
export async function listPage<T>(path: string): Promise<Page<T>> {
  const body = await api.get<Partial<Page<T>>>(path);
  return {
    data: body.data ?? [],
    next_before: body.next_before ?? null,
    next_before_id: body.next_before_id ?? null,
  };
}
