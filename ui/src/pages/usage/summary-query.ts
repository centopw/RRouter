/**
 * `/usage`'s one summary fetch: `GET /api/usage/summary?group_by=connection` returns the
 * per-(connection, provider, model) rows in `data` and, since #13, an additive top-level
 * `unrouted` breakdown of requests rejected before dispatch. The shared
 * `useUsageSummaryByConnection` unwraps `data` only, so this page keeps the whole envelope
 * under its own cache entry (still under the `usage-summary-connection` prefix, so any
 * invalidation of that key refreshes it too).
 */

import { keepPreviousData, useQuery } from "@tanstack/react-query";
import { api, type UsageSummaryByConnection } from "../../lib/api";
import { normalizeUnroutedBreakdown, type UnroutedApiBreakdown } from "./unrouted-model";

export type UsageSummaryWithUnrouted = {
  rows: UsageSummaryByConnection[];
  /** `null` until the gateway reports the breakdown (older builds omit the key). */
  unrouted: UnroutedApiBreakdown | null;
};

export function useUsageSummaryWithUnrouted(hours: number | null) {
  const query = new URLSearchParams({ group_by: "connection" });
  if (hours !== null) query.set("hours", String(hours));
  return useQuery({
    queryKey: ["usage-summary-connection", hours ?? "all", "", "with-unrouted"] as const,
    queryFn: async (): Promise<UsageSummaryWithUnrouted> => {
      const body = await api.get<{ data?: UsageSummaryByConnection[]; unrouted?: unknown }>(
        `/api/usage/summary?${query}`,
      );
      return {
        rows: body?.data ?? [],
        unrouted: normalizeUnroutedBreakdown(body?.unrouted),
      };
    },
    refetchInterval: 15_000,
    placeholderData: keepPreviousData,
  });
}
