import { useState } from "react";
import { Badge, Button, Card, CardHeader, EmptyState, ErrorNote, PageHeader, Select } from "../components/ui";
import { DataTable, type Column } from "../components/table";
import type { AuditEvent } from "../lib/auth";
import { useAudit } from "../lib/queries";
import { when } from "../lib/format";

const KINDS = [
  "sign_in",
  "sign_in_failed",
  "sign_out",
  "setup",
  "invitation_created",
  "invitation_accepted",
  "password_reset",
  "password_changed",
  "password_change_failed",
  "profile_updated",
  "session_revoked",
  "sessions_revoked",
  "user_updated",
  "user_removed",
];

/** Security trail: who did what, from where. Read-only and admin-only. */
export default function Audit() {
  const [kind, setKind] = useState("");
  const audit = useAudit(kind);
  const rows = audit.data ?? [];

  const columns: Array<Column<AuditEvent>> = [
    {
      key: "ts",
      title: "when",
      width: "12rem",
      render: (event) => <span className="text-muted tnum">{when(event.ts)}</span>,
    },
    {
      key: "kind",
      title: "event",
      render: (event) => (
        <Badge tone={event.kind.includes("failed") ? "neg" : event.kind === "sign_in" ? "pos" : "neutral"}>
          {event.kind.replaceAll("_", " ")}
        </Badge>
      ),
    },
    { key: "actor", title: "actor", render: (event) => event.actor ?? "—" },
    {
      key: "ip",
      title: "source",
      width: "10rem",
      render: (event) => <span className="font-mono text-[11.5px] text-muted">{event.ip ?? "—"}</span>,
    },
    {
      key: "detail",
      title: "detail",
      render: (event) => <span className="text-muted">{event.detail ?? "—"}</span>,
    },
  ];

  return (
    <>
      <PageHeader
        title="Audit"
        subtitle="Security events for this gateway: sign-ins, invitations, role changes, resets and session revocations."
        actions={
          <Button variant="quiet" onClick={() => void audit.refetch()} disabled={audit.isFetching}>
            {audit.isFetching ? "refreshing…" : "Refresh"}
          </Button>
        }
      />

      {audit.isError ? (
        <div className="mb-4">
          <ErrorNote error={audit.error} onRetry={() => void audit.refetch()} />
        </div>
      ) : null}

      <Card>
        <CardHeader
          title="Events"
          action={
            <div className="w-[210px]">
              <Select
                value={kind}
                onChange={setKind}
                ariaLabel="event kind"
              >
                <option value="">All events</option>
                {KINDS.map((name) => (
                  <option key={name} value={name}>
                    {name.replaceAll("_", " ")}
                  </option>
                ))}
              </Select>
            </div>
          }
        />
        {rows.length === 0 && !audit.isLoading ? (
          <EmptyState>No audit events yet. Signing in or inviting someone writes the first one.</EmptyState>
        ) : (
          <DataTable columns={columns} rows={rows} rowKey={(event) => String(event.id)} loading={audit.isLoading} />
        )}
      </Card>
    </>
  );
}
