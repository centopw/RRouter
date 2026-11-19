import { useQuery } from "@tanstack/react-query";
import { api } from "../../lib/api";

export type CompressionLevel = "lite" | "full" | "ultra";

/** Which compression methods run on the input and output sides of a request. */
export type CompressionSettings = {
  /** Compress noisy tool results before they reach the provider. */
  rtk?: boolean;
  /** Inject a terse-output style instruction into the system prompt. */
  caveman?: boolean;
  caveman_level?: CompressionLevel;
};

/** Gateway settings stored through `/api/settings` (one flat object). */
export type Brand = {
  brand_name?: string;
  brand_color?: string;
  logo_url?: string;
  footer_text?: string;
  compression?: CompressionSettings;
};

export const GATEWAY_SETTINGS_KEY = ["gateway-settings"] as const;

/** One cached read of `/api/settings`, shared by the General, Runtime and Compression pages. */
export function useGatewaySettings() {
  return useQuery({
    queryKey: GATEWAY_SETTINGS_KEY,
    queryFn: () => api.get<Brand>("/api/settings"),
    staleTime: 60_000,
  });
}
