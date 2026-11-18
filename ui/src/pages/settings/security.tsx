import { useState } from "react";
import { Link } from "react-router-dom";
import { adminToken, setAdminToken } from "../../lib/api";
import {
  Button,
  Card,
  CardHeader,
  ErrorNote,
  Field,
  Input,
  PageHeader,
} from "../../components/ui";
import { useUpdateMe } from "../../lib/queries";

/** Credentials for this account and for this tab's break-glass admin token. */
export default function SecuritySettings() {
  const update = useUpdateMe();
  const [current, setCurrent] = useState("");
  const [next, setNext] = useState("");
  const [notice, setNotice] = useState<string | null>(null);
  const [failure, setFailure] = useState<unknown>(null);
  const [hasAdminToken, setHasAdminToken] = useState(() => adminToken() !== null);

  const savePassword = () => {
    setFailure(null);
    setNotice(null);
    update.mutate(
      { current_password: current, new_password: next },
      {
        onSuccess: () => {
          setCurrent("");
          setNext("");
          setNotice("Password changed. Other sessions were signed out.");
        },
        onError: setFailure,
      },
    );
  };

  return (
    <>
      <PageHeader
        title="Security"
        subtitle="Your password, and the admin token this tab is holding."
      />

      {notice ? (
        <p className="mb-4 text-small text-[var(--color-pos)]" role="status">
          {notice}
        </p>
      ) : null}

      <Card>
        <CardHeader title="Password" />
        <div className="flex max-w-[420px] flex-col gap-4 px-5 pb-5">
          <Field label="current password">
            <Input
              type="password"
              value={current}
              onChange={setCurrent}
              ariaLabel="current password"
              autoComplete="current-password"
            />
          </Field>
          <Field label="new password" hint="At least 12 characters. Other sessions end.">
            <Input
              type="password"
              value={next}
              onChange={setNext}
              ariaLabel="new password"
              autoComplete="new-password"
            />
          </Field>
          <div className="flex justify-end">
            <Button
              variant="primary"
              disabled={update.isPending || current === "" || next.length < 12}
              onClick={savePassword}
            >
              {update.isPending ? "saving…" : "Change password"}
            </Button>
          </div>
        </div>
      </Card>

      <Card className="mt-4">
        <CardHeader
          title="Admin token"
          action={
            <span className="text-caption text-faint">
              {hasAdminToken ? "stored in this tab" : "not stored"}
            </span>
          }
        />
        <div className="flex flex-wrap items-center justify-between gap-4 px-5 pb-5">
          <p className="max-w-[62ch] text-small leading-relaxed text-muted">
            {hasAdminToken ? (
              <>
                This tab is calling the admin API with <code className="font-mono text-caption text-fg">RROUTER_ADMIN_TOKEN</code>{" "}
                instead of, or alongside, your sign-in. The token lives in session storage and is
                discarded when the tab closes.
              </>
            ) : (
              <>
                No admin token is stored. A deployment that requires{" "}
                <code className="font-mono text-caption text-fg">RROUTER_ADMIN_TOKEN</code> can
                authenticate this tab with it; the token stays in session storage.
              </>
            )}
          </p>
          {hasAdminToken ? (
            <Button
              variant="danger"
              onClick={() => {
                setAdminToken(null);
                setHasAdminToken(false);
                setNotice("Admin token forgotten for this tab.");
              }}
            >
              Forget token
            </Button>
          ) : (
            <Link
              to="/sign-in/token"
              className="inline-flex h-8 items-center rounded-full border border-line bg-surface px-3 text-body text-fg transition-colors hover:border-line-strong hover:bg-surface-3"
            >
              Use an admin token
            </Link>
          )}
        </div>
      </Card>

      {failure ? (
        <div className="mt-4">
          <ErrorNote error={failure} title="Could not change password" />
        </div>
      ) : null}
    </>
  );
}
