import { useEffect, useId, useMemo, useState } from "react";
import { Link, useNavigate } from "react-router-dom";
import { useQueryClient } from "@tanstack/react-query";
import { api, type Connection } from "../../lib/api";
import {
  qk,
  useCapabilities,
  useConnections,
  useModels,
  type Capabilities,
  type CapabilityName,
  type CapabilityProvider,
} from "../../lib/queries";
import {
  AlertBanner,
  Badge,
  Button,
  Card,
  CardFooter,
  CardHeader,
  CopyButton,
  EmptyState,
  ErrorNote,
  Field,
  Modal,
  PageHeader,
  Select,
  Text,
  Textarea,
  control,
  cx,
} from "../../components/ui";
import { GATEWAY_SETTINGS_KEY } from "../settings/gateway-settings";

const MAX_DEFAULT_LENGTH = 200;

/** Stable empty lists so the memos below do not re-run on every render. */
const NO_PROVIDERS: CapabilityProvider[] = [];
const NO_MODELS: Array<{ provider: string; id: string }> = [];

/** Where a `/v1` call finds its model, in the order the gateway tries it. */
const FALLBACK_ORDER = "request model/provider → stored default → 400";

/** Gateway key for the tryout. sessionStorage only: it must not outlive the tab. */
const TRY_KEY_STORAGE = "rrouter:try-key";

export type CapabilityPageProps = {
  capability: CapabilityName;
  /** Page title, e.g. "Search". */
  title: string;
  /** The `/v1` endpoint this page's default backs, e.g. `POST /v1/search`. */
  endpoint: string;
  /**
   * How the registry established these providers: `serving` when the capability is
   * explicit, `eligible` when only the wire dialect matches and the upstream still has
   * to expose the endpoint itself.
   */
  providerWording?: "serving" | "eligible";
  /** Explains the eligibility test; only rendered with `providerWording="eligible"`. */
  eligibilityNote?: string;
};

/**
 * One layout for the three capability defaults: a catalog of the providers that can serve
 * the capability, and a drawer per provider holding its accounts, the stored default and a
 * tryout that calls the real `/v1` endpoint with a gateway key.
 */
export function CapabilityPage({
  capability,
  title,
  endpoint,
  providerWording = "serving",
  eligibilityNote,
}: CapabilityPageProps) {
  const providerHeading =
    providerWording === "eligible"
      ? `Providers eligible for ${title}`
      : `Providers serving ${title}`;
  const caps = useCapabilities();
  // Search suggests refs from the capability listing alone; the other two join it with
  // the model registry, so that read stays idle on the Search page.
  const models = useModels(capability !== "search");
  // Account rows for the cards' state and the drawer's list. The admin routes already
  // gate this page, so the read is never a 403 for a reader.
  const connections = useConnections();

  const providers = caps.data?.providers[capability] ?? NO_PROVIDERS;
  const stored = caps.data?.defaults[capability] ?? null;

  const [selectedId, setSelectedId] = useState<string | null>(null);
  const selected = useMemo(
    () => providers.find((row) => row.provider === selectedId) ?? null,
    [providers, selectedId],
  );

  const accountsOf = useMemo(() => {
    const byProvider = new Map<string, Connection[]>();
    for (const connection of connections.data ?? []) {
      const list = byProvider.get(connection.provider);
      if (list) list.push(connection);
      else byProvider.set(connection.provider, [connection]);
    }
    return byProvider;
  }, [connections.data]);

  const manageHref = (provider: string): string => {
    const account = (accountsOf.get(provider) ?? []).find((row) => row.is_active);
    return account ? `/providers?inspect=${encodeURIComponent(account.id)}` : "/providers";
  };

  return (
    <>
      <PageHeader
        title={title}
        subtitle={`The stored default backs ${endpoint}. It applies only when the client omits a model and never overrides one.`}
      />

      {caps.isError ? (
        <div className="mb-4">
          <ErrorNote
            error={caps.error}
            title="Could not load capability defaults"
            onRetry={() => void caps.refetch()}
          />
        </div>
      ) : null}

      <Card>
        <CardHeader title={providerHeading} />
        {eligibilityNote ? (
          <Text as="p" variant="caption" tone="muted" className="px-5 pb-3">
            {eligibilityNote}
          </Text>
        ) : null}

        {caps.isPending ? (
          <div
            aria-busy="true"
            aria-label={`Loading ${providerHeading.toLowerCase()}`}
            className="grid gap-3 px-5 pb-5 sm:grid-cols-2 xl:grid-cols-3"
          >
            {[0, 1, 2].map((slot) => (
              <div
                key={slot}
                className="h-[116px] animate-pulse rounded-[var(--radius-card)] border border-line bg-surface-2"
              />
            ))}
          </div>
        ) : providers.length === 0 ? (
          <EmptyState>
            {providerWording === "eligible" ? (
              <span>
                No compiled provider is eligible for this capability. A client must still pin{" "}
                <span className="font-mono">provider/model</span> on every request.
              </span>
            ) : (
              <span>
                No compiled provider serves this capability. A client must still pin{" "}
                <span className="font-mono">provider/model</span> on every request.
              </span>
            )}
          </EmptyState>
        ) : (
          <div className="grid gap-3 px-5 pb-5 sm:grid-cols-2 xl:grid-cols-3">
            {providers.map((row) => (
              <ProviderCard
                key={row.provider}
                row={row}
                capability={capability}
                title={title}
                onOpen={() => setSelectedId(row.provider)}
                manageHref={manageHref(row.provider)}
              />
            ))}
          </div>
        )}
      </Card>

      <Text as="p" variant="caption" tone="faint" className="mt-3">
        {endpoint} resolves a call in this order: {FALLBACK_ORDER}.
      </Text>

      {selected ? (
        <CapabilityDrawer
          key={selected.provider}
          capability={capability}
          title={title}
          endpoint={endpoint}
          row={selected}
          stored={stored}
          providers={providers}
          models={models.data ?? NO_MODELS}
          accounts={accountsOf.get(selected.provider) ?? []}
          onClose={() => setSelectedId(null)}
        />
      ) : null}
    </>
  );
}

/** One provider in the catalog: the card body opens the drawer, its footer links out. */
function ProviderCard({
  row,
  capability,
  title,
  onOpen,
  manageHref,
}: {
  row: CapabilityProvider;
  capability: CapabilityName;
  title: string;
  onOpen: () => void;
  manageHref: string;
}) {
  return (
    <Card as="article" className="flex flex-col">
      <button
        type="button"
        onClick={onOpen}
        className="flex flex-1 flex-col gap-1.5 px-4 pb-3 pt-4 text-left focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-[-2px] focus-visible:outline-[var(--color-focus-ring)]"
      >
        <span className="flex min-w-0 items-center gap-2">
          <span className="min-w-0 flex-1 truncate text-body font-medium text-fg">
            {row.display}
          </span>
          <Badge tone={row.connected ? "pos" : "neutral"}>
            {row.connected ? `Connected · ${row.accounts}` : "Not connected"}
          </Badge>
        </span>
        <span className="min-w-0 truncate font-mono text-caption text-faint">{row.provider}</span>
        <span className="min-w-0 truncate text-caption text-muted">
          {row.default_model ? (
            <>
              Default model <span className="font-mono text-ink-soft">{row.default_model}</span>
            </>
          ) : capability === "search" ? (
            "No default model"
          ) : (
            `${title} · model is sent per request`
          )}
        </span>
      </button>
      <div className="flex items-center justify-end border-t border-line px-4 py-2">
        {row.connected ? (
          <Link
            to={manageHref}
            className="text-caption text-muted underline underline-offset-2 hover:text-fg"
          >
            Manage
          </Link>
        ) : (
          <Link
            to={`/providers?add=${encodeURIComponent(row.provider)}`}
            className="text-caption text-muted underline underline-offset-2 hover:text-fg"
          >
            Connect
          </Link>
        )}
      </div>
    </Card>
  );
}

/** Identity, accounts, the stored default and a live tryout for one provider. */
function CapabilityDrawer({
  capability,
  title,
  endpoint,
  row,
  stored,
  providers,
  models,
  accounts,
  onClose,
}: {
  capability: CapabilityName;
  title: string;
  endpoint: string;
  row: CapabilityProvider;
  stored: string | null;
  providers: CapabilityProvider[];
  models: Array<{ provider: string; id: string }>;
  accounts: Connection[];
  onClose: () => void;
}) {
  const navigate = useNavigate();
  const addHref = `/providers?add=${encodeURIComponent(row.provider)}`;

  return (
    <Modal
      open
      variant="drawer"
      width="xl"
      title={row.display}
      description={`${title} · ${row.provider}`}
      onClose={onClose}
      footer={
        <Button variant="secondary" onClick={onClose}>
          Close
        </Button>
      }
    >
      <div className="flex flex-col gap-5">
        <section className="flex flex-wrap items-center gap-2">
          <Badge tone="accent">{title}</Badge>
          <Badge tone={row.connected ? "pos" : "neutral"}>
            {row.connected ? `Connected · ${row.accounts} active` : "Not connected"}
          </Badge>
          <span className="min-w-0 truncate font-mono text-caption text-faint">{row.provider}</span>
          {row.default_model ? (
            <span className="min-w-0 truncate font-mono text-caption text-muted">
              default {row.default_model}
            </span>
          ) : null}
        </section>

        <Card>
          <CardHeader
            title="Accounts"
            action={
              <Button variant="quiet" size="sm" onClick={() => navigate(addHref)}>
                Add connection
              </Button>
            }
          />
          {accounts.length === 0 ? (
            <Text as="p" variant="caption" tone="muted" className="px-5 pb-4">
              No connection yet. Add one to route this capability through{" "}
              <span className="font-mono">{row.provider}</span>.
            </Text>
          ) : (
            <ul className="flex flex-col gap-1.5 px-5 pb-4">
              {accounts.map((account) => (
                <li
                  key={account.id}
                  className="flex items-center justify-between gap-3 rounded-[var(--radius-input)] border border-line px-3 py-2"
                >
                  <span className="min-w-0">
                    <span className="block truncate text-small text-fg">
                      {account.name ?? account.email ?? account.id.slice(0, 8)}
                    </span>
                    <span className="block truncate text-caption text-faint">
                      {account.auth_type}
                      {account.email && account.name ? ` · ${account.email}` : ""}
                    </span>
                  </span>
                  <Badge tone={account.is_active ? "pos" : "neutral"}>
                    {account.is_active ? "Active" : "Disabled"}
                  </Badge>
                </li>
              ))}
            </ul>
          )}
        </Card>

        <DefaultEditor
          capability={capability}
          providers={providers}
          models={models}
          stored={stored}
          provider={row}
        />

        <Tryout capability={capability} endpoint={endpoint} row={row} stored={stored} />
      </div>
    </Modal>
  );
}

/** The stored default input, unchanged in behaviour, now scoped to an open drawer. */
function DefaultEditor({
  capability,
  providers,
  models,
  stored,
  provider,
}: {
  capability: CapabilityName;
  providers: CapabilityProvider[];
  models: Array<{ provider: string; id: string }>;
  stored: string | null;
  provider: CapabilityProvider;
}) {
  const client = useQueryClient();
  const [draft, setDraft] = useState(stored ?? "");
  const [dirty, setDirty] = useState(false);
  const [saving, setSaving] = useState<null | "save" | "clear">(null);
  const [error, setError] = useState<unknown>(null);
  const [invalid, setInvalid] = useState<string | null>(null);
  const listId = useId();
  const busy = saving !== null;

  // A refetch never rewrites what the operator is typing; it only refreshes the field
  // while the draft is still the stored value.
  useEffect(() => {
    if (!dirty) setDraft(stored ?? "");
  }, [stored, dirty]);

  const suggestions = useMemo(() => {
    const refs = new Set<string>();
    if (capability === "search") {
      for (const row of providers) {
        refs.add(row.provider);
        if (row.default_model) refs.add(`${row.provider}/${row.default_model}`);
      }
    } else {
      const connected = new Set(
        providers.filter((row) => row.connected).map((row) => row.provider),
      );
      for (const model of models) {
        if (connected.has(model.provider)) refs.add(`${model.provider}/${model.id}`);
      }
    }
    return [...refs].sort();
  }, [capability, providers, models]);

  /** PUT the one-key block; the server deep-merges it into the stored settings. */
  const write = async (mode: "save" | "clear", value: string | null) => {
    setSaving(mode);
    setError(null);
    try {
      await api.put("/api/settings", { capability_defaults: { [capability]: value } });
      client.setQueryData<Capabilities>(qk.capabilities, (old) =>
        old ? { ...old, defaults: { ...old.defaults, [capability]: value } } : old,
      );
      setDraft(value ?? "");
      setDirty(false);
      setInvalid(null);
      void client.invalidateQueries({ queryKey: qk.capabilities });
      void client.invalidateQueries({ queryKey: GATEWAY_SETTINGS_KEY });
    } catch (e) {
      // The field still holds the draft, so a failed save changes nothing upstream.
      setError(e);
    } finally {
      setSaving(null);
    }
  };

  const submit = () => {
    const next = draft.trim();
    if (next.length === 0) {
      setInvalid("Enter a provider or provider/model.");
      return;
    }
    if (next.length > MAX_DEFAULT_LENGTH) {
      setInvalid(`Keep the default to ${MAX_DEFAULT_LENGTH} characters or fewer.`);
      return;
    }
    setInvalid(null);
    void write("save", next);
  };

  return (
    <Card>
      <CardHeader title="Stored default" />
      <div className="flex flex-col gap-3 px-5 pb-4">
        {error ? (
          <AlertBanner
            tone="neg"
            title="Could not save the default"
            onDismiss={() => setError(null)}
          >
            {error instanceof Error ? error.message : String(error)}
          </AlertBanner>
        ) : null}
        <Field
          label="Default provider or model"
          hint={`Applies to every client that omits a model on this capability. Leave unset to require one. Up to ${MAX_DEFAULT_LENGTH} characters.`}
          error={invalid ?? undefined}
        >
          <input
            list={listId}
            value={draft}
            disabled={busy}
            placeholder={
              capability === "search"
                ? provider.default_model
                  ? `${provider.provider}/${provider.default_model}`
                  : "provider or provider/model"
                : "provider/model"
            }
            autoComplete="off"
            spellCheck={false}
            className={cx(control, "font-mono text-small")}
            onChange={(event) => {
              setDraft(event.target.value);
              setDirty(true);
              setInvalid(null);
            }}
            onKeyDown={(event) => {
              if (event.key === "Enter") {
                event.preventDefault();
                submit();
              }
            }}
          />
        </Field>
        <datalist id={listId}>
          {suggestions.map((ref) => (
            <option key={ref} value={ref} />
          ))}
        </datalist>
        <Text as="p" variant="caption" tone="muted">
          Current default:{" "}
          {stored ? (
            <span className="font-mono">{stored}</span>
          ) : (
            <span>none — a client must send provider/model</span>
          )}
        </Text>
      </div>
      <CardFooter>
        <Button
          variant="ghost"
          className="text-muted"
          disabled={busy || (stored === null && draft.trim() === "")}
          loading={saving === "clear"}
          onClick={() => void write("clear", null)}
        >
          Clear
        </Button>
        <Button variant="secondary" disabled={busy} loading={saving === "save"} onClick={submit}>
          Save
        </Button>
      </CardFooter>
    </Card>
  );
}

type RunResult = {
  ok: boolean;
  status: number;
  latencyMs: number;
  requestId: string | null;
  text: string;
  error: string | null;
};

const SEARCH_TYPES = ["web", "news", "images"] as const;

/** The model a tryout sends when the operator has not edited the field. */
function initialModel(
  capability: CapabilityName,
  row: CapabilityProvider,
  stored: string | null,
): string {
  if (capability === "search") return row.default_model ?? row.provider;
  // Rerank and embeddings take a real `provider/model` ref; the provider alone is not a
  // model the planner can resolve.
  if (stored && (stored === row.provider || stored.startsWith(`${row.provider}/`))) {
    return stored;
  }
  return `${row.provider}/`;
}

function splitLines(value: string): string[] {
  return value
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter((line) => line.length > 0);
}

function prettyBody(text: string): string {
  const trimmed = text.trim();
  if (trimmed === "") return "(empty response body)";
  try {
    return JSON.stringify(JSON.parse(trimmed), null, 2);
  } catch {
    return trimmed;
  }
}

/** The gateway's own error line, when the body carries one. */
function errorMessage(text: string): string | null {
  try {
    const parsed: unknown = JSON.parse(text);
    if (typeof parsed !== "object" || parsed === null) return null;
    const body = parsed as Record<string, unknown>;
    const error = body.error;
    if (typeof error === "string") return error;
    if (typeof error === "object" && error !== null) {
      const message = (error as Record<string, unknown>).message;
      if (typeof message === "string") return message;
    }
    for (const key of ["message", "detail", "reason"]) {
      const value = body[key];
      if (typeof value === "string") return value;
    }
    return null;
  } catch {
    return null;
  }
}

function readTryKey(): string {
  try {
    return window.sessionStorage.getItem(TRY_KEY_STORAGE) ?? "";
  } catch {
    return "";
  }
}

function storeTryKey(value: string): void {
  try {
    if (value) window.sessionStorage.setItem(TRY_KEY_STORAGE, value);
    else window.sessionStorage.removeItem(TRY_KEY_STORAGE);
  } catch {
    // Storage can be unavailable (private mode); the key simply is not persisted.
  }
}

function curlFor(path: string, body: Record<string, unknown>): string {
  const payload = JSON.stringify(body).replace(/'/g, `'\\''`);
  return `curl -sS ${window.location.origin}${path} \\
  -H "Authorization: Bearer $RROUTER_API_KEY" \\
  -H "Content-Type: application/json" \\
  -d '${payload}'`;
}

/**
 * Live example for one provider: builds the exact body `/v1` accepts and sends it with the
 * operator's gateway key. The key never leaves sessionStorage and is never logged or
 * copied into the curl snippet.
 */
function Tryout({
  capability,
  endpoint,
  row,
  stored,
}: {
  capability: CapabilityName;
  endpoint: string;
  row: CapabilityProvider;
  stored: string | null;
}) {
  const path = endpoint.split(/\s+/)[1] ?? endpoint;
  const [key, setKey] = useState(readTryKey);
  const [model, setModel] = useState(() => initialModel(capability, row, stored));
  const [query, setQuery] = useState("");
  const [searchType, setSearchType] = useState<string>("web");
  const [maxResults, setMaxResults] = useState("5");
  const [documents, setDocuments] = useState("");
  const [topN, setTopN] = useState("");
  const [input, setInput] = useState("");
  const [dimensions, setDimensions] = useState("");
  const [running, setRunning] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);
  const [result, setResult] = useState<RunResult | null>(null);

  const body = useMemo<Record<string, unknown> | null>(() => {
    const ref = model.trim();
    if (ref === "") return null;
    if (capability === "search") {
      const text = query.trim();
      if (text === "") return null;
      const out: Record<string, unknown> = { model: ref, query: text, search_type: searchType };
      const count = Number(maxResults);
      if (Number.isFinite(count) && count > 0) out.max_results = Math.floor(count);
      return out;
    }
    if (capability === "rerank") {
      const text = query.trim();
      const docs = splitLines(documents);
      if (text === "" || docs.length === 0) return null;
      const out: Record<string, unknown> = { model: ref, query: text, documents: docs };
      const n = Number(topN);
      if (topN.trim() !== "" && Number.isFinite(n) && n > 0) out.top_n = Math.floor(n);
      return out;
    }
    const text = input.trim();
    if (text === "") return null;
    const out: Record<string, unknown> = { model: ref, input: text };
    const d = Number(dimensions);
    if (dimensions.trim() !== "" && Number.isFinite(d) && d > 0) out.dimensions = Math.floor(d);
    return out;
  }, [capability, model, query, searchType, maxResults, documents, topN, input, dimensions]);

  const validate = (): string | null => {
    if (key.trim() === "") return "Add a gateway key. A /v1 request is refused without one.";
    const ref = model.trim();
    if (ref === "") return "Enter the model to send.";
    if (capability !== "search" && (!ref.includes("/") || ref.endsWith("/"))) {
      return "Rerank and embeddings need a full provider/model ref.";
    }
    if (capability === "search" && query.trim() === "") return "Enter a query.";
    if (capability === "rerank" && query.trim() === "") return "Enter a query.";
    if (capability === "rerank" && splitLines(documents).length === 0) {
      return "Add at least one document, one per line.";
    }
    if (capability === "embeddings" && input.trim() === "") return "Enter the text to embed.";
    return null;
  };

  const run = async () => {
    const invalid = validate();
    if (invalid || body === null) {
      setProblem(invalid ?? "Complete the request first.");
      return;
    }
    setProblem(null);
    setRunning(true);
    const started = performance.now();
    try {
      const response = await fetch(path, {
        method: "POST",
        headers: {
          "content-type": "application/json",
          authorization: `Bearer ${key.trim()}`,
        },
        body: JSON.stringify(body),
      });
      const text = await response.text();
      setResult({
        ok: response.ok,
        status: response.status,
        latencyMs: Math.round(performance.now() - started),
        requestId: response.headers.get("x-request-id"),
        text: prettyBody(text),
        error: response.ok ? null : errorMessage(text),
      });
    } catch (e) {
      setResult({
        ok: false,
        status: 0,
        latencyMs: Math.round(performance.now() - started),
        requestId: null,
        text: e instanceof Error ? e.message : String(e),
        error: "The request did not reach the gateway.",
      });
    } finally {
      setRunning(false);
    }
  };

  return (
    <Card>
      <CardHeader
        title="Try it"
        action={<span className="font-mono text-caption text-faint">{endpoint}</span>}
      />
      <div className="flex flex-col gap-3 px-5 pb-5">
        <Field
          label="Gateway key"
          hint={
            <>
              `/v1/*` needs a gateway key, not the admin token. Create one on{" "}
              <Link to="/keys" className="text-muted underline underline-offset-2 hover:text-fg">
                Keys
              </Link>
              . Stored in this tab only.
            </>
          }
        >
          <input
            type="password"
            value={key}
            autoComplete="off"
            spellCheck={false}
            placeholder="rrk_…"
            className={cx(control, "font-mono text-small")}
            onChange={(event) => {
              setKey(event.target.value);
              storeTryKey(event.target.value);
            }}
          />
        </Field>

        <Field
          label="Model"
          hint={
            capability === "search"
              ? "A provider id is expanded to its search model; provider/model pins both."
              : model.trim().endsWith("/")
                ? "Complete the model id after the provider prefix. The full provider/model ref is what the upstream receives."
                : "provider/model — the ref the gateway routes and the upstream receives."
          }
        >
          <input
            value={model}
            autoComplete="off"
            spellCheck={false}
            className={cx(control, "font-mono text-small")}
            onChange={(event) => setModel(event.target.value)}
          />
        </Field>

        {capability === "search" ? (
          <>
            <Field label="Query">
              <input
                value={query}
                autoComplete="off"
                placeholder="latest release notes for Bun"
                className={cx(control, "text-small")}
                onChange={(event) => setQuery(event.target.value)}
              />
            </Field>
            <div className="grid gap-3 sm:grid-cols-2">
              <Field label="Type" hint="Provider-native search is web only today.">
                <Select value={searchType} onChange={setSearchType} ariaLabel="search type">
                  {SEARCH_TYPES.map((type) => (
                    <option key={type} value={type} disabled={type !== "web"}>
                      {type === "web" ? "web" : `${type} — not supported`}
                    </option>
                  ))}
                </Select>
              </Field>
              <Field label="Max results" hint="1–20; the gateway default is 5.">
                <input
                  type="number"
                  min={1}
                  max={20}
                  value={maxResults}
                  className={cx(control, "tnum text-small")}
                  onChange={(event) => setMaxResults(event.target.value)}
                />
              </Field>
            </div>
          </>
        ) : null}

        {capability === "rerank" ? (
          <>
            <Field label="Query">
              <input
                value={query}
                autoComplete="off"
                placeholder="how do I reset my password?"
                className={cx(control, "text-small")}
                onChange={(event) => setQuery(event.target.value)}
              />
            </Field>
            <Field label="Documents" hint="One document per line. Blank lines are dropped.">
              <Textarea
                value={documents}
                rows={4}
                placeholder={"Reset your password from the sign-in page.\nContact support to recover an account."}
                className="font-mono text-small"
                onChange={setDocuments}
              />
            </Field>
            <Field label="Top N" optional hint="Leave blank to let the provider decide.">
              <input
                type="number"
                min={1}
                value={topN}
                className={cx(control, "tnum text-small")}
                onChange={(event) => setTopN(event.target.value)}
              />
            </Field>
          </>
        ) : null}

        {capability === "embeddings" ? (
          <>
            <Field label="Input" hint="The text is relayed to the provider unchanged.">
              <Textarea
                value={input}
                rows={3}
                placeholder="The quick brown fox jumps over the lazy dog."
                className="text-small"
                onChange={setInput}
              />
            </Field>
            <Field label="Dimensions" optional hint="Only sent when filled; the upstream must support it.">
              <input
                type="number"
                min={1}
                value={dimensions}
                className={cx(control, "tnum text-small")}
                onChange={(event) => setDimensions(event.target.value)}
              />
            </Field>
          </>
        ) : null}

        {problem ? (
          <p role="alert" className="text-small text-[var(--color-neg-ink)]">
            {problem}
          </p>
        ) : null}

        <div className="flex flex-wrap items-center gap-2">
          <Button variant="primary" loading={running} onClick={() => void run()}>
            Run
          </Button>
          <CopyButton
            value={curlFor(path, body ?? { model: model.trim() })}
            label="Copy curl"
          />
          <span className="text-caption text-faint">The curl uses $RROUTER_API_KEY, not this key.</span>
        </div>

        <div className="min-h-[120px]">
          {result ? (
            <div className="flex flex-col gap-2">
              <div className="flex flex-wrap items-center gap-2">
                <Badge tone={result.ok ? "pos" : "neg"}>
                  {result.status === 0 ? "No response" : `HTTP ${result.status}`}
                </Badge>
                <span className="tnum text-caption text-muted">{result.latencyMs} ms</span>
                {result.requestId ? (
                  <span className="min-w-0 truncate font-mono text-caption text-faint">
                    {result.requestId}
                  </span>
                ) : null}
              </div>
              {result.error ? (
                <p role="alert" className="text-small text-[var(--color-neg-ink)]">
                  {result.error}
                </p>
              ) : null}
              <pre className="max-h-72 overflow-auto whitespace-pre-wrap break-words rounded-[var(--radius-input)] border border-line bg-surface-2 p-3 text-caption leading-relaxed text-ink-soft">
                {result.text}
              </pre>
            </div>
          ) : (
            <p className="text-caption text-faint">Run a request to see the gateway response.</p>
          )}
        </div>
      </div>
    </Card>
  );
}
