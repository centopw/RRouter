import { useEffect, useState } from "react";
import { Badge, Button, Card, CardHeader, ConfirmDialog, ErrorNote, Field, Input, PageHeader } from "../components/ui";
import { DataTable, type Column } from "../components/table";
import type { SessionRow } from "../lib/auth";
import { useMe, useRevokeOtherSessions, useRevokeSession, useUpdateMe } from "../lib/queries";
import { when } from "../lib/format";

/** The signed-in person's own profile, password and devices. */
export default function Account() {
  const me = useMe();
  const update = useUpdateMe();
  const revoke = useRevokeSession();
  const revokeOthers = useRevokeOtherSessions();
  const [name, setName] = useState("");
  const [current, setCurrent] = useState("");
  const [next, setNext] = useState("");
  const [notice, setNotice] = useState<string | null>(null);
  const [failure, setFailure] = useState<unknown>(null);
  const [confirming, setConfirming] = useState<null | { title: string; message: string; run: () => void }>(null);

  useEffect(() => {
    if (me.data) setName(me.data.user.name);
  }, [me.data]);

  const user = me.data?.user;
  const sessions = me.data?.sessions ?? [];

  const saveName = () => {
    setFailure(null);
    update.mutate(
      { name: name.trim() },
      { onSuccess: () => setNotice("Name updated."), onError: setFailure },
    );
  };

  const savePassword = () => {
    setFailure(null);
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

  const columns: Array<Column<SessionRow>> = [
    {
      key: "created",
      title: "signed in",
      render: (row) => <span className="text-muted">{when(row.created_at)}</span>,
    },
    {
      key: "expires",
      title: "expires",
      render: (row) => <span className="text-muted">{when(row.expires_at)}</span>,
    },
    {
      key: "current",
      title: "device",
      render: (row) => (row.current ? <Badge tone="pos">this device</Badge> : <Badge tone="neutral">other</Badge>),
    },
    {
      key: "actions",
      title: "",
      align: "right",
      render: (row) =>
        row.current ? null : (
          <Button
            variant="danger"
            onClick={() =>
              setConfirming({
                title: "Sign out this device",
                message: `End the session created ${when(row.created_at)}? That device has to sign in again.`,
                run: () => revoke.mutate(row.id),
              })
            }
          >
            revoke
          </Button>
        ),
    },
  ];

  return (
    <>
      <ConfirmDialog
        open={confirming !== null}
        title={confirming?.title ?? ""}
        message={confirming?.message ?? ""}
        confirmLabel="Sign out device"
        busy={revoke.isPending || revokeOthers.isPending}
        onConfirm={() => {
          confirming?.run();
          setConfirming(null);
        }}
        onClose={() => setConfirming(null)}
      />

      <PageHeader title="My account" subtitle="Your name, password and the devices currently signed in." />

      {me.isError ? (
        <div className="mb-4">
          <ErrorNote error={me.error} onRetry={() => void me.refetch()} />
        </div>
      ) : null}
      {notice ? (
        <p className="mb-4 rounded-[8px] border border-[var(--color-pos)]/35 bg-[var(--color-pos-tint)] px-3 py-2 text-[12.5px] text-[var(--color-pos)]">
          {notice}
        </p>
      ) : null}

      <div className="grid gap-4 min-[900px]:grid-cols-2">
        <Card>
          <CardHeader title="Profile" action={user ? <Badge tone="accent">{user.role}</Badge> : null} />
          <div className="flex flex-col gap-4 px-5 pb-5">
            <Field label="name" hint="Shown in the sidebar and the audit trail.">
              <Input value={name} onChange={setName} ariaLabel="display name" />
            </Field>
            <Field label="email" hint="Your sign-in identity; ask an admin to change it.">
              <Input value={user?.email ?? ""} onChange={() => {}} disabled ariaLabel="email" />
            </Field>
            <div className="flex justify-end">
              <Button
                variant="primary"
                disabled={update.isPending || !user || name.trim() === "" || name.trim() === user.name}
                onClick={saveName}
              >
                {update.isPending ? "saving…" : "Save name"}
              </Button>
            </div>
          </div>
        </Card>

        <Card>
          <CardHeader title="Password" />
          <div className="flex flex-col gap-4 px-5 pb-5">
            <Field label="current password">
              <Input type="password" value={current} onChange={setCurrent} ariaLabel="current password" />
            </Field>
            <Field label="new password" hint="At least 12 characters. Other sessions end.">
              <Input type="password" value={next} onChange={setNext} ariaLabel="new password" />
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
      </div>

      {failure ? (
        <div className="mt-4">
          <ErrorNote error={failure} />
        </div>
      ) : null}

      <div className="mt-4">
        <Card>
          <CardHeader
            title="Devices"
            action={
              sessions.length > 1 ? (
                <Button
                  variant="danger"
                  disabled={revokeOthers.isPending}
                  onClick={() =>
                    setConfirming({
                      title: "Sign out every other device",
                      message: "Every session except this browser ends immediately. Each device must sign in again.",
                      run: () => revokeOthers.mutate(),
                    })
                  }
                >
                  sign out others
                </Button>
              ) : null
            }
          />
          <DataTable columns={columns} rows={sessions} rowKey={(row) => row.id} loading={me.isLoading} empty="No active sessions." />
        </Card>
      </div>
    </>
  );
}
