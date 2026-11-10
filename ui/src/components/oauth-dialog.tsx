import { useEffect, useRef, useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { CheckCircle } from "@phosphor-icons/react";
import { Badge, Button, Dot, ErrorNote, Field, Modal, cx } from "./ui";
import { ProviderIcon } from "./provider-icon";
import { ApiError, api, type OAuthProvider, type OAuthStart } from "../lib/api";
import { qk, useOAuthStatus } from "../lib/queries";

type CompleteResult = {
  id: string;
  provider: string;
  name: string | null;
  warning: string | null;
};

/**
 * Provider-specific copy, kept here so the shared flow stays generic. Antigravity is a
 * Cloud Code Assist sign-in, not an API key: the Cloud project is discovered server-side
 * after approval, so this dialog asks for nothing the operator cannot know.
 */
const PROVIDER_NOTES: Record<string, string> = {
  "google-antigravity":
    "Sign in with the Google account that has Antigravity access. The gateway discovers your Cloud Code Assist project after approval and stores it on the connection; there is no project id to type, and the tokens stay on the server.",
  antigravity:
    "Sign in with the Google account that has Antigravity access. The gateway discovers your Cloud Code Assist project after approval and stores it on the connection; there is no project id to type, and the tokens stay on the server.",
};

/**
 * What is wrong with a pasted callback URL, before the gateway ever sees it.
 *
 * The gateway is still the authority (it owns the `state` it issued); this only catches the
 * three mistakes an operator on a VPS actually makes: pasting the authorize URL, pasting a
 * fragment of the address, or pasting a callback from a different sign-in.
 */
function callbackProblem(value: string, redirect: string): string | null {
  const raw = value.trim();
  if (raw === "") return null;
  let parsed: URL;
  try {
    parsed = new URL(raw);
  } catch {
    return "That is not a full URL. Copy the address bar of the page the provider redirected to, including the query string.";
  }
  if (!parsed.searchParams.get("code") && !parsed.searchParams.get("error")) {
    return "That URL has no authorization code (?code=…). Paste the callback address the provider redirected to, not the sign-in page.";
  }
  if (redirect && !raw.startsWith(redirect)) {
    return `It should begin with the redirect URI this sign-in registered: ${redirect}`;
  }
  return null;
}

/** The `{error:{code}}` envelope the admin API uses, narrowed rather than asserted. */
function apiErrorCode(error: unknown): string | null {
  if (!(error instanceof ApiError)) return null;
  const body = error.body;
  if (!body || typeof body !== "object" || !("error" in body)) return null;
  const inner = body.error;
  if (!inner || typeof inner !== "object" || !("code" in inner)) return null;
  return typeof inner.code === "string" ? inner.code : null;
}

/** Turn the gateway's manual-completion failure into one sentence an operator can act on. */
function callbackErrorText(error: unknown): string {
  const code = apiErrorCode(error) ?? "";
  const message = error instanceof Error ? error.message : String(error);
  if (code === "state_mismatch") {
    return "That callback belongs to a different sign-in attempt. Copy the address from the browser tab that just approved this one.";
  }
  if (code === "signin_expired" || code === "expired" || code === "not_found") {
    return "This sign-in expired or was cancelled. Start again to get a fresh URL.";
  }
  if (/denied|access_denied/i.test(message) || code === "authorization_denied") {
    return "The provider reported that access was denied. Approve again, or pick a different account.";
  }
  if (code === "callback_invalid") {
    return "The gateway could not read a code from that URL. Paste the complete callback address.";
  }
  return message;
}

/**
 * One in-flight sign-in. Owns its start request, its polling, and its cancellation.
 *
 * Local mode finishes itself through the loopback listener. Manual mode exists for a
 * gateway on a remote VPS whose loopback the signing-in browser cannot reach: the dialog
 * starts the flow with `manual: true`, shows the authorize URL and the registered redirect
 * URI, and accepts the callback URL pasted back from the operator's own browser.
 */
export function OAuthDialog({
  provider,
  onClose,
  onConnected,
  onViewProviders,
}: {
  provider: OAuthProvider;
  onClose: () => void;
  onConnected: (label: string, warning: string | null) => void;
  /** The success panel's "view providers"; defaults to closing the dialog. */
  onViewProviders?: () => void;
}) {
  const client = useQueryClient();
  // PKCE always uses the paste-back flow. A device-code provider has no callback to
  // paste, and a build that predates manual completion falls back to the loopback UI.
  const manual = provider.flow === "pkce" && provider.manual !== false;
  const [start, setStart] = useState<OAuthStart | null>(null);
  const [startError, setStartError] = useState<unknown>(null);
  const [callback, setCallback] = useState("");
  const [connected, setConnected] = useState<CompleteResult | null>(null);
  const completing = useRef(false);

  // (Re)start whenever the mode changes: a manual attempt has a different challenge and a
  // callback the gateway will only accept through the manual completion call.
  useEffect(() => {
    let live = true;
    let mine: string | null = null;
    completing.current = false;
    // A mode switch is a new attempt: neither mutation's previous failure applies to it.
    complete.reset();
    completeManual.reset();
    setStart(null);
    setStartError(null);
    setConnected(null);
    setCallback("");
    api
      .post<OAuthStart>(
        `/api/oauth/${provider.id}/start`,
        manual ? { manual: true } : undefined,
      )
      .then((s) => {
        if (!live) {
          void api.del<{ cancelled: boolean }>(`/api/oauth/${provider.id}/${s.id}`).catch(() => {});
          return;
        }
        mine = s.id;
        setStart(s);
      })
      .catch((error) => {
        if (live) setStartError(error);
      });
    return () => {
      live = false;
      if (mine && !completing.current) {
        void api.del<{ cancelled: boolean }>(`/api/oauth/${provider.id}/${mine}`).catch(() => {});
      }
    };
  }, [provider.id, manual]);

  const challenge = start?.challenge ?? null;
  // The server echoes the mode it actually started. A build that ignored `manual` falls
  // back to the loopback UI with a note instead of showing a paste box that cannot work.
  const manualRefused =
    manual && challenge !== null && (challenge.kind !== "pkce" || challenge.manual !== true);
  const paste = manual && !manualRefused;
  const pollSeconds = challenge?.kind === "device_code" ? Math.max(challenge.interval, 2) : 2;
  // Manual mode never polls: the answer arrives by paste, so the status request would only
  // ask the gateway about an attempt nobody is watching.
  const status = useOAuthStatus(
    provider.id,
    !paste ? (start?.id ?? null) : null,
    pollSeconds * 1000,
  );

  const settle = (result: CompleteResult) => {
    completing.current = true;
    void client.invalidateQueries({ queryKey: qk.connections });
    void client.invalidateQueries({ queryKey: qk.modelProviders });
    void client.invalidateQueries({ queryKey: qk.system });
    setConnected(result);
  };

  const complete = useMutation({
    mutationFn: (id: string) =>
      api.post<CompleteResult>(`/api/oauth/${provider.id}/complete`, { id }),
    onSuccess: settle,
  });

  const completeManual = useMutation({
    mutationFn: ({ id, url }: { id: string; url: string }) =>
      api.post<CompleteResult>(`/api/oauth/${provider.id}/complete`, {
        id,
        callback_url: url,
      }),
    onSuccess: settle,
  });

  // The server stops reporting pending once the provider callback has landed.
  useEffect(() => {
    if (paste || !start || status.data?.pending !== false || completing.current) return;
    completing.current = true;
    complete.mutate(start.id);
  }, [paste, start, status.data?.pending, complete]);

  const close = () => {
    if (start && !completing.current) {
      void api.del<{ cancelled: boolean }>(`/api/oauth/${provider.id}/${start.id}`).catch(() => {});
    }
    onClose();
  };

  const copy = (value: string) => {
    void navigator.clipboard.writeText(value);
  };

  const redirect = challenge?.kind === "pkce" ? challenge.redirect_uri : null;
  const problem = paste ? callbackProblem(callback, redirect ?? "") : null;
  const manualError = completeManual.isError ? callbackErrorText(completeManual.error) : null;

  const finish = () => {
    if (connected) {
      onConnected(connected.name ?? provider.display, connected.warning);
      onClose();
    }
  };

  return (
    <Modal
      title={connected ? `${provider.display} connected` : `Sign in — ${provider.display}`}
      open
      onClose={connected ? finish : close}
      footer={
        connected ? (
          <>
            <Button variant="quiet" onClick={finish}>
              done
            </Button>
            <Button
              variant="primary"
              onClick={() => {
                onConnected(connected.name ?? provider.display, connected.warning);
                if (onViewProviders) onViewProviders();
                else onClose();
              }}
            >
              view providers
            </Button>
          </>
        ) : (
          <>
            <Button variant="quiet" onClick={close}>
              cancel
            </Button>
            <Button
              variant="primary"
              disabled={
                paste
                  ? !start || callback.trim() === "" || problem !== null || completeManual.isPending
                  : !start || complete.isPending
              }
              onClick={() => {
                if (!start) return;
                if (paste) completeManual.mutate({ id: start.id, url: callback.trim() });
                else complete.mutate(start.id);
              }}
            >
              {paste
                ? completeManual.isPending
                  ? "completing…"
                  : "Complete sign-in"
                : complete.isPending
                  ? "completing…"
                  : "complete"}
            </Button>
          </>
        )
      }
    >
      <div className="flex min-w-0 flex-col gap-4">
        {connected ? (
          <>
            <div className="flex items-center gap-3 rounded-[8px] border border-[var(--color-pos)]/35 bg-[var(--color-pos-tint)] px-4 py-3">
              <ProviderIcon provider={provider.id} size={28} />
              <div className="min-w-0 flex-1">
                <p className="truncate text-body font-medium text-fg">
                  {provider.display} connected
                </p>
                <p className="truncate text-caption text-[var(--color-pos)]">
                  {connected.name ?? "account ready"}
                </p>
              </div>
              <CheckCircle size={22} aria-hidden className="shrink-0 text-[var(--color-pos)]" />
            </div>
            {connected.warning ? (
              <p role="status" className="text-caption leading-relaxed text-[var(--color-warn)]">
                {connected.warning}
              </p>
            ) : null}
            <p className="text-caption leading-relaxed text-faint">
              Requests can route through this account now. Credentials stay on the gateway —
              the dashboard never shows or returns them.
            </p>
          </>
        ) : (
          <>
            <div className="flex items-center gap-2 text-body text-muted">
              <Dot tone={startError ? "neg" : status.data?.pending === false ? "pos" : "warn"} />
              <span aria-live="polite">
                {startError
                  ? "could not start the sign-in"
                  : !start
                    ? "starting…"
                    : complete.isPending || completeManual.isPending
                      ? "exchanging the authorization code…"
                      : paste
                        ? "approve, then paste the callback"
                        : status.data?.pending === false
                          ? "approved — finishing"
                          : status.isError
                            ? "approved? the status check failed"
                            : "waiting for approval"}
              </span>
              <span className="ml-auto flex items-center gap-1.5">
                <Badge tone="neutral">{provider.flow}</Badge>
                {paste ? <Badge tone="accent">manual</Badge> : null}
              </span>
            </div>

            {PROVIDER_NOTES[provider.id] ? (
              <p className="text-caption leading-relaxed text-faint">
                {PROVIDER_NOTES[provider.id]}
              </p>
            ) : null}

            {startError ? <ErrorNote error={startError} /> : null}
            {!paste && complete.isError ? <ErrorNote error={complete.error} /> : null}

            {manualRefused ? (
              <p role="status" className="text-caption leading-relaxed text-faint">
                This gateway did not start a manual attempt, so it is using the loopback
                flow. Approve in the browser tab and wait for the callback.
              </p>
            ) : null}

            {!paste && !start ? (
              <div className="rounded-lg border border-line-strong bg-surface-2 px-3 py-6 text-center text-body text-faint">
                requesting an authorization challenge…
              </div>
            ) : null}

            {challenge?.kind === "pkce" ? (
              <>
                {paste ? (
                  <p className="text-body leading-relaxed text-muted">
                    Open the authorize URL yourself in the browser that holds your provider
                    session. After you approve, that browser lands on the redirect URI (it may
                    fail to load — that is expected). Copy its whole address bar and paste it
                    below.
                  </p>
                ) : (
                  <p className="text-body text-muted">
                    This gateway build uses the loopback flow. Open the authorize URL yourself,
                    approve access, and wait for the callback.
                  </p>
                )}
                <Field
                  label={paste ? "redirect uri (registered)" : "redirect uri"}
                  hint={paste ? "the callback must begin with this address" : "the loopback the browser must reach"}
                >
                  <div className="flex min-w-0 items-start gap-2">
                    <code
                      className={cx(
                        "min-w-0 flex-1 rounded-lg border border-line-strong bg-[var(--color-surface-2)] px-2.5 py-1.5 font-mono text-small text-muted",
                        paste ? "break-all" : "truncate",
                      )}
                    >
                      {challenge.redirect_uri}
                    </code>
                    <Button onClick={() => copy(challenge.redirect_uri)} ariaLabel="copy redirect uri">
                      copy
                    </Button>
                  </div>
                </Field>
                <Field
                  label="authorize url"
                  hint={paste ? "click open, or copy it into the browser that holds your session" : "open this yourself to approve the sign-in"}
                >
                  <div className="flex min-w-0 items-start gap-2">
                    <a
                      href={challenge.authorize_url}
                      target="_blank"
                      rel="noreferrer"
                      className="min-w-0 flex-1 break-all rounded-lg border border-line-strong bg-[var(--color-surface-2)] px-2.5 py-1.5 font-mono text-small text-accent underline underline-offset-2"
                    >
                      {challenge.authorize_url}
                    </a>
                    <Button
                      onClick={() =>
                        window.open(
                          challenge.authorize_url,
                          "_blank",
                          "noopener,width=520,height=720",
                        )
                      }
                    >
                      open
                    </Button>
                    <Button onClick={() => copy(challenge.authorize_url)} ariaLabel="copy authorize url">
                      copy
                    </Button>
                  </div>
                </Field>

                {paste ? (
                  <Field
                    label="callback URL"
                    hint="the address the browser was sent to, including ?code=… and state"
                  >
                    <textarea
                      value={callback}
                      onChange={(event) => setCallback(event.target.value)}
                      rows={3}
                      spellCheck={false}
                      autoComplete="off"
                      placeholder="http://localhost:51121/oauth-callback?code=…&state=…"
                      aria-label="pasted callback URL"
                      aria-invalid={problem !== null || undefined}
                      className="w-full min-w-0 resize-y break-all rounded-[var(--radius-input)] border border-line-strong bg-surface px-2.5 py-2 font-mono text-small text-fg placeholder:text-faint focus:border-[var(--color-accent-text)] focus:outline-none focus-visible:ring-2 focus-visible:ring-[var(--color-accent-text)]/25"
                    />
                  </Field>
                ) : null}

                {paste && problem ? (
                  <p role="alert" className="text-caption leading-relaxed text-neg">
                    {problem}
                  </p>
                ) : null}
                {paste && manualError ? (
                  <p role="alert" className="rounded-[8px] border border-[var(--color-neg-line)] bg-[var(--color-neg-tint)] px-3 py-2 text-caption leading-relaxed text-[var(--color-neg-ink)]">
                    {manualError}
                  </p>
                ) : null}

                {!paste ? (
                  <p className="text-caption text-faint">
                    {manualRefused
                      ? "Approve in the browser tab; this dialog finishes when the loopback receives the callback."
                      : "If the provider cannot reach the loopback, approve anyway and press complete — the server still holds the pending exchange."}
                  </p>
                ) : null}
              </>
            ) : challenge ? (
              <>
                <Field label="user code" hint={`expires in ${Math.round(challenge.expires_in / 60)} min`}>
                  <div className="flex min-w-0 items-center gap-2">
                    <code className="min-w-0 rounded-lg border border-line-strong bg-[var(--color-surface-2)] px-3 py-2 font-mono text-title tracking-[0.18em] text-fg">
                      {challenge.user_code}
                    </code>
                    <Button onClick={() => copy(challenge.user_code)} ariaLabel="copy user code">
                      copy
                    </Button>
                  </div>
                </Field>
                <Field label="verification page" hint={`polling every ${pollSeconds}s`}>
                  <div className="flex min-w-0 items-start gap-2">
                    <a
                      href={challenge.verification_uri}
                      target="_blank"
                      rel="noreferrer"
                      className="min-w-0 flex-1 break-all rounded-lg border border-line-strong bg-[var(--color-surface-2)] px-2.5 py-1.5 font-mono text-small text-accent underline underline-offset-2"
                    >
                      {challenge.verification_uri}
                    </a>
                    <Button onClick={() => copy(challenge.verification_uri)} ariaLabel="copy verification page">
                      copy
                    </Button>
                  </div>
                </Field>
                <p className="text-caption text-faint">
                  Enter the code on that page. This dialog polls the gateway and completes the
                  sign-in by itself.
                </p>
              </>
            ) : null}
          </>
        )}
      </div>
    </Modal>
  );
}
