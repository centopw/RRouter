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

async function rotateSession(): Promise<boolean> {
  try {
    const res = await fetch("/api/auth/refresh", {
      method: "POST",
      headers: { "x-rrouter-csrf": "1" },
    });
    return res.ok;
  } catch {
    return false;
  }
}

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const token = isAdminPath(path) ? adminToken() : null;
  const headers = {
    ...(init?.body ? { "content-type": "application/json" } : {}),
    ...csrfHeader(init?.method),
    ...(token ? { "x-admin-token": token } : {}),
    ...init?.headers,
  };
  const send = () => fetch(path, { ...init, headers });
  let res = await send();
  // A revoked/expired JWT gets one rotation attempt, then the original request is
  // replayed with the new cookie. Auth endpoints are excluded so sign-in failures do
  // not loop through refresh.
  if (
    res.status === 401 &&
    isAdminPath(path) &&
    !token &&
    !path.startsWith("/api/auth/")
  ) {
    if (await rotateSession()) res = await send();
  }
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

/** Save a downloaded blob under `filename` and release the object URL. */
export function saveBlob(blob: Blob, filename: string): void {
  const url = URL.createObjectURL(blob);
  const link = document.createElement("a");
  link.href = url;
  link.download = filename;
  document.body.appendChild(link);
  link.click();
  link.remove();
  // Revoking immediately can cancel the download in Safari; one tick is enough.
  setTimeout(() => URL.revokeObjectURL(url), 1000);
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
  /** Owning team; `null` means shared/instance-wide (serves non-team keys only). */
  team_id: string | null;
  /** Dashboard attribution: who added the account. Not an access grant. */
  owner_user_id: string | null;
  /**
   * Origin this account dispatches to; `null` means the provider's manifest URL. The
   * gateway keeps the provider's endpoint path and takes only the origin from here.
   */
  base_url_override: string | null;
  priority: number;
  /** Share of traffic inside one priority group; only used by the weighted strategy. */
  weight: number;
  is_active: boolean;
  backoff_level: number;
  has_access_token: boolean;
  has_refresh_token: boolean;
  has_api_key: boolean;
  last_error: string | null;
  /**
   * Cloud project an Antigravity (Cloud Code Assist) sign-in was provisioned with. The
   * gateway stores it in `credentials.extra.project_id`; a build that does not project it
   * simply omits the field, and the row shows no project line.
   */
  project_id?: string | null;
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

/** Start a subscription window with a tiny request; Claude/Codex OAuth accounts only. */
export type QuotaPingMode = "off" | "reset" | "schedule" | "smart";

/** Usage history behind the smart stagger: recent requests and the assigned phase. */
export type QuotaPingSmart = {
  recent_requests: number;
  phase_offset_secs: number;
};

/** One active Claude/Codex connection's auto-ping schedule and last known state. */
export type QuotaPingRow = {
  connection_id: string;
  provider: string;
  name: string | null;
  mode: QuotaPingMode;
  anchor: string;
  timezone: string;
  window_hours: number;
  slots_per_day: number;
  ping_model: string | null;
  last_ping_at: number | null;
  last_ping_reset_at: number | null;
  next_ping_at: number | null;
  last_error: string | null;
  updated_at: number;
  smart: QuotaPingSmart;
};

/** Omitted fields keep their stored value; `mode=off` clears the schedule server-side. */
export type UpdateQuotaPing = Partial<{
  mode: QuotaPingMode;
  anchor: string;
  timezone: string;
  window_hours: number;
  slots_per_day: number;
  ping_model: string | null;
}>;

export async function getQuotaPings(): Promise<QuotaPingRow[]> {
  const body = await api.get<{ data?: QuotaPingRow[] }>("/api/quota-pings");
  return body.data ?? [];
}

export function updateQuotaPing(id: string, patch: UpdateQuotaPing): Promise<QuotaPingRow> {
  return api.put<QuotaPingRow>(`/api/quota-pings/${encodeURIComponent(id)}`, patch);
}

/** Older builds predate a route; the UI hides the control instead of reporting an error. */
export function isEndpointMissing(error: unknown): boolean {
  return error instanceof ApiError && (error.status === 404 || error.status === 405);
}

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
  /**
   * Mask emails, phone numbers, card numbers and credential-shaped strings in the prompt
   * and in a non-streaming response. Off unless the operator opts in.
   */
  redact_pii?: boolean;
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
  /** Tenancy: the team this key spends for, or null for instance-wide. */
  team_id?: string | null;
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
  /** Tenancy; null releases the key back to instance-wide. */
  team_id: string | null;
}>;


/** A member of a team; `team_admin` is scoped administration. */
export type TeamRole = "team_admin" | "member" | "viewer";

export type TeamMember = {
  team_id: string;
  user_id: string;
  email: string | null;
  name: string | null;
  role: TeamRole;
  created_at: number;
};

export type TeamSpend = { requests: number; tokens: number; cost_micros: number };

export type Team = {
  id: string;
  name: string;
  created_at: number;
  budget_tokens: number;
  budget_micros: number;
  budget_window: BudgetWindow;
  members: number;
  spend: TeamSpend;
  /** Present on the single-team read only. */
  members_list?: TeamMember[];
};

/** Exactly the fields `POST`/`PATCH /api/teams/{id}` accept. */
export type UpdateTeam = Partial<{
  name: string;
  budget_tokens: number;
  budget_micros: number;
  budget_window: BudgetWindow;
}>;


/** One eval suite: a named list of cases a model is run against. */
export type EvalSuite = {
  id: string;
  name: string;
  description: string | null;
  /** Prepended to every case as a system message; null/empty = no system message. */
  system_prompt: string | null;
  cases: number;
  created_at: number;
  updated_at: number;
};

/** One case: the prompt, and the text a correct answer should contain. */
export type EvalCase = {
  id: string;
  suite_id: string;
  name: string | null;
  input: string;
  expected: string | null;
  created_at: number;
};

export type EvalRunStatus = "pending" | "running" | "completed" | "failed";

export type EvalSummary = {
  cases: number;
  passed: number;
  failed: number;
  unscored: number;
  errors: number;
  duration_ms: number;
  error?: string | null;
};

export type EvalRun = {
  id: string;
  suite_id: string;
  model: string;
  /** Optional decision model that judged the output semantically; null = literal check. */
  judge_model: string | null;
  status: EvalRunStatus;
  created_at: number;
  completed_at: number | null;
  summary: EvalSummary | null;
  progress: { done: number; total: number };
};

/** One case's outcome. `passed` is null when the case had no expectation. */
export type EvalResult = {
  id: string;
  run_id: string;
  case_id: string;
  case_name: string | null;
  case_input: string;
  output: string | null;
  score: number | null;
  passed: boolean | null;
  latency_ms: number | null;
  error: string | null;
  created_at: number;
};

export type EvalSuiteDetail = { data: EvalSuite; cases: EvalCase[]; runs: EvalRun[] };
export type EvalRunDetail = { data: EvalRun; results: EvalResult[] };

/** One webhook receiver. `secret_env` names an environment variable, never a secret. */
export type Webhook = {
  id: string;
  url: string;
  event_types: string[];
  secret_env: string | null;
  has_secret: boolean;
  /** The named variable resolves in the gateway's environment right now. */
  secret_configured: boolean;
  /** `secret_configured && enabled`: whether this endpoint could receive an event today. */
  ready: boolean;
  enabled: boolean;
  created_at: number;
  /** `null` until the row is first patched. */
  updated_at: number | null;
};

export type WebhookList = {
  webhooks: Webhook[];
  /** How many endpoints may be registered. */
  max: number;
  /** The subscription vocabulary; `*` means everything. */
  event_types: string[];
  /** Legacy comma-joined copy of `event_types`, for a server build that predates the array. */
  event_types_csv?: string;
};

/** One outbox attempt. Never carries a payload. */
export type WebhookAttempt = {
  id: number;
  type: string;
  status: string;
  attempts: number;
  created_at: number;
  next_attempt_at: number | null;
  last_error: string | null;
  delivered_at: number | null;
};

export type WebhookStatus = {
  outbox: {
    pending: number;
    delivering: number;
    delivered: number;
    /** Terminal, but nothing was sent: no endpoint subscribed. Never folded into `delivered`. */
    unmatched: number;
    dead: number;
  };
  recent: WebhookAttempt[];
};

/** Exactly the fields `PATCH /api/connections/{id}` accepts; weight is clamped to 1..=1000. */
export type UpdateConnection = Partial<{
  priority: number;
  weight: number;
}>;

export type NewKey = { id: string; key: string };

export type Alias = { alias: string; target: string };
/**
 * A named candidate chain.
 *
 * `fallback` (the default for every existing combo) tries the chain in order. `round_robin`
 * rotates which model starts each request; `smart` asks `router_model` to pick the first
 * candidate. In both non-fallback strategies the remaining models stay the fallback order.
 * The fields are additive: a build that predates them returns only name/models, which the
 * dashboard reads as `fallback`.
 */
export type ComboStrategy = "fallback" | "round_robin" | "smart";

export type Combo = {
  name: string;
  models: string[];
  strategy?: ComboStrategy;
  /** Required when `strategy` is `smart`; ignored (and hidden) otherwise. */
  router_model?: string | null;
};

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
  /**
   * The provider the operator chose for this model id. It is the same on every provider
   * row of that id; `null` (or absent on an older build) means "no preference".
   */
  preferred_provider?: string | null;
};

export type ModelProvider = {
  id: string;
  display: string;
  /** How the provider authenticates: a subscription sign-in, an API key, or local. */
  kind: "oauth" | "api_key" | "local";
  has_connection: boolean;
  /**
   * Where a connected account is reachable from: `global` = a shared instance account,
   * `team` = only a team-owned account (this caller's team), `none` = nothing connected.
   */
  scope: "global" | "team" | "none";
  listed: boolean;
  /**
   * The manifest host is a placeholder (Azure's `RESOURCE.openai.azure.com`), so every
   * connection must supply its own base URL before it can route.
   */
  requires_base_url_override: boolean;
  /** Origin to show in the field, e.g. `https://<resource>.openai.azure.com`. */
  base_url_example: string | null;
};

// ---------------------------------------------------------------- model presets
//
// `/api/model-presets` (landed in the gateway): a preset is a named layout document. The
// list omits `layout`; a single GET includes it. Metadata edits never bump `revision`;
// a layout write does, and is guarded by the revision the client edited.
//
//   GET    /api/model-presets                 -> { data, active_id, default_id }
//   GET    /api/model-presets/{id}            -> ModelPreset (layout included)
//   GET    /api/model-presets/active          -> ModelPreset (this account's choice)
//   POST   /api/model-presets                 { name, description?, layout? } -> 201
//   PATCH  /api/model-presets/{id}            { name?, description? }
//   DELETE /api/model-presets/{id}            409 preset_is_default for the default
//   POST   /api/model-presets/{id}/duplicate  { name } -> 201
//   PUT    /api/model-presets/{id}/layout     { revision, layout } -> ModelPreset
//          409 preset_conflict + error.current_revision
//   POST   /api/model-presets/{id}/activate   -> { active_id, persisted }   (session)
//   POST   /api/model-presets/{id}/default    -> ModelPreset (demotes the old default)
//
// A list row carries `entry_count`/`visible_count` instead of the layout, so a picker can
// say how big a board is without loading it. The UI enables the default control and still
// degrades to a disabled state if a 404/405 says the route is absent.

/** The only layout format this build reads and writes. */
export const LAYOUT_SCHEMA_VERSION = 1;

/** One model on a preset's board. `position` is dense and zero-based inside its group. */
export type LayoutItem = {
  provider: string;
  id: string;
  position: number;
  /** `false` keeps the model on the board but out of the active list; it still routes. */
  visible: boolean;
  /**
   * Restore memory, owned by the client and stored with the document: where the item sat
   * in the visible group before it was hidden. Omitted for items that were never visible.
   */
  last_visible_position?: number;
  /** Unix seconds when the item was hidden; omitted while it is visible. */
  hidden_at?: number;
};

export type LayoutDocument = {
  schema_version: number;
  /** Merge registry models the document does not name, instead of leaving them unlisted. */
  auto_include_new: boolean;
  items: LayoutItem[];
};

export type ModelPreset = {
  id: string;
  name: string;
  description: string | null;
  /** `global` in this release. */
  scope: string;
  owner_user_id: string | null;
  team_id: string | null;
  is_default: boolean;
  /** Bumped only by a layout write. */
  revision: number;
  layout: LayoutDocument;
  created_by: string | null;
  created_at: number;
  updated_at: number;
};

/**
 * A list row: the same preset without its layout document, plus counts. Both counts are 0
 * for a document with no explicit items — the seeded default merges the live catalog and
 * stores no rows of its own.
 */
export type PresetSummary = Omit<ModelPreset, "layout"> & {
  entry_count: number;
  visible_count: number;
};

export type PresetIndex = {
  data: PresetSummary[];
  /** This caller's resolved choice; a caller with no session resolves to the default. */
  active_id: string | null;
  /** The instance default; `null` only when the library is empty. */
  default_id: string | null;
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

export type OAuthProvider = {
  id: string;
  display: string;
  flow: string;
  /**
   * This gateway accepts `{ manual: true }` for the provider: a sign-in whose callback is
   * pasted back instead of received on the loopback. Absent on older builds.
   */
  manual?: boolean;
};
export type OAuthChallenge =
  | {
      kind: "pkce";
      authorize_url: string;
      redirect_uri: string;
      /** Echoed as `true` when the attempt was started in manual mode. */
      manual?: boolean;
    }
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
