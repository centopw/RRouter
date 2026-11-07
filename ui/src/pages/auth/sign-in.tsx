import { useState, type FormEvent } from "react";
import { useNavigate, useSearchParams } from "react-router-dom";
import { useQueryClient } from "@tanstack/react-query";
import { apiCode, signIn } from "../../lib/auth";
import { setAdminToken } from "../../lib/api";
import {
  AuthCard,
  AuthField,
  AuthFooterLink,
  AuthInput,
  AuthNotice,
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
  const [tokenOpen, setTokenOpen] = useState(false);
  const [token, setToken] = useState("");

  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (busy) return;
    setBusy(true);
    setFailure(null);
    signIn({ email: email.trim(), password, remember })
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

  return (
    <AuthShell>
      <AuthCard>
        <AuthTitle title="Sign in" lead={`to RRouter at ${window.location.host}`} />
        <form onSubmit={submit} className="flex flex-col gap-5">
          <AuthField label="Email" htmlFor="signin-email">
            <AuthInput
              id="signin-email"
              type="email"
              value={email}
              onChange={setEmail}
              autoComplete="username"
              disabled={busy}
            />
          </AuthField>
          <PasswordField
            id="signin-password"
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
          <Button type="submit" variant="primary" className="w-full" disabled={busy}>
            {busy ? "Signing in…" : "Sign in"}
          </Button>
        </form>
      </AuthCard>

      <p className="mx-auto mt-7 max-w-[380px] text-center text-[12.5px] leading-relaxed text-muted">
        Got an invitation? Open the link an admin sent you to create your account.
      </p>

      <div className="mt-5 text-center">
        {tokenOpen ? (
          <div className="mx-auto mt-3 flex max-w-[340px] flex-col gap-2 text-left">
            <AuthField
              label="Admin token"
              htmlFor="admin-token"
              hint="RROUTER_ADMIN_TOKEN. Stored for this tab only."
            >
              <AuthInput id="admin-token" value={token} onChange={setToken} />
            </AuthField>
            <Button
              onClick={() => {
                setAdminToken(token.trim() || null);
                navigate(next, { replace: true });
              }}
              disabled={token.trim().length === 0}
            >
              Use token
            </Button>
          </div>
        ) : (
          <button
            type="button"
            onClick={() => setTokenOpen(true)}
            className="text-[11.5px] text-faint underline underline-offset-2 hover:text-muted"
          >
            Use an admin token instead
          </button>
        )}
      </div>
    </AuthShell>
  );
}
