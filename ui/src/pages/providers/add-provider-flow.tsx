import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import { Link } from "react-router-dom";
import { useMutation } from "@tanstack/react-query";
import { CheckCircle, MagnifyingGlass, WarningCircle, X } from "@phosphor-icons/react";
import { Heading } from "../../components/atoms";
import { ProviderIcon } from "../../components/provider-icon";
import {
  Badge,
  Button,
  Card,
  CardHeader,
  Dot,
  ErrorNote,
  Field,
  Input,
  Modal,
  Select,
} from "../../components/ui";
import {
  api,
  type Connection,
  type ModelProvider,
  type OAuthProvider,
} from "../../lib/api";
import { ms } from "../../lib/format";
import { useTeams } from "../../lib/queries";
import {
  baseUrlProblem,
  codeProblemFor,
  isAntigravity,
  matchesProvider,
  protocolLabel,
  providerMeta,
  publishedDefaultUrl,
  useInvalidateProviders,
  type ConnectionTestResult,
} from "./types";

export function EmptyProviders({ onAdd }: { onAdd: (() => void) | null }) {
  return (
    <Card className="px-6 py-14 text-center">
      <img
        src={`${import.meta.env.BASE_URL}logo-mark.png`}
        alt=""
        width={56}
        height={56}
        className="mx-auto h-14 w-14 opacity-90"
      />
      <Heading level={2} className="mt-4 text-heading font-medium">
        Connect your first provider
      </Heading>
      <p className="mx-auto mt-1.5 max-w-[440px] text-body text-muted">
        RRouter has nothing to route to yet. Sign in to a subscription account, paste an API key,
        or turn on zero-config free models.
      </p>
      {onAdd ? (
        <div className="mt-5 flex flex-wrap items-center justify-center gap-3">
          <Button variant="primary" onClick={onAdd}>
            Add your first provider
          </Button>
          <Link
            to="/free-mode"
            className="text-small font-medium text-[var(--color-accent-text)] underline-offset-2 hover:underline focus-visible:underline"
          >
            Try free models →
          </Link>
        </div>
      ) : (
        <p className="mt-4 text-caption text-faint">
          An administrator adds the first provider account.
        </p>
      )}
    </Card>
  );
}

export function EmptyTeamProviders({
  teamLabel,
  onShowAll,
  onAdd,
}: {
  teamLabel: string;
  onShowAll: () => void;
  onAdd: (() => void) | null;
}) {
  return (
    <Card className="px-6 py-12 text-center">
      <Heading level={2} className="text-heading font-medium">
        No provider accounts assigned to {teamLabel}
      </Heading>
      <p className="mx-auto mt-1.5 max-w-[460px] text-body text-muted">
        Team keys route only to their own team's accounts; shared accounts serve instance-wide keys.
        Requests from this team fail until an administrator assigns an account here.
      </p>
      <div className="mt-5 flex flex-wrap justify-center gap-2">
        <Button variant="quiet" onClick={onShowAll}>
          Show all accounts
        </Button>
        {onAdd ? (
          <Button variant="primary" onClick={onAdd}>
            Add provider
          </Button>
        ) : null}
      </div>
    </Card>
  );
}

/** Modal wrapper around ProviderCodesCard for viewing and editing shorthand codes. */
export function ProviderCodesModal({
  open,
  providers,
  onClose,
}: {
  open: boolean;
  providers: ModelProvider[];
  onClose: () => void;
}) {
  if (!open) return null;
  return (
    <Modal
      title="Provider codes"
      open
      width="xl"
      description="The shorthand each provider's model refs use. Unique across every provider, so two providers serving one model id can never be confused."
      onClose={onClose}
    >
      <ProviderCodesCard providers={providers} embedded />
    </Modal>
  );
}

type AddStep = "pick" | "credential" | "result";

type DraftFields = {
  name: string;
  team: string;
  baseUrl: string;
};

const EMPTY_DRAFT: DraftFields = { name: "", team: "", baseUrl: "" };

/**
 * Pick a provider, enter the credential (API key first, focused), then save and run a
 * live probe right inside the dialog so a bad key is caught in one click instead of on
 * the first client request.
 */
export function AddProviderFlow({
  open,
  initialCustomProtocol,
  initialProvider = null,
  onClose,
  onChooseOAuth,
  subscriptionProviders,
  keyProviders,
  providers,
  onAccountCreated,
  onTestRecorded,
}: {
  open: boolean;
  initialCustomProtocol: "openai" | "anthropic" | null;
  /** Deep link (`/providers?add=<id>`): open straight on this provider's credential step. */
  initialProvider?: string | null;
  onClose: () => void;
  onChooseOAuth: (provider: OAuthProvider) => void;
  subscriptionProviders: Array<OAuthProvider & { display: string }>;
  keyProviders: ModelProvider[];
  /** Every provider row, for the connection scope of a subscription sign-in. */
  providers: ModelProvider[];
  onAccountCreated: (connection: Connection) => void;
  onTestRecorded: (result: ConnectionTestResult) => void;
}) {
  const { invalidate } = useInvalidateProviders();
  const [step, setStep] = useState<AddStep>("pick");
  const [keyProvider, setKeyProvider] = useState<string | null>(null);
  const [customProtocol, setCustomProtocol] = useState<"openai" | "anthropic" | null>(null);
  const [query, setQuery] = useState("");
  const [draft, setDraft] = useState<DraftFields>(EMPTY_DRAFT);
  const [created, setCreated] = useState<{ connection: Connection; display: string } | null>(null);
  const [probeResult, setProbeResult] = useState<ConnectionTestResult | null>(null);
  const [probeBusy, setProbeBusy] = useState(false);
  const [removeError, setRemoveError] = useState<unknown>(null);
  const probeRunId = useRef(0);

  useEffect(() => {
    setQuery("");
    setDraft(EMPTY_DRAFT);
    setCreated(null);
    setProbeResult(null);
    setProbeBusy(false);
    setRemoveError(null);
    probeRunId.current += 1;
    if (!open) {
      setStep("pick");
      setKeyProvider(null);
      setCustomProtocol(null);
      return;
    }
    if (initialCustomProtocol) {
      setStep("credential");
      setKeyProvider(initialCustomProtocol);
      setCustomProtocol(initialCustomProtocol);
    } else if (initialProvider) {
      setStep("credential");
      setKeyProvider(initialProvider);
      setCustomProtocol(null);
    } else {
      setStep("pick");
      setKeyProvider(null);
      setCustomProtocol(null);
    }
  }, [open, initialCustomProtocol, initialProvider]);

  const runProbe = useCallback(
    async (connectionId: string) => {
      const run = ++probeRunId.current;
      setProbeBusy(true);
      setProbeResult(null);
      setRemoveError(null);
      try {
        const res = await api.post<{
          ok: boolean;
          status: number;
          latency_ms: number;
          error?: string;
        }>(`/api/connections/${connectionId}/test`);
        if (probeRunId.current !== run) return;
        const result: ConnectionTestResult = { id: connectionId, ...res };
        setProbeResult(result);
        onTestRecorded(result);
      } catch (error) {
        if (probeRunId.current !== run) return;
        const message = error instanceof Error ? error.message : String(error);
        const result: ConnectionTestResult = {
          id: connectionId,
          ok: false,
          status: 0,
          latency_ms: 0,
          error: message,
        };
        setProbeResult(result);
        onTestRecorded(result);
      } finally {
        if (probeRunId.current === run) setProbeBusy(false);
      }
    },
    [onTestRecorded],
  );

  const removeAndRetry = useMutation({
    mutationFn: (id: string) => api.del<{ deleted: boolean }>(`/api/connections/${id}`),
    onSuccess: () => {
      probeRunId.current += 1;
      setCreated(null);
      setProbeResult(null);
      setProbeBusy(false);
      setRemoveError(null);
      invalidate();
      setStep("credential");
    },
    onError: setRemoveError,
  });

  const needle = query.trim().toLowerCase();
  const signIn = subscriptionProviders.filter((provider) => matchesProvider(needle, provider));
  const keyMatches = keyProviders.filter((provider) => matchesProvider(needle, provider));
  const noMatch = needle !== "" && signIn.length === 0 && keyMatches.length === 0;
  const chosenLabel = keyProviders.find((provider) => provider.id === keyProvider)?.display;

  const choose = (provider: string, protocol: "openai" | "anthropic" | null) => {
    setQuery("");
    setDraft(EMPTY_DRAFT);
    setKeyProvider(provider);
    setCustomProtocol(protocol);
    setStep("credential");
  };

  const resetToPick = () => {
    probeRunId.current += 1;
    setStep("pick");
    setKeyProvider(null);
    setCustomProtocol(null);
    setDraft(EMPTY_DRAFT);
    setCreated(null);
    setProbeResult(null);
    setProbeBusy(false);
    setRemoveError(null);
  };

  return (
    <Modal
      onEscapeKeyDown={(event) => {
        if (query !== "") {
          event.preventDefault();
          setQuery("");
        }
      }}
      title={
        step === "pick"
          ? "Add a provider"
          : step === "result"
            ? probeBusy
              ? `Testing ${created?.display ?? "connection"}…`
              : probeResult?.ok
                ? `${created?.display ?? "Provider"} connected`
                : `${created?.display ?? "Provider"} test failed`
            : customProtocol !== null
              ? "Add a custom endpoint"
              : keyProvider === "opencode"
                ? "Add OpenCode"
                : `Add an API key${chosenLabel ? ` · ${chosenLabel}` : ""}`
      }
      open={open}
      variant="dialog"
      width={step === "pick" ? "lg" : "md"}
      onClose={onClose}
      footer={
        step === "result" && created ? (
          probeBusy ? (
            <Button variant="quiet" onClick={onClose}>
              Close
            </Button>
          ) : probeResult?.ok ? (
            <>
              <Button variant="quiet" onClick={resetToPick}>
                Add another
              </Button>
              <Button variant="primary" onClick={onClose}>
                Done
              </Button>
            </>
          ) : (
            <>
              <Button
                variant="danger"
                loading={removeAndRetry.isPending}
                disabled={removeAndRetry.isPending}
                onClick={() => removeAndRetry.mutate(created.connection.id)}
                className="mr-auto"
              >
                Remove and re-enter
              </Button>
              <Button
                variant="secondary"
                disabled={removeAndRetry.isPending}
                onClick={() => void runProbe(created.connection.id)}
              >
                Retry test
              </Button>
              <Button
                variant="quiet"
                disabled={removeAndRetry.isPending}
                onClick={onClose}
              >
                Keep and close
              </Button>
            </>
          )
        ) : null
      }
    >
      {step === "pick" ? (
        <div className="flex flex-col gap-3">
          <div className="relative">
            <MagnifyingGlass
              size={14}
              aria-hidden
              className="pointer-events-none absolute left-2.5 top-1/2 -translate-y-1/2 text-faint"
            />
            <input
              type="search"
              value={query}
              onChange={(event) => setQuery(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === "Escape" && query !== "") {
                  event.preventDefault();
                  setQuery("");
                }
              }}
              aria-label="Search providers"
              placeholder="Search sign-in and API-key providers…"
              autoComplete="off"
              spellCheck={false}
              className="h-9 w-full min-w-0 rounded-[var(--radius-input)] border border-[var(--color-line-strong)] bg-surface pl-8 pr-8 text-body text-fg placeholder:text-faint focus:border-[var(--color-focus-ring)] focus:outline-none focus-visible:ring-2 focus-visible:ring-[var(--color-focus-ring)]/25 [&::-webkit-search-cancel-button]:hidden"
            />
            {query !== "" ? (
              <button
                type="button"
                onClick={() => setQuery("")}
                aria-label="Clear provider search"
                className="absolute right-1 top-1/2 flex h-7 w-7 -translate-y-1/2 items-center justify-center rounded-full text-faint hover:bg-surface-3 hover:text-fg focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--color-focus-ring)]"
              >
                <X size={12} aria-hidden />
              </button>
            ) : null}
          </div>

          <div className="h-[min(48vh,380px)] overflow-y-auto pr-1">
            {noMatch ? (
              <div className="flex flex-col items-start gap-2 px-1 py-3">
                <p className="text-body text-faint">
                  No provider matches <span className="text-fg">“{query.trim()}”</span>.
                </p>
                <Button variant="quiet" size="sm" onClick={() => setQuery("")}>
                  Clear search
                </Button>
              </div>
            ) : (
              <div className="flex flex-col gap-5">
                <section className="flex flex-col gap-2">
                  <div className="flex items-baseline justify-between gap-3">
                    <h3 className="text-small font-semibold text-fg">Sign in</h3>
                    <span className="text-caption text-faint">
                      subscription or account sign-in
                    </span>
                  </div>
                  <ProviderCatalogGrid
                    empty={
                      <p className="px-1 py-2 text-body text-faint">No sign-in provider matches.</p>
                    }
                    items={signIn.map((provider) => {
                      const spec = providers.find((row) => row.id === provider.id);
                      return {
                        id: provider.id,
                        title: provider.display,
                        meta: [
                          provider.id === "openrouter"
                            ? "browser sign-in · creates an API key"
                            : isAntigravity(provider.id)
                              ? "Google Cloud Code Assist · browser sign-in"
                              : provider.flow === "device_code"
                                ? "device code"
                                : "browser sign-in",
                          spec?.compaction === "anthropic_messages"
                            ? "compaction beta"
                            : spec?.compaction === "openai_responses"
                              ? "server compact"
                              : null,
                        ]
                          .filter((part): part is string => part !== null)
                          .join(" · "),
                        scope: spec?.scope ?? "none",
                        onClick: () => onChooseOAuth(provider),
                      };
                    })}
                  />
                </section>

                <section className="flex flex-col gap-2">
                  <div className="flex items-baseline justify-between gap-3">
                    <h3 className="text-small font-semibold text-fg">API key</h3>
                    <span className="text-caption text-faint tnum">
                      {keyProviders.length} available
                    </span>
                  </div>
                  <div className="grid gap-2 sm:grid-cols-2">
                    {(
                      [
                        { protocol: "openai", body: "OpenAI-compatible · any chat API" },
                        { protocol: "anthropic", body: "Anthropic Messages · any messages API" },
                      ] as const
                    ).map((item) => (
                      <button
                        key={item.protocol}
                        type="button"
                        onClick={() => choose(item.protocol, item.protocol)}
                        className="flex items-center gap-3 rounded-[var(--radius-card)] border border-line bg-surface px-3 py-2.5 text-left transition-colors hover:bg-surface-2 focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--color-focus-ring)]"
                      >
                        <ProviderIcon provider={item.protocol} size={22} />
                        <span className="min-w-0">
                          <span className="block truncate text-body font-medium">
                            Custom endpoint
                          </span>
                          <span className="block truncate text-caption text-muted">
                            {item.body}
                          </span>
                        </span>
                      </button>
                    ))}
                  </div>
                  <ProviderCatalogGrid
                    empty={
                      <p className="px-1 py-2 text-body text-faint">
                        No API-key provider matches.
                      </p>
                    }
                    items={keyMatches.map((provider) => ({
                      id: provider.id,
                      title: provider.display,
                      meta:
                        provider.kind === "oauth"
                          ? "paste an API key · or sign in above"
                          : providerMeta(provider),
                      scope: provider.scope,
                      onClick: () => choose(provider.id, null),
                    }))}
                  />
                </section>
              </div>
            )}
          </div>

          <p className="text-caption leading-relaxed text-faint">
            Missing a provider?{" "}
            <a
              className="text-muted underline underline-offset-2 hover:text-fg"
              href="https://github.com/centopw/RRouter/issues/new?template=provider-api.yml"
              target="_blank"
              rel="noreferrer"
            >
              request an API provider
            </a>{" "}
            ·{" "}
            <a
              className="text-muted underline underline-offset-2 hover:text-fg"
              href="https://github.com/centopw/RRouter/issues/new?template=provider-oauth.yml"
              target="_blank"
              rel="noreferrer"
            >
              request an OAuth provider
            </a>
            . Keys are stored encrypted and never returned by the API.
          </p>
        </div>
      ) : null}

      {step === "credential" ? (
        <div className="flex flex-col gap-4">
          {keyProvider === "opencode" ? <OpenCodeGuide onCloseDialog={onClose} /> : null}
          <ApiKeyForm
            providers={keyProviders}
            fixedProvider={keyProvider}
            customProtocol={customProtocol}
            draft={draft}
            onDraftChange={setDraft}
            onBack={resetToPick}
            onSaved={(connection, display) => {
              setCreated({ connection, display });
              setStep("result");
              onAccountCreated(connection);
              void runProbe(connection.id);
            }}
          />
        </div>
      ) : null}

      {step === "result" && created ? (
        <AddTestResultPanel
          connection={created.connection}
          display={created.display}
          busy={probeBusy}
          result={probeResult}
          removeError={removeError}
        />
      ) : null}
    </Modal>
  );
}

function AddTestResultPanel({
  connection,
  display,
  busy,
  result,
  removeError,
}: {
  connection: Connection;
  display: string;
  busy: boolean;
  result: ConnectionTestResult | null;
  removeError: unknown;
}) {
  const accountName = connection.name ?? display;

  if (busy || !result) {
    return (
      <div className="flex flex-col gap-3" role="status" aria-live="polite">
        <div className="flex items-center gap-3 rounded-lg border border-line bg-surface-2 px-4 py-3.5">
          <ProviderIcon provider={connection.provider} size={24} />
          <div className="min-w-0 flex-1">
            <p className="truncate text-body font-medium text-fg">{accountName}</p>
            <p className="mt-0.5 flex items-center gap-1.5 text-caption text-muted">
              <Dot tone="muted" />
              <span>Account saved — running a live test request…</span>
            </p>
          </div>
        </div>
      </div>
    );
  }

  if (result.ok) {
    return (
      <div className="flex flex-col gap-3" role="status" aria-live="polite">
        <div className="flex items-center gap-3 rounded-lg border border-[var(--color-pos-line)] bg-[var(--color-pos-tint)] px-4 py-3.5">
          <ProviderIcon provider={connection.provider} size={26} />
          <div className="min-w-0 flex-1">
            <p className="truncate text-body font-medium text-fg">{accountName}</p>
            <p className="mt-0.5 text-caption text-[var(--color-pos-ink)] tnum">
              Works · HTTP {result.status} · {ms(result.latency_ms)}
            </p>
          </div>
          <CheckCircle size={22} aria-hidden className="shrink-0 text-[var(--color-pos)]" />
        </div>
        <p className="text-caption leading-relaxed text-faint">
          Requests can route through this account now. Credentials stay encrypted on the gateway
          and are never returned by the API.
        </p>
      </div>
    );
  }

  return (
    <div className="flex flex-col gap-3" role="alert">
      <div className="flex items-start gap-3 rounded-lg border border-[var(--color-neg-line)] bg-[var(--color-neg-tint)] px-4 py-3.5">
        <WarningCircle
          size={20}
          aria-hidden
          className="mt-0.5 shrink-0 text-[var(--color-neg)]"
        />
        <div className="min-w-0 flex-1">
          <p className="text-body font-medium text-[var(--color-neg-ink)]">
            Saved, but the live test failed{result.status ? ` · HTTP ${result.status}` : ""}
          </p>
          {result.error ? (
            <p className="mt-1 break-words font-mono text-caption text-[var(--color-neg-ink)]">
              {result.error}
            </p>
          ) : null}
        </div>
      </div>
      <p className="text-caption leading-relaxed text-muted">
        The account was saved so the gateway could probe it. Remove and re-enter the key if there
        was a typo, retry the test, or keep it and adjust its endpoint later.
      </p>
      {removeError ? <ErrorNote error={removeError} /> : null}
    </div>
  );
}

function ProviderCatalogGrid({
  items,
  empty,
}: {
  items: Array<{
    id: string;
    title: string;
    meta: string;
    /** `global` = shared account, `team` = a team-owned account, `none` = nothing. */
    scope: ModelProvider["scope"];
    onClick: () => void;
  }>;
  empty: ReactNode;
}) {
  if (items.length === 0) return <>{empty}</>;
  return (
    <ul className="grid gap-2 sm:grid-cols-2">
      {items.map((item) => (
        <li key={item.id} className="min-w-0">
          <button
            type="button"
            onClick={item.onClick}
            className="flex h-full w-full items-center gap-3 rounded-lg border border-line bg-surface-2 px-3 py-2.5 text-left transition-colors hover:border-[var(--color-line-strong)] hover:bg-[var(--color-selected)] focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--color-focus-ring)]"
          >
            <ProviderIcon provider={item.id} size={20} />
            <span className="min-w-0 flex-1">
              <span className="flex flex-wrap items-center gap-1.5">
                <span className="truncate text-body font-medium text-fg">{item.title}</span>
                {item.scope === "global" ? (
                  <Badge tone="neutral">connected · shared</Badge>
                ) : item.scope === "team" ? (
                  <Badge tone="neutral">connected · team</Badge>
                ) : null}
              </span>
              <span className="mt-0.5 block truncate text-caption text-faint">{item.meta}</span>
            </span>
            <span className="shrink-0 text-caption text-faint" aria-hidden>
              →
            </span>
          </button>
        </li>
      ))}
    </ul>
  );
}

/** OpenCode plan reference, folded away so the key input stays first on screen. */
function OpenCodeGuide({ onCloseDialog }: { onCloseDialog: () => void }) {
  return (
    <details className="rounded-[var(--radius-card)] border border-line bg-surface-2/60">
      <summary className="cursor-pointer list-none px-3 py-2 text-small font-medium text-fg focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--color-focus-ring)]">
        OpenCode plans — free models, pay-as-you-go, Go
      </summary>
      <div className="flex flex-col gap-2 border-t border-line px-3 py-3">
        <p className="text-caption leading-relaxed text-muted">
          OpenCode serves its models over an OpenAI-compatible API. For zero-price free models,
          use{" "}
          <Link
            to="/free-mode"
            onClick={onCloseDialog}
            className="font-medium text-[var(--color-accent-text)] underline-offset-2 hover:underline"
          >
            Free mode
          </Link>{" "}
          — it discovers and routes them automatically. Add an OpenCode Console API key here for
          pay-as-you-go credits or a Go plan.
        </p>
        {[
          {
            title: "Pay as you go",
            meta: "Console credits",
            body: "Buy credits in the OpenCode Console and create an API key. Billing is per request, with no monthly subscription.",
          },
          {
            title: "Go / Go Plus / Go Teams",
            meta: "$10 / $40 / $20 per member monthly",
            body: "Subscription access to the Go model list with 5-hour, weekly and monthly allowances.",
          },
        ].map((plan) => (
          <div key={plan.title} className="text-caption leading-relaxed">
            <span className="font-medium text-fg">{plan.title}</span>
            <span className="text-faint"> · {plan.meta}</span>
            <span className="block text-muted">{plan.body}</span>
          </div>
        ))}
        <div className="flex flex-wrap items-center gap-3 text-caption">
          <a
            className="text-muted underline underline-offset-2 hover:text-fg"
            href="https://opencode.ai/console"
            target="_blank"
            rel="noreferrer"
          >
            Create an OpenCode API key →
          </a>
          <a
            className="text-muted underline underline-offset-2 hover:text-fg"
            href="https://opencode.ai/docs/go"
            target="_blank"
            rel="noreferrer"
          >
            Go plans and limits
          </a>
        </div>
      </div>
    </details>
  );
}

function ApiKeyForm({
  providers,
  fixedProvider,
  customProtocol,
  draft,
  onDraftChange,
  onBack,
  onSaved,
}: {
  providers: ModelProvider[];
  fixedProvider: string | null;
  customProtocol: "openai" | "anthropic" | null;
  draft: DraftFields;
  onDraftChange: (next: DraftFields) => void;
  onBack: () => void;
  onSaved: (connection: Connection, display: string) => void;
}) {
  const { invalidate } = useInvalidateProviders();
  const teams = useTeams();
  const teamList = teams.data ?? [];
  const [provider, setProvider] = useState(fixedProvider ?? providers[0]?.id ?? "");
  const [key, setKey] = useState("");
  const [overrideOpen, setOverrideOpen] = useState(false);
  const [code, setCode] = useState("");
  const [codeError, setCodeError] = useState<unknown>(null);
  const [codeNotice, setCodeNotice] = useState<string | null>(null);
  const [error, setError] = useState<unknown>(null);

  useEffect(() => {
    if (fixedProvider) setProvider(fixedProvider);
  }, [fixedProvider]);

  const selected = providers.find((p) => p.id === (fixedProvider ?? provider));
  const selectedId = selected?.id ?? "";
  useEffect(() => {
    setCode(providers.find((p) => p.id === selectedId)?.code ?? "");
    setCodeError(null);
    setCodeNotice(null);
    // `providers` is deliberately not a dependency so saving a code keeps the confirmation.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [selectedId]);

  const fixedDisplay = selected?.display;
  const requiresBaseUrl = selected?.requires_base_url_override === true;
  const example = selected?.base_url_example ?? "https://<resource>.openai.azure.com";
  const baseProblem = baseUrlProblem(draft.baseUrl, requiresBaseUrl);
  const isCustom = customProtocol !== null;
  const protocol = customProtocol === null ? null : protocolLabel(customProtocol);
  const publishedDefault = publishedDefaultUrl(selected);
  const effectiveUrl = draft.baseUrl.trim() !== "" ? draft.baseUrl.trim() : publishedDefault;
  const codeIssue = codeProblemFor(code, providers, selected?.id ?? null);
  const providerName = draft.name.trim();
  const needsName = isCustom && providerName === "";
  const needsBaseUrl = isCustom && draft.baseUrl.trim() === "";

  const save = useMutation({
    mutationFn: () =>
      api.post<Connection>("/api/connections", {
        provider,
        auth_type: "api_key",
        name: isCustom ? providerName : providerName || null,
        team_id: draft.team === "" ? null : draft.team,
        credentials: {
          api_key: key,
          ...(draft.baseUrl.trim() ? { base_url_override: draft.baseUrl.trim() } : {}),
        },
      }),
    onSuccess: (conn) => {
      setKey("");
      onSaved(conn, isCustom ? providerName : (fixedDisplay ?? conn.provider));
    },
    onError: setError,
  });

  const saveCode = useMutation({
    mutationFn: () => {
      if (!selected) return Promise.reject(new Error("no provider selected"));
      return api.patch<ModelProvider>(
        `/api/models/providers/${encodeURIComponent(selected.id)}`,
        { code: code.trim() },
      );
    },
    onSuccess: () => {
      setCodeError(null);
      setCodeNotice(`Code saved — refs now read ${code.trim()}/…`);
      invalidate();
    },
    onError: (err) => {
      setCodeNotice(null);
      setCodeError(err);
    },
  });

  const ready =
    provider !== "" &&
    key !== "" &&
    !needsName &&
    !needsBaseUrl &&
    baseProblem === null &&
    !save.isPending;

  return (
    <form
      className="flex flex-col gap-3"
      onSubmit={(e) => {
        e.preventDefault();
        if (!ready) return;
        save.mutate();
      }}
    >
      <Field
        label={isCustom ? "Protocol" : "Provider"}
        hint={
          isCustom
            ? `${protocol} wire shape; models are added on the Models page.`
            : fixedProvider === "opencode"
              ? "OpenCode Console key for pay-as-you-go or Go. Free models live under Free mode."
              : undefined
        }
      >
        {fixedProvider ? (
          <div className="flex h-9 items-center gap-2 rounded-lg border border-line bg-surface-2 px-3 text-body text-muted">
            <ProviderIcon provider={fixedProvider} size={16} />
            {isCustom ? protocol : (fixedDisplay ?? fixedProvider)}
          </div>
        ) : (
          <Select value={provider} onChange={setProvider} ariaLabel="api key provider">
            {providers.map((p) => (
              <option key={p.id} value={p.id}>
                {p.display} · {p.code}
              </option>
            ))}
          </Select>
        )}
      </Field>

      {/* API key comes first and takes focus immediately so pasting a key is one gesture. */}
      <Field label="API key" hint="Stored encrypted; tested immediately after saving.">
        <Input
          value={key}
          onChange={(next) => {
            setKey(next);
            setError(null);
          }}
          type="password"
          placeholder="sk-…"
          ariaLabel="api key"
          autoFocus
        />
      </Field>

      {isCustom ? (
        <Field
          label="Provider name"
          hint={`Shown in Providers and Models; the protocol stays ${protocol}.`}
        >
          <Input
            value={draft.name}
            onChange={(name) => onDraftChange({ ...draft, name })}
            placeholder="Acme Gateway"
            ariaLabel="provider name"
          />
        </Field>
      ) : null}

      {isCustom || requiresBaseUrl ? (
        <Field
          label={requiresBaseUrl ? "Resource base URL" : "Base URL"}
          hint={
            requiresBaseUrl
              ? `Your deployment's endpoint, for example ${example}. The gateway keeps the provider's API path.`
              : "Endpoint root that serves this protocol."
          }
          error={baseProblem ?? undefined}
        >
          <Input
            value={draft.baseUrl}
            onChange={(baseUrl) => {
              onDraftChange({ ...draft, baseUrl });
              setError(null);
            }}
            placeholder={requiresBaseUrl ? example : "https://api.example.com/v1"}
            ariaLabel="base url"
            mono
          />
        </Field>
      ) : null}

      {!isCustom ? (
        <Field label="Label" optional hint="How this account appears in the list.">
          <Input
            value={draft.name}
            onChange={(name) => onDraftChange({ ...draft, name })}
            placeholder="production-key"
            ariaLabel="account label"
          />
        </Field>
      ) : null}

      {teamList.length > 0 ? (
        <Field
          label="Owner"
          hint="Team accounts serve that team's keys; shared serves instance-wide keys."
        >
          <Select
            value={draft.team}
            onChange={(team) => onDraftChange({ ...draft, team })}
            ariaLabel="connection owner team"
          >
            <option value="">Shared / instance-wide</option>
            {teamList.map((row) => (
              <option key={row.id} value={row.id}>
                {row.name}
              </option>
            ))}
          </Select>
        </Field>
      ) : null}

      <details className="group rounded-lg border border-line bg-surface-2/60">
        <summary className="flex cursor-pointer list-none items-center justify-between gap-3 rounded-lg px-3 py-2 text-small font-medium text-fg focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--color-focus-ring)]">
          <span>Advanced</span>
          <span className="text-caption font-normal text-faint">endpoint · provider code</span>
        </summary>
        <div className="flex flex-col gap-3 border-t border-line px-3 py-3">
          {!isCustom && !requiresBaseUrl ? (
            <div className="flex min-w-0 flex-col gap-1">
              <span className="text-caption font-semibold text-ink-soft">
                Endpoint this account calls
              </span>
              <span className="break-all font-mono text-small text-fg">
                {effectiveUrl ?? <span className="text-faint">endpoint not published</span>}
              </span>
              <span className="text-caption text-faint">
                {draft.baseUrl.trim() !== ""
                  ? "This account's override"
                  : publishedDefault
                    ? "The provider's published endpoint"
                    : "The gateway uses its compiled route for this provider."}
              </span>
              {overrideOpen || draft.baseUrl.trim() !== "" ? (
                <div className="mt-2">
                  <Field
                    label="Base URL override"
                    optional
                    hint="For a self-hosted or proxy endpoint."
                    error={baseProblem ?? undefined}
                  >
                    <Input
                      value={draft.baseUrl}
                      onChange={(baseUrl) => {
                        onDraftChange({ ...draft, baseUrl });
                        setError(null);
                      }}
                      placeholder="https://api.example.com/v1"
                      ariaLabel="base url override"
                      mono
                    />
                  </Field>
                </div>
              ) : (
                <button
                  type="button"
                  onClick={() => setOverrideOpen(true)}
                  className="mt-0.5 self-start text-caption text-muted underline underline-offset-2 hover:text-fg"
                >
                  Customise for this account
                </button>
              )}
            </div>
          ) : null}

          <div
            className={
              !isCustom && !requiresBaseUrl
                ? "flex flex-col gap-2 border-t border-line pt-3"
                : "flex flex-col gap-2"
            }
          >
            <Field
              label="Provider code"
              hint={`The ${selected?.display ?? "provider"} segment in group/code/model`}
            >
              <div className="flex items-center gap-2">
                <Input
                  value={code}
                  invalid={Boolean(codeIssue)}
                  onChange={(next) => {
                    setCode(next);
                    setCodeError(null);
                    setCodeNotice(null);
                  }}
                  placeholder={selected?.code ?? "code"}
                  ariaLabel="provider code"
                  disabled={!selected || saveCode.isPending}
                  mono
                  className="max-w-[180px]"
                  onKeyDown={(event) => {
                    if (event.key !== "Enter") return;
                    event.preventDefault();
                    if (
                      !selected ||
                      saveCode.isPending ||
                      codeIssue !== null ||
                      code.trim() === (selected.code ?? "")
                    ) {
                      return;
                    }
                    saveCode.mutate();
                  }}
                />
                <Button
                  variant="quiet"
                  disabled={
                    !selected ||
                    saveCode.isPending ||
                    codeIssue !== null ||
                    code.trim() === (selected.code ?? "")
                  }
                  onClick={() => saveCode.mutate()}
                >
                  {saveCode.isPending ? "Saving…" : "Save code"}
                </Button>
              </div>
            </Field>
            <p className="text-caption text-faint">
              lowercase a–z, 0–9, “-”, “_” or “.” · 1–16 characters · unique across providers.
              Changing it changes every ref built from this provider.
            </p>
            {codeIssue ? (
              <span role="alert" className="text-caption text-neg">
                {codeIssue}
              </span>
            ) : null}
            {codeNotice ? (
              <span role="status" className="text-caption text-[var(--color-pos-ink)]">
                {codeNotice}
              </span>
            ) : null}
            {codeError ? <ErrorNote error={codeError} /> : null}
          </div>
        </div>
      </details>

      {error ? <ErrorNote error={error} /> : null}
      {isCustom ? (
        <p className="text-caption leading-relaxed text-faint">
          This account is added to the {customProtocol} provider. To give an endpoint its own name
          and model ref code, create it in Provider codes first.
        </p>
      ) : null}

      <div className="flex items-center justify-between gap-2 border-t border-line pt-4">
        <Button variant="quiet" onClick={onBack} disabled={save.isPending}>
          Back
        </Button>
        <Button type="submit" variant="primary" loading={save.isPending} disabled={!ready}>
          {isCustom ? "Add endpoint and test" : "Save and test"}
        </Button>
      </div>
    </form>
  );
}

/**
 * Every provider's shorthand, editable in place, plus operator-defined providers.
 *
 * A client-facing model ref is `group/code/model`; the code is what makes a ref short
 * without making it ambiguous, so it is unique across compiled and custom providers.
 */
export function ProviderCodesCard({
  providers,
  embedded = false,
}: {
  providers: ModelProvider[];
  /** Inside the codes dialog: no outer card chrome, and no repeated title. */
  embedded?: boolean;
}) {
  const { invalidate } = useInvalidateProviders();
  const [editing, setEditing] = useState<string | null>(null);
  const [draft, setDraft] = useState("");
  const [confirming, setConfirming] = useState<string | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [creating, setCreating] = useState(false);
  const [form, setForm] = useState({
    name: "",
    code: "",
    base_url: "",
    dialect: "openai" as "openai" | "anthropic",
  });

  const save = useMutation({
    mutationFn: (row: { id: string; code: string }) =>
      api.patch(`/api/models/providers/${encodeURIComponent(row.id)}`, { code: row.code }),
    onSuccess: () => {
      setEditing(null);
      setError(null);
      invalidate();
    },
    onError: setError,
  });

  const create = useMutation({
    mutationFn: () =>
      api.post("/api/models/providers", {
        name: form.name.trim(),
        ...(form.code.trim() ? { code: form.code.trim() } : {}),
        base_url: form.base_url.trim(),
        dialect: form.dialect,
      }),
    onSuccess: () => {
      setCreating(false);
      setForm({ name: "", code: "", base_url: "", dialect: "openai" });
      setError(null);
      invalidate();
    },
    onError: setError,
  });

  const remove = useMutation({
    mutationFn: (id: string) => api.del(`/api/models/providers/${encodeURIComponent(id)}`),
    onSuccess: () => {
      setError(null);
      invalidate();
    },
    onError: setError,
  });

  const createReady = form.name.trim() !== "" && form.base_url.trim() !== "" && !create.isPending;
  const newProviderButton = (
    <Button variant="quiet" onClick={() => setCreating((open) => !open)}>
      {creating ? "Cancel" : "New provider"}
    </Button>
  );

  return (
    <div className={embedded ? "" : "mb-4"}>
      <Card className={embedded ? "border-0" : undefined}>
        {embedded ? (
          <div className="flex justify-end pb-3">{newProviderButton}</div>
        ) : (
          <CardHeader title="Provider codes" action={newProviderButton} />
        )}
        <div className={embedded ? "" : "px-5 pb-4"}>
          {embedded ? null : (
            <p className="mb-3 text-small text-muted">
              A model reference is <span className="font-mono">group/code/model</span>; the code is
              unique across every provider, so two providers serving the same model id can never be
              confused.
            </p>
          )}
          {error ? (
            <div className="mb-3">
              <ErrorNote error={error} />
            </div>
          ) : null}
          {creating ? (
            <form
              className="mb-4 grid gap-3 rounded-[var(--radius-card)] border border-line bg-surface-2/60 p-3 sm:grid-cols-2"
              onSubmit={(event) => {
                event.preventDefault();
                if (createReady) create.mutate();
              }}
            >
              <Field label="Name" hint="Shown in every provider list">
                <Input
                  value={form.name}
                  onChange={(name) => setForm((f) => ({ ...f, name }))}
                  placeholder="Acme Gateway"
                  ariaLabel="provider name"
                />
              </Field>
              <Field label="Code" hint="Optional; derived from the name">
                <Input
                  value={form.code}
                  onChange={(code) => setForm((f) => ({ ...f, code }))}
                  placeholder="acme"
                  ariaLabel="provider code"
                  mono
                />
              </Field>
              <Field label="Base URL" hint="Origin or /v1 root of the chat API">
                <Input
                  value={form.base_url}
                  onChange={(base_url) => setForm((f) => ({ ...f, base_url }))}
                  placeholder="https://api.acme.example/v1"
                  ariaLabel="provider base url"
                  mono
                />
              </Field>
              <Field label="Protocol" hint="The wire shape the endpoint speaks">
                <Select
                  value={form.dialect}
                  onChange={(dialect) =>
                    setForm((f) => ({ ...f, dialect: dialect as "openai" | "anthropic" }))
                  }
                  ariaLabel="provider protocol"
                >
                  <option value="openai">OpenAI-compatible</option>
                  <option value="anthropic">Anthropic Messages</option>
                </Select>
              </Field>
              <div className="flex justify-end sm:col-span-2">
                <Button type="submit" variant="primary" loading={create.isPending} disabled={!createReady}>
                  Add provider
                </Button>
              </div>
            </form>
          ) : null}
          <ul className="divide-y divide-line">
            {providers.map((provider) => (
              <li
                key={provider.id}
                className="flex flex-wrap items-center gap-2 py-2 first:pt-0 last:pb-0"
              >
                <ProviderIcon provider={provider.id} size={17} />
                <span className="min-w-0 break-words font-medium">{provider.display}</span>
                <span className="font-mono text-caption text-faint">{provider.id}</span>
                {provider.custom ? <Badge tone="accent">custom</Badge> : null}
                <span className="ml-auto flex items-center gap-2">
                  {editing === provider.id ? (
                    <>
                      <Input
                        value={draft}
                        onChange={setDraft}
                        ariaLabel={`Code for ${provider.display}`}
                        mono
                        className="w-28"
                        onKeyDown={(event) => {
                          if (event.key === "Enter" && !save.isPending && draft.trim() !== "") {
                            save.mutate({ id: provider.id, code: draft.trim() });
                          } else if (event.key === "Escape") {
                            setEditing(null);
                          }
                        }}
                      />
                      <button
                        type="button"
                        className="text-caption underline underline-offset-2 disabled:opacity-50"
                        disabled={save.isPending || draft.trim() === ""}
                        onClick={() => save.mutate({ id: provider.id, code: draft.trim() })}
                      >
                        save
                      </button>
                      <button
                        type="button"
                        className="text-caption text-muted underline underline-offset-2"
                        onClick={() => setEditing(null)}
                      >
                        cancel
                      </button>
                    </>
                  ) : (
                    <>
                      <code className="rounded bg-surface-3 px-1.5 py-0.5 font-mono text-caption">
                        {provider.code}
                      </code>
                      <button
                        type="button"
                        className="text-caption underline underline-offset-2"
                        onClick={() => {
                          setEditing(provider.id);
                          setDraft(provider.code);
                          setError(null);
                        }}
                      >
                        edit
                      </button>
                      {provider.custom ? (
                        <button
                          type="button"
                          className="text-caption text-neg underline underline-offset-2 disabled:opacity-50"
                          disabled={remove.isPending}
                          onClick={() => {
                            if (confirming === provider.id) {
                              setConfirming(null);
                              remove.mutate(provider.id);
                            } else {
                              setConfirming(provider.id);
                            }
                          }}
                        >
                          {confirming === provider.id ? "confirm remove" : "remove"}
                        </button>
                      ) : null}
                    </>
                  )}
                </span>
              </li>
            ))}
          </ul>
        </div>
      </Card>
    </div>
  );
}
