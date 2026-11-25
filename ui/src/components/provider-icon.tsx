import { useState } from "react";
import {
  siAnthropic,
  siGithub,
  siGoogle,
  siGooglegemini,
  siMistralai,
  siOllama,
  siOpenai,
  siOpenrouter,
  siX,
} from "simple-icons";

/** `path` is the 24×24 simple-icons glyph; absent brands fall back to their favicon. */
type Brand = {
  path?: string;
  /** 6-digit hex color kept for callers that append alpha suffixes (`${brand.color}66`). */
  color: string;
  /** Theme-aware CSS custom property (`var(--provider-*)`) for light & dark surfaces. */
  cssColor?: string;
  domain?: string;
};

/**
 * Terminal prompt mark (`>_`, 24×24) for Codex so it reads distinctly from OpenAI's
 * blossom in grouped provider lists and tables.
 */
const CODEX_PATH =
  "M2.5 7A1.414 1.414 0 0 1 4.5 5L10.5 11A1.414 1.414 0 0 1 10.5 13L4.5 19A1.414 1.414 0 0 1 2.5 17L7.5 12Z" +
  "M13.91 16.59H20.5A1.414 1.414 0 0 1 20.5 19.41H13.91A1.414 1.414 0 0 1 13.91 16.59Z";

/** Brand marks where simple-icons has one, and each provider's theme-aware token + site. */
export const PROVIDER_BRANDS: Record<string, Brand> = {
  anthropic: { path: siAnthropic.path, color: "#D97757", cssColor: "var(--provider-anthropic)", domain: "anthropic.com" },
  claude: { path: siAnthropic.path, color: "#D97757", cssColor: "var(--provider-claude)", domain: "claude.ai" },
  openai: { path: siOpenai.path, color: "#10A37F", cssColor: "var(--provider-openai)", domain: "openai.com" },
  // Distinct teal/cyan tone (`--sem-series-5`: #0e7490 light / #22d3ee dark) paired with
  // the prompt glyph so Codex never blends with OpenAI's emerald blossom.
  codex: { path: CODEX_PATH, color: "#0E7490", cssColor: "var(--sem-series-5)", domain: "openai.com" },
  opencode: { color: "#171C26", cssColor: "var(--provider-opencode)", domain: "opencode.ai" },
  "opencode-go": { color: "#E87040", cssColor: "var(--provider-opencode-go)", domain: "opencode.ai" },
  github: { path: siGithub.path, color: "#181717", cssColor: "var(--provider-github)", domain: "github.com" },
  gemini: { path: siGooglegemini.path, color: "#4285F4", cssColor: "var(--provider-gemini)", domain: "gemini.google.com" },
  // Antigravity is Google's agent surface on Cloud Code Assist. The Google mark (rather
  // than the Gemini sparkle) keeps it distinct from a Gemini API-key account; both aliases
  // resolve so a spec id and a short id render the same glyph.
  "google-antigravity": { path: siGoogle.path, color: "#4285F4", cssColor: "var(--provider-google-antigravity)", domain: "antigravity.google" },
  antigravity: { path: siGoogle.path, color: "#4285F4", cssColor: "var(--provider-antigravity)", domain: "antigravity.google" },
  openrouter: { path: siOpenrouter.path, color: "#6366F1", cssColor: "var(--provider-openrouter)", domain: "openrouter.ai" },
  xai: { path: siX.path, color: "#171C26", cssColor: "var(--provider-xai)", domain: "x.ai" },
  mistral: { path: siMistralai.path, color: "#FA520F", cssColor: "var(--provider-mistral)", domain: "mistral.ai" },
  ollama: { path: siOllama.path, color: "#171C26", cssColor: "var(--provider-ollama)", domain: "ollama.com" },
  deepseek: { color: "#4D6BFE", cssColor: "var(--provider-deepseek)", domain: "deepseek.com" },
  groq: { color: "#F55036", cssColor: "var(--provider-groq)", domain: "groq.com" },
  together: { color: "#0F6FFF", cssColor: "var(--provider-together)", domain: "together.ai" },
  cerebras: { color: "#F26B3A", cssColor: "var(--provider-cerebras)", domain: "cerebras.ai" },
};

export function providerBrand(provider: string): Brand {
  return PROVIDER_BRANDS[provider] ?? { color: "#5A667C", cssColor: "var(--color-muted)" };
}

const LOCAL_TLDS = [
  ".localhost",
  ".local",
  ".localdomain",
  ".internal",
  ".home.arpa",
  ".lan",
  ".intranet",
];

function isPrivateOrLocalHost(host: string): boolean {
  if (!host || host === "localhost") return true;
  if (LOCAL_TLDS.some((suffix) => host.endsWith(suffix))) return true;
  // IPv6 literals (`[::1]`, `[fe80::1]`, `[fd00::1]`, etc.) carry no public domain.
  if (host.startsWith("[") || host.includes(":")) return true;

  const ipv4 = /^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$/.exec(host);
  if (ipv4) {
    const a = Number(ipv4[1]);
    const b = Number(ipv4[2]);
    const c = Number(ipv4[3]);
    const d = Number(ipv4[4]);
    if (a > 255 || b > 255 || c > 255 || d > 255) return true;
    if (a === 0) return true; // 0.0.0.0/8 unspecified / current network
    if (a === 10) return true; // 10.0.0.0/8 RFC 1918
    if (a === 100 && b >= 64 && b <= 127) return true; // 100.64.0.0/10 CGNAT / Tailscale
    if (a === 127) return true; // 127.0.0.0/8 loopback
    if (a === 169 && b === 254) return true; // 169.254.0.0/16 link-local
    if (a === 172 && b >= 16 && b <= 31) return true; // 172.16.0.0/12 RFC 1918
    if (a === 192 && b === 0 && (c === 0 || c === 2)) return true; // 192.0.0.0/24, 192.0.2.0/24
    if (a === 192 && b === 168) return true; // 192.168.0.0/16 RFC 1918
    if (a === 198 && (b === 18 || b === 19)) return true; // 198.18.0.0/15 benchmarking
    if (a === 198 && b === 51 && c === 100) return true; // 198.51.100.0/24 TEST-NET-2
    if (a === 203 && b === 0 && c === 113) return true; // 203.0.113.0/24 TEST-NET-3
    if (a >= 224) return true; // multicast, reserved, broadcast
    return false;
  }

  // Single-label hosts (e.g. `ollama`, `vllm`, `litellm`) and malformed empty labels
  // cannot resolve to a public site favicon.
  const labels = host.split(".");
  if (labels.length < 2 || labels.some((label) => label.length === 0)) {
    return true;
  }

  return false;
}

/**
 * Extract a public hostname from a custom provider's `baseUrl` for favicon lookup.
 * Returns `null` for blank/unparseable URLs, non-HTTP(S) schemes, single-label hosts,
 * localhost, and private/reserved IP addresses.
 */
export function faviconHostFromBaseUrl(baseUrl?: string | null): string | null {
  if (!baseUrl) return null;
  const trimmed = baseUrl.trim();
  if (!trimmed || /\s/.test(trimmed)) return null;

  let candidate = trimmed;
  if (!/^[a-z][a-z0-9+.-]*:\/\//i.test(candidate)) {
    // Reject non-HTTP schemes (`javascript:`, `data:`, `mailto:`, `file:`, etc.)
    // while still allowing schemeless `host:port/path` values.
    if (
      /^[a-z][a-z0-9+.-]*:/i.test(candidate) &&
      !/^[a-z0-9.-]+:\d+(?:[/?#]|$)/i.test(candidate)
    ) {
      return null;
    }
    candidate = `https://${candidate}`;
  }

  let parsed: URL;
  try {
    parsed = new URL(candidate);
  } catch {
    return null;
  }

  if (parsed.protocol !== "http:" && parsed.protocol !== "https:") {
    return null;
  }

  const host = parsed.hostname.toLowerCase().replace(/\.+$/, "");
  if (!host || isPrivateOrLocalHost(host)) {
    return null;
  }

  return host;
}

export type ProviderIconProps = {
  provider: string;
  size?: number;
  className?: string;
  baseUrl?: string | null;
};

/**
 * Provider mark. The simple-icons `path` is inlined as a 24×24 `<svg>` filled with the
 * theme-aware `--provider-*` CSS token; providers without a packaged mark use their known
 * `brand.domain` or (for custom providers) a public host derived from `baseUrl` via Google's
 * favicon service, falling back to the provider's initial tile when unavailable or on load error.
 */
export function ProviderIcon({
  provider,
  size = 18,
  className,
  baseUrl,
}: ProviderIconProps) {
  const brand = providerBrand(provider);
  const fill = brand.cssColor ?? brand.color;
  const domain = brand.path ? null : (brand.domain ?? faviconHostFromBaseUrl(baseUrl));
  const faviconSrc = domain
    ? `https://www.google.com/s2/favicons?domain=${encodeURIComponent(domain)}&sz=64`
    : null;
  const [failedSrc, setFailedSrc] = useState<string | null>(null);

  if (brand.path) {
    // An inline path beats a CSS mask: `mask-image: url("…")` needs a real URL, so the
    // old SVG-string masks resolved to `none` and the icon painted as a coloured square.
    return (
      <svg
        aria-hidden
        className={className}
        width={size}
        height={size}
        viewBox="0 0 24 24"
        fill={fill}
        style={{ flexShrink: 0, display: "block" }}
      >
        <path d={brand.path} />
      </svg>
    );
  }
  if (faviconSrc && failedSrc !== faviconSrc) {
    return (
      <img
        src={faviconSrc}
        alt=""
        width={size}
        height={size}
        loading="lazy"
        referrerPolicy="no-referrer"
        className={className}
        style={{ width: size, height: size, objectFit: "contain", flexShrink: 0 }}
        onError={() => setFailedSrc(faviconSrc)}
      />
    );
  }
  return (
    <span
      aria-hidden
      className={className}
      style={{
        display: "inline-flex",
        width: size,
        height: size,
        alignItems: "center",
        justifyContent: "center",
        borderRadius: "var(--radius-control, 4px)",
        backgroundColor: "var(--color-accent)",
        color: "var(--color-accent-ink)",
        fontSize: Math.max(9, Math.round(size * 0.55)),
        fontWeight: 700,
        flexShrink: 0,
      }}
    >
      {provider.slice(0, 1).toUpperCase()}
    </span>
  );
}
