import { useEffect, useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import {
  Badge,
  Button,
  CopyIconButton,
  ErrorNote,
  Field,
  Input,
  Modal,
} from "../../components/ui";
import { Checkbox, Toggle } from "../../components/atoms";
import { useIsAdmin, useKeys, useTeams, useUsers } from "../../lib/queries";
import { setGroupAccess } from "../../lib/api";
import type { DraftEntry, DraftGroup } from "../../lib/layout-draft";

/** A section title above one group's table; the controls live in the group manager. */
export function GroupHeading({
  group,
  count,
}: {
  group: DraftGroup & { implicit?: boolean };
  count: number;
}) {
  return (
    <div className="flex flex-wrap items-baseline justify-between gap-2 border-b border-line bg-surface-2 px-4 py-2.5">
      <div className="flex min-w-0 items-baseline gap-2">
        <h3 className="truncate text-small font-semibold text-fg">{group.name}</h3>
        <CopyIconButton value={group.name} label={`Copy group name ${group.name}`} />
        <span className="shrink-0 text-caption text-faint">
          {group.provider ? `provider · ${group.provider}` : "custom group"}
        </span>
        {group.visible === false ? (
          <Badge tone="warn" title="This group and its models are not listed by /v1/models">
            hidden from /v1/models
          </Badge>
        ) : null}
      </div>
      <span className="text-caption text-faint tnum">{count.toLocaleString()} models</span>
    </div>
  );
}
/** One group row in the manager; local text state keeps every keystroke cheap. */
export function GroupNameField({ name, onCommit }: { name: string; onCommit: (name: string) => void }) {
  const [value, setValue] = useState(name);
  useEffect(() => setValue(name), [name]);
  return (
    <Input
      value={value}
      ariaLabel={`Rename group ${name}`}
      onChange={setValue}
      onBlur={() => {
        if (value.trim() && value.trim() !== name) onCommit(value.trim());
        else setValue(name);
      }}
      onKeyDown={(event) => {
        if (event.key === "Enter") event.currentTarget.blur();
        if (event.key === "Escape") {
          setValue(name);
          event.currentTarget.blur();
        }
      }}
    />
  );
}

export type GroupCandidate = {
  ref: string;
  label: string;
  provider: string;
  kind: "model" | "alias" | "combo";
};
/** Create, rename, reorder and remove the groups of the active preset. */
export function GroupManager({
  open,
  groups,
  onClose,
  onCreate,
  onRename,
  onReorder,
  onDelete,
  entries,
  candidates,
  onAssign,
  onSetVisible,
  isDefault,
}: {
  open: boolean;
  groups: Array<DraftGroup & { implicit?: boolean }>;
  onClose: () => void;
  onCreate: (name: string) => void;
  onRename: (id: string, name: string) => void;
  onReorder: (from: string, to: string) => void;
  onDelete: (id: string) => void;
  entries: DraftEntry[];
  candidates: GroupCandidate[];
  onAssign: (ref: string, group: string | null) => void;
  onSetVisible: (id: string, visible: boolean) => void;
  isDefault: boolean;
}) {
  const [name, setName] = useState("");
  const [openModels, setOpenModels] = useState<string | null>(null);
  const [filter, setFilter] = useState("");
  const [accessFor, setAccessFor] = useState<DraftGroup | null>(null);
  useEffect(() => {
    if (open) {
      setName("");
      setOpenModels(null);
      setFilter("");
    }
  }, [open]);
  return (
    <Modal
      title="model groups"
      description="Groups change this preset's board and the default preset's /v1/models discovery. Routing is unchanged. Provider buckets start with every unassigned model of that provider."
      open={open}
      onClose={onClose}
      width="lg"
      footer={<Button onClick={onClose}>done</Button>}
    >
      <ul className="flex flex-col gap-2">
        {groups.map((group, index) => (
          <li
            key={group.id}
            className="flex flex-wrap items-center gap-2 rounded-[var(--radius-card)] border border-line bg-surface-2 p-2"
          >
            <span className="min-w-0 flex-1">
              <GroupNameField name={group.name} onCommit={(next) => onRename(group.id, next)} />
            </span>
            <Badge tone={group.provider ? "neutral" : "accent"}>
              {group.provider ? "provider" : "custom"}
            </Badge>
            <Button
              variant="quiet"
              ariaLabel={`Access for ${group.name}`}
              disabled={!isDefault}
              title={
                isDefault
                  ? "Team, key and user access for this group"
                  : "Access is stored on the instance default preset"
              }
              onClick={() => setAccessFor(group)}
            >
              {group.access ? "restricted" : "access"}
            </Button>
            <span className="flex items-center gap-1.5 text-caption text-faint">
              <Toggle
                size="sm"
                checked={group.visible}
                label={`Show ${group.name} in /v1/models`}
                onChange={(next) => onSetVisible(group.id, next)}
              />
              /v1/models
            </span>
            {group.provider ? null : (
              <Button
                variant="quiet"
                aria-label={`${openModels === group.id ? "Close" : "Edit"} models in ${group.name}`}
                onClick={() => setOpenModels(openModels === group.id ? null : group.id)}
              >
                models ({entries.filter((entry) => entry.group === group.id).length})
              </Button>
            )}
            <Button
              variant="quiet"
              aria-label={`Move ${group.name} up`}
              disabled={index === 0}
              onClick={() => onReorder(group.id, groups[index - 1]?.id ?? group.id)}
            >
              ↑
            </Button>
            <Button
              variant="quiet"
              aria-label={`Move ${group.name} down`}
              disabled={index === groups.length - 1}
              onClick={() => onReorder(group.id, groups[index + 1]?.id ?? group.id)}
            >
              ↓
            </Button>
            {group.provider ? null : (
              <Button
                variant="quiet"
                aria-label={`Delete group ${group.name}`}
                onClick={() => onDelete(group.id)}
              >
                delete
              </Button>
            )}
            {!group.provider && openModels === group.id ? (
              <div className="basis-full border-t border-line pt-2">
                <Input
                  value={filter}
                  placeholder="filter models, aliases and combos"
                  onChange={setFilter}
                />
                <div className="mt-2 max-h-64 overflow-y-auto rounded-[var(--radius-input)] border border-line bg-surface">
                  {candidates
                    .filter((candidate) => {
                      const needle = filter.trim().toLowerCase();
                      return (
                        !needle ||
                        candidate.ref.toLowerCase().includes(needle) ||
                        candidate.label.toLowerCase().includes(needle)
                      );
                    })
                    .map((candidate) => {
                      const checked =
                        entries.find((entry) => entry.ref === candidate.ref)?.group ===
                        group.id;
                      return (
                        <div
                          key={candidate.ref}
                          className="flex items-center gap-2 border-b border-[var(--color-rule)] px-2 py-1.5 last:border-b-0"
                        >
                          <Checkbox
                            checked={checked}
                            ariaLabel={`${checked ? "Remove" : "Add"} ${candidate.ref} ${checked ? "from" : "to"} ${group.name}`}
                            onChange={(next) =>
                              onAssign(candidate.ref, next ? group.id : null)
                            }
                          />
                          <span className="shrink-0 rounded-full border border-line bg-surface-2 px-1.5 py-px font-mono text-[10px] uppercase tracking-[0.06em] text-muted">
                            {candidate.provider}
                          </span>
                          <span className="min-w-0 flex-1 truncate font-mono text-caption">
                            {candidate.label}
                          </span>
                          {candidate.kind !== "model" ? (
                            <Badge tone="neutral">{candidate.kind}</Badge>
                          ) : null}
                        </div>
                      );
                    })}
                </div>
              </div>
            ) : null}
          </li>
        ))}
      </ul>
      <form
        className="mt-4 flex items-end gap-2 border-t border-line pt-4"
        onSubmit={(event) => {
          event.preventDefault();
          if (!name.trim()) return;
          onCreate(name.trim());
          setName("");
        }}
      >
        <div className="min-w-0 flex-1">
          <Field label="new group">
            <Input
              value={name}
              placeholder="Fast models"
              onChange={setName}
            />
          </Field>
        </div>
        <Button variant="primary" type="submit" disabled={!name.trim()}>
          add group
        </Button>
      </form>
      {accessFor ? (
        <GroupAccessDialog group={accessFor} onClose={() => setAccessFor(null)} />
      ) : null}
    </Modal>
  );
}
/** Team/key/user grants for one group. `Everyone` clears the access object. */
export function GroupAccessDialog({ group, onClose }: { group: DraftGroup; onClose: () => void }) {
  const client = useQueryClient();
  // Team, key and user lists are only needed once this dialog opens; keeping the reads
  // here means the board does not fetch them on load.
  const admin = useIsAdmin();
  const teams = useTeams();
  const keys = useKeys(admin);
  const users = useUsers(admin);
  const [everyone, setEveryone] = useState(!group.access);
  const [teamIds, setTeamIds] = useState<string[]>(group.access?.teams ?? []);
  const [keyIds, setKeyIds] = useState<string[]>(group.access?.keys ?? []);
  const [userIds, setUserIds] = useState<string[]>(group.access?.users ?? []);
  const save = useMutation({
    mutationFn: () =>
      setGroupAccess(
        group.id,
        everyone
          ? null
          : {
              teams: teamIds,
              keys: keyIds,
              users: userIds,
            },
      ),
    onSuccess: () => {
      void client.invalidateQueries();
      onClose();
    },
  });
  const section = (
    title: string,
    rows: Array<{ id: string; label: string }>,
    selected: string[],
    set: (next: string[]) => void,
  ) => (
    <div className="min-w-0">
      <div className="text-caption uppercase tracking-[0.06em] text-faint">{title}</div>
      <div className="mt-1 max-h-36 overflow-y-auto rounded-lg border border-line bg-surface-2 p-1">
        {rows.length === 0 ? (
          <div className="px-2 py-1.5 text-small text-faint">none</div>
        ) : (
          rows.map((row) => (
            <div
              key={row.id}
              className="flex items-center gap-2 rounded px-2 py-1 text-small hover:bg-surface-3"
            >
              <Checkbox
                checked={selected.includes(row.id)}
                label={row.label}
                onChange={() =>
                  set(
                    selected.includes(row.id)
                      ? selected.filter((item) => item !== row.id)
                      : [...selected, row.id],
                  )
                }
              />
            </div>
          ))
        )}
      </div>
    </div>
  );
  return (
    <Modal
      title={`access · ${group.name}`}
      description="Who may use this group's /v1/models entry and its group/model alias."
      open
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose}>cancel</Button>
          <Button variant="primary" disabled={save.isPending} onClick={() => save.mutate()}>
            save access
          </Button>
        </>
      }
    >
      <div className="flex flex-col gap-3">
        <label className="flex items-center gap-2 text-body">
          <input
            type="radio"
            name={`access-${group.id}`}
            checked={everyone}
            onChange={() => setEveryone(true)}
          />
          Everyone
        </label>
        <label className="flex items-center gap-2 text-body">
          <input
            type="radio"
            name={`access-${group.id}`}
            checked={!everyone}
            onChange={() => setEveryone(false)}
          />
          Only selected teams, keys and users
        </label>
        {!everyone ? (
          <>
            {section(
              "teams",
              (teams.data ?? []).map((team) => ({ id: team.id, label: team.name })),
              teamIds,
              setTeamIds,
            )}
            {section(
              "api keys",
              (keys.data ?? []).map((key) => ({ id: key.id, label: key.name ?? key.id.slice(0, 8) })),
              keyIds,
              setKeyIds,
            )}
            {section(
              "users",
              (users.data ?? []).map((user) => ({ id: user.id, label: `${user.name} · ${user.email}` })),
              userIds,
              setUserIds,
            )}
          </>
        ) : null}
        {save.error ? <ErrorNote error={save.error} /> : null}
      </div>
    </Modal>
  );
}
