/**
 * The free-mode document as the dashboard edits it: its shape, its local checks and the
 * small edits the knobs and the model picker make.
 *
 * Kept free of network imports so Node's test runner can load it directly (the UI has no
 * bundler in its test path). The gateway validates the same document authoritatively — this
 * is the subset that can be checked before a request is sent at all.
 */

/** The knobs that decide how `free` spreads load once accounts are imported. */
export type FreeRouting = {
  enabled: boolean;
  /** Proactive cap per account, requests per minute; `null` means no local cap. */
  per_account_rpm: number | null;
  max_attempts: number;
  prefer: FreePrefer;
  /**
   * Model ids the operator picked explicitly. Empty means bare `free` reaches every
   * discovered free model; a populated list narrows it to those ids.
   */
  models: string[];
};

/** `spread` rotates accounts; `cheapest` orders them by the registry price. */
export type FreePrefer = "spread" | "cheapest";

export const FREE_PREFERS: ReadonlyArray<{ id: FreePrefer; label: string; hint: string }> = [
  { id: "spread", label: "Spread", hint: "Rotate accounts so consecutive requests use different proxies." },
  { id: "cheapest", label: "Cheapest", hint: "Order candidates by the published price first." },
];

/** Defaults the gateway applies when the document omits a knob. */
export const ROUTING_DEFAULTS: FreeRouting = {
  enabled: true,
  per_account_rpm: null,
  max_attempts: 8,
  prefer: "spread",
  models: [],
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

// ---------------------------------------------------------------- document edits

/** Pretty-print for the editor; the gateway stores whatever JSON object it is sent. */
export function documentText(document: Record<string, unknown>): string {
  return `${JSON.stringify(document, null, 2)}\n`;
}

/**
 * The knobs for the form: the document's `routing` block wins, the stored knobs fill a
 * document that does not name them, and the documented defaults fill the rest.
 */
export function routingOf(
  document: Record<string, unknown> | null,
  stored: Partial<FreeRouting> | null,
): FreeRouting {
  const fromDocument = isObject(document?.routing) ? document.routing : {};
  const merged: Record<string, unknown> = { ...(stored ?? {}), ...fromDocument };
  return {
    enabled: typeof merged.enabled === "boolean" ? merged.enabled : ROUTING_DEFAULTS.enabled,
    per_account_rpm:
      typeof merged.per_account_rpm === "number" && merged.per_account_rpm >= 1
        ? merged.per_account_rpm
        : null,
    max_attempts:
      typeof merged.max_attempts === "number" && merged.max_attempts >= 1
        ? merged.max_attempts
        : ROUTING_DEFAULTS.max_attempts,
    prefer: merged.prefer === "cheapest" ? "cheapest" : "spread",
    models: Array.isArray(merged.models)
      ? merged.models.filter((id): id is string => typeof id === "string" && id.trim() !== "")
      : [],
  };
}

/** The same document with its routing block replaced; the knobs live inside the document. */
export function withRouting(
  document: Record<string, unknown>,
  routing: FreeRouting,
): Record<string, unknown> {
  const current = isObject(document.routing) ? document.routing : {};
  return { ...document, routing: { ...current, ...routing } };
}

/** `""` (no cap) or a positive whole number; the knob shows this inline. */
export function rpmProblem(value: string): string | null {
  const text = value.trim();
  if (text === "") return null;
  return /^\d+$/.test(text) && Number(text) >= 1
    ? null
    : "whole number of requests per minute, or empty for no cap";
}

/** `max_attempts`: always present, always a positive whole number. */
export function attemptsProblem(value: string): string | null {
  const text = value.trim();
  if (text === "") return "required";
  return /^\d+$/.test(text) && Number(text) >= 1 ? null : "a whole number of attempts";
}

/** A registry price in USD per million tokens, or `—` when no catalogue priced the model. */
export function priceLabel(price: number | null): string {
  if (price === null) return "—";
  if (price === 0) return "$0";
  return `$${price.toFixed(price < 1 ? 4 : 2)}`;
}

/** How a model became a free one, in the words the badge uses. */
export function sourceLabel(source: string): string {
  switch (source) {
    case "document":
      return "document";
    case "provider-list":
      return "provider list";
    case "registry":
      return "registry";
    default:
      return source === "" ? "unknown" : source;
  }
}

export type FreeRefHint = { ref: string; hint: string };

/**
 * The four refs `/v1` accepts for free mode, in the order the docs explain them. A real id
 * is used wherever one is known; the caller decides the sample, and `null` leaves
 * placeholders that show the shape without inventing a model.
 */
export function freeRefHints(sample: { provider: string; model: string } | null): FreeRefHint[] {
  const provider = sample?.provider ?? "<provider>";
  const model = sample?.model ?? "<model-id>";
  return [
    { ref: "free", hint: "every free model discovery recorded, across every free account" },
    { ref: `free:${provider}`, hint: "one provider's free models" },
    { ref: `free/${model}`, hint: "one exact model across every free account" },
    { ref: `free:${provider}/${model}`, hint: "one model on one provider" },
  ];
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
      models: ["deepseek-v4.1-flash"],
    },
    proxies: { "p1": "http://user:pass@10.0.0.1:8080" },
    providers: [
      {
        provider: "openrouter",
        models: ["deepseek-v4.1-flash"],
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
