import { useState, type FormEvent } from "react";
import {
  Badge,
  Button,
  Card,
  CardHeader,
  CopyChip,
  ConfirmDialog,
  ErrorNote,
  Field,
  Input,
  Modal,
  PageHeader,
  Select,
} from "../components/ui";
import { DataTable, type Column } from "../components/table";
import type { AuthUser, Invitation, UserRole } from "../lib/auth";
import {
  useAuthState,
  useCreateInvite,
  useInvites,
  useRemoveUser,
  useRevokeInvite,
  useUpdateUser,
  useUsers,
} from "../lib/queries";
import { when } from "../lib/format";

const ROLES: Array<{ id: UserRole; label: string }> = [
  { id: "admin", label: "Admin" },
  { id: "member", label: "Member" },
  { id: "viewer", label: "Viewer" },
];

/** Who can sign in to this RRouter, and the invitation links that let them. */
export default function Users() {
  const me = useAuthState();
  const users = useUsers(true);
  const invites = useInvites(true);
  const update = useUpdateUser();
  const remove = useRemoveUser();
  const revoke = useRevokeInvite();
  const [inviting, setInviting] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const [removing, setRemoving] = useState<AuthUser | null>(null);
  const [revoking, setRevoking] = useState<Invitation | null>(null);

  const columns: Array<Column<AuthUser>> = [
    {
      key: "name",
      title: "name",
      render: (u) => (
        <span className="flex min-w-0 items-center gap-2">
          <span className="truncate font-medium">{u.name}</span>
          {u.is_owner ? <Badge tone="accent">owner</Badge> : null}
          {u.id === me.data?.user?.id ? <Badge tone="neutral">you</Badge> : null}
        </span>
      ),
    },
    { key: "email", title: "email", render: (u) => <span className="text-muted">{u.email}</span> },
    {
      key: "role",
      title: "role",
      render: (u) =>
        u.is_owner ? (
          <span className="text-muted">admin</span>
        ) : (
          <Select
            value={u.role}
            onChange={(role) => update.mutate({ id: u.id, patch: { role: role as UserRole } })}
            ariaLabel={`role for ${u.email}`}
            disabled={update.isPending}
          >
            {ROLES.map((role) => (
              <option key={role.id} value={role.id}>
                {role.label}
              </option>
            ))}
          </Select>
        ),
    },
    {
      key: "status",
      title: "status",
      render: (u) =>
        u.disabled ? <Badge tone="warn">disabled</Badge> : <Badge tone="pos">active</Badge>,
    },
    {
      key: "last",
      title: "last sign in",
      render: (u) => (
        <span className="text-muted">{u.last_sign_in_at ? when(u.last_sign_in_at) : "never"}</span>
      ),
    },
    {
      key: "actions",
      title: "",
      align: "right",
      render: (u) => (
        <span className="flex justify-end gap-1.5">
          <Button
            disabled={u.is_owner || update.isPending}
            onClick={() => update.mutate({ id: u.id, patch: { disabled: !u.disabled } })}
            title={u.is_owner ? "the owner cannot be disabled" : undefined}
          >
            {u.disabled ? "enable" : "disable"}
          </Button>
          <Button
            variant="danger"
            disabled={u.is_owner}
            title={u.is_owner ? "the owner cannot be removed" : undefined}
            onClick={() => setRemoving(u)}
          >
            remove
          </Button>
        </span>
      ),
    },
  ];

  const inviteColumns: Array<Column<Invitation>> = [
    { key: "email", title: "email", render: (i) => i.email },
    { key: "role", title: "role", render: (i) => <span className="capitalize">{i.role}</span> },
    {
      key: "expires",
      title: "expires",
      render: (i) => <span className="text-muted">{when(i.expires_at)}</span>,
    },
    {
      key: "actions",
      title: "",
      align: "right",
      render: (i) => (
        <Button variant="danger" disabled={revoke.isPending} onClick={() => setRevoking(i)}>
          revoke
        </Button>
      ),
    },
  ];

  return (
    <>
      <PageHeader
        title="Users"
        subtitle="Accounts that can sign in to this dashboard. Gateway API keys are separate, on the Keys page."
        actions={
          <Button variant="primary" onClick={() => setInviting(true)}>
            invite someone
          </Button>
        }
      />

      <ConfirmDialog
        open={removing !== null}
        title="Remove user"
        message={<>Remove <span className="font-medium">{removing?.email}</span>? Their sessions end immediately and the account cannot sign in again.</>}
        confirmLabel="Remove user"
        busy={remove.isPending}
        onConfirm={() => {
          if (removing) remove.mutate(removing.id);
          setRemoving(null);
        }}
        onClose={() => setRemoving(null)}
      />

      <ConfirmDialog
        open={revoking !== null}
        title="Revoke invitation"
        message={<>The link sent to <span className="font-medium">{revoking?.email}</span> stops working. They can be invited again.</>}
        confirmLabel="Revoke invitation"
        busy={revoke.isPending}
        onConfirm={() => {
          if (revoking) revoke.mutate(revoking.id);
          setRevoking(null);
        }}
        onClose={() => setRevoking(null)}
      />

      {notice ? (
        <p className="mb-4 rounded-md border border-line bg-surface-2 px-3 py-2 text-small text-muted">
          {notice}
        </p>
      ) : null}

      {users.isError ? (
        <div className="mb-4">
          <ErrorNote error={users.error} onRetry={() => void users.refetch()} />
        </div>
      ) : null}

      <Card>
        <CardHeader
          title="Accounts"
          action={<span className="text-caption text-faint">{users.data?.length ?? 0} total</span>}
        />
        <DataTable
          columns={columns}
          rows={users.data}
          rowKey={(u) => u.id}
          loading={users.isLoading}
          empty="No accounts yet."
        />
      </Card>

      <div className="mt-4">
        <Card>
          <CardHeader
            title="Invitations"
            action={<span className="text-caption text-faint">single use · expire in 7 days</span>}
          />
          <DataTable
            columns={inviteColumns}
            rows={invites.data}
            rowKey={(i) => i.id}
            loading={invites.isLoading}
            empty="No pending invitations."
          />
        </Card>
      </div>

      {inviting ? (
        <InviteModal
          onClose={() => setInviting(false)}
          onCreated={(link, email) => {
            setInviting(false);
            setNotice(`Invitation for ${email} created. Send them this link: ${link}`);
          }}
        />
      ) : null}
    </>
  );
}

function InviteModal({
  onClose,
  onCreated,
}: {
  onClose: () => void;
  onCreated: (link: string, email: string) => void;
}) {
  const create = useCreateInvite();
  const [email, setEmail] = useState("");
  const [role, setRole] = useState<UserRole>("member");
  const [link, setLink] = useState<string | null>(null);
  const [failure, setFailure] = useState<unknown>(null);

  const submit = (event: FormEvent) => {
    event.preventDefault();
    setFailure(null);
    create.mutate(
      { email: email.trim(), role },
      {
        onSuccess: (result) => {
          // The dashboard is mounted under the Vite base ("/app/"); the API returns a
          // route-relative path, so the app base is added here.
          const base = import.meta.env.BASE_URL.replace(/\/$/, "");
          const url = `${window.location.origin}${base}${result.path}`;
          setLink(url);
          onCreated(url, email.trim());
        },
        onError: setFailure,
      },
    );
  };

  return (
    <Modal
      title="Invite someone"
      open
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose}>close</Button>
          <Button
            variant="primary"
            disabled={create.isPending || email.trim() === ""}
            onClick={() => {
              const form = document.getElementById("invite-form") as HTMLFormElement | null;
              form?.requestSubmit();
            }}
          >
            {create.isPending ? "creating…" : "create invitation"}
          </Button>
        </>
      }
    >
      <form id="invite-form" onSubmit={submit} className="flex flex-col gap-4">
        <Field label="email" hint="Sign-up is invite-only; this address is fixed on the invitation.">
          <Input value={email} onChange={setEmail} ariaLabel="invite email" />
        </Field>
        <Field label="role" hint="Roles can be changed later from this page.">
          <Select value={role} onChange={(v) => setRole(v as UserRole)} ariaLabel="invite role">
            {ROLES.map((item) => (
              <option key={item.id} value={item.id}>
                {item.label}
              </option>
            ))}
          </Select>
        </Field>
        {link ? (
          <div className="flex flex-col gap-1.5">
            <span className="text-caption text-muted">Share this link once:</span>
            <CopyChip value={link} />
          </div>
        ) : null}
        {failure ? <ErrorNote error={failure} /> : null}
      </form>
    </Modal>
  );
}
