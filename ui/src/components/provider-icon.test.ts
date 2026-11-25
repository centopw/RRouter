/**
 * Tests for `ProviderIcon` baseUrl favicon host resolution and fallback rendering.
 *
 * Run with `node --test src/components/provider-icon.test.ts` from `ui/`.
 */
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { registerHooks } from "node:module";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { transformWithOxc } from "vite";

const iconUrl = new URL("./provider-icon.tsx", import.meta.url);
const iconPath = fileURLToPath(iconUrl);
const transformed = await transformWithOxc(readFileSync(iconPath, "utf8"), iconPath, {
  lang: "tsx",
  jsx: { runtime: "automatic" },
});

registerHooks({
  load(url, context, nextLoad) {
    if (url === iconUrl.href) {
      return { format: "module", source: transformed.code, shortCircuit: true };
    }
    return nextLoad(url, context);
  },
});

const { ProviderIcon, faviconHostFromBaseUrl } = await import("./provider-icon.tsx");

test("faviconHostFromBaseUrl extracts public hostnames from valid URLs", () => {
  assert.equal(
    faviconHostFromBaseUrl("https://api.fireworks.ai/inference/v1"),
    "api.fireworks.ai",
  );
  assert.equal(
    faviconHostFromBaseUrl("http://GATEWAY.Example.com:8443/v1"),
    "gateway.example.com",
  );
  assert.equal(
    faviconHostFromBaseUrl("https://user:pass@api.moonshot.cn/v1"),
    "api.moonshot.cn",
  );
  assert.equal(
    faviconHostFromBaseUrl("api.together.xyz/v1"),
    "api.together.xyz",
  );
  assert.equal(
    faviconHostFromBaseUrl("https://example.com."),
    "example.com",
  );
});

test("faviconHostFromBaseUrl rejects blank, unparseable, non-http, localhost, and private hosts", () => {
  for (const invalid of [
    undefined,
    null,
    "",
    "   ",
    "not a url",
    "://broken",
    "https://",
    "https://<resource>.openai.azure.com",
    "ftp://example.com/v1",
    "javascript:alert(1)",
    "data:text/html,hi",
    "http://localhost:11434/v1",
    "https://api.localhost/v1",
    "http://127.0.0.1:8080/v1",
    "http://127.1:8080/v1",
    "http://0.0.0.0:8000/v1",
    "http://10.0.0.5:8000/v1",
    "http://172.16.0.1/v1",
    "http://172.31.255.254/v1",
    "http://192.168.1.50:11434/v1",
    "http://100.64.0.1:8080/v1",
    "http://169.254.169.254/latest",
    "http://[::1]:8080/v1",
    "http://[fe80::1]:8080/v1",
    "http://[fd00::1]:8080/v1",
    "http://ollama:11434/v1",
    "http://box.local:8000/v1",
    "http://llm.internal/v1",
    "http://gateway.home.arpa/v1",
  ]) {
    assert.equal(faviconHostFromBaseUrl(invalid), null, `expected null for ${String(invalid)}`);
  }
});

test("ProviderIcon prefers brand path, then brand domain, then baseUrl favicon, then initial tile", () => {
  // Known simple-icons brand still renders SVG even if baseUrl is passed.
  const openaiHtml = renderToStaticMarkup(
    createElement(ProviderIcon, { provider: "openai", baseUrl: "https://proxy.example.com/v1" }),
  );
  assert.match(openaiHtml, /^<svg\b/);

  // Known brand.domain keeps its own domain favicon over baseUrl.
  const deepseekHtml = renderToStaticMarkup(
    createElement(ProviderIcon, { provider: "deepseek", baseUrl: "https://proxy.example.com/v1" }),
  );
  assert.match(
    deepseekHtml,
    /src="https:\/\/www\.google\.com\/s2\/favicons\?domain=deepseek\.com&amp;sz=64"/,
  );

  // Unknown/custom provider with public baseUrl loads Google favicon for derived host.
  const customHtml = renderToStaticMarkup(
    createElement(ProviderIcon, {
      provider: "fireworks",
      baseUrl: "https://api.fireworks.ai/inference/v1",
    }),
  );
  assert.match(
    customHtml,
    /^<img\b.*src="https:\/\/www\.google\.com\/s2\/favicons\?domain=api\.fireworks\.ai&amp;sz=64"/,
  );

  // Unknown/custom provider with localhost/private/blank baseUrl falls back to initial tile.
  for (const baseUrl of [undefined, "", "http://localhost:11434/v1", "http://192.168.1.2:8000"]) {
    const fallbackHtml = renderToStaticMarkup(
      createElement(ProviderIcon, { provider: "custom", baseUrl }),
    );
    assert.match(fallbackHtml, /^<span\b[^>]*>C<\/span>$/);
  }
});
