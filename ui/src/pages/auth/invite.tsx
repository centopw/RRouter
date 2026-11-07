import { useState, type FormEvent } from "react";
import { useNavigate, useParams, useSearchParams } from "react-router-dom";
import { safeNext } from "./sign-in";
import { useQuery } from "@tanstack/react-query";
import { apiCode, inviteMeta, signUp } from "../../lib/auth";
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
  RecoveryCodes,
} from "../../components/auth-form";
import { Button } from "../../components/ui";

const ROLE_LEAD: Record<string, string> = {
  admin: "An admin can add providers, edit routing and manage keys and people.",
  member: "A member signs in, but the admin surfaces stay closed.",
  viewer: "A viewer can sign in and read, without changing anything.",
};

/** C1 join by invitation, C2 expired/used/revoked link. */
export default function Invite() {
  const { token = "" } = useParams();
  const navigate = useNavigate();
  const [params] = useSearchParams();
  const meta = useQuery({
    queryKey: ["auth", "invite", token],
    queryFn: () => inviteMeta(token),
    retry: false,
    staleTime: Infinity,
  });
  const [name, setName] = useState("");
  const [password, setPassword] = useState("");
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState<string | null>(null);
  const [codes, setCodes] = useState<string[] | null>(null);

  if (meta.isLoading) {
    return (
      <AuthShell>
        <p className="text-center text-[13px] text-muted">Checking your invitation…</p>
      </AuthShell>
    );
  }
  if (meta.isError) {
    return (
      <AuthShell>
        <AuthCard>
          <AuthTitle
            title="This invitation has expired"
            lead="Invitation links work once and expire. Ask an admin to send you a new one."
          />
          <AuthActions>
            <AuthPrimary type="button" onClick={() => navigate("/sign-in", { replace: true })}>
              Back to sign in
            </AuthPrimary>
          </AuthActions>
        </AuthCard>
      </AuthShell>
    );
  }

  if (codes !== null) {
    return (
      <AuthShell width={540}>
        <AuthCard>
          <AuthTitle
            badge={<span className="text-[11.5px] font-medium text-pos">Account created</span>}
            title="Save your recovery codes"
            lead="There is no reset email here. Each code works once, and this is the only time they are shown."
          />
          <RecoveryCodes codes={codes} />
          <AuthActions>
            <AuthPrimary type="button" onClick={() => navigate(safeNext(params.get("next")), { replace: true })}>
              Continue
            </AuthPrimary>
          </AuthActions>
        </AuthCard>
      </AuthShell>
    );
  }

  const invite = meta.data!;
  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (busy) return;
    setBusy(true);
    setFailure(null);
    signUp({ token, name: name.trim(), password })
      .then((result) => setCodes(result.recovery_codes))
      .catch((error: unknown) => {
        const kind = apiCode(error);
        setFailure(
          kind === "invite_invalid" || kind === "invite_required"
            ? "This invitation has expired. Ask an admin for a new one."
            : kind === "password_too_short"
              ? "Use at least 12 characters."
              : error instanceof Error
                ? error.message
                : String(error),
        );
      })
      .finally(() => setBusy(false));
  };

  return (
    <AuthShell>
      <AuthCard>
        <div className="mb-4 flex items-center gap-3">
          <span
            aria-hidden
            className="flex h-9 w-9 items-center justify-center rounded-full bg-surface-2 text-[13px] font-semibold"
          >
            {invite.inviter_name.slice(0, 1).toUpperCase()}
          </span>
          <p className="text-[12.5px] text-muted">
            <span className="font-medium text-fg">{invite.inviter_name}</span> invited you to join
            this RRouter as a{" "}
            <span className="font-medium capitalize text-fg">{invite.role}</span>.
          </p>
        </div>
        <AuthTitle
          title="Create your account"
          lead={ROLE_LEAD[invite.role] ?? "You will be able to sign in to this RRouter."}
        />
        <form onSubmit={submit} className="flex flex-col gap-4">
          <AuthField label="Full name" htmlFor="invite-name">
            <AuthInput id="invite-name" value={name} onChange={setName} autoComplete="name" disabled={busy} />
          </AuthField>
          <AuthField label="Email" htmlFor="invite-email" hint="from your invitation">
            <AuthInput id="invite-email" value={invite.email} onChange={() => {}} disabled />
          </AuthField>
          <PasswordField
            id="invite-password"
            label="Password"
            value={password}
            onChange={setPassword}
            autoComplete="new-password"
            disabled={busy}
          />
          {failure ? <AuthNotice>{failure}</AuthNotice> : null}
          <AuthActions
            secondary={
              <Button size="lg" onClick={() => navigate("/sign-in", { replace: true })}>
                Sign in
              </Button>
            }
          >
            <AuthPrimary disabled={busy || name.trim() === "" || password.length < 12}>
              {busy ? "Creating…" : "Create account"}
            </AuthPrimary>
          </AuthActions>
        </form>      </AuthCard>
    </AuthShell>
  );
}
