import { useEffect, useState } from "react";
import {
  Badge,
  Button,
  Card,
  CardHeader,
  ConfirmDialog,
  CopyChip,
  EmptyState,
  ErrorNote,
  Field,
  Input,
  PageHeader,
  cx,
} from "../components/ui";
import { DataTable, type Column } from "../components/table";
import type { SessionRow } from "../lib/auth";
import { useMe, useRevokeOtherSessions, useRevokeSession, useUpdateMe } from "../lib/queries";
import { usePhone } from "../lib/media";
import { when } from "../lib/format";

/** Sessions are identified by a hash prefix; the full token never leaves the gateway. */
function shortRef(id: string): string {
  return `session ${id.slice(0, 12)}`;
}

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
  const currentSession = sessions.find((session) => session.current) ?? null;
  const phone = usePhone();

  const saveName = () => {
    setFailure(null);
    update.mutate(
      { name: name.trim() },
      { onSuccess: () => setNotice("Name updated."), onError: setFailure },
    );
  };

  const confirmRevoke = (session: SessionRow) =>
    setConfirming({
      title: "Sign out this device",
      message: `${shortRef(session.id)}, signed in ${when(session.created_at)}. That device has to sign in again.`,
      run: () => revoke.mutate(session.id),
    });

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
      render: (row) => (
        <div className="flex flex-col">
          <span className="whitespace-nowrap text-muted">{when(row.created_at)}</span>
          {/* Below md the expiry column is hidden and folds in here, so a phone keeps
              both timestamps without a second column pushing the device off-screen. */}
          <span className="whitespace-nowrap text-caption text-faint md:hidden">
            expires {when(row.expires_at)}
          </span>
        </div>
      ),
    },
    {
      key: "expires",
      title: "expires",
      className: "hidden md:table-cell",
      render: (row) => <span className="whitespace-nowrap text-muted">{when(row.expires_at)}</span>,
    },
    {
      key: "current",
      title: "device",
      render: (row) => <SessionDevice session={row} clamped />,
    },
    {
      key: "actions",
      title: "",
      align: "right",
      render: (row) =>
        row.current ? null : <SessionRevoke session={row} onConfirm={confirmRevoke} />,
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
        <p className="mb-4 rounded-[8px] border border-[var(--color-pos)]/35 bg-[var(--color-pos-tint)] px-3 py-2 text-small text-[var(--color-pos)]">
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
          {currentSession ? (
            <div className="border-b border-line px-5 py-4">
              <div className="flex flex-wrap items-start justify-between gap-3">
                <div className="min-w-0">
                  <p className="text-small font-medium text-fg">This device</p>
                  <p className="mt-1 text-small text-fg">
                    {currentSession.device_label ?? "Unknown device"}
                  </p>
                  <p className="mt-1 text-caption text-muted">
                    Signed in {when(currentSession.created_at)} · expires{" "}
                    {when(currentSession.expires_at)}
                  </p>
                  <div className="mt-1 flex min-w-0 flex-col gap-0.5 text-caption text-faint">
                    {currentSession.user_agent ? (
                      <span
                        className="max-w-[62ch] truncate font-mono"
                        title={currentSession.user_agent}
                      >
                        {currentSession.user_agent}
                      </span>
                    ) : null}
                    <span className="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-0.5">
                      {currentSession.ip ? (
                        <span className="font-mono" title="address that created this session">
                          {currentSession.ip}
                        </span>
                      ) : null}
                      <span className="font-mono" title="session reference">
                        {shortRef(currentSession.id)}
                      </span>
                    </span>
                  </div>
                  <p className="mt-2 max-w-[62ch] text-caption text-faint">
                    A session stores the address and user agent that created it, the label
                    derived from that user agent, and its own timestamps. Sessions written
                    before this metadata existed keep working and read as “Unknown device”.
                  </p>
                </div>
                <div className="flex shrink-0 items-center gap-2">
                  <Badge tone="pos">this device</Badge>
                  <CopyChip
                    value={currentSession.id}
                    label="session reference"
                    className="w-[min(320px,46vw)]"
                  />
                </div>
              </div>
            </div>
          ) : null}
          {phone ? (
            sessions.length === 0 ? (
              <EmptyState>No active sessions.</EmptyState>
            ) : (
              <SessionCards sessions={sessions} onConfirm={confirmRevoke} />
            )
          ) : (
            <DataTable
              columns={columns}
              rows={sessions}
              rowKey={(row) => row.id}
              rowHeaderKey="created"
              loading={me.isLoading}
              empty="No active sessions."
            />
          )}
          <p className="border-t border-line px-5 py-3 text-caption text-faint">
            Addresses are taken from <span className="font-mono">X-Forwarded-For</span> when a
            proxy sets it and from the connection otherwise. The header is not authenticated,
            so behind a proxy these addresses are only as trustworthy as that proxy; a direct
            caller can send any value.
          </p>
        </Card>
      </div>
    </>
  );
}

/** One session's identity. Shared by the table cell and the phone card so both read the
 *  same fields in the same order, with the same honest placeholders. */
function SessionDevice({ session, clamped = false }: { session: SessionRow; clamped?: boolean }) {
  return (
    <div className={cx("flex min-w-0 flex-col gap-0.5", clamped && "max-w-[34ch]")}>
      <div className="flex min-w-0 flex-wrap items-center gap-1.5">
        <span
          className={cx(
            "min-w-0 truncate text-small",
            session.device_label ? "text-fg" : "text-faint",
          )}
          title={session.device_label ?? "the gateway did not recognise this user agent"}
        >
          {session.device_label ?? "Unknown device"}
        </span>
        {session.current ? <Badge tone="pos">this device</Badge> : null}
      </div>
      {session.user_agent ? (
        <span
          className="min-w-0 truncate font-mono text-caption text-faint"
          title={session.user_agent}
        >
          {session.user_agent}
        </span>
      ) : null}
      <span className="flex min-w-0 flex-wrap items-center gap-1.5 text-caption text-faint">
        {session.ip ? (
          <span className="shrink-0 font-mono" title="address that created this session">
            {session.ip}
          </span>
        ) : null}
        <span className="min-w-0 truncate font-mono" title="session reference">
          {shortRef(session.id)}
        </span>
      </span>
      {!session.device_label && !session.user_agent && !session.ip ? (
        <span className="text-caption text-faint">
          recorded before this gateway stored session details
        </span>
      ) : null}
    </div>
  );
}

/** The one revoke control; the accessible name carries the session so every row differs. */
function SessionRevoke({
  session,
  onConfirm,
}: {
  session: SessionRow;
  onConfirm: (session: SessionRow) => void;
}) {
  return (
    <Button
      variant="danger"
      ariaLabel={`Sign out ${shortRef(session.id)}, signed in ${when(session.created_at)}`}
      onClick={() => onConfirm(session)}
    >
      revoke
    </Button>
  );
}

/** Below `sm` a session is a card: four table columns would scroll sideways to reach
 *  revoke, which is the one thing a phone user comes here to do. */
function SessionCards({
  sessions,
  onConfirm,
}: {
  sessions: SessionRow[];
  onConfirm: (session: SessionRow) => void;
}) {
  return (
    <ul aria-label="Active sessions" className="flex flex-col">
      {sessions.map((session) => (
        <li
          key={session.id}
          className="border-b border-[var(--color-rule)] px-5 py-3 last:border-b-0"
        >
          <SessionDevice session={session} />
          <div className="mt-2 flex flex-wrap items-center justify-between gap-x-3 gap-y-1 text-caption text-muted">
            <span>
              signed in <span className="whitespace-nowrap">{when(session.created_at)}</span>
            </span>
            <span>
              expires <span className="whitespace-nowrap">{when(session.expires_at)}</span>
            </span>
          </div>
          {session.current ? null : (
            <div className="mt-2">
              <SessionRevoke session={session} onConfirm={onConfirm} />
            </div>
          )}
        </li>
      ))}
    </ul>
  );
}
