import { useQuery } from "@tanstack/react-query";
import {
  api,
  list,
  type Alias,
  type Combo,
  type Connection,
  type Health,
  type KeyResult,
  type Model,
  type ModelProvider,
  type OAuthProvider,
  type OAuthStatus,
  type SystemInfo,
  type UsageBucket,
  type UsageEvent,
  type UsageModelBucket,
  type UsageSummaryRow,
} from "./api";

/** Query keys live here so invalidation after a mutation is always exact. */
export const qk = {
  health: ["health"] as const,
  system: ["system"] as const,
  connections: ["connections"] as const,
  keys: ["keys"] as const,
  models: ["models"] as const,
  modelProviders: ["model-providers"] as const,
  aliases: ["aliases"] as const,
  combos: ["combos"] as const,
  usage: (limit: number) => ["usage", limit] as const,
  series: (hours: number, bucket?: string, by?: string) =>
    ["usage-series", hours, bucket ?? "", by ?? ""] as const,
  summary: (hours: number | null) => ["usage-summary", hours ?? "all"] as const,
  oauthProviders: ["oauth-providers"] as const,
  oauthStatus: (provider: string, id: string) => ["oauth-status", provider, id] as const,
};

export function useHealth() {
  return useQuery({
    queryKey: qk.health,
    queryFn: () => api.get<Health>("/health"),
    refetchInterval: 5_000,
  });
}

export function useSystem() {
  return useQuery({ queryKey: qk.system, queryFn: () => api.get<SystemInfo>("/api/system") });
}

export function useConnections() {
  return useQuery({ queryKey: qk.connections, queryFn: () => list<Connection>("/api/connections") });
}

export function useKeys() {
  return useQuery({ queryKey: qk.keys, queryFn: () => list<KeyResult>("/api/keys") });
}

export function useModels() {
  return useQuery({
    queryKey: qk.models,
    queryFn: () => list<Model>("/api/models"),
    staleTime: 30_000,
  });
}

export function useModelProviders() {
  return useQuery({
    queryKey: qk.modelProviders,
    queryFn: () => list<ModelProvider>("/api/models/providers"),
    staleTime: 30_000,
  });
}

export function useAliases() {
  return useQuery({ queryKey: qk.aliases, queryFn: () => list<Alias>("/api/aliases") });
}

export function useCombos() {
  return useQuery({ queryKey: qk.combos, queryFn: () => list<Combo>("/api/combos") });
}

export function useUsage(limit = 1000, hours?: number) {
  const query = new URLSearchParams({ limit: String(limit) });
  if (hours !== undefined) query.set("hours", String(hours));
  return useQuery({
    queryKey: [...qk.usage(limit), hours ?? "all"] as const,
    queryFn: () => list<UsageEvent>(`/api/usage?${query}`),
    refetchInterval: 15_000,
  });
}

/** Buckets aggregated server-side, so the window is real regardless of volume. */
export function useUsageSeries(hours: number, bucket?: string) {
  const query = new URLSearchParams({ hours: String(hours) });
  if (bucket) query.set("bucket", bucket);
  return useQuery({
    queryKey: qk.series(hours, bucket),
    queryFn: () => list<UsageBucket>(`/api/usage/series?${query}`),
    refetchInterval: 30_000,
  });
}

/** The same series split by model, for the stacked traffic chart. */
export function useUsageByModel(hours: number, bucket?: string) {
  const query = new URLSearchParams({ hours: String(hours), by: "model" });
  if (bucket) query.set("bucket", bucket);
  return useQuery({
    queryKey: qk.series(hours, bucket, "model"),
    queryFn: () => list<UsageModelBucket>(`/api/usage/series?${query}`),
    refetchInterval: 30_000,
  });
}

/** Per-model totals, scoped to the last `hours` when given (the Activity table). */
export function useUsageSummary(hours: number | null = null) {
  return useQuery({
    queryKey: qk.summary(hours),
    queryFn: () =>
      list<UsageSummaryRow>(`/api/usage/summary${hours === null ? "" : `?hours=${hours}`}`),
    refetchInterval: 15_000,
  });
}

export function useOAuthProviders() {
  return useQuery({
    queryKey: qk.oauthProviders,
    queryFn: () => list<OAuthProvider>("/api/oauth/providers"),
    staleTime: 60_000,
  });
}

/** Poll one in-flight sign-in; stops the moment the server stops reporting pending. */
export function useOAuthStatus(provider: string | null, id: string | null, intervalMs: number) {
  return useQuery({
    queryKey: qk.oauthStatus(provider ?? "", id ?? ""),
    queryFn: () => api.get<OAuthStatus>(`/api/oauth/${provider}/status/${id}`),
    enabled: Boolean(provider && id),
    refetchInterval: intervalMs,
    retry: false,
    staleTime: 0,
  });
}
