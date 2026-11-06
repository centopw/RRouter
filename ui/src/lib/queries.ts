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
  type SystemInfo,
  type UsageEvent,
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
  summary: ["usage-summary"] as const,
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

export function useUsage(limit = 1000) {
  return useQuery({
    queryKey: qk.usage(limit),
    queryFn: () => list<UsageEvent>(`/api/usage?limit=${limit}`),
    refetchInterval: 15_000,
  });
}

export function useUsageSummary() {
  return useQuery({
    queryKey: qk.summary,
    queryFn: () => list<UsageSummaryRow>("/api/usage/summary"),
    refetchInterval: 15_000,
  });
}
