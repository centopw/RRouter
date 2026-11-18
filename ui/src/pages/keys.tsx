import { useCallback, useMemo, useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { Key as KeyIcon, MagnifyingGlass, Trash } from "@phosphor-icons/react";
import {
  Badge,
  Button,
  Card,
  ConfirmDialog,
  ErrorNote,
  Input,
  PageHeader,
  Segmented,
} from "../components/ui";
import { IconButton } from "../components/atoms";
import { DataTable, type Column } from "../components/table";
import { ConnectionCard } from "../components/connection-card";
import { CreateKeyModal } from "../components/create-key-modal";
import { usd } from "../lib/format";
import { qk, useKeys, useSystem, useTeams } from "../lib/queries";
import { api, type KeyResult } from "../lib/api";
import { KeySettingsDrawer, PlaygroundKeyModal } from "./keys/key-settings-drawer";
import { KeyCell, LastUsedCell, LimitsCell, SpendCell, StatusCell, TeamCell, modelScopeLabel, statusOf } from "./keys/spend-cell";

export default function Keys() {
  const keys = useKeys();
  const system = useSystem();
  // Team names for the badge; the picker inside the drawer reads the same list.
  const teams = useTeams();
  const teamName = useCallback(
    (id: string | null | undefined) =>
      id === null || id === undefined
        ? null
        : (teams.data?.find((team) => team.id === id)?.name ?? id.slice(0, 8)),
    [teams.data],
  );
  const client = useQueryClient();
  const [editing, setEditing] = useState<KeyResult | null>(null);
  const [creating, setCreating] = useState(false);
  const [revoking, setRevoking] = useState<KeyResult | null>(null);
  const [revealing, setRevealing] = useState(false);
  const [query, setQuery] = useState("");
  const [statusFilter, setStatusFilter] = useState<"all" | "active" | "disabled">("all");

  // The playground key is the gateway's own: it is listed here, but not an operator key.
  const allRows = useMemo(() => keys.data ?? [], [keys.data]);
  const visibleRows = useMemo(() => {
    const needle = query.trim().toLowerCase();
    return allRows.filter((row) => {
      const status = statusOf(row).label;
      if (statusFilter === "active" && status !== "active") return false;
      if (statusFilter === "disabled" && status !== "disabled") return false;
      if (!needle) return true;
      const haystack = [
        row.name ?? "",
        row.id,
        row.kind,
        row.scope,
        teamName(row.team_id) ?? "",
        modelScopeLabel(row),
      ]
        .join(" ")
        .toLowerCase();
      return haystack.includes(needle);
    });
  }, [allRows, query, statusFilter, teamName]);

  const configured = allRows.filter((row) => row.kind !== "playground").length;
  const activeCount = allRows.filter((row) => statusOf(row).label === "active").length;
  const totalSpent = allRows.reduce((sum, row) => sum + row.spent_micros, 0);
  const filtering = query.trim().length > 0 || statusFilter !== "all";
  const clearFilters = () => {
    setQuery("");
    setStatusFilter("all");
  };

  // Modal re-focuses itself whenever `onClose` changes identity; keep it stable.
  const closeEdit = useCallback(() => setEditing(null), []);

  const remove = useMutation({
    mutationFn: (id: string) => api.del<{ deleted: boolean }>(`/api/keys/${id}`),
    onSuccess: () => {
      void client.invalidateQueries({ queryKey: qk.keys });
      void client.invalidateQueries({ queryKey: qk.system });
    },
  });

  const actionsFor = (r: KeyResult) => (
    <span className="flex justify-end gap-0.5">
      {r.kind === "playground" ? (
        <IconButton label="Show the playground key" onClick={() => setRevealing(true)}>
          <KeyIcon size={15} aria-hidden />
        </IconButton>
      ) : null}
      <IconButton
        label={`Delete key ${r.name ?? r.id}`}
        onClick={() => setRevoking(r)}
        className="hover:bg-[var(--color-neg-tint)] hover:text-[var(--color-neg-ink)]"
      >
        <Trash size={15} aria-hidden />
      </IconButton>
    </span>
  );

  const columns: Array<Column<KeyResult>> = [
    {
      key: "key",
      title: "Key",
      width: "34%",
      className: "min-w-0 py-3",
      render: (r) => <KeyCell row={r} team={teamName(r.team_id)} onOpen={() => setEditing(r)} />,
    },
    {
      key: "team",
      title: "Team",
      width: "12%",
      className: "min-w-0 py-3",
      render: (r) => <TeamCell team={teamName(r.team_id)} />,
    },
    {
      key: "status",
      title: "Status",
      width: "8rem",
      className: "py-3",
      render: (r) => <StatusCell row={r} />,
    },
    {
      key: "last_used",
      title: "Last used",
      width: "8.5rem",
      className: "py-3",
      render: (r) => <LastUsedCell lastUsedAt={r.last_used_at} />,
    },
    {
      key: "limits",
      title: "Access & limits",
      className: "min-w-0 py-3",
      render: (r) => <LimitsCell row={r} />,
    },
    {
      key: "spend",
      title: "Spend",
      align: "right",
      width: "9rem",
      className: "hidden md:table-cell py-3",
      render: (r) => <SpendCell row={r} />,
    },
    {
      key: "actions",
      title: "Actions",
      srOnlyTitle: true,
      align: "right",
      width: "4.5rem",
      className: "py-3",
      render: (r) => actionsFor(r),
    },
  ];

  const emptyState = (
    <div className="flex flex-col items-center gap-2">
      <span>
        {query.trim()
          ? `No keys match “${query.trim()}”.`
          : statusFilter === "active"
            ? "No active keys."
            : statusFilter === "disabled"
              ? "No disabled keys."
              : "No keys yet. Create one above to require authentication on /v1."}
      </span>
      {filtering ? (
        <Button variant="quiet" onClick={clearFilters}>
          Clear filters
        </Button>
      ) : null}
    </div>
  );

  return (
    <>
      <ConfirmDialog
        open={revoking !== null}
        title="Revoke key"
        message={
          <>
            Every client using <span className="font-mono">{revoking?.name ?? revoking?.id}</span>{" "}
            stops working immediately. This cannot be undone.
          </>
        }
        confirmLabel="Revoke key"
        busy={remove.isPending}
        error={remove.error}
        onConfirm={() => {
          if (revoking) {
            remove.mutate(revoking.id, { onSuccess: () => setRevoking(null) });
          }
        }}
        onClose={() => {
          if (!remove.isPending) {
            remove.reset();
            setRevoking(null);
          }
        }}
      />

      <PageHeader
        title="API keys"
        subtitle="Clients authenticate with these against /v1. Keys are stored hashed; the plaintext is shown once."
        actions={
          <Button variant="primary" onClick={() => setCreating(true)}>
            Create key
          </Button>
        }
      />

      <ConnectionCard />

      {keys.isError ? (
        <div className="mb-4">
          <ErrorNote error={keys.error} onRetry={() => void keys.refetch()} />
        </div>
      ) : null}

      <Card>
        <div className="flex flex-wrap items-start justify-between gap-4 border-b border-line px-5 py-4">
          <div className="min-w-0">
            <div className="flex flex-wrap items-baseline gap-2">
              <h2 className="text-lead font-semibold">Keys</h2>
              <span className="text-caption text-faint tnum">
                {filtering ? `${visibleRows.length} of ${allRows.length}` : `${allRows.length} total`}
              </span>
            </div>
            <div className="mt-1 flex flex-wrap items-center gap-x-4 gap-y-1 text-small text-muted">
              <span>
                <span className="font-medium text-fg tnum">{activeCount}</span> active
              </span>
              <span>
                <span className="font-medium text-fg tnum">{usd(totalSpent)}</span> spent
              </span>
              <span>
                <span className="font-medium text-fg tnum">{configured}</span> operator{" "}
                {configured === 1 ? "key" : "keys"}
              </span>
            </div>
          </div>
          <div className="flex flex-wrap items-center gap-2">
            <div className="relative w-[210px] max-[640px]:w-full">
              <MagnifyingGlass
                size={14}
                aria-hidden
                className="pointer-events-none absolute left-2.5 top-1/2 -translate-y-1/2 text-faint"
              />
              <Input
                value={query}
                onChange={setQuery}
                placeholder="Search keys"
                ariaLabel="Search keys"
                className="pl-8"
              />
            </div>
            <Segmented
              options={[
                { id: "all", label: "All" },
                { id: "active", label: "Active" },
                { id: "disabled", label: "Disabled" },
              ]}
              value={statusFilter}
              onChange={setStatusFilter}
              ariaLabel="Filter keys by status"
            />
          </div>
        </div>
        <div className="hidden lg:block">
          <DataTable
            columns={columns}
            rows={visibleRows}
            rowKey={(r) => r.id}
            loading={keys.isLoading}
            empty={emptyState}
            tableLayout="fixed"
            rowHeaderKey="key"
            stickyRight="actions"
            scrollLabel="API keys"
          />
        </div>
        <div className="lg:hidden">
          {keys.isLoading && visibleRows.length === 0 ? (
            <div className="px-5 py-10 text-center text-body text-faint">loading…</div>
          ) : visibleRows.length === 0 ? (
            <div className="px-5 py-10 text-center text-body text-faint">{emptyState}</div>
          ) : (
            <div className="divide-y divide-[var(--color-rule)]">
              {visibleRows.map((row) => (
                <article key={row.id} className="px-4 py-4">
                  <div className="flex items-start justify-between gap-3">
                    <div className="min-w-0 flex-1">
                      <KeyCell
                        row={row}
                        team={teamName(row.team_id)}
                        onOpen={() => setEditing(row)}
                      />
                    </div>
                    {actionsFor(row)}
                  </div>
                  <div className="mt-3 grid grid-cols-[minmax(0,1fr)_auto] items-start gap-3 border-t border-line pt-3">
                    <div className="flex min-w-0 flex-col gap-1.5">
                      <div className="flex min-w-0 items-center gap-2">
                        <TeamCell team={teamName(row.team_id)} />
                        <span aria-hidden className="text-faint">·</span>
                        <StatusCell row={row} />
                      </div>
                      <LimitsCell row={row} />
                    </div>
                    <SpendCell row={row} />
                  </div>
                </article>
              ))}
            </div>
          )}
        </div>
        <div className="flex flex-wrap items-center gap-2 border-t border-line px-5 py-3 text-caption text-faint">
          <span className="ml-auto flex items-center gap-1.5">
            {system.data?.require_api_key ? (
              <>
                clients must present a key · <Badge>RROUTER_REQUIRE_API_KEY=true</Badge>
              </>
            ) : (
              <>
                enforcement is off · <Badge>RROUTER_REQUIRE_API_KEY=false</Badge>
              </>
            )}
          </span>
        </div>
      </Card>

      {creating ? (
        <CreateKeyModal
          onClose={() => setCreating(false)}
          onCreated={() => {
            setCreating(false);
            void client.invalidateQueries({ queryKey: qk.keys });
            void client.invalidateQueries({ queryKey: qk.system });
          }}
        />
      ) : null}

      {editing ? (
        <KeySettingsDrawer key={editing.id} row={editing} onClose={closeEdit} />
      ) : null}

      {revealing ? <PlaygroundKeyModal onClose={() => setRevealing(false)} /> : null}
    </>
  );
}
