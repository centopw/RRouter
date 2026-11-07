import { useEffect, useRef, useState, type RefObject } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { Button, CopyButton, ErrorNote, Field, Input, Select, cx } from "../../components/ui";
import {
  api,
  type Connection,
  type ModelProvider,
  type OAuthProvider,
  type OAuthStart,
} from "../../lib/api";
import { qk, useOAuthStatus } from "../../lib/queries";

/** Step 1, sub-screens: browser sign-in (PKCE), device code, and a pasted API key. */

type SignInDone = {
  id: string;
  provider: string;
  name: string | null;
  warning: string | null;
};

/**
 * One `/start` call, cancelled if the screen unmounts before the exchange completes.
 * A fresh mount (retry) starts a new exchange; the parent remounts with a new key.
 */
function useSignInStart(providerId: string) {
  const [start, setStart] = useState<OAuthStart | null>(null);
  const [error, setError] = useState<unknown>(null);
  const startId = useRef<string | null>(null);
  const settled = useRef(false);

  useEffect(() => {
    let live = true;
    api
      .post<OAuthStart>(`/api/oauth/${providerId}/start`)
      .then((s) => {
        if (!live) {
          void api.del<{ cancelled: boolean }>(`/api/oauth/${providerId}/${s.id}`).catch(() => {});
          return;
        }
        startId.current = s.id;
        setStart(s);
      })
      .catch((e: unknown) => {
        if (live) setError(e);
      });
    return () => {
      live = false;
      const id = startId.current;
      if (id && !settled.current) {
        void api.del<{ cancelled: boolean }>(`/api/oauth/${providerId}/${id}`).catch(() => {});
      }
    };
  }, [providerId]);

  return { start, error, settled };
}

/** Shared last leg: `complete` the exchange, refresh the caches, hand over the id. */
function useCompleteSignIn(
  provider: OAuthProvider,
  settled: RefObject<boolean>,
  onConnected: (id: string, label: string) => void,
  onFail: (error: unknown) => void,
) {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (id: string) => api.post<SignInDone>(`/api/oauth/${provider.id}/complete`, { id }),
    onSuccess: (result) => {
      settled.current = true;
      void client.invalidateQueries({ queryKey: qk.connections });
      void client.invalidateQueries({ queryKey: qk.modelProviders });
      void client.invalidateQueries({ queryKey: qk.models });
      void client.invalidateQueries({ queryKey: qk.system });
      onConnected(result.id, result.name ?? provider.display);
    },
    onError: (error: unknown) => {
      settled.current = true;
      onFail(error);
    },
  });
}

/** Navigate the popup opened by the click; fall back to a direct tab when blocked. */
function openAuthorize(url: string, popup: Window | null): "popup" | "tab" | "blocked" {
  if (popup && !popup.closed) {
    popup.location.replace(url);
    return "popup";
  }
  const opened = window.open(url, "_blank", "noopener,noreferrer");
  return opened ? "tab" : "blocked";
}

export function PkceStep({
  provider,
  popup,
  onConnected,
  onFail,
}: {
  provider: OAuthProvider;
  popup: Window | null;
  onConnected: (id: string, label: string) => void;
  onFail: (error: unknown) => void;
}) {
  const { start, error: startError, settled } = useSignInStart(provider.id);
  const [finished, setFinished] = useState(false);
  const [tab, setTab] = useState<"opened" | "blocked" | null>(null);
  const opened = useRef(false);
  const complete = useCompleteSignIn(provider, settled, onConnected, onFail);

  const challenge = start?.challenge.kind === "pkce" ? start.challenge : null;

  useEffect(() => {
    if (!challenge || opened.current) return;
    opened.current = true;
    const result = openAuthorize(challenge.authorize_url, popup);
    setTab(result === "blocked" ? "blocked" : "opened");
  }, [challenge, popup]);

  useEffect(() => {
    if (startError) {
      settled.current = true;
      onFail(startError);
    }
  }, [startError, onFail, settled]);

  const status = useOAuthStatus(provider.id, finished ? null : (start?.id ?? null), 2_000);

  useEffect(() => {
    if (!start || finished || complete.isPending) return;
    if (status.data?.pending !== false) return;
    setFinished(true);
    complete.mutate(start.id);
  }, [start, status.data?.pending, finished, complete]);

  const approveUrl = challenge?.authorize_url ?? null;

  return (
    <div className="mt-7">
      <div
        role="status"
        aria-live="polite"
        className="flex flex-wrap items-center gap-3 rounded-[var(--radius-card)] border border-line bg-surface px-5 py-4"
      >
        <span aria-hidden className="pulse inline-block h-2.5 w-2.5 shrink-0 rounded-full bg-pos" />
        <span className="text-lead">
          {startError ? "Could not start the sign-in" : !start ? "Starting the sign-in…" : "Waiting for approval…"}
        </span>
        <span className="ml-auto rounded-full border border-line px-2.5 py-[1px] text-small text-muted">
          Browser sign-in
        </span>
      </div>

      <ol className="mt-6 flex flex-wrap items-center gap-x-3 gap-y-2 text-body">
        <Stage
          n={1}
          state="done"
          label="Tab opened"
          detail={tab === "blocked" ? "blocked by the browser" : undefined}
        />
        <Connector done={status.data?.pending === false} />
        <Stage n={2} state={status.data?.pending === false ? "done" : "current"} label="Approve access" />
        <Connector done={false} />
        <Stage n={3} state={status.data?.pending === false ? "current" : "upcoming"} label="Connected" />
      </ol>

      <div className="mt-7 flex flex-col gap-2">
        <span className="text-caption font-medium uppercase tracking-[0.08em] text-faint">
          Callback address
        </span>
        <div className="flex flex-wrap items-center gap-2 rounded-[10px] border border-line bg-surface-2 px-3 py-2">
          <code className="min-w-0 flex-1 truncate font-mono text-small text-muted">
            {challenge?.redirect_uri ?? "waiting for the server…"}
          </code>
          {challenge ? <CopyButton value={challenge.redirect_uri} label="Copy" /> : null}
        </div>
        <p className="text-small text-faint">
          Your browser has to be able to reach this address. It is on this machine, so sign in from a
          browser that runs here.{" "}
          {approveUrl ? (
            <a
              href={approveUrl}
              target="_blank"
              rel="noreferrer"
              className="text-muted underline underline-offset-2 hover:text-fg"
            >
              Tab didn't open? Open it again
            </a>
          ) : null}
        </p>
      </div>

      {status.isError || complete.isError ? (
        <div className="mt-4">
          <ErrorNote
            error={status.isError ? status.error : complete.error}
            onRetry={() => void status.refetch()}
          />
        </div>
      ) : null}
    </div>
  );
}

export function PkceFailedStep({
  error,
  onRetry,
  onChoose,
}: {
  error: unknown;
  onRetry: () => void;
  onChoose: () => void;
}) {
  return (
    <div className="mt-7">
      <div
        role="alert"
        className="flex items-start gap-3 rounded-[var(--radius-card)] border border-[var(--color-neg)]/30 bg-[var(--color-neg)]/8 px-5 py-4"
      >
        <svg
          aria-hidden
          width="20"
          height="20"
          viewBox="0 0 24 24"
          fill="none"
          stroke="var(--color-neg)"
          strokeWidth="2"
          strokeLinecap="round"
          strokeLinejoin="round"
          className="mt-0.5 shrink-0"
        >
          <path d="M12 3l10 18H2L12 3zM12 10v5M12 18h.01" />
        </svg>
        <div className="min-w-0">
          <div className="text-lead font-semibold">Approval didn't go through</div>
          <p className="mt-1 text-body text-muted">
            Nothing was saved. The tab may have been closed before you approved, or your browser
            could not reach the callback address.
          </p>
          {error ? <p className="mt-2 break-words text-small text-muted">{describe(error)}</p> : null}
        </div>
      </div>
      <div className="mt-5 flex flex-wrap gap-2">
        <Button variant="primary" onClick={onRetry}>
          Try again
        </Button>
        <Button onClick={onChoose}>Choose another way</Button>
      </div>
    </div>
  );
}

export function DeviceStep({
  provider,
  onConnected,
  onFail,
  onNewCode,
}: {
  provider: OAuthProvider;
  onConnected: (id: string, label: string) => void;
  onFail: (error: unknown) => void;
  onNewCode: () => void;
}) {
  const { start, error: startError, settled } = useSignInStart(provider.id);
  const [finished, setFinished] = useState(false);
  const complete = useCompleteSignIn(provider, settled, onConnected, onFail);
  const challenge = start?.challenge.kind === "device_code" ? start.challenge : null;

  const [remaining, setRemaining] = useState<number | null>(null);
  const total = useRef(0);

  useEffect(() => {
    if (!challenge) return;
    total.current = Math.max(challenge.expires_in, 1);
    setRemaining(challenge.expires_in);
    const timer = window.setInterval(() => {
      setRemaining((prev) => (prev === null ? null : Math.max(0, prev - 1)));
    }, 1_000);
    return () => window.clearInterval(timer);
  }, [challenge]);

  const expired = remaining !== null && remaining <= 0;
  const intervalMs = Math.max(challenge?.interval ?? 5, 2) * 1_000;
  const status = useOAuthStatus(
    provider.id,
    finished || expired ? null : (start?.id ?? null),
    intervalMs,
  );

  useEffect(() => {
    if (!start || finished || expired || complete.isPending) return;
    if (status.data?.pending !== false) return;
    setFinished(true);
    complete.mutate(start.id);
  }, [start, status.data?.pending, finished, expired, complete]);

  useEffect(() => {
    if (startError) {
      settled.current = true;
      onFail(startError);
    }
  }, [startError, onFail, settled]);

  if (expired) {
    return (
      <div className="mt-7">
        <div
          role="alert"
          className="rounded-[var(--radius-card)] border border-[var(--color-warn)]/35 bg-[var(--color-warn)]/10 px-5 py-4"
        >
          <div className="text-lead font-semibold">This code expired</div>
          <p className="mt-1 text-body text-muted">
            Codes are short lived. Get a new one and enter it on the provider's page.
          </p>
        </div>
        <div className="mt-5 flex flex-wrap gap-2">
          <Button variant="primary" onClick={onNewCode}>
            Get a new code
          </Button>
        </div>
      </div>
    );
  }

  return (
    <div className="mt-7">
      <div className="rounded-[var(--radius-card)] border border-line bg-surface px-6 py-7 text-center">
        <div className="text-body text-muted">Your code</div>
        <div className="mt-2 select-all font-mono text-hero font-medium leading-none tracking-[0.14em] text-fg">
          {challenge?.user_code ?? "————"}
        </div>
        <div className="mt-5 flex flex-wrap items-center justify-center gap-2">
          {challenge ? <CopyButton value={challenge.user_code} label="Copy code" /> : null}
          {challenge ? (
            <a
              href={challenge.verification_uri}
              target="_blank"
              rel="noreferrer"
              className="inline-flex h-8 shrink-0 items-center gap-1.5 rounded-lg border border-transparent bg-accent px-3 text-body font-semibold text-[var(--color-accent-ink)] hover:brightness-110"
            >
              Open verification page <span aria-hidden>→</span>
            </a>
          ) : null}
        </div>
        <div className="mt-4 truncate font-mono text-small text-faint">
          {challenge?.verification_uri ?? "waiting for the server…"}
        </div>
      </div>

      <div
        role="status"
        aria-live="polite"
        className="mt-4 flex flex-wrap items-center gap-3 rounded-[var(--radius-card)] border border-line bg-surface px-5 py-4"
      >
        <span aria-hidden className="pulse inline-block h-2.5 w-2.5 shrink-0 rounded-full bg-pos" />
        <span className="text-lead">
          {startError
            ? "Could not start the sign-in"
            : !start
              ? "Starting the sign-in…"
              : `Checking every ${Math.round(intervalMs / 1_000)}s…`}
        </span>
        <span className="ml-auto font-mono text-small tnum text-muted">
          {remaining === null ? "—" : `Expires in ${clock(remaining)}`}
        </span>
      </div>
      <div className="mt-2.5 h-[3px] overflow-hidden rounded-full bg-rule">
        <div
          className="h-full rounded-full bg-fg"
          style={{ width: `${remaining === null ? 100 : Math.round((remaining / Math.max(total.current, 1)) * 100)}%` }}
        />
      </div>

      <ol className="mt-6 flex flex-wrap items-center gap-x-3 gap-y-2 text-body">
        <Stage n={1} state="current" label="Open the page" />
        <Connector done={false} />
        <Stage n={2} state="upcoming" label="Enter the code" />
        <Connector done={false} />
        <Stage n={3} state="upcoming" label="Approve" />
      </ol>

      {status.isError || complete.isError ? (
        <div className="mt-4">
          <ErrorNote
            error={status.isError ? status.error : complete.error}
            onRetry={() => void status.refetch()}
          />
        </div>
      ) : null}
    </div>
  );
}

export function ApiKeyStep({
  providers,
  pending,
  error,
  onRetryProviders,
  onConnected,
}: {
  providers: ModelProvider[];
  pending: boolean;
  error: unknown;
  onRetryProviders: () => void;
  onConnected: (connection: Connection) => void;
}) {
  const client = useQueryClient();
  const [provider, setProvider] = useState("");
  const [key, setKey] = useState("");
  const [label, setLabel] = useState("");
  const [baseUrl, setBaseUrl] = useState("");
  const [reveal, setReveal] = useState(false);

  const selected = provider || providers[0]?.id || "openai";

  const save = useMutation({
    mutationFn: () =>
      api.post<Connection>("/api/connections", {
        provider: selected,
        auth_type: "api_key",
        name: label || null,
        credentials: {
          api_key: key,
          ...(baseUrl ? { base_url_override: baseUrl } : {}),
        },
      }),
    onSuccess: (connection) => {
      void client.invalidateQueries({ queryKey: qk.connections });
      void client.invalidateQueries({ queryKey: qk.modelProviders });
      void client.invalidateQueries({ queryKey: qk.models });
      void client.invalidateQueries({ queryKey: qk.system });
      setKey("");
      onConnected(connection);
    },
  });

  return (
    <form
      className="mt-7 flex max-w-[560px] flex-col gap-5"
      onSubmit={(e) => {
        e.preventDefault();
        if (!save.isPending) save.mutate();
      }}
    >
      <Field label="Provider" hint="the service that will bill you per token">
        {pending ? (
          <div className="h-8 w-full animate-pulse rounded-lg bg-surface-2" />
        ) : (
          <Select value={selected} onChange={setProvider} ariaLabel="api key provider">
            {providers.map((p) => (
              <option key={p.id} value={p.id}>
                {p.display}
              </option>
            ))}
          </Select>
        )}
      </Field>

      {error ? <ErrorNote error={error} onRetry={onRetryProviders} /> : null}

      <Field label="API key" hint="Find it in your provider's dashboard under API keys.">
        <div className="flex items-center gap-2">
          <Input
            value={key}
            onChange={setKey}
            type={reveal ? "text" : "password"}
            placeholder="Paste your key"
            ariaLabel="api key"
          />
          <Button onClick={() => setReveal((v) => !v)} className="w-16 justify-center">
            {reveal ? "Hide" : "Show"}
          </Button>
        </div>
      </Field>

      <Field label="Label (optional)" hint="A name so you can tell accounts apart later.">
        <Input value={label} onChange={setLabel} placeholder="e.g. Personal OpenAI" ariaLabel="account label" />
      </Field>

      <details className="rounded-[var(--radius-card)] border border-line bg-surface">
        <summary className="cursor-pointer px-4 py-3 text-body font-medium">Advanced</summary>
        <div className="border-t border-rule px-4 py-4">
          <Field label="Base URL" hint="Leave empty to use the provider default.">
            <Input
              value={baseUrl}
              onChange={setBaseUrl}
              placeholder="https://…/v1"
              ariaLabel="base url override"
            />
          </Field>
        </div>
      </details>

      <div className="flex items-start gap-3 rounded-[var(--radius-card)] border border-line bg-surface px-4 py-3 text-body text-muted">
        <svg
          aria-hidden
          width="18"
          height="18"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          strokeWidth="1.8"
          strokeLinecap="round"
          className="mt-0.5 shrink-0"
        >
          <path d="M12 21a9 9 0 100-18 9 9 0 000 18zM12 11v5M12 8h.01" />
        </svg>
        <span>The key is stored on this machine and is never shown again after you save.</span>
      </div>

      {save.isError ? <ErrorNote error={save.error} /> : null}

      <div className="flex items-center justify-end gap-2">
        <Button type="submit" variant="primary" disabled={!selected || !key || save.isPending}>
          {save.isPending ? "Saving…" : "Verify and save"}
        </Button>
      </div>
    </form>
  );
}

/** Small numbered marker used by both timelines. */
export function Stage({
  n,
  state,
  label,
  detail,
}: {
  n: number;
  state: "done" | "current" | "upcoming";
  label: string;
  detail?: string;
}) {
  const done = state === "done";
  return (
    <li className={cx("inline-flex items-center gap-2.5", done || state === "current" ? "text-fg" : "text-muted")}>
      <span
        aria-hidden
        className={cx(
          "flex h-6 w-6 shrink-0 items-center justify-center rounded-full border font-mono text-caption",
          done && "border-fg bg-fg text-bg",
          state === "current" && "border-fg text-fg",
          state === "upcoming" && "border-line text-faint",
        )}
      >
        {done ? (
          <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="3" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
            <path d="M5 12.5l4.5 4.5L19 7.5" />
          </svg>
        ) : (
          n
        )}
      </span>
      {label}
      {detail ? <span className="text-small text-faint">· {detail}</span> : null}
    </li>
  );
}

function Connector({ done }: { done: boolean }) {
  return (
    <li aria-hidden className={cx("h-px min-w-3 flex-[0_1_36px]", done ? "bg-fg" : "bg-line")} />
  );
}

function clock(secs: number): string {
  const m = Math.floor(secs / 60);
  const s = secs % 60;
  return `${m}:${String(s).padStart(2, "0")}`;
}

/** One sentence from an unknown error, without leaking a stack trace. */
export function describe(error: unknown): string {
  if (error instanceof Error) return error.message;
  return String(error);
}
