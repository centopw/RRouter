import { useState, type FormEvent } from "react";
import { Navigate, useNavigate } from "react-router-dom";
import { useQueryClient } from "@tanstack/react-query";
import { ApiError } from "../../lib/api";
import { apiCode, setupAdmin } from "../../lib/auth";
import { useAuthState } from "../../lib/queries";
import {
  AuthCard,
  AuthField,
  AuthInput,
  AuthNotice,
  AuthShell,
  AuthTitle,
  PasswordField,
  RecoveryCodes,
} from "../../components/auth-form";
import { Badge, Button } from "../../components/ui";

/** A1 first run, then A2 recovery codes. The setup code proves control of the machine. */
export default function SetupAdmin() {
  const state = useAuthState();
  const client = useQueryClient();
  const navigate = useNavigate();
  const [code, setCode] = useState("");
  const [name, setName] = useState("");
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState<{ message: string; field?: string } | null>(null);
  const [codes, setCodes] = useState<string[] | null>(null);
  const [stored, setStored] = useState(false);

  if (state.data?.initialized && codes === null) {
    return <Navigate to="/sign-in" replace />;
  }

  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (busy) return;
    setBusy(true);
    setFailure(null);
    setupAdmin({ setup_code: code.trim(), name: name.trim(), email: email.trim(), password })
      .then(async (result) => {
        await client.invalidateQueries({ queryKey: ["auth", "state"] });
        setCodes(result.recovery_codes);
      })
      .catch((error: unknown) => {
        const message = error instanceof Error ? error.message : String(error);
        const failureCode = error instanceof ApiError ? apiCode(error) : "";
        const field =
          failureCode === "setup_code_invalid"
            ? "setup_code"
            : failureCode === "invalid_email"
              ? "email"
              : failureCode === "password_too_short"
                ? "password"
                : failureCode === "invalid_name"
                  ? "name"
                  : undefined;
        setFailure({ message, field });
      })
      .finally(() => setBusy(false));
  };

  if (codes !== null) {
    return (
      <AuthShell width={540}>
        <AuthCard>
          <AuthTitle
            badge={<Badge tone="pos">Account created · 2 of 2</Badge>}
            title="Save your recovery codes"
            lead="There is no reset email on a self-hosted gateway. Each of these works once, and this is the only time they are shown."
          />
          <RecoveryCodes codes={codes} />
          <div className="mt-5 rounded-[10px] border border-[var(--color-warn-line)] bg-[var(--color-warn-tint)] px-3 py-2 text-[12.5px] text-[var(--color-warn-ink)]">
            These are shown only once.
          </div>
          <label className="mt-4 flex items-start gap-2 text-[13px]">
            <input
              type="checkbox"
              checked={stored}
              onChange={(event) => setStored(event.target.checked)}
              className="mt-0.5 h-4 w-4 accent-[var(--color-accent-text)]"
            />
            I have stored these somewhere safe
          </label>
          <Button
            variant="primary"
            className="mt-5 w-full"
            disabled={!stored}
            onClick={() => navigate("/setup", { replace: true })}
          >
            Continue to setup
          </Button>
        </AuthCard>
      </AuthShell>
    );
  }

  const canSubmit =
    code.trim().length > 0 && name.trim().length > 0 && email.trim().length > 0 && password.length >= 12;

  return (
    <AuthShell width={960}>
      <div className="grid gap-6 min-[880px]:grid-cols-[minmax(0,1fr)_320px]">
        <AuthCard>
          <AuthTitle
            badge={<Badge tone="accent">First-time setup · 1 of 2</Badge>}
            title="Create the administrator account"
            lead="This is the first account on this RRouter. It becomes the owner, with full admin permission."
          />
          <form onSubmit={submit} className="flex flex-col gap-4">
            <AuthField
              label="Setup code"
              htmlFor="setup-code"
              hint={
                <>
                  Printed in the terminal where RRouter started, as{" "}
                  <code className="font-mono text-[11px]">Setup code: XXXX-XXXX</code>. It proves
                  you control this machine.
                </>
              }
              problem={failure?.field === "setup_code" ? failure.message : null}
            >
              <AuthInput
                id="setup-code"
                value={code}
                onChange={setCode}
                autoComplete="off"
                ariaLabel="setup code"
                disabled={busy}
              />
            </AuthField>
            <AuthField
              label="Full name"
              htmlFor="setup-name"
              problem={failure?.field === "name" ? failure.message : null}
            >
              <AuthInput
                id="setup-name"
                value={name}
                onChange={setName}
                autoComplete="name"
                disabled={busy}
              />
            </AuthField>
            <AuthField
              label="Email"
              htmlFor="setup-email"
              problem={failure?.field === "email" ? failure.message : null}
            >
              <AuthInput
                id="setup-email"
                type="email"
                value={email}
                onChange={setEmail}
                autoComplete="username"
                disabled={busy}
              />
            </AuthField>
            <PasswordField
              id="setup-password"
              label="Password"
              value={password}
              onChange={setPassword}
              autoComplete="new-password"
              disabled={busy}
            />
            {failure && !failure.field ? <AuthNotice>{failure.message}</AuthNotice> : null}
            <Button type="submit" variant="primary" className="w-full" disabled={!canSubmit || busy}>
              {busy ? "Creating…" : "Create admin account"}
            </Button>
            <p className="text-center text-[11.5px] text-faint">Everything stays on this machine.</p>
          </form>
        </AuthCard>

        <aside className="rounded-[16px] border border-line bg-surface-2 px-5 py-6">
          <h2 className="text-[13px] font-semibold">What an admin can do</h2>
          <ul className="mt-3 space-y-2 text-[12.5px] text-muted">
            {[
              "Add, test and remove provider accounts",
              "Edit models, aliases and fallback combos",
              "Create and revoke gateway keys",
              "Invite people and manage their roles",
            ].map((item) => (
              <li key={item} className="flex gap-2">
                <span aria-hidden className="text-pos">
                  ✓
                </span>
                {item}
              </li>
            ))}
          </ul>
          <h2 className="mt-5 text-[13px] font-semibold">Roles you can give later</h2>
          <dl className="mt-2 space-y-2 text-[12.5px]">
            <div>
              <dt className="font-medium">Admin</dt>
              <dd className="text-muted">Everything on this page.</dd>
            </div>
            <div>
              <dt className="font-medium">Member</dt>
              <dd className="text-muted">Signs in; admin surfaces stay closed.</dd>
            </div>
            <div>
              <dt className="font-medium">Viewer</dt>
              <dd className="text-muted">Read-only account.</dd>
            </div>
          </dl>
          <p className="mt-5 border-t border-line pt-4 text-[11.5px] leading-relaxed text-faint">
            The first account is the permanent owner. It can't be removed or demoted.
          </p>
        </aside>
      </div>
    </AuthShell>
  );
}
