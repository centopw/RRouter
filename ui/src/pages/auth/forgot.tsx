import { useState, type FormEvent } from "react";
import { useNavigate } from "react-router-dom";
import { apiCode, recover } from "../../lib/auth";
import {
  AuthActions,
  AuthCard,
  AuthField,
  AuthInput,
  AuthNotice,
  AuthPrimary,
  AuthShell,
  AuthTitle,
  PasswordField,
} from "../../components/auth-form";
import { Button, CopyButton } from "../../components/ui";
import { Heading } from "../../components/atoms";

/** B3: recovery-code reset, plus the two local escape hatches. */
export default function Forgot() {
  const navigate = useNavigate();
  const [email, setEmail] = useState("");
  const [code, setCode] = useState("");
  const [password, setPassword] = useState("");
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState<string | null>(null);
  const [done, setDone] = useState(false);

  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (busy) return;
    setBusy(true);
    setFailure(null);
    recover({ email: email.trim(), recovery_code: code.trim(), new_password: password })
      .then(() => setDone(true))
      .catch((error: unknown) => {
        const kind = apiCode(error);
        setFailure(
          kind === "password_too_short"
            ? "Use at least 12 characters for the new password."
            : "That email and recovery code don't match. Each code works once.",
        );
      })
      .finally(() => setBusy(false));
  };

  return (
    <AuthShell width={540}>
      {done ? (
        <AuthCard>
          <AuthTitle
            title="Password reset"
            lead="Every other session for this account was signed out. Sign in with the new password."
          />
          <AuthActions>
            <AuthPrimary type="button" onClick={() => navigate("/sign-in", { replace: true })}>
              Back to sign in
            </AuthPrimary>
          </AuthActions>
        </AuthCard>
      ) : (
        <AuthCard>
          <AuthTitle
            title="Reset your password"
            lead="RRouter runs on your own machine, so there is no reset email. Use whichever of these you can."
          />
          <form onSubmit={submit} className="flex flex-col gap-4">
            <AuthField label="Email" htmlFor="recover-email">
              <AuthInput id="recover-email" type="email" value={email} onChange={setEmail} autoComplete="username" disabled={busy} />
            </AuthField>
            <AuthField label="Recovery code" htmlFor="recover-code" hint="One of the ten saved when the account was created.">
              <AuthInput id="recover-code" value={code} onChange={setCode} autoComplete="off" disabled={busy} />
            </AuthField>
            <PasswordField
              id="recover-password"
              label="New password"
              value={password}
              onChange={setPassword}
              autoComplete="new-password"
              disabled={busy}
            />
            {failure ? <AuthNotice>{failure}</AuthNotice> : null}
            <AuthActions
              secondary={
                <Button size="lg" onClick={() => navigate("/sign-in", { replace: true })}>
                  Back to sign in
                </Button>
              }
            >
              <AuthPrimary disabled={busy || email.trim() === "" || code.trim() === "" || password.length < 12}>
                {busy ? "Resetting…" : "Reset password"}
              </AuthPrimary>
            </AuthActions>
          </form>

          <div className="mt-6 border-t border-line pt-5">
            <Heading level={2} className="text-body font-semibold">No recovery code?</Heading>
            <p className="mt-2 text-small text-muted">
              An administrator can reset the password from the terminal:
            </p>
            <div className="mt-2 flex items-center gap-2 rounded-md border border-line bg-surface-2 px-2.5 py-1.5 font-mono text-caption">
              <code className="min-w-0 flex-1 truncate">rrouter admin reset-password &lt;email&gt;</code>
              <CopyButton value="rrouter admin reset-password <email>" label="copy" />
            </div>
          </div>
        </AuthCard>
      )}
    </AuthShell>
  );
}
