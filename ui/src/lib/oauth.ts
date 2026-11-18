import { useCallback, useEffect, useRef, useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import {
  ApiError,
  api,
  type OAuthChallenge,
  type OAuthProvider,
  type OAuthStart,
} from "./api";
import { qk, useOAuthStatus } from "./queries";

export type OAuthCompleteResult = {
  id: string;
  provider: string;
  name: string | null;
  warning: string | null;
};

export type OAuthFlowPhase =
  | "starting"
  | "awaiting_callback"
  | "awaiting_approval"
  | "exchanging"
  | "expired"
  | "completed"
  | "error";

export type UseOAuthFlowOptions = {
  provider: OAuthProvider;
  /** Request manual paste-back mode for PKCE providers when supported by the gateway. */
  manual?: boolean;
  /** Stop status polling and enter the `expired` phase when a device code hits 0s. */
  stopOnExpire?: boolean;
  onStartError?: (error: unknown) => void;
  onComplete?: (result: OAuthCompleteResult) => void;
  onCompleteError?: (error: unknown) => void;
};

/**
 * What is wrong with a pasted callback URL, before the gateway ever sees it.
 *
 * The gateway is still the authority (it owns the `state` it issued); this only catches the
 * three mistakes an operator on a VPS actually makes: pasting the authorize URL, pasting a
 * fragment of the address, or pasting a callback from a different sign-in.
 */
export function callbackProblem(value: string, redirect: string): string | null {
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
export function callbackErrorText(error: unknown): string {
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

/** Format remaining device-code seconds as `m:ss`. */
export function formatOAuthClock(secs: number): string {
  const clamped = Math.max(0, Math.floor(secs));
  const m = Math.floor(clamped / 60);
  const s = clamped % 60;
  return `${m}:${String(s).padStart(2, "0")}`;
}

function cancelFlow(providerId: string, id: string): void {
  void api.del<{ cancelled: boolean }>(`/api/oauth/${providerId}/${id}`).catch(() => {});
}

/**
 * Shared OAuth state machine for both the provider dialog (manual paste-back + loopback +
 * device code) and the setup wizard (loopback PKCE + device code with expiry timer).
 */
export function useOAuthFlow({
  provider,
  manual = false,
  stopOnExpire = true,
  onStartError,
  onComplete,
  onCompleteError,
}: UseOAuthFlowOptions) {
  const client = useQueryClient();
  const [start, setStart] = useState<OAuthStart | null>(null);
  const [startError, setStartError] = useState<unknown>(null);
  const [connected, setConnected] = useState<OAuthCompleteResult | null>(null);
  const [remaining, setRemaining] = useState<number | null>(null);
  const [totalSeconds, setTotalSeconds] = useState(0);
  const [autoTriggered, setAutoTriggered] = useState(false);

  // Refs prevent inline callbacks from resetting an in-flight attempt or double-cancelling.
  const settledRef = useRef(false);
  const startIdRef = useRef<string | null>(null);
  const onStartErrorRef = useRef(onStartError);
  const onCompleteRef = useRef(onComplete);
  const onCompleteErrorRef = useRef(onCompleteError);
  onStartErrorRef.current = onStartError;
  onCompleteRef.current = onComplete;
  onCompleteErrorRef.current = onCompleteError;

  const invalidateProviderCaches = useCallback(() => {
    void client.invalidateQueries({ queryKey: qk.connections });
    void client.invalidateQueries({ queryKey: qk.modelProviders });
    void client.invalidateQueries({ queryKey: qk.models });
    void client.invalidateQueries({ queryKey: qk.system });
  }, [client]);

  const completeMutation = useMutation({
    mutationFn: (id: string) =>
      api.post<OAuthCompleteResult>(`/api/oauth/${provider.id}/complete`, { id }),
    onSuccess: (result) => {
      settledRef.current = true;
      invalidateProviderCaches();
      setConnected(result);
      onCompleteRef.current?.(result);
    },
    onError: (error: unknown) => {
      settledRef.current = true;
      onCompleteErrorRef.current?.(error);
    },
  });

  const completeManualMutation = useMutation({
    mutationFn: ({ id, url }: { id: string; url: string }) =>
      api.post<OAuthCompleteResult>(`/api/oauth/${provider.id}/complete`, {
        id,
        callback_url: url,
      }),
    onSuccess: (result) => {
      settledRef.current = true;
      invalidateProviderCaches();
      setConnected(result);
      onCompleteRef.current?.(result);
    },
    onError: (error: unknown) => {
      onCompleteErrorRef.current?.(error);
    },
  });

  const resetComplete = completeMutation.reset;
  const resetCompleteManual = completeManualMutation.reset;

  // (Re)start whenever the provider or manual mode changes; cancel unsettled attempts on cleanup.
  useEffect(() => {
    let live = true;
    let mine: string | null = null;
    settledRef.current = false;
    startIdRef.current = null;
    resetComplete();
    resetCompleteManual();
    setStart(null);
    setStartError(null);
    setConnected(null);
    setRemaining(null);
    setTotalSeconds(0);
    setAutoTriggered(false);

    api
      .post<OAuthStart>(
        `/api/oauth/${provider.id}/start`,
        manual ? { manual: true } : undefined,
      )
      .then((s) => {
        if (!live) {
          cancelFlow(provider.id, s.id);
          return;
        }
        mine = s.id;
        startIdRef.current = s.id;
        setStart(s);
      })
      .catch((error: unknown) => {
        if (!live) return;
        settledRef.current = true;
        setStartError(error);
        onStartErrorRef.current?.(error);
      });

    return () => {
      live = false;
      if (mine && !settledRef.current) {
        settledRef.current = true;
        cancelFlow(provider.id, mine);
      }
    };
  }, [provider.id, manual, resetComplete, resetCompleteManual]);

  const challenge: OAuthChallenge | null = start?.challenge ?? null;
  // Echoed `manual` confirms the gateway supports paste-back; otherwise fall back to loopback.
  const manualRefused =
    manual && challenge !== null && (challenge.kind !== "pkce" || challenge.manual !== true);
  const paste = manual && !manualRefused;

  // Device-code countdown timer.
  useEffect(() => {
    if (challenge?.kind !== "device_code" || connected !== null) return;
    setTotalSeconds(Math.max(challenge.expires_in, 1));
    setRemaining(challenge.expires_in);
    const timer = window.setInterval(() => {
      setRemaining((prev) => (prev === null ? null : Math.max(0, prev - 1)));
    }, 1_000);
    return () => window.clearInterval(timer);
  }, [challenge, connected]);

  const expired =
    challenge?.kind === "device_code" && remaining !== null && remaining <= 0;
  const pollSeconds =
    challenge?.kind === "device_code" ? Math.max(challenge.interval, 2) : 2;
  const pollIntervalMs =
    challenge?.kind === "device_code"
      ? Math.max(challenge.interval, 2) * 1_000
      : 2_000;

  const pollingPaused =
    paste ||
    !start ||
    connected !== null ||
    autoTriggered ||
    (stopOnExpire && expired);

  const status = useOAuthStatus(
    provider.id,
    pollingPaused ? null : start.id,
    pollIntervalMs,
  );

  const runComplete = completeMutation.mutate;
  const isCompletePending = completeMutation.isPending;

  // Exchange automatically once the server reports the provider callback has landed.
  useEffect(() => {
    if (
      paste ||
      !start ||
      (stopOnExpire && expired) ||
      autoTriggered ||
      settledRef.current ||
      isCompletePending
    ) {
      return;
    }
    if (status.data?.pending !== false) return;
    setAutoTriggered(true);
    settledRef.current = true;
    runComplete(start.id);
  }, [
    paste,
    start,
    stopOnExpire,
    expired,
    autoTriggered,
    isCompletePending,
    status.data?.pending,
    runComplete,
  ]);

  const complete = useCallback(() => {
    if (!start) return;
    settledRef.current = true;
    completeMutation.mutate(start.id);
  }, [start, completeMutation]);

  const completeManual = useCallback(
    (callbackUrl: string) => {
      if (!start) return;
      completeManualMutation.mutate({ id: start.id, url: callbackUrl.trim() });
    },
    [start, completeManualMutation],
  );

  const cancel = useCallback(() => {
    const id = startIdRef.current;
    if (id && !settledRef.current) {
      settledRef.current = true;
      cancelFlow(provider.id, id);
    }
  }, [provider.id]);

  const isCompleting =
    completeMutation.isPending || completeManualMutation.isPending;
  const isCompleteError = paste
    ? completeManualMutation.isError
    : completeMutation.isError;
  const completeError = paste
    ? completeManualMutation.error
    : completeMutation.error;

  const phase: OAuthFlowPhase = connected
    ? "completed"
    : startError
      ? "error"
      : stopOnExpire && expired
        ? "expired"
        : isCompleting
          ? "exchanging"
          : !paste && completeMutation.isError
            ? "error"
            : !start
              ? "starting"
              : paste
                ? "awaiting_callback"
                : "awaiting_approval";

  return {
    phase,
    start,
    startError,
    challenge,
    paste,
    manualRefused,
    pollSeconds,
    pollIntervalMs,
    status,
    remaining,
    totalSeconds,
    expired,
    connected,
    isCompleting,
    isCompleteError,
    completeError,
    complete,
    completeManual,
    cancel,
  };
}
