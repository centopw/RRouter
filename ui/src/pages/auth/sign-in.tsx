import { useState, type FormEvent } from "react";
import { useNavigate, useSearchParams } from "react-router-dom";
import { useQueryClient } from "@tanstack/react-query";
import { apiCode, signIn } from "../../lib/auth";
import {
  AuthActions,
  AuthCard,
  AuthField,
  AuthFooterLink,
  AuthInput,
  AuthNotice,
  AuthPrimary,
  AuthShell,
  AuthTitle,
  PasswordField,
} from "../../components/auth-form";
import { Button } from "../../components/ui";

/** Only same-origin paths are honoured as a `next` target. */
export function safeNext(raw: string | null): string {
  if (!raw || !raw.startsWith("/") || raw.startsWith("//") || raw.startsWith("/sign-in")) return "/";
  return raw;
}

export default function SignIn() {
  const [params] = useSearchParams();
  const navigate = useNavigate();
  const client = useQueryClient();
  const next = safeNext(params.get("next"));
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [remember, setRemember] = useState(true);
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState<string | null>(null);

  const submit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    if (busy) return;
    // Password managers and browser autofill can write the visible fields without firing
    // `input`, leaving React state empty while the form looks filled. Read the DOM values.
    const data = new FormData(event.currentTarget);
    const submittedEmail = String(data.get("signin-email") ?? email).trim();
    const submittedPassword = String(data.get("signin-password") ?? password);
    setEmail(submittedEmail);
    setBusy(true);
    setFailure(null);
    signIn({ email: submittedEmail, password: submittedPassword, remember })
      .then(async () => {
        await client.invalidateQueries({ queryKey: ["auth", "state"] });
        navigate(next, { replace: true });
      })
      .catch((error: unknown) => {
        const code = apiCode(error);
        setFailure(
          code === "too_many_attempts"
            ? "Too many attempts. Try again in a minute."
            : "Email or password is incorrect. Check both, or reset your password.",
        );
        setPassword("");
      })
      .finally(() => setBusy(false));
  };

  const registerTo = `/register${next !== "/" ? `?next=${encodeURIComponent(next)}` : ""}`;

  return (
    <AuthShell>
      <AuthCard>
        <AuthTitle title="Sign in" lead="Welcome back. Sign in to continue." />
        <form onSubmit={submit} className="flex flex-col gap-5">
          <AuthField label="Email" htmlFor="signin-email">
            <AuthInput
              id="signin-email"
              name="signin-email"
              type="email"
              value={email}
              onChange={setEmail}
              autoComplete="username"
              disabled={busy}
            />
          </AuthField>
          <PasswordField
            id="signin-password"
            name="signin-password"
            label="Password"
            value={password}
            onChange={setPassword}
            autoComplete="current-password"
            disabled={busy}
            rules={false}
            aside={<AuthFooterLink to="/forgot">Forgot password?</AuthFooterLink>}
          />
          <label className="mt-1 flex items-center gap-2 text-[13px]">
            <input
              type="checkbox"
              checked={remember}
              onChange={(event) => setRemember(event.target.checked)}
              className="h-4 w-4 accent-[var(--color-accent-text)]"
            />
            Keep me signed in on this device
          </label>
          {failure ? <AuthNotice>{failure}</AuthNotice> : null}
          <AuthActions
            secondary={
              <Button size="lg" onClick={() => navigate(registerTo)}>
                Register
              </Button>
            }
          >
            <AuthPrimary disabled={busy}>{busy ? "Signing in…" : "Sign in"}</AuthPrimary>
          </AuthActions>
        </form>
      </AuthCard>

      <p className="mt-6 text-center text-[12.5px] text-muted">
        Signing up is by invitation. Register with the link an admin sent you.
      </p>
      <p className="mt-3 text-center">
        <AuthFooterLink to={`/sign-in/token${next !== "/" ? `?next=${encodeURIComponent(next)}` : ""}`}>
          Sign in with an admin token
        </AuthFooterLink>
      </p>
    </AuthShell>
  );
}
