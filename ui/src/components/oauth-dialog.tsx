import { useEffect, useRef, useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { Badge, Button, Dot, ErrorNote, Field, Modal } from "./ui";
import { api, type OAuthProvider, type OAuthStart } from "../lib/api";
import { qk, useOAuthStatus } from "../lib/queries";

type CompleteResult = {
  id: string;
  provider: string;
  name: string | null;
  warning: string | null;
};

/** One in-flight sign-in. Owns its start request, its polling, and its cancellation. */
export function OAuthDialog({
  provider,
  authWindow,
  onClose,
  onConnected,
}: {
  provider: OAuthProvider;
  /** A blank popup opened during the click, so the browser treats it as user-initiated. */
  authWindow: Window | null;
  onClose: () => void;
  onConnected: (label: string, warning: string | null) => void;
}) {
  const client = useQueryClient();
  const [start, setStart] = useState<OAuthStart | null>(null);
  const [startError, setStartError] = useState<unknown>(null);
  const started = useRef(false);
  const completing = useRef(false);

  useEffect(() => {
    if (started.current) return;
    started.current = true;
    api
      .post<OAuthStart>(`/api/oauth/${provider.id}/start`)
      .then((s) => {
        setStart(s);
        if (s.challenge.kind === "pkce") {
          // The popup was opened synchronously by the click; navigate it only once the
          // server has produced the authorize URL.
          if (authWindow && !authWindow.closed) {
            authWindow.location.replace(s.challenge.authorize_url);
          } else {
            window.open(s.challenge.authorize_url, "_blank", "noopener,width=520,height=720");
          }
        }
      })
      .catch(setStartError);
  }, [provider.id, authWindow]);

  const challenge = start?.challenge ?? null;
  const pollSeconds = challenge?.kind === "device_code" ? Math.max(challenge.interval, 2) : 2;
  const status = useOAuthStatus(provider.id, start?.id ?? null, pollSeconds * 1000);

  const complete = useMutation({
    mutationFn: (id: string) =>
      api.post<CompleteResult>(`/api/oauth/${provider.id}/complete`, { id }),
    onSuccess: (result) => {
      completing.current = true;
      void client.invalidateQueries({ queryKey: qk.connections });
      void client.invalidateQueries({ queryKey: qk.modelProviders });
      void client.invalidateQueries({ queryKey: qk.system });
      onConnected(result.name ?? provider.display, result.warning);
      onClose();
    },
  });

  // The server stops reporting pending once the provider callback has landed.
  useEffect(() => {
    if (!start || status.data?.pending !== false || completing.current) return;
    completing.current = true;
    complete.mutate(start.id);
  }, [start, status.data?.pending, complete]);

  const close = () => {
    if (start && !completing.current) {
      void api.del<{ cancelled: boolean }>(`/api/oauth/${provider.id}/${start.id}`).catch(() => {});
    }
    onClose();
  };

  const copy = (value: string) => {
    void navigator.clipboard.writeText(value);
  };

  return (
    <Modal
      title={`Sign in — ${provider.display}`}
      open
      onClose={close}
      footer={
        <>
          <Button variant="quiet" onClick={close}>
            cancel
          </Button>
          <Button
            variant="primary"
            disabled={!start || complete.isPending}
            onClick={() => {
              if (start) complete.mutate(start.id);
            }}
          >
            {complete.isPending ? "completing…" : "complete"}
          </Button>
        </>
      }
    >
      <div className="space-y-4">
        <div className="flex items-center gap-2 text-[13px] text-muted">
          <Dot tone={status.data?.pending === false ? "pos" : "warn"} />
          <span aria-live="polite">
            {startError
              ? "could not start the sign-in"
              : !start
                ? "starting…"
                : complete.isPending
                  ? "exchanging the authorization code…"
                  : status.data?.pending === false
                    ? "approved — finishing"
                    : status.isError
                      ? "approved? the status check failed"
                      : "waiting for approval"}
          </span>
          <span className="ml-auto flex items-center gap-1.5">
            <Badge tone="neutral">{provider.flow}</Badge>
          </span>
        </div>

        {startError ? <ErrorNote error={startError} /> : null}
        {complete.isError ? <ErrorNote error={complete.error} /> : null}

        {!start ? (
          <div className="rounded-lg border border-line-strong bg-surface-2 px-3 py-6 text-center text-[13px] text-faint">
            requesting an authorization challenge…
          </div>
        ) : challenge?.kind === "pkce" ? (
          <>
            <p className="text-[13px] text-muted">
              A browser tab opened at the provider. Approve access there; this dialog finishes
              automatically once the loopback listener receives the callback.
            </p>
            <Field label="redirect uri" hint="the loopback the browser must reach">
              <div className="flex items-center gap-2">
                <code className="min-w-0 flex-1 truncate rounded-lg border border-line-strong bg-[#0e0e0e] px-2.5 py-1.5 font-mono text-[12px] text-muted">
                  {challenge.redirect_uri}
                </code>
                <Button onClick={() => copy(challenge.redirect_uri)}>copy</Button>
              </div>
            </Field>
            <Field label="authorize url" hint="if the tab did not open, use this">
              <div className="flex items-center gap-2">
                <a
                  href={challenge.authorize_url}
                  target="_blank"
                  rel="noreferrer"
                  className="min-w-0 flex-1 truncate rounded-lg border border-line-strong bg-[#0e0e0e] px-2.5 py-1.5 font-mono text-[12px] text-accent underline underline-offset-2"
                >
                  {challenge.authorize_url}
                </a>
                <Button onClick={() => copy(challenge.authorize_url)}>copy</Button>
              </div>
            </Field>
            <p className="text-[11px] text-faint">
              If the provider cannot reach the loopback, approve anyway and press <b>complete</b> —
              the server still holds the pending exchange.
            </p>
          </>
        ) : challenge ? (
          <>
            <Field label="user code" hint={`expires in ${Math.round(challenge.expires_in / 60)} min`}>
              <div className="flex items-center gap-2">
                <code className="min-w-0 flex-1 rounded-lg border border-line-strong bg-[#0e0e0e] px-3 py-2 font-mono text-[24px] tracking-[0.18em] text-fg">
                  {challenge.user_code}
                </code>
                <Button onClick={() => copy(challenge.user_code)}>copy</Button>
              </div>
            </Field>
            <Field label="verification page" hint={`polling every ${pollSeconds}s`}>
              <div className="flex items-center gap-2">
                <a
                  href={challenge.verification_uri}
                  target="_blank"
                  rel="noreferrer"
                  className="min-w-0 flex-1 truncate rounded-lg border border-line-strong bg-[#0e0e0e] px-2.5 py-1.5 font-mono text-[12px] text-accent underline underline-offset-2"
                >
                  {challenge.verification_uri}
                </a>
                <Button onClick={() => copy(challenge.verification_uri)}>copy</Button>
              </div>
            </Field>
            <p className="text-[11px] text-faint">
              Enter the code on that page. This dialog polls the gateway and completes the sign-in
              by itself.
            </p>
          </>
        ) : null}
      </div>
    </Modal>
  );
}
