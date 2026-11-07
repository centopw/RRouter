import { useState, type FormEvent } from "react";
import { useNavigate, useSearchParams } from "react-router-dom";
import { AuthActions, AuthCard, AuthField, AuthInput, AuthPrimary, AuthShell, AuthTitle } from "../../components/auth-form";
import { Button } from "../../components/ui";
import { safeNext } from "./sign-in";

/** Extract the token from a full invitation URL, a path, or a bare code. */
export function inviteToken(raw: string): string {
  const text = raw.trim();
  if (!text) return "";
  const marker = text.split("/invite/")[1];
  const tail = marker ?? text;
  return tail.split(/[?#\s]/, 1)[0] ?? "";
}

/** Registration is invite-only: this screen takes the link and hands off to the join form. */
export default function Register() {
  const navigate = useNavigate();
  const [params] = useSearchParams();
  const next = safeNext(params.get("next"));
  const [link, setLink] = useState("");
  const token = inviteToken(link);
  const invalid = link.trim() !== "" && token.length < 8;

  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (!token) return;
    const suffix = next !== "/" ? `?next=${encodeURIComponent(next)}` : "";
    navigate(`/invite/${encodeURIComponent(token)}${suffix}`, { replace: true });
  };

  return (
    <AuthShell>
      <AuthCard>
        <AuthTitle
          title="Create your account"
          lead="Registration is by invitation. Paste the link an administrator sent you."
        />
        <form onSubmit={submit} className="flex flex-col gap-5">
          <AuthField
            label="Invitation link"
            htmlFor="register-link"
            hint="Looks like …/invite/abc123. The email and role come from the invitation."
            problem={invalid ? "that does not look like an invitation link" : null}
          >
            <AuthInput
              id="register-link"
              value={link}
              onChange={setLink}
              autoComplete="off"
              ariaLabel="invitation link"
              disabled={false}
            />
          </AuthField>
          <AuthActions
            secondary={<Button size="lg" onClick={() => navigate("/sign-in", { replace: true })}>Sign in</Button>}
          >
            <AuthPrimary disabled={!token || invalid}>Continue</AuthPrimary>
          </AuthActions>
        </form>
      </AuthCard>
    </AuthShell>
  );
}
