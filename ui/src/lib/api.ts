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

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const res = await fetch(path, {
    ...init,
    headers: {
      ...(init?.body ? { "content-type": "application/json" } : {}),
      ...init?.headers,
    },
  });
  const text = await res.text();
  const body: unknown = text ? safeJson(text) : null;
  if (!res.ok) {
    const message =
      (isObject(body) && isObject(body.error) && typeof body.error.message === "string"
        ? body.error.message
        : null) ?? `${res.status} ${res.statusText}`;
    throw new ApiError(res.status, message, body);
  }
  return body as T;
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
  del: <T>(path: string) => request<T>(path, { method: "DELETE" }),
};

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
  is_active: boolean;
  backoff_level: number;
  has_access_token: boolean;
  has_refresh_token: boolean;
  has_api_key: boolean;
  last_error: string | null;
};

export type KeyResult = { id: string; name: string | null; created_at: number };
export type NewKey = { id: string; key: string };

export type Alias = { alias: string; target: string };
export type Combo = { name: string; models: string[] };

export type UsageSummaryRow = {
  provider: string;
  model: string;
  requests: number;
  errors: number;
  input_tokens: number;
  output_tokens: number;
  cache_read_tokens: number;
  cache_write_tokens: number;
  avg_latency_ms: number;
};

export type UsageEvent = {
  /** Usage row id: the request id the log copies. */
  id: number | null;
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
  latency_ms: number;
  /** Time to the first model output; 0 when unmeasured (non-streaming, failures). */
  ttft_ms: number;
  status: number;
  streamed: boolean;
  error: string | null;
};

export type UsageBucket = {
  start: number;
  requests: number;
  errors: number;
  input_tokens: number;
  output_tokens: number;
  cache_read_tokens: number;
  cache_write_tokens: number;
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
