import { useEffect, useState } from "react";
import {
  Badge,
  Button,
  Card,
  CardHeader,
  ConfirmDialog,
  EmptyState,
  ErrorNote,
  Field,
  Input,
  Modal,
  PageHeader,
  Select,
  cx,
} from "../components/ui";
import { DataTable, type Column } from "../components/table";
import { compact, parseUsd, usd, usdInput, when } from "../lib/format";
import {
  useAddTeamMember,
  useAuthState,
  useCreateTeam,
  useDeleteTeam,
  useIsAdmin,
  useRemoveTeamMember,
  useTeamKeys,
  useTeamMembers,
  useTeams,
  useUpdateTeam,
} from "../lib/queries";
import type { BudgetWindow, KeyResult, Team, TeamMember } from "../lib/api";

/**
 * Teams: the tenancy unit above a gateway key.
 *
 * An admin sees every team, changes its budget, and manages every roster. A `team_admin`
 * manages membership of their own team and may read that team's key rows — nothing else:
 * team lifecycle, budgets and the admin pages stay closed. A member or viewer sees the
 * team read-only. The server scopes every list, so this page only decides what to paint.
 */

const ROLES: ReadonlyArray<{ id: string; label: string }> = [
  { id: "team_admin", label: "Team admin" },
  { id: "member", label: "Member" },
  { id: "viewer", label: "Viewer" },
];

export default function Teams() {
  const isAdmin = useIsAdmin();
  const teams = useTeams();
  const remove = useDeleteTeam();
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);
  const [renaming, setRenaming] = useState<Team | null>(null);
  const [deleting, setDeleting] = useState<Team | null>(null);

  const rows = teams.data ?? [];
  const current = rows.find((team) => team.id === selectedId) ?? rows[0] ?? null;

  const columns: Array<Column<Team>> = [
    {
      key: "name",
      title: "Team",
      render: (team) => (
        <button
          type="button"
          onClick={() => setSelectedId(team.id)}
          className={cx(
            "text-left font-medium hover:underline",
            current?.id === team.id ? "text-accent-text" : "text-fg",
          )}
        >
          {team.name}
        </button>
      ),
    },
    {
      key: "members",
      title: "Members",
      width: "7rem",
      render: (team) => <span className="text-muted tnum">{team.members}</span>,
    },
    {
      key: "budget",
      title: "Budget",
      render: (team) => <BudgetCell team={team} />,
    },
    {
      key: "spend",
      title: "Spend",
      render: (team) => (
        <span className="whitespace-nowrap text-small">
          <span className="text-muted tnum">{compact(team.spend.tokens)} tokens</span>
          <span className="text-faint"> · </span>
          <span className="text-muted tnum">{usd(team.spend.cost_micros)}</span>
          <span className="text-faint"> · </span>
          <span className="text-faint tnum">{team.spend.requests} req</span>
        </span>
      ),
    },
    ...(isAdmin
      ? [
          {
            key: "actions",
            title: "",
            align: "right" as const,
            render: (team: Team) => (
              <span className="flex items-center justify-end gap-1.5">
                <Button variant="quiet" onClick={() => setRenaming(team)}>
                  rename
                </Button>
                <Button
                  variant="danger"
                  onClick={() => {
                    remove.reset();
                    setDeleting(team);
                  }}
                >
                  delete
                </Button>
              </span>
            ),
          },
        ]
      : []),
  ];

  return (
    <>
      <ConfirmDialog
        open={deleting !== null}
        title="Delete team"
        message={
          <>
            Delete <span className="font-medium">{deleting?.name}</span>? Its keys are
            released to instance-wide rather than deleted, and usage already recorded stays
            attributed to this team in the request log.
          </>
        }
        confirmLabel="Delete team"
        busy={remove.isPending}
        error={remove.error}
        onConfirm={() => {
          if (deleting) {
            remove.mutate(deleting.id, { onSuccess: () => setDeleting(null) });
          }
        }}
        onClose={() => {
          if (!remove.isPending) {
            remove.reset();
            setDeleting(null);
          }
        }}
      />

      {creating ? <CreateTeamModal onClose={() => setCreating(false)} /> : null}
      {renaming ? (
        <RenameTeamModal team={renaming} onClose={() => setRenaming(null)} />
      ) : null}

      <PageHeader
        title="Teams"
        subtitle={
          isAdmin
            ? "Group keys under a team, cap its spend and decide who can see it."
            : "The teams you belong to, with their budgets and spend."
        }
        actions={
          isAdmin ? (
            <Button variant="primary" onClick={() => setCreating(true)}>
              New team
            </Button>
          ) : undefined
        }
      />

      {teams.isError ? (
        <div className="mb-4">
          <ErrorNote error={teams.error} onRetry={() => void teams.refetch()} />
        </div>
      ) : null}

      <Card className="mb-6">
        <CardHeader
          title="All teams"
          action={
            <span className="text-caption text-faint tnum">
              {rows.length} {rows.length === 1 ? "team" : "teams"}
            </span>
          }
        />
        {rows.length === 0 && !teams.isLoading ? (
          <EmptyState>
            {isAdmin
              ? "No teams yet. Create one, then assign keys to it from the API Keys page."
              : "You are not a member of any team. Ask an administrator to add you to one."}
          </EmptyState>
        ) : (
          <DataTable
            columns={columns}
            rows={rows}
            rowKey={(team) => team.id}
            loading={teams.isLoading}
          />
        )}
      </Card>

      {current ? (
        <TeamDetail key={current.id} team={current} isAdmin={isAdmin} />
      ) : null}
    </>
  );
}

function BudgetCell({ team }: { team: Team }) {
  const unlimited = team.budget_tokens === 0 && team.budget_micros === 0;
  if (unlimited) {
    return <span className="text-faint">unlimited</span>;
  }
  return (
    <span className="whitespace-nowrap text-small">
      {team.budget_tokens > 0 ? (
        <span className="text-muted tnum">{compact(team.budget_tokens)} tokens</span>
      ) : null}
      {team.budget_tokens > 0 && team.budget_micros > 0 ? (
        <span className="text-faint"> · </span>
      ) : null}
      {team.budget_micros > 0 ? (
        <span className="text-muted tnum">{usd(team.budget_micros)}</span>
      ) : null}
      {team.budget_window !== "total" ? (
        <>
          <span className="text-faint"> · </span>
          <span className="text-muted">{team.budget_window}</span>
        </>
      ) : null}
    </span>
  );
}

/** One team's budget editor and roster. */
function TeamDetail({
  team,
  isAdmin,
}: {
  team: Team;
  isAdmin: boolean;
}) {
  const members = useTeamMembers(team.id);
  const me = useAuthState().data?.user;
  const update = useUpdateTeam();
  const add = useAddTeamMember();
  const removeMember = useRemoveTeamMember();

  // A team admin is delegated by the roster, not by the global role: the same row that
  // lists them here is what the server checks on every membership call.
  const myRole = members.data?.find((member) => member.user_id === me?.id)?.role;
  const canManageMembers = isAdmin || myRole === "team_admin";
  const keys = useTeamKeys(team.id, canManageMembers);

  const [tokens, setTokens] = useState(team.budget_tokens > 0 ? String(team.budget_tokens) : "");
  const [budget, setBudget] = useState(usdInput(team.budget_micros));
  const [window, setWindow] = useState<BudgetWindow>(team.budget_window);
  const [problem, setProblem] = useState<string | null>(null);
  const [failure, setFailure] = useState<unknown>(null);
  const [email, setEmail] = useState("");
  const [role, setRole] = useState("member");

  // A different team is a different form: the key prop remounts, but the values above are
  // seeded from the row, so keep them in step if the row itself changes after a save.
  useEffect(() => {
    setTokens(team.budget_tokens > 0 ? String(team.budget_tokens) : "");
    setBudget(usdInput(team.budget_micros));
    setWindow(team.budget_window);
  }, [team.id, team.budget_tokens, team.budget_micros, team.budget_window]);

  const save = () => {
    const parsed = parseUsd(budget);
    if ("problem" in parsed) {
      setProblem(parsed.problem);
      return;
    }
    setProblem(null);
    setFailure(null);
    update.mutate(
      {
        id: team.id,
        patch: {
          budget_tokens: Number(tokens.replace(/[^0-9]/g, "")) || 0,
          budget_micros: parsed.micros,
          budget_window: window,
        },
      },
      { onError: (error) => setFailure(error) },
    );
  };

  const addMember = () => {
    const address = email.trim();
    if (address === "") return;
    setFailure(null);
    add.mutate(
      { id: team.id, email: address, role },
      {
        onSuccess: () => setEmail(""),
        onError: (error) => setFailure(error),
      },
    );
  };

  const memberColumns: Array<Column<TeamMember>> = [
    {
      key: "who",
      title: "Member",
      render: (member) => (
        <div className="min-w-0">
          <div className="truncate font-medium text-fg">{member.name ?? "—"}</div>
          <div className="truncate text-caption text-faint">{member.email ?? member.user_id}</div>
        </div>
      ),
    },
    {
      key: "role",
      title: "Role",
      width: "8rem",
      render: (member) => (
        <Badge tone={member.role === "team_admin" ? "accent" : "neutral"}>
          {member.role.replaceAll("_", " ")}
        </Badge>
      ),
    },
    {
      key: "since",
      title: "Since",
      width: "9rem",
      render: (member) => (
        <span className="whitespace-nowrap text-muted">{when(member.created_at)}</span>
      ),
    },
    ...(canManageMembers
      ? [
          {
            key: "actions",
            title: "",
            align: "right" as const,
            render: (member: TeamMember) =>
              member.user_id === me?.id ? (
                <span className="text-caption text-faint" title="a team admin cannot change their own membership">
                  you
                </span>
              ) : (
                <Button
                  variant="quiet"
                  disabled={removeMember.isPending}
                  onClick={() => removeMember.mutate({ id: team.id, userId: member.user_id })}
                >
                  remove
                </Button>
              ),
          },
        ]
      : []),
  ];

  const keyColumns: Array<Column<KeyResult>> = [
    {
      key: "name",
      title: "Key",
      render: (key) => (
        <span className="font-medium">
          {key.name ?? <span className="text-faint">unnamed</span>}
        </span>
      ),
    },
    {
      key: "scope",
      title: "Scope",
      width: "7rem",
      render: (key) => (
        <Badge tone={key.scope === "admin" ? "accent" : "neutral"}>{key.scope}</Badge>
      ),
    },
    {
      key: "limits",
      title: "Limits",
      render: (key) => {
        const parts = [
          key.rpm > 0 ? `${key.rpm} rpm` : null,
          key.tpm > 0 ? `${key.tpm} tpm` : null,
          key.budget_tokens > 0 ? `${compact(key.budget_tokens)} tokens` : null,
          key.budget_micros > 0 ? usd(key.budget_micros) : null,
        ].filter((part): part is string => part !== null);
        return (
          <span className="text-small text-muted">
            {parts.length > 0 ? parts.join(" · ") : <span className="text-faint">unlimited</span>}
          </span>
        );
      },
    },
    {
      key: "spend",
      title: "Spend",
      render: (key) => (
        <span className="whitespace-nowrap text-small text-muted tnum">
          {compact(key.spent_tokens)} tokens · {usd(key.spent_micros)}
        </span>
      ),
    },
  ];

  return (
    <div className="flex flex-col gap-6">
      {failure ? <ErrorNote error={failure} /> : null}

      <Card>
        <CardHeader
          title={`${team.name} · budget`}
          action={
            <span className="text-caption text-faint">
              resets {team.budget_window === "total" ? "never" : team.budget_window}
            </span>
          }
        />
        <div className="grid gap-3 px-6 pb-5 sm:grid-cols-3">
          <Field label="token budget" hint="0 = unlimited">
            <Input
              type="number"
              value={tokens}
              onChange={setTokens}
              ariaLabel="team token budget"
              disabled={!isAdmin}
            />
          </Field>
          <Field label="spend budget (USD)" hint="0 = unlimited">
            <Input
              value={budget}
              onChange={setBudget}
              ariaLabel="team spend budget"
              disabled={!isAdmin}
            />
          </Field>
          <Field label="window">
            <Select
              value={window}
              onChange={(v) => setWindow(v as BudgetWindow)}
              ariaLabel="team budget window"
              disabled={!isAdmin}
            >
              <option value="total">total (never)</option>
              <option value="daily">daily</option>
              <option value="weekly">weekly</option>
              <option value="monthly">monthly</option>
            </Select>
          </Field>
          {problem ? (
            <span role="alert" className="text-caption text-neg sm:col-span-3">
              {problem}
            </span>
          ) : null}
        </div>
        {isAdmin ? (
          <div className="flex items-center justify-between gap-3 border-t border-line px-6 py-3">
            <span className="text-caption text-faint">
              {team.spend.requests} requests · {compact(team.spend.tokens)} tokens ·{" "}
              {usd(team.spend.cost_micros)} this window
            </span>
            <Button variant="primary" onClick={save} disabled={update.isPending}>
              {update.isPending ? "saving…" : "save budget"}
            </Button>
          </div>
        ) : (
          <div className="border-t border-line px-6 py-3 text-caption text-faint">
            {team.spend.requests} requests · {compact(team.spend.tokens)} tokens ·{" "}
            {usd(team.spend.cost_micros)} this window
          </div>
        )}
      </Card>

      <Card>
        <CardHeader
          title="Members"
          action={
            <span className="text-caption text-faint tnum">
              {members.data?.length ?? team.members}
            </span>
          }
        />
        {members.isError ? (
          <div className="px-6 pb-5">
            <ErrorNote error={members.error} onRetry={() => void members.refetch()} />
          </div>
        ) : (
          <DataTable
            columns={memberColumns}
            rows={members.data ?? []}
            rowKey={(member) => member.user_id}
            loading={members.isLoading}
            empty="No members yet."
          />
        )}
        {canManageMembers ? (
          <div className="flex flex-wrap items-end gap-3 border-t border-line px-6 py-4">
            <div className="min-w-[220px] flex-1">
              <Field label="email" hint="they must already have a dashboard account">
                <Input
                  value={email}
                  onChange={setEmail}
                  placeholder="teammate@example.com"
                  ariaLabel="member email"
                />
              </Field>
            </div>
            <div className="w-[148px]">
              <Field label="role">
                <Select value={role} onChange={setRole} ariaLabel="member role">
                  {ROLES.map((option) => (
                    <option key={option.id} value={option.id}>
                      {option.label}
                    </option>
                  ))}
                </Select>
              </Field>
            </div>
            <Button
              variant="primary"
              onClick={addMember}
              disabled={add.isPending || email.trim() === ""}
            >
              {add.isPending ? "adding…" : "add member"}
            </Button>
          </div>
        ) : null}
      </Card>

      {canManageMembers ? (
        <Card>
          <CardHeader
            title="Keys"
            action={
              <span className="text-caption text-faint">
                read-only · an administrator creates and edits keys
              </span>
            }
          />
          {keys.isError ? (
            <div className="px-6 pb-5">
              <ErrorNote error={keys.error} onRetry={() => void keys.refetch()} />
            </div>
          ) : (
            <DataTable
              columns={keyColumns}
              rows={keys.data ?? []}
              rowKey={(key) => key.id}
              loading={keys.isLoading}
              empty="No keys are assigned to this team yet."
            />
          )}
        </Card>
      ) : null}
    </div>
  );
}

function CreateTeamModal({ onClose }: { onClose: () => void }) {
  const create = useCreateTeam();
  const [name, setName] = useState("");
  const [tokens, setTokens] = useState("");
  const [budget, setBudget] = useState("");
  const [window, setWindow] = useState<BudgetWindow>("total");
  const [problem, setProblem] = useState<string | null>(null);

  const submit = () => {
    const trimmed = name.trim();
    if (trimmed === "") {
      setProblem("A name is required.");
      return;
    }
    const parsed = parseUsd(budget);
    if ("problem" in parsed) {
      setProblem(parsed.problem);
      return;
    }
    setProblem(null);
    create.mutate(
      {
        name: trimmed,
        budget_tokens: Number(tokens.replace(/[^0-9]/g, "")) || 0,
        budget_micros: parsed.micros,
        budget_window: window,
      },
      { onSuccess: onClose },
    );
  };

  return (
    <Modal
      title="New team"
      open
      onClose={onClose}
      description="A team owns keys and a budget. Limits can be changed later."
      footer={
        <>
          <Button onClick={onClose} disabled={create.isPending}>
            cancel
          </Button>
          <Button variant="primary" onClick={submit} disabled={create.isPending}>
            {create.isPending ? "creating…" : "create team"}
          </Button>
        </>
      }
    >
      <div className="flex flex-col gap-4">
        <Field label="name" hint="unique across this gateway">
          <Input value={name} onChange={setName} placeholder="core-platform" ariaLabel="team name" />
        </Field>
        <div className="grid grid-cols-2 gap-3">
          <Field label="token budget" hint="empty = unlimited">
            <Input type="number" value={tokens} onChange={setTokens} ariaLabel="team token budget" />
          </Field>
          <Field label="spend budget (USD)" hint="empty = unlimited">
            <Input value={budget} onChange={setBudget} ariaLabel="team spend budget" />
          </Field>
        </div>
        <Field label="window" hint="when the counters reset">
          <Select
            value={window}
            onChange={(v) => setWindow(v as BudgetWindow)}
            ariaLabel="team budget window"
          >
            <option value="total">total (never)</option>
            <option value="daily">daily</option>
            <option value="weekly">weekly</option>
            <option value="monthly">monthly</option>
          </Select>
        </Field>
        {problem ? (
          <span role="alert" className="text-caption text-neg">
            {problem}
          </span>
        ) : null}
        {create.isError ? <ErrorNote error={create.error} /> : null}
      </div>
    </Modal>
  );
}

function RenameTeamModal({ team, onClose }: { team: Team; onClose: () => void }) {
  const update = useUpdateTeam();
  const [name, setName] = useState(team.name);
  const submit = () => {
    const trimmed = name.trim();
    if (trimmed === "" || trimmed === team.name) return;
    update.mutate({ id: team.id, patch: { name: trimmed } }, { onSuccess: onClose });
  };
  return (
    <Modal
      title="Rename team"
      open
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose} disabled={update.isPending}>
            cancel
          </Button>
          <Button
            variant="primary"
            onClick={submit}
            disabled={update.isPending || name.trim() === "" || name.trim() === team.name}
          >
            {update.isPending ? "saving…" : "rename"}
          </Button>
        </>
      }
    >
      <div className="flex flex-col gap-4">
        <Field label="name" hint="unique across this gateway">
          <Input value={name} onChange={setName} ariaLabel="team name" />
        </Field>
        {update.isError ? <ErrorNote error={update.error} /> : null}
      </div>
    </Modal>
  );
}
