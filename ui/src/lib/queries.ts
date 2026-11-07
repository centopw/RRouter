import { useInfiniteQuery, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  api,
  clearAccountQuota,
  getAccountSubscriptions,
  getAccountQuotas,
  list,
  listPage,
  updateAccountQuota,
  type Alias,
  type Combo,
  type Connection,
  type Health,
  type KeyModelMap,
  type KeyResult,
  type Model,
  type ModelProvider,
  type OAuthProvider,
  type OAuthStatus,
  type SystemInfo,
  type Template,
  type TemplateVersion,
  type UpdateConnection,
  type UpdateAccountQuota,
  type UpdateKey,
  type UsageBucket,
  type UsageDetail,
  type UsageEvent,
  type UsageModelBucket,
  type UsageSummaryRow,
} from "./api";
import {
  authState,
  getMe,
  listAudit,
  revokeOtherSessions,
  revokeSession,
  updateMe,
  createInvite,
  listInvites,
  listUsers,
  removeUser,
  revokeInvite,
  signOut,
  updateUser,
} from "./auth";

/** Query keys live here so invalidation after a mutation is always exact. */
export const qk = {
  health: ["health"] as const,
  system: ["system"] as const,
  connections: ["connections"] as const,
  accountQuotas: ["accounts", "quotas"] as const,
  accountSubscriptions: ["accounts", "subscriptions"] as const,
  authState: ["auth", "state"] as const,
  me: ["auth", "me"] as const,
  audit: (kind: string) => ["audit", kind] as const,
  users: ["users"] as const,
  invites: ["users", "invites"] as const,
  keys: ["keys"] as const,
  keyModels: (id: string) => ["key-models", id] as const,
  models: ["models"] as const,
  modelProviders: ["model-providers"] as const,
  aliases: ["aliases"] as const,
  combos: ["combos"] as const,
  templates: ["templates"] as const,
  templateVersions: (id: string) => ["template-versions", id] as const,
  usage: (limit: number) => ["usage", limit] as const,
  usageDetail: (requestId: string) => ["usage-detail", requestId] as const,
  usagePages: (filters: UsageFilters) => ["usage-pages", filters] as const,
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

export function useAccountSubscriptions() {
  return useQuery({
    queryKey: qk.accountSubscriptions,
    queryFn: getAccountSubscriptions,
    // Provider usage endpoints are shared with the vendor's own client; a minute is
    // frequent enough for a dashboard and polite to the upstream.
    refetchInterval: 60_000,
    staleTime: 55_000,
  });
}

export function useAccountQuotas() {
  return useQuery({ queryKey: qk.accountQuotas, queryFn: getAccountQuotas });
}

export function useUpdateAccountQuota() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: ({ id, patch }: { id: string; patch: UpdateAccountQuota }) =>
      updateAccountQuota(id, patch),
    onSuccess: () => void client.invalidateQueries({ queryKey: qk.accountQuotas }),
  });
}

export function useClearAccountQuota() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: clearAccountQuota,
    onSuccess: () => void client.invalidateQueries({ queryKey: qk.accountQuotas }),
  });
}

export function useKeys() {
  return useQuery({ queryKey: qk.keys, queryFn: () => list<KeyResult>("/api/keys") });
}

/**
 * Routing knobs live on the connection row, and `PATCH` returns the updated row. The
 * active strategy is env-only, so nothing here can widen or narrow its effect.
 */
export function useUpdateConnection() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: ({ id, patch }: { id: string; patch: UpdateConnection }) =>
      api.patch<Connection>(`/api/connections/${id}`, patch),
    onSuccess: () => {
      void client.invalidateQueries({ queryKey: qk.connections });
      void client.invalidateQueries({ queryKey: qk.system });
    },
  });
}

/**
 * Limits live on the key row itself (`spent_tokens` is computed per request), so a
 * successful PATCH only needs the key list and the system counts refreshed.
 */
export function useUpdateKey() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: ({ id, patch }: { id: string; patch: UpdateKey }) =>
      api.patch<KeyResult>(`/api/keys/${id}`, patch),
    onSuccess: () => {
      void client.invalidateQueries({ queryKey: qk.keys });
      void client.invalidateQueries({ queryKey: qk.system });
    },
  });
}

/**
 * The key's `from → to` rewrite map. It lives beside the key row rather than on it so a
 * failed read can be retried in place, without hiding the limits the modal already edits.
 */
export function useKeyModels(id: string) {
  return useQuery({
    queryKey: qk.keyModels(id),
    queryFn: () => list<KeyModelMap>(`/api/keys/${id}/models`),
  });
}

/**
 * Replace the whole map (`[]` clears it). `GET /api/keys` carries a copy for the table,
 * so both the list and this key's map are refreshed.
 */
export function useUpdateKeyModels(id: string) {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (models: KeyModelMap[]) =>
      api.put<{ data?: KeyModelMap[] }>(`/api/keys/${id}/models`, { models }),
    onSuccess: () => {
      void client.invalidateQueries({ queryKey: qk.keys });
      void client.invalidateQueries({ queryKey: qk.keyModels(id) });
    },
  });
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

export function useTemplates() {
  return useQuery({ queryKey: qk.templates, queryFn: () => list<Template>("/api/templates") });
}

export function useCreateTemplate() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (input: { name: string; body: string }) =>
      api.post<Template>("/api/templates", input),
    onSuccess: () => void client.invalidateQueries({ queryKey: qk.templates }),
  });
}

/** A changed body appends a revision, so the history joins the list invalidation. */
export function useUpdateTemplate() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: ({ id, name, body }: { id: string; name: string; body: string }) =>
      api.put<Template>(`/api/templates/${encodeURIComponent(id)}`, { name, body }),
    onSuccess: (row) => {
      void client.invalidateQueries({ queryKey: qk.templates });
      void client.invalidateQueries({ queryKey: qk.templateVersions(row.id) });
    },
  });
}

export function useDeleteTemplate() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (id: string) => api.del<{ deleted: boolean }>(`/api/templates/${encodeURIComponent(id)}`),
    onSuccess: () => void client.invalidateQueries({ queryKey: qk.templates }),
  });
}

/** Newest first, straight from the server; loaded only while a history panel is open. */
export function useTemplateVersions(id: string | null) {
  return useQuery({
    queryKey: qk.templateVersions(id ?? ""),
    queryFn: () => list<TemplateVersion>(`/api/templates/${encodeURIComponent(id ?? "")}/versions`),
    enabled: id !== null && id !== "",
  });
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

/** Rows per `/api/usage` page; the cursor walks back from the last row of each page. */
export const USAGE_PAGE_SIZE = 100;

/** The pair cursor: both halves go back together so rows sharing a `ts` are not skipped. */
export type UsageCursor = { before: number; before_id: number };

/**
 * Filters the Logs page controls. All of them reach the server, so they compose with the
 * window and the cursor. They are part of the query key: any change restarts from page one.
 */
export type UsageFilters = {
  hours: number | null;
  provider?: string;
  model?: string;
  keyId?: string;
  status?: "2xx" | "error";
};

/**
 * The filter half of `/api/usage`'s query string. Shared by the log walk and the CSV export
 * so both select exactly the same rows; the cursor and limit stay with the walk.
 */
export function usageFilterQuery(filters: UsageFilters): URLSearchParams {
  const query = new URLSearchParams();
  if (filters.hours !== null) query.set("hours", String(filters.hours));
  if (filters.provider) query.set("provider", filters.provider);
  if (filters.model) query.set("model", filters.model);
  if (filters.keyId) query.set("key_id", filters.keyId);
  if (filters.status) query.set("status", filters.status);
  return query;
}

/** Keyset-paged request log, newest first. Load older until the server reports no cursor. */
export function useUsagePage(filters: UsageFilters) {
  return useInfiniteQuery({
    queryKey: qk.usagePages(filters),
    queryFn: ({ pageParam }) => {
      const query = usageFilterQuery(filters);
      query.set("limit", String(USAGE_PAGE_SIZE));
      if (pageParam) {
        query.set("before", String(pageParam.before));
        query.set("before_id", String(pageParam.before_id));
      }
      return listPage<UsageEvent>(`/api/usage?${query}`);
    },
    initialPageParam: null as UsageCursor | null,
    getNextPageParam: (last): UsageCursor | undefined =>
      last.next_before === null || last.next_before_id === null
        ? undefined
        : { before: last.next_before, before_id: last.next_before_id },
  });
}

/**
 * One request plus every attempt it dispatched. Loads only while a row is
 * selected; the caller refetches when that row's status changes, so a fallback
 * that lands later appears without polling.
 */
export function useUsageDetail(requestId: string | null) {
  return useQuery({
    queryKey: qk.usageDetail(requestId ?? ""),
    queryFn: async () => {
      const body = await api.get<{ data?: UsageDetail }>(
        `/api/usage/${encodeURIComponent(requestId ?? "")}`,
      );
      return body.data ?? null;
    },
    enabled: requestId !== null && requestId !== "",
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

// ---------------------------------------------------------------- dashboard identity

export function useAuthState() {
  return useQuery({
    queryKey: qk.authState,
    queryFn: authState,
    staleTime: 30_000,
    retry: 1,
  });
}

export function useSignOut() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: signOut,
    onSettled: () => {
      client.setQueryData(qk.authState, { initialized: true, setup_code_required: false, signup: "invite_only", user: null });
      void client.invalidateQueries({ queryKey: qk.authState });
    },
  });
}

export function useUsers(enabled: boolean) {
  return useQuery({
    queryKey: qk.users,
    queryFn: listUsers,
    enabled,
  });
}

export function useInvites(enabled: boolean) {
  return useQuery({
    queryKey: qk.invites,
    queryFn: listInvites,
    enabled,
  });
}

export function useUpdateUser() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: ({ id, patch }: { id: string; patch: Parameters<typeof updateUser>[1] }) =>
      updateUser(id, patch),
    onSuccess: () => void client.invalidateQueries({ queryKey: qk.users }),
  });
}

export function useRemoveUser() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: removeUser,
    onSuccess: () => void client.invalidateQueries({ queryKey: qk.users }),
  });
}

export function useCreateInvite() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: createInvite,
    onSuccess: () => void client.invalidateQueries({ queryKey: qk.invites }),
  });
}

export function useRevokeInvite() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: revokeInvite,
    onSuccess: () => void client.invalidateQueries({ queryKey: qk.invites }),
  });
}

// ---------------------------------------------------------------- audit and account

export function useAudit(kind: string) {
  return useQuery({ queryKey: qk.audit(kind), queryFn: () => listAudit(kind || undefined) });
}

export function useMe() {
  return useQuery({ queryKey: qk.me, queryFn: getMe });
}

export function useUpdateMe() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: updateMe,
    onSuccess: () => {
      void client.invalidateQueries({ queryKey: qk.me });
      void client.invalidateQueries({ queryKey: qk.authState });
    },
  });
}

export function useRevokeSession() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: revokeSession,
    onSuccess: () => void client.invalidateQueries({ queryKey: qk.me }),
  });
}

export function useRevokeOtherSessions() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: revokeOtherSessions,
    onSuccess: () => void client.invalidateQueries({ queryKey: qk.me }),
  });
}
