import { useEffect, useMemo, useRef, useState } from "react";
import { Check } from "@phosphor-icons/react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useNavigate } from "react-router-dom";
import {
  Button,
  CopyButton,
  ErrorNote,
  Field,
  Input,
  Select,
  StatusPill,
  cx,
} from "../../components/ui";
import {
  api,
  list,
  type Connection,
  type ModelTest,
  type NewKey,
  type OAuthProvider,
  type UsageEvent,
} from "../../lib/api";
import {
  qk,
  useConnections,
  useModelProviders,
  useModels,
  useOAuthProviders,
} from "../../lib/queries";
import { compact, ms, splitRef } from "../../lib/format";
import { ApiKeyStep, DeviceStep, PkceFailedStep, PkceStep } from "./signin";
import { SNIPPET_LANGS, snippetFor, type SnippetLang } from "./snippets";

/** First-run wizard: connect an account, mint a client key, watch the first request. */

type Step =
  | "choose"
  | "pkce"
  | "pkce-failed"
  | "device"
  | "apikey"
  | "connected"
  | "key"
  | "key-created"
  | "request"
  | "request-done";

type Account = {
  /** Connection id. */
  id: string;
  provider: string;
  label: string;
};

const MACRO: Record<Step, 1 | 2 | 3> = {
  choose: 1,
  pkce: 1,
  "pkce-failed": 1,
  device: 1,
  apikey: 1,
  connected: 1,
  key: 2,
  "key-created": 2,
  request: 3,
  "request-done": 3,
};

const STEP_TITLES = ["Connect an account", "Create a client key", "Send a request"];
const STEP_HELP = [
  "Sign in or paste an API key",
  "So your apps can call the gateway",
  "Confirm it works end to end",
];

const DISMISSED = "rrouter:setup-dismissed";

export default function Setup() {
  const navigate = useNavigate();
  const [step, setStep] = useState<Step>("choose");
  const [account, setAccount] = useState<Account | null>(null);
  const [key, setKey] = useState<NewKey | null>(null);
  const [signInKey, setSignInKey] = useState(0);
  const [signInError, setSignInError] = useState<unknown>(null);
  const [firstRequest, setFirstRequest] = useState<UsageEvent | null>(null);
  const [requestStarted, setRequestStarted] = useState<number | null>(null);
  const [probe, setProbe] = useState<ModelTest | null>(null);
  const popup = useRef<Window | null>(null);

  const oauth = useOAuthProviders();
  const providers = useModelProviders();
  const claude = oauth.data?.find((p) => p.id === "claude");
  const codex = oauth.data?.find((p) => p.id === "codex");
  const keyProviders = (providers.data ?? []).filter((p) => p.kind === "api_key");

  const macro = MACRO[step];
  const baseUrl = `${window.location.origin}/v1`;

  const finish = (to: "/activity") => {
    window.localStorage.setItem(DISMISSED, "1");
    navigate(to);
  };

  const skip = () => {
    popup.current?.close();
    finish("/activity");
  };

  const back = () => {
    popup.current?.close();
    popup.current = null;
    if (step === "key") setStep(account ? "connected" : "choose");
    else if (step === "request") setStep(key ? "key-created" : "key");
    else if (step === "key-created" || step === "connected") setStep(step === "key-created" ? "key" : "choose");
    else setStep("choose");
  };

  const backLabel =
    step === "request" ? "Back" : step === "key-created" ? "Back" : step === "key" ? "Back" : "Choose another way";

  const connected = (id: string, provider: string, label: string) => {
    setAccount({ id, provider, label });
    setProbe(null);
    setStep("connected");
  };

  return (
    <div className="min-h-full bg-bg text-fg">
      <header className="flex h-16 shrink-0 items-center gap-3 border-b border-white/10 bg-[var(--color-sidebar)] px-4 min-[820px]:px-7">
        <img
          src={`${import.meta.env.BASE_URL}logo-mark-light.svg`}
          alt=""
          width={26}
          height={26}
          className="h-[26px] w-[26px] shrink-0 select-none"
        />
        <span className="text-[14px] font-semibold tracking-[-0.01em] text-white">RRouter</span>
        <span aria-hidden className="h-[18px] w-px bg-white/20" />
        <span className="text-[14px] text-white/60">Setup</span>
        <span className="ml-auto hidden text-[12.5px] text-white/50 min-[820px]:inline">
          About 2 minutes · finish whenever you like
        </span>
        <Button variant="quiet" onClick={skip}>
          Skip setup
        </Button>
      </header>

      <div className="mx-auto flex w-full max-w-[1280px] flex-wrap items-stretch">
        <aside className="hidden w-[300px] shrink-0 border-r border-line bg-[var(--color-sidebar)] px-7 py-9 min-[820px]:block">
          <Rail step={step} account={account} keyCreated={key !== null} firstRequest={firstRequest} />
          <p className="mt-8 text-[12.5px] leading-relaxed text-white/45">
            Self-hosted. RRouter runs on this machine and talks to your providers directly.
          </p>
        </aside>

        <main className="min-w-0 flex-1 px-4 py-6 min-[820px]:px-14 min-[820px]:pb-16 min-[820px]:pt-12">
          <div className="mx-auto w-full max-w-[880px]">
            <MobileProgress step={step} />
            <div className="mb-2.5 hidden text-[13px] text-faint min-[820px]:block">
              Step {macro} of 3
            </div>
            <h1 className="text-[26px] font-semibold tracking-[-0.02em] min-[820px]:text-[30px]">
              {stepTitle(step)}
            </h1>
            <p className="mt-2.5 max-w-[60ch] text-[15px] text-muted">{stepLead(step)}</p>

            <div className="mt-7">
              {step === "choose" ? (
                <ChooseStep
                  claude={claude}
                  codex={codex}
                  onClaude={() => {
                    popup.current = window.open("about:blank", "rrouter-signin");
                    setSignInKey((n) => n + 1);
                    setSignInError(null);
                    setStep("pkce");
                  }}
                  onCodex={() => {
                    setSignInKey((n) => n + 1);
                    setSignInError(null);
                    setStep("device");
                  }}
                  onApiKey={() => setStep("apikey")}
                  onSkip={() => setStep("key")}
                />
              ) : null}

              {step === "pkce" && claude ? (
                <PkceStep
                  key={signInKey}
                  provider={claude}
                  popup={popup.current}
                  onConnected={(id, label) => connected(id, claude.id, label)}
                  onFail={(error) => {
                    setSignInError(error);
                    setStep("pkce-failed");
                  }}
                />
              ) : null}

              {step === "pkce-failed" ? (
                <PkceFailedStep
                  error={signInError}
                  onRetry={() => {
                    popup.current = window.open("about:blank", "rrouter-signin");
                    setSignInKey((n) => n + 1);
                    setSignInError(null);
                    setStep("pkce");
                  }}
                  onChoose={() => setStep("choose")}
                />
              ) : null}

              {step === "device" && codex ? (
                <DeviceStep
                  key={signInKey}
                  provider={codex}
                  onConnected={(id, label) => connected(id, codex.id, label)}
                  onFail={(error) => {
                    setSignInError(error);
                    setStep("choose");
                  }}
                  onNewCode={() => setSignInKey((n) => n + 1)}
                />
              ) : null}

              {step === "apikey" ? (
                <ApiKeyStep
                  providers={keyProviders}
                  pending={providers.isLoading}
                  error={providers.error}
                  onRetryProviders={() => void providers.refetch()}
                  onConnected={(connection: Connection) =>
                    connected(connection.id, connection.provider, connection.name ?? connection.provider)
                  }
                />
              ) : null}

              {step === "connected" && account ? (
                <ConnectedStep
                  account={account}
                  probe={probe}
                  onProbe={setProbe}
                  onContinue={() => setStep("key")}
                  onAnother={() => setStep("choose")}
                />
              ) : null}

              {step === "key" ? (
                <KeyStep
                  onBack={() => setStep(account ? "connected" : "choose")}
                  onCreated={(created) => {
                    setKey(created);
                    setStep("key-created");
                  }}
                />
              ) : null}

              {step === "key-created" && key ? (
                <KeyCreatedStep
                  created={key}
                  baseUrl={baseUrl}
                  onContinue={() => {
                    setRequestStarted(Math.floor(Date.now() / 1000));
                    setStep("request");
                  }}
                />
              ) : null}

              {step === "request" ? (
                <RequestStep
                  apiKey={key?.key ?? ""}
                  baseUrl={baseUrl}
                  account={account}
                  startedAt={requestStarted ?? Math.floor(Date.now() / 1000)}
                  onFirst={(event) => {
                    setFirstRequest(event);
                    setStep("request-done");
                  }}
                  onBack={() => setStep(key ? "key-created" : "key")}
                />
              ) : null}

              {step === "request-done" ? (
                <DoneStep
                  event={firstRequest}
                  account={account}
                  onActivity={() => finish("/activity")}
                  onProviders={() => navigate("/providers")}
                   onCombos={() => navigate("/combos")}
                  onAliases={() => navigate("/aliases")}
                />
              ) : null}
            </div>

            <footer className="mt-9 flex flex-wrap items-center justify-between gap-3 border-t border-line pt-6 text-[12.5px] text-faint">
              <span className="flex flex-wrap items-center gap-3">
                {step !== "choose" && step !== "request-done" && step !== "key" && step !== "request" ? (
                  <Button onClick={back}>{backLabel}</Button>
                ) : null}
                <span>
                {step === "request"
                    ? "Listening for your first request."
                    : "You can rerun this from Health at any time."}
                </span>
              </span>
              <button type="button" onClick={skip} className="underline underline-offset-4 hover:text-fg">
                Finish later
              </button>
            </footer>
          </div>
        </main>
      </div>
    </div>
  );
}

function stepTitle(step: Step): string {
  switch (step) {
    case "choose":
      return "Connect your first account";
    case "pkce":
      return "Approve access in your browser";
    case "pkce-failed":
      return "Approval didn't go through";
    case "device":
      return "Enter this code on the Codex page";
    case "apikey":
      return "Add an API key";
    case "connected":
      return "Account connected";
    case "key":
      return "Create a key for your apps";
    case "key-created":
      return "This is the only time you'll see this key";
    case "request":
      return "Send a request and watch it arrive";
    case "request-done":
      return "You're live";
  }
}

function stepLead(step: Step): string {
  switch (step) {
    case "choose":
      return "RRouter routes requests through accounts you already have. Choose how to sign in. You can add more at any time.";
    case "pkce":
      return "We opened Claude in a new tab. Approve access there and this page continues on its own.";
    case "pkce-failed":
      return "Nothing was saved. Try again, or pick a different way to connect.";
    case "device":
      return "Open the page on any device, sign in, and type the code below. This screen continues when you approve.";
    case "apikey":
      return "Paste a key from your provider. We make one tiny request to check it before saving anything.";
    case "connected":
      return "We sent one test request through it and it worked. Here is what you can route to now.";
    case "key":
      return "Your tools use this key to call RRouter. It is separate from your provider accounts, and RRouter stores only a hash of it.";
    case "key-created":
      return "Copy it now and keep it somewhere safe. If you lose it, create a new one.";
    case "request":
      return "Run the snippet from your terminal or app. This page reacts the moment the gateway receives it.";
    case "request-done":
      return "The gateway received your first request and routed it through your account. Everything from here shows up in Activity.";
  }
}

function ChooseStep({
  claude,
  codex,
  onClaude,
  onCodex,
  onApiKey,
  onSkip,
}: {
  claude: OAuthProvider | undefined;
  codex: OAuthProvider | undefined;
  onClaude: () => void;
  onCodex: () => void;
  onApiKey: () => void;
  onSkip: () => void;
}) {
  return (
    <div>
      <div className="grid gap-3.5 md:grid-cols-2 xl:grid-cols-3">
        <Option
          title="Claude"
          tag="Browser sign-in"
          body="Use your Claude subscription. You approve access in a new browser tab. No key needed."
          action="Sign in"
          onClick={claude ? onClaude : undefined}
          icon="M12 3l8 4.5v9L12 21l-8-4.5v-9L12 3z"
        />
        <Option
          title="Codex"
          tag="Device code"
          body="Use your Codex subscription. Type a short code on the provider's page. Works on a server with no browser."
          action="Get a code"
          onClick={codex ? onCodex : undefined}
          icon="M4 7h16v10H4zM8 21h8"
        />
        <Option
          title="API key"
          tag="Paste a key"
          body="Use a key from any supported provider. The provider bills you per token."
          action="Add a key"
          onClick={onApiKey}
          icon="M15 7a4 4 0 11-3.5 5.9L4 20H2v-3l8.1-8.1A4 4 0 0115 7z"
        />
      </div>

      <details className="mt-6 rounded-[var(--radius-card)] border border-line bg-surface">
        <summary className="cursor-pointer px-5 py-3.5 text-[13.5px] font-medium">
          Which one should I pick?
        </summary>
        <p className="border-t border-rule px-5 py-4 text-[13.5px] text-muted">
          A subscription uses the plan you already pay for and needs no key, which makes it the
          fastest start. An API key is billed per token and suits servers and shared setups. Both
          work the same once connected.
        </p>
      </details>

      <button
        type="button"
        onClick={onSkip}
        className="mt-5 text-[13.5px] text-muted underline underline-offset-4 hover:text-fg"
      >
        Skip for now. I'll connect later
      </button>
    </div>
  );
}

function Option({
  title,
  tag,
  body,
  action,
  onClick,
  icon,
}: {
  title: string;
  tag: string;
  body: string;
  action: string;
  onClick?: () => void;
  icon: string;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      disabled={!onClick}
      className="flex min-h-[210px] flex-col items-start gap-2.5 rounded-[var(--radius-card)] border border-line bg-surface p-5 text-left transition-colors hover:border-line-strong hover:bg-[var(--color-row-hover)] disabled:cursor-not-allowed disabled:opacity-50"
    >
      <span className="flex h-10 w-10 items-center justify-center rounded-[10px] border border-line bg-surface-2 text-fg">
        <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
          <path d={icon} />
        </svg>
      </span>
      <span className="flex flex-wrap items-center gap-2">
        <span className="text-[17px] font-semibold">{title}</span>
        <span className="rounded-full border border-line px-2.5 py-0.5 text-[12px] text-muted">{tag}</span>
      </span>
      <span className="text-[13.5px] text-muted">{body}</span>
      <span className="mt-auto inline-flex items-center gap-2 text-[13.5px] font-medium">
        {action} <span aria-hidden>→</span>
      </span>
    </button>
  );
}

function ConnectedStep({
  account,
  probe,
  onProbe,
  onContinue,
  onAnother,
}: {
  account: Account;
  probe: ModelTest | null;
  onProbe: (result: ModelTest) => void;
  onContinue: () => void;
  onAnother: () => void;
}) {
  const connections = useConnections();
  const models = useModels();
  const connection = connections.data?.find((c) => c.id === account.id);
  const accountModels = useMemo(
    () => (models.data ?? []).filter((m) => m.provider === account.provider),
    [models.data, account.provider],
  );
  const first = accountModels[0];

  const test = useMutation({
    mutationFn: (model: string) => api.post<ModelTest>("/api/models/test", { model }),
    onSuccess: onProbe,
  });

  useEffect(() => {
    if (!first || probe || test.isPending) return;
    test.mutate(first.ref);
    // One probe per account; `probe` marks it done.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [first?.ref, probe]);

  return (
    <div className="max-w-[640px]">
      <div className="flex flex-wrap items-center gap-3 rounded-[var(--radius-card)] border border-line bg-surface px-5 py-4">
        <span className="flex h-10 w-10 items-center justify-center rounded-full border border-line bg-surface-2 font-mono text-[13px] uppercase">
          {account.provider.slice(0, 2)}
        </span>
        <div className="min-w-0">
          <div className="truncate text-[14px] font-medium">{account.label}</div>
          <div className="truncate text-[12.5px] text-faint">
            {connection?.email ?? connection?.name ?? account.provider}
          </div>
        </div>
        <span className="ml-auto rounded-full border border-[var(--color-pos)]/35 bg-[var(--color-pos)]/10 px-2.5 py-0.5 text-[12px] text-pos">
          Active
        </span>
        <span className="rounded-full border border-line px-2.5 py-0.5 text-[12px] text-muted">
          Priority {(connection?.priority ?? 0) + 1}
        </span>
      </div>

      <div
        role="status"
        aria-live="polite"
        className="mt-3 flex flex-wrap items-center gap-3 rounded-[var(--radius-card)] border border-line bg-surface px-5 py-3.5 text-[13px]"
      >
        {probe?.ok ? (
          <>
            <span aria-hidden className="h-2 w-2 rounded-full bg-pos" />
            <span>Test request succeeded</span>
            <span className="font-mono text-[12.5px] text-muted tnum">{ms(probe.latency_ms)}</span>
          </>
        ) : probe ? (
          <>
            <span aria-hidden className="h-2 w-2 rounded-full bg-neg" />
            <span className="min-w-0 flex-1 text-neg">{probe.error ?? "The test request failed."}</span>
          </>
        ) : test.isError ? (
          <ErrorNote error={test.error} onRetry={() => first && test.mutate(first.ref)} />
        ) : (
          <>
            <span aria-hidden className="pulse h-2 w-2 rounded-full bg-accent" />
            <span>Running a test request…</span>
          </>
        )}
      </div>

      <div className="mt-5">
        <div className="text-[13px] text-muted">
          {accountModels.length} models available
        </div>
        <div className="mt-2 flex flex-wrap gap-1.5">
          {accountModels.slice(0, 8).map((m) => (
            <span
              key={m.ref}
              className="rounded-lg border border-line bg-surface-2 px-2.5 py-1 font-mono text-[12px] text-muted"
            >
              {splitRef(m.ref).id}
            </span>
          ))}
          {accountModels.length > 8 ? (
            <span className="px-2.5 py-1 text-[12px] text-faint">
              +{accountModels.length - 8} more
            </span>
          ) : null}
        </div>
      </div>

      <div className="mt-8 flex flex-wrap items-center gap-2 border-t border-line pt-6">
        <Button onClick={onAnother}>Connect another account</Button>
        <Button variant="primary" onClick={onContinue} className="ml-auto">
          Continue
        </Button>
      </div>
    </div>
  );
}

function KeyStep({
  onBack,
  onCreated,
}: {
  onBack: () => void;
  onCreated: (created: NewKey) => void;
}) {
  const client = useQueryClient();
  const [name, setName] = useState("");
  const create = useMutation({
    mutationFn: () => api.post<NewKey>("/api/keys", { name: name || null }),
    onSuccess: (created) => {
      void client.invalidateQueries({ queryKey: qk.keys });
      void client.invalidateQueries({ queryKey: qk.system });
      onCreated(created);
    },
  });

  return (
    <form
      className="max-w-[560px]"
      onSubmit={(e) => {
        e.preventDefault();
        if (!create.isPending) create.mutate();
      }}
    >
      <Field label="Key name" hint="A label so you can tell keys apart and revoke the right one later.">
        <Input value={name} onChange={setName} placeholder="e.g. laptop" ariaLabel="key name" />
      </Field>
      {create.isError ? (
        <div className="mt-4">
          <ErrorNote error={create.error} />
        </div>
      ) : null}
      <div className="mt-7 flex flex-wrap items-center gap-2 border-t border-line pt-6">
        <Button onClick={onBack}>Back</Button>
        <Button type="submit" variant="primary" disabled={create.isPending} className="ml-auto">
          {create.isPending ? "Creating…" : "Create key"}
        </Button>
      </div>
    </form>
  );
}

function KeyCreatedStep({
  created,
  baseUrl,
  onContinue,
}: {
  created: NewKey;
  baseUrl: string;
  onContinue: () => void;
}) {
  return (
    <div className="max-w-[640px]">
      <div className="rounded-[var(--radius-card)] border border-[var(--color-warn)]/35 bg-[var(--color-warn)]/10 px-5 py-4 text-[13.5px] text-[var(--color-warn-ink)]">
        Copy the key now. It cannot be shown again.
      </div>

      <div className="mt-4">
        <div className="text-[13px] text-muted">Your key</div>
        <div className="mt-1.5 flex items-center gap-2 rounded-lg border border-line bg-surface-2 py-1 pl-3.5 pr-1.5">
          <code className="min-w-0 flex-1 truncate font-mono text-[13px]">{created.key}</code>
          <CopyButton value={created.key} label="Copy key" />
        </div>
      </div>

      <div className="mt-4">
        <div className="text-[13px] text-muted">Base URL</div>
        <div className="mt-1.5 flex items-center gap-2 rounded-lg border border-line bg-surface-2 py-1 pl-3.5 pr-1.5">
          <code className="min-w-0 flex-1 truncate font-mono text-[13px]">{baseUrl}</code>
          <CopyButton value={baseUrl} label="Copy" />
        </div>
      </div>

      <p className="mt-4 text-[13px] text-faint">
        Point any OpenAI-compatible client at the base URL and use the key above.
      </p>

      <div className="mt-8 flex flex-wrap items-center gap-2 border-t border-line pt-6">
        <Button variant="primary" onClick={onContinue} className="ml-auto">
          I've saved it somewhere else
        </Button>
      </div>
    </div>
  );
}

function RequestStep({
  apiKey,
  baseUrl,
  account,
  startedAt,
  onFirst,
  onBack,
}: {
  apiKey: string;
  baseUrl: string;
  account: Account | null;
  startedAt: number;
  onFirst: (event: UsageEvent) => void;
  onBack: () => void;
}) {
  const models = useModels();
  const [lang, setLang] = useState<SnippetLang>("curl");
  const accountModels = (models.data ?? []).filter(
    (m) => !account || m.provider === account.provider,
  );
  // The provider's own /models list is the only one that is certainly served; the
  // builtin catalog can name a model the account cannot call. Prefer upstream rows.
  const upstream = accountModels.filter((m) => m.source === "upstream");
  const candidates = upstream.length > 0 ? upstream : accountModels;
  const [chosen, setChosen] = useState<string | null>(null);
  const model = chosen ?? candidates[0]?.ref ?? models.data?.[0]?.ref ?? "";
  const snippet = snippetFor(lang, baseUrl, apiKey, model);
  const [probeError, setProbeError] = useState<string | null>(null);

  const events = useQuery({
    queryKey: ["setup", "first-request", startedAt],
    queryFn: () => list<UsageEvent>("/api/usage?limit=5"),
    refetchInterval: 2_000,
    staleTime: 0,
  });

  useEffect(() => {
    const hit = (events.data ?? []).find((e) => e.ts >= startedAt);
    if (hit) onFirst(hit);
  }, [events.data, startedAt, onFirst]);

  const probe = useMutation({
    mutationFn: () => api.post<ModelTest>("/api/models/test", { model }),
    onSuccess: (result) => {
      if (!result.ok) {
        setProbeError(result.error ?? `upstream answered ${result.status}`);
        return;
      }
      setProbeError(null);
      // The probe is a real request through the proxy, so it records a usage row.
      // Read that row instead of inventing token counts; the poll above picks it up.
      void events.refetch();
    },
  });

  return (
    <div className="max-w-[720px]">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <label htmlFor="setup-model" className="text-[13px] text-muted">
          Model
        </label>
        <div className="min-w-[240px] max-w-[420px] flex-1">
          <Select
            value={model}
            onChange={(value) => {
              setChosen(value);
              setProbeError(null);
            }}
            ariaLabel="setup model"
          >
            {candidates.map((m) => (
              <option key={m.ref} value={m.ref}>
                {m.ref}
              </option>
            ))}
          </Select>
        </div>
      </div>

      <div className="mt-3 flex flex-wrap items-center justify-between gap-2">
        <div className="inline-flex rounded-[10px] border border-line bg-surface-2 p-0.5">
          {SNIPPET_LANGS.map((l) => (
            <button
              key={l.id}
              type="button"
              aria-pressed={lang === l.id}
              onClick={() => setLang(l.id)}
              className={cx(
                "h-[30px] rounded-lg px-3 text-[12.5px]",
                lang === l.id ? "bg-fg font-medium text-bg" : "text-muted hover:text-fg",
              )}
            >
              {l.label}
            </button>
          ))}
        </div>
        <CopyButton value={snippet} label="Copy snippet" />
      </div>

      <pre className="mt-2 overflow-x-auto rounded-[10px] border border-line bg-[var(--color-bg)] p-4 font-mono text-[12.5px] leading-relaxed text-fg">
        {snippet}
      </pre>

      <div
        role="status"
        aria-live="polite"
        className="mt-5 flex flex-wrap items-center gap-3 rounded-[var(--radius-card)] border border-line bg-surface px-5 py-4"
      >
        <span aria-hidden className="pulse h-2.5 w-2.5 shrink-0 rounded-full bg-accent" />
        <span className="text-[14px]">Listening for the first request</span>
        <span className="text-[12.5px] text-faint">Nothing yet. Your request will appear here.</span>
      </div>

      <div className="mt-5 flex flex-wrap items-center gap-2">
        <Button onClick={onBack}>Back</Button>
        <Button
          onClick={() => probe.mutate()}
          disabled={probe.isPending}
          className="ml-auto"
        >
          {probe.isPending ? "Sending…" : "Send a test request from here"}
        </Button>
      </div>
      {probe.isError || probeError ? (
        <div className="mt-4">
          <ErrorNote error={probe.isError ? probe.error : probeError} />
        </div>
      ) : null}
    </div>
  );
}

function DoneStep({
  event,
  account,
  onActivity,
  onProviders,
  onCombos,
  onAliases,
}: {
  event: UsageEvent | null;
  account: Account | null;
  onActivity: () => void;
  onProviders: () => void;
  onCombos: () => void;
  onAliases: () => void;
}) {
  return (
    <div className="max-w-[720px]">
      <div className="flex flex-wrap items-center gap-3 rounded-[var(--radius-card)] border border-line bg-surface px-5 py-4">
        <span aria-hidden className="h-2.5 w-2.5 rounded-full bg-pos" />
        <span className="text-[14px]">
          {account ? `${account.label} received the request` : "The first request arrived"}
        </span>
      </div>

      {event ? (
        <dl className="mt-4 divide-y divide-[var(--color-rule)] rounded-[var(--radius-card)] border border-line bg-surface px-5 text-[13px]">
          <Row k="Model">
            <span className="font-mono text-[12.5px]">{event.model}</span>
          </Row>
          <Row k="Status">
            <StatusPill status={event.status} />
          </Row>
          <Row k="Tokens">
            <span className="font-mono tnum">
              {compact(event.input_tokens)} → {compact(event.output_tokens)}
            </span>
          </Row>
          <Row k="Latency">
            <span className="font-mono tnum">{ms(event.latency_ms)}</span>
          </Row>
        </dl>
      ) : null}

      <h2 className="mt-8 text-[15px] font-semibold">What to do next</h2>
      <div className="mt-3 grid gap-3 sm:grid-cols-3">
        <Next title="Add a fallback" body="Chain models in a Combo so a rate limit never reaches your app." onClick={onCombos} />
        <Next title="Create an alias" body="Give clients a stable name like default and repoint it later." onClick={onAliases} />
        <Next title="Connect another account" body="Spread load across accounts and fail over between them." onClick={onProviders} />
      </div>

      <div className="mt-8 flex flex-wrap items-center gap-2 border-t border-line pt-6">
        <span className="text-[12.5px] text-faint">Setup complete. You can reopen it from Health.</span>
        <Button variant="primary" onClick={onActivity} className="ml-auto">
          Open Activity
        </Button>
      </div>
    </div>
  );
}

function Row({ k, children }: { k: string; children: React.ReactNode }) {
  return (
    <div className="flex items-center justify-between gap-3 py-2.5">
      <dt className="text-faint">{k}</dt>
      <dd className="min-w-0 truncate text-right text-muted">{children}</dd>
    </div>
  );
}

function Next({ title, body, onClick }: { title: string; body: string; onClick: () => void }) {
  return (
    <button
      type="button"
      onClick={onClick}
      className="flex flex-col gap-1.5 rounded-[var(--radius-card)] border border-line bg-surface p-4 text-left transition-colors hover:border-line-strong hover:bg-[var(--color-row-hover)]"
    >
      <span className="text-[13.5px] font-medium">{title}</span>
      <span className="text-[12.5px] text-muted">{body}</span>
    </button>
  );
}

function Rail({
  step,
  account,
  keyCreated,
  firstRequest,
}: {
  step: Step;
  account: Account | null;
  keyCreated: boolean;
  firstRequest: UsageEvent | null;
}) {
  const macro = MACRO[step];
  const summaries = [
    account
      ? `${account.provider[0]?.toUpperCase() ?? ""}${account.provider.slice(1)} · ${account.label}`
      : STEP_HELP[0],
    keyCreated ? "Key created" : STEP_HELP[1],
    firstRequest ? `First request in ${ms(firstRequest.latency_ms)}` : STEP_HELP[2],
  ];
  return (
    <ol className="flex flex-col">
      {[1, 2, 3].map((n) => {
        const done = n < macro || (n === 3 && step === "request-done");
        const current = n === macro && !done;
        return (
          <li key={n} className="flex gap-3.5">
            <div className="flex flex-col items-center">
              <span
                className={cx(
                  "flex h-7 w-7 shrink-0 items-center justify-center rounded-full border font-mono text-[12px]",
                  done && "border-white bg-white text-[var(--color-sidebar)]",
                  current && "border-[var(--color-nav-accent)] text-white",
                  !done && !current && "border-white/25 text-white/50",
                )}
              >
                {done ? <Check size={13} weight="bold" aria-hidden /> : n}
              </span>
              {n < 3 ? (
                <span aria-hidden className={cx("my-1.5 min-h-5 w-px flex-1", done ? "bg-fg" : "bg-line")} />
              ) : null}
            </div>
            <div className={cx("min-w-0 pb-6", n === 3 && "pb-0")}>
              <div className={cx("text-[13.5px] font-medium", done || current ? "text-white" : "text-white/60")}>
                {STEP_TITLES[n - 1]}
              </div>
              <div className="mt-0.5 text-[12.5px] text-white/45">{summaries[n - 1]}</div>
            </div>
          </li>
        );
      })}
    </ol>
  );
}

function MobileProgress({ step }: { step: Step }) {
  const macro = MACRO[step];
  return (
    <div className="mb-5 min-[820px]:hidden">
      <div className="mb-2 text-[12.5px] text-faint">Step {macro} of 3</div>
      <div className="flex gap-1">
        {[1, 2, 3].map((n) => (
          <span
            key={n}
            aria-hidden
            className={cx("h-[3px] flex-1 rounded-full", n <= macro ? "bg-fg" : "bg-line")}
          />
        ))}
      </div>
    </div>
  );
}

