import { useEffect, useState } from "react";
import {
  Badge,
  Button,
  Card,
  CardHeader,
  CopyChip,
  ErrorNote,
  Field,
  Input,
  PageHeader,
} from "../../components/ui";
import { useMe, useUpdateMe } from "../../lib/queries";
import { when } from "../../lib/format";

/** Your identity and the one session you are reading this on. */
export default function ProfileSettings() {
  const me = useMe();
  const update = useUpdateMe();
  const [name, setName] = useState("");
  const [notice, setNotice] = useState<string | null>(null);
  const [failure, setFailure] = useState<unknown>(null);

  useEffect(() => {
    if (me.data) setName(me.data.user.name);
  }, [me.data]);

  const user = me.data?.user;
  const sessions = me.data?.sessions ?? [];
  const currentSession = sessions.find((session) => session.current) ?? null;

  const saveName = () => {
    setFailure(null);
    setNotice(null);
    update.mutate(
      { name: name.trim() },
      { onSuccess: () => setNotice("Name updated."), onError: setFailure },
    );
  };

  return (
    <>
      <PageHeader
        title="Profile"
        subtitle="How you appear in the dashboard and audit trail, and the session you are signed in on."
      />

      {me.isError ? (
        <div className="mb-4">
          <ErrorNote error={me.error} onRetry={() => void me.refetch()} />
        </div>
      ) : null}
      {notice ? (
        <p className="mb-4 text-small text-[var(--color-pos)]" role="status">
          {notice}
        </p>
      ) : null}

      <div className="grid gap-4 min-[900px]:grid-cols-2">
        <Card>
          <CardHeader
            title="Identity"
            action={user ? <Badge tone="accent">{user.role}</Badge> : null}
          />
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
          <CardHeader
            title="This session"
            action={currentSession ? <Badge tone="pos">this device</Badge> : null}
          />
          {currentSession ? (
            <div className="flex flex-col gap-3 px-5 pb-5">
              <div>
                <p className="text-small font-medium text-fg">
                  {currentSession.device_label ?? "Unknown device"}
                </p>
                <p className="mt-1 text-caption text-muted tnum">
                  Signed in {when(currentSession.created_at)} · expires{" "}
                  {when(currentSession.expires_at)}
                </p>
              </div>
              {currentSession.user_agent ? (
                <p className="font-mono text-caption text-faint" title={currentSession.user_agent}>
                  {currentSession.user_agent}
                </p>
              ) : null}
              <div className="flex flex-wrap items-center gap-x-2 gap-y-1 text-caption text-faint">
                {currentSession.ip ? (
                  <span className="font-mono tnum" title="address that created this session">
                    {currentSession.ip}
                  </span>
                ) : null}
                <CopyChip
                  value={currentSession.id}
                  label="session reference"
                  className="w-[min(320px,46vw)]"
                />
              </div>
              <p className="text-caption text-faint">
                A session stores the address and user agent that created it, the label derived
                from that user agent, and its own timestamps.
              </p>
            </div>
          ) : (
            <p className="px-5 pb-5 text-small text-faint">No current session reported.</p>
          )}
        </Card>
      </div>

      {failure ? (
        <div className="mt-4">
          <ErrorNote error={failure} title="Could not update profile" />
        </div>
      ) : null}
    </>
  );
}
