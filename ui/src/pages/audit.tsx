import { useMemo, useState } from "react";
import {
  Badge,
  Button,
  Card,
  CardHeader,
  CopyButton,
  EmptyState,
  ErrorNote,
  Modal,
  PageHeader,
  SectionLabel,
  Segmented,
  Select,
} from "../components/ui";
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

/** The full instant, to the second, in the reader's own timezone. */
function fullWhen(ts: number): string {
  return new Date(ts * 1000).toLocaleString(undefined, {
    dateStyle: "full",
    timeStyle: "long",
  });
}

/** One event, untruncated: the row's cells elide, this is where the whole record sits. */
function AuditDetail({ event, onClose }: { event: AuditEvent; onClose: () => void }) {
  const detail = event.detail ?? "";
  return (
    <Modal
      title="Audit event"
      open
      onClose={onClose}
      width="md"
      description="One row of this gateway's security trail, shown in full."
      footer={
        <Button variant="primary" onClick={onClose}>
          Done
        </Button>
      }
    >
      <div className="flex flex-col gap-6">
        <dl className="grid grid-cols-[6.5rem_minmax(0,1fr)] gap-x-4 gap-y-3 text-small">
          <dt className="text-faint">when</dt>
          <dd className="text-fg tnum">{fullWhen(event.ts)}</dd>
          <dt className="text-faint">event</dt>
          <dd className="flex flex-wrap items-center gap-2">
            <Badge
              tone={
                event.kind.includes("failed")
                  ? "neg"
                  : event.kind === "sign_in"
                    ? "pos"
                    : "neutral"
              }
            >
              {event.kind.replaceAll("_", " ")}
            </Badge>
            <span className="font-mono text-caption text-muted">{event.kind}</span>
          </dd>
          <dt className="text-faint">actor</dt>
          <dd className="break-words text-fg">{event.actor ?? "—"}</dd>
          <dt className="text-faint">source</dt>
          <dd className="break-all font-mono text-fg tnum">{event.ip ?? "—"}</dd>
        </dl>

        <div className="flex flex-col gap-2">
          <div className="flex items-center justify-between gap-3">
            <SectionLabel>detail</SectionLabel>
            <CopyButton value={detail} label="Copy detail" />
          </div>
          <pre className="max-h-[50vh] overflow-auto whitespace-pre-wrap break-words rounded-[var(--radius-input)] border border-line bg-[var(--color-surface-2)] px-3 py-2.5 text-small text-fg">
            {detail || "—"}
          </pre>
        </div>
      </div>
    </Modal>
  );
}

/** Security trail: who did what, from where. Read-only and admin-only. */
export default function Audit() {
  const [category, setCategory] = useState<Category>("all");
  const [kind, setKind] = useState("");
  const [selected, setSelected] = useState<AuditEvent | null>(null);
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
      cellTitle: (event) => fullWhen(event.ts),
      render: (event) => <span className="font-mono text-small text-muted tnum">{when(event.ts)}</span>,
    },
    {
      key: "kind",
      title: "event",
      cellTitle: (event) => event.kind.replaceAll("_", " "),
      render: (event) => (
        <Badge tone={event.kind.includes("failed") ? "neg" : event.kind === "sign_in" ? "pos" : "neutral"}>
          {event.kind.replaceAll("_", " ")}
        </Badge>
      ),
    },
    {
      key: "actor",
      title: "actor",
      cellTitle: (event) => event.actor,
      render: (event) => event.actor ?? "—",
    },
    {
      key: "ip",
      title: "source",
      width: "10rem",
      cellTitle: (event) => event.ip,
      render: (event) => <span className="font-mono text-caption text-muted tnum">{event.ip ?? "—"}</span>,
    },
    {
      key: "detail",
      title: "detail",
      cellTitle: (event) => event.detail,
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
          <DataTable
            columns={columns}
            rows={rows}
            rowKey={(event) => String(event.id)}
            selectedKey={selected?.id}
            onRowClick={setSelected}
            loading={audit.isLoading}
          />
        )}
      </Card>

      {selected ? (
        <AuditDetail event={selected} onClose={() => setSelected(null)} />
      ) : null}
    </>
  );
}
