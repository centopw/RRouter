import { useInfiniteQuery, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  adminToken,
  api,
  clearAccountQuota,
  getAccountSubscriptions,
  getAccountQuotas,
  getQuotaPings,
  getUpdateStatus,
  isEndpointMissing,
  list,
  listPage,
  updateAccountQuota,
  updateQuotaPing,
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
  type Webhook,
  type LayoutDocument,
  type ModelPreset,
  type PresetIndex,
  type WebhookList,
  type WebhookStatus,
  type EvalRun,
  type EvalRunDetail,
  type EvalSuite,
  type EvalSuiteDetail,
  type Team,
  type TeamMember,
  type Template,
  type TemplateVersion,
  type UpdateTeam,
  type UpdateConnection,
  type UpdateAccountQuota,
  type UpdateKey,
  type UpdateQuotaPing,
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
  quotaPings: ["quota-pings"] as const,
  authState: ["auth", "state"] as const,
  me: ["auth", "me"] as const,
  audit: (kind: string) => ["audit", kind] as const,
  users: ["users"] as const,
  invites: ["users", "invites"] as const,
  keys: ["keys"] as const,
  teams: ["teams"] as const,
  evals: ["evals"] as const,
  /** Index and every layout document share this prefix so one invalidation covers both. */
  modelPresets: ["model-presets"] as const,
  modelPreset: (id: string) => ["model-presets", id] as const,
  webhooks: ["webhooks"] as const,
  webhookStatus: ["webhooks", "status"] as const,
  evalSuite: (id: string) => ["evals", "suite", id] as const,
  evalRun: (id: string) => ["evals", "run", id] as const,
  teamMembers: (id: string) => ["teams", id, "members"] as const,
  teamKeys: (id: string) => ["teams", id, "keys"] as const,
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

export function useSystem(enabled = true) {
  return useQuery({
    queryKey: qk.system,
    queryFn: () => api.get<SystemInfo>("/api/system"),
    enabled,
  });
}

/** Accounts the caller may see, optionally narrowed to one team (`?team_id=`). */
export function useConnections(enabled = true, teamId?: string) {
  return useQuery({
    queryKey: [...qk.connections, teamId ?? ""] as const,
    queryFn: () =>
      list<Connection>(teamId ? `/api/connections?team_id=${encodeURIComponent(teamId)}` : "/api/connections"),
    enabled,
  });
}

export function useAccountSubscriptions(enabled = true) {
  return useQuery({
    queryKey: qk.accountSubscriptions,
    queryFn: getAccountSubscriptions,
    // Provider usage endpoints are shared with the vendor's own client; a minute is
    // frequent enough for a dashboard and polite to the upstream.
    refetchInterval: 300_000,
    staleTime: 270_000,
    enabled,
  });
}

export function useAccountQuotas(enabled = true) {
  return useQuery({ queryKey: qk.accountQuotas, queryFn: getAccountQuotas, enabled });
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

/** Auto-ping schedules for Claude/Codex connections; absent on older builds. */
export function useQuotaPings(enabled = true) {
  return useQuery({
    queryKey: qk.quotaPings,
    queryFn: getQuotaPings,
    enabled,
    // Next ping moves with provider resets; a minute keeps the badge honest.
    refetchInterval: 60_000,
    staleTime: 30_000,
    // A 404/405 is a build without the feature, not a transient failure worth retrying.
    retry: (count, error) => !isEndpointMissing(error) && count < 1,
  });
}

export function useUpdateQuotaPing() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: ({ id, patch }: { id: string; patch: UpdateQuotaPing }) =>
      updateQuotaPing(id, patch),
    onSuccess: () => void client.invalidateQueries({ queryKey: qk.quotaPings }),
  });
}

export function useKeys(enabled = true) {
  return useQuery({
    queryKey: qk.keys,
    queryFn: () => list<KeyResult>("/api/keys"),
    enabled,
  });
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


// --------------------------------------------------------------- model presets
//
// The library is global; which preset an account is looking at is a session preference, so
// activation is its own call and the list's `active_id` is already per-caller.

export function usePresetIndex(enabled = true) {
  return useQuery({
    queryKey: qk.modelPresets,
    queryFn: () => api.get<PresetIndex>("/api/model-presets"),
    enabled,
    retry: false,
  });
}

/** One preset including its layout and the revision its next save must carry. */
export function usePresetLayout(id: string | null) {
  return useQuery({
    queryKey: qk.modelPreset(id ?? "none"),
    queryFn: () => api.get<ModelPreset>(`/api/model-presets/${encodeURIComponent(id ?? "")}`),
    enabled: Boolean(id),
    retry: false,
    staleTime: 5_000,
  });
}

/** This account's choice. A caller with no session gets the default back, unpersisted. */
export function useActivatePreset() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (id: string) =>
      api.post<{ active_id: string; persisted: boolean }>(
        `/api/model-presets/${encodeURIComponent(id)}/activate`,
      ),
    onSuccess: () => void client.invalidateQueries({ queryKey: qk.modelPresets }),
  });
}

export function useCreatePreset() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (input: { name: string; description?: string; layout?: LayoutDocument }) =>
      api.post<ModelPreset>("/api/model-presets", input),
    onSuccess: () => void client.invalidateQueries({ queryKey: qk.modelPresets }),
  });
}

export function useDuplicatePreset() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: ({ id, name }: { id: string; name: string }) =>
      api.post<ModelPreset>(`/api/model-presets/${encodeURIComponent(id)}/duplicate`, { name }),
    onSuccess: () => void client.invalidateQueries({ queryKey: qk.modelPresets }),
  });
}

/** Name and description only: neither touches the layout or its revision. */
export function usePatchPreset() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: ({
      id,
      patch,
    }: {
      id: string;
      patch: { name?: string; description?: string | null };
    }) => api.patch<ModelPreset>(`/api/model-presets/${encodeURIComponent(id)}`, patch),
    onSuccess: () => void client.invalidateQueries({ queryKey: qk.modelPresets }),
  });
}

/** Pending endpoint: the bar disables the control once this answers 404/405. */
export function useMakeDefaultPreset() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (id: string) =>
      api.post<ModelPreset>(`/api/model-presets/${encodeURIComponent(id)}/default`),
    onSuccess: () => {
      void client.invalidateQueries({ queryKey: qk.modelPresets });
      void client.invalidateQueries({ queryKey: qk.modelPreset("none") });
    },
  });
}

export function useDeletePreset() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (id: string) =>
      api.del<{ deleted: boolean }>(`/api/model-presets/${encodeURIComponent(id)}`),
    onSuccess: () => void client.invalidateQueries({ queryKey: qk.modelPresets }),
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

export function useUsage(limit = 1000, hours?: number, teamId?: string) {
  const query = new URLSearchParams({ limit: String(limit) });
  if (hours !== undefined) query.set("hours", String(hours));
  if (teamId) query.set("team_id", teamId);
  return useQuery({
    queryKey: [...qk.usage(limit), hours ?? "all", teamId ?? ""] as const,
    queryFn: () => list<UsageEvent>(`/api/usage?${query}`),
    refetchInterval: 15_000,
  });
}

/** Newest N rows, polled fast for live routing. Kept separate from the history query. */
export function useLiveUsage(limit = 200, teamId?: string) {
  const query = new URLSearchParams({ limit: String(limit) });
  if (teamId) query.set("team_id", teamId);
  return useQuery({
    queryKey: ["usage-live", limit, teamId ?? ""] as const,
    queryFn: () => list<UsageEvent>(`/api/usage?${query}`),
    refetchInterval: 5_000,
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
  teamId?: string;
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
  if (filters.teamId) query.set("team_id", filters.teamId);
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
export function useUsageSeries(hours: number, bucket?: string, teamId?: string) {
  const query = new URLSearchParams({ hours: String(hours) });
  if (bucket) query.set("bucket", bucket);
  if (teamId) query.set("team_id", teamId);
  return useQuery({
    queryKey: [...qk.series(hours, bucket), teamId ?? ""] as const,
    queryFn: () => list<UsageBucket>(`/api/usage/series?${query}`),
    refetchInterval: 30_000,
  });
}

/** The same series split by model, for the stacked traffic chart. */
export function useUsageByModel(hours: number, bucket?: string, teamId?: string) {
  const query = new URLSearchParams({ hours: String(hours), by: "model" });
  if (bucket) query.set("bucket", bucket);
  if (teamId) query.set("team_id", teamId);
  return useQuery({
    queryKey: [...qk.series(hours, bucket, "model"), teamId ?? ""] as const,
    queryFn: () => list<UsageModelBucket>(`/api/usage/series?${query}`),
    refetchInterval: 30_000,
  });
}

/** Per-model totals, scoped to the last `hours` when given (the Activity table). */
export function useUsageSummary(hours: number | null = null, teamId?: string) {
  const query = new URLSearchParams();
  if (hours !== null) query.set("hours", String(hours));
  if (teamId) query.set("team_id", teamId);
  const suffix = query.toString();
  return useQuery({
    queryKey: [...qk.summary(hours), teamId ?? ""] as const,
    queryFn: () =>
      list<UsageSummaryRow>(`/api/usage/summary${suffix === "" ? "" : `?${suffix}`}`),
    refetchInterval: 15_000,
  });
}

export function useOAuthProviders(enabled = true) {
  return useQuery({
    queryKey: qk.oauthProviders,
    queryFn: () => list<OAuthProvider>("/api/oauth/providers"),
    staleTime: 60_000,
    enabled,
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


// ------------------------------------------------------------------------ teams

/** Teams visible to this caller: every team for an admin, their own for a reader. */
export function useTeams() {
  return useQuery({ queryKey: qk.teams, queryFn: () => list<Team>("/api/teams") });
}

export function useCreateTeam() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (input: UpdateTeam & { name: string }) => api.post<Team>("/api/teams", input),
    onSuccess: () => void client.invalidateQueries({ queryKey: qk.teams }),
  });
}

export function useUpdateTeam() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: ({ id, patch }: { id: string; patch: UpdateTeam }) =>
      api.patch<Team>(`/api/teams/${encodeURIComponent(id)}`, patch),
    onSuccess: () => void client.invalidateQueries({ queryKey: qk.teams }),
  });
}

export function useDeleteTeam() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (id: string) =>
      api.del<{ deleted: boolean }>(`/api/teams/${encodeURIComponent(id)}`),
    onSuccess: () => {
      void client.invalidateQueries({ queryKey: qk.teams });
      // Keys are released, not deleted: the list's team badges changed.
      void client.invalidateQueries({ queryKey: qk.keys });
    },
  });
}

export function useTeamMembers(id: string | null) {
  return useQuery({
    queryKey: qk.teamMembers(id ?? ""),
    queryFn: () => list<TeamMember>(`/api/teams/${encodeURIComponent(id ?? "")}/members`),
    enabled: id !== null,
  });
}

/**
 * One team's key rows. Only an admin or that team's `team_admin` may ask; the server
 * refuses everyone else, so the caller gates on the role it already knows.
 */
export function useTeamKeys(id: string | null, enabled = true) {
  return useQuery({
    queryKey: qk.teamKeys(id ?? ""),
    queryFn: () => list<KeyResult>(`/api/teams/${encodeURIComponent(id ?? "")}/keys`),
    enabled: enabled && id !== null,
  });
}

export function useAddTeamMember() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: ({ id, email, role }: { id: string; email: string; role: string }) =>
      api.post<TeamMember>(`/api/teams/${encodeURIComponent(id)}/members`, { email, role }),
    onSuccess: (_member, { id }) => {
      void client.invalidateQueries({ queryKey: qk.teams });
      // The roster is its own query: a refreshed member count without the list is a lie.
      void client.invalidateQueries({ queryKey: qk.teamMembers(id) });
    },
  });
}

export function useRemoveTeamMember() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: ({ id, userId }: { id: string; userId: string }) =>
      api.del<{ removed: boolean }>(
        `/api/teams/${encodeURIComponent(id)}/members/${encodeURIComponent(userId)}`,
      ),
    onSuccess: (_result, { id }) => {
      void client.invalidateQueries({ queryKey: qk.teams });
      void client.invalidateQueries({ queryKey: qk.teamMembers(id) });
    },
  });
}



// ------------------------------------------------------------------------ evals

/** Every suite with its case count. */
export function useEvalSuites() {
  return useQuery({ queryKey: qk.evals, queryFn: () => list<EvalSuite>("/api/evals/suites") });
}

/** One suite with its cases and recent runs. */
export function useEvalSuite(id: string | null) {
  return useQuery({
    queryKey: qk.evalSuite(id ?? ""),
    queryFn: () => api.get<EvalSuiteDetail>(`/api/evals/suites/${encodeURIComponent(id ?? "")}`),
    enabled: id !== null,
  });
}

/**
 * One run. The runner is asynchronous, so a pending or running run is polled once a
 * second; a finished one stops polling on its own.
 */
export function useEvalRun(id: string | null) {
  return useQuery({
    queryKey: qk.evalRun(id ?? ""),
    queryFn: () => api.get<EvalRunDetail>(`/api/evals/runs/${encodeURIComponent(id ?? "")}`),
    enabled: id !== null,
    refetchInterval: (query) => {
      const status = query.state.data?.data.status;
      return status === "pending" || status === "running" ? 1_000 : false;
    },
  });
}

function invalidateEvals(client: ReturnType<typeof useQueryClient>, suiteId?: string) {
  void client.invalidateQueries({ queryKey: qk.evals });
  if (suiteId) void client.invalidateQueries({ queryKey: qk.evalSuite(suiteId) });
}

export function useCreateEvalSuite() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (input: { name: string; description?: string; system_prompt?: string }) =>
      api.post<EvalSuite>("/api/evals/suites", input),
    onSuccess: () => invalidateEvals(client),
  });
}

export function useUpdateEvalSuite() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: ({
      id,
      patch,
    }: {
      id: string;
      patch: { name?: string; description?: string | null; system_prompt?: string | null };
    }) => api.patch<EvalSuite>(`/api/evals/suites/${encodeURIComponent(id)}`, patch),
    onSuccess: (_body, { id }) => invalidateEvals(client, id),
  });
}

export function useDeleteEvalSuite() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (id: string) =>
      api.del<{ deleted: boolean }>(`/api/evals/suites/${encodeURIComponent(id)}`),
    onSuccess: () => invalidateEvals(client),
  });
}

export function useCreateEvalCase() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: ({ suiteId, input, expected, name }: { suiteId: string; input: string; expected?: string; name?: string }) =>
      api.post<{ data: unknown }>(`/api/evals/suites/${encodeURIComponent(suiteId)}/cases`, {
        input,
        expected,
        name,
      }),
    onSuccess: (_body, { suiteId }) => invalidateEvals(client, suiteId),
  });
}

export function useDeleteEvalCase() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: ({ suiteId, caseId }: { suiteId: string; caseId: string }) =>
      api.del<{ deleted: boolean }>(
        `/api/evals/suites/${encodeURIComponent(suiteId)}/cases/${encodeURIComponent(caseId)}`,
      ),
    onSuccess: (_body, { suiteId }) => invalidateEvals(client, suiteId),
  });
}

/** Start a run. Returns the pending run; poll `useEvalRun` for its progress. */
export function useStartEvalRun() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: ({
      suiteId,
      model,
      judgeModel,
      apiKey,
    }: {
      suiteId: string;
      model: string;
      judgeModel?: string;
      apiKey?: string;
    }) =>
      api.post<{ data: EvalRun }>("/api/evals/runs", {
        suite_id: suiteId,
        model,
        ...(judgeModel ? { judge_model: judgeModel } : {}),
        ...(apiKey ? { api_key: apiKey } : {}),
      }),
    onSuccess: (_body, { suiteId }) => invalidateEvals(client, suiteId),
  });
}


// --------------------------------------------------------------------- webhooks

/** Every delivery endpoint, plus the event vocabulary the server accepts. */
export function useWebhooks(enabled = true) {
  return useQuery({
    queryKey: qk.webhooks,
    queryFn: () => api.get<WebhookList>("/api/webhooks"),
    enabled,
  });
}

/** Outbox depth and recent attempts. Manual refresh; nothing here polls by itself. */
export function useWebhookStatus(enabled = true) {
  return useQuery({
    queryKey: qk.webhookStatus,
    queryFn: () => api.get<WebhookStatus>("/api/webhooks/status"),
    enabled,
  });
}

export function useCreateWebhook() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (input: { url: string; event_types: string[]; secret_env: string }) =>
      api.post<{ webhook: Webhook }>("/api/webhooks", input),
    onSuccess: () => void client.invalidateQueries({ queryKey: qk.webhooks }),
  });
}

/** Present fields are applied; `secret_env` cannot be cleared, only replaced. */
export function useUpdateWebhook() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: ({
      id,
      patch,
    }: {
      id: string;
      patch: Partial<{ url: string; event_types: string[]; secret_env: string; enabled: boolean }>;
    }) => api.patch<{ webhook: Webhook }>(`/api/webhooks/${encodeURIComponent(id)}`, patch),
    onSuccess: () => void client.invalidateQueries({ queryKey: qk.webhooks }),
  });
}

export function useDeleteWebhook() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (id: string) =>
      api.del<{ deleted: boolean }>(`/api/webhooks/${encodeURIComponent(id)}`),
    onSuccess: () => void client.invalidateQueries({ queryKey: qk.webhooks }),
  });
}

// ---------------------------------------------------------------- dashboard identity

/** Update metadata for the sidebar marker; admin-only because the route is. */
export function useUpdateStatus(enabled = true) {
  return useQuery({
    queryKey: ["update"] as const,
    queryFn: getUpdateStatus,
    enabled,
    staleTime: 5 * 60_000,
    refetchInterval: 5 * 60_000,
    retry: false,
  });
}

export function useAuthState() {
  return useQuery({
    queryKey: qk.authState,
    queryFn: authState,
    staleTime: 30_000,
    retry: 1,
  });
}

/**
 * May this tab use the admin surface? A break-glass admin token counts: the API accepts
 * it exactly when the shell is allowed to render without a user session.
 *
 * The server is the authority — this only decides what to paint, so a bug here shows a
 * page that then refuses, never a privilege.
 */
export function useIsAdmin(): boolean {
  const state = useAuthState();
  return state.data?.user?.role === "admin" || adminToken() !== null;
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
