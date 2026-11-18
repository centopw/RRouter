import { useState } from "react";
import {
  Badge,
  Button,
  Card,
  CardHeader,
  ConfirmDialog,
  CopyChip,
  EmptyState,
  ErrorNote,
  PageHeader,
  cx,
} from "../../components/ui";
import { DataTable, type Column } from "../../components/table";
import type { SessionRow } from "../../lib/auth";
import { useMe, useRevokeOtherSessions, useRevokeSession } from "../../lib/queries";
import { usePhone } from "../../lib/media";
import { when } from "../../lib/format";

/** Sessions are identified by a hash prefix; the full token never leaves the gateway. */
function shortRef(id: string): string {
  return `session ${id.slice(0, 12)}`;
}

/** Every device currently signed in to this account, with one revoke per row. */
export default function SessionsSettings() {
  const me = useMe();
  const revoke = useRevokeSession();
  const revokeOthers = useRevokeOtherSessions();
  const [failure, setFailure] = useState<unknown>(null);
  const [confirming, setConfirming] = useState<null | {
    title: string;
    message: string;
    run: () => void;
  }>(null);

  const sessions = me.data?.sessions ?? [];
  const currentSession = sessions.find((session) => session.current) ?? null;
  const phone = usePhone();

  const confirmRevoke = (session: SessionRow) =>
    setConfirming({
      title: "Sign out this device",
      message: `${shortRef(session.id)}, signed in ${when(session.created_at)}. That device has to sign in again.`,
      run: () => revoke.mutate(session.id, { onError: setFailure }),
    });

  const columns: Array<Column<SessionRow>> = [
    {
      key: "created",
      title: "signed in",
      render: (row) => (
        <div className="flex flex-col">
          <span className="whitespace-nowrap text-muted tnum">{when(row.created_at)}</span>
          {/* Below md the expiry column is hidden and folds in here, so a phone keeps
              both timestamps without a second column pushing the device off-screen. */}
          <span className="whitespace-nowrap text-caption text-faint tnum md:hidden">
            expires {when(row.expires_at)}
          </span>
        </div>
      ),
    },
    {
      key: "expires",
      title: "expires",
      className: "hidden md:table-cell",
      render: (row) => <span className="whitespace-nowrap text-muted tnum">{when(row.expires_at)}</span>,
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

      <PageHeader
        title="Sessions"
        subtitle="Every browser signed in to this account. Revoking ends that session only."
      />

      {me.isError ? (
        <div className="mb-4">
          <ErrorNote error={me.error} onRetry={() => void me.refetch()} />
        </div>
      ) : null}
      {failure ? (
        <div className="mb-4">
          <ErrorNote error={failure} title="Could not revoke" />
        </div>
      ) : null}

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
                    message:
                      "Every session except this browser ends immediately. Each device must sign in again.",
                    run: () => revokeOthers.mutate(undefined, { onError: setFailure }),
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
                <p className="mt-1 text-caption text-muted tnum">
                  Signed in {when(currentSession.created_at)} · expires{" "}
                  {when(currentSession.expires_at)}
                </p>
                <div className="mt-1 flex min-w-0 flex-col gap-0.5 text-caption text-faint">
                  {currentSession.user_agent ? (
                    <span className="max-w-[62ch] truncate font-mono" title={currentSession.user_agent}>
                      {currentSession.user_agent}
                    </span>
                  ) : null}
                  <span className="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-0.5">
                    {currentSession.ip ? (
                      <span className="font-mono tnum" title="address that created this session">
                        {currentSession.ip}
                      </span>
                    ) : null}
                    <span className="font-mono tnum" title="session reference">
                      {shortRef(currentSession.id)}
                    </span>
                  </span>
                </div>
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
          Addresses are taken from <span className="font-mono">X-Forwarded-For</span> when a proxy
          sets it and from the connection otherwise. The header is not authenticated, so behind a
          proxy these addresses are only as trustworthy as that proxy; a direct caller can send
          any value.
        </p>
      </Card>
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
        <span className="min-w-0 truncate font-mono text-caption text-faint" title={session.user_agent}>
          {session.user_agent}
        </span>
      ) : null}
      <span className="flex min-w-0 flex-wrap items-center gap-1.5 text-caption text-faint">
        {session.ip ? (
          <span className="shrink-0 font-mono tnum" title="address that created this session">
            {session.ip}
          </span>
        ) : null}
        <span className="min-w-0 truncate font-mono tnum" title="session reference">
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
          <div className="mt-2 flex flex-wrap items-center justify-between gap-x-3 gap-y-1 text-caption text-muted tnum">
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
