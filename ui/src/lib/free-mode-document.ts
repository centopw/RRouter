/**
 * The advanced free-mode JSON document: its shape, local schema validation and example.
 *
 * Kept free of network imports so Node's test runner can load it directly (the UI has no
 * bundler in its test path). The gateway validates the same document authoritatively — this
 * is the subset that can be checked before a request is sent at all.
 */

/** Routing knobs stored on the server alongside an optional imported document. */
export type FreeRouting = {
  enabled: boolean;
  fallback_on_quota?: boolean;
  /** Proactive cap per account, requests per minute; `null` means no local cap. */
  per_account_rpm: number | null;
  max_attempts: number;
  prefer: "spread" | "cheapest";
  /** Model ids the operator picked explicitly in a legacy document. */
  models?: string[];
  /** `"provider/model"` keys disabled via the per-model switches. */
  disabled_models?: string[];
};

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

// ---------------------------------------------------------------- validation

/** One validation problem, named the way the gateway names it: `path: message`. */
export type FreeDocumentProblem = { path: string; message: string };

export type FreeDocumentCheck = {
  document: Record<string, unknown> | null;
  problems: FreeDocumentProblem[];
  /** How many providers and accounts the document declares, for the Validate line. */
  providers: number;
  accounts: number;
};

/** A full proxy URL: `scheme://[user:pass@]host[:port]`, scheme in the four the gateway dials. */
const PROXY_URL = /^(https?|socks5h?):\/\/[^\s/]+$/i;

/**
 * Client-side half of validation: JSON syntax and the rules stated in the document schema.
 *
 * It is deliberately a subset — the gateway validates the whole document atomically and
 * names every problem it finds, and that answer is shown as-is. Anything the server owns
 * (whether a provider exists, a `custom` id collision) is not guessed at here.
 */
export function validateFreeDocument(text: string): FreeDocumentCheck {
  const problems: FreeDocumentProblem[] = [];
  const push = (path: string, message: string) => problems.push({ path, message });
  const empty: FreeDocumentCheck = { document: null, problems, providers: 0, accounts: 0 };

  if (text.trim() === "") {
    push("document", "paste a document or drop a .json file");
    return empty;
  }

  let parsed: unknown;
  try {
    parsed = JSON.parse(text);
  } catch (error) {
    push("document", `not valid JSON: ${error instanceof Error ? error.message : String(error)}`);
    return empty;
  }
  if (!isObject(parsed)) {
    push("document", "the document must be a JSON object");
    return empty;
  }

  if (parsed.version !== undefined && parsed.version !== 1) {
    push("version", "this build reads document version 1");
  }
  if (parsed.prune !== undefined && typeof parsed.prune !== "boolean") {
    push("prune", "must be true or false");
  }

  const proxies = parsed.proxies;
  const proxyKeys = new Set<string>();
  if (proxies !== undefined) {
    if (!isObject(proxies)) {
      push("proxies", "must be an object of name → URL");
    } else {
      for (const [key, raw] of Object.entries(proxies)) {
        if (typeof raw !== "string" || !PROXY_URL.test(raw.trim())) {
          push(`proxies.${key}`, "must be a scheme://[user:pass@]host[:port] URL");
        } else {
          proxyKeys.add(key);
        }
      }
    }
  }

  const routing = parsed.routing;
  if (routing !== undefined) {
    if (!isObject(routing)) {
      push("routing", "must be an object");
    } else {
      if (routing.enabled !== undefined && typeof routing.enabled !== "boolean") {
        push("routing.enabled", "must be true or false");
      }
      if (routing.fallback_on_quota !== undefined && typeof routing.fallback_on_quota !== "boolean") {
        push("routing.fallback_on_quota", "must be true or false");
      }
      if (routing.per_account_rpm !== undefined && routing.per_account_rpm !== null) {
        const rpm = routing.per_account_rpm;
        if (typeof rpm !== "number" || !Number.isInteger(rpm) || rpm < 1) {
          push("routing.per_account_rpm", "must be a whole number of requests per minute");
        }
      }
      if (routing.max_attempts !== undefined) {
        const attempts = routing.max_attempts;
        if (typeof attempts !== "number" || !Number.isInteger(attempts) || attempts < 1) {
          push("routing.max_attempts", "must be a whole number of attempts");
        }
      }
      if (
        routing.prefer !== undefined &&
        routing.prefer !== "spread" &&
        routing.prefer !== "cheapest"
      ) {
        push("routing.prefer", "must be spread or cheapest");
      }
      if (routing.models !== undefined) {
        const models = routing.models;
        if (
          !Array.isArray(models) ||
          models.some((id) => typeof id !== "string" || id.trim() === "")
        ) {
          push("routing.models", "must be a list of model ids");
        }
      }
    }
  }

  const providers = parsed.providers;
  let providerCount = 0;
  let accountCount = 0;
  if (!Array.isArray(providers) || providers.length === 0) {
    push("providers", "the document must list at least one provider");
  } else {
    providers.forEach((entry, index) => {
      const at = `providers[${index}]`;
      if (!isObject(entry)) {
        push(at, "must be an object");
        return;
      }
      providerCount += 1;
      if (typeof entry.provider !== "string" || entry.provider.trim() === "") {
        push(`${at}.provider`, "a provider id or short code is required");
      }
      if (entry.custom !== undefined && !isObject(entry.custom)) {
        push(`${at}.custom`, "must be an object with id, display, base_url and dialect");
      }
      if (entry.models !== undefined) {
        const models = entry.models;
        if (!Array.isArray(models) || models.some((id) => typeof id !== "string" || id.trim() === "")) {
          push(`${at}.models`, "must be a list of model ids");
        }
      }
      const accounts = entry.accounts;
      if (!Array.isArray(accounts) || accounts.length === 0) {
        push(`${at}.accounts`, "list at least one account");
        return;
      }
      const labels = new Set<string>();
      accounts.forEach((account, accountIndex) => {
        const accountAt = `${at}.accounts[${accountIndex}]`;
        if (!isObject(account)) {
          push(accountAt, "must be an object");
          return;
        }
        accountCount += 1;
        const label = typeof account.label === "string" ? account.label.trim() : "";
        if (label === "") {
          push(`${accountAt}.label`, "a label is required: it is the account's identity on re-import");
        } else if (labels.has(label)) {
          push(`${accountAt}.label`, `duplicate label ${label}: accounts are matched by (provider, label)`);
        } else {
          labels.add(label);
        }
        if (account.proxy !== undefined) {
          const proxy = account.proxy;
          if (typeof proxy !== "string" || proxy.trim() === "") {
            push(`${accountAt}.proxy`, "must be a key of proxies or a full URL");
          } else if (!proxyKeys.has(proxy.trim()) && !PROXY_URL.test(proxy.trim())) {
            push(`${accountAt}.proxy`, `unknown proxy key ${proxy.trim()}`);
          }
        }
        if (account.rpm !== undefined) {
          const rpm = account.rpm;
          if (typeof rpm !== "number" || !Number.isInteger(rpm) || rpm < 1) {
            push(`${accountAt}.rpm`, "must be a whole number of requests per minute");
          }
        }
      });
    });
  }

  return { document: parsed, problems, providers: providerCount, accounts: accountCount };
}

/** Pretty-print for the editor; the gateway stores whatever JSON object it is sent. */
export function documentText(document: Record<string, unknown>): string {
  return `${JSON.stringify(document, null, 2)}\n`;
}

/** Smallest free-mode example that imports cleanly; placeholders only, never a real key. */
export const FREE_DOCUMENT_EXAMPLE = `${JSON.stringify(
  {
    version: 1,
    prune: false,
    routing: {
      enabled: true,
      per_account_rpm: 10,
      max_attempts: 8,
      prefer: "spread",
    },
    proxies: { p1: "http://user:pass@10.0.0.1:8080" },
    providers: [
      {
        provider: "openrouter",
        accounts: [
          { label: "free-1", api_key: "sk-REPLACE", proxy: "p1", rpm: 5 },
          { label: "free-2", api_key: "sk-REPLACE", proxy: "socks5h://10.0.0.2:1080", rpm: 5 },
        ],
      },
    ],
  },
  null,
  2,
)}\n`;
