import { useMemo, useState } from "react";
import { Badge, Button, Card, CardHeader, EmptyState, ErrorNote, PageHeader, Segmented, Select } from "../components/ui";
import { DataTable, type Column } from "../components/table";
import type { AuditEvent } from "../lib/auth";
import { useAudit } from "../lib/queries";
import { when } from "../lib/format";

const CATEGORIES = [
  { id: "all", label: "All" },
  { id: "auth", label: "Auth" },
  { id: "keys", label: "Keys" },
  { id: "providers", label: "Providers" },
  { id: "routing", label: "Routing" },
] as const;

type Category = (typeof CATEGORIES)[number]["id"];

const KINDS = [
  "sign_in",
  "sign_in_failed",
  "sign_out",
  "setup",
  "invitation_created",
  "invitation_accepted",
  "key_created",
  "key_updated",
  "key_deleted",
  "team_created",
  "team_updated",
  "team_deleted",
  "team_member_added",
  "team_member_removed",
  "connection_created",
  "connection_updated",
  "connection_deleted",
  "provider_code_changed",
  "provider_created",
  "provider_deleted",
  "preset_created",
  "preset_updated",
  "preset_deleted",
  "preset_activated",
  "preset_layout_saved",
  "group_access_saved",
  "preset_default_set",
  "settings_updated",
  "password_reset",
  "password_changed",
  "password_change_failed",
  "profile_updated",
  "session_revoked",
  "sessions_revoked",
  "user_updated",
  "user_removed",
];

function categoryOf(kind: string): Exclude<Category, "all"> {
  if (kind.startsWith("key_")) return "keys";
  if (kind.startsWith("connection_") || kind.startsWith("provider_")) return "providers";
  if (
    kind.startsWith("preset_") ||
    kind.startsWith("group_") ||
    kind.startsWith("alias_") ||
    kind.startsWith("combo_") ||
    kind.startsWith("model_")
  ) {
    return "routing";
  }
  return "auth";
}

/** Security trail: who did what, from where. Read-only and admin-only. */
export default function Audit() {
  const [category, setCategory] = useState<Category>("all");
  const [kind, setKind] = useState("");
  const audit = useAudit(kind);
  const rawRows = audit.data ?? [];

  const filteredKinds = useMemo(
    () => (category === "all" ? KINDS : KINDS.filter((k) => categoryOf(k) === category)),
    [category],
  );
  const rows = useMemo(
    () =>
      category === "all"
        ? rawRows
        : rawRows.filter((event) => categoryOf(event.kind) === category),
    [rawRows, category],
  );

  const onCategoryChange = (next: Category) => {
    setCategory(next);
    if (next !== "all" && kind !== "" && categoryOf(kind) !== next) {
      setKind("");
    }
  };

  const dirty = category !== "all" || kind !== "";

  const columns: Array<Column<AuditEvent>> = [
    {
      key: "ts",
      title: "when",
      width: "12rem",
      render: (event) => <span className="font-mono text-small text-muted tnum">{when(event.ts)}</span>,
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
      render: (event) => <span className="font-mono text-caption text-muted tnum">{event.ip ?? "—"}</span>,
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
        subtitle="Security events for this gateway: sign-ins, invitations, role changes, API key changes, provider updates, routing presets, and session revocations."
        actions={
          <Button variant="quiet" onClick={() => void audit.refetch()} disabled={audit.isFetching}>
            {audit.isFetching ? "Refreshing…" : "Refresh"}
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
            <div className="flex flex-wrap items-center gap-2">
              <Segmented
                options={CATEGORIES}
                value={category}
                onChange={onCategoryChange}
                ariaLabel="event category"
              />
              <div className="w-[200px]">
                <Select
                  value={kind}
                  onChange={setKind}
                  ariaLabel="event kind"
                >
                  <option value="">All events</option>
                  {filteredKinds.map((name) => (
                    <option key={name} value={name}>
                      {name.replaceAll("_", " ")}
                    </option>
                  ))}
                </Select>
              </div>
              {dirty ? (
                <Button
                  variant="quiet"
                  onClick={() => {
                    setCategory("all");
                    setKind("");
                  }}
                >
                  Clear filters
                </Button>
              ) : null}
            </div>
          }
        />
        {rows.length === 0 && !audit.isLoading ? (
          <EmptyState>
            {dirty ? (
              <>
                <div>No audit events match the active filters.</div>
                <div className="mt-3">
                  <Button
                    variant="quiet"
                    onClick={() => {
                      setCategory("all");
                      setKind("");
                    }}
                  >
                    Clear filters
                  </Button>
                </div>
              </>
            ) : (
              "No audit events yet. Signing in or inviting someone writes the first one."
            )}
          </EmptyState>
        ) : (
          <DataTable columns={columns} rows={rows} rowKey={(event) => String(event.id)} loading={audit.isLoading} />
        )}
      </Card>
    </>
  );
}
