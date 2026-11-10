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
  color: string;
  domain?: string;
};

/** Brand marks where simple-icons has one, and each provider's own colour + site. */
export const PROVIDER_BRANDS: Record<string, Brand> = {
  anthropic: { path: siAnthropic.path, color: "#D97757", domain: "anthropic.com" },
  claude: { path: siAnthropic.path, color: "#D97757", domain: "claude.ai" },
  openai: { path: siOpenai.path, color: "#10A37F", domain: "openai.com" },
  codex: { path: siOpenai.path, color: "#10A37F", domain: "openai.com" },
  opencode: { color: "#111111", domain: "opencode.ai" },
  "opencode-go": { color: "#E87040", domain: "opencode.ai" },
  github: { path: siGithub.path, color: "#181717", domain: "github.com" },
  gemini: { path: siGooglegemini.path, color: "#4285F4", domain: "gemini.google.com" },
  // Antigravity is Google's agent surface on Cloud Code Assist. The Google mark (rather
  // than the Gemini sparkle) keeps it distinct from a Gemini API-key account; both aliases
  // resolve so a spec id and a short id render the same glyph.
  "google-antigravity": { path: siGoogle.path, color: "#4285F4", domain: "antigravity.google" },
  antigravity: { path: siGoogle.path, color: "#4285F4", domain: "antigravity.google" },
  openrouter: { path: siOpenrouter.path, color: "#6366F1", domain: "openrouter.ai" },
  xai: { path: siX.path, color: "#111111", domain: "x.ai" },
  mistral: { path: siMistralai.path, color: "#FA520F", domain: "mistral.ai" },
  ollama: { path: siOllama.path, color: "#111111", domain: "ollama.com" },
  deepseek: { color: "#4D6BFE", domain: "deepseek.com" },
  groq: { color: "#F55036", domain: "groq.com" },
  together: { color: "#0F6FFF", domain: "together.ai" },
  cerebras: { color: "#F26B3A", domain: "cerebras.ai" },
};

export function providerBrand(provider: string): Brand {
  return PROVIDER_BRANDS[provider] ?? { color: "#626F86" };
}

/**
 * Provider mark. The simple-icons `path` is inlined as a 24×24 `<svg>` filled with the
 * brand colour; providers without a packaged mark use their site favicon instead of an
 * invented glyph, and the final fallback is the provider's initial.
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
        fill={brand.color}
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
        borderRadius: 4,
        backgroundColor: brand.color,
        color: "#fff",
        fontSize: Math.max(9, Math.round(size * 0.55)),
        fontWeight: 700,
        flexShrink: 0,
      }}
    >
      {provider.slice(0, 1).toUpperCase()}
    </span>
  );
}
