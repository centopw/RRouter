import { useState, type FormEvent } from "react";
import { useNavigate, useSearchParams } from "react-router-dom";
import { setAdminToken } from "../../lib/api";
import { AuthActions, AuthCard, AuthField, AuthInput, AuthNotice, AuthPrimary, AuthShell, AuthTitle } from "../../components/auth-form";
import { Button } from "../../components/ui";
import { safeNext } from "./sign-in";

/**
 * Break-glass sign-in for deployments that run RROUTER_ADMIN_TOKEN. The token is kept in
 * session storage only and never becomes a user session, so it has its own screen rather
 * than a form tucked under the normal one.
 */
export default function TokenSignIn() {
  const navigate = useNavigate();
  const [params] = useSearchParams();
  const next = safeNext(params.get("next"));
  const [token, setToken] = useState("");
  const [failure, setFailure] = useState<string | null>(null);

  const submit = (event: FormEvent) => {
    event.preventDefault();
    const value = token.trim();
    if (!value) return;
    setAdminToken(value);
    setFailure(null);
    navigate(next, { replace: true });
  };

  return (
    <AuthShell>
      <AuthCard>
        <AuthTitle
          title="Use an admin token"
          lead="For automation and break-glass access when user sign-in is unavailable. The token stays in this tab."
        />
        <form onSubmit={submit} className="flex flex-col gap-5">
          <AuthField
            label="Admin token"
            htmlFor="admin-token"
            hint="The value of RROUTER_ADMIN_TOKEN on this gateway."
          >
            <AuthInput id="admin-token" type="password" value={token} onChange={setToken} autoComplete="off" />
          </AuthField>
          {failure ? <AuthNotice>{failure}</AuthNotice> : null}
          <AuthActions
            secondary={
              <Button size="lg" onClick={() => navigate("/sign-in", { replace: true })}>
                Back to sign in
              </Button>
            }
          >
            <AuthPrimary disabled={token.trim() === ""}>Use token</AuthPrimary>
          </AuthActions>
        </form>
        <p className="mt-5 text-center text-[11.5px] text-faint">
          A wrong token is rejected by the API; sign in as a user instead.
        </p>
      </AuthCard>
    </AuthShell>
  );
}
