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

/** Brand marks where simple-icons has one, and each provider's theme-aware token + site. */
export const PROVIDER_BRANDS: Record<string, Brand> = {
  anthropic: { path: siAnthropic.path, color: "#D97757", cssColor: "var(--provider-anthropic)", domain: "anthropic.com" },
  claude: { path: siAnthropic.path, color: "#D97757", cssColor: "var(--provider-claude)", domain: "claude.ai" },
  openai: { path: siOpenai.path, color: "#10A37F", cssColor: "var(--provider-openai)", domain: "openai.com" },
  codex: { path: siOpenai.path, color: "#10A37F", cssColor: "var(--provider-codex)", domain: "openai.com" },
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

/**
 * Provider mark. The simple-icons `path` is inlined as a 24×24 `<svg>` filled with the
 * theme-aware `--provider-*` CSS token; providers without a packaged mark use their site
 * favicon instead of an invented glyph, and the final fallback is the provider's initial.
 */
export function ProviderIcon({
  provider,
  size = 18,
  className,
}: {
  provider: string;
  size?: number;
  className?: string;
}) {
  const brand = providerBrand(provider);
  const fill = brand.cssColor ?? brand.color;
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
  if (brand.domain) {
    return (
      <img
        src={`https://www.google.com/s2/favicons?domain=${brand.domain}&sz=64`}
        alt=""
        width={size}
        height={size}
        loading="lazy"
        referrerPolicy="no-referrer"
        className={className}
        style={{ width: size, height: size, objectFit: "contain", flexShrink: 0 }}
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
