import { useQuery } from "@tanstack/react-query";
import { api } from "../../lib/api";

/** White-label brand fields stored through `/api/settings` (flat object). */
export type Brand = {
  brand_name?: string;
  brand_color?: string;
  logo_url?: string;
  footer_text?: string;
  /** Token Saver: compress noisy tool outputs before they reach the provider. */
  rtk?: boolean;
};

export const GATEWAY_SETTINGS_KEY = ["gateway-settings"] as const;

/** One cached read of `/api/settings`, shared by the General and Runtime tabs. */
export function useGatewaySettings() {
  return useQuery({
    queryKey: GATEWAY_SETTINGS_KEY,
    queryFn: () => api.get<Brand>("/api/settings"),
    staleTime: 60_000,
  });
}
