import anthropic from "simple-icons/icons/anthropic.svg";
import openai from "simple-icons/icons/openai.svg";
import github from "simple-icons/icons/github.svg";
import gemini from "simple-icons/icons/googlegemini.svg";
import openrouter from "simple-icons/icons/openrouter.svg";
import x from "simple-icons/icons/x.svg";
import mistral from "simple-icons/icons/mistralai.svg";
import ollama from "simple-icons/icons/ollama.svg";

type Brand = {
  /** Inline SVG mask from simple-icons; absent brands fall back to their favicon. */
  icon?: string;
  color: string;
  domain?: string;
};

/** Brand marks where simple-icons has one, and each provider's own colour + site. */
export const PROVIDER_BRANDS: Record<string, Brand> = {
  anthropic: { icon: anthropic, color: "#D97757", domain: "anthropic.com" },
  claude: { icon: anthropic, color: "#D97757", domain: "claude.ai" },
  openai: { icon: openai, color: "#10A37F", domain: "openai.com" },
  codex: { icon: openai, color: "#10A37F", domain: "openai.com" },
  opencode: { color: "#111111", domain: "opencode.ai" },
  github: { icon: github, color: "#181717", domain: "github.com" },
  gemini: { icon: gemini, color: "#4285F4", domain: "gemini.google.com" },
  openrouter: { icon: openrouter, color: "#6366F1", domain: "openrouter.ai" },
  xai: { icon: x, color: "#111111", domain: "x.ai" },
  mistral: { icon: mistral, color: "#FA520F", domain: "mistral.ai" },
  ollama: { icon: ollama, color: "#111111", domain: "ollama.com" },
  deepseek: { color: "#4D6BFE", domain: "deepseek.com" },
  groq: { color: "#F55036", domain: "groq.com" },
  together: { color: "#0F6FFF", domain: "together.ai" },
  cerebras: { color: "#F26B3A", domain: "cerebras.ai" },
};

export function providerBrand(provider: string): Brand {
  return PROVIDER_BRANDS[provider] ?? { color: "#626F86" };
}

/**
 * Provider mark. Simple-icons paths are rendered as a CSS mask so the brand colour themes
 * them; providers without a packaged mark use their site favicon instead of an invented
 * glyph, and the final fallback is the provider's initial.
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
  if (brand.icon) {
    return (
      <span
        aria-hidden
        className={className}
        style={{
          display: "inline-block",
          width: size,
          height: size,
          flexShrink: 0,
          backgroundColor: brand.color,
          WebkitMaskImage: `url(${brand.icon})`,
          maskImage: `url(${brand.icon})`,
          WebkitMaskSize: "contain",
          maskSize: "contain",
          WebkitMaskRepeat: "no-repeat",
          maskRepeat: "no-repeat",
          WebkitMaskPosition: "center",
          maskPosition: "center",
        }}
      />
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
